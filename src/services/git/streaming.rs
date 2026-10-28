//! Commands whose output is shown as it comes, and the commit itself.

use super::*;

/// Whether `name` already names a local branch.
///
/// A bare `rev-parse --verify <name>` resolves anything: a tag, a remote-
/// tracking ref, even a raw object id. The commit step uses this to decide
/// whether it has to disambiguate a new branch name, and a tag of the same
/// name would have sent it renaming a branch that did not exist. A caller
/// wanting a fully-qualified ref passes one — `refs/remotes/origin/main`
/// already starts with `refs/` and is used as given.
pub async fn branch_exists(repo: &str, name: &str) -> bool {
    let git_ref = if name.starts_with("refs/") {
        name.to_string()
    } else {
        format!("refs/heads/{name}")
    };
    run(repo, "git", &["rev-parse", "--verify", "--quiet", &git_ref])
        .await
        .is_ok()
}

/// Stages exactly the paths listed — never `git add -A`. The approval step
/// shows this list, so the two must not be able to drift apart.
/// Which of `wanted` still need `git add`.
///
/// A porcelain code is two columns: index, then worktree. A space in the
/// worktree column means the path is already fully staged and there is
/// nothing left to add — and calling `git add` on one of those is not merely
/// redundant, it fails outright when the path is also gitignored, because git
/// treats it as a new file being added rather than a tracked one being
/// removed. A staged deletion of an ignored file hits exactly that.
///
/// Anything we cannot classify is included, so unknown states behave as before.
pub fn needs_staging(changes: &[FileChange], wanted: &[String]) -> Vec<String> {
    wanted
        .iter()
        .filter(|path| {
            changes
                .iter()
                .find(|c| &c.path == *path)
                .map(|c| c.code.chars().nth(1).unwrap_or(' ') != ' ')
                .unwrap_or(true)
        })
        .cloned()
        .collect()
}

/// Commits whatever is in the index.
///
/// Deliberately no pathspec: `git commit -- <paths>` would commit the *working
/// tree* content of those paths and throw away any partial staging, so a
/// carefully `git add -p`-ed hunk would silently become the whole file. The
/// caller makes the index match the approval instead — staging what was
/// checked, unstaging what was not — and then this commits it as it stands.
pub async fn commit(repo: &str, subject: &str, body: &str) -> Result<String, String> {
    let mut args = vec!["commit", "-m", subject];
    if !body.trim().is_empty() {
        args.push("-m");
        args.push(body);
    }
    run(repo, "git", &args).await
}

/// Spawns `program args...` — in `cwd` when given — writes `stdin` to it
/// first if non-empty, and streams combined stdout+stderr to `on_line` one
/// line at a time as it arrives, not only once the process exits. Every
/// other runner in this module is a thin wrapper over this with a no-op
/// callback, so there is exactly one place that spawns a child and reads its
/// output — a live "Running…" log and a final captured one are the same
/// data, read at a different time.
pub async fn run_streaming(
    program: &str,
    args: &[String],
    cwd: Option<&str>,
    stdin: &str,
    on_line: &mut dyn FnMut(&str),
) -> (bool, String) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::sync::mpsc;

    let mut cmd = Command::new(crate::services::env::resolve_program(program));
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.stdin(if stdin.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    // A step's child must not outlive the run that started it. With this, a
    // dropped future — the window closing, or the task being aborted — kills
    // the process instead of leaving it running with nothing reading it.
    cmd.kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return (false, spawn_error(program, &e)),
    };

    // Written from its own task rather than inline. A child that does not
    // drain stdin fills the pipe buffer at around 64 KB, and an inline
    // `write_all` would then block here — before either reader below has
    // started — with the child in turn blocked writing output nobody is
    // reading. Neither side can move: a textbook pipe deadlock. Today's
    // payload is one short line, so it never fires; the ordering is still
    // the thing that would make it fire.
    if !stdin.is_empty() {
        if let Some(mut pipe) = child.stdin.take() {
            let mut answer = stdin.to_string();
            if !answer.ends_with('\n') {
                answer.push('\n');
            }
            tokio::spawn(async move {
                let _ = pipe.write_all(answer.as_bytes()).await;
                let _ = pipe.shutdown().await;
            });
        }
    }

    // Both streams feed one channel so a line reaches `on_line` in the order
    // it actually arrived, rather than all of stdout followed by all of
    // stderr the way a single buffered `.output()` read would show it.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let out_tx = tx.clone();
    let out_task = tokio::spawn(async move {
        if let Some(out) = stdout {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = out_tx.send(line);
            }
        }
    });
    let err_task = tokio::spawn(async move {
        if let Some(err) = stderr {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(line);
            }
        }
    });

    // Reading the pipes and reaping the child are raced rather than
    // sequenced. Draining until both pipes close is the natural way to write
    // this and it hangs forever on a step that backgrounds a daemon: the
    // grandchild inherits stdout, so the pipe stays open long after the
    // command itself exited. Once the child is reaped, the tail of the output
    // gets a grace period and then the loop gives up on whatever is still
    // holding the pipe.
    const DRAIN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

    let mut collected = String::new();
    let mut exited: Option<std::process::ExitStatus> = None;
    // Far enough out to be inert until the child actually exits and resets it.
    let grace = tokio::time::sleep(std::time::Duration::from_secs(60 * 60 * 24));
    tokio::pin!(grace);

    loop {
        tokio::select! {
            // Output first: a line already in hand is never dropped in favour
            // of noticing the exit that followed it.
            biased;
            line = rx.recv() => match line {
                Some(line) => {
                    on_line(&line);
                    if !collected.is_empty() {
                        collected.push('\n');
                    }
                    collected.push_str(&line);
                }
                // Both pipes closed: the ordinary end of a command.
                None => break,
            },
            // `Child::wait` is cancel-safe, and the guard keeps it from being
            // polled again once it has produced a status.
            status = child.wait(), if exited.is_none() => {
                exited = Some(status.unwrap_or_else(|_| Default::default()));
                grace.as_mut().reset(tokio::time::Instant::now() + DRAIN_GRACE);
            }
            _ = &mut grace => break,
        }
    }

    out_task.abort();
    err_task.abort();

    // Not reaped above only if the drain ended first, which means the pipes
    // closed before the child was waited on — so this returns immediately.
    let status = match exited {
        Some(status) => Ok(status),
        None => child.wait().await,
    };
    let ok = matches!(status, Ok(status) if status.success());
    (ok, cap(collected.trim()))
}

/// Runs a shell command in `repo`, optionally writing `stdin` to it first,
/// reporting each line of output as it arrives — a step that streams into
/// the run's live log, rather than only showing it once the command
/// finishes. Returns success plus the full combined output either way.
pub async fn run_shell_streaming(
    repo: &str,
    command: &str,
    stdin: &str,
    on_line: &mut dyn FnMut(&str),
) -> (bool, String) {
    run_streaming(
        "sh",
        &["-c".to_string(), command.to_string()],
        Some(repo),
        stdin,
        on_line,
    )
    .await
}

pub async fn gh_pr_create(
    repo: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Result<String, String> {
    let out = run(
        repo,
        "gh",
        &[
            "pr", "create", "--base", base, "--head", head, "--title", title, "--body", body,
        ],
    )
    .await?;
    // gh prints the PR URL on its own line.
    Ok(out
        .lines()
        .find(|l| l.starts_with("https://"))
        .unwrap_or(out.trim())
        .to_string())
}
