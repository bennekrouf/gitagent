//! Whether a repository's uncommitted files have stopped changing.
//!
//! A coding agent — Claude, Cursor, anything — writes files in bursts, often
//! for minutes. Committing in the middle of one takes half an edit: a file
//! written, the one that calls it not yet. Nothing on disk says "an agent is
//! working here", and asking each tool would only cover the tools we know of.
//! What every one of them does is change files, so that is what is watched:
//! the uncommitted files' modification times, and the set of them. Once
//! neither has moved for `WINDOW`, whatever was writing has paused.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use super::git;

/// How long the files must sit still before they count as settled. Long
/// enough to span an agent thinking between two edits, short enough that a
/// person pressing All on a finished tree barely waits.
pub const WINDOW: Duration = Duration::from_secs(20);

/// How long to keep waiting for a repository whose files keep changing
/// before leaving it alone. Something still writing after this is busy, and
/// is better left to finish than committed under.
pub const LIMIT: Duration = Duration::from_secs(120);

/// Each uncommitted file, with when it last changed and its size. A deleted
/// file has neither, and still counts: one appearing or going is a change.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Snapshot {
    files: BTreeMap<String, Option<(SystemTime, u64)>>,
}

impl Snapshot {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// When the most recently changed file was changed.
    pub fn newest(&self) -> Option<SystemTime> {
        self.files.values().flatten().map(|(at, _)| *at).max()
    }

    /// How long the files have sat still, as of `now`. A file stamped in
    /// the future — a clock skew, an archive extracted with its times — reads
    /// as just written, which only costs a wait.
    pub fn still_for(&self, now: SystemTime) -> Duration {
        match self.newest() {
            Some(at) => now.duration_since(at).unwrap_or(Duration::ZERO),
            None => Duration::MAX,
        }
    }
}

/// Reads every uncommitted file — untracked ones one by one, not as their
/// directory, since a new file inside a new directory is exactly what an
/// agent writes.
pub async fn snapshot(repo: &str) -> Result<Snapshot, String> {
    let out = git::run(
        repo,
        "git",
        &[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            git::IGNORE_DIRTY_SUBMODULES,
        ],
    )
    .await?;
    let root = std::path::Path::new(repo);
    let files = paths(&out)
        .into_iter()
        .map(|path| {
            let seen = std::fs::symlink_metadata(root.join(&path))
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len())));
            (path, seen)
        })
        .collect();
    Ok(Snapshot { files })
}

/// The paths in `git status --porcelain -z` output. A rename or copy is
/// followed by its old path as an entry of its own, which is skipped: the
/// old path is gone from disk and says nothing the new one does not.
fn paths(porcelain: &str) -> Vec<String> {
    let mut out = vec![];
    let mut entries = porcelain.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        out.push(path.to_string());
        if code.contains('R') || code.contains('C') {
            entries.next();
        }
    }
    out
}

/// How a wait ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quiet {
    /// Nothing changed for a whole `WINDOW` — or there was nothing
    /// uncommitted to watch.
    Settled,
    /// Still changing after `LIMIT`.
    StillWriting,
}

/// Waits until the uncommitted files in `repo` have sat still for `WINDOW`,
/// giving up after `LIMIT`. `on_wait` hears how many seconds are left before
/// the files would count as settled, as long as they keep still; it is
/// called each time that changes.
///
/// Settled means both: the newest file is older than the window, *and* the
/// set of files and their sizes is what it was at the last look. Times alone
/// would miss a tool that writes files with old modification times.
pub async fn wait_until_quiet(repo: &str, mut on_wait: impl FnMut(u64)) -> Quiet {
    let started = std::time::Instant::now();
    let mut last: Option<Snapshot> = None;
    // When the set of files or their sizes last moved, once seen to move.
    // Until then the files' own times are all there is to go on.
    let mut moved_at: Option<std::time::Instant> = None;
    loop {
        let Ok(now) = snapshot(repo).await else {
            // git refused — not something waiting longer fixes, and not
            // a reason to believe anything is writing.
            return Quiet::Settled;
        };
        if now.is_empty() {
            return Quiet::Settled;
        }
        if last.as_ref().is_some_and(|prev| prev != &now) {
            moved_at = Some(std::time::Instant::now());
        }
        let age = now.still_for(SystemTime::now());
        let still = match moved_at {
            Some(at) => age.min(at.elapsed()),
            None => age,
        };
        if still >= WINDOW {
            return Quiet::Settled;
        }
        if started.elapsed() >= LIMIT {
            return Quiet::StillWriting;
        }
        on_wait((WINDOW - still).as_secs().max(1));
        last = Some(now);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untracked_files_and_renames_are_read_by_their_paths() {
        let out = " M src/a.rs\0?? new/dir/b.rs\0R  c.rs\0old_c.rs\0 D gone.rs\0";
        assert_eq!(
            paths(out),
            vec!["src/a.rs", "new/dir/b.rs", "c.rs", "gone.rs"]
        );
    }

    #[test]
    fn a_file_just_written_has_not_been_still_for_long() {
        let now = SystemTime::now();
        let snap = Snapshot {
            files: BTreeMap::from([
                (
                    "old.rs".to_string(),
                    Some((now - Duration::from_secs(600), 10)),
                ),
                (
                    "hot.rs".to_string(),
                    Some((now - Duration::from_secs(3), 10)),
                ),
                ("gone.rs".to_string(), None),
            ]),
        };
        assert_eq!(snap.still_for(now), Duration::from_secs(3));
        assert!(snap.still_for(now) < WINDOW);
    }

    #[test]
    fn a_file_stamped_in_the_future_counts_as_just_written() {
        let now = SystemTime::now();
        let snap = Snapshot {
            files: BTreeMap::from([(
                "skewed.rs".to_string(),
                Some((now + Duration::from_secs(60), 1)),
            )]),
        };
        assert_eq!(snap.still_for(now), Duration::ZERO);
    }

    #[test]
    fn only_deleted_files_have_nothing_to_wait_for() {
        let snap = Snapshot {
            files: BTreeMap::from([("gone.rs".to_string(), None)]),
        };
        assert!(snap.still_for(SystemTime::now()) >= WINDOW);
    }
}
