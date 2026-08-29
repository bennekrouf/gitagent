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
use super::graph::{Remedy, RunState, Step};
use super::llm::{complete_json, LlmConfig};

/// The merge approval is the whole point of this flow: both signals, stated
/// plainly, with the disagreement visible when there is one.
pub fn proposal(step: Step, state: &RunState) -> String {
    match step {
        Step::Merge => format!(
            "gh pr merge {} --squash --delete-branch\n\n\
             ── CI ────────────────────────────────\n{}\n\
             merge state: {}\n{}\n\
             ── Model ─────────────────────────────\nverdict: {}\n\n{}",
            state.artifact("pr_number"),
            state.artifact("checks_summary"),
            state.artifact("merge_state"),
            state.artifact("checks_detail"),
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
    // Set when this run was started from a specific pull request in the
    // sidebar's PR list, rather than from "whatever the checked-out branch
    // has open" — lets a review run against any open PR, not only the one
    // for the branch you currently happen to be sitting on.
    let selected = state.artifact("selected_pr_number").trim();

    match forge {
        Forge::GitHub => {
            let mut args: Vec<&str> = vec!["pr", "view"];
            if !selected.is_empty() {
                args.push(selected);
            }
            args.extend([
                "--json",
                "number,title,url,state,baseRefName,headRefName,isDraft",
            ]);
            let out = git::run(repo, "gh", &args).await.map_err(|e| {
                if e.contains("no pull requests found") || e.contains("no open pull requests") {
                    StepFailure::from(if selected.is_empty() {
                        format!("No open pull request for `{branch}`. Run the commit flow first.")
                    } else {
                        format!("Pull request #{selected} is not open.")
                    })
                } else {
                    StepFailure::from(e)
                }
            })?;

            let value: serde_json::Value = serde_json::from_str(&out)
                .map_err(|e| format!("could not read the gh response: {e}"))?;

            // A merged or closed pull request is not an error, there is simply
            // nothing to review — and its branch is usually gone from the
            // remote, which would fail confusingly at `pr_diff` instead.
            if !crate::services::probe::is_open(&value) {
                let state = value["state"].as_str().unwrap_or("not open");
                let number = value["number"].as_i64().unwrap_or_default();
                return Ok(StepOutcome::nothing(format!(
                    "Pull request #{number} for `{branch}` is {state}. Nothing to review — \
                     switch to {} and pull.",
                    value["baseRefName"].as_str().unwrap_or("the base branch"),
                )));
            }

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
                nothing_to_do: false,
                items: vec![],
            })
        }
        Forge::AzureDevOps => {
            let (pr, head): (serde_json::Value, String) = if !selected.is_empty() {
                let out = git::run(
                    repo,
                    "az",
                    &["repos", "pr", "show", "--id", selected, "--output", "json"],
                )
                .await?;
                let pr: serde_json::Value = serde_json::from_str(&out)
                    .map_err(|e| format!("could not read az output: {e}"))?;
                let head = pr["sourceRefName"]
                    .as_str()
                    .unwrap_or(&branch)
                    .trim_start_matches("refs/heads/")
                    .to_string();
                (pr, head)
            } else {
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
                let list: serde_json::Value = serde_json::from_str(&out)
                    .map_err(|e| format!("could not read az output: {e}"))?;
                let pr = list
                    .as_array()
                    .and_then(|a| a.first())
                    .cloned()
                    .ok_or_else(|| format!("No active pull request for `{branch}`."))?;
                (pr, branch.clone())
            };

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
                log: format!("!{number}  {title}\n{url}\n\n{head} → {base}"),
                artifacts: vec![
                    ("pr_number".into(), number),
                    ("pr_title".into(), title),
                    ("pr_url".into(), url),
                    ("pr_base".into(), base),
                    ("pr_head".into(), head),
                ],
                nothing_to_do: false,
                items: vec![],
            })
        }
        other => Err(StepFailure::from(format!(
            "Cannot look up pull requests on {}.",
            other.label()
        ))),
    }
}

/// The job id out of a check's `detailsUrl`
/// (`…/actions/runs/<run>/job/<job>`).
fn job_id(details_url: &str) -> Option<&str> {
    details_url
        .rsplit_once("/job/")
        .map(|(_, id)| id)
        .filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()))
}

/// Boils a GitHub Actions job log down to the failure.
///
/// `gh run view --log-failed` returns the entire job — over a megabyte of
/// runner boilerplate, with post-job cleanup *after* the error. Neither the
/// head nor the tail is the interesting part. The `##[error]` line is, so this
/// keeps that line and the lines leading up to it, which is where the compiler
/// output, the failing assertion or the rustfmt diff actually is.
/// Removes ANSI colour codes.
///
/// cargo colours its output, and GitHub Actions keeps the escapes in the
/// stored log. Rendered as text they show up as `^[[1m^[[92m` in front of
/// every interesting line, which is worse than no colour at all.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // CSI: ESC '[' params… final byte in @-~. Anything else, drop the
        // escape alone rather than guessing at a length.
        if chars.next() == Some('[') {
            for p in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&p) {
                    break;
                }
            }
        }
    }
    out
}

fn distil_log(raw: &str, context: usize) -> String {
    let lines: Vec<String> = raw
        .lines()
        .map(|line| {
            // Rows arrive as `job\tstep\t<ISO timestamp> text`.
            let text = line.rsplit('\t').next().unwrap_or(line);
            match text.split_once(' ') {
                Some((first, rest))
                    if first.len() > 19 && first.ends_with('Z') && first.contains('T') =>
                {
                    rest.to_string()
                }
                _ => text.to_string(),
            }
        })
        .map(|line| strip_ansi(line.trim_start_matches('\u{feff}')))
        .filter(|line| {
            let l = line.trim();
            !l.is_empty()
                && !l.starts_with("##[group]")
                && !l.starts_with("##[endgroup]")
                && !l.starts_with("##[warning]")
        })
        .collect();

    if lines.is_empty() {
        return String::new();
    }

    let end = lines
        .iter()
        .position(|l| l.contains("##[error]"))
        .unwrap_or(lines.len().saturating_sub(1));
    let start = end.saturating_sub(context);

    lines[start..=end.min(lines.len().saturating_sub(1))]
        .join("\n")
        .replace("##[error]", "")
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
            nothing_to_do: false,
            items: vec![],
        });
    }

    // `find_pr` already resolved which PR this run is about — reusing that
    // number here (rather than a bare `gh pr view`, which re-resolves off
    // the checked-out branch) is what lets this step work for a PR picked
    // from the sidebar's list, not just the one the local branch happens to
    // have open.
    let number = state.artifact("pr_number");
    let out = git::run(
        repo,
        "gh",
        &[
            "pr",
            "view",
            number,
            "--json",
            "statusCheckRollup,mergeable,mergeStateStatus",
        ],
    )
    .await?;
    let value: serde_json::Value =
        serde_json::from_str(&out).map_err(|e| format!("could not read the gh response: {e}"))?;

    let mut lines = vec![];
    let mut failing: Vec<(String, String)> = vec![];
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
                failing.push((
                    name.to_string(),
                    check["detailsUrl"].as_str().unwrap_or_default().to_string(),
                ));
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

    // Only for checks that actually failed, and only the first few: each one is
    // a separate round-trip that downloads the whole job log.
    let mut detail = String::new();
    for (name, url) in failing.iter().take(3) {
        detail.push_str(&format!("\n── {name} ──\n"));
        match job_id(url) {
            Some(id) => {
                let (_, raw) = git::run_command(
                    "gh",
                    &[
                        "run".into(),
                        "view".into(),
                        "--job".into(),
                        id.to_string(),
                        "--log-failed".into(),
                    ],
                )
                .await;
                let boiled = distil_log(&raw, 30);
                detail.push_str(if boiled.trim().is_empty() {
                    "(could not read the log)"
                } else {
                    &boiled
                });
                detail.push('\n');
            }
            None => detail.push_str("(no job log for this check)\n"),
        }
        if !url.is_empty() {
            detail.push_str(&format!("{url}\n"));
        }
    }

    Ok(StepOutcome {
        summary: format!("{state_word} · {merge_state}"),
        log: format!("{summary}\n\nmergeable:   {mergeable}\nmerge state: {merge_state}\n{detail}"),
        artifacts: vec![
            ("checks_summary".into(), summary),
            ("checks_detail".into(), detail),
            ("checks_state".into(), state_word.into()),
            ("merge_state".into(), merge_state),
        ],
        nothing_to_do: false,
        items: vec![],
    })
}

/// The diff comes from git, not the forge — one code path for both platforms,
/// and no dependency on a CLI being installed to read it.
async fn pr_diff(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let base = state.artifact("pr_base");
    let head = state.artifact("pr_head");
    // Swallowing this used to leave a failed fetch looking like a missing
    // ref two lines later — "ambiguous argument origin/X...origin/Y" is a
    // confusing way to learn the actual problem was "couldn't fetch base
    // or head from origin" (wrong remote, network, a base branch that no
    // longer exists). Surface the real reason instead.
    git::run(repo, "git", &["fetch", "origin", base, head])
        .await
        .map_err(|e| format!("could not fetch {base} and {head} from origin: {e}"))?;

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
        nothing_to_do: false,
        items: vec![],
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
        nothing_to_do: false,
        items: vec![],
    })
}

/// "not mergeable: the merge commit cannot be cleanly created" means a real
/// conflict against the base branch — nothing here can resolve that
/// automatically, but closing the pull request is always an available way
/// out, worth offering right where the failure is shown rather than sending
/// the human to a terminal.
fn merge_failure(number: &str, base: &str, message: String) -> StepFailure {
    let mut remedies = vec![];
    if message.contains("not mergeable") {
        // The constructive option first. It cannot finish the job — a content
        // conflict needs a person — but it is the step that person would take
        // first, and it leaves the tree ready to resolve rather than sending
        // them to a terminal to work out what to type.
        //
        // Not retryable afterwards: the merge stops mid-way on a conflict, and
        // offering "retry the merge" against a half-merged tree would be a
        // trap. Resolve, commit, push, then run the flow again.
        remedies.push(Remedy::terminal(
            &format!("Bring {base} in — you resolve any conflicts"),
            "git",
            &["merge", &format!("origin/{base}")],
        ));
        remedies.push(Remedy::terminal(
            &format!("Abandon — close #{number} without merging"),
            "gh",
            &[
                "pr",
                "close",
                number,
                "--comment",
                "Closing — conflicts with the base branch and this run is being abandoned rather than resolved.",
            ],
        ));
    }
    StepFailure { message, remedies }
}

async fn merge(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    let number = state.artifact("pr_number").to_string();

    let out = match forge {
        Forge::GitHub => git::run(
            repo,
            "gh",
            &["pr", "merge", &number, "--squash", "--delete-branch"],
        )
        .await
        .map_err(|e| merge_failure(&number, state.artifact("pr_base"), e))?,
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
        nothing_to_do: false,
        items: vec![],
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
        nothing_to_do: false,
        items: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_id_is_read_out_of_the_details_url() {
        assert_eq!(
            job_id("https://github.com/o/r/actions/runs/32857682937/job/97833574009"),
            Some("97833574009")
        );
        assert_eq!(job_id("https://example.com/no-job-here"), None);
        assert_eq!(job_id(""), None);
    }

    #[test]
    fn the_log_is_cut_down_to_the_failure_and_what_led_to_it() {
        let raw = "\
fmt\tUNKNOWN STEP\t2026-08-25T14:08:59.4479744Z Current runner version: '2.336.0'
fmt\tUNKNOWN STEP\t2026-08-25T14:08:59.4505213Z ##[group]Runner Image Provisioner
fmt\tUNKNOWN STEP\t2026-08-25T14:09:12.9480177Z -const RELEASES_URL: &str =
fmt\tUNKNOWN STEP\t2026-08-25T14:09:12.9480622Z +const RELEASES_URL: &str = \"…\";
fmt\tUNKNOWN STEP\t2026-08-25T14:09:12.9490017Z ##[error]Process completed with exit code 1.
fmt\tUNKNOWN STEP\t2026-08-25T14:09:13.0464818Z Post job cleanup.
fmt\tUNKNOWN STEP\t2026-08-25T14:09:13.0508478Z git version 2.55.0";

        let out = distil_log(raw, 3);
        assert!(out.contains("Process completed with exit code 1"));
        assert!(
            out.contains("-const RELEASES_URL"),
            "the rustfmt diff is the point"
        );
        assert!(
            !out.contains("Post job cleanup"),
            "nothing after the failure"
        );
        assert!(!out.contains("##[group]"), "group markers stripped");
        assert!(!out.contains("2026-08-25T"), "timestamps stripped");
        assert!(!out.contains("##[error]"), "the marker itself is noise");
    }

    #[test]
    fn cargo_colours_do_not_reach_the_reader() {
        let raw = concat!(
            "j\ts\t2026-08-28T14:09:12.9480177Z ",
            "\u{1b}[1m\u{1b}[91merror\u{1b}[0m: found call to `str::trim`\n",
            "j\ts\t2026-08-28T14:09:12.9490017Z ",
            "##[error]Process completed with exit code 101."
        );
        let out = distil_log(raw, 3);
        assert!(!out.contains('\u{1b}'), "no escapes survive");
        assert!(!out.contains("[1m"), "and no leftovers either");
        assert!(
            out.contains("error: found call to `str::trim`"),
            "got {out}"
        );
    }

    #[test]
    fn stripping_colour_leaves_ordinary_text_alone() {
        assert_eq!(strip_ansi("plain [not] an escape"), "plain [not] an escape");
    }

    #[test]
    fn a_truncated_escape_does_not_eat_the_rest_of_the_line() {
        assert_eq!(strip_ansi("before\u{1b}"), "before");
    }

    #[test]
    fn only_the_lines_asked_for_are_kept() {
        let raw = (0..100)
            .map(|i| format!("j\ts\t2026-08-25T14:09:12.9480177Z line {i}"))
            .chain(["j\ts\t2026-08-25T14:09:12.9490017Z ##[error]boom".to_string()])
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(distil_log(&raw, 5).lines().count(), 6);
    }

    #[test]
    fn a_log_with_no_error_marker_still_yields_its_tail() {
        let raw = "j\ts\t2026-08-25T14:09:12.9480177Z something went sideways";
        assert!(distil_log(raw, 10).contains("something went sideways"));
    }

    #[test]
    fn an_empty_log_does_not_panic() {
        assert_eq!(distil_log("", 10), "");
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

    #[test]
    fn only_the_merge_step_proposes_anything() {
        let s = RunState::default();
        for step in [
            Step::FindPr,
            Step::PrStatus,
            Step::PrDiff,
            Step::Analyse,
            Step::Sync,
        ] {
            assert!(proposal(step, &s).is_empty());
        }
    }

    #[test]
    fn a_merge_conflict_offers_resolving_before_abandoning() {
        let failure = merge_failure(
            "8",
            "master",
            "X Pull request bennekrouf/gitagent#8 is not mergeable: the merge commit cannot be cleanly created.".to_string(),
        );
        assert_eq!(failure.remedies.len(), 2);

        // Constructive first: closing the pull request should never be the
        // only thing on offer for a conflict.
        let update = &failure.remedies[0];
        assert_eq!(update.program, "git");
        assert_eq!(update.args, vec!["merge", "origin/master"]);
        assert!(
            !update.retry_after,
            "a conflicted merge leaves work to do; retrying the merge would be a trap"
        );

        let abandon = &failure.remedies[1];
        assert_eq!(abandon.program, "gh");
        assert_eq!(abandon.args[0..3], ["pr", "close", "8"]);
        assert!(!abandon.retry_after);
    }

    #[test]
    fn the_update_remedy_targets_the_pull_requests_own_base() {
        let failure = merge_failure("1", "develop", "not mergeable".to_string());
        assert_eq!(failure.remedies[0].args, vec!["merge", "origin/develop"]);
    }

    #[test]
    fn a_merge_failure_for_any_other_reason_offers_nothing() {
        let failure = merge_failure("8", "master", "gh: authentication required".to_string());
        assert!(failure.remedies.is_empty());
    }
}
