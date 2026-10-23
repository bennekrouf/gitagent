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
//! The run itself is `services::runner::Run`, the same one `gitagent run`
//! drives from a terminal; what is here is the protocol, and the report a
//! client reads.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::services::flowdef::{self, FlowBook, FlowDef};
use crate::services::graph::{NodeKind, NodeStatus};
use crate::services::runner::{repo_path, Next, Run};
use crate::services::{catalogue, live, probe, store, trusted};

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
the user's behalf without asking them. Pass trusted: true to run_flow only when the user asks for \
a trusted run: GitAgent then answers approvals itself by the rules of its Trusted run — it holds \
at anything risky, any script or deploy, and release notes — and carries on into the next flow \
the repository needs. If the user turned on Trust all in the GitAgent window, runs are trusted \
already. Either way, report what was approved automatically and where it stopped. The flows \
themselves can be read and changed too: list_steps names the steps a flow can use, get_flow \
shows one, save_flow creates or changes one (it is refused, with the reason, if it would not \
run), delete_flow removes one, and show_flow sets which repositories show it. Say what you \
are about to change before saving, as the user would see it in Setup. repo_insight tells \
everything GitAgent knows about one repository.";

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
                    "trusted": { "type": "boolean", "description": "Only when the user asked \
                        for a trusted run: GitAgent approves what its Trusted run would, holds \
                        at the rest, and continues into the next flow the repository needs. \
                        Already on if the user turned on Trust all in the GitAgent window." },
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
        {
            "name": "list_steps",
            "description": "Every kind of step a flow can be built from: what it does, what it \
                            reads and writes, whether it stops for approval by default, and the \
                            settings it takes.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "get_flow",
            "description": "One flow's full definition — its steps, how they depend on each \
                            other, their settings — in the shape save_flow takes, plus any \
                            problems that keep it from running and which repositories show it.",
            "inputSchema": {
                "type": "object",
                "properties": { "flow": { "type": "string", "description": "Flow id." } },
                "required": ["flow"],
            },
        },
        {
            "name": "save_flow",
            "description": "Create a flow, or replace one with the same id, as Setup's Save \
                            does. Take the shape from get_flow: id (names the flow to replace; \
                            a flow you made always ends up with the id its label reads as, \
                            which `saved` in the result gives), label, handles, and nodes, each with id, \
                            step (a key from list_steps), deps, and optional title, config and \
                            gated. Refused, with the reasons, if the flow would not run. A \
                            script or server step always stops for approval: `gated: false` \
                            on one is refused unless it was already so. flows.toml is backed \
                            up first, and the open window picks the change up by itself.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "flow": { "type": "object", "description": "The flow definition." },
                    "repo": { "type": "string", "description": "For a new flow: show it on \
                        this repository only, as a flow made in Setup is. Leave out to show \
                        it everywhere." },
                },
                "required": ["flow"],
            },
        },
        {
            "name": "delete_flow",
            "description": "Remove a flow. flows.toml is backed up first.",
            "inputSchema": {
                "type": "object",
                "properties": { "flow": { "type": "string", "description": "Flow id." } },
                "required": ["flow"],
            },
        },
        {
            "name": "show_flow",
            "description": "Which repositories show a flow: on one repository (shown true or \
                            false), or on every repository (everywhere true).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "flow": { "type": "string", "description": "Flow id." },
                    "repo": { "type": "string", "description": "Absolute path." },
                    "shown": { "type": "boolean" },
                    "everywhere": { "type": "boolean" },
                },
                "required": ["flow"],
            },
        },
        {
            "name": "repo_insight",
            "description": "Everything GitAgent knows about one repository: branch, uncommitted \
                            files, commits ahead or behind, what it needs next and which flow \
                            answers that, every open pull request with its checks and size, \
                            the release state, each shown flow with what it would act on, and \
                            any run Claude has going there.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo": { "type": "string", "description": "Absolute path." } },
                "required": ["repo"],
            },
        },
    ])
}

async fn call(name: &str, args: &Value, runs: &mut BTreeMap<u64, Run>) -> Result<Value, String> {
    let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
    match name {
        "list_repos" => list_repos(&text("folder")).await,
        "list_flows" => list_flows(&repo_path(&text("repo"))?),
        "run_flow" => {
            let trusted = args["trusted"].as_bool().unwrap_or(false);
            run_flow(&text("repo"), &text("flow"), &text("pr"), trusted, runs).await
        }
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
                    run.decide(true, &exclude).await.map(|()| report(run))
                }
                "reject" => run.decide(false, &[]).await.map(|()| report(run)),
                _ => Ok(report(run)),
            }
        }
        "list_steps" => Ok(list_steps()),
        "get_flow" => get_flow(&text("flow")),
        "save_flow" => save_flow(&args["flow"], &text("repo")),
        "delete_flow" => delete_flow(&text("flow")),
        "show_flow" => show_flow(
            &text("flow"),
            &text("repo"),
            args["shown"].as_bool(),
            args["everywhere"].as_bool().unwrap_or(false),
        ),
        "repo_insight" => repo_insight(&repo_path(&text("repo"))?).await,
        _ => Err(format!("no tool {name}")),
    }
}

fn list_steps() -> Value {
    json!(catalogue::CATALOGUE
        .iter()
        .map(|s| json!({
            "step": s.key,
            "title": s.title,
            "about": s.about,
            "uses_model": s.kind == NodeKind::Model,
            "reads": s.reads,
            "writes": s.writes,
            "stops_for_approval": s.gate_by_default,
            "settings": s.config.iter().map(|c| json!({
                "key": c.key,
                "label": c.label,
                "help": c.help,
                "required": c.required,
                "example": c.placeholder,
            })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

/// A flow as save_flow takes it, with what keeps it from running and where
/// it is shown alongside — those two are read-only and ignored on save.
fn flow_json(def: &FlowDef, shown: &store::RepoFlows) -> Value {
    let mut out = serde_json::to_value(def).unwrap_or_default();
    if let Some(nodes) = out.as_object_mut().and_then(|o| o.remove("node")) {
        out["nodes"] = nodes;
    }
    out["problems"] = json!(flowdef::validate(def)
        .iter()
        .map(|p| p.message())
        .collect::<Vec<_>>());
    out["shown_only_on"] = json!(shown.shown_only_on(&def.id));
    out["hidden_on"] = json!(shown
        .hidden
        .iter()
        .filter(|(_, ids)| ids.iter().any(|id| id == &def.id))
        .map(|(repo, _)| repo)
        .collect::<Vec<_>>());
    out
}

fn get_flow(id: &str) -> Result<Value, String> {
    let book = FlowBook::load();
    let def = book
        .get(id)
        .ok_or(format!("no flow {id}; see list_flows"))?;
    Ok(flow_json(def, &store::load_repo_flows()))
}

/// flows.toml as it was before a change, beside it — one step of undo for a
/// change a client made.
fn back_up_flows() {
    let dir = store::data_dir();
    let _ = std::fs::copy(dir.join("flows.toml"), dir.join("flows.toml.bak"));
}

fn save_flow(raw: &Value, repo: &str) -> Result<Value, String> {
    let mut raw = raw.clone();
    let obj = raw
        .as_object_mut()
        .ok_or("`flow` must be an object, in the shape get_flow returns")?;
    for read_only in ["problems", "shown_only_on", "hidden_on"] {
        obj.remove(read_only);
    }
    if !obj.contains_key("node") {
        if let Some(nodes) = obj.remove("nodes") {
            obj.insert("node".into(), nodes);
        }
    }
    let label = obj
        .get("label")
        .and_then(|l| l.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if label.is_empty() {
        return Err("a flow needs a `label` — the name shown on its tab".into());
    }
    if obj
        .get("id")
        .and_then(|i| i.as_str())
        .is_none_or(|i| i.trim().is_empty())
    {
        obj.insert("id".into(), json!(flowdef::id_from_label(&label)));
    }
    let flow: FlowDef = serde_json::from_value(raw)
        .map_err(|e| format!("not a flow definition ({e}); see get_flow for the shape"))?;

    let mut book = FlowBook::load();
    let before = book.get(&flow.id).cloned();
    // A command the app cannot vet only runs unattended on the person's own
    // say-so, given in Setup — never on a client's.
    for node in &flow.nodes {
        let command = matches!(node.step.as_str(), "run_script" | "run_remote");
        if command && node.gated == Some(false) {
            let already = before.as_ref().is_some_and(|f| {
                f.nodes
                    .iter()
                    .any(|n| n.id == node.id && n.step == node.step && n.gated == Some(false))
            });
            if !already {
                return Err(format!(
                    "step `{}` runs a command, and GitAgent's MCP tools will not make it run \
                     without approval. Leave `gated` out, or have the user untick Needs \
                     approval on it in Setup.",
                    node.id
                ));
            }
        }
    }
    let problems = flowdef::validate(&flow);
    if !problems.is_empty() {
        return Err(format!(
            "not saved — this flow would not run:\n- {}",
            problems
                .iter()
                .map(|p| p.message())
                .collect::<Vec<_>>()
                .join("\n- ")
        ));
    }

    back_up_flows();
    let created = before.is_none();
    match book.flows.iter_mut().find(|f| f.id == flow.id) {
        Some(existing) => *existing = flow.clone(),
        None => book.flows.push(flow.clone()),
    }
    let mut shown = store::load_repo_flows();
    let mut id = flow.id.clone();
    for (old, new) in book.ids_from_labels() {
        shown.rename_flow(&old, &new);
        if old == id {
            id = new;
        }
    }
    if created && !repo.trim().is_empty() {
        shown.only_on(&id, &repo_path(repo)?);
    }
    book.save();
    store::save_repo_flows(&shown);
    let saved = book.get(&id).ok_or("saved, but could not read it back")?;
    Ok(json!({
        "saved": id,
        "created": created,
        "flow": flow_json(saved, &shown),
    }))
}

fn delete_flow(id: &str) -> Result<Value, String> {
    let mut book = FlowBook::load();
    if book.get(id).is_none() {
        return Err(format!("no flow {id}; see list_flows"));
    }
    if book.flows.len() == 1 {
        return Err("this is the only flow left; GitAgent needs at least one".into());
    }
    back_up_flows();
    book.flows.retain(|f| f.id != id);
    book.save();
    let mut shown = store::load_repo_flows();
    shown.forget_flow(id);
    store::save_repo_flows(&shown);
    Ok(json!({ "deleted": id, "backup": "flows.toml.bak, in GitAgent's data folder" }))
}

fn show_flow(id: &str, repo: &str, shown: Option<bool>, everywhere: bool) -> Result<Value, String> {
    if FlowBook::load().get(id).is_none() {
        return Err(format!("no flow {id}; see list_flows"));
    }
    let mut lists = store::load_repo_flows();
    if everywhere {
        lists.everywhere(id);
        for ids in lists.hidden.values_mut() {
            ids.retain(|f| f != id);
        }
        lists.hidden.retain(|_, ids| !ids.is_empty());
    } else {
        let repo = repo_path(repo)?;
        match shown {
            Some(true) => lists.show(&repo, id),
            Some(false) => lists.hide(&repo, id),
            None => {
                return Err("say `shown` true or false for the repo, or `everywhere` true".into())
            }
        }
    }
    store::save_repo_flows(&lists);
    let def = FlowBook::load();
    Ok(flow_json(def.get(id).expect("checked above"), &lists))
}

async fn repo_insight(repo: &str) -> Result<Value, String> {
    let status = probe::probe(repo).await;
    let book = FlowBook::load();
    let lists = store::load_repo_flows();
    let hidden = lists.hidden_for(repo);
    let wants = status.wants();
    let next = trusted::next_flow(&book, &status, &hidden);
    let flows: Vec<Value> = book
        .runnable_for(&hidden)
        .into_iter()
        .map(|f| {
            let count = status.count_for(&f.handles);
            json!({
                "flow": f.id,
                "label": f.label,
                "would_act_on": count.as_ref().map(|c| &c.0),
                "detail": count.as_ref().map(|c| &c.1),
            })
        })
        .collect();
    let prs: Vec<Value> = status
        .prs
        .iter()
        .map(|p| {
            json!({
                "number": p.number,
                "title": p.title,
                "checks": format!("{:?}", p.checks).to_lowercase(),
                "files": p.files,
                "additions": p.additions,
                "deletions": p.deletions,
            })
        })
        .collect();
    let live_here: Vec<Value> = live::load()
        .into_iter()
        .filter(|r| r.repo == repo)
        .map(|r| {
            json!({
                "flow": r.flow_label,
                "finished": r.finished,
                "trusted": r.trusted,
                "waiting_on": r.waiting.map(|w| w.0),
            })
        })
        .collect();
    Ok(json!({
        "repo": repo,
        "branch": status.branch,
        "uncommitted_files": status.changes,
        "ahead_of_upstream": status.ahead,
        "behind_upstream": status.behind,
        "commits_without_a_pull_request": status.unmerged,
        "needs": wants.note(),
        "next_flow": next.as_ref().map(|(f, _)| f),
        "why_nothing_runs_next": if next.is_none() {
            trusted::why_stopped(&book, &status, &hidden)
        } else {
            None
        },
        "open_pull_requests": prs,
        "pull_requests_unreadable": status.prs_error,
        "release": status.release.summary(),
        "git_operation_in_progress": status.in_progress.as_ref().map(|p| p.label()),
        "flows": flows,
        "claude_runs_here": live_here,
    }))
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
    trusted: bool,
    runs: &mut BTreeMap<u64, Run>,
) -> Result<Value, String> {
    let trusted = trusted || store::trust_all();
    let mut legs: Vec<Value> = vec![];
    let (mut flow_id, mut pr) = (flow_id.to_string(), pr.to_string());
    // A trusted run goes on into the next flow the repository needs, as the
    // window's does; anything else is one flow.
    for _ in 0..trusted::MOST_FLOWS {
        let id = start_flow(repo, &flow_id, &pr, trusted, runs).await?;
        let run = runs.get(&id).expect("just started");
        legs.push(report(run));
        match run.next().await {
            Next::Flow(next, next_pr) => (flow_id, pr) = (next, next_pr),
            Next::Stop(why) => {
                if let (Some(why), Some(last)) = (why, legs.last_mut()) {
                    last["chain_stopped"] = json!(why);
                }
                break;
            }
        }
    }
    let mut last = legs.pop().expect("at least one flow ran");
    if !legs.is_empty() {
        last["earlier_flows"] = json!(legs);
    }
    Ok(last)
}

/// One flow, started and run to its first stop. Returns the run's id.
async fn start_flow(
    repo: &str,
    flow_id: &str,
    pr: &str,
    trusted: bool,
    runs: &mut BTreeMap<u64, Run>,
) -> Result<u64, String> {
    let repo = repo_path(repo)?;
    // One run per repository, as in the window: two flows sharing a working
    // tree is how a commit picks up a half-finished merge.
    if let Some(busy) = runs.values().find(|r| r.repo == repo && !r.is_finished()) {
        return Err(format!(
            "run {} ({}) is still going in this repository; finish or reject it first",
            busy.state.run, busy.flow
        ));
    }
    let mut run =
        Run::prepare(&repo, flow_id, pr, trusted).map_err(|e| format!("{e}; see list_flows"))?;
    run.advance().await;
    let id = run.state.run;
    runs.insert(id, run);
    Ok(id)
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

    let mut out = json!({
        "run_id": state.run,
        "repo": run.repo,
        "flow": run.flow,
        "phase": phase,
        "awaiting": awaiting,
        "steps": steps,
        "produced": produced,
    });
    if run.trusted || !run.approved_by_trust.is_empty() || run.held.is_some() {
        out["trusted"] = json!(run.held.is_none() && run.trusting());
        out["approved_automatically"] = json!(run.approved_by_trust);
        if let Some(why) = &run.held {
            out["held"] = json!(why);
        }
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::graph::RunState;

    fn run(trusted: bool) -> Run {
        let graph = FlowBook::defaults()
            .get("commit_and_pr")
            .expect("shipped")
            .to_graph();
        let state = RunState::fresh(&graph);
        Run::new("/r".into(), "commit_and_pr".into(), graph, state, trusted)
    }

    #[test]
    fn a_trusted_run_stops_trusting_at_its_first_hold() {
        let mut r = run(true);
        assert!(r.trusting());
        r.held = Some("CI is still running".into());
        assert!(!r.trusting(), "from the hold on, the run is the person's");
        let out = report(&r);
        assert_eq!(out["held"], "CI is still running");
        assert_eq!(out["trusted"], false);
    }

    #[test]
    fn an_ordinary_run_reports_nothing_about_trust() {
        // Trust all lives on disk; this only holds while it is off.
        if store::trust_all() {
            return;
        }
        let out = report(&run(false));
        assert!(out.get("trusted").is_none() && out.get("approved_automatically").is_none());
    }
}
