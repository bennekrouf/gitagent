//! `gitagent mcp` — the flows, served to an MCP client over stdio.
//!
//! No window, no Dioxus: the same steps the run view drives, driven from tool
//! calls instead. The one promise carried over unchanged is the approval. A
//! gated step never runs on the strength of the call that reached it: the run
//! stops there and hands back exactly what it would do — the files, the
//! message, the command — and only a separate `approve` carries on. That is
//! the same two moments the window has, a proposal and a decision, with the
//! client's model and its user in between.
//!
//! Requests are handled one at a time, so a tool call that runs a long step
//! (a test suite, a model on a slow machine) holds the next one until it
//! settles. Fine for one client driving one repository; the place to change
//! if that stops being true is `serve`, not the driver.
//!
//! The driver here mirrors `drive` in the workspace screen step for step. The
//! two belong in one shared runner; until they are, a change to how a node
//! runs has to be made in both.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::services::flow;
use crate::services::flowdef::FlowBook;
use crate::services::graph::{Graph, NodeKind, NodeSpec, NodeStatus, RunState, Step};
use crate::services::llm::LlmConfig;
use crate::services::{forge, git, licence, live, probe, store, trusted};
use crate::telemetry;

/// The newest protocol revision this server speaks. A client asking for an
/// older one gets its own back; everything used here exists in all of them.
const PROTOCOL: &str = "2025-06-18";

/// How much of a diff or a log one tool result carries. A client's context is
/// the budget here, not the screen: the end of a log is where a failure is,
/// and the start of a diff is enough to see what kind of change it is.
const DIFF_CAP: usize = 20_000;
const LOG_CAP: usize = 4_000;

const INSTRUCTIONS: &str = "GitAgent runs git work as flows of steps: commit and open a pull \
request, review and merge one, cut a release. Start with list_repos to see what each repository \
needs, then run_flow. Any step that touches git history or the remote stops and returns what it \
will do; show that to the user and call approve or reject with their answer. Never approve on \
the user's behalf without asking them.";

/// One flow being run for one repository.
struct Run {
    repo: String,
    flow: String,
    graph: Graph,
    state: RunState,
}

impl Run {
    /// The step stopped at an approval, if one is.
    fn waiting(&self) -> Option<NodeSpec> {
        self.graph
            .nodes
            .iter()
            .find(|n| self.state.status(&n.id) == NodeStatus::AwaitingApproval)
            .cloned()
    }
}

pub fn serve() {
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime.block_on(serve_stdio());
}

async fn serve_stdio() {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut runs: BTreeMap<u64, Run> = BTreeMap::new();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            let reply = error(Value::Null, -32700, "not JSON");
            write(&mut stdout, &reply).await;
            continue;
        };
        // A notification (no id) asks for nothing back: `initialized`,
        // cancellations, and whatever later revisions add.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message["method"].as_str().unwrap_or("");
        let params = &message["params"];

        let reply = match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or(PROTOCOL);
                let version = if asked <= PROTOCOL { asked } else { PROTOCOL };
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "gitagent", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": tools() })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let args = &params["arguments"];
                let (text, is_error) = match call(name, args, &mut runs).await {
                    Ok(value) => (pretty(&value), false),
                    Err(why) => (why, true),
                };
                result(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
                )
            }
            _ => error(id, -32601, &format!("no method {method}")),
        };
        write(&mut stdout, &reply).await;
    }
}

async fn write(stdout: &mut tokio::io::Stdout, reply: &Value) {
    // One message per line, and nothing else ever on stdout: the client reads
    // it as the protocol, so a stray print is a corrupt message.
    let mut bytes = serde_json::to_vec(reply).unwrap_or_default();
    bytes.push(b'\n');
    let _ = stdout.write_all(&bytes).await;
    let _ = stdout.flush().await;
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn tools() -> Value {
    let run_id = json!({ "run_id": { "type": "integer", "description": "From run_flow." } });
    json!([
        {
            "name": "list_repos",
            "description": "Every git repository directly inside a folder, with its branch, \
                            uncommitted changes, open pull requests, what it needs next, and \
                            the flow GitAgent would run for that.",
            "inputSchema": {
                "type": "object",
                "properties": { "folder": { "type": "string", "description": "Absolute path." } },
                "required": ["folder"],
            },
        },
        {
            "name": "list_flows",
            "description": "The flows that can run in a repository, and their steps. Steps \
                            marked gated stop for approval.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo": { "type": "string", "description": "Absolute path." } },
                "required": ["repo"],
            },
        },
        {
            "name": "run_flow",
            "description": "Start a flow and run it until it finishes or reaches a step that \
                            needs approval. That step's proposal comes back under `awaiting`: \
                            show it to the user, then call approve or reject.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Absolute path." },
                    "flow": { "type": "string", "description": "Flow id from list_flows." },
                    "pr": { "type": "string", "description": "Pull request number, for a \
                                                              review flow." },
                },
                "required": ["repo", "flow"],
            },
        },
        {
            "name": "approve",
            "description": "Approve the step a run is waiting on, as the user decided, and \
                            run on to the next approval or the end.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": run_id["run_id"],
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files the user unchecked, from `awaiting.files`: \
                                        left out of the commit. Only a step that lists \
                                        files (the commit) takes this.",
                    },
                },
                "required": ["run_id"],
            },
        },
        {
            "name": "reject",
            "description": "Decline the step a run is waiting on. Nothing is run, and the \
                            steps that depend on it are blocked.",
            "inputSchema": {
                "type": "object",
                "properties": run_id,
                "required": ["run_id"],
            },
        },
        {
            "name": "run_status",
            "description": "Where a run stands, without moving it.",
            "inputSchema": {
                "type": "object",
                "properties": run_id,
                "required": ["run_id"],
            },
        },
    ])
}

async fn call(name: &str, args: &Value, runs: &mut BTreeMap<u64, Run>) -> Result<Value, String> {
    let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
    match name {
        "list_repos" => list_repos(&text("folder")).await,
        "list_flows" => list_flows(&repo_path(&text("repo"))?),
        "run_flow" => run_flow(&text("repo"), &text("flow"), &text("pr"), runs).await,
        "approve" | "reject" | "run_status" => {
            let id = args["run_id"].as_u64().ok_or("run_id is required")?;
            let run = runs.get_mut(&id).ok_or(format!("no run {id}"))?;
            match name {
                "approve" => {
                    let exclude: Vec<String> = args["exclude"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    decide(run, true, &exclude).await
                }
                "reject" => decide(run, false, &[]).await,
                _ => Ok(report(run)),
            }
        }
        _ => Err(format!("no tool {name}")),
    }
}

/// The path as the app keys repositories, so a licence slot taken here is the
/// same one the window counts.
fn repo_path(raw: &str) -> Result<String, String> {
    let path = std::fs::canonicalize(raw).map_err(|e| format!("{raw}: {e}"))?;
    if !path.join(".git").exists() {
        return Err(format!("{} is not a git repository", path.display()));
    }
    Ok(path.to_string_lossy().into_owned())
}

async fn list_repos(folder: &str) -> Result<Value, String> {
    if !std::path::Path::new(folder).is_dir() {
        return Err(format!("{folder} is not a folder"));
    }
    let book = FlowBook::load();
    let hidden = store::load_repo_flows();
    let mut out = vec![];
    for repo in store::discover_repos(folder) {
        let status = probe::probe(&repo.path).await;
        let suggested = trusted::next_flow(&book, &status, &hidden.hidden_for(&repo.path));
        out.push(json!({
            "repo": repo.path,
            "name": repo.label,
            "branch": status.branch,
            "uncommitted_files": status.changes,
            "needs": status.wants().note(),
            "pull_requests": status.prs.iter().map(|pr| json!({
                "number": pr.number,
                "title": pr.title,
                "url": pr.url,
                "checks": format!("{:?}", pr.checks).to_lowercase(),
                "size": pr.size(),
            })).collect::<Vec<_>>(),
            "suggested_flow": suggested.map(|(flow, pr)| json!({ "flow": flow, "pr": pr })),
        }));
    }
    Ok(json!(out))
}

fn list_flows(repo: &str) -> Result<Value, String> {
    let book = FlowBook::load();
    let hidden = store::load_repo_flows().hidden_for(repo);
    Ok(json!(book
        .runnable_for(&hidden)
        .into_iter()
        .map(|def| json!({
            "flow": def.id,
            "label": def.label,
            "steps": def.to_graph().nodes.iter().map(|n| json!({
                "id": n.id,
                "title": n.title,
                "gated": n.requires_approval,
            })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>()))
}

async fn run_flow(
    repo: &str,
    flow_id: &str,
    pr: &str,
    runs: &mut BTreeMap<u64, Run>,
) -> Result<Value, String> {
    let repo = repo_path(repo)?;
    // One run per repository, as in the window: two flows sharing a working
    // tree is how a commit picks up a half-finished merge.
    if let Some(busy) = runs
        .values()
        .find(|r| r.repo == repo && !r.state.is_finished(&r.graph))
    {
        return Err(format!(
            "run {} ({}) is still going in this repository; finish or reject it first",
            busy.state.run, busy.flow
        ));
    }

    let book = FlowBook::load();
    let hidden = store::load_repo_flows().hidden_for(&repo);
    let def = book
        .runnable_for(&hidden)
        .into_iter()
        .find(|f| f.id == flow_id)
        .cloned()
        .ok_or(format!(
            "no flow {flow_id} for this repository; see list_flows"
        ))?;

    if !licence::may_run(&licence::current(), &repo) {
        telemetry::record(telemetry::Event::LicenceWallHit);
        return Err(
            "The free version runs flows in 5 repositories, and this would be a sixth. \
                    Give one back or add a GitAgent Pro licence from the app (Get Pro\u{2026})."
                .into(),
        );
    }

    let graph = def.to_graph();
    let mut state = RunState::fresh(&graph);
    state.started = true;
    if !pr.is_empty() {
        state
            .artifacts
            .insert("selected_pr_number".into(), pr.into());
    }
    let id = state.run;
    runs.insert(
        id,
        Run {
            repo,
            flow: def.id,
            graph,
            state,
        },
    );
    let run = runs.get_mut(&id).expect("just inserted");
    advance(run).await;
    Ok(report(run))
}

/// The person's answer to the step a run is waiting on, then on to the next.
async fn decide(run: &mut Run, approved: bool, exclude: &[String]) -> Result<Value, String> {
    let node = run
        .waiting()
        .ok_or("this run is not waiting on an approval")?;
    // An exclusion that matches nothing on offer is refused rather than
    // dropped: the user unchecked that file, and approving anyway would commit
    // it. Scan is where this bites — its files are only offered at the commit
    // step, once scan has found them.
    let offered: Vec<String> = run
        .state
        .runs
        .get(&node.id)
        .map(|r| r.items.iter().map(|i| i.key.clone()).collect())
        .unwrap_or_default();
    let unknown: Vec<&String> = exclude.iter().filter(|p| !offered.contains(p)).collect();
    if approved && !unknown.is_empty() {
        return Err(if offered.is_empty() {
            format!(
                "{} offers no files to leave out; nothing was approved. Approve without \
                 `exclude`, and leave files out at the commit step, which lists them.",
                node.title
            )
        } else {
            format!(
                "not offered here: {}; nothing was approved. The files are: {}",
                unknown
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                offered.join(", ")
            )
        });
    }
    telemetry::record(telemetry::Event::ApprovalDecided {
        approved,
        trusted: false,
    });
    if !approved {
        run.state.reject(&node.id, &run.graph);
        advance(run).await;
        return Ok(report(run));
    }

    // Unchecking a file here is unchecking it in the window: the commit step
    // stages what is still included on its own node.
    if let Some(entry) = run.state.runs.get_mut(&node.id) {
        for item in entry.items.iter_mut() {
            if exclude.iter().any(|path| path == &item.key) {
                item.included = false;
            }
        }
        if entry.has_nothing_selected() {
            return Err(
                "every file is excluded, so there is nothing to commit; reject instead".into(),
            );
        }
    }
    run.state.decisions.insert(node.id.clone(), true);
    run_node(run, &node).await;
    advance(run).await;
    Ok(report(run))
}

/// Tells an open window where this run stands — see `services::live`.
fn publish(run: &Run) {
    let label = FlowBook::load()
        .get(&run.flow)
        .map(|f| f.label.clone())
        .unwrap_or_else(|| run.flow.clone());
    live::publish(live::snapshot(
        run.state.run,
        &run.repo,
        &run.flow,
        &label,
        &run.graph,
        &run.state,
    ));
}

/// Runs ready steps until the run ends or one stops for approval.
async fn advance(run: &mut Run) {
    loop {
        if run.waiting().is_some() {
            return;
        }
        let Some(node) = run.state.next_ready(&run.graph) else {
            break;
        };
        let resolved = run.state.resolved_for(&node);
        if node.requires_approval && !flow::nothing_to_approve(&node, &resolved) {
            let proposal = flow::proposal(&node, &resolved);
            let items = flow::proposal_items(&node, &resolved);
            let preview_diff = flow::diff_preview(&node, &run.repo, &resolved)
                .await
                .unwrap_or_default();
            let entry = run.state.runs.entry(node.id.clone()).or_default();
            entry.proposal = proposal;
            entry.items = items;
            entry.preview_diff = preview_diff;
            run.state.set_status(&node.id, NodeStatus::AwaitingApproval);
            publish(run);
            return;
        }
        run_node(run, &node).await;
    }
    publish(run);
    report_finished(run).await;
}

/// One step, start to settled. See `drive` in the workspace screen for why
/// each of the stand-ins below exists.
async fn run_node(run: &mut Run, node: &NodeSpec) {
    // Read each time: Settings may change in the window while a client holds
    // a run open, and the next step should use what is set now.
    let cfg: LlmConfig = store::load_settings();
    run.state.set_status(&node.id, NodeStatus::Running);
    publish(run);
    let state = &mut run.state;

    let bypass = |state: &mut RunState, summary: &str, log: &str| {
        state.set_status(&node.id, NodeStatus::Bypassed);
        let entry = state.runs.entry(node.id.clone()).or_default();
        entry.summary = summary.into();
        entry.log = log.into();
    };
    if node.kind == NodeKind::Model && !cfg.uses_model() {
        let stand_in = flow::without_model(node, state);
        for (k, v) in stand_in.artifacts {
            state
                .artifacts
                .insert(crate::services::graph::qualified(&node.id, &k), v.clone());
            state.artifacts.insert(k, v);
        }
        bypass(state, &stand_in.summary, &stand_in.log);
        return;
    }
    if node.step == Step::Lenses && !cfg.lenses.any() {
        bypass(state, "skipped \u{2014} no focused review turned on", "");
        return;
    }
    if node.step == Step::SecondOpinion && !cfg.second_config().uses_model() {
        bypass(state, "skipped \u{2014} no second reviewer chosen", "");
        return;
    }

    let resolved = state.resolved_for(node);
    // Live lines have nowhere to go until the call returns; the step's own
    // final log replaces them anyway.
    let result = flow::execute(node, &run.repo, &cfg, &resolved, &mut |_| {}).await;

    match result {
        Ok(outcome) => {
            let nothing = outcome.nothing_to_do;
            for (k, v) in outcome.artifacts {
                state.artifacts.insert(k, v);
            }
            state.set_status(
                &node.id,
                if nothing {
                    NodeStatus::Skipped
                } else {
                    NodeStatus::Done
                },
            );
            let entry = state.runs.entry(node.id.clone()).or_default();
            entry.summary = outcome.summary;
            entry.log = outcome.log;
            if !outcome.items.is_empty() {
                entry.items = outcome.items;
            }
            if nothing {
                state.propagate_block(&run.graph);
            }
        }
        Err(failure) => {
            state.set_status(&node.id, NodeStatus::Failed);
            let entry = state.runs.entry(node.id.clone()).or_default();
            entry.summary = "failed".into();
            entry.log = failure.message;
            entry.remedies = failure.remedies;
            state.propagate_block(&run.graph);
        }
    }
}

async fn report_finished(run: &Run) {
    if !telemetry::active() || !run.state.is_finished(&run.graph) {
        return;
    }
    let statuses: Vec<NodeStatus> = run
        .graph
        .nodes
        .iter()
        .map(|n| run.state.status(&n.id))
        .collect();
    let forge = match git::remote_url(&run.repo).await {
        Some(url) => forge::detect(&url),
        None => forge::Forge::None,
    };
    telemetry::record(telemetry::Event::flow_finished(
        &run.flow,
        telemetry::outcome_of(&statuses),
        &forge,
        store::load_settings().kind,
    ));
    tokio::spawn(telemetry::flush());
}

/// What a client needs to tell its user where the run stands, and to ask
/// them the next question if there is one.
fn report(run: &Run) -> Value {
    let state = &run.state;
    let waiting = run.waiting();
    let phase = if waiting.is_some() {
        "awaiting_approval"
    } else if state.is_finished(&run.graph) {
        "finished"
    } else {
        "stopped"
    };

    let steps: Vec<Value> = run
        .graph
        .nodes
        .iter()
        .map(|n| {
            let entry = state.runs.get(&n.id).cloned().unwrap_or_default();
            let mut step = json!({
                "id": n.id,
                "title": n.title,
                "status": state.status(&n.id).label(),
                "summary": entry.summary,
            });
            if state.status(&n.id) == NodeStatus::Failed {
                step["log"] = json!(tail(&entry.log, LOG_CAP));
                step["fixes"] = json!(entry
                    .remedies
                    .iter()
                    .map(|r| json!({ "label": r.label, "command": r.display }))
                    .collect::<Vec<_>>());
            }
            step
        })
        .collect();

    let awaiting = waiting.map(|node| {
        let entry = state.runs.get(&node.id).cloned().unwrap_or_default();
        json!({
            "step": node.id,
            "title": node.title,
            "will_run": entry.proposal,
            "files": entry.items.iter().map(|i| json!({
                "path": i.key,
                "change": i.note,
                "included": i.included,
            })).collect::<Vec<_>>(),
            "diff": head(&entry.preview_diff, DIFF_CAP),
        })
    });

    // Only the short, unqualified artifacts — a branch name, a PR URL — are
    // worth a client's context; diffs and drafts are already in the steps.
    let produced: BTreeMap<&String, &String> = state
        .artifacts
        .iter()
        .filter(|(k, v)| !k.contains('.') && v.len() <= 300 && !v.trim().is_empty())
        .collect();

    json!({
        "run_id": state.run,
        "repo": run.repo,
        "flow": run.flow,
        "phase": phase,
        "awaiting": awaiting,
        "steps": steps,
        "produced": produced,
    })
}

fn head(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n\u{2026} ({} more bytes)",
        &text[..cut],
        text.len() - cut
    )
}

fn tail(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_string();
    }
    let mut cut = text.len() - cap;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    format!("\u{2026}\n{}", &text[cut..])
}
