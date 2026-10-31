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
    let id = start_flow(repo, flow_id, pr, trusted, runs).await?;
    carry_on(id, runs).await
}

/// The person's yes to the step a run is waiting on. With `stop_asking`, as
/// the window's "Stop asking me": the rest of this run is trusted from here,
/// held where a Trusted run would hold, and goes on into the next flow the
/// repository needs once this one is done.
pub(super) async fn approve(
    id: u64,
    exclude: &[String],
    stop_asking: bool,
    runs: &mut BTreeMap<u64, Run>,
) -> Result<Value, String> {
    let run = runs.get_mut(&id).ok_or(format!("no run {id}"))?;
    if run.waiting().is_none() {
        return Err("this run is not waiting on an approval".into());
    }
    if stop_asking {
        // A hold is a trusted run handing a step back to the person; the
        // person has now answered it, and said to stop asking.
        run.trusted = true;
        run.held = None;
    }
    run.decide(true, exclude).await?;
    carry_on(id, runs).await
}

/// One run's report, and — when it was trusted through to its end — each
/// next flow the repository needs, as the window's trusted run goes on.
async fn carry_on(first: u64, runs: &mut BTreeMap<u64, Run>) -> Result<Value, String> {
    let mut legs: Vec<Value> = vec![];
    let mut id = first;
    for leg in 1..=trusted::MOST_FLOWS {
        let run = runs.get(&id).ok_or(format!("run {id} went away"))?;
        legs.push(report(run));
        match run.next().await {
            Next::Flow(next, next_pr) if leg < trusted::MOST_FLOWS => {
                let repo = run.repo.clone();
                id = start_flow(&repo, &next, &next_pr, true, runs).await?;
            }
            Next::Flow(..) => break,
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
        last["earlier_flows"] = json!(legs.iter().map(brief).collect::<Vec<_>>());
    }
    Ok(last)
}

/// The Refresh button of every GitAgent window open on `path`: a folder
/// refreshes every repository in it, one repository just that one, and no
/// path every open window's every repository. Answers at once; the windows
/// pick it up within a second, and list_repos reads the same repositories
/// fresh for the client.
pub(super) fn refresh(path: &str) -> Result<Value, String> {
    let path = path.trim();
    if path.is_empty() {
        store::request_refresh(None);
        return Ok(json!({
            "refreshing": "every repository in every open GitAgent window",
        }));
    }
    if !std::path::Path::new(path).is_dir() {
        return Err(format!("{path} is not a folder"));
    }
    store::request_refresh(Some(path));
    Ok(json!({
        "refreshing": path,
        "note": "A GitAgent window open on this folder, or on the folder this repository is \
                 in, refreshes it now. list_repos gives the result here.",
    }))
}

/// The api0 region the model runs in: read, or switched. `folder` scopes it
/// as Settings in the window on that folder does; without one, it is the
/// default every folder without a choice of its own runs on.
pub(super) fn model_region(region: &str, folder: &str) -> Result<Value, String> {
    let folder = folder.trim().trim_end_matches('/');
    let folder = (!folder.is_empty()).then_some(folder);
    if let Some(f) = folder {
        if !std::path::Path::new(f).is_dir() {
            return Err(format!("{f} is not a folder"));
        }
    }
    let region = region.trim();
    if !region.is_empty() {
        let chosen = region_named(region).ok_or(format!(
            "no region {region}; the regions are {}",
            llm::REGIONS
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
                .join(", ")
        ))?;
        // The folder whose choice a run there reads — the nearest one above
        // it that chose — so the switch lands where it takes effect.
        let models = store::load_folder_models();
        // Failing that, the folder a window opens it in: a repository's own
        // choice would be one no window ever shows.
        let within =
            |k: &str, f: &str| f == k || f.starts_with(&format!("{}/", k.trim_end_matches('/')));
        let scope = folder.map(|f| {
            let nearest = |keys: Vec<&String>| {
                keys.into_iter()
                    .filter(|k| within(k, f))
                    .max_by_key(|k| k.len())
                    .cloned()
            };
            nearest(models.by_folder.keys().collect())
                .or_else(|| nearest(store::load_registry().recent.iter().collect()))
                .unwrap_or_else(|| f.to_string())
        });
        let mut cfg = store::load_settings_for(scope.as_deref());
        cfg.use_region(chosen.id);
        store::save_settings_for(scope.as_deref(), &cfg);
    }
    let cfg = store::load_settings_for(folder);
    let shared = store::load_settings();
    let own: Vec<Value> = store::load_folder_models()
        .by_folder
        .iter()
        .map(|(f, choice)| {
            let cfg = shared.with_model_choice(choice).with_region_filled();
            json!({ "folder": f, "runs_on": runs_on(&cfg) })
        })
        .collect();
    let mut out = json!({
        "runs_on": runs_on(&cfg),
        "for": folder.unwrap_or("folders without a model of their own"),
        "default": runs_on(&shared),
        "folders_with_their_own": own,
    });
    if cfg.active_region().is_some() && llm::key_for(llm::remote("api0")).is_none() {
        out["licence"] =
            json!("api0 needs a licence on this computer; the user adds it in the GitAgent window");
    }
    Ok(out)
}

/// A region by its id, its name, or the short name people use.
pub(super) fn region_named(name: &str) -> Option<&'static llm::Region> {
    let name = name.trim().to_lowercase();
    let id = match name.as_str() {
        "eu" | "europe" => "europe",
        "cn" | "china" => "china",
        "us" | "usa" | "united states" => "usa",
        other => other,
    };
    llm::REGIONS.iter().find(|r| r.id == id)
}

fn runs_on(cfg: &llm::LlmConfig) -> String {
    match cfg.active_region() {
        Some(r) => format!("api0 in {}", r.label),
        None => cfg.active_model().to_string(),
    }
}

/// "Trust all", as the GitAgent window's top bar has it. A client may turn it
/// off, never on: that is a person's choice for every repository at once,
/// made in the window.
pub(super) fn trust_all(turn_off: bool) -> Value {
    if turn_off && store::trust_all() {
        store::save_trust_all(false);
        return json!({
            "trust_all": false,
            "note": "Trust all is off. Runs already going keep the trust they were started \
                     with; every new one asks again.",
        });
    }
    json!({
        "trust_all": store::trust_all(),
        "note": if store::trust_all() {
            "On: every run answers its own approvals by the Trusted run's rules. Only the \
             user can turn it on, in the GitAgent window; pass turn_off: true to turn it off."
        } else {
            "Off: runs ask, unless started with trusted: true or approved with \
             stop_asking: true. Only the user can turn it on, in the GitAgent window."
        },
    })
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
    let mut run = Run::prepare(&repo, flow_id, pr, trusted, busy::By::Claude).map_err(|e| {
        if e.starts_with("no flow ") {
            format!("{e}; see list_flows")
        } else {
            e
        }
    })?;
    run.progress = super::report_progress;
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
            let status = state.status(&n.id);
            let mut step = json!({
                "id": n.id,
                "title": n.title,
                "status": status.label(),
            });
            // "failed" on a failed step says nothing its status doesn't.
            if !entry.summary.is_empty() && entry.summary != status.label() {
                step["summary"] = json!(entry.summary);
            }
            if status == NodeStatus::Failed {
                step["log"] = json!(tail(&entry.log, LOG_CAP));
                if !entry.remedies.is_empty() {
                    step["fixes"] = json!(entry
                        .remedies
                        .iter()
                        .map(|r| json!({ "label": r.label, "command": r.display }))
                        .collect::<Vec<_>>());
                }
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
    let mut produced: BTreeMap<&String, &String> = state
        .artifacts
        .iter()
        .filter(|(k, v)| !k.contains('.') && v.len() <= 300 && !v.trim().is_empty())
        .collect();
    // Nor is anything said twice: a draft already reads as its step's
    // summary, file_notes names every path with what happened to it, and the
    // branch committed on is the branch drafted unless it had to change.
    let summaries: Vec<&str> = steps.iter().filter_map(|s| s["summary"].as_str()).collect();
    produced.retain(|_, v| !summaries.contains(&v.as_str()));
    if produced.keys().any(|k| *k == "file_notes") {
        produced.retain(|k, _| *k != "commit_paths" && *k != "untracked");
    }
    let work_branch = produced
        .iter()
        .find(|(k, _)| **k == "work_branch")
        .map(|(_, v)| *v);
    if let Some(work) = work_branch {
        produced.retain(|k, v| *k != "branch_name" || *v != work);
    }

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

/// A flow a trusted run already finished, as the report of the flow it went
/// on to carries it: what it made and one line per step that did something.
/// Its diffs and approvals are behind it; the client only needs to say so.
pub(super) fn brief(report: &Value) -> Value {
    let steps: Vec<String> = report["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let summary = s["summary"].as_str().filter(|t| !t.is_empty())?;
            Some(format!("{}: {summary}", s["title"].as_str().unwrap_or("")))
        })
        .collect();
    let mut out = json!({
        "run_id": report["run_id"],
        "flow": report["flow"],
        "phase": report["phase"],
        "steps": steps,
        "produced": report["produced"],
    });
    for key in ["approved_automatically", "chain_stopped"] {
        if !report[key].is_null() {
            out[key] = report[key].clone();
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
