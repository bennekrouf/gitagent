//! The MCP tools that look at repositories and run flows on them.

use super::*;

pub(super) async fn repo_insight(repo: &str) -> Result<Value, String> {
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

pub(super) async fn list_repos(folder: &str) -> Result<Value, String> {
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

pub(super) async fn run_flow(
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
        let run = runs
            .get(&id)
            .ok_or_else(|| format!("the run of {flow_id} went away"))?;
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
    let Some(mut last) = legs.pop() else {
        return Err("no flow ran".into());
    };
    if !legs.is_empty() {
        last["earlier_flows"] = json!(legs);
    }
    Ok(last)
}

/// One flow, started and run to its first stop. Returns the run's id.
pub(super) async fn start_flow(
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
pub(super) fn report(run: &Run) -> Value {
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

pub(super) fn head(text: &str, cap: usize) -> String {
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
