# Changelog

What changed in each release of **GitAgent**, the desktop app that takes a
repository from a dirty working tree to a merged, released pull request, with
your approval at every step that touches git history or the remote.

The public version of this page — with the download for each release — lives at
<https://mayorana.ch/en/apps/gitagent/releases>. It is generated from this file
by `scripts/changelog_to_json.py`, so this file is the only place a release note
is written.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each heading is dated on the day its tag was pushed. Releases that carried only
build or packaging work say so rather than being hidden: the version numbers a
user sees in the update prompt should all be accounted for. 0.1.5 and 0.1.6
were never released.

## [Unreleased]

### Added

- A release with no notes in CHANGELOG.md no longer stops at the script. The
  model drafts the notes from what changed since the last release, and you
  read and approve them before they are added and committed. A release that
  only touched CI, scripts or docs gets a "no user-visible change" line
  without asking the model. Flows that run `release.sh` get the two new steps
  added in front of it once; remove them in Setup if you don't want them.

## [0.1.54] - 2026-09-25

### Fixed

- A release that stops because CHANGELOG.md has no notes now explains the two
  ways forward and offers to release as build-only, with no notes, in one
  click. Before, it said "no automatic fix" and left you to rerun the script
  in a terminal.

## [0.1.53] - 2026-09-25

### Fixed

- A fix offered from a failed step, such as closing a pull request, now runs
  inside the repository it is for. Run from anywhere else, `gh pr close 11`
  could close #11 of a different repository.
- An open pull request that no longer changes any files is reported as such,
  with an offer to close it, instead of the sidebar showing "ready to merge"
  with no way forward.

## [0.1.52] - 2026-09-25

### Fixed

- Reviewing a pull request whose branch is already fully merged into its base
  says there is nothing to review instead of failing.

## [0.1.51] - 2026-09-24

### Changed

- Packaging only — no user-visible change.

## [0.1.50] - 2026-09-15

### Fixed

- With a local Ollama model, draft steps now queue for the model one at a time
  and show as queued, instead of all starting at once and timing out while they
  waited their turn.

## [0.1.49] - 2026-09-15

### Added

- **Trust all** in the top bar: every repository and every flow runs trusted,
  answering its own approvals, until you turn it off. Turning it off lets a run
  already in progress finish its current flow before asking again.

### Changed

- The sidebar's action buttons are larger and show a tooltip.

## [0.1.48] - 2026-09-14

### Added

- A banner at startup for occasional messages from us, such as a request for
  feedback. It is fetched once from mayorana.ch, stays until you dismiss it and
  is not shown again after that. If the notice cannot be fetched, no banner
  appears and startup is not slowed.

## [0.1.47] - 2026-09-14

### Added

- A flow picker in the workspace, to show or hide a repository's flows without
  leaving the page.

## [0.1.46] - 2026-09-12

### Changed

- Larger, better-spaced buttons in the repository sidebar. The old ones were
  too small to hit reliably.

## [0.1.45] - 2026-09-09

### Added

- Flows can be hidden per repository. A hidden flow is not offered there and a
  trusted run does not use it.

## [0.1.44] - 2026-09-09

### Fixed

- Clearing a flow's name in Setup no longer leaves an unnamed flow in the
  sidebar; it becomes "Untitled flow".
- Setup fields for names, commands and branches no longer autocorrect,
  capitalise or spell-check what you type.

## [0.1.43] - 2026-09-06

### Added

- Flow edits in Setup are kept as a draft until you save. Save is enabled only
  when something changed, and Cancel asks before discarding your edits.

## [0.1.42] - 2026-09-05

### Added

- A first-run screen that asks what writes the commit messages: a model on
  this machine, a hosted API, or none. It preselects what it finds, and a
  hosted API key is kept for the session only. With no model, GitAgent builds
  a plain commit message from the list of changed files.

## [0.1.41] - 2026-09-05

### Added

- **Skip** on a step that is waiting or running, to move past one that is slow
  or not needed this time. A trusted run carries on past a skipped step.

### Changed

- Clearer messages when a model call times out or fails.

## [0.1.40] - 2026-09-05

### Changed

- Large diffs open and scroll much faster. Each diff is highlighted once
  instead of on every redraw.

### Fixed

- Unchecking every file at the commit approval no longer commits the files you
  unchecked. An empty selection now means nothing is staged.
- Branch names generated from a commit message are always valid, even when a
  cut lands on a dot or produces `..` or `.lock`.
- Files with spaces or unusual characters in their paths are staged correctly.

## [0.1.39] - 2026-09-03

### Changed

- Packaging only — no user-visible change.

## [0.1.38] - 2026-09-03

### Added

- The app's icon in the window title bar and taskbar.

## [0.1.37] - 2026-09-03

### Changed

- A repository whose commit flow just opened a pull request and switched back
  to the base branch still shows that pull request as waiting. GitAgent now
  looks at every open pull request, not only the checked-out branch's.

## [0.1.36] - 2026-09-02

### Changed

- The list of files to commit is shown by default at the commit approval,
  instead of hidden behind an unlabelled toggle.

## [0.1.35] - 2026-09-02

### Added

- A trusted run that stops says why: which step needs a person, or that no
  flow answers what the repository needs next.
- **Stop asking me** on an approval: approves that step and every later one
  in the repository, turning the run you started into a trusted run. It still
  stops at a merge the analysis or CI is unhappy about.
- Setup marks a flow that answers no repository need, since nothing will
  ever start it.

## [0.1.34] - 2026-09-02

### Added

- A tally at the top of the sidebar: how many repositories are running, have
  a failed run, or need something from you.

## [0.1.33] - 2026-09-02

### Added

- The sidebar marks repositories whose last run failed, and each repository
  has a button to check it again without refreshing the whole list.

## [0.1.32] - 2026-09-02

### Fixed

- A trusted run no longer stops at the commit flow because the new pull
  request's checks are still pending. It moves on to the review.

## [0.1.31] - 2026-09-02

### Changed

- A trusted run now works through the repository rather than one flow: after
  a commit it moves on to the review, then the release, re-checking the
  repository between each, and stops when something needs a person.
- The test step is added to your existing commit flows too, between scanning
  and committing, keeping your own settings and steps. If you remove it, it
  stays removed.

## [0.1.30] - 2026-09-02

### Added

- **Trusted run**: a flow answers its own approvals and stops only at a step
  that needs a person, showing why it stopped there.
- A test step in the commit flow that runs the project's own test command
  before anything is committed. GitAgent detects the command.

## [0.1.29] - 2026-09-02

### Changed

- Packaging only — no user-visible change.

## [0.1.28] - 2026-09-02

### Added

- A banner when a newer version is available.

## [0.1.27] - 2026-09-01

### Added

- Flows that are no longer valid are listed with what is wrong, and cannot be
  started until they are fixed.

## [0.1.26] - 2026-08-31

### Changed

- Opening the review flow selects the pull request that needs reviewing,
  instead of leaving you to pick it.

### Fixed

- The base branch is worked out the same way everywhere. The sidebar and the
  flows could disagree about what a branch was compared against.

## [0.1.25] - 2026-08-31

### Changed

- Packaging only — no user-visible change.

## [0.1.24] - 2026-08-31

### Changed

- The Windows installer installs for the current user by default, with no
  administrator prompt. Administrators can still install for every user from
  the command line.

## [0.1.23] - 2026-08-30

### Added

- GitAgent detects a rebase, merge, cherry-pick or revert left unfinished,
  shows it in the sidebar with the conflicted files, and offers to finish it
  once everything is resolved, or abandon it and go back to where you were.

## [0.1.22] - 2026-08-30

### Added

- A push rejected because the branch is behind its remote offers to pull
  first.

## [0.1.21] - 2026-08-30

### Changed

- Packaging only — no user-visible change.

## [0.1.20] - 2026-08-29

### Changed

- Packaging only — no user-visible change.

## [0.1.19] - 2026-08-29

### Changed

- A pull request with merge conflicts now offers to bring the base branch into
  it and resolve the conflicts in a terminal, before offering to abandon it.

## [0.1.18] - 2026-08-29

### Fixed

- On macOS, notifications no longer ask which application should show them
  each time one is raised.

## [0.1.17] - 2026-08-29

### Added

- Flows declare which repository needs they answer (uncommitted changes, an
  unpushed branch, an open pull request, a release due), chosen in Setup. The workspace opens the
  flow that answers what a repository needs, whatever the flow is called.

### Changed

- A step you reject now shows as declined rather than failed.

## [0.1.16] - 2026-08-28

### Changed

- The sidebar shows a running, waiting or failed run ahead of what the
  repository needs next.

### Fixed

- A release that was due could be hidden behind an open pull request with
  pending checks.

## [0.1.15] - 2026-08-28

### Added

- A system notification when a run stops and needs you, only while the
  GitAgent window is in the background.

## [0.1.14] - 2026-08-28

### Fixed

- Long repository names are shortened in the workspace instead of pushing the
  buttons off their row. The full name shows on hover.

## [0.1.13] - 2026-08-28

### Fixed

- GitAgent picks up the environment variables from your login shell, such as
  API keys and tokens, so commands behave as they do in a terminal when the app
  is started from the Dock or Start menu.
- Command output is shown without stray colour codes.

## [0.1.12] - 2026-08-28

### Changed

- Downloads and update checks now come from mayorana.ch instead of GitHub.
- GitAgent is source-available under the PolyForm Noncommercial licence: free
  for personal, educational and noncommercial use.

## [0.1.11] - 2026-08-27

### Added

- A spinner on a branch while deleting it or opening a pull request for it,
  with its buttons disabled until the action finishes.

## [0.1.10] - 2026-08-27

### Added

- The sidebar shows merged work that has not been released yet.

### Fixed

- Only open pull requests are offered for review. A merged or closed one
  could be picked and then fail.

## [0.1.9] - 2026-08-27

### Changed

- Packaging only — no user-visible change.

## [0.1.8] - 2026-08-27

### Fixed

- GitAgent uses your login shell's PATH, so `gh`, `az`, `cargo` and other tools
  are found when the app is started from the Dock or Start menu.

## [0.1.7] - 2026-08-27

### Changed

- Packaging only — no user-visible change.

## [0.1.4] - 2026-08-26

### Added

- A Target branch setting for pull requests, for repositories that merge into
  something other than their default branch, such as `develop`. Left empty,
  GitAgent detects it.

### Fixed

- Clearer errors when fetching the base branch fails.

## [0.1.3] - 2026-08-26

### Changed

- Packaging only — no user-visible change.

## [0.1.2] - 2026-08-26

### Added

- Review any open pull request on a repository, not only the one for the
  checked-out branch, with several reviews in progress at once. The sidebar
  shows how many pull requests each repository has open.
- A syntax-highlighted diff view, with a preview of the pull request's diff
  before anything runs. Showing the diff is a step you approve.
- A step waiting for your approval is marked in the graph.
- Output from push and other remote commands streams in as it happens.
- Flows can be shown or hidden per repository.
- A pull request with merge conflicts offers to abandon it.

### Fixed

- A failure to list pull requests (rate limit, network, sign-in) is shown as
  an error instead of looking like a clean repository.
- The CI status step checks the pull request being reviewed, not the one for
  the checked-out branch.
- Changes committed outside GitAgent are picked up, and the rest of the flow
  continues.
- Long diffs scroll instead of being squeezed to fit.

## [0.1.1] - 2026-08-26

### Added

- First release: turn a dirty working tree into an open pull request. GitAgent
  scans the changes, drafts the commit message and pull request description
  with a model, then commits, pushes and opens the pull request.
- Nothing touches git history or the remote without your approval. Each gated
  step shows the exact commands it will run first, and only tracked files are
  committed.
- Local Ollama or remote DeepSeek for the drafts.
- A Setup screen with a flow editor.
