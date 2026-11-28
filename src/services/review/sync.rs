//! Merging a reviewed pull request and bringing the local branch in line
//! afterwards.

use super::*;

/// What the Merge step becomes once "Bring the base in" stopped on conflicts:
/// the merge is half done in the working tree, and the next move is the
/// person's — so the step says which files, and offers to finish or abandon
/// right there instead of leaving the old message and a button that looks
/// like it did nothing. `None` when the output is not a conflict.
pub fn conflict_followup(
    number: &str,
    base: &str,
    output: &str,
    settled: &[String],
) -> Option<StepFailure> {
    let files: Vec<&str> = output
        .lines()
        .filter_map(|l| l.split_once("Merge conflict in ").map(|(_, f)| f.trim()))
        .collect();
    merge_stopped(number, base, &files, settled)
}

/// The same, for a merge git is known to be in the middle of, whatever the
/// last command printed: running "Bring the base in" again during it, or
/// coming back to it after a restart, prints no "Merge conflict in" lines,
/// and left the step offering to start the merge it is already in. `files`
/// are the ones it stopped on, settled or not; `settled` the changelogs
/// GitAgent merged itself, which nobody needs to look at.
///
/// Two steps and a way out, nothing more: which side is which is said on
/// the buttons that pick one, where it is needed, not here ahead of time.
pub fn merge_stopped(
    number: &str,
    base: &str,
    files: &[&str],
    settled: &[String],
) -> Option<StepFailure> {
    if files.is_empty() {
        return None;
    }
    let left: Vec<&str> = files
        .iter()
        .copied()
        .filter(|f| !settled.iter().any(|s| s == f))
        .collect();
    let finish_line = format!(
        "Finish below: it checks the project still builds, commits, pushes and \
         merges #{number} again."
    );
    let settled_line = (!settled.is_empty()).then(|| {
        format!(
            "{} merged for you: every note from both sides is kept, under [Unreleased].",
            settled.join(", ")
        )
    });
    let message = if left.is_empty() {
        format!(
            "#{number} conflicted with {base} only in {}. Nothing has been pushed.\n\n\
             {finish_line}\n\n\
             Or abandon the merge to put the branch back as it was.",
            settled_line.unwrap_or_default()
        )
    } else {
        format!(
            "#{number} conflicts with {base} in {}. Nothing has been pushed.\n\n\
             1. Resolve {} above, with Resolve with AI or by picking a side:\n{}\n\
             2. {finish_line}\n\n\
             {}Or abandon the merge to put the branch back as it was.",
            if left.len() == 1 {
                "1 file".to_string()
            } else {
                format!("{} files", left.len())
            },
            if left.len() == 1 { "it" } else { "each" },
            left.iter()
                .map(|f| format!("  {f}"))
                .collect::<Vec<_>>()
                .join("\n"),
            settled_line.map(|l| format!("{l}\n\n")).unwrap_or_default(),
        )
    };
    let script = format!(
        r#"left=$(git diff --name-only --diff-filter=U | while IFS= read -r f; do grep -l '^<<<<<<< ' "$f"; done)
if [ -n "$left" ]; then printf 'Still has conflict markers, fix it first:\n%s\n' "$left"; exit 1; fi
git diff --name-only --diff-filter=U | while IFS= read -r f; do git add -- "$f"; done
{KEEPS_THE_CHANGE}
{STILL_BUILDS}
git commit --no-edit && git push || exit 1
{WAIT_FOR_FORGE}"#
    );
    let finish = Remedy::new(
        &format!("Finish the merge, push, and merge #{number} again"),
        "sh",
        &["-c", &script, "sh", number, base],
    );
    let finish = Remedy {
        display: format!(
            "cargo check / go build, then git add <fixed files> && git commit --no-edit && git push   (then waits for GitHub to recheck #{number})"
        ),
        ..finish
    }
    .once_resolved();
    Some(StepFailure {
        message,
        remedies: vec![
            finish,
            Remedy::terminal(
                "Abandon the merge — put the branch back as it was",
                "git",
                &["merge", "--abort"],
            ),
        ],
    })
}

/// The fix a Finish that found nothing left to merge offers (`NOTHING_LEFT`):
/// close the pull request as superseded, then abandon the merge and go back
/// to `base`. The pull request is closed first, so a close that fails (signed
/// out, no rights) leaves the merge as it was, to Finish or abandon instead.
/// Its branch is kept. `None` on a forge GitAgent cannot close pull requests
/// on.
pub async fn close_superseded(
    repo: &str,
    forge: &Forge,
    number: &str,
    base: &str,
) -> Option<Remedy> {
    let (program, args) = match forge {
        Forge::GitHub => {
            let comment = format!("Closing as superseded: {base} already has this change.");
            (
                "gh",
                vec![
                    "pr".into(),
                    "close".into(),
                    number.into(),
                    "--comment".into(),
                    comment,
                ],
            )
        }
        Forge::AzureDevOps => {
            let scope = forge::azure_scope(repo, Scope::Organization).await;
            close_command(forge, &scope, number, false)?
        }
        _ => return None,
    };
    Some(superseded_remedy(number, base, program, &args))
}

/// `close_superseded` once the close command is known: `sh` takes the base
/// and then the close command as its arguments, so nothing is quoted by hand.
pub(super) fn superseded_remedy(
    number: &str,
    base: &str,
    program: &str,
    args: &[String],
) -> Remedy {
    const SCRIPT: &str = r#"base=$1; shift
"$@" || exit 1
git merge --abort && git checkout "$base" || exit 1
git merge --ff-only "origin/$base" >/dev/null 2>&1 || true"#;
    let mut sh_args: Vec<&str> = vec!["-c", SCRIPT, "sh", base, program];
    sh_args.extend(args.iter().map(String::as_str));
    let verb = if program == "az" { "Abandon" } else { "Close" };
    Remedy {
        display: format!(
            "{program} {}   then git merge --abort && git checkout {base}",
            args.join(" ")
        ),
        ..Remedy::terminal(
            &format!("{verb} #{number} as superseded, abandon this merge and go back to {base}"),
            "sh",
            &sh_args,
        )
    }
}

/// "not mergeable: the merge commit cannot be cleanly created" means a real
/// conflict against the base branch. Nothing here can resolve a content
/// conflict for a person, but it can take them to the point where only that
/// is left — and say, right where the failure is, every step after it.
///
/// "not mergeable: the base branch policy prohibits the merge" is nothing of
/// the kind: the branch merges cleanly, and GitHub is holding it for checks
/// still running or a review still missing. It used to be read as a
/// conflict, which offered to merge the base in — a push that restarts the
/// very checks being waited on — or to close the pull request.
pub(super) fn merge_failure(
    number: &str,
    base: &str,
    message: String,
    auto_merge: bool,
) -> StepFailure {
    if is_policy_hold(&message) {
        let hold = Hold {
            auto_merge,
            ..Hold::default()
        };
        return policy_guide(number, &message, &hold);
    }
    if !message.contains("not mergeable") {
        return StepFailure {
            message,
            remedies: vec![],
        };
    }
    conflict_guide(number, base, &message)
}

pub(super) fn is_policy_hold(message: &str) -> bool {
    message.contains("base branch policy prohibits")
}

/// A required check that already came back red.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct FailedCheck {
    pub name: String,
    pub link: String,
}

impl FailedCheck {
    /// The job, for its log: `…/job/<id>`.
    fn job_id(&self) -> Option<&str> {
        let id = self
            .link
            .split("/job/")
            .nth(1)?
            .split(['/', '?', '#'])
            .next()?;
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
    }

    /// The Actions run it belongs to, for `gh run rerun`: `…/actions/runs/<id>/job/…`.
    fn run_id(&self) -> Option<&str> {
        let rest = self.link.split("/actions/runs/").nth(1)?;
        let id = rest.split('/').next()?;
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
    }
}

/// What is known about a pull request GitHub is holding, read once so the
/// fixes offered fit it: watching checks that already failed only ends at
/// once, with gh's table and nothing that moves the merge on.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Hold {
    /// The repository lets GitHub merge it by itself once the rules are met.
    pub auto_merge: bool,
    /// The person may merge past the rules (`--admin`).
    pub admin: bool,
    /// Checks that already failed. Empty while they are still running.
    pub failed: Vec<FailedCheck>,
    /// The error in the code the first failed check stopped on (a compiler
    /// or lint error, a failed test), from its log. Running it again cannot
    /// pass, so no rerun is offered then.
    pub error: Option<String>,
}

impl Hold {
    pub(super) async fn read(repo: &str, number: &str) -> Self {
        // Asked apart: `gh repo view` has no field for auto-merge, and one
        // unknown field fails the whole call, which loses your role with it.
        // `{owner}/{repo}` is for gh to fill in from the repository.
        let auto_merge = git::run(
            repo,
            "gh",
            &["api", "repos/{owner}/{repo}", "--jq", ".allow_auto_merge"],
        )
        .await
        .is_ok_and(|out| out.trim() == "true");
        let admin = git::run(
            repo,
            "gh",
            &[
                "repo",
                "view",
                "--json",
                "viewerPermission",
                "-q",
                ".viewerPermission",
            ],
        )
        .await
        .is_ok_and(|out| out.trim() == "ADMIN");
        // `gh pr checks` exits non-zero when one failed, and git::run keeps
        // only stderr then: `; true` keeps its JSON on stdout.
        let checks = git::run(
            repo,
            "sh",
            &[
                "-c",
                r#"gh pr checks "$1" --required --json name,bucket,link; true"#,
                "sh",
                number,
            ],
        )
        .await
        .unwrap_or_default();
        let failed = failed_checks(&checks);
        let mut error = None;
        if let Some(job) = failed.iter().find_map(FailedCheck::job_id) {
            if let Ok(log) = git::job_log(repo, job).await {
                error = crate::services::checkhint::code_error(&log);
            }
        }
        Hold {
            auto_merge,
            admin,
            failed,
            error,
        }
    }
}

/// The checks `gh pr checks --json name,bucket,link` reports as failed or cancelled.
pub(super) fn failed_checks(json: &str) -> Vec<FailedCheck> {
    let start = json.find('[').unwrap_or(json.len());
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&json[start..]) else {
        return vec![];
    };
    items
        .iter()
        .filter(|c| matches!(c["bucket"].as_str(), Some("fail" | "cancel")))
        .map(|c| FailedCheck {
            name: c["name"].as_str().unwrap_or("").to_string(),
            link: c["link"].as_str().unwrap_or("").to_string(),
        })
        .collect()
}

/// A pull request GitHub will not merge yet because the branch's rules are
/// not met — required checks still running or red, or a required review.
pub(super) fn policy_guide(number: &str, message: &str, hold: &Hold) -> StepFailure {
    if !hold.failed.is_empty() {
        return failed_checks_guide(number, message, hold);
    }
    // GitAgent never sits waiting for checks: GitHub merges it by itself
    // when the repository allows that, otherwise the merge is run again
    // once they are green.
    let guide = format!(
        "#{number} can\u{2019}t merge yet: its required checks are still running. It has \
         no conflicts, and nothing has been merged or changed.\n\n\
         {next}\n\n\
         If it is waiting on a review instead, that review is the way forward.\n\n\
         Not expecting this? The base branch has a protection rule or ruleset that \
         requires checks (GitHub \u{2192} Settings \u{2192} Branches / Rules). Remove that \
         requirement there to merge without them.\n\n{message}",
        next = if hold.auto_merge {
            "Merge automatically (below) hands it to GitHub, which merges it the moment \
             the checks pass. Nothing here waits for it."
        } else if hold.admin {
            "Run Review \u{2192} Merge again once the checks are green. Or Allow \
             auto-merge (below): it turns that on for the repository and hands this pull \
             request to GitHub, which merges it the moment the checks pass."
        } else {
            "Run Review \u{2192} Merge again once the checks are green. To have GitHub \
             merge it by itself instead, ask an admin to turn on Allow auto-merge in the \
             repository\u{2019}s Settings \u{2192} General."
        },
    );
    let mut remedies = allow_auto_merge(number, hold);
    if hold.auto_merge {
        remedies.push(auto_merge(number));
    }
    if hold.admin {
        remedies.push(admin_merge(number));
    }
    StepFailure {
        message: guide,
        remedies,
    }
}

/// Required checks already failed: waiting would end at once and change nothing.
fn failed_checks_guide(number: &str, message: &str, hold: &Hold) -> StepFailure {
    let names: Vec<&str> = hold.failed.iter().map(|c| c.name.as_str()).collect();
    let links: Vec<String> = hold
        .failed
        .iter()
        .filter(|c| !c.link.is_empty())
        .map(|c| format!("- {}: {}", c.name, c.link))
        .collect();
    let what = format!(
        "{} required check{} failed ({})",
        names.len(),
        if names.len() == 1 { "" } else { "s" },
        names.join(", "),
    );
    let admin_line = if hold.admin {
        " Or, if you are sure, merge it anyway: as an admin you can skip the checks."
    } else {
        ""
    };
    let guide = match &hold.error {
        // The code is at fault: a rerun gives the same answer every time.
        Some(error) => format!(
            "#{number} can\u{2019}t merge: {what}, because of an error in the code:\n\n  \
             {error}\n\n\
             Running the checks again won\u{2019}t help: they will stop on the same line. \
             Fix it in a commit on #{number}\u{2019}s branch (asking Claude to fix it works), \
             push, then run Review \u{2192} Merge again. Nothing has been merged or \
             changed.{admin_line}\n\n{}\n\n{message}",
            links.join("\n"),
        ),
        None => format!(
            "#{number} can\u{2019}t merge: {what}. It has no conflicts, and nothing has been \
             merged or changed.\n\n\
             The logs show no error in the code, so it looks like CI itself (a timeout, a \
             runner problem, a flaky test). Run the failed checks again (below): it reruns \
             only those and returns at once; nothing here waits. Merge again once \
             they are green. If they fail again, open the link to see why.{admin_line}\n\n{}\n\n{message}",
            links.join("\n"),
        ),
    };
    let mut runs: Vec<&str> = hold.failed.iter().filter_map(FailedCheck::run_id).collect();
    runs.sort_unstable();
    runs.dedup();
    let mut remedies = vec![];
    if !runs.is_empty() && hold.error.is_none() {
        // Reruns, and nothing waits on them: GitHub merges once they pass
        // where auto-merge is allowed; otherwise the merge is run again.
        // Run ids and the number travel as arguments, never spliced in.
        let auto = if hold.auto_merge {
            "\ngh pr merge \"$pr\" --auto --squash --delete-branch || exit 1"
        } else {
            ""
        };
        let script = format!(
            r#"pr="$1"; shift
for run in "$@"; do gh run rerun "$run" --failed || exit 1; done{auto}"#
        );
        let mut args = vec!["-c", script.as_str(), "sh", number];
        args.extend(runs.iter().copied());
        let label = if hold.auto_merge {
            format!("Run the failed checks again; GitHub merges #{number} once they pass")
        } else {
            "Run the failed checks again".to_string()
        };
        let rerun = Remedy::completes(&label, "sh", &args);
        remedies.push(Remedy {
            display: format!("gh run rerun {} --failed", runs.join(" ")),
            ..rerun
        });
    }
    if hold.admin {
        remedies.push(admin_merge(number));
    }
    StepFailure {
        message: guide,
        remedies,
    }
}

/// Required checks still running: the normal state of a fresh pull request,
/// not a failure. The run stops green. The merge was already approved, so
/// where the repository allows it the merge goes to GitHub (`--auto`) at
/// once, rather than asking a second time on a button: GitHub merges it the
/// moment the checks pass, and nothing here waits. Where it doesn't, an
/// admin is offered the one click that allows it and hands the merge over.
pub(super) async fn checks_pending(repo: &str, number: &str, hold: &Hold) -> StepOutcome {
    if hold.auto_merge {
        let args = auto_merge(number).args;
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        if git::run(repo, "gh", &args).await.is_ok() {
            git::forget_prs(repo);
            return StepOutcome::nothing(format!(
                "#{number}\u{2019}s checks are still running: GitHub will merge it the moment they pass"
            ));
        }
        return StepOutcome {
            remedies: vec![auto_merge(number)],
            ..StepOutcome::nothing(format!(
                "#{number}\u{2019}s checks are still running. Merge automatically (below) \
                 hands it to GitHub, which merges it the moment they pass, or run \
                 Review \u{2192} Merge again once they are green"
            ))
        };
    }
    StepOutcome {
        remedies: allow_auto_merge(number, hold),
        ..StepOutcome::nothing(pending_without_auto_merge(number, hold))
    }
}

/// What to do about running checks in a repository that doesn't let GitHub
/// merge by itself.
fn pending_without_auto_merge(number: &str, hold: &Hold) -> String {
    if hold.admin {
        format!(
            "#{number}\u{2019}s checks are still running, and this repository doesn\u{2019}t \
             let GitHub merge pull requests by itself yet. Allow auto-merge (below) turns \
             that on for the repository and hands #{number} to GitHub, which merges it the \
             moment they pass; every merge after that goes the same way on its own. Or run \
             Review \u{2192} Merge again once they are green"
        )
    } else {
        format!(
            "#{number}\u{2019}s checks are still running. Run Review \u{2192} Merge again once \
             they pass. To have GitHub merge by itself instead, ask an admin of the \
             repository to turn on Allow auto-merge in its Settings \u{2192} General"
        )
    }
}

/// Turns on Allow auto-merge for the repository and hands `number` to GitHub,
/// for someone allowed to change the repository's settings. Nothing for
/// anyone else: the setting is an admin's to change.
fn allow_auto_merge(number: &str, hold: &Hold) -> Vec<Remedy> {
    if hold.auto_merge || !hold.admin {
        return vec![];
    }
    // `{owner}/{repo}` is gh's to fill in; the number travels as an argument.
    let script = r#"gh api -X PATCH "repos/{owner}/{repo}" -F allow_auto_merge=true >/dev/null \
&& gh pr merge "$1" --auto --squash --delete-branch"#;
    let remedy = Remedy::completes(
        &format!("Allow auto-merge on this repository, and merge #{number} once it passes"),
        "sh",
        &["-c", script, "sh", number],
    );
    vec![Remedy {
        display: format!(
            "gh api -X PATCH repos/{{owner}}/{{repo}} -F allow_auto_merge=true \
             && gh pr merge {number} --auto --squash --delete-branch"
        ),
        ..remedy
    }]
}

/// The merge, done another way: GitHub finishes it once the rules are met.
fn auto_merge(number: &str) -> Remedy {
    Remedy::completes(
        &format!("Merge #{number} automatically once it passes"),
        "gh",
        &[
            "pr",
            "merge",
            number,
            "--auto",
            "--squash",
            "--delete-branch",
        ],
    )
}

/// The merge past the branch's rules, for someone allowed to.
fn admin_merge(number: &str) -> Remedy {
    Remedy::completes(
        &format!("Merge #{number} anyway, skipping the required checks"),
        "gh",
        &[
            "pr",
            "merge",
            number,
            "--admin",
            "--squash",
            "--delete-branch",
        ],
    )
}

/// A pull request that conflicts with its base, and the ways out of it.
pub(super) fn conflict_guide(number: &str, base: &str, message: &str) -> StepFailure {
    let guide = format!(
        "#{number} conflicts with {base}: GitHub cannot merge it as it stands. Nothing \
         has been merged or changed.\n\n\
         To keep the change, Bring {base} in (below). It switches to #{number}'s branch \
         and merges the latest {base} into it.\n\
         - If that merges cleanly, it pushes the branch, waits for GitHub to recheck \
           #{number}, and tries the merge again.\n\
         - If both sides changed the same lines, git stops and marks them in the files \
           with <<<<<<< and >>>>>>>, and nothing is pushed. They open here to resolve: \
           Resolve with AI proposes a merge of each for you to check, or pick sides \
           yourself. Then Finish the merge: it commits, pushes, and merges again.\n\n\
         Otherwise, close #{number} without merging.\n\n{message}"
    );
    // Positional arguments rather than text spliced into the script: a branch
    // name may legally contain `;`, `$` or `&`.
    //
    // GitHub works out mergeability after a push, not during it, and for a few
    // seconds can still report the old conflict — merging again straight away
    // would fail on stale news. So the script waits for a fresh answer.
    let script = format!(
        r#"gh pr checkout "$1" && git fetch origin "$2" && git merge --no-edit "origin/$2" && git push || exit 1
{WAIT_FOR_FORGE}"#
    );
    let bring_in = Remedy::new(
        &format!("Bring {base} in — merge it into #{number}'s branch, push, and merge again"),
        "sh",
        &["-c", &script, "sh", number, base],
    );
    let bring_in = Remedy {
        display: format!(
            "gh pr checkout {number} && git fetch origin {base} && git merge origin/{base} \
             && git push   (then waits for GitHub to recheck #{number})"
        ),
        ..bring_in
    };
    StepFailure {
        message: guide,
        remedies: vec![
            bring_in,
            Remedy::terminal(
                &format!("Abandon — close #{number} without merging"),
                "gh",
                &[
                    "pr",
                    "close",
                    number,
                    "--comment",
                    "Closing — conflicts with the base branch and this run is being abandoned rather than resolved.",
                ],
            ),
        ],
    }
}

/// The same for Azure DevOps: there is no `gh pr checkout`, so the branch is
/// checked out by name, brought up to date with origin first (never reset:
/// local commits not pushed yet stop it instead of being lost), and Abandon
/// abandons the pull request, as Azure calls closing one. `az_scope` is what
/// `forge::azure_scope` gave for `update`.
pub(super) fn azure_conflict_guide(
    number: &str,
    base: &str,
    head: &str,
    az_scope: &[String],
    message: &str,
) -> StepFailure {
    let guide = format!(
        "#{number} conflicts with {base}: Azure DevOps cannot merge it as it stands. \
         Nothing has been merged or changed.\n\n\
         To keep the change, Bring {base} in (below). It switches to {head} and merges \
         the latest {base} into it.\n\
         - If that merges cleanly, it pushes the branch, waits for Azure DevOps to \
           recheck #{number}, and tries the merge again.\n\
         - If both sides changed the same lines, git stops and marks them in the files \
           with <<<<<<< and >>>>>>>, and nothing is pushed. They open here to resolve: \
           Resolve with AI proposes a merge of each for you to check, or pick sides \
           yourself. Then Finish the merge: it commits, pushes, and merges again.\n\n\
         Otherwise, abandon #{number} without merging.\n\n{message}"
    );
    let script = format!(
        r#"git fetch origin "$2" "$3" && git checkout "$3" && git merge --ff-only "origin/$3" && git merge --no-edit "origin/$2" && git push origin "HEAD:refs/heads/$3" || exit 1
{WAIT_FOR_FORGE}"#
    );
    let bring_in = Remedy::new(
        &format!("Bring {base} in — merge it into {head}, push, and merge again"),
        "sh",
        &["-c", &script, "sh", number, base, head],
    );
    let bring_in = Remedy {
        display: format!(
            "git fetch origin {base} {head} && git checkout {head} && git merge --ff-only \
             origin/{head} && git merge origin/{base} && git push   (then waits for Azure \
             DevOps to recheck #{number})"
        ),
        ..bring_in
    };
    let mut remedies = vec![bring_in];
    if let Some((program, args)) = close_command(&Forge::AzureDevOps, az_scope, number, false) {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        remedies.push(Remedy::terminal(
            &format!("Abandon — close #{number} without merging"),
            program,
            &args,
        ));
    }
    StepFailure {
        message: guide,
        remedies,
    }
}

/// What Azure DevOps made of `az repos pr update --status completed`. The
/// command succeeds whether or not the pull request was completed: with
/// conflicts, or a policy in the way, Azure leaves it active and says
/// nothing, which used to be reported as "merged #N" and sent the run back
/// to the base with the pull request still open. A merge Azure is still
/// working on is given a few seconds before it counts as not done.
async fn azure_completed(
    repo: &str,
    number: &str,
    base: &str,
    head: &str,
) -> Result<(), StepFailure> {
    let mut last = serde_json::Value::Null;
    for attempt in 0..6 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let Ok(pr) = azure_pr_now(repo, number).await else {
            continue;
        };
        match azure_completion(&pr) {
            AzureCompletion::Done => return Ok(()),
            AzureCompletion::Conflicts => {
                return Err(azure_conflicts(repo, number, base, head).await)
            }
            AzureCompletion::Waiting => last = pr,
            AzureCompletion::Refused => {
                last = pr;
                break;
            }
        }
    }
    Err(azure_not_completed(number, &last))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum AzureCompletion {
    Done,
    Conflicts,
    /// Still active, and Azure is still merging or about to.
    Waiting,
    /// Still active, and Azure has said why not.
    Refused,
}

pub(super) fn azure_completion(pr: &serde_json::Value) -> AzureCompletion {
    let status = pr["status"].as_str().unwrap_or_default();
    if status.eq_ignore_ascii_case("completed") {
        return AzureCompletion::Done;
    }
    let merge = azure_merge_status(pr);
    if merge.eq_ignore_ascii_case("conflicts") {
        return AzureCompletion::Conflicts;
    }
    let failed = !pr["mergeFailureMessage"]
        .as_str()
        .unwrap_or_default()
        .is_empty()
        || matches!(
            merge.to_ascii_lowercase().as_str(),
            "failure" | "rejectedbypolicy"
        )
        || status.eq_ignore_ascii_case("abandoned");
    if failed {
        AzureCompletion::Refused
    } else {
        AzureCompletion::Waiting
    }
}

/// The failure for a pull request Azure left open after being asked to
/// complete it, with what it said, if anything.
pub(super) fn azure_not_completed(number: &str, pr: &serde_json::Value) -> StepFailure {
    let status = pr["status"].as_str().unwrap_or("unknown");
    let merge = azure_merge_status(pr);
    let why = pr["mergeFailureMessage"].as_str().unwrap_or_default();
    let mut message = format!(
        "Azure DevOps did not complete #{number}: it is still {status}, so nothing was merged.\n\n\
         Usually a branch policy is in the way (required reviewers, a build that has not \
         passed, linked work items, comments to resolve). Open #{number} in the browser to \
         see which, then run Review → Merge again.\n\n\
         status:       {status}\nmerge status: {}",
        if merge.is_empty() { "unknown" } else { &merge }
    );
    if !why.is_empty() {
        message.push_str(&format!("\nAzure says:   {why}"));
    }
    StepFailure::from(message)
}

pub(super) async fn merge(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let forge = Forge::from_key(state.artifact("forge"));
    let number = state.artifact("pr_number").to_string();
    // A merge of the base still open here — "Bring the base in" stopped on
    // conflicts — is what the step is waiting on, not edits to commit: run
    // again during it, or after a restart, it must offer to finish it, not
    // report its staged files as uncommitted with no way forward.
    if matches!(forge, Forge::GitHub | Forge::AzureDevOps) {
        if let Some(open) = merge_in_progress(repo, &number, state.artifact("pr_base")).await {
            return Err(open);
        }
    }
    if let Some(refusal) = uncommitted_in_the_way(repo, &number).await {
        return Err(refusal);
    }
    if forge == Forge::GitHub {
        let (base, head) = (state.artifact("pr_base"), state.artifact("pr_head"));
        if let Some(stop) = misfiled_notes(repo, &number, base, head).await {
            return Err(stop);
        }
    }

    let out = match forge {
        Forge::GitHub => match git::run(
            repo,
            "gh",
            &["pr", "merge", &number, "--squash", "--delete-branch"],
        )
        .await
        .inspect(|_| git::forget_prs(repo))
        {
            Ok(out) => out,
            Err(e) => match merged_anyway(repo, &number, state.artifact("pr_head"), &e).await {
                Some(out) => out,
                None => {
                    if is_policy_hold(&e) {
                        let hold = Hold::read(repo, &number).await;
                        if hold.failed.is_empty() {
                            return Ok(checks_pending(repo, &number, &hold).await);
                        }
                        return Err(policy_guide(&number, &e, &hold));
                    }
                    return Err(merge_failure(&number, state.artifact("pr_base"), e, false));
                }
            },
        },
        Forge::AzureDevOps => {
            let (base, head) = (state.artifact("pr_base"), state.artifact("pr_head"));
            // Asked again rather than taken from the checks step: "Bring the
            // base in" retries this step alone, after a push that changed it.
            if let Ok(pr) = azure_pr_now(repo, &number).await {
                if azure_merge_status(&pr).eq_ignore_ascii_case("conflicts") {
                    return Err(azure_conflicts(repo, &number, base, head).await);
                }
            }
            let scope = forge::azure_scope(repo, Scope::Organization).await;
            let mut args = vec![
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
            ];
            args.extend(scope.iter().map(String::as_str));
            let out = git::run(repo, "az", &args).await?;
            git::forget_prs(repo);
            azure_completed(repo, &number, base, head).await?;
            out
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
        remedies: vec![],
    })
}

/// The Finish / Abandon failure for a merge git is in the middle of, with the
/// files it stopped on, settled or not. `None` when no merge is open, or it
/// stopped on nothing.
pub(super) async fn merge_in_progress(repo: &str, number: &str, base: &str) -> Option<StepFailure> {
    if git::in_progress(repo).await != Some(git::InProgress::Merge) {
        return None;
    }
    let settled = crate::services::conflicts::changelog::settle_all(repo).await;
    let mut files: Vec<String> = crate::services::conflicts::list(repo)
        .await
        .into_iter()
        .map(|c| c.path)
        .collect();
    files.extend(crate::services::conflicts::resolved(repo).await);
    files.sort();
    files.dedup();
    let files: Vec<&str> = files.iter().map(String::as_str).collect();
    merge_stopped(number, base, &files, &settled)
}

/// Release notes the merge would file under the wrong version: `base`
/// released `[Unreleased]` after `head` branched, and git, merging without
/// a clash, puts the pull request's notes under that release. `None` when
/// they would land under `[Unreleased]`, when the merge clashes (that has
/// its own way through), or when anything here cannot be read — the merge
/// then goes ahead as it always did.
async fn misfiled_notes(repo: &str, number: &str, base: &str, head: &str) -> Option<StepFailure> {
    use crate::services::conflicts::changelog;
    if base.is_empty() || head.is_empty() {
        return None;
    }
    let _ = git::fetch(repo, &["origin", base, head]).await;
    let (theirs_ref, ours_ref) = (format!("origin/{base}"), format!("origin/{head}"));
    let rev = |args: Vec<String>| async move {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        git::run(repo, "git", &args)
            .await
            .ok()
            .map(|s| s.trim().to_string())
    };
    let at = rev(vec!["rev-parse".into(), theirs_ref.clone()]).await?;
    let fork = rev(vec![
        "merge-base".into(),
        theirs_ref.clone(),
        ours_ref.clone(),
    ])
    .await?;
    // What GitHub's merge would make of it, without touching the checkout.
    // A clash exits non-zero, and is left to the merge that reports it.
    let tree = rev(vec![
        "merge-tree".into(),
        "--write-tree".into(),
        theirs_ref.clone(),
        ours_ref.clone(),
    ])
    .await?;
    let tree = tree.lines().next()?.to_string();
    let changed = rev(vec![
        "diff".into(),
        "--name-only".into(),
        fork.clone(),
        ours_ref.clone(),
    ])
    .await?;
    for path in changed.lines().filter(|p| changelog::is_changelog_path(p)) {
        let show = |at: &str| rev(vec!["show".into(), format!("{at}:{path}")]);
        let (Some(old), Some(ours), Some(theirs), Some(merged)) = (
            show(&fork).await,
            show(&ours_ref).await,
            show(&theirs_ref).await,
            show(&tree).await,
        ) else {
            continue;
        };
        let lost = changelog::misfiled(&old, &ours, &theirs, &merged);
        if lost.is_empty() {
            continue;
        }
        let fixed = changelog::merge(&old, &ours, &theirs)?;
        // Through a file, not an argument: a changelog outgrows the longest
        // command line Windows takes.
        let file = crate::services::store::data_dir().join(format!("changelog-fix-{number}.md"));
        std::fs::write(&file, &fixed).ok()?;
        return Some(misfiled_guide(
            number,
            base,
            path,
            &lost,
            &file.to_string_lossy(),
            &at,
        ));
    }
    None
}

/// What stops the merge, and the one click that puts the notes right.
pub(super) fn misfiled_guide(
    number: &str,
    base: &str,
    path: &str,
    lost: &[String],
    fixed: &str,
    at: &str,
) -> StepFailure {
    let message = format!(
        "#{number} would file its release notes under the wrong version. {base} released \
         [Unreleased] after #{number} branched, and merged as it stands, these notes from \
         {path} land under that release, which never shipped them:\n{}\n\n\
         Move them (below): it brings {base} into #{number}'s branch with the notes back \
         under [Unreleased], pushes, and merges again. Nothing has been merged or changed.",
        lost.iter()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    // Positional arguments, never spliced in. `$5` is where the base stood
    // when this was worked out: moved since, the notes are worked out again.
    let script = format!(
        r#"gh pr checkout "$1" || exit 1
git fetch origin "$2" || exit 1
if [ "$(git rev-parse "origin/$2")" != "$5" ]; then echo "$2 moved since this was checked. Run Review → Merge again."; exit 1; fi
git merge --no-edit "origin/$2" || exit 1
cp "$3" "$4" && git add -- "$4" && git commit --amend --no-edit && git push || exit 1
{WAIT_FOR_FORGE}"#
    );
    let fix = Remedy::new(
        &format!("Move the notes back under [Unreleased], push, and merge #{number} again"),
        "sh",
        &["-c", &script, "sh", number, base, fixed, path, at],
    );
    StepFailure {
        message,
        remedies: vec![Remedy {
            display: format!(
                "gh pr checkout {number} && git merge origin/{base} && (notes under [Unreleased] in {path}) && git commit --amend && git push"
            ),
            ..fix
        }],
    }
}

/// Why the merge is refused while tracked files have uncommitted changes, or
/// `None` when nothing is in the way.
///
/// The pull request is merged as it is on the forge, so those edits are not
/// in it — and on its branch they are usually the rest of it, a fix for the
/// review or for CI. Merging first ships it without them, and `gh pr merge`
/// then fails to switch off the branch with them in the way, leaving them on
/// a branch it has just deleted on origin. Untracked files are left out:
/// no commit here takes them, and they do not stop a branch switch.
async fn uncommitted_in_the_way(repo: &str, number: &str) -> Option<StepFailure> {
    let files: Vec<String> = git::status(repo)
        .await
        .ok()?
        .into_iter()
        .filter(|f| !f.is_untracked())
        .map(|f| f.path)
        .collect();
    if files.is_empty() {
        return None;
    }
    let shown: Vec<String> = files.iter().take(10).map(|f| format!("  {f}")).collect();
    let more = match files.len().saturating_sub(shown.len()) {
        0 => String::new(),
        n => format!("\n  … and {n} more"),
    };
    let message = format!(
        "This repository has uncommitted changes, so #{number} was not merged. Nothing \
         has been changed.\n\n{}{more}\n\n\
         Merging now would merge #{number} without them, and leave them stranded on a \
         branch that is being deleted. Run Commit → PR first: on #{number}'s \
         branch it adds them to the pull request, elsewhere it opens one of their own. \
         Then Review → Merge again.",
        shown.join("\n"),
    );
    Some(StepFailure {
        message,
        remedies: vec![Remedy::flow(
            &format!("Commit them with Commit → PR, then review and merge #{number}"),
            "Commit → PR",
            crate::services::probe::Need::Uncommitted,
        )],
    })
}

/// `gh pr merge --delete-branch` merges on GitHub first, then tidies up here:
/// switches off the branch, deletes it locally, then on origin. When that
/// tidying up fails — uncommitted edits in the way of the switch, usually —
/// gh exits with an error although the pull request is merged. The step asks
/// GitHub which it was, so a merge that happened is reported as one, and
/// finishes the one bit of tidying that matters off this machine: the
/// branch on origin, which the person approved deleting. The local side is
/// the Sync step's.
async fn merged_anyway(repo: &str, number: &str, head: &str, err: &str) -> Option<String> {
    let state = git::run(
        repo,
        "gh",
        &["pr", "view", number, "--json", "state", "-q", ".state"],
    )
    .await
    .ok()?;
    if state.trim() != "MERGED" {
        return None;
    }
    let deleted = if head.is_empty() {
        false
    } else {
        git::run(repo, "git", &["push", "origin", "--delete", head])
            .await
            .is_ok()
    };
    Some(format!(
        "Merged #{number} on GitHub. Only the tidying up on this machine failed \
         afterwards — gh could not switch off the branch here:\n\n{}\n\n{}\
         The Sync step brings the base branch up to date without touching your edits.",
        err.trim(),
        if deleted {
            format!("{head} was deleted on origin. ")
        } else {
            String::new()
        },
    ))
}

pub(super) async fn sync(repo: &str, state: &RunState) -> Result<StepOutcome, StepFailure> {
    let base = state.artifact("pr_base");
    if let Some(there) = git::worktree_holding(repo, base).await {
        return sync_elsewhere(repo, base, &there).await;
    }
    // Work in progress is never touched to bring the base up to date: the
    // merge happened on the forge, and this step is a convenience. Checking
    // out or fast-forwarding over uncommitted edits is what failed here
    // whenever a review ran while something else was being worked on.
    let edited = git::run(
        repo,
        "git",
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            git::IGNORE_DIRTY_SUBMODULES,
        ],
    )
    .await
    .map(|out| !out.trim().is_empty())
    .unwrap_or(false);
    // Files left in conflict by an earlier set-aside-and-update are not
    // "edits": they need resolving, and the usual path below says which.
    if edited && git::unmerged_paths(repo).await.is_empty() {
        return sync_around_edits(repo, base).await;
    }
    // Already where it needs to be, git is not asked again: after an
    // autostash whose edits clashed, a checkout or merge would refuse over
    // the conflicts even though there is nothing left for either to do.
    let mut log = if git::current_branch(repo).await.ok().as_deref() == Some(base) {
        String::new()
    } else {
        git::run(repo, "git", &["checkout", base]).await?
    };
    // Fetch only the base, then fast-forward, rather than `git pull`: a pull
    // fetches every branch (and prunes, where configured), which widens the
    // window for another git process to move a ref under it — and that race
    // is the one failure here with a fix that is simply "do it again".
    match git::fetch(repo, &["origin", base]).await {
        Ok(out) => log.push_str(&out),
        Err(e) => return Err(sync_fetch_failure(base, e)),
    }
    let target = format!("origin/{base}");
    if !git::same_commit(repo, "HEAD", &target).await {
        match git::run(repo, "git", &["merge", "--ff-only", &target]).await {
            Ok(out) => log.push_str(&out),
            // Local base has commits of its own. GitAgent's release notes are
            // the usual ones, and are settled here; anything else gets a button.
            Err(e) if e.contains("Not possible to fast-forward") => {
                match flow::settle_own_notes(repo, &target).await {
                    Ok(Some(note)) => log.push_str(&note),
                    Ok(None) => return Err(sync_diverged(base, e, "")),
                    Err(why) => return Err(sync_diverged(base, e, &why)),
                }
            }
            Err(e) => return Err(sync_merge_failure(base, e)),
        }
    }
    let clashed = git::unmerged_paths(repo).await;
    if !clashed.is_empty() {
        log = format!("{}\n\n{}", clashes_note(&clashed), log.trim());
    }
    Ok(StepOutcome {
        summary: if clashed.is_empty() {
            format!("on {base}, up to date")
        } else {
            format!(
                "on {base}, up to date; {} to resolve",
                plural_files(clashed.len())
            )
        },
        log: log.trim().to_string(),
        artifacts: vec![("sync_output".into(), log)],
        nothing_to_do: false,
        items: vec![],
        remedies: vec![],
    })
}

/// Brings the base up to date without touching uncommitted edits, and says
/// where that leaves things. Never a failure: the merge is done, and nothing
/// here is in the way of it.
pub(super) async fn sync_around_edits(repo: &str, base: &str) -> Result<StepOutcome, StepFailure> {
    let here = git::current_branch(repo).await.unwrap_or_default();
    let target = format!("origin/{base}");
    if here == base {
        let log = match git::fetch(repo, &["origin", base]).await {
            Ok(out) => out,
            Err(e) => return Err(sync_fetch_failure(base, e)),
        };
        let behind: usize = git::run(
            repo,
            "git",
            &["rev-list", "--count", &format!("HEAD..{target}")],
        )
        .await
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
        let (summary, note) = if behind == 0 {
            (
                format!("on {base}, up to date"),
                format!(
                    "{base} was already up to date; your uncommitted edits are as you left them."
                ),
            )
        } else {
            (
                format!("on {base}, {behind} behind; your edits kept"),
                format!(
                    "You have uncommitted edits on {base}, so it was not updated under them. \
                     It is {behind} commit{} behind {target}: run `git pull --ff-only` once \
                     they are committed or put away.",
                    if behind == 1 { "" } else { "s" }
                ),
            )
        };
        return Ok(StepOutcome {
            summary,
            log: format!("{note}\n\n{}", log.trim()).trim().to_string(),
            artifacts: vec![("sync_output".into(), log)],
            nothing_to_do: false,
            items: vec![],
            remedies: vec![],
        });
    }

    // On another branch, the base can move without being checked out:
    // fetching straight into it fast-forwards the ref and touches no file.
    let into = format!("{base}:{base}");
    let (summary, note, log) = match git::fetch(repo, &["origin", &into]).await {
        Ok(out) => (
            format!("{base} up to date; you stay on {here}"),
            format!(
                "You have uncommitted edits on {here}, so the checkout stayed where it is. \
                 {base} was brought up to date without switching to it."
            ),
            out,
        ),
        // Git refuses only when the local base has commits origin does not —
        // not this step's to resolve.
        Err(e) => {
            let log = match git::fetch(repo, &["origin", base]).await {
                Ok(out) => out,
                Err(e) => return Err(sync_fetch_failure(base, e)),
            };
            (
                format!("fetched {base}; local {base} left as is"),
                format!(
                    "You have uncommitted edits on {here}, so the checkout stayed where it \
                     is. Your local {base} has commits {target} does not, so it was not \
                     moved either:\n\n{e}"
                ),
                log,
            )
        }
    };
    Ok(StepOutcome {
        summary,
        log: format!("{note}\n\n{}", log.trim()).trim().to_string(),
        artifacts: vec![("sync_output".into(), log)],
        nothing_to_do: false,
        items: vec![],
        remedies: vec![],
    })
}

/// Run from a second worktree whose base branch is checked out in another
/// one. Git refuses to check a branch out twice, and there is nothing the
/// person could do about that here short of removing a worktree. So the base
/// is brought up to date where it lives, and this worktree is left on its
/// branch — which is merged now, and theirs to remove when they are done.
pub(super) async fn sync_elsewhere(
    repo: &str,
    base: &str,
    there: &str,
) -> Result<StepOutcome, StepFailure> {
    let mut log = match git::fetch(repo, &["origin", base]).await {
        Ok(out) => out,
        Err(e) => return Err(sync_fetch_failure(base, e)),
    };
    let stays = git::current_branch(repo).await.unwrap_or_default();
    let (summary, note) = match git::run(
        there,
        "git",
        &["merge", "--ff-only", &format!("origin/{base}")],
    )
    .await
    {
        Ok(out) => {
            log.push_str(&out);
            (
                format!("{base} up to date in {there}"),
                format!(
                    "{base} is checked out in {there}, so it was updated there. This \
                     worktree stays on {stays}."
                ),
            )
        }
        // Not a failure of this run: the merge is done and origin/{base} is
        // fetched. Git refused to move a working tree with changes in the way,
        // which is exactly what it should do, and pulling there later finishes it.
        Err(e) => (
            format!("fetched {base}; {there} not updated"),
            format!(
                "{base} is checked out in {there}, and git would not fast-forward it \
                 there:\n\n{e}\n\nThe merge is done. Run `git pull` in {there} once \
                 its changes are committed or put away. This worktree stays on {stays}."
            ),
        ),
    };
    Ok(StepOutcome {
        summary,
        log: format!("{note}\n\n{}", log.trim()).trim().to_string(),
        artifacts: vec![("sync_output".into(), log)],
        nothing_to_do: false,
        items: vec![],
        remedies: vec![],
    })
}

/// The fetch has already been retried once by `git::fetch`. If it is still
/// another process moving the ref, say so — the raw git message reads like
/// corruption, and it is not — and put the retry on a button.
pub(super) fn sync_fetch_failure(base: &str, err: String) -> StepFailure {
    if !git::ref_moved_underneath(&err) {
        return StepFailure::from(err);
    }
    StepFailure {
        message: format!(
            "Another git program updated origin/{base} at the same moment GitAgent \
             did — an editor's background fetch, usually. Nothing is wrong with the \
             repository and the merge is done; fetching again picks up where it left \
             off. If this keeps happening, close whatever else is fetching here.\n\n{err}"
        ),
        remedies: vec![Remedy::new(
            "Fetch again and finish",
            "git",
            &["fetch", "origin", base],
        )],
    }
}

/// The pull request is merged and this copy is on the base; only the
/// fast-forward is left. When uncommitted edits are what stopped it, say so
/// and offer to move them out of the way and back.
/// The merge is done; only this copy's base is out of step — it has commits
/// origin's does not. Replaying them on top is the usual answer, offered here
/// rather than left to a terminal.
pub(super) fn sync_diverged(base: &str, err: String, why: &str) -> StepFailure {
    let target = format!("origin/{base}");
    StepFailure {
        message: format!(
            "The merge is done, but your local {base} has commits {target} does not, so it \
             could not simply be moved forward. Replay them on top of {target} below, then \
             push them when you are ready.{}\n\n{err}",
            if why.is_empty() {
                String::new()
            } else {
                format!("\n\n{why}")
            }
        ),
        remedies: vec![Remedy::new(
            &format!("Replay your local commits on top of {target}"),
            "git",
            &["rebase", &target],
        )],
    }
}

pub(super) fn sync_merge_failure(base: &str, err: String) -> StepFailure {
    if !git::local_changes_in_the_way(&err) {
        return StepFailure::from(err);
    }
    StepFailure {
        message: format!(
            "The merge is done, but {base} could not be brought up to date: you have \
             uncommitted edits to files that changed on origin/{base}. Nothing was \
             touched. Set them aside and put them back after updating — if they clash \
             with what came in, both versions are left marked in the file for you to \
             pick from, a copy stays in the stash, and this step still finishes.\n\n{err}"
        ),
        remedies: vec![flow::autostash_remedy(&format!("origin/{base}"))],
    }
}
