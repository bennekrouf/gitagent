//! Whether a release actually came out: the build the release tag started.
//!
//! `release.sh` ends at pushing a tag, and that is where a flow used to end
//! too. Everything that then goes wrong happens on the forge — a macOS job
//! that sits for hours, a Windows runner that cannot install its signing tool —
//! and nothing on screen said so: the step was green, the download was still
//! the old version, and finding out meant opening the Actions page. This
//! follows the run the tag started, job by job, until it is done.
//!
//! GitHub Actions only. Anything else says so and stops quietly: the release
//! itself already went out, and not knowing how it is built is not a failure.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use super::flow::{StepFailure, StepOutcome};
use super::forge::{self, Forge};
use super::git;
use super::graph::Remedy;

/// How long the run is given to appear after the tag was pushed.
const APPEAR: Duration = Duration::from_secs(120);
/// A job running this long is stuck, not slow: release builds finish well
/// inside it. Past it the step stops and offers to cancel.
const STUCK: Duration = Duration::from_secs(60 * 60);
/// Looks that may fail in a row (a dropped connection) before giving up.
const MISSES: usize = 10;
/// Longest the log goes without a line while jobs are still running. A
/// client following the run over MCP reads silence as a hang.
const QUIET: Duration = Duration::from_secs(5 * 60);

/// The run a tag started.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RunRef {
    pub id: String,
    pub workflow: String,
    pub url: String,
}

/// One job of the run, as far as this step cares.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Job {
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub url: String,
}

impl Job {
    fn finished(&self) -> bool {
        self.status == "completed"
    }

    fn failed(&self) -> bool {
        self.finished() && !matches!(self.conclusion.as_str(), "success" | "skipped" | "neutral")
    }

    /// What the job is doing, in the words the log uses.
    fn state(&self) -> &'static str {
        match (self.status.as_str(), self.conclusion.as_str()) {
            ("completed", "success") => "done",
            ("completed", "skipped") => "skipped",
            ("completed", "cancelled") => "cancelled",
            ("completed", _) => "failed",
            ("in_progress", _) => "running",
            _ => "waiting for a runner",
        }
    }
}

/// The run as a whole, at one look.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RunView {
    pub status: String,
    pub conclusion: String,
    pub jobs: Vec<Job>,
}

/// The newest run `gh run list --json databaseId,workflowName,url` lists.
pub(crate) fn run_from_list(json: &str) -> Option<RunRef> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let run = value.as_array()?.first()?;
    Some(RunRef {
        id: run["databaseId"].as_i64()?.to_string(),
        workflow: run["workflowName"]
            .as_str()
            .unwrap_or("Release")
            .to_string(),
        url: run["url"].as_str().unwrap_or_default().to_string(),
    })
}

/// `gh run view --json status,conclusion,jobs`.
pub(crate) fn view_from_json(json: &str) -> Option<RunView> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    Some(RunView {
        status: text(&value["status"]),
        conclusion: text(&value["conclusion"]),
        jobs: value["jobs"]
            .as_array()?
            .iter()
            .map(|j| Job {
                name: text(&j["name"]),
                status: text(&j["status"]),
                conclusion: text(&j["conclusion"]),
                url: text(&j["url"]),
            })
            .collect(),
    })
}

/// Follows the build the version tag on HEAD started, until it finishes.
pub async fn watch(repo: &str, on_line: &mut dyn FnMut(&str)) -> Result<StepOutcome, StepFailure> {
    let forge = match git::remote_url(repo).await {
        Some(url) => forge::detect(&url),
        None => Forge::None,
    };
    if forge != Forge::GitHub {
        return Ok(StepOutcome::nothing(
            "Released. Only builds on GitHub Actions are followed from here, so this one \
             is not.",
        ));
    }
    let Some(tag) = version_tag(repo).await else {
        return Ok(StepOutcome::nothing(
            "No version tag on this commit, so no release build to follow.",
        ));
    };

    let Some(run) = find_run(repo, &tag).await else {
        return Ok(StepOutcome::nothing(format!(
            "No GitHub Actions run started for {tag} within two minutes: this repository's \
             releases are not built there, or Actions is turned off for it."
        )));
    };
    on_line(&format!(
        "Following {} for {tag}: {}",
        run.workflow, run.url
    ));

    let mut seen: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut running_since: BTreeMap<String, Instant> = BTreeMap::new();
    let mut misses = 0;
    let mut said = Instant::now();
    // A release build takes tens of minutes, and every look costs a call
    // against the hourly GitHub budget: looks start at 30 s and stretch to 3 min.
    let mut backoff = super::poll::Backoff::build();
    loop {
        let view = match look(repo, &run.id).await {
            Some(view) => {
                misses = 0;
                view
            }
            None => {
                misses += 1;
                if misses >= MISSES {
                    return Err(StepFailure::from(format!(
                        "Lost track of {} for {tag}: GitHub has not answered for several \
                         minutes. The build carries on without this step; see it at {}, or \
                         retry this step to follow it again.",
                        run.workflow, run.url
                    )));
                }
                backoff.sleep().await;
                continue;
            }
        };

        for job in &view.jobs {
            let state = job.state();
            if seen.get(&job.name) != Some(&state) {
                seen.insert(job.name.clone(), state);
                on_line(&format!("{}: {state}", job.name));
                said = Instant::now();
            }
            if job.status == "in_progress" {
                running_since
                    .entry(job.name.clone())
                    .or_insert_with(Instant::now);
            }
        }

        if view.status == "completed" {
            let log = if view.jobs.iter().any(Job::failed) {
                failed_log(repo, &run.id).await
            } else {
                None
            };
            return finished(&run, &tag, &view, log.as_deref());
        }
        if said.elapsed() > QUIET {
            let open: Vec<String> = view
                .jobs
                .iter()
                .filter(|j| !j.finished())
                .map(|j| match running_since.get(&j.name) {
                    Some(since) => format!("{} ({} min)", j.name, since.elapsed().as_secs() / 60),
                    None => format!("{} (waiting for a runner)", j.name),
                })
                .collect();
            on_line(&format!("Still building: {}", open.join(", ")));
            said = Instant::now();
        }

        if let Some(job) = view.jobs.iter().find(|j| {
            j.status == "in_progress"
                && running_since
                    .get(&j.name)
                    .is_some_and(|since| since.elapsed() > STUCK)
        }) {
            return Err(StepFailure {
                message: format!(
                    "{} has been running for over an hour, far longer than a release build \
                     takes, so it is most likely stuck. Nothing is published until it \
                     finishes. Cancel the run, then re-run it once it has stopped.\n\n{}",
                    job.name, job.url
                ),
                remedies: vec![Remedy::new(
                    "Cancel the stuck run",
                    "gh",
                    &["run", "cancel", &run.id],
                )],
            });
        }

        backoff.sleep().await;
    }
}

/// A failure a re-run cannot fix, recognised from what the failed jobs logged.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KnownCause {
    /// What happened and what the user does about it, in place of the hint
    /// that a second try usually passes.
    pub why: &'static str,
    /// The re-run button, renamed to say when it is worth pressing.
    pub rerun: &'static str,
}

/// Failures that come back on every re-run until someone fixes them outside
/// the build. Each is matched on text the tool prints, which is stable.
const KNOWN_CAUSES: &[(&str, KnownCause)] = &[
    (
        "A required agreement is missing or has expired",
        KnownCause {
            why: "Apple's notary service refused the macOS build: your Apple Developer team \
                  has an agreement that is new or has expired, and Apple notarizes nothing \
                  until it is accepted. Re-running fails the same way until then. The \
                  Account Holder accepts it at https://developer.apple.com/account, and in \
                  App Store Connect under Business if one waits there too; then re-run the \
                  failed jobs.",
            rerun: "Re-run once the agreement is accepted",
        },
    ),
    (
        "CSSMERR_TP_CERT_EXPIRED",
        KnownCause {
            why: "The macOS signing certificate has expired, so the build cannot be signed. \
                  Re-running fails the same way. Make a new Developer ID Application \
                  certificate at https://developer.apple.com/account, put it in the \
                  repository's signing secrets in place of the old one, then re-run the \
                  failed jobs.",
            rerun: "Re-run once the certificate is replaced",
        },
    ),
];

/// The first known cause the failed jobs' log shows.
pub(crate) fn known_cause(log: &str) -> Option<KnownCause> {
    KNOWN_CAUSES
        .iter()
        .find(|(text, _)| log.contains(text))
        .map(|(_, cause)| *cause)
}

/// What a completed run says about the release. `log` is what the failed
/// jobs printed, when it could be read.
fn finished(
    run: &RunRef,
    tag: &str,
    view: &RunView,
    log: Option<&str>,
) -> Result<StepOutcome, StepFailure> {
    let failed: Vec<&Job> = view.jobs.iter().filter(|j| j.failed()).collect();
    if view.conclusion == "success" && failed.is_empty() {
        let built: Vec<&str> = view
            .jobs
            .iter()
            .filter(|j| j.conclusion == "success")
            .map(|j| j.name.as_str())
            .collect();
        let summary = format!("{tag} published — {}", built.join(", "));
        return Ok(StepOutcome {
            log: format!("{summary}\n\n{}", run.url),
            summary,
            artifacts: vec![],
            nothing_to_do: false,
            items: vec![],
        });
    }

    let list = failed
        .iter()
        .map(|j| format!("  {} — {}\n    {}", j.name, j.state(), j.url))
        .collect::<Vec<_>>()
        .join("\n");
    let cause = log.and_then(known_cause);
    let advice = match cause {
        Some(cause) => cause.why,
        None => {
            "A job that never got a runner, or failed installing a tool, usually passes \
             on a second try."
        }
    };
    Err(StepFailure {
        message: format!(
            "{} for {tag} did not finish: {}. What it built before that is not published. \
             {advice}\n\n{list}\n\n{}",
            run.workflow,
            if failed.is_empty() {
                view.conclusion.clone()
            } else {
                format!(
                    "{} {}",
                    failed.len(),
                    if failed.len() == 1 {
                        "job failed"
                    } else {
                        "jobs failed"
                    }
                )
            },
            run.url
        ),
        remedies: vec![Remedy::new(
            cause.map_or("Re-run the failed jobs", |c| c.rerun),
            "gh",
            &["run", "rerun", &run.id, "--failed"],
        )],
    })
}

/// What the failed jobs of run `id` printed.
async fn failed_log(repo: &str, id: &str) -> Option<String> {
    git::run(repo, "gh", &["run", "view", id, "--log-failed"])
        .await
        .ok()
}

/// The `v…` tag on HEAD: what `release.sh` just pushed.
async fn version_tag(repo: &str) -> Option<String> {
    let out = git::run(repo, "git", &["tag", "--points-at", "HEAD", "--list", "v*"])
        .await
        .ok()?;
    out.lines()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .max_by(|a, b| version_order(a, b))
        .map(str::to_string)
}

/// Orders `v1.2.10` after `v1.2.9`, which a plain string comparison does not.
fn version_order(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |t: &str| -> Vec<u64> {
        t.trim_start_matches('v')
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parts(a).cmp(&parts(b))
}

/// The run the push of `tag` started. It can take a few seconds to show up.
async fn find_run(repo: &str, tag: &str) -> Option<RunRef> {
    let started = Instant::now();
    let mut backoff = super::poll::Backoff::new(Duration::from_secs(10), Duration::from_secs(30));
    loop {
        let listed = git::run(
            repo,
            "gh",
            &[
                "run",
                "list",
                "--branch",
                tag,
                "--event",
                "push",
                "--limit",
                "1",
                "--json",
                "databaseId,workflowName,url",
            ],
        )
        .await
        .ok();
        if let Some(run) = listed.as_deref().and_then(run_from_list) {
            return Some(run);
        }
        if started.elapsed() > APPEAR {
            return None;
        }
        backoff.sleep().await;
    }
}

async fn look(repo: &str, id: &str) -> Option<RunView> {
    let out = git::run(
        repo,
        "gh",
        &["run", "view", id, "--json", "status,conclusion,jobs"],
    )
    .await
    .ok()?;
    view_from_json(&out)
}

#[cfg(test)]
mod tests;
