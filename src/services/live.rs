//! Runs GitAgent is driving without a window, as the window should see them.
//!
//! `gitagent mcp` is its own process: a client such as Claude starts it, and
//! it shares nothing with an open window but the files in the data directory.
//! `gitagent run` is the same, started from a terminal instead.
//! So after every step it writes what its runs look like here, and the window
//! reads that and shows it — step by step, read-only. Approvals stay with the
//! client that started the run: one place answers, never two.

use serde::{Deserialize, Serialize};

use super::graph::{Graph, RunState};
use super::store;

const FILE: &str = "mcp_runs.json";

/// How long a finished run stays on screen, so its end can be seen.
const KEEP_FINISHED_SECS: u64 = 10 * 60;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LiveStep {
    pub title: String,
    /// `NodeStatus::css`, so the window draws it like its own.
    pub status: String,
    /// `NodeStatus::label`.
    pub status_label: String,
    pub summary: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct LiveRun {
    /// The MCP server's process, so a run whose server is gone can be dropped.
    pub pid: u32,
    pub run: u64,
    pub repo: String,
    pub flow: String,
    pub flow_label: String,
    pub steps: Vec<LiveStep>,
    /// The step stopped for approval, and exactly what it would do.
    pub waiting: Option<(String, String)>,
    pub finished: bool,
    /// Answering its own approvals by the Trusted run's rules.
    #[serde(default)]
    pub trusted: bool,
    /// Started with `gitagent run` rather than by an MCP client, so its
    /// approvals are answered in that terminal.
    #[serde(default)]
    pub terminal: bool,
    /// Seconds since the epoch.
    pub updated: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct LiveFile {
    #[serde(default)]
    runs: Vec<LiveRun>,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path() -> std::path::PathBuf {
    store::data_dir().join(FILE)
}

fn read() -> LiveFile {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Whether the process that wrote a run is still there. Off Unix there is no
/// cheap way to ask, so a run is kept until it is old.
fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Worth showing: its server is still running, and it is either still going
/// or finished recently enough for its end to be seen.
fn current(run: &LiveRun, at: u64) -> bool {
    if run.finished {
        at.saturating_sub(run.updated) < KEEP_FINISHED_SECS
    } else {
        at.saturating_sub(run.updated) < 24 * 3600
    }
}

/// What one run looks like now, from the server's own state.
pub fn snapshot(
    run: u64,
    repo: &str,
    flow: &str,
    flow_label: &str,
    graph: &Graph,
    state: &RunState,
    trusted: bool,
) -> LiveRun {
    let steps = graph
        .nodes
        .iter()
        .map(|n| {
            let status = state.status(&n.id);
            LiveStep {
                title: n.title.clone(),
                status: status.css().to_string(),
                status_label: status.label().to_string(),
                summary: state
                    .runs
                    .get(&n.id)
                    .map(|r| r.summary.clone())
                    .unwrap_or_default(),
            }
        })
        .collect();
    let waiting = graph
        .nodes
        .iter()
        .find(|n| state.status(&n.id) == super::graph::NodeStatus::AwaitingApproval)
        .map(|n| {
            let proposal = state
                .runs
                .get(&n.id)
                .map(|r| r.proposal.clone())
                .unwrap_or_default();
            (n.title.clone(), proposal)
        });
    LiveRun {
        pid: std::process::id(),
        run,
        repo: repo.to_string(),
        flow: flow.to_string(),
        flow_label: flow_label.to_string(),
        steps,
        waiting,
        finished: state.is_finished(graph),
        trusted,
        terminal: false,
        updated: now(),
    }
}

/// Records one run's latest state, replacing its previous one and dropping
/// any whose server has gone.
pub fn publish(run: LiveRun) {
    let at = now();
    let mut file = read();
    file.runs
        .retain(|r| !(r.pid == run.pid && r.run == run.run) && current(r, at) && alive(r.pid));
    file.runs.push(run);
    if let Ok(text) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::create_dir_all(store::data_dir());
        let _ = store::write_atomic(&path(), text.as_bytes());
    }
}

/// Every run worth showing, newest first.
pub fn load() -> Vec<LiveRun> {
    let at = now();
    let mut runs: Vec<LiveRun> = read()
        .runs
        .into_iter()
        .filter(|r| current(r, at) && alive(r.pid))
        .collect();
    runs.sort_by_key(|r| std::cmp::Reverse(r.updated));
    runs
}

/// When the file last changed, so the window reads it only then.
pub fn changed_at() -> Option<std::time::SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(finished: bool, updated: u64) -> LiveRun {
        LiveRun {
            pid: std::process::id(),
            run: 1,
            repo: "/r".into(),
            flow: "commit_and_pr".into(),
            flow_label: "Commit → PR".into(),
            steps: vec![],
            waiting: None,
            finished,
            trusted: false,
            terminal: false,
            updated,
        }
    }

    #[test]
    fn a_run_written_before_terminals_reads_as_an_mcp_one() {
        let mut old = serde_json::to_value(run(false, 1)).unwrap();
        old.as_object_mut().unwrap().remove("terminal");
        let read: LiveRun = serde_json::from_value(old).unwrap();
        assert!(!read.terminal);
    }

    #[test]
    fn a_finished_run_shows_for_a_while_then_goes() {
        assert!(current(&run(true, 1000), 1000 + 60));
        assert!(!current(&run(true, 1000), 1000 + KEEP_FINISHED_SECS + 1));
        assert!(
            current(&run(false, 1000), 1000 + 3600),
            "a run waiting on you stays"
        );
    }

    #[test]
    fn a_run_says_which_step_waits_and_what_it_would_do() {
        let graph = crate::services::flowdef::FlowBook::defaults()
            .get("commit_and_pr")
            .unwrap()
            .to_graph();
        let mut state = RunState::fresh(&graph);
        state.started = true;
        state.set_status("preflight", super::super::graph::NodeStatus::Done);
        state.set_status("scan", super::super::graph::NodeStatus::AwaitingApproval);
        state.runs.get_mut("scan").unwrap().proposal = "git status --porcelain".into();
        let live = snapshot(
            7,
            "/r",
            "commit_and_pr",
            "Commit → PR",
            &graph,
            &state,
            false,
        );
        assert_eq!(live.steps[0].status, "done");
        assert_eq!(
            live.waiting,
            Some((
                "Scan changes".to_string(),
                "git status --porcelain".to_string()
            ))
        );
        assert!(!live.finished);
    }

    #[test]
    fn this_process_is_alive() {
        assert!(alive(std::process::id()));
    }
}
