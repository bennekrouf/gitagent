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
use crate::services::runner::{self, repo_path, Next, Run};
use crate::services::{busy, catalogue, live, llm, probe, store, trusted};
mod flows;
use flows::*;
mod run;
use run::*;

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
the repository needs. When the user answers an approval with \"stop asking me\", approve it \
with stop_asking: true: the rest of that run is trusted the same way. If the user turned on \
Trust all in the GitAgent window, runs are trusted already; trust_all says whether it is on, \
and turns it off if the user asks — never on, which only the user does, in the window. Either way, report what was approved automatically and where it stopped. The flows \
themselves can be read and changed too: list_steps names the steps a flow can use, get_flow \
shows one, save_flow creates or changes one (it is refused, with the reason, if it would not \
run), delete_flow removes one, and show_flow sets which repositories show it. Say what you \
are about to change before saving, as the user would see it in Setup. repo_insight tells \
everything GitAgent knows about one repository. refresh is the window's Refresh button, for \
one repository, a folder, or everything open. model_region says where the model runs and \
switches it between api0's regions (europe, china, usa) when the user asks.";

pub fn serve() {
    match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime.block_on(serve_stdio()),
        Err(e) => {
            eprintln!("gitagent mcp: could not start: {e}");
            std::process::exit(1);
        }
    }
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
                let token = params["_meta"]["progressToken"].clone();
                let outcome = if token.is_null() {
                    call(name, args, &mut runs).await
                } else {
                    with_progress(token, call(name, args, &mut runs), &mut stdout).await
                };
                let (text, is_error) = match outcome {
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

// ── Progress ──────────────────────────────────────────────────────────────
//
// A tool call that runs a step can take minutes — a model on a slow machine,
// a test suite — and the client sees nothing until it returns. When the client
// sent a progress token with the call, the run's progress goes back to it as
// `notifications/progress` while the call is still going: each step as it
// starts and settles, the model's "still waiting" lines, and a line of its own
// whenever nothing else has been said for a while.

/// How often to say something when nothing else has: often enough that a
/// client never looks hung, rarely enough not to drown its log.
const QUIET_FOR: std::time::Duration = std::time::Duration::from_secs(15);

/// Output lines of a running step (a test suite prints hundreds) are passed
/// on at most this often. Steps starting and settling always are.
const LINES_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

/// One line of progress, and whether it may be dropped when lines come
/// thick and fast.
type ProgressTx = tokio::sync::mpsc::UnboundedSender<(bool, String)>;

/// Where `Run::progress` sends to while a call with a progress token runs.
/// `Run::progress` is a plain `fn`, so it cannot capture the call's channel;
/// requests are handled one at a time, so one slot is enough.
fn progress_slot() -> &'static std::sync::Mutex<Option<ProgressTx>> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<Option<ProgressTx>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(None))
}

/// `Run::progress` for runs started here.
pub(crate) fn report_progress(progress: runner::Progress<'_>) {
    let message = match progress {
        runner::Progress::Step(node) => (false, format!("{}…", node.title)),
        runner::Progress::Trusted(node) => (
            false,
            format!("{}: approved by the trusted run", node.title),
        ),
        runner::Progress::Settled(node, state) => (
            false,
            match state.runs.get(&node.id).map(|r| r.summary.as_str()) {
                Some(summary) if !summary.is_empty() => format!("{}: {summary}", node.title),
                _ => format!("{}: {}", node.title, state.status(&node.id).label()),
            },
        ),
        runner::Progress::Line(line) => (true, head(line.trim(), 200)),
    };
    if message.1.is_empty() {
        return;
    }
    if let Some(tx) = progress_slot().lock().ok().and_then(|slot| slot.clone()) {
        let _ = tx.send(message);
    }
}

/// Runs `call`, passing what its run reports on to the client as progress
/// notifications for `token`.
async fn with_progress<F>(token: Value, call: F, stdout: &mut tokio::io::Stdout) -> F::Output
where
    F: std::future::Future,
{
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    if let Ok(mut slot) = progress_slot().lock() {
        *slot = Some(tx);
    }
    let started = std::time::Instant::now();
    let mut sent = 0u64;
    let mut last_said = std::time::Instant::now();
    let mut last_line = std::time::Instant::now() - LINES_EVERY;
    let mut doing = String::new();
    tokio::pin!(call);
    let out = loop {
        tokio::select! {
            biased;
            out = &mut call => break out,
            Some((droppable, text)) = rx.recv() => {
                if droppable && last_line.elapsed() < LINES_EVERY {
                    continue;
                }
                if droppable {
                    last_line = std::time::Instant::now();
                } else if let Some(step) = text.strip_suffix('…') {
                    doing = step.to_string();
                }
                sent += 1;
                last_said = std::time::Instant::now();
                write(stdout, &progress_note(&token, sent, &text)).await;
            }
            _ = tokio::time::sleep(QUIET_FOR.saturating_sub(last_said.elapsed())) => {
                let text = match doing.as_str() {
                    "" => format!("Still working: {}s so far", started.elapsed().as_secs()),
                    step => format!("Still on {step}: {}s so far", started.elapsed().as_secs()),
                };
                sent += 1;
                last_said = std::time::Instant::now();
                write(stdout, &progress_note(&token, sent, &text)).await;
            }
        }
    };
    if let Ok(mut slot) = progress_slot().lock() {
        *slot = None;
    }
    out
}

fn progress_note(token: &Value, progress: u64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": { "progressToken": token, "progress": progress, "message": message },
    })
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

async fn call(name: &str, args: &Value, runs: &mut BTreeMap<u64, Run>) -> Result<Value, String> {
    let text = |key: &str| args[key].as_str().unwrap_or("").to_string();
    match name {
        "list_repos" => list_repos(&text("folder")).await,
        "list_flows" => list_flows(&repo_path(&text("repo"))?),
        "run_flow" => {
            let trusted = args["trusted"].as_bool().unwrap_or(false);
            run_flow(&text("repo"), &text("flow"), &text("pr"), trusted, runs).await
        }
        "approve" => {
            let id = args["run_id"].as_u64().ok_or("run_id is required")?;
            let exclude: Vec<String> = args["exclude"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let stop_asking = args["stop_asking"].as_bool().unwrap_or(false);
            approve(id, &exclude, stop_asking, runs).await
        }
        "reject" | "run_status" => {
            let id = args["run_id"].as_u64().ok_or("run_id is required")?;
            let run = runs.get_mut(&id).ok_or(format!("no run {id}"))?;
            match name {
                "reject" => run.decide(false, &[]).await.map(|()| report(run)),
                _ => Ok(report(run)),
            }
        }
        "trust_all" => Ok(trust_all(args["turn_off"].as_bool().unwrap_or(false))),
        "refresh" => refresh(&text("path")),
        "model_region" => model_region(&text("region"), &text("folder")),
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

/// Says so when a flow was stored under another id than the one given: a
/// client that keeps using its own id would find no such flow.
fn id_note(given: Option<&str>, saved: &str) -> Option<String> {
    let given = given.filter(|g| *g != saved)?;
    Some(format!(
        "stored as {saved}, not {given}: a flow you made takes the id its label \
         reads as. Use {saved} with get_flow, show_flow, delete_flow and run_flow."
    ))
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
mod tests;
