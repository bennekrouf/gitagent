//! Why a pull request's checks failed, in one sentence, and the one thing to
//! do about it.
//!
//! Three red crosses linking to CI logs left the person to open a megabyte of
//! log, find the real error, and notice — or not — that the base branch had
//! already fixed it. This works that out: a branch behind its base is offered
//! the base's newer commits first, since the fix is often already there;
//! otherwise the first real error line of the log is named.
//!
//! Costs nothing from the GraphQL budget: the behind count is local git, and
//! the log comes from the REST API, read once per failed check and kept.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::git;
use super::probe::PrBrief;
use super::review::strip_ansi;

#[derive(Clone, PartialEq, Debug)]
pub enum Hint {
    /// The base branch has commits the pull request's branch does not.
    Behind { base: String, count: usize },
    /// The first real error in the failed check's log.
    Error(String),
    /// Nothing better to say than the red crosses already do.
    Nothing,
}

fn kept() -> &'static Mutex<HashMap<String, Hint>> {
    static KEPT: OnceLock<Mutex<HashMap<String, Hint>>> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What a failed check run is, as a key: the same failure is read once.
fn key(repo: &str, pr: &PrBrief) -> String {
    let urls: Vec<&str> = pr.failed.iter().map(|f| f.url.as_str()).collect();
    format!("{repo}#{}#{}", pr.number, urls.join(","))
}

/// Forgets what was worked out for a pull request, once it has been updated.
pub fn forget(repo: &str, pr: &PrBrief) {
    if let Ok(mut map) = kept().lock() {
        map.remove(&key(repo, pr));
    }
}

pub async fn why_failed(repo: &str, pr: &PrBrief) -> Hint {
    let key = key(repo, pr);
    if let Some(hint) = kept().lock().ok().and_then(|m| m.get(&key).cloned()) {
        return hint;
    }
    let hint = work_out(repo, pr).await;
    if let Ok(mut map) = kept().lock() {
        map.insert(key, hint.clone());
    }
    hint
}

async fn work_out(repo: &str, pr: &PrBrief) -> Hint {
    if !pr.base.is_empty() && !pr.head.is_empty() {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            git::fetch(repo, &["--quiet", "origin", &pr.base, &pr.head]),
        )
        .await;
        let range = format!("origin/{}..origin/{}", pr.head, pr.base);
        if let Ok(out) = git::run(repo, "git", &["rev-list", "--count", &range]).await {
            if let Ok(count) = out.trim().parse::<usize>() {
                if count > 0 {
                    return Hint::Behind {
                        base: pr.base.clone(),
                        count,
                    };
                }
            }
        }
    }
    for check in &pr.failed {
        let Some(job) = job_id(&check.url) else {
            continue;
        };
        if let Ok(log) = git::job_log(repo, &job).await {
            if let Some(line) = first_error(&log) {
                return Hint::Error(line);
            }
        }
    }
    Hint::Nothing
}

/// `…/actions/runs/<run>/job/<job>` → `<job>`.
fn job_id(url: &str) -> Option<String> {
    let (_, rest) = url.split_once("/job/")?;
    let id: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    (!id.is_empty()).then_some(id)
}

/// The first line of a job log that says what broke, with where when the
/// compiler gave a place: `src/x.rs:86:14 — error: using map_err over inspect_err`.
pub fn first_error(raw: &str) -> Option<String> {
    code_error(raw).or_else(|| {
        raw.lines()
            .map(text_of)
            .find(|t| t.trim().starts_with("##[error]"))
            .map(|t| t.trim().trim_start_matches("##[error]").trim().to_string())
    })
}

/// What [`first_error`] finds when the code itself is at fault: a compiler or
/// lint error, a failed test, a formatting diff. Running the check again
/// cannot change any of these; a runner that died or timed out leaves none.
pub fn code_error(raw: &str) -> Option<String> {
    let lines: Vec<String> = raw.lines().map(text_of).collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let compiler = (t.starts_with("error:") || t.starts_with("error["))
            && !t.starts_with("error: could not compile")
            && !t.starts_with("error: process didn't exit")
            && !t.starts_with("error: test failed");
        if compiler {
            let place = lines
                .iter()
                .skip(i + 1)
                .take(3)
                .find_map(|l| l.trim().strip_prefix("--> ").map(str::to_string));
            return Some(match place {
                Some(place) => format!("{place} \u{2014} {t}"),
                None => t.to_string(),
            });
        }
    }
    lines
        .iter()
        .map(|l| l.trim())
        .find(|t| t.contains("panicked at") || t.starts_with("Diff in "))
        .map(str::to_string)
}

/// A row of `gh run view --log-failed`: `job\tstep\t<timestamp> text`.
fn text_of(line: &str) -> String {
    let text = line.rsplit('\t').next().unwrap_or(line);
    let text = match text.split_once(' ') {
        Some((first, rest)) if first.len() > 19 && first.ends_with('Z') && first.contains('T') => {
            rest
        }
        _ => text,
    };
    // `gh` writes the escape as the two characters `^[`, not the byte.
    strip_ansi(&text.trim_start_matches('\u{feff}').replace("^[", "\u{1b}"))
}

/// Merges the base branch into the pull request's branch and pushes it, so
/// its checks run again with whatever the base has fixed since.
///
/// Done in a worktree of its own, never the checked-out one: the person's
/// branch and uncommitted files are left exactly as they are.
pub async fn update_from_base(repo: &str, pr: &PrBrief) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!(
        "gitagent-update-{}-{}",
        pr.number,
        std::process::id()
    ));
    let dir_s = dir.to_string_lossy().to_string();
    git::fetch(repo, &["--quiet", "origin", &pr.base, &pr.head]).await?;
    let _ = git::run(repo, "git", &["worktree", "remove", "--force", &dir_s]).await;
    git::run(
        repo,
        "git",
        &[
            "worktree",
            "add",
            "--detach",
            &dir_s,
            &format!("origin/{}", pr.head),
        ],
    )
    .await?;
    let result = async {
        let base = format!("origin/{}", pr.base);
        if let Err(e) = git::run(&dir_s, "git", &["merge", "--no-edit", &base]).await {
            let _ = git::run(&dir_s, "git", &["merge", "--abort"]).await;
            return Err(format!(
                "{} and #{} changed the same lines, so this cannot be done by itself. \
                 Run Review \u{2192} Merge on it: it walks through the conflict.\n\n{e}",
                pr.base, pr.number
            ));
        }
        git::run(
            &dir_s,
            "git",
            &["push", "origin", &format!("HEAD:refs/heads/{}", pr.head)],
        )
        .await?;
        Ok(format!(
            "Brought {} into #{} and pushed. Its checks are running again.",
            pr.base, pr.number
        ))
    }
    .await;
    let _ = git::run(repo, "git", &["worktree", "remove", "--force", &dir_s]).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clippy_error_is_named_with_its_place() {
        let log = "ci / test\tUNKNOWN STEP\t2026-11-05T10:31:23.7136710Z \u{1b}[1merror\u{1b}[0m: using `map_err` over `inspect_err`\n\
                   ci / test\tUNKNOWN STEP\t2026-11-05T10:31:23.7136710Z   \u{1b}[1m\u{1b}[94m--> \u{1b}[0msrc/services/probe/prs.rs:86:14\n\
                   ci / test\tUNKNOWN STEP\t2026-11-05T10:31:24.0Z error: could not compile `gitagent`";
        assert_eq!(
            first_error(log).unwrap(),
            "src/services/probe/prs.rs:86:14 \u{2014} error: using `map_err` over `inspect_err`"
        );
    }

    #[test]
    fn colour_written_out_as_caret_bracket_is_stripped_too() {
        let log = "t\ts\t2026-11-05T10:31:23.7031000Z ^[[1m^[[91merror^[[0m^[[1m: using `map_err` over `inspect_err`^[[0m\n\
                   t\ts\t2026-11-05T10:31:23.7136710Z   ^[[1m^[[94m--> ^[[0msrc/services/probe/prs.rs:86:14";
        assert_eq!(
            first_error(log).unwrap(),
            "src/services/probe/prs.rs:86:14 \u{2014} error: using `map_err` over `inspect_err`"
        );
    }

    #[test]
    fn a_failing_test_is_named_by_its_panic() {
        let log = "t\ts\t2026-11-05T10:31:23.7136710Z thread 'x' panicked at src/a.rs:3:5:\n\
                   t\ts\t2026-11-05T10:31:23.7136710Z ##[error]Process completed with exit code 101.";
        assert_eq!(
            first_error(log).unwrap(),
            "thread 'x' panicked at src/a.rs:3:5:"
        );
    }

    #[test]
    fn the_job_is_read_from_the_check_link() {
        assert_eq!(
            job_id("https://github.com/o/r/actions/runs/42/job/127189451533").as_deref(),
            Some("127189451533")
        );
        assert_eq!(job_id("https://example.com/build/1"), None);
    }
}
