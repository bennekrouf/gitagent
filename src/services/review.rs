//! The review-and-merge flow: take an open pull request, put its CI status and
//! a model's read of the diff side by side, and let the human merge it.
//!
//! ```text
//!                ┌──> pr_status ──┐
//!   find_pr ──>  ┤                ├──> merge ──> sync
//!                └──> pr_diff ──> analyse ──┘
//! ```
//!
//! `pr_status` asks the forge; `pr_diff` asks git. Neither reads the other, so
//! they are siblings rather than a sequence. `merge` joins them because the
//! decision needs both — a green build the model dislikes, and a red build the
//! model is happy with, are different situations and the approval shows which
//! one you are in.
//!
//! Nothing here blocks a merge on its own. Their `fmt` check is deliberately
//! non-blocking, so a rule like "refuse when any check is red" would refuse
//! every pull request they open. The gate is the human, given the full picture.

use serde_json::json;

use super::flow::{StepFailure, StepOutcome};
use super::forge::Forge;
use super::git;
use super::graph::{Graph, NodeKind, RunState, Step};
use super::llm::{complete_json, LlmConfig};

pub fn review_and_merge_flow() -> Graph {
    use super::flow::node;
    Graph {
        nodes: vec![
            node(
                "find_pr",
                "Find the pull request",
                "The open PR for this branch",
                Step::FindPr,
                NodeKind::Deterministic,
            )
            .writes(&["pr_number", "pr_title", "pr_url", "pr_base", "pr_head"])
            .done(),
            node(
                "pr_status",
                "CI status",
                "Checks and mergeability, from the forge",
                Step::PrStatus,
                NodeKind::Deterministic,
            )
            .after(&["find_pr"])
            .reads(&["pr_number"])
            .writes(&["checks_summary", "checks_state", "merge_state"])
            .done(),
            node(
                "pr_diff",
                "Fetch the diff",
                "git diff base...head — no forge involved",
                Step::PrDiff,
                NodeKind::Deterministic,
            )
            .after(&["find_pr"])
            .reads(&["pr_base", "pr_head"])
            .writes(&["pr_diff", "pr_stat"])
            .done(),
            node(
                "analyse",
                "Analyse for regressions",
                "Model call — what could this break?",
                Step::Analyse,
                NodeKind::Model,
            )
            .after(&["pr_diff"])
            .reads(&["pr_diff", "pr_stat", "pr_title"])
            .writes(&["verdict", "analysis", "finding_count"])
            .done(),
            node(
                "merge",
                "Merge",
                "Squash and delete the branch",
                Step::Merge,
                NodeKind::Deterministic,
            )
            .after(&["pr_status", "analyse"])
            .reads(&["pr_number", "checks_summary", "verdict", "analysis"])
            .writes(&["merge_output"])
            .gated()
            .done(),
            node(
                "sync",
                "Back to base",
                "Checkout the base branch and pull",
                Step::Sync,
                NodeKind::Deterministic,
            )
            .after(&["merge"])
            .reads(&["pr_base"])
            .writes(&["sync_output"])
            .done(),
        ],
    }
}

/// The merge approval is the whole point of this flow: both signals, stated
/// plainly, with the disagreement visible when there is one.
pub fn proposal(step: Step, state: &RunState) -> String {
    match step {
        Step::Merge => format!(
            "gh pr merge {} --squash --delete-branch\n\n\
             ── CI ────────────────────────────────\n{}\n\
             merge state: {}\n\n\
             ── Model ─────────────────────────────\nverdict: {}\n\n{}",
            state.artifact("pr_number"),
            state.artifact("checks_summary"),
            state.artifact("merge_state"),
            state.artifact("verdict"),
            state.artifact("analysis"),
        ),
        _ => String::new(),
    }
}

pub async fn execute(
    step: Step,
    repo: &str,
    cfg: &LlmConfig,
    state: &RunState,
) -> Result<StepOutcome, StepFailure> {
    match step {
        Step::FindPr => find_pr(repo, state).await,
        Step::PrStatus => pr_status(repo, state).await,
        Step::PrDiff => pr_diff(repo, state).await,
        Step::Analyse => analyse(cfg, state).await,
        Step::Merge => merge(repo, state).await,
        Step::Sync => sync(repo, state).await,
        _ => Err(StepFailure::from("step does not belong to this flow")),
    }
}

async fn find_pr(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    let branch = git::current_branch(repo).await?;

    match forge {
        Forge::GitHub => {
            let out = git::run(
                repo,
                "gh",
                &[
                    "pr",
                    "view",
                    "--json",
                    "number,title,url,state,baseRefName,headRefName,isDraft",
                ],
            )
            .await
            .map_err(|e| {
                if e.contains("no pull requests found") || e.contains("no open pull requests") {
                    StepFailure::from(format!(
                        "No open pull request for `{branch}`. Run the commit flow first."
                    ))
                } else {
                    StepFailure::from(e)
                }
            })?;

            let value: serde_json::Value = serde_json::from_str(&out)
                .map_err(|e| format!("could not read the gh response: {e}"))?;

            let number = value["number"].as_i64().unwrap_or_default().to_string();
            let title = value["title"].as_str().unwrap_or_default().to_string();
            let url = value["url"].as_str().unwrap_or_default().to_string();
            let base = value["baseRefName"]
                .as_str()
                .unwrap_or("master")
                .to_string();
            let head = value["headRefName"].as_str().unwrap_or(&branch).to_string();
            let draft = value["isDraft"].as_bool().unwrap_or(false);

            Ok(StepOutcome {
                summary: format!("#{number} {title}"),
                log: format!(
                    "#{number}  {title}\n{url}\n\n{head} → {base}{}",
                    if draft { "\n\nThis is a draft." } else { "" }
                ),
                artifacts: vec![
                    ("pr_number".into(), number),
                    ("pr_title".into(), title),
                    ("pr_url".into(), url),
                    ("pr_base".into(), base),
                    ("pr_head".into(), head),
                ],
            })
        }
        Forge::AzureDevOps => {
            let out = git::run(
                repo,
                "az",
                &[
                    "repos",
                    "pr",
                    "list",
                    "--source-branch",
                    &branch,
                    "--status",
                    "active",
                    "--output",
                    "json",
                ],
            )
            .await?;
            let list: serde_json::Value =
                serde_json::from_str(&out).map_err(|e| format!("could not read az output: {e}"))?;
            let pr = list
                .as_array()
                .and_then(|a| a.first())
                .ok_or_else(|| format!("No active pull request for `{branch}`."))?;

            let number = pr["pullRequestId"].as_i64().unwrap_or_default().to_string();
            let title = pr["title"].as_str().unwrap_or_default().to_string();
            let base = pr["targetRefName"]
                .as_str()
                .unwrap_or("refs/heads/main")
                .trim_start_matches("refs/heads/")
                .to_string();
            let web = pr["repository"]["webUrl"].as_str().unwrap_or_default();
            let url = format!("{web}/pullrequest/{number}");

            Ok(StepOutcome {
                summary: format!("!{number} {title}"),
                log: format!("!{number}  {title}\n{url}\n\n{branch} → {base}"),
                artifacts: vec![
                    ("pr_number".into(), number),
                    ("pr_title".into(), title),
                    ("pr_url".into(), url),
                    ("pr_base".into(), base),
                    ("pr_head".into(), branch),
                ],
            })
        }
        other => Err(StepFailure::from(format!(
            "Cannot look up pull requests on {}.",
            other.label()
        ))),
    }
}

async fn pr_status(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    if forge != Forge::GitHub {
        // Azure policy evaluation is a different shape entirely; saying so beats
        // reporting a green build that was never checked.
        return Ok(StepOutcome {
            summary: "not available".into(),
            log: "Check status is only read from GitHub so far. Review the policies \
                  in the browser before merging."
                .into(),
            artifacts: vec![
                (
                    "checks_summary".into(),
                    "unknown — not read on this forge".into(),
                ),
                ("checks_state".into(), "unknown".into()),
                ("merge_state".into(), "unknown".into()),
            ],
        });
    }

    let out = git::run(
        repo,
        "gh",
        &[
            "pr",
            "view",
            "--json",
            "statusCheckRollup,mergeable,mergeStateStatus",
        ],
    )
    .await?;
    let value: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("could not read the gh response: {e}"))?;

    let mut lines = vec![];
    let (mut pass, mut fail, mut pending) = (0, 0, 0);
    if let Some(checks) = value["statusCheckRollup"].as_array() {
        for check in checks {
            let name = check["name"]
                .as_str()
                .or_else(|| check["context"].as_str())
                .unwrap_or("check");
            let status = check["status"].as_str().unwrap_or("");
            let conclusion = check["conclusion"]
                .as_str()
                .or_else(|| check["state"].as_str())
                .unwrap_or("");

            let verdict = if status == "IN_PROGRESS" || status == "QUEUED" || status == "PENDING" {
                pending += 1;
                "pending"
            } else if matches!(conclusion, "SUCCESS" | "NEUTRAL" | "SKIPPED") {
                pass += 1;
                "pass"
            } else {
                fail += 1;
                "fail"
            };
            lines.push(format!("  {verdict:<8} {name}"));
        }
    }

    let merge_state = value["mergeStateStatus"]
        .as_str()
        .unwrap_or("UNKNOWN")
        .to_string();
    let mergeable = value["mergeable"].as_str().unwrap_or("UNKNOWN");
    let state_word = if fail > 0 {
        "failing"
    } else if pending > 0 {
        "pending"
    } else {
        "passing"
    };

    let summary = format!(
        "{pass} passing, {fail} failing, {pending} pending\n{}",
        lines.join("\n")
    );

    Ok(StepOutcome {
        summary: format!("{state_word} · {merge_state}"),
        log: format!("{summary}\n\nmergeable:   {mergeable}\nmerge state: {merge_state}"),
        artifacts: vec![
            ("checks_summary".into(), summary),
            ("checks_state".into(), state_word.into()),
            ("merge_state".into(), merge_state),
        ],
    })
}

/// The diff comes from git, not the forge — one code path for both platforms,
/// and no dependency on a CLI being installed to read it.
async fn pr_diff(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let base = state.artifact("pr_base");
    let head = state.artifact("pr_head");
    let _ = git::run(repo, "git", &["fetch", "origin", base, head]).await;

    let range = format!("origin/{base}...origin/{head}");
    let stat = git::run(repo, "git", &["diff", &range, "--stat"])
        .await
        .unwrap_or_default();
    let diff = git::run(repo, "git", &["diff", &range, "--unified=3"]).await?;

    if diff.trim().is_empty() {
        return Err(StepFailure::from(format!(
            "No difference between origin/{base} and origin/{head}."
        )));
    }

    let files = stat.lines().count().saturating_sub(1);
    Ok(StepOutcome {
        summary: format!("{files} file(s)"),
        log: stat.trim().to_string(),
        artifacts: vec![
            ("pr_diff".into(), git::cap(&diff)),
            ("pr_stat".into(), stat.trim().to_string()),
        ],
    })
}

async fn analyse(cfg: &LlmConfig, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let system = "You review a pull request diff for regressions in a Rust desktop application.\n\
        Report only what the diff itself shows. Rules:\n\
        - `verdict`: `looks_safe` if you found nothing concrete, `worth_a_look` for \
          plausible problems, `risky` for a specific likely break.\n\
        - Each finding needs a `claim` (the defect in one sentence), a `trigger` \
          (concrete inputs or state that produce it), and `evidence` quoted \
          VERBATIM from the diff above. If you cannot quote the diff, do not \
          report the finding.\n\
        - Report nothing rather than something vague. \"Consider adding tests\" and \
          \"verify error handling\" are not findings.";

    let user = format!(
        "Title: {}\n\nDiffstat:\n{}\n\nDiff:\n{}",
        state.artifact("pr_title"),
        state.artifact("pr_stat"),
        state.artifact("pr_diff"),
    );

    let schema = json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["looks_safe", "worth_a_look", "risky"] },
            "summary": { "type": "string" },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "severity": { "type": "string", "enum": ["high", "medium", "low"] },
                        "file":     { "type": "string" },
                        "claim":    { "type": "string" },
                        "trigger":  { "type": "string" },
                        "evidence": { "type": "string" }
                    },
                    "required": ["severity", "file", "claim", "trigger", "evidence"]
                }
            }
        },
        "required": ["verdict", "summary", "findings"]
    });

    let value = complete_json(cfg, system, &user, &schema).await?;
    let verdict = value["verdict"]
        .as_str()
        .unwrap_or("worth_a_look")
        .to_string();
    let summary = value["summary"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    let diff = state.artifact("pr_diff");

    let mut kept = vec![];
    let mut dropped = 0;
    for finding in value["findings"].as_array().cloned().unwrap_or_default() {
        let evidence = finding["evidence"].as_str().unwrap_or_default();
        // The cheapest hallucination filter there is: if the quote is not in
        // what we sent, the finding is about code that does not exist.
        if evidence.trim().is_empty() || !diff.contains(evidence.trim()) {
            dropped += 1;
            continue;
        }
        kept.push(format!(
            "[{}] {}\n     file:    {}\n     trigger: {}\n     evidence: {}",
            finding["severity"].as_str().unwrap_or("low"),
            finding["claim"].as_str().unwrap_or_default(),
            finding["file"].as_str().unwrap_or_default(),
            finding["trigger"].as_str().unwrap_or_default(),
            evidence.trim(),
        ));
    }

    let mut analysis = summary.clone();
    if !kept.is_empty() {
        analysis.push_str(&format!("\n\n{}", kept.join("\n\n")));
    }
    if dropped > 0 {
        analysis.push_str(&format!(
            "\n\n({dropped} finding(s) dropped — their quoted evidence was not in the diff.)"
        ));
    }

    Ok(StepOutcome {
        summary: format!("{verdict} · {} finding(s)", kept.len()),
        log: analysis.clone(),
        artifacts: vec![
            ("verdict".into(), verdict),
            ("analysis".into(), analysis),
            ("finding_count".into(), kept.len().to_string()),
        ],
    })
}

async fn merge(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    let number = state.artifact("pr_number").to_string();

    let out = match forge {
        Forge::GitHub => {
            git::run(
                repo,
                "gh",
                &["pr", "merge", &number, "--squash", "--delete-branch"],
            )
            .await?
        }
        Forge::AzureDevOps => {
            git::run(
                repo,
                "az",
                &[
                    "repos",
                    "pr",
                    "update",
                    "--id",
                    &number,
                    "--status",
                    "completed",
                    "--delete-source-branch",
                    "true",
                    "--output",
                    "none",
                ],
            )
            .await?
        }
        other => {
            return Err(StepFailure::from(format!(
                "Cannot merge on {}.",
                other.label()
            )))
        }
    };

    Ok(StepOutcome {
        summary: format!("merged #{number}"),
        log: if out.trim().is_empty() {
            format!("merged #{number}")
        } else {
            out.clone()
        },
        artifacts: vec![("merge_output".into(), out)],
    })
}

async fn sync(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let base = state.artifact("pr_base");
    let mut log = git::run(repo, "git", &["checkout", base]).await?;
    log.push_str(&git::run(repo, "git", &["pull", "--ff-only"]).await?);
    Ok(StepOutcome {
        summary: format!("on {base}, up to date"),
        log: log.trim().to_string(),
        artifacts: vec![("sync_output".into(), log)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::graph::NodeStatus;

    #[test]
    fn every_dependency_names_a_node_that_exists() {
        let g = review_and_merge_flow();
        for n in &g.nodes {
            for d in &n.deps {
                assert!(g.get(d).is_some(), "{} depends on unknown node {d}", n.id);
            }
        }
    }

    #[test]
    fn the_ci_check_and_the_diff_do_not_wait_on_each_other() {
        let g = review_and_merge_flow();
        let status = g.get("pr_status").unwrap();
        let diff = g.get("pr_diff").unwrap();
        assert!(!status.deps.contains(&"pr_diff".to_string()));
        assert!(!diff.deps.contains(&"pr_status".to_string()));
    }

    #[test]
    fn the_merge_decision_waits_for_both_signals() {
        let g = review_and_merge_flow();
        let merge = g.get("merge").unwrap();
        assert!(merge.deps.contains(&"pr_status".to_string()));
        assert!(merge.deps.contains(&"analyse".to_string()));
    }

    #[test]
    fn merging_is_the_only_gated_step() {
        let g = review_and_merge_flow();
        for node in &g.nodes {
            assert_eq!(
                node.requires_approval,
                node.id == "merge",
                "{} gating is wrong",
                node.id
            );
        }
    }

    #[test]
    fn the_flow_ends_by_returning_to_the_base_branch() {
        let g = review_and_merge_flow();
        let mut s = RunState::fresh(&g);
        let mut order = vec![];
        while let Some(n) = s.next_ready(&g) {
            order.push(n.id.clone());
            s.set_status(&n.id, NodeStatus::Done);
        }
        assert_eq!(order.first().unwrap(), "find_pr");
        assert_eq!(order.last().unwrap(), "sync");
    }

    #[test]
    fn declining_the_merge_leaves_the_branch_checked_out() {
        let g = review_and_merge_flow();
        let mut s = RunState::fresh(&g);
        for id in ["find_pr", "pr_status", "pr_diff", "analyse"] {
            s.set_status(id, NodeStatus::Done);
        }
        s.set_status("merge", NodeStatus::Rejected);
        s.propagate_block(&g);
        assert_eq!(s.status("sync"), NodeStatus::Blocked);
    }

    #[test]
    fn the_merge_approval_shows_ci_and_the_model_side_by_side() {
        let mut s = RunState::default();
        s.artifacts.insert("pr_number".into(), "7".into());
        s.artifacts
            .insert("checks_summary".into(), "1 passing, 1 failing".into());
        s.artifacts.insert("merge_state".into(), "UNSTABLE".into());
        s.artifacts.insert("verdict".into(), "worth_a_look".into());
        s.artifacts
            .insert("analysis".into(), "the lockfile moved".into());

        let text = proposal(Step::Merge, &s);
        assert!(text.contains("1 failing"), "CI state must be visible");
        assert!(text.contains("UNSTABLE"));
        assert!(text.contains("worth_a_look"));
        assert!(text.contains("the lockfile moved"));
    }
}
