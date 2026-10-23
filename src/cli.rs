//! `gitagent run` — one flow, driven from a terminal, with no window.
//!
//! The run is `services::runner::Run`, the same one the MCP server drives, so
//! a step does here exactly what it does there. What this adds is a person at
//! a prompt: each approval is put to them in the terminal, and nothing gated
//! runs without their answer or, with `--trusted`, the Trusted run's rules.
//!
//! Without a terminal to ask (CI, a pipe, cron) the promise is kept the only
//! way it can be: the run stops at the first approval nobody answered and
//! says so in its exit code, rather than guessing.

use std::io::{BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::services::flowdef::FlowBook;
use crate::services::graph::{NodeSpec, NodeStatus};
use crate::services::runner::{self, Next, Progress, Run};
use crate::services::{probe, store, trusted};
use crate::telemetry;

/// The flow finished, and nothing in it failed or was declined.
const DONE: i32 = 0;
/// A step failed.
const FAILED: i32 = 1;
/// Nothing ran: the arguments, the folder or the flow were wrong.
const USAGE: i32 = 2;
/// Stopped at an approval nobody here could answer — no terminal, a trusted
/// run holding with no one to hand it to, or `q` at the prompt.
const HELD: i32 = 3;
/// Someone declined an approval.
const DECLINED: i32 = 4;

const HELP: &str = "\
Run a GitAgent flow from the terminal, without opening the window.

Usage: gitagent run [--repo PATH] [--flow FLOW] [--pr NUMBER] [--trusted]

  --repo PATH    the repository to run in (default: the current folder)
  --flow FLOW    the flow to run (default: the one the repository needs next)
  --pr NUMBER    the pull request a review or merge flow is about
  --trusted      answer approvals by the Trusted run's rules, and carry on
                 into the next flow the repository needs; it still stops at
                 anything risky, any script or deploy, and release notes

Every step that touches git history or the remote asks before it runs. With
no terminal to ask, the run stops there instead.

Exit codes:
  0  finished, nothing failed or was declined
  1  a step failed
  2  nothing ran: bad arguments, not a repository, or no such flow
  3  stopped at an approval nobody answered
  4  an approval was declined";

struct Args {
    repo: String,
    flow: Option<String>,
    pr: String,
    trusted: bool,
}

/// `None` when the person asked for help.
fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let mut out = Args {
        repo: ".".into(),
        flow: None,
        pr: String::new(),
        trusted: false,
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        // `--flow=x` as well as `--flow x`.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next().cloned())
                .filter(|v| !v.is_empty())
                .ok_or(format!("{name} needs a value"))
        };
        match name {
            "-h" | "--help" => return Ok(None),
            "--repo" => out.repo = value()?,
            "--flow" => out.flow = Some(value()?),
            "--pr" => {
                let pr = value()?;
                let pr = pr.trim_start_matches('#');
                if pr.parse::<u64>().is_err() {
                    return Err(format!("--pr takes a number, not {pr}"));
                }
                out.pr = pr.into();
            }
            "--trusted" => out.trusted = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Some(out))
}

pub fn main(args: &[String]) -> i32 {
    let args = match parse(args) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{HELP}");
            return DONE;
        }
        Err(why) => {
            eprintln!("gitagent run: {why}\n\n{HELP}");
            return USAGE;
        }
    };
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let code = runtime.block_on(run(args));
    // A finished run sends its statistics on a detached task; give it a
    // moment rather than cutting it off, and never more than that.
    runtime.block_on(async {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), telemetry::flush()).await;
    });
    code
}

async fn run(args: Args) -> i32 {
    let repo = match runner::repo_path(&args.repo) {
        Ok(repo) => repo,
        Err(why) => {
            eprintln!("gitagent run: {why}");
            return USAGE;
        }
    };
    let trusted = args.trusted || store::trust_all();
    let ask = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();

    let (mut flow, mut pr) = match args.flow {
        Some(flow) => (flow, args.pr.clone()),
        None => match suggested(&repo).await {
            Ok((flow, pr)) => (
                flow,
                if args.pr.is_empty() {
                    pr
                } else {
                    args.pr.clone()
                },
            ),
            Err(why) => {
                eprintln!(
                    "{}",
                    why.unwrap_or("Nothing for GitAgent to do here.".into())
                );
                return DONE;
            }
        },
    };

    let mut code = DONE;
    // A trusted run goes on into the next flow the repository needs, as the
    // window's does; anything else is one flow.
    for _ in 0..trusted::MOST_FLOWS {
        let mut run = match Run::prepare(&repo, &flow, &pr, trusted) {
            Ok(run) => run,
            Err(why) => {
                eprintln!("gitagent run: {why}\n\n{}", flows_for(&repo));
                return USAGE;
            }
        };
        run.terminal = true;
        run.progress = show;
        let label = FlowBook::load()
            .get(&run.flow)
            .map(|f| f.label.clone())
            .unwrap_or_else(|| run.flow.clone());
        eprintln!(
            "\n{label}{}{}",
            if pr.is_empty() {
                String::new()
            } else {
                format!(" for #{pr}")
            },
            if run.trusting() {
                " \u{b7} trusted"
            } else {
                ""
            }
        );

        run.advance().await;
        while let Some(node) = run.waiting() {
            if !ask {
                describe(&run, &node);
                eprintln!("\nStopped: no terminal to ask for approval.");
                return HELD;
            }
            let answer = match prompt(&run, &node) {
                Answer::Approve(exclude) => run.decide(true, &exclude).await,
                Answer::Decline => run.decide(false, &[]).await,
                Answer::Quit => {
                    eprintln!("Stopped at {}; nothing was approved.", node.title);
                    return HELD;
                }
            };
            if let Err(why) = answer {
                eprintln!("{why}");
            }
        }

        not_reached(&run);
        code = outcome(&run);
        produced(&run);
        if code != DONE {
            break;
        }
        match run.next().await {
            Next::Flow(next, next_pr) => (flow, pr) = (next, next_pr),
            Next::Stop(why) => {
                if let Some(why) = why {
                    eprintln!("\n{why}");
                }
                break;
            }
        }
    }
    code
}

/// The flow the repository needs next, or why there is none.
async fn suggested(repo: &str) -> Result<(String, String), Option<String>> {
    let status = probe::probe(repo).await;
    let hidden = store::load_repo_flows().hidden_for(repo);
    let book = FlowBook::load();
    trusted::next_flow(&book, &status, &hidden)
        .ok_or_else(|| trusted::why_stopped(&book, &status, &hidden))
}

fn flows_for(repo: &str) -> String {
    let hidden = store::load_repo_flows().hidden_for(repo);
    let book = FlowBook::load();
    let mut out = String::from("Flows for this repository:");
    for def in book.runnable_for(&hidden) {
        out.push_str(&format!("\n  {:<20} {}", def.id, def.label));
    }
    out
}

fn outcome(run: &Run) -> i32 {
    let statuses: Vec<NodeStatus> = run
        .graph
        .nodes
        .iter()
        .map(|n| run.state.status(&n.id))
        .collect();
    match telemetry::outcome_of(&statuses) {
        telemetry::Outcome::Failed => FAILED,
        telemetry::Outcome::Rejected => DECLINED,
        telemetry::Outcome::Done | telemetry::Outcome::Nothing => DONE,
    }
}

// ── What the terminal shows ───────────────────────────────────────────────

/// Whether the step now running has printed any output of its own, so a
/// failure is not printed a second time from its log.
static STREAMED: AtomicBool = AtomicBool::new(false);

fn show(progress: Progress<'_>) {
    match progress {
        Progress::Step(node) => {
            STREAMED.store(false, Ordering::Relaxed);
            eprintln!("\u{25b8} {}", node.title);
        }
        Progress::Line(line) => {
            STREAMED.store(true, Ordering::Relaxed);
            eprintln!("  \u{2502} {line}");
        }
        Progress::Trusted(node) => {
            eprintln!("\u{2713} approved by the trusted run: {}", node.title);
        }
        Progress::Settled(node, state) => {
            let status = state.status(&node.id);
            let entry = state.runs.get(&node.id).cloned().unwrap_or_default();
            let mark = match status {
                NodeStatus::Done => "\u{2713}",
                NodeStatus::Failed => "\u{2717}",
                _ => "\u{2013}",
            };
            let summary = if entry.summary.is_empty() {
                status.label()
            } else {
                &entry.summary
            };
            eprintln!("{mark} {} \u{2014} {summary}", node.title);
            if status == NodeStatus::Failed {
                if !STREAMED.load(Ordering::Relaxed) {
                    for line in last_lines(&entry.log, 20) {
                        eprintln!("  \u{2502} {line}");
                    }
                }
                for fix in &entry.remedies {
                    eprintln!("  fix: {} \u{2014} {}", fix.label, fix.display);
                }
            }
        }
    }
}

fn last_lines(text: &str, count: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(count)..].to_vec()
}

/// What a step stopped for approval will do, as the window's card says it.
fn describe(run: &Run, node: &NodeSpec) {
    let entry = run.state.runs.get(&node.id).cloned().unwrap_or_default();
    eprintln!("\n? {} needs approval", node.title);
    if !entry.held.is_empty() {
        eprintln!("  The trusted run stopped here: {}", entry.held);
    }
    for line in entry.proposal.lines() {
        eprintln!("    {line}");
    }
}

fn list_files(run: &Run, node: &NodeSpec, excluded: &[String]) {
    let Some(entry) = run.state.runs.get(&node.id) else {
        return;
    };
    for (i, item) in entry.items.iter().enumerate() {
        let mark = if excluded.contains(&item.key) {
            " "
        } else {
            "x"
        };
        eprintln!("  [{mark}] {:>2}  {}  {}", i + 1, item.label, item.note);
    }
}

enum Answer {
    Approve(Vec<String>),
    Decline,
    Quit,
}

/// Puts one approval to the person until they answer it.
fn prompt(run: &Run, node: &NodeSpec) -> Answer {
    describe(run, node);
    let entry = run.state.runs.get(&node.id).cloned().unwrap_or_default();
    let mut excluded: Vec<String> = vec![];
    list_files(run, node, &excluded);

    let stdin = std::io::stdin();
    loop {
        let files = if entry.items.is_empty() {
            ""
        } else {
            ", [x N\u{2026}] leave files out"
        };
        let diff = if entry.preview_diff.trim().is_empty() {
            ""
        } else {
            ", [d]iff"
        };
        eprint!("Approve? [y]es, [n]o{diff}{files}, [q]uit: ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        // End of input is nobody left to answer.
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            eprintln!();
            return Answer::Quit;
        }
        let line = line.trim();
        match line {
            "y" | "yes" => return Answer::Approve(excluded),
            "n" | "no" => return Answer::Decline,
            "q" | "quit" => return Answer::Quit,
            "d" | "diff" if !diff.is_empty() => eprintln!("{}", entry.preview_diff),
            _ if !files.is_empty() && (line.starts_with("x ") || line == "x") => {
                // Toggles, so a file left out by mistake can be put back.
                let picks: Vec<usize> = line[1..]
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter_map(|n| n.parse().ok())
                    .collect();
                for pick in picks {
                    let Some(item) = pick.checked_sub(1).and_then(|i| entry.items.get(i)) else {
                        eprintln!("no file {pick}");
                        continue;
                    };
                    if let Some(at) = excluded.iter().position(|k| k == &item.key) {
                        excluded.remove(at);
                    } else {
                        excluded.push(item.key.clone());
                    }
                }
                list_files(run, node, &excluded);
            }
            _ => {}
        }
    }
}

/// The steps a decline or a failure kept from running.
fn not_reached(run: &Run) {
    for node in &run.graph.nodes {
        let status = run.state.status(&node.id);
        if matches!(status, NodeStatus::Rejected | NodeStatus::Blocked) {
            eprintln!("\u{b7} {} \u{2014} {}", node.title, status.label());
        }
    }
}

/// The short things the run made — a branch, a pull request's URL — on
/// stdout, one `key: value` a line, for a script to pick up.
fn produced(run: &Run) {
    for (key, value) in &run.state.artifacts {
        if key.contains('.') || value.len() > 300 || value.trim().is_empty() || value.contains('\n')
        {
            continue;
        }
        println!("{key}: {value}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Option<Args>, String> {
        parse(&list.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn reads_both_spellings_of_a_value() {
        let a = args(&["--flow", "commit_and_pr", "--pr=#12", "--trusted"])
            .unwrap()
            .unwrap();
        assert_eq!(a.flow.as_deref(), Some("commit_and_pr"));
        assert_eq!(a.pr, "12");
        assert!(a.trusted);
        assert_eq!(a.repo, ".");
    }

    #[test]
    fn refuses_what_it_cannot_use() {
        assert!(args(&["--pr", "abc"]).is_err());
        assert!(args(&["--flow"]).is_err());
        assert!(args(&["--yes"]).is_err());
        assert!(args(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn keeps_the_end_of_a_log() {
        assert_eq!(last_lines("a\nb\nc", 2), vec!["b", "c"]);
        assert_eq!(last_lines("a", 5), vec!["a"]);
    }
}
