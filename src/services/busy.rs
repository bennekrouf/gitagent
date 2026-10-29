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

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::store;

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
}

/// Repositories this process holds, so a claim file naming this process can
/// be told apart from one left behind by an earlier process with the same id.
fn held_here() -> &'static Mutex<HashSet<String>> {
    static HELD: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    HELD.get_or_init(|| Mutex::new(HashSet::new()))
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
        if read(&self.file).is_some_and(|h| h.pid == std::process::id() && h.token == self.token) {
            let _ = std::fs::remove_file(&self.file);
        }
        if let Ok(mut held) = held_here().lock() {
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

/// Whether `holder` still holds the repository. A claim older than a day is
/// let go whatever it says: by then its process id may belong to something
/// else entirely.
fn still_held(holder: &Holder, key: &str) -> bool {
    if now().saturating_sub(holder.since) > 24 * 3600 {
        return false;
    }
    if holder.pid == std::process::id() {
        return held_here().lock().is_ok_and(|held| held.contains(key));
    }
    alive(holder.pid)
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
                    held.insert(key.clone());
                }
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
        let dir = std::env::temp_dir().join(format!(
            "gitagent-test-busy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
        };
        // This process's id, but not held here: what a crashed earlier
        // process with the same id would have left.
        std::fs::write(&file, serde_json::to_vec(&dead).unwrap()).unwrap();
        claim(&path, "Commit → PR", By::Window).expect("stale claim taken over");
    }

    #[test]
    fn different_repositories_do_not_block_each_other() {
        let (a, b) = (repo(), repo());
        let _a = claim(&a.to_string_lossy(), "Release", By::Window).unwrap();
        claim(&b.to_string_lossy(), "Release", By::Window).expect("other repository");
    }
}
