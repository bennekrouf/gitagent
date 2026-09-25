//! Release notes: the model drafts the `[Unreleased]` entry of CHANGELOG.md
//! from what changed since the last tag, and a gated step writes it in.
//!
//! ```text
//!   draft_notes ──> write_notes ──> release
//! ```
//!
//! Split in two for the same reason `draft_commit` and `commit` are: the notes
//! are published word for word, so a person reads them before anything is
//! written. `draft_notes` never fails a run over notes that already exist —
//! it reports there is nothing to write, and the release goes ahead.

use serde_json::json;

use super::flow::{StepFailure, StepOutcome};
use super::git;
use super::llm::{complete_json, LlmConfig};

const CHANGELOG: &str = "CHANGELOG.md";
const COMMIT_MESSAGE: &str = "docs: add release notes";
const WRAP: usize = 80;
const SECTIONS: [(&str, &str); 4] = [
    ("added", "Added"),
    ("changed", "Changed"),
    ("fixed", "Fixed"),
    ("removed", "Removed"),
];

pub fn proposal(notes: &str) -> String {
    format!(
        "Adds these notes under ## [Unreleased] in {CHANGELOG}:\n\n{notes}\n\n\
         git add {CHANGELOG}\ngit commit -m \"{COMMIT_MESSAGE}\"\n\n\
         Committed locally. The release pushes it along with the version bump. \
         They are published as written: reject to write them yourself instead."
    )
}

pub async fn draft(repo: &str, cfg: &LlmConfig) -> Result<StepOutcome, StepFailure> {
    let path = std::path::Path::new(repo).join(CHANGELOG);
    let Ok(changelog) = std::fs::read_to_string(&path) else {
        return Ok(nothing_to_write(format!(
            "No {CHANGELOG} in this repository, so there is nowhere to write notes."
        )));
    };
    if unreleased_has_notes(&changelog) {
        return Ok(nothing_to_write(format!(
            "{CHANGELOG} already has notes under [Unreleased]."
        )));
    }

    let tag = git::run(repo, "git", &["describe", "--tags", "--abbrev=0", "HEAD"])
        .await
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let range = match &tag {
        Some(tag) => format!("{tag}..HEAD"),
        None => "HEAD".to_string(),
    };
    let since = tag.as_deref().unwrap_or("the first commit");

    // First parent only: a merge counts once, for what it brought in.
    let log = git::run(
        repo,
        "git",
        &[
            "log",
            &range,
            "-n",
            "50",
            "--first-parent",
            "-m",
            "--name-only",
            "--format=%x1e%s%x1f%b%x1f",
        ],
    )
    .await?;
    let commits = parse_log(&log);
    if commits.is_empty() {
        return Ok(nothing_to_write(format!(
            "Nothing committed since {since}."
        )));
    }

    // Which files a commit touched is a fact, not a judgement, so it is not
    // left to the model: a `feat:` that only added a CI workflow read as a
    // feature to it every time.
    let (app, internal): (Vec<&Commit>, Vec<&Commit>) =
        commits.iter().partition(|c| c.touches_app());
    let mut verdicts: Vec<String> = internal
        .iter()
        .map(|c| {
            format!(
                "  internal      {}  (CI, scripts, tests or docs only)",
                c.subject
            )
        })
        .collect();

    let groups: Vec<(&str, Vec<String>)> = if app.is_empty() {
        vec![]
    } else {
        let entries = ask_model(repo, cfg, &range, since, &changelog, &app).await?;
        for e in &entries {
            verdicts.push(format!(
                "  {}  {}",
                if e["user_visible"].as_bool() == Some(true) {
                    "user-visible"
                } else {
                    "internal    "
                },
                e["subject"].as_str().unwrap_or_default()
            ));
        }
        SECTIONS
            .iter()
            .map(|(key, heading)| {
                let bullets = entries
                    .iter()
                    .filter(|e| e["user_visible"].as_bool() == Some(true))
                    .filter(|e| e["section"].as_str() == Some(key))
                    .filter_map(|e| e["note"].as_str())
                    .map(|n| n.trim().trim_start_matches("- ").trim().to_string())
                    .filter(|n| !n.is_empty())
                    .collect();
                (*heading, bullets)
            })
            .collect()
    };
    let notes = render(&groups, &changelog);
    let count: usize = groups.iter().map(|(_, b)| b.len()).sum();

    Ok(StepOutcome {
        summary: if count == 0 {
            "no user-visible change".into()
        } else {
            format!("{count} note(s) drafted")
        },
        log: format!(
            "Drafted from {since}.\n\nHow each commit was read:\n{}\n\n{notes}",
            verdicts.join("\n")
        ),
        artifacts: vec![("release_notes".into(), notes)],
        nothing_to_do: false,
        items: vec![],
    })
}

async fn ask_model(
    repo: &str,
    cfg: &LlmConfig,
    range: &str,
    since: &str,
    changelog: &str,
    commits: &[&Commit],
) -> Result<Vec<serde_json::Value>, StepFailure> {
    let listed: Vec<String> = commits
        .iter()
        .map(|c| {
            format!(
                "commit: {}\n{}\nfiles: {}",
                c.subject,
                c.body.trim(),
                c.files.join(", ")
            )
        })
        .collect();

    // Only the application's files: the rest cannot produce a note, and on a
    // small local model they push the instructions out of the context.
    let mut files: Vec<&str> = commits
        .iter()
        .flat_map(|c| c.files.iter().map(|f| f.as_str()))
        .filter(|f| !is_internal(f) && !f.ends_with(".lock"))
        .collect();
    files.sort_unstable();
    files.dedup();
    files.truncate(200);
    let diff = if range.contains("..") && !files.is_empty() {
        let mut args = vec!["diff", range, "--unified=2", "--"];
        args.extend(files.iter());
        git::run(repo, "git", &args).await.unwrap_or_default()
    } else {
        String::new()
    };
    let guidance = std::fs::read_to_string(std::path::Path::new(repo).join("CLAUDE.md"))
        .ok()
        .and_then(|text| section(&text, "release notes"))
        .unwrap_or_default();

    let system = "You write the release notes for the next version of an application, \
        for the [Unreleased] section of its CHANGELOG.md.\n\
        Go through the commits one by one and return one entry per commit:\n\
        - `user_visible`: true when the commit changes something someone using the \
          application could notice. A refactor, or a change only to tests or internals, \
          is not.\n\
        - `section`: one of added, changed, fixed, removed.\n\
        - `note`: for a user-visible commit, one to three sentences for someone using \
          the application, not a code reviewer: what they will see, or no longer run \
          into, and why it matters. Never name functions, modules, types or source \
          files; screens, buttons, settings and files the user works with are fine. \
          Match the tone of the existing entries. Empty for a commit that is not \
          user-visible.\n\
        Describe only what the commits and diff show. Do not invent motivation.";

    let user = format!(
        "The project's own rules for release notes:\n{guidance}\n\n\
         The top of {CHANGELOG}, for its conventions and tone:\n{}\n\n\
         Commits since {since}, newest first:\n\n{}\n\n\
         Their diff:\n{}",
        head_of(changelog),
        listed.join("\n\n"),
        git::cap(&diff),
    );

    let schema = json!({
        "type": "object",
        "properties": {
            "commits": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "subject": { "type": "string" },
                        "user_visible": { "type": "boolean" },
                        "section": {
                            "type": "string",
                            "enum": ["added", "changed", "fixed", "removed"]
                        },
                        "note": { "type": "string" }
                    },
                    "required": ["subject", "user_visible", "section", "note"]
                }
            }
        },
        "required": ["commits"]
    });
    let value = complete_json(cfg, system, &user, &schema).await?;
    Ok(value["commits"].as_array().cloned().unwrap_or_default())
}

struct Commit {
    subject: String,
    body: String,
    files: Vec<String>,
}

impl Commit {
    fn touches_app(&self) -> bool {
        self.files.iter().any(|f| !is_internal(f))
    }
}

/// `git log --format=%x1e%s%x1f%b%x1f --name-only`, one record per commit.
fn parse_log(log: &str) -> Vec<Commit> {
    log.split('\x1e')
        .filter_map(|record| {
            let mut parts = record.splitn(3, '\x1f');
            let subject = parts.next()?.trim().to_string();
            let body = parts.next().unwrap_or_default().trim().to_string();
            let files = parts
                .next()
                .unwrap_or_default()
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && *l != CHANGELOG)
                .map(str::to_string)
                .collect();
            (!subject.is_empty()).then_some(Commit {
                subject,
                body,
                files,
            })
        })
        .collect()
}

/// Paths that never reach someone using the application — the same list the
/// `Release notes` pull-request check uses to decide a note is not needed.
fn is_internal(path: &str) -> bool {
    const DIRS: &[&str] = &[
        ".github/",
        ".githooks/",
        ".gitlab/",
        ".circleci/",
        ".azuredevops/",
        ".pipelines/",
        "scripts/",
        "docs/",
        "tests/",
        "testdata/",
        "homebrew/",
    ];
    DIRS.iter().any(|d| path.starts_with(d))
        || path.ends_with(".md")
        || path.starts_with("azure-pipelines")
        || matches!(path, "LICENSE" | ".gitignore" | ".gitlab-ci.yml")
}

pub async fn write(repo: &str, notes: &str) -> Result<StepOutcome, StepFailure> {
    if notes.trim().is_empty() {
        return Ok(nothing_to_write(
            "Nothing drafted, so nothing written.".into(),
        ));
    }
    let dirty = git::run(repo, "git", &["status", "--porcelain", "--", CHANGELOG]).await?;
    if !dirty.trim().is_empty() {
        return Err(format!(
            "{CHANGELOG} has uncommitted changes, and writing the notes would mix them in. \
             Commit or discard them, then retry this step."
        )
        .into());
    }

    let path = std::path::Path::new(repo).join(CHANGELOG);
    let current =
        std::fs::read_to_string(&path).map_err(|e| format!("could not read {CHANGELOG}: {e}"))?;
    if unreleased_has_notes(&current) {
        return Ok(nothing_to_write(format!(
            "{CHANGELOG} gained notes under [Unreleased] since the draft; left as it is."
        )));
    }
    std::fs::write(&path, insert(&current, notes))
        .map_err(|e| format!("could not write {CHANGELOG}: {e}"))?;

    let mut log = git::run(repo, "git", &["add", "--", CHANGELOG]).await?;
    log.push_str(&git::run(repo, "git", &["commit", "-m", COMMIT_MESSAGE]).await?);

    Ok(StepOutcome {
        summary: format!("notes committed to {CHANGELOG}"),
        log: format!("{notes}\n\n{}", log.trim()),
        artifacts: vec![],
        nothing_to_do: false,
        items: vec![],
    })
}

/// Not `StepOutcome::nothing`: that stops the run, and existing notes are the
/// case where the release should go ahead untouched.
fn nothing_to_write(reason: String) -> StepOutcome {
    StepOutcome {
        summary: reason.clone(),
        log: reason,
        artifacts: vec![("release_notes".into(), String::new())],
        nothing_to_do: false,
        items: vec![],
    }
}

/// The same test `release.sh` makes: a bullet anywhere under `[Unreleased]`.
pub fn unreleased_has_notes(changelog: &str) -> bool {
    let mut inside = false;
    for line in changelog.lines() {
        if line.starts_with("## ") {
            inside = is_unreleased(line);
        } else if inside && line.starts_with("- ") {
            return true;
        }
    }
    false
}

fn is_unreleased(line: &str) -> bool {
    line.to_ascii_lowercase().starts_with("## [unreleased]")
}

/// The preamble and the newest two versions: enough for conventions and tone
/// without spending the prompt on history.
fn head_of(changelog: &str) -> String {
    let mut out = vec![];
    let mut versions = 0;
    for line in changelog.lines() {
        if line.starts_with("## [") && !is_unreleased(line) {
            versions += 1;
            if versions > 2 {
                break;
            }
        }
        out.push(line);
    }
    out.join("\n")
}

/// A `## ` section of a markdown file whose heading contains `name`.
fn section(text: &str, name: &str) -> Option<String> {
    let mut out: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.starts_with("## ") {
            if out.is_some() {
                break;
            }
            if line.to_lowercase().contains(name) {
                out = Some(vec![]);
            }
        } else if let Some(lines) = out.as_mut() {
            lines.push(line);
        }
    }
    out.map(|l| l.join("\n").trim().to_string())
}

/// The drafted groups as CHANGELOG markdown. With nothing user-visible, the
/// release still gets an entry — the project's own wording for one when it
/// has used it before — so no version on the releases page is left blank.
fn render(groups: &[(&str, Vec<String>)], changelog: &str) -> String {
    let parts: Vec<String> = groups
        .iter()
        .filter(|(_, bullets)| !bullets.is_empty())
        .map(|(heading, bullets)| {
            let body: Vec<String> = bullets.iter().map(|b| wrap_bullet(b)).collect();
            format!("### {heading}\n\n{}", body.join("\n"))
        })
        .collect();
    if !parts.is_empty() {
        return parts.join("\n\n");
    }
    let line = changelog
        .lines()
        .find(|l| l.starts_with("- ") && l.to_lowercase().contains("no user-visible change"))
        .unwrap_or("- Build and packaging only — no user-visible change.");
    format!("### Changed\n\n{line}")
}

fn wrap_bullet(text: &str) -> String {
    let mut lines: Vec<String> = vec![];
    let mut current = String::from("-");
    for word in text.split_whitespace() {
        if current.chars().count() + 1 + word.chars().count() > WRAP && current.len() > 2 {
            lines.push(current);
            current = String::from(" ");
        }
        current.push(' ');
        current.push_str(word);
    }
    lines.push(current);
    lines.join("\n")
}

/// Puts `notes` under `## [Unreleased]`: into an existing, empty heading, or
/// as a new one above the newest version.
pub fn insert(changelog: &str, notes: &str) -> String {
    let lines: Vec<&str> = changelog.lines().collect();
    let block = |heading: &str| format!("{heading}\n\n{}\n", notes.trim_end());

    let (start, end, heading) = if let Some(at) = lines.iter().position(|l| is_unreleased(l)) {
        let end = lines[at + 1..]
            .iter()
            .position(|l| l.starts_with("## "))
            .map_or(lines.len(), |p| at + 1 + p);
        (at, end, lines[at].to_string())
    } else if let Some(at) = lines.iter().position(|l| l.starts_with("## [")) {
        (at, at, "## [Unreleased]".to_string())
    } else {
        let mut out = changelog.trim_end().to_string();
        out.push_str(&format!("\n\n{}", block("## [Unreleased]")));
        return out;
    };

    let mut out: Vec<String> = lines[..start].iter().map(|l| l.to_string()).collect();
    out.push(block(&heading));
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let mut text = out.join("\n");
    if changelog.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "# Changelog\n\nIntro.\n\n## [0.2.0] - 2026-09-01\n\n### Fixed\n\n\
                       - Old fix.\n\n## [0.1.0] - 2026-08-01\n\n### Changed\n\n\
                       - Packaging only — no user-visible change.\n";

    #[test]
    fn notes_go_in_a_new_unreleased_section_above_the_newest_version() {
        let out = insert(LOG, "### Fixed\n\n- New fix.");
        assert!(out.contains("Intro.\n\n## [Unreleased]\n\n### Fixed\n\n- New fix.\n\n## [0.2.0]"));
        assert!(unreleased_has_notes(&out));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn an_empty_unreleased_heading_is_filled_rather_than_duplicated() {
        let log = LOG.replace("## [0.2.0]", "## [Unreleased]\n\n### Added\n\n## [0.2.0]");
        let out = insert(&log, "### Fixed\n\n- New fix.");
        assert_eq!(out.matches("[Unreleased]").count(), 1);
        assert!(!out.contains("### Added"), "the empty stub is replaced");
        assert!(out.contains("- New fix.\n\n## [0.2.0]"));
    }

    #[test]
    fn a_changelog_with_no_versions_yet_gets_the_section_at_the_end() {
        let out = insert("# Changelog\n", "### Added\n\n- First.");
        assert_eq!(
            out,
            "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- First.\n"
        );
    }

    #[test]
    fn notes_are_only_counted_when_they_sit_under_unreleased() {
        assert!(
            !unreleased_has_notes(LOG),
            "bullets under versions do not count"
        );
        assert!(!unreleased_has_notes(
            "## [Unreleased]\n\n### Added\n\n## [0.1.0]\n- x"
        ));
        assert!(unreleased_has_notes("## [unreleased]\n\n- x\n"));
    }

    #[test]
    fn nothing_user_visible_reuses_the_projects_own_wording() {
        let empty = [("Added", vec![]), ("Fixed", vec![])];
        assert_eq!(
            render(&empty, LOG),
            "### Changed\n\n- Packaging only — no user-visible change."
        );
        assert!(render(&empty, "# Changelog").contains("no user-visible change"));
    }

    #[test]
    fn groups_are_rendered_in_order_and_empty_ones_left_out() {
        let groups = [
            ("Added", vec!["A thing.".to_string()]),
            ("Changed", vec![]),
            ("Fixed", vec!["B.".to_string(), "C.".to_string()]),
        ];
        assert_eq!(
            render(&groups, LOG),
            "### Added\n\n- A thing.\n\n### Fixed\n\n- B.\n- C."
        );
    }

    #[test]
    fn long_bullets_wrap_at_80_columns_with_a_hanging_indent() {
        let text = "word ".repeat(40);
        let out = wrap_bullet(text.trim());
        assert!(out.lines().all(|l| l.chars().count() <= WRAP), "{out}");
        assert!(out.starts_with("- word"));
        assert!(out.lines().skip(1).all(|l| l.starts_with("  word")));
    }

    #[test]
    fn a_commit_counts_as_app_work_only_when_it_touches_the_app() {
        let log = "\x1efeat: add release notes check (#68)\x1fAdds a workflow.\x1f\n\n\
                   .github/workflows/release-notes.yml\nscripts/release.sh\nCHANGELOG.md\nCLAUDE.md\n\
                   \x1efix: offer a build-only release (#69)\x1f\x1f\n\n\
                   src/services/flow.rs\nCHANGELOG.md\n";
        let commits = parse_log(log);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "feat: add release notes check (#68)");
        assert_eq!(commits[0].body, "Adds a workflow.");
        assert!(!commits[0].files.iter().any(|f| f == "CHANGELOG.md"));
        assert!(
            !commits[0].touches_app(),
            "a feat: that is only CI is still only CI"
        );
        assert!(commits[1].touches_app());
    }

    #[test]
    fn the_projects_release_note_rules_are_read_from_its_own_section() {
        let md = "# X\n\n## Release notes\n\nWrite for users.\n\n## Other\n\nNo.";
        assert_eq!(section(md, "release notes").unwrap(), "Write for users.");
        assert!(section(md, "missing").is_none());
    }

    #[test]
    fn the_prompt_sees_the_preamble_and_two_newest_versions_only() {
        let log = format!("{LOG}\n## [0.0.1] - 2026-01-01\n\n- Ancient.\n");
        let head = head_of(&log);
        assert!(head.contains("Intro.") && head.contains("0.1.0"));
        assert!(!head.contains("Ancient"));
    }
}
