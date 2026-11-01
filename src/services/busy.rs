//! One run at a time per repository, across every GitAgent process.
//!
//! The window, `gitagent run` and each `gitagent mcp` are separate processes —
//! every Claude session starts its own server — and each already refuses a
//! second run of its own in a repository. None of them could see the others'.
//! Two runs that both scanned the same working tree then both committed it:
//! the first made the pull request, and the second found nothing left to stage
//! on a branch it had just created.
//!
//! So a run claims the repository with a file in the data directory, created
//! with `create_new` so two claims can never both succeed. The file names the
//! process that holds it; a claim whose process is gone is stale and taken
//! over, which is how a crash or a closed Claude session lets go.
//!
//! Gone is not the same question as "is that process id alive", though. The
//! file is deleted by the claim's own `Drop`, which a force quit, a crash, or
//! a Claude session killing its server never reaches — and once the dead
//! holder's process id is handed out again, `kill -0` says yes about a run
//! that ended hours ago. So a live run also *touches* its claim, on a thread
//! of its own, and a claim nobody has touched for `GRACE` is gone whatever
//! its process id now belongs to.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::store;

/// How often a run touches the claim it holds.
const BEAT_EVERY: Duration = Duration::from_secs(20);

/// How long a claim is respected after the last touch. Generously longer than
/// `BEAT_EVERY`: a machine coming back from sleep, or a step that pins the
/// disk, must not cost a live run its repository.
const GRACE: u64 = 120;

/// Who started the run, for the sentence that says what is in the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum By {
    Window,
    Claude,
    Terminal,
}

impl By {
    fn phrase(self) -> &'static str {
        match self {
            By::Window => "in a GitAgent window",
            By::Claude => "by Claude, through GitAgent's MCP server",
            By::Terminal => "from a terminal",
        }
    }

    fn where_to_answer(self) -> &'static str {
        match self {
            By::Window => "Wait for it to finish, or stop it in that window.",
            By::Claude => {
                "Wait for it to finish, or answer or reject its approval in Claude — \
                 ending that Claude session lets it go too."
            }
            By::Terminal => "Wait for it to finish, or answer or stop it in that terminal.",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Holder {
    pid: u32,
    /// Tells two claims by the same process apart, so dropping one never
    /// removes a file another has since written.
    token: u64,
    repo: String,
    what: String,
    by: By,
    /// Seconds since the epoch.
    since: u64,
    /// When the holder last said it was still there, seconds since the epoch.
    /// Zero from a version that did not touch its claims, which is why the
    /// process id still has the last word — see `still_held`.
    #[serde(default)]
    beat: u64,
}

/// Repositories this process holds, each with the token of the claim that
/// holds it: so a claim file naming this process can be told apart from one
/// left behind by an earlier process with the same id, and so the beating
/// thread touches only the files this process still owns.
fn held_here() -> &'static Mutex<HashMap<String, u64>> {
    static HELD: std::sync::OnceLock<Mutex<HashMap<String, u64>>> = std::sync::OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The repository held, released when this is dropped.
#[derive(Debug)]
pub struct Claim {
    file: PathBuf,
    token: u64,
    repo: String,
}

impl Drop for Claim {
    fn drop(&mut self) {
        // Both under one lock, and the lock the beating thread takes: a beat
        // that read this file just before it was deleted would otherwise
        // write it straight back, and a claim nobody holds would go on
        // refusing other processes until it stopped being touched.
        let held = held_here().lock();
        if read(&self.file).is_some_and(|h| h.pid == std::process::id() && h.token == self.token) {
            let _ = std::fs::remove_file(&self.file);
        }
        if let Ok(mut held) = held {
            held.remove(&self.repo);
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The same repository however it was named: symlinks and `..` resolved.
fn key(repo: &str) -> String {
    dunce::canonicalize(repo)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| repo.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// FNV-1a: stable across builds, which `DefaultHasher` does not promise, and
/// the window and an MCP server of another version must agree on the name.
fn file_for(key: &str) -> PathBuf {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in key.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    store::data_dir()
        .join("busy")
        .join(format!("{hash:016x}.json"))
}

fn read(file: &PathBuf) -> Option<Holder> {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}

/// Whether `holder` still holds the repository.
///
/// A claim that is being touched answers this on its own: nobody has said it
/// is alive for longer than `GRACE`, so it is gone, and whoever owns its
/// process id now is beside the point. A claim from a version that never
/// touched its file has only the old answer — the process id, plus a day's
/// cap, by when that id may belong to something else entirely.
fn still_held(holder: &Holder, key: &str) -> bool {
    let (last, cap) = match holder.beat {
        0 => (holder.since, 24 * 3600),
        beat => (beat, GRACE),
    };
    if now().saturating_sub(last) > cap {
        return false;
    }
    if holder.pid == std::process::id() {
        return held_here().lock().is_ok_and(|held| held.contains_key(key));
    }
    alive(holder.pid)
}

/// Touches every claim this process holds, so a holder that is gone can be
/// told from one that is merely quiet.
///
/// A thread of its own, not a task on the app's runtime: that runtime is busy
/// running flows, and a claim that stops being touched is a claim another
/// process may take. One thread for the process, started with its first
/// claim.
fn start_beating() {
    static BEATING: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    BEATING.get_or_init(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(BEAT_EVERY);
            beat();
        });
    });
}

/// One round of touching. Only files this process still owns, by both its
/// process id and the claim's token: a claim another process has since taken
/// over is not ours to keep alive, and one already deleted is not recreated.
///
/// The lock is held for the whole round, which is also what stops a claim
/// being released half-way through one — see `Claim::drop`.
fn beat() {
    let Ok(held) = held_here().lock() else {
        return;
    };
    for (key, token) in held.iter() {
        let file = file_for(key);
        let Some(mut holder) = read(&file) else {
            continue;
        };
        if holder.pid != std::process::id() || holder.token != *token {
            continue;
        }
        holder.beat = now();
        if let Ok(bytes) = serde_json::to_vec(&holder) {
            let _ = store::write_atomic(&file, &bytes);
        }
    }
}

/// Drops the claims left behind by processes that are gone — a quit part-way
/// through a run, a crash, a Claude session ended with its server still
/// holding a repository.
///
/// Called as each process starts. Without it such a claim sits there until
/// something next tries to claim that same repository, and its process id can
/// be handed out again in the meantime, which is how a repository ends up
/// refused by a run that no longer exists.
pub fn sweep() {
    sweep_in(&store::data_dir().join("busy"));
}

fn sweep_in(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.path();
        // Not the temporary files an atomic write leaves, and not a format
        // some other version may be reading: only claims this one wrote.
        if file.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        // A file that will not parse is left alone: it may be a newer
        // version's, and `claim` clears it anyway when it gets in the way.
        if let Some(holder) = read(&file) {
            let key = holder.repo.clone();
            if !still_held(&holder, &key) {
                let _ = std::fs::remove_file(&file);
            }
        }
    }
}

fn minutes_ago(since: u64) -> String {
    match now().saturating_sub(since) / 60 {
        0 => "just now".into(),
        1 => "a minute ago".into(),
        n if n < 120 => format!("{n} minutes ago"),
        n => format!("{} hours ago", n / 60),
    }
}

fn refusal(holder: &Holder) -> String {
    format!(
        "{} is already running in this repository — started {} {}. GitAgent runs one \
         flow at a time per repository: two runs on the same working tree commit the \
         same changes twice. Nothing was started. {}",
        holder.what,
        minutes_ago(holder.since),
        holder.by.phrase(),
        holder.by.where_to_answer(),
    )
}

/// Claims `repo` for a run of `what` (the flow's label), or says which run
/// already has it.
pub fn claim(repo: &str, what: &str, by: By) -> Result<Claim, String> {
    let key = key(repo);
    let file = file_for(&key);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let token = (u64::from(std::process::id()) << 32)
        ^ std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
    let holder = Holder {
        pid: std::process::id(),
        token,
        repo: key.clone(),
        what: what.to_string(),
        by,
        since: now(),
        beat: now(),
    };
    // Twice at most: once as things are, once more after clearing a stale
    // claim. Losing the second race means someone else just claimed it.
    for _ in 0..2 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
        {
            Ok(mut f) => {
                let bytes = serde_json::to_vec(&holder).unwrap_or_default();
                let _ = f.write_all(&bytes);
                if let Ok(mut held) = held_here().lock() {
                    held.insert(key.clone(), token);
                }
                start_beating();
                return Ok(Claim {
                    file,
                    token,
                    repo: key,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match read(&file) {
                Some(other) if still_held(&other, &key) => return Err(refusal(&other)),
                // Stale, or written half-way by a process that died while
                // writing it: neither holds anything.
                _ => {
                    let _ = std::fs::remove_file(&file);
                }
            },
            // The data directory cannot be written at all. Refusing every run
            // over that would be worse than the race this guards against.
            Err(_) => {
                return Ok(Claim {
                    file,
                    token,
                    repo: key,
                })
            }
        }
    }
    match read(&file) {
        Some(other) => Err(refusal(&other)),
        None => Err("Another run claimed this repository at the same moment. Try again.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> std::path::PathBuf {
        // A counter as well as the clock: these tests run in parallel in one
        // process, against one data directory, and two that share a path
        // share a claim file.
        static NTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gitagent-test-busy-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_second_claim_on_the_same_repository_is_refused() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        let first = claim(&path, "Commit → PR", By::Claude).expect("first claim");
        let second = claim(&path, "Commit → PR", By::Window).unwrap_err();
        assert!(
            second.contains("Commit → PR is already running"),
            "{second}"
        );
        assert!(second.contains("by Claude"), "{second}");
        drop(first);
        claim(&path, "Commit → PR", By::Window).expect("free once the first is dropped");
    }

    #[test]
    fn a_claim_left_by_a_process_that_is_gone_is_taken_over() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        let file = file_for(&key(&path));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let dead = Holder {
            pid: std::process::id(),
            token: 1,
            repo: key(&path),
            what: "Release".into(),
            by: By::Terminal,
            since: now(),
            beat: now(),
        };
        // This process's id, but not held here: what a crashed earlier
        // process with the same id would have left.
        std::fs::write(&file, serde_json::to_vec(&dead).unwrap()).unwrap();
        claim(&path, "Commit → PR", By::Window).expect("stale claim taken over");
    }

    /// A live process that is not this one — the test harness that started
    /// it. Not launchd: `alive` asks `kill -0`, which another user's process
    /// refuses, so pid 1 reads as gone.
    fn another_live_pid() -> u32 {
        #[cfg(unix)]
        {
            std::os::unix::process::parent_id()
        }
        #[cfg(not(unix))]
        {
            std::process::id() + 1
        }
    }

    /// A claim naming a process that is alive but is not the one that wrote
    /// it: what every process id becomes once it has been handed out again.
    fn held_by_another_process(repo: &str, what: &str, since: u64, beat: Option<u64>) -> PathBuf {
        let file = file_for(&key(repo));
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut holder = serde_json::json!({
            "pid": another_live_pid(),
            "token": 7,
            "repo": key(repo),
            "what": what,
            "by": "Claude",
            "since": since,
        });
        if let Some(beat) = beat {
            holder["beat"] = beat.into();
        }
        std::fs::write(&file, serde_json::to_vec(&holder).unwrap()).unwrap();
        file
    }

    #[test]
    fn a_claim_nobody_has_touched_is_taken_over_however_alive_its_process_id_looks() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        held_by_another_process(
            &path,
            "Deploy",
            now() - GRACE - 600,
            Some(now() - GRACE - 1),
        );
        claim(&path, "Commit \u{2192} PR", By::Window).expect("a claim that stopped beating");
    }

    #[test]
    fn a_claim_being_touched_is_respected_however_long_the_run_has_gone_on() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        let file = held_by_another_process(&path, "Deploy", now() - 10 * 3600, Some(now()));
        let refused = claim(&path, "Commit \u{2192} PR", By::Window).unwrap_err();
        assert!(refused.contains("Deploy is already running"), "{refused}");
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn a_claim_from_a_version_that_never_touched_its_file_still_holds() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        // No `beat` at all: only the process id and the day's cap to go on,
        // and stealing it from a live process would run two flows at once.
        let file = held_by_another_process(&path, "Release", now() - 600, None);
        let refused = claim(&path, "Commit \u{2192} PR", By::Window).unwrap_err();
        assert!(refused.contains("Release is already running"), "{refused}");
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn a_held_claim_is_touched_while_the_run_goes_on() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        let held = claim(&path, "Commit \u{2192} PR", By::Window).expect("claim");
        let file = file_for(&key(&path));
        // As it would look after a quiet minute, which is what `beat` is for.
        let mut holder = read(&file).expect("the claim file");
        holder.beat = now() - 60;
        std::fs::write(&file, serde_json::to_vec(&holder).unwrap()).unwrap();

        beat();

        let touched = read(&file).expect("the claim file");
        assert!(
            now().saturating_sub(touched.beat) < 5,
            "the claim should have been touched, beat was {} ago",
            now().saturating_sub(touched.beat)
        );
        drop(held);
    }

    #[test]
    fn a_claim_another_process_took_over_is_not_touched_again() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        let held = claim(&path, "Commit \u{2192} PR", By::Window).expect("claim");
        let file = file_for(&key(&path));
        // The claim file as another process would have rewritten it.
        let theirs = held_by_another_process(&path, "Deploy", now() - 5, Some(now() - 60));
        let before = read(&theirs).expect("the claim file").beat;

        beat();

        let after = read(&theirs).expect("the claim file");
        assert_eq!(
            after.pid,
            another_live_pid(),
            "someone else's claim was overwritten"
        );
        assert_eq!(after.beat, before, "someone else's claim was touched");
        drop(held);
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn a_claim_that_was_released_is_not_written_back_by_the_next_beat() {
        let dir = repo();
        let path = dir.to_string_lossy().into_owned();
        let file = file_for(&key(&path));
        drop(claim(&path, "Release", By::Window).expect("claim"));
        assert!(!file.exists(), "dropping the claim frees the repository");

        beat();

        assert!(
            !file.exists(),
            "the beat put a claim nobody holds back on disk"
        );
    }

    #[test]
    fn a_startup_sweep_drops_the_claims_of_processes_that_are_gone() {
        let dir = std::env::temp_dir().join(format!(
            "gitagent-test-sweep-{}-{}",
            std::process::id(),
            now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, holder: serde_json::Value| {
            let file = dir.join(name);
            std::fs::write(&file, serde_json::to_vec(&holder).unwrap()).unwrap();
            file
        };
        // Left behind by a quit part-way through a run: this process's id,
        // which it never held.
        let gone = write(
            "gone.json",
            serde_json::json!({
                "pid": std::process::id(), "token": 3, "repo": "/r",
                "what": "Deploy", "by": "Window", "since": now(), "beat": now(),
            }),
        );
        // A run that is still going, in another process.
        let live = write(
            "live.json",
            serde_json::json!({
                "pid": another_live_pid(), "token": 4, "repo": "/s",
                "what": "Release", "by": "Claude", "since": now(), "beat": now(),
            }),
        );
        // Not ours to read, and not ours to delete.
        let other = write("other.json", serde_json::json!({"shape": "unknown"}));
        let tmp = dir.join(".gone.json.1.tmp");
        std::fs::write(&tmp, b"half-written").unwrap();

        sweep_in(&dir);

        assert!(
            !gone.exists(),
            "a claim whose holder is gone should be swept"
        );
        assert!(live.exists(), "a run still going keeps its repository");
        assert!(other.exists(), "an unreadable claim is left alone");
        assert!(tmp.exists(), "only claim files are swept");
    }

    #[test]
    fn different_repositories_do_not_block_each_other() {
        let (a, b) = (repo(), repo());
        let _a = claim(&a.to_string_lossy(), "Release", By::Window).unwrap();
        claim(&b.to_string_lossy(), "Release", By::Window).expect("other repository");
    }
}
