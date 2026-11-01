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

use desktop_kit::mcp::{self, Incoming, Server};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::services::flowdef::{self, FlowBook, FlowDef};
use crate::services::graph::{NodeKind, NodeStatus};
use crate::services::runner::{self, repo_path, Next, Run};
use crate::services::{busy, catalogue, live, llm, probe, store, trusted};
mod flows;
use flows::*;
mod run;
use run::*;

/// What the client is told: the plumbing is desktop-kit's, the tools are below.
const SERVER: Server = Server {
    name: "gitagent",
    version: env!("CARGO_PKG_VERSION"),
    instructions: INSTRUCTIONS,
    tools,
    library: false,
};

/// How much of a diff or a log one tool result carries. A client's context is
/// the budget here, not the screen: the end of a log is where a failure is,
/// and the start of a diff is enough to see what kind of change it is.
const DIFF_CAP: usize = 8_000;
const LOG_CAP: usize = 4_000;

const INSTRUCTIONS: &str = "GitAgent runs git work as flows of steps: commit and open a pull \
request, review and merge one, cut a release. Start with list_repos to see what each repository \
needs, then run_flow. Any step that touches git history or the remote stops and returns what it \
will do; show that to the user and call approve or reject with their answer. Never approve on \
the user's behalf without asking them, and run trusted only when the user asks for it. The \
user may also answer a waiting step in the GitAgent window: if approve or reject says the run \
is not waiting, call run_status to see where it went. After a \
trusted run, report what was approved automatically and where it stopped. \
Before calling these tools, read the gitagent skill if one is available: it has the confirmation \
rules, defaults and report layouts that these instructions leave out.";

pub fn serve() {
    // As the window does at startup: a claim whose server was killed with the
    // Claude session that started it holds nothing any more.
    busy::sweep();
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
    let mut runs: BTreeMap<u64, Run> = BTreeMap::new();

    // Between calls, an approval answered in the window instead of in Claude.
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let line = tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => line,
                _ => break,
            },
            _ = tick.tick() => {
                apply_window_answers(&mut runs).await;
                continue;
            }
        };
        let call = match SERVER.read(&line) {
            Some(Incoming::Call(call)) => call,
            Some(Incoming::Reply(reply)) => {
                mcp::write(&reply);
                continue;
            }
            None => continue,
        };
        let outcome = if call.progress.is_null() {
            self::call(&call.name, &call.args, &mut runs).await
        } else {
            with_progress(
                call.progress.clone(),
                self::call(&call.name, &call.args, &mut runs),
            )
            .await
        };
        mcp::write(&SERVER.answer(&call, outcome));
    }
}

/// Applies what the window answered for this server's runs, as `approve` and
/// `reject` would. Claude sees where the run went on with `run_status`.
async fn apply_window_answers(runs: &mut BTreeMap<u64, Run>) {
    for answer in live::take_answers() {
        let Some(run) = runs.get_mut(&answer.run) else {
            continue;
        };
        if run.waiting().map(|n| n.id) != Some(answer.step) {
            continue;
        }
        let _ = if answer.approved {
            approve(answer.run, &[], false, runs).await.map(|_| ())
        } else {
            run.decide(false, &[]).await
        };
    }
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
async fn with_progress<F>(token: Value, call: F) -> F::Output
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
                mcp::write(&progress_note(&token, sent, &text));
            }
            _ = tokio::time::sleep(QUIET_FOR.saturating_sub(last_said.elapsed())) => {
                let text = match doing.as_str() {
                    "" => format!("Still working: {}s so far", started.elapsed().as_secs()),
                    step => format!("Still on {step}: {}s so far", started.elapsed().as_secs()),
                };
                sent += 1;
                last_said = std::time::Instant::now();
                mcp::write(&progress_note(&token, sent, &text));
            }
        }
    };
    if let Ok(mut slot) = progress_slot().lock() {
        *slot = None;
    }
    out
}

fn progress_note(token: &Value, progress: u64, message: &str) -> Value {
    mcp::notification(
        "notifications/progress",
        json!({ "progressToken": token, "progress": progress, "message": message }),
    )
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
