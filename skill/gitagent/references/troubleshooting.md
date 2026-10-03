# GitAgent troubleshooting

Known failures, grouped by flow. Each entry: what the user sees, why, and what
to do, including the fix GitAgent offers on a button where there is one.
Versions in brackets are where a fix or behaviour arrived; if the user is
older, updating is often the answer.

## Contents

- [Before anything runs](#before-anything-runs)
- [Commit → PR](#commit--pr)
- [Review → Merge](#review--merge)
- [Release](#release)
- [The model](#the-model)
- [Repositories and branches](#repositories-and-branches)

## Before anything runs

**`gh`, `az`, `cargo` or another tool isn't found**, although it works in a
terminal.
Apps started from the Dock, Finder or Start menu don't get the shell's PATH
and environment. GitAgent adopts the login shell's PATH and environment at
startup (0.1.8, 0.1.13); if a tool is still missing, it's installed somewhere
the login shell doesn't set up either. Fix the shell profile, or start
GitAgent from a terminal.

**A repository is greyed out with a 🔒 Pro tag.**
The free version runs flows in 5 repositories (0.1.63, 0.1.67). Click the
repository to give it one of the 5 places, or add a licence in **Get Pro…**.

**A flow is shown as broken and can't start.**
Its definition in `flows.toml` is no longer valid; GitAgent lists what's
wrong. Fix it in **Setup**.

**A button is greyed out.**
Another flow or branch action is running in the same repository (0.1.73).
Hover the button to see which; wait for it, or cancel it.

**An unfinished rebase, merge, cherry-pick or revert is shown on the
repository.**
Something (often a terminal command) left one half-done. GitAgent lists the
conflicted files and offers to finish it once they're resolved, or abandon it
and go back to where it started. Nothing else runs in that repository until
it's dealt with.

## Commit → PR

**The commit approval lists fewer files than expected.**
Only tracked files are committed. New files must be added to git (`git add`)
once before GitAgent will offer them. Unchecking a file at the approval leaves
it out; unchecking all of them commits nothing (0.1.40).

**"Run tests" fails.**
It runs the project's own test command, which GitAgent detects. The failure is
the project's; read the test output in the step's log. The command can be
changed in Setup, or the step skipped for this run with **Skip**.

**Push rejected: the branch is behind its remote.**
Someone pushed to the same branch. GitAgent offers to pull first (0.1.22).

**Push rejected: GitHub still has a branch with this name** from an earlier,
merged or closed pull request.
GitAgent offers to push under a new name, and opens the pull request under it
(0.1.56). Don't pull the old branch in: that mixes its commits into this
change. From 0.1.55 GitAgent avoids picking such a name in the first place.

**"ambiguous argument 'HEAD'" on a brand-new repository.**
Fixed in 0.1.61: the first commit after `git init` goes on the current branch
and is pushed with no pull request.

**Changes were committed outside GitAgent.**
The scan picks them up and the flow carries on from push.

## Review → Merge

**The pull request can't be merged because it conflicts with the base.**
**Bring the base in** switches to the pull request's branch and merges the
base into it (0.1.64). A clean merge is pushed and the merge is retried
(0.1.66). A real conflict stops before pushing, with the files to fix marked;
resolve them in the editor, then finish from GitAgent. Abandoning the pull
request is the last option, not the first.

**"Nothing to review" / the pull request changes no files.**
Everything on the branch is already in the base. GitAgent offers to close the
pull request (0.1.52, 0.1.53).

**CI checks are still pending.**
Review waits for them; a trusted run doesn't stop at the commit flow for
pending checks (0.1.32). A merge is held when checks fail or the analysis
flags a likely regression; that hold also stops a trusted run.

**"Back to base" stops with "cannot lock ref".**
Another program (often an editor's background fetch) updated the same branch
at the same moment. GitAgent retries by itself and otherwise offers to fetch
again (0.1.72). The repository is fine.

## Release

**The release stops because `CHANGELOG.md` has no notes for this version.**
From 0.1.55 the Release flow drafts the notes with the model from what changed
since the last release, for the user to read and approve before they're
committed. A release that only touched CI, scripts or docs gets a
"no user-visible change" line without asking. **Release as build-only, with
no notes** skips them entirely (0.1.54).

**The release offered "build-only" although a pull request was just merged.**
The local copy hadn't pulled the merge yet. From 0.1.72 GitAgent pulls first,
and offers to if it can't do so safely.

**The release script tagged the version but its push was rejected.**
Nothing reached the remote. GitAgent offers one button that undoes the local
release commit and tag, pulls and releases again (0.1.72). Retrying the push
alone can't succeed.

## The model

**A drafting step runs for a very long time, or until the 15-minute timeout,
with a local model.**
The diff was larger than the model's context window and the instructions got
lost. From 0.1.57 what's sent is sized to the context window; lower
**Context window** in Settings for faster answers, or use a bigger model. With
Ollama, steps queue for the model one at a time and show as queued (0.1.50).

**The step log says some files weren't shown to the model.**
They didn't fit in the context window. Lock files go first, then docs and CI,
then the largest code files. Read the draft with that in mind, or raise the
context window if the model supports it.

**"No API key" or an authentication error from a hosted API.**
The key is read from the provider's environment variable or typed in for the
session; it isn't saved. Export it in the shell profile and restart GitAgent,
or enter it again.

**Commit messages are plain lists of files.**
No model is configured (**none** in Settings). That's a valid choice; pick a
model to get drafted messages.

## Repositories and branches

**A repository cloned into the folder doesn't appear** (or a deleted one is
still listed).
Press **Refresh** at the top of the list (0.1.61 and later re-read the
folder).

**A branch is marked "nothing new".**
It has no change the base branch lacks: old merge commits, or work the base
already has from a squash merge. **Clean up** closes its pull request and
deletes it on the forge and locally, after confirming (0.1.62, 0.1.76). A
branch with an open pull request is never offered for clean-up.

**"Delete all merged" couldn't delete some branches.**
It lists each one it couldn't delete, with the error (0.1.73). Read the
error for that branch: it comes from git or the forge, not from GitAgent.
