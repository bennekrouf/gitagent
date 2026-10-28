//! What runs before a commit can go anywhere: the repository's tests and
//! scripts, catching up with the remote, and the preflight.

use super::*;

/// Runs an arbitrary command in the repository.
///
/// `sh -c` rather than an argument array, so what you type in Setup behaves the
/// way it does in a terminal — pipes, flags, `&&` and all.
///
/// stdin matters more than it looks: a subprocess has no terminal, so a script
/// that asks `Proceed? [y/N]` reads EOF and aborts. The `stdin` setting answers
/// it. That is not a way around the confirmation — GitAgent already asked at
/// the approval step, and showed the exact command it was asking about.
/// Runs a command on another machine. See `services::remote` for why no key
/// ever reaches this app.
pub(super) async fn run_remote(
    node: &NodeSpec,
    on_line: &mut dyn FnMut(&str),
) -> Result<StepOutcome, StepFailure> {
    let host = node.setting("host").trim().to_string();
    let command = node.setting("command").trim().to_string();
    if host.is_empty() || command.is_empty() {
        return Err(StepFailure::from(
            "This step needs a host and a command. Set them in Setup.",
        ));
    }

    let (ok, output) = remote::run_streaming(
        &host,
        node.setting("port"),
        node.setting("identity"),
        &command,
        node.setting("stdin"),
        on_line,
    )
    .await;

    if !ok {
        return Err(StepFailure::from(format!(
            "`{command}` failed on {host}.\n\n{output}"
        )));
    }

    Ok(StepOutcome {
        summary: format!("{host} — ok"),
        log: if output.trim().is_empty() {
            format!("{host}: {command}\n\n(no output)")
        } else {
            output.clone()
        },
        artifacts: vec![
            (format!("{}_output", node.id), output),
            (format!("{}_exit", node.id), "0".to_string()),
        ],
        nothing_to_do: false,
        items: vec![],
    })
}

/// Runs the project's test suite, and stops the flow if it fails.
///
/// The command is optional: left empty, it is detected from the repository.
/// That is what lets the step ship in the default commit flow — a required
/// setting would make the shipped flow arrive invalid.
///
/// A repository with no suite this recognises finishes `Done`, not `Skipped`.
/// The difference is load-bearing: `Skipped` blocks everything downstream, and
/// "this project has no tests" must not be the thing that stops you committing.
pub(super) async fn run_tests(
    node: &NodeSpec,
    repo: &str,
    on_line: &mut dyn FnMut(&str),
) -> Result<StepOutcome, StepFailure> {
    let configured = node.setting("command").trim().to_string();
    let (command, why) = if configured.is_empty() {
        match testsuite::detect(std::path::Path::new(repo)) {
            Some(suite) => (suite.command, format!("detected from {}", suite.why)),
            None => {
                let said = "No test suite detected in this repository, so there was nothing \
                            to run. Set a command in Setup if that is wrong.";
                on_line(said);
                return Ok(StepOutcome {
                    summary: "No test suite found".into(),
                    log: said.into(),
                    artifacts: vec![
                        (format!("{}_output", node.id), String::new()),
                        (format!("{}_exit", node.id), "none".to_string()),
                    ],
                    nothing_to_do: false,
                    items: vec![],
                });
            }
        }
    } else {
        (configured, "set in Setup".to_string())
    };

    on_line(&format!("$ {command}   ({why})"));
    let (ok, output) = git::run_shell_streaming(repo, &command, "", on_line).await;
    if !ok {
        return Err(StepFailure::from(format!(
            "Tests failed.\n\n$ {command}   ({why})\n\n{output}"
        )));
    }

    Ok(StepOutcome {
        summary: format!("{command} — passed"),
        log: if output.trim().is_empty() {
            format!("$ {command}   ({why})\n\n(no output)")
        } else {
            format!("$ {command}   ({why})\n\n{output}")
        },
        artifacts: vec![
            (format!("{}_output", node.id), output),
            (format!("{}_exit", node.id), "0".to_string()),
        ],
        nothing_to_do: false,
        items: vec![],
    })
}

pub(super) async fn run_script(
    node: &NodeSpec,
    repo: &str,
    on_line: &mut dyn FnMut(&str),
) -> Result<StepOutcome, StepFailure> {
    let command = node.setting("command").trim().to_string();
    if command.is_empty() {
        return Err(StepFailure::from(
            "This step has no command. Set one in Setup.",
        ));
    }

    // A release cuts whatever this copy has, so this copy has to be current
    // first — see `catch_up`.
    let release = is_release(&command);
    let caught_up = if release { catch_up(repo).await? } else { None };
    if let Some(note) = &caught_up {
        on_line(note);
    }

    let (ok, output) =
        git::run_shell_streaming(repo, &command, node.setting("stdin"), on_line).await;
    if !ok {
        let found = if release {
            ReleaseFindings {
                upstream: git::upstream(repo).await,
                unpushed_tag: if push_rejected(&output) {
                    git::unpushed_release_tag(repo).await
                } else {
                    None
                },
            }
        } else {
            ReleaseFindings::default()
        };
        return Err(script_failure(&command, output, &found));
    }

    let output = match caught_up {
        Some(note) => format!("{note}\n\n{output}"),
        None => output,
    };
    Ok(StepOutcome {
        summary: format!("{command} — ok"),
        log: if output.trim().is_empty() {
            format!("{command}\n\n(no output)")
        } else {
            output.clone()
        },
        artifacts: vec![
            (format!("{}_output", node.id), output),
            (format!("{}_exit", node.id), "0".to_string()),
        ],
        nothing_to_do: false,
        items: vec![],
    })
}

/// When the only commits this copy has that `upstream` lacks are GitAgent's
/// own release-notes commits — written moments ago, never pushed, with a pull
/// request merged on the forge meanwhile — sorts them out without a person:
///
/// - already on `upstream` (a later pull request carried them): the local copy
///   is dropped, losing nothing;
/// - otherwise: put back on top of `upstream`.
///
/// `Ok(None)` when it is not that case, or work in progress is in the way;
/// `Err` with a sentence for the person when putting them back clashed —
/// undone, nothing changed.
pub(crate) async fn settle_own_notes(repo: &str, upstream: &str) -> Result<Option<String>, String> {
    let range = format!("{upstream}..HEAD");
    let ours = git::run(repo, "git", &["log", "--format=%s", &range])
        .await
        .map(|out| {
            let subjects: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
            !subjects.is_empty()
                && subjects
                    .iter()
                    .all(|s| s.trim() == crate::services::notes::COMMIT_MESSAGE)
        })
        .unwrap_or(false);
    let clean = git::run(
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
    .is_ok_and(|out| out.trim().is_empty());
    if !ours || !clean {
        return Ok(None);
    }

    // Every line those commits add is already in upstream's changelog: the
    // notes went in with a later pull request, and replaying them would only
    // clash with themselves.
    let added = git::run(repo, "git", &["diff", &format!("{upstream}...HEAD")])
        .await
        .unwrap_or_default();
    let theirs = git::run(repo, "git", &["show", &format!("{upstream}:CHANGELOG.md")])
        .await
        .unwrap_or_default();
    let new_lines: Vec<&str> = added
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .map(|l| l[1..].trim_end())
        .filter(|l| !l.trim().is_empty())
        .collect();
    if !new_lines.is_empty()
        && new_lines
            .iter()
            .all(|l| theirs.lines().any(|t| t.trim_end() == *l))
    {
        git::run(repo, "git", &["reset", "--keep", upstream])
            .await
            .map_err(|e| format!("Could not drop the local copy of the release notes: {e}"))?;
        return Ok(Some(format!(
            "The release notes committed here were already on {upstream}, carried by a \
             later pull request, so the local copy was dropped."
        )));
    }

    match git::run(repo, "git", &["rebase", "--quiet", upstream]).await {
        Ok(_) => Ok(Some(format!(
            "{upstream} moved on after the release notes were committed here, so the \
             notes commit was put back on top of it."
        ))),
        Err(e) => {
            let _ = git::run(repo, "git", &["rebase", "--abort"]).await;
            Err(format!(
                "Putting the release notes commit back on top of {upstream} clashed, so \
                 that was undone and nothing changed — most likely both edited \
                 CHANGELOG.md. The button below does the same, and leaves the clash \
                 marked in the file for you to resolve.\n\n{e}"
            ))
        }
    }
}

/// Brings the branch up to date with origin before a release reads it.
///
/// A release only ships what this copy has. A pull request merged on the
/// forge's website never reaches it until something pulls — the review flow's
/// `sync` does, but nothing else — so without this the release saw no new
/// commits and no notes, and offered to ship build-only a version that held a
/// real change.
///
/// A fast-forward is done without asking: it only adds commits origin already
/// has, and loses nothing. Anything else — local changes in the way, or local
/// commits origin lacks — stops the step with the fix on a button, because
/// that one rewrites history and should be seen first.
///
/// `Ok(Some(note))` when it pulled, for the step's log.
pub(crate) async fn catch_up(repo: &str) -> Result<Option<String>, StepFailure> {
    // An autostash whose edits clashed leaves this copy up to date but with
    // conflict markers in files — a release must not read those.
    let clashed = git::unmerged_paths(repo).await;
    if !clashed.is_empty() {
        return Err(StepFailure::from(format!(
            "This copy is up to date, but these files still have both versions \
             marked from your edits clashing with the update:\n\n  {}\n\nKeep what \
             you want in each, `git add` it, then retry this step. Once all are \
             resolved, `git stash drop` removes the copy kept in the stash.",
            clashed.join("\n  ")
        )));
    }
    let git::Upstream::Behind {
        tracking,
        behind,
        ahead,
    } = git::upstream(repo).await
    else {
        return Ok(None);
    };
    let commits = plural(behind, "commit");

    if ahead == 0 {
        return match git::run(repo, "git", &["merge", "--ff-only", "--quiet", "@{u}"]).await {
            Ok(_) => Ok(Some(format!(
                "Pulled {commits} from {tracking} first, so the release includes them."
            ))),
            Err(e) => Err(StepFailure {
                remedies: if git::local_changes_in_the_way(&e) {
                    vec![autostash_remedy("@{u}")]
                } else {
                    vec![]
                },
                message: format!(
                    "{tracking} has {commits} this copy does not, and local changes are \
                     in the way of pulling them. Commit or stash them, then retry this \
                     step — a release cut from here would leave those commits out.\n\n{e}"
                ),
            }),
        };
    }

    // A release commit from an earlier, rejected attempt is the most likely
    // reason to be ahead here, and pulling on top of it is the wrong fix.
    if let Some(tag) = git::unpushed_release_tag(repo).await {
        return Err(StepFailure {
            message: unpushed_release_message(&tag, &tracking, behind),
            remedies: vec![undo_release_remedy(&tag)],
        });
    }

    let mut clash = String::new();
    match settle_own_notes(repo, "@{u}").await {
        Ok(Some(note)) => return Ok(Some(note)),
        Ok(None) => {}
        Err(why) => clash = format!("\n\n{why}"),
    }
    Err(StepFailure {
        message: format!(
            "{tracking} has {commits} this copy does not, and this copy has {} that \
             {tracking} does not. Bring them together before releasing — a release \
             cut from here would leave origin's commits out, and its push would be \
             rejected anyway.{clash}",
            plural(ahead, "commit")
        ),
        remedies: vec![Remedy::new(
            "Pull, and replay your commits on top",
            "git",
            &["pull", "--rebase"],
        )],
    })
}

/// A script is opaque, but a release stopping is common enough, and its
/// causes recognisable enough, that the choice belongs on screen rather than
/// in a terminal.
pub(super) fn script_failure(
    command: &str,
    output: String,
    found: &ReleaseFindings,
) -> StepFailure {
    if !is_release(command) {
        return StepFailure::from(format!("`{command}` failed.\n\n{output}"));
    }
    let behind = match &found.upstream {
        git::Upstream::Behind {
            tracking, behind, ..
        } => Some((tracking.as_str(), *behind)),
        _ => None,
    };

    // The script got as far as committing and tagging, then lost the push.
    if push_rejected(&output) {
        if let Some(tag) = &found.unpushed_tag {
            let (tracking, behind) = behind.unwrap_or(("origin", 0));
            return StepFailure {
                message: format!(
                    "{}\n\n{output}",
                    unpushed_release_message(tag, tracking, behind)
                ),
                remedies: vec![undo_release_remedy(tag)],
            };
        }
        return StepFailure {
            message: format!("`{command}` failed.\n\n{output}"),
            remedies: pull_remedy(&output),
        };
    }

    let wants_notes = output.contains("--no-notes") && !command.contains("--no-notes");
    if !wants_notes {
        return StepFailure::from(format!("`{command}` failed.\n\n{output}"));
    }

    // No notes here may only mean they have not been pulled yet. Offering a
    // build-only release then would ship a real change with no notes.
    if let Some((tracking, behind)) = behind {
        let commits = plural(behind, "commit");
        return StepFailure {
            message: format!(
                "The release stopped because CHANGELOG.md has nothing under [Unreleased] \
                 — but {tracking} has {commits} this copy does not, and the notes are \
                 likely among them. Nothing was bumped, tagged or pushed.\n\n\
                 Pull them and retry: the release then runs on what origin has.\n\n{output}"
            ),
            remedies: vec![Remedy::new(
                &format!("Pull the {commits}, then release again"),
                "git",
                &["pull", "--rebase"],
            )],
        };
    }

    let build_only = format!("{command} --no-notes");
    StepFailure {
        message: format!(
            "The release stopped because CHANGELOG.md has nothing under [Unreleased], \
             so this version would ship with no release notes. Nothing was bumped, \
             tagged or pushed.\n\n\
             If users will notice something in this release, add the notes under \
             [Unreleased] in CHANGELOG.md, commit and push them, then retry this step.\n\n\
             If nothing users can see changed (CI, scripts, docs), release it as \
             build-only below.\n\n{output}"
        ),
        remedies: vec![Remedy::completes(
            "Release as build-only, with no notes",
            "sh",
            &["-c", &build_only],
        )],
    }
}

pub(super) async fn preflight(repo: &str, cfg: &LlmConfig) -> Result<StepOutcome, StepFailure> {
    // Before anything else: a repository part-way through a rebase or a merge
    // cannot do anything a flow would ask of it, and the state is the single
    // most disorienting one git has — the prompt says HEAD, ordinary commands
    // refuse, and nothing says which of continue/abort applies. Say where you
    // are and offer both moves.
    if let Some(state) = git::in_progress(repo).await {
        let changes = git::status(repo).await.unwrap_or_default();
        let stuck = git::conflicted(&changes);

        let what = if stuck.is_empty() {
            format!(
                "A {} is in progress with nothing left conflicting — it just needs finishing.",
                state.label()
            )
        } else {
            format!(
                "A {} is in progress. Git is waiting on {} file(s):\n{}\n\n\
                 Resolve them above — pick a side for each conflict — or in your \
                 editor, where each has <<<<<<< markers, then finish below. \
                 Nothing else can run until this is settled.",
                state.label(),
                stuck.len(),
                stuck
                    .iter()
                    .map(|p| format!("  {p}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        };

        let finish_args = state.finish_args();
        let abort_args = state.abort_args();
        let finish: Vec<&str> = finish_args.iter().map(|s| &**s).collect();
        let abort: Vec<&str> = abort_args.iter().map(|s| &**s).collect();

        return Err(StepFailure {
            message: what,
            remedies: vec![
                Remedy::new(
                    &format!(
                        "Finish the {} — only once every file is resolved",
                        state.label()
                    ),
                    "git",
                    &finish,
                )
                .once_resolved(),
                // Terminal on purpose: aborting throws the work away, so the
                // run should not quietly carry on as if nothing happened.
                Remedy::terminal(
                    &format!(
                        "Abandon the {} and go back to where you were",
                        state.label()
                    ),
                    "git",
                    &abort,
                ),
            ],
        });
    }

    let mut log = String::new();
    let mut failures: Vec<String> = vec![];

    let url = git::remote_url(repo).await;
    let forge = url.as_deref().map(forge::detect).unwrap_or(Forge::None);
    log.push_str(&format!(
        "remote  {}\nforge   {}\n",
        url.as_deref().unwrap_or("(none)"),
        forge.label()
    ));

    // Read fresh from the workspace's Base setting rather than threaded
    // through a node's own config: one setting per repository, not one that
    // has to be repeated on every flow's Preflight node to actually apply
    // everywhere.
    let (base, how) = crate::services::probe::stored_base_branch(repo).await;
    let branch = git::current_branch(repo).await?;
    log.push_str(&format!("base    {base}  ({how})\nbranch  {branch}\n"));
    if git::is_protected(&branch) {
        log.push_str(&format!(
            "        {branch} is protected — the commit node will branch off it\n"
        ));
    }

    let mut remedies: Vec<Remedy> = vec![];
    // Step-by-step instructions for failures no button can fix.
    let mut help: Vec<String> = vec![];

    // An explicit override is otherwise trusted at face value with no check
    // that the branch actually exists on the remote — confirmed 2026-08-27:
    // an override copied from another project's naming convention
    // ("develop") sailed through preflight, then failed only at the PR step
    // with a bare `gh pr create` error instead of a clear, early message.
    if how == crate::services::probe::OVERRIDDEN {
        let git_ref = format!("refs/remotes/origin/{base}");
        if git::branch_exists(repo, &git_ref).await {
            log.push_str(&format!("ok    base branch on origin  {base}\n"));
        } else {
            let (detected, detected_how) = git::default_remote_branch(repo).await;
            let msg = format!(
                "base branch '{base}' does not exist on origin — \
                 this repo's actual default looks like '{detected}' ({detected_how})"
            );
            log.push_str(&format!("FAIL  base branch on origin  {msg}\n"));
            failures.push(msg);
        }
    }

    log.push('\n');

    for check in forge::check_credentials(&forge, repo).await {
        log.push_str(&format!(
            "{}  {:<24} {}\n",
            if check.ok { "ok  " } else { "FAIL" },
            check.name,
            check.detail
        ));
        if !check.ok {
            failures.push(format!("{}: {}", check.name, check.detail));
            if let Some(fix) = check.fix {
                remedies.push(fix);
            }
            if !check.help.is_empty() {
                help.push(check.help);
            }
        }
    }

    match llm::probe(cfg).await {
        Ok(detail) => log.push_str(&format!("ok    {:<24} {detail}\n", cfg.active_model())),
        Err(detail) => {
            log.push_str(&format!("FAIL  {:<24} {detail}\n", cfg.active_model()));
            failures.push(format!("model: {detail}"));
            // A model that is simply not pulled yet is worth a button; an
            // unreachable ollama is not something this app can start for you.
            if detail.contains("not pulled") {
                remedies.push(Remedy::new(
                    &format!("Pull {}", cfg.ollama_model),
                    "ollama",
                    &["pull", &cfg.ollama_model],
                ));
            }
        }
    }

    if !failures.is_empty() {
        let help: String = help.iter().map(|h| format!("{h}\n\n")).collect();
        return Err(StepFailure {
            message: format!(
                "Preflight failed. Nothing in the repository has been touched.\n\n{}\n\n{help}{log}",
                failures.join("\n")
            ),
            remedies,
        });
    }

    Ok(StepOutcome {
        summary: format!("{} · base {base}", forge.label()),
        log,
        artifacts: vec![
            ("remote_url".into(), url.unwrap_or_default()),
            ("forge".into(), forge.as_key()),
            ("base".into(), base),
        ],
        nothing_to_do: false,
        items: vec![],
    })
}
