//! A conflicted `CHANGELOG.md`, settled without asking anyone.
//!
//! Every pull request adds its notes under `## [Unreleased]`, so two of them
//! in flight always clash there — and a release in between renames the
//! heading on the base, which turns the clash into one no side-picking gets
//! right: taking both left two `[Unreleased]` headings, and a merge on the
//! forge filed the pull request's note under a version that never shipped it.
//!
//! The fix reads the three versions git keeps rather than the markers: the
//! entries the pull request added to `[Unreleased]` since the two sides
//! parted go into the base's `[Unreleased]`, under the same `###` heading,
//! after what is there already. The section is created above the newest
//! version when a release has just taken it. Anything else — a pull request
//! that also edited a released section, prose in the section, CRLF line
//! endings — is left for a person, exactly as git left it.

use std::path::Path;

use super::{list, Conflicted};
use crate::services::git;

/// Settles every conflicted `CHANGELOG.md` it can, stages it, and returns
/// the paths it settled. Files it cannot settle are left as git left them.
pub async fn settle_all(repo: &str) -> Vec<String> {
    let mut settled = vec![];
    for file in list(repo).await {
        if !is_changelog(&file) {
            continue;
        }
        let side = |n: u8| {
            let spec = format!(":{n}:{}", file.path);
            async move { git::run(repo, "git", &["show", &spec]).await }
        };
        let (Ok(base), Ok(ours), Ok(theirs)) = (side(1).await, side(2).await, side(3).await) else {
            continue;
        };
        let Some(merged) = merge(&base, &ours, &theirs) else {
            continue;
        };
        if tokio::fs::write(Path::new(repo).join(&file.path), &merged)
            .await
            .is_err()
        {
            continue;
        }
        if git::run(repo, "git", &["add", "--", &file.path])
            .await
            .is_ok()
        {
            settled.push(file.path);
        }
    }
    settled
}

/// A changelog edited on both sides, by its file name in any folder.
fn is_changelog(file: &Conflicted) -> bool {
    file.code == "UU" && is_changelog_path(&file.path)
}

/// The base's changelog with the entries `ours` added to `[Unreleased]`
/// since `base` filed into its `[Unreleased]`. `None` when this cannot be
/// done safely, which leaves the conflict to a person.
pub fn merge(base: &str, ours: &str, theirs: &str) -> Option<String> {
    if [base, ours, theirs].iter().any(|t| t.contains('\r')) {
        return None;
    }
    // Only `[Unreleased]` may differ: a pull request that rewrote a released
    // section meant something this cannot guess.
    if outside_unreleased(ours) != outside_unreleased(base) {
        return None;
    }
    let before = entries(unreleased(base).unwrap_or(""))?;
    let added: Vec<(String, String)> = entries(unreleased(ours).unwrap_or(""))?
        .into_iter()
        .filter(|(_, e)| !before.iter().any(|(_, b)| same(b, e)))
        // Already on the base — the same note, merged through another
        // pull request or a release — is not added twice.
        .filter(|(_, e)| !squashed(theirs).contains(&squashed(e)))
        .collect();
    if added.is_empty() {
        return Some(theirs.to_string());
    }

    let (head, section, tail) = match split(theirs) {
        Some((head, section, tail)) => (head, entries(section)?, tail),
        // A release took `[Unreleased]`: open a fresh one above the newest
        // version, or at the end of a file that has none yet.
        None => match theirs.find("\n## ") {
            Some(at) => (&theirs[..at + 1], vec![], &theirs[at + 1..]),
            None => (theirs, vec![], ""),
        },
    };
    let mut all = section;
    all.extend(added);

    let mut out = String::from(head);
    if !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
    out.push_str("## [Unreleased]\n\n");
    // Ends on a blank line, which is what keeps it apart from what follows.
    out.push_str(&render(&all));
    if tail.is_empty() {
        out.truncate(out.trim_end().len());
        out.push('\n');
    } else {
        out.push_str(tail);
    }
    Some(out)
}

/// The notes `ours` added to `[Unreleased]` that a merge git did without a
/// clash filed somewhere else in `merged` — under a version `theirs`
/// released meanwhile, which never shipped them. A release that only renames
/// the heading is lines away from the pull request's note, so git merges
/// the two cleanly and the note lands under the new version. Each note by
/// its first line. Empty when every note is where it belongs, or already
/// on `theirs` through another pull request.
pub fn misfiled(base: &str, ours: &str, theirs: &str, merged: &str) -> Vec<String> {
    let Some(before) = entries(unreleased(base).unwrap_or("")) else {
        return vec![];
    };
    let Some(added) = entries(unreleased(ours).unwrap_or("")) else {
        return vec![];
    };
    let filed = squashed(unreleased(merged).unwrap_or(""));
    let shipped = squashed(theirs);
    added
        .into_iter()
        .map(|(_, e)| e)
        .filter(|e| !before.iter().any(|(_, b)| same(b, e)))
        .filter(|e| !shipped.contains(&squashed(e)))
        .filter(|e| !filed.contains(&squashed(e)))
        .map(|e| e.lines().next().unwrap_or_default().trim().to_string())
        .collect()
}

/// A changelog by its file name, in any folder.
pub fn is_changelog_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("CHANGELOG.md"))
}

/// The `[Unreleased]` heading's line, case aside.
fn is_unreleased(line: &str) -> bool {
    line.starts_with("## ") && line.to_ascii_lowercase().contains("[unreleased]")
}

/// The text before the `[Unreleased]` heading, the section under it, and
/// everything from the next `## ` heading on.
fn split(text: &str) -> Option<(&str, &str, &str)> {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if is_unreleased(line.trim_end()) {
            let body = at + line.len();
            let end = text[body..]
                .find("\n## ")
                .map(|i| body + i + 1)
                .unwrap_or(text.len());
            return Some((&text[..at], &text[body..end], &text[end..]));
        }
        at += line.len();
    }
    None
}

fn unreleased(text: &str) -> Option<&str> {
    split(text).map(|(_, section, _)| section)
}

/// Everything but `[Unreleased]`, blank lines and wrapping aside.
fn outside_unreleased(text: &str) -> String {
    match split(text) {
        Some((head, _, tail)) => squashed(&format!("{head}\n{tail}")),
        None => squashed(text),
    }
}

/// Each entry with the `###` heading it sits under. `None` for a section
/// with anything but headings, entries and blank lines in it.
fn entries(section: &str) -> Option<Vec<(String, String)>> {
    let mut found: Vec<(String, String)> = vec![];
    let mut heading = String::new();
    for line in section.lines() {
        if let Some(h) = line.strip_prefix("### ") {
            heading = h.trim().to_string();
        } else if line.starts_with("- ") || line.starts_with("* ") {
            found.push((heading.clone(), format!("{line}\n")));
        } else if line.starts_with([' ', '\t']) && !line.trim().is_empty() {
            // A wrapped entry's next line.
            found.last_mut()?.1.push_str(&format!("{line}\n"));
        } else if !line.trim().is_empty() {
            return None;
        }
    }
    Some(found)
}

/// The section again: headings in Keep a Changelog's order, then any others
/// in the order first seen, each entry where it was put.
fn render(all: &[(String, String)]) -> String {
    const ORDER: [&str; 6] = [
        "Added",
        "Changed",
        "Deprecated",
        "Removed",
        "Fixed",
        "Security",
    ];
    let mut headings: Vec<&str> = vec![];
    for (h, _) in all {
        if !headings.contains(&h.as_str()) {
            headings.push(h);
        }
    }
    let rank = |h: &str| ORDER.iter().position(|o| *o == h).unwrap_or(ORDER.len());
    headings.sort_by_key(|h| rank(h));
    let mut out = String::new();
    for h in headings {
        if !h.is_empty() {
            out.push_str(&format!("### {h}\n\n"));
        }
        for (_, entry) in all.iter().filter(|(eh, _)| eh == h) {
            out.push_str(entry);
        }
        out.push('\n');
    }
    out
}

/// Whitespace aside: rewrapping a note does not make it another note.
fn squashed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn same(a: &str, b: &str) -> bool {
    squashed(a) == squashed(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: &str = "# Changelog\n\nIntro.\n\n";

    #[test]
    fn two_pull_requests_keep_both_notes_under_one_heading() {
        let base = format!("{TOP}## [0.1.0] - 2026-01-01\n\n### Added\n\n- First.\n");
        let ours = format!(
            "{TOP}## [Unreleased]\n\n### Fixed\n\n- Ours, wrapped\n  over two lines.\n\n## [0.1.0] - 2026-01-01\n\n### Added\n\n- First.\n"
        );
        let theirs = format!(
            "{TOP}## [Unreleased]\n\n### Changed\n\n- Theirs.\n\n### Fixed\n\n- Theirs too.\n\n## [0.1.0] - 2026-01-01\n\n### Added\n\n- First.\n"
        );
        let merged = merge(&base, &ours, &theirs).unwrap();
        assert_eq!(
            merged,
            format!(
                "{TOP}## [Unreleased]\n\n### Changed\n\n- Theirs.\n\n### Fixed\n\n- Theirs too.\n- Ours, wrapped\n  over two lines.\n\n## [0.1.0] - 2026-01-01\n\n### Added\n\n- First.\n"
            )
        );
        assert_eq!(merged.matches("[Unreleased]").count(), 1);
    }

    #[test]
    fn a_release_in_between_gets_a_fresh_unreleased_section_not_our_note() {
        // gitagent#239: main cut 0.1.190 from [Unreleased] while the pull
        // request added its own note there. Its note must not land in 0.1.190.
        let base = format!(
            "{TOP}## [Unreleased]\n\n### Changed\n\n- Shipped.\n\n## [0.1.189] - 2026-11-16\n"
        );
        let ours = format!(
            "{TOP}## [Unreleased]\n\n### Changed\n\n- Shipped.\n\n### Fixed\n\n- Ours.\n\n## [0.1.189] - 2026-11-16\n"
        );
        let theirs = format!(
            "{TOP}## [0.1.190] - 2026-11-17\n\n### Changed\n\n- Shipped.\n\n## [0.1.189] - 2026-11-16\n"
        );
        let merged = merge(&base, &ours, &theirs).unwrap();
        assert_eq!(
            merged,
            format!(
                "{TOP}## [Unreleased]\n\n### Fixed\n\n- Ours.\n\n## [0.1.190] - 2026-11-17\n\n### Changed\n\n- Shipped.\n\n## [0.1.189] - 2026-11-16\n"
            )
        );
    }

    #[test]
    fn a_note_already_on_the_base_is_not_added_twice() {
        let base = format!("{TOP}## [0.1.0]\n");
        let ours = format!("{TOP}## [Unreleased]\n\n### Fixed\n\n- Same note.\n\n## [0.1.0]\n");
        let theirs = format!("{TOP}## [0.1.1]\n\n### Fixed\n\n- Same\n  note.\n\n## [0.1.0]\n");
        assert_eq!(merge(&base, &ours, &theirs).unwrap(), theirs);
    }

    #[test]
    fn an_edit_to_a_released_section_is_left_for_a_person() {
        let base = format!("{TOP}## [0.1.0]\n\n- Old wording.\n");
        let ours = format!("{TOP}## [Unreleased]\n\n- New.\n\n## [0.1.0]\n\n- New wording.\n");
        let theirs = format!("{TOP}## [Unreleased]\n\n- Theirs.\n\n## [0.1.0]\n\n- Old wording.\n");
        assert!(merge(&base, &ours, &theirs).is_none());
    }

    #[test]
    fn prose_in_the_section_or_crlf_is_left_for_a_person() {
        let base = format!("{TOP}## [0.1.0]\n");
        let ours = format!("{TOP}## [Unreleased]\n\nSome prose.\n\n## [0.1.0]\n");
        assert!(merge(&base, &ours, &base).is_none());
        let crlf = base.replace('\n', "\r\n");
        assert!(merge(&crlf, &crlf, &crlf).is_none());
    }

    /// In a real repository: a release on main, with a last note, while the
    /// pull request added its own at the same spot, then main brought in.
    /// (A release alone, with no note at the spot, merges without a clash —
    /// and files the pull request's note under the release.)
    #[tokio::test]
    async fn a_real_merge_is_settled_and_staged() {
        let dir = std::env::temp_dir().join(format!(
            "gitagent-test-changelog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let repo = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            out.status.success()
        };
        let write = |text: &str| std::fs::write(dir.join("CHANGELOG.md"), text).unwrap();
        assert!(git(&["init", "-q", "-b", "main"]));
        write(&format!(
            "{TOP}## [Unreleased]\n\n### Changed\n\n- Shipped.\n\n## [0.1.0]\n"
        ));
        assert!(git(&["add", "."]) && git(&["commit", "-qm", "base"]));
        assert!(git(&["checkout", "-qb", "pr"]));
        write(&format!(
            "{TOP}## [Unreleased]\n\n### Changed\n\n- Shipped.\n- Ours.\n\n## [0.1.0]\n"
        ));
        assert!(git(&["commit", "-qam", "ours"]));
        assert!(git(&["checkout", "-q", "main"]));
        write(&format!(
            "{TOP}## [0.1.1] - 2026-11-17\n\n### Changed\n\n- Shipped.\n- Theirs.\n\n## [0.1.0]\n"
        ));
        assert!(git(&["commit", "-qam", "release"]));
        assert!(git(&["checkout", "-q", "pr"]));
        assert!(!git(&["merge", "-q", "main"]), "the two sides clash");

        assert_eq!(settle_all(repo).await, vec!["CHANGELOG.md".to_string()]);
        let text = std::fs::read_to_string(dir.join("CHANGELOG.md")).unwrap();
        assert_eq!(
            text,
            format!(
                "{TOP}## [Unreleased]\n\n### Changed\n\n- Ours.\n\n## [0.1.1] - 2026-11-17\n\n### Changed\n\n- Shipped.\n- Theirs.\n\n## [0.1.0]\n"
            )
        );
        assert!(
            list(repo).await.is_empty(),
            "staged: nothing left in conflict"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// gitagent#239 for real: main released `[Unreleased]` and nothing else,
    /// the pull request added a note, and git merges them with no clash —
    /// filing the note under the release. Caught, and put right.
    #[test]
    fn a_clean_merge_that_files_a_note_under_a_release_is_caught() {
        let dir = std::env::temp_dir().join(format!(
            "gitagent-test-misfiled-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let write = |text: &str| std::fs::write(dir.join("CHANGELOG.md"), text).unwrap();
        let base = format!("{TOP}## [Unreleased]\n\n### Changed\n\n- Shipped.\n\n## [0.1.0]\n");
        let ours = format!(
            "{TOP}## [Unreleased]\n\n### Changed\n\n- Shipped.\n\n### Fixed\n\n- Ours.\n\n## [0.1.0]\n"
        );
        let theirs =
            format!("{TOP}## [0.1.1] - 2026-11-17\n\n### Changed\n\n- Shipped.\n\n## [0.1.0]\n");
        git(&["init", "-q", "-b", "main"]);
        write(&base);
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["checkout", "-qb", "pr"]);
        write(&ours);
        git(&["commit", "-qam", "ours"]);
        git(&["checkout", "-q", "main"]);
        write(&theirs);
        git(&["commit", "-qam", "release"]);

        let tree = git(&["merge-tree", "--write-tree", "main", "pr"]);
        let merged = git(&[
            "show",
            &format!("{}:CHANGELOG.md", tree.lines().next().unwrap()),
        ]);
        assert!(
            merged.contains(
                "## [0.1.1] - 2026-11-17\n\n### Changed\n\n- Shipped.\n\n### Fixed\n\n- Ours."
            ),
            "git files the note under the release: {merged}"
        );
        assert_eq!(misfiled(&base, &ours, &theirs, &merged), vec!["- Ours."]);
        let fixed = merge(&base, &ours, &theirs).unwrap();
        assert!(
            misfiled(&base, &ours, &theirs, &fixed).is_empty(),
            "{fixed}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_note_filed_where_it_belongs_or_already_shipped_is_not_flagged() {
        let base = format!("{TOP}## [0.1.0]\n");
        let ours = format!("{TOP}## [Unreleased]\n\n### Fixed\n\n- Ours.\n\n## [0.1.0]\n");
        assert!(misfiled(&base, &ours, &base, &ours).is_empty());
        let shipped = format!("{TOP}## [0.1.1]\n\n### Fixed\n\n- Ours.\n\n## [0.1.0]\n");
        assert!(misfiled(&base, &ours, &shipped, &shipped).is_empty());
    }

    #[test]
    fn only_a_file_named_changelog_edited_on_both_sides_counts() {
        let file = |path: &str, code: &str| Conflicted {
            path: path.into(),
            code: code.into(),
        };
        assert!(is_changelog(&file("CHANGELOG.md", "UU")));
        assert!(is_changelog(&file("crates/core/changelog.md", "UU")));
        assert!(!is_changelog(&file("CHANGELOG.md", "UD")));
        assert!(!is_changelog(&file("docs/CHANGES.md", "UU")));
    }
}
