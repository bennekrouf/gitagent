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

### Fixed

- A repository with another repository inside it, such as a submodule or a
  worktree folder committed by mistake, no longer starts a commit that can only
  fail. Edits made inside that inner repository belong to it, so GitAgent no
  longer counts them as changes here; moving it to another commit still counts.

## [0.1.128] - 2026-10-25

### Fixed

- Installing silently, as IT does for an unattended update with `/VERYSILENT`,
  no longer hangs when GitAgent is already installed: setup waited on a
  question nobody could see. It now replaces the installed version without
  asking.
- A repository with no remote can now run a flow up to the commit. Preflight
  used to stop it for having no remote; now only the push says so, and how to
  add one.
- A repository path that doesn't exist is reported in English, whatever
  language Windows is set to, rather than in Windows' own words.

## [0.1.127] - 2026-10-25

### Changed

- Activating a licence now shows a small spinner while it is checked, then a
  window that says you're Pro, with the AI credits left and the renewal date in
  small type below. Removing the licence and checking it again no longer sit
  beside it; Manage subscription is a small link. Press Esc or the × to close
  it.

## [0.1.126] - 2026-10-25

### Changed

- Activating a licence now shows a small spinner while it is checked, then
  a window that says you're Pro, with the AI credits left and the renewal
  date in small type below. Removing the licence and checking it again no
  longer sit beside it; Manage subscription is a small link. Press Esc or
  the × to close it.
- The run controls, the replay speed and zoom buttons and the side-by-side
  choice buttons are now 30px tall, like every other button.

## [0.1.125] - 2026-10-23

### Added

- `gitagent run` runs a flow from the terminal, with no window. Leave out
  `--flow` and it runs the one the repository needs next. Every step that
  touches git history or the remote is put to you at a prompt, where you can
  read the diff and leave files out of the commit before you approve.
  `--trusted` answers approvals by the Trusted run's rules and carries on into
  the next flow, stopping at the same things it stops at in the window.

- With no terminal to ask, as in CI or a scheduled job, `gitagent run` stops
  at the first approval rather than guessing, and its exit code says how the
  run ended: finished, a step failed, nothing ran, stopped at an approval, or
  an approval declined. A pull request's address and the other things a run
  produced are printed one per line for a script to read.

- An open window shows a run started from the terminal as it goes, and says
  to answer its approvals in that terminal, as it already did for runs
  started from Claude.

### Fixed

- A repository whose `origin` is a folder on your machine (`C:\...` on
  Windows) no longer fails Preflight as an unsupported host named `c`. Every
  step up to and including the push now runs; only opening the pull request
  needs GitHub or Azure DevOps.
- On Windows, repository paths in replies to Claude no longer start with
  `\\?\`, so they match the path Claude sent.
- A repository folder that no longer exists is now reported in English,
  instead of in the language Windows is set to.
- The PRO badge in the top bar is now the same height as the buttons beside
  it.

## [0.1.124] - 2026-10-23

### Fixed

- Fields that hold an exact value no longer correct what you type into them:
  the target branch under Base branch, the ollama address and model name in
  Model provider, and the licence number and email under GitAgent Pro. A
  branch or model name is not prose, and it was being capitalised,
  autocorrected, underlined in red and offered completions from other fields,
  while git and the model APIs take only what is written. Setup's own fields
  already behaved this way.

## [0.1.123] - 2026-10-23

### Changed

- A repository whose pull request is ready now offers the decision instead of
  reporting it: where the list said "ready to merge", it has a Merge #34
  button that opens that repository and starts the review, with the pull
  request it names already chosen. Four repositories all reading "ready to
  merge" used to need opening one by one to tell apart. The note stays
  wherever the button cannot be offered — a hidden Review → Merge flow, or a
  pull request whose checks are red.
- Settings → Model provider is down to four choices, side by side: the model
  on this computer, or api0 in Europe, China or the United States. No step
  between them and no fifth option — the flags are the choice, and what a
  step costs in each region is on the button.
- The second reviewer now has a switch of its own. Tick "Review every pull
  request a second time" and the same four choices appear for it; untick it
  and there is one review, as before.
- The first-run question offers that same row, so the choice made on day one
  is the one Settings shows afterwards.

### Removed

- Bringing your own API key is gone from Settings and from the first-run
  screen, which no longer has anything to paste into, and so is "No AI". Every
  model GitAgent calls is either on your computer or on api0's regions, which
  run on your plan's AI credits with nothing to paste. A setting from an
  earlier version that named a provider of your own, or no model at all,
  keeps working exactly as it did; Settings says so, and one click moves it
  to one of the four.

## [0.1.122] - 2026-10-23

### Changed

- Settings → Model provider now offers the same four choices on both tabs:
  api0, Ollama, a provider you have a key for, and none. The second reviewer
  is picked the way the default model is, api0 region and all, instead of a
  drop-down of its own, and the list of key-based providers (OpenAI, Mistral
  and the rest) only appears once you say you have a key.
- api0 carries the mayorana mark in that row, and the region you run in — its
  flag and what a step costs — sits right under it.
- A model set to api0 now names its region everywhere. The button in the top
  bar that opens this window used to read "default" for a plan that had never
  been sent to a region; it reads the region's flag and name from now on, and
  an older setting is moved to Europe the first time it is read.
- With no licence on this computer, api0 offers to sign in from inside the
  window rather than sending you to look for Get Pro… in the top bar.
- The default model can be turned off from this window. "No AI" used to be
  offered only while setting GitAgent up for the first time.
- The question a new install is asked — what writes the commit messages —
  now offers that same row of providers, so the choice made on the first
  screen looks like the one Settings offers later. A key, or an api0 licence
  number, is still pasted right there and kept for the session only.

### Removed

- DeepSeek is no longer in the list of providers you bring a key for: api0
  runs the model in Europe, China or the United States instead. A setting
  still on DeepSeek moves to OpenAI; pick api0 or another provider in
  Settings. The OpenRouter preset now starts on an OpenAI model.
## [0.1.121] - 2026-10-23

### Changed

- The default model and the second reviewer now offer the same four choices in
  the same row: api0, Ollama, your own key, or none. The second reviewer can be
  set to an api0 region of its own, and the long list of providers only appears
  once you say you have a key. When api0 needs a licence, Settings offers a
  button that opens the licence window directly instead of pointing you to the
  top bar. A model saved before api0 had regions now shows the region it runs in
  rather than "default".

## [0.1.120] - 2026-10-22

### Changed

- A new install only asks what writes the commit messages when there is a model
  on this machine to offer. With ollama running it is the same one-click
  question as before, now naming the model it found — and a model other than the
  usual one is picked up rather than reported as missing. Without ollama the
  question is skipped: GitAgent starts on the Europe model and opens straight
  onto your repositories, and Settings changes it whenever you like. "No AI" is
  no longer offered as a way to set up GitAgent; every flow still runs without a
  model where one is unreachable, and the second reviewer is still off until you
  turn it on.

## [0.1.119] - 2026-10-22

### Added

- Settings has three new choices for the model: Europe, China and the
  United States. Each is one click, runs on your plan's AI credits with no
  API key to set up, and shows about how many credits a step costs, so you
  can weigh them before a run. Ollama sits beside them, and a provider with
  a key of your own is still one click away.

- Each open folder keeps its own model. Set one window to China and the one
  beside it to the United States, and both stay that way — the model shown in
  the top bar is the folder's, and changing it there no longer moves any other
  window. A folder you have not chosen for runs the model set with no folder
  open, so nothing has to be picked twice. The second reviewer and the focused
  reviews are still one setting for the whole computer.

### Changed

- A new install only asks what writes the commit messages when there is a
  model on this machine to offer. With ollama running it is the same one-click
  question as before, now naming the model it found — and a model other than
  the usual one is picked up rather than reported as missing. Without ollama
  the question is skipped: GitAgent starts on the Europe model and opens
  straight onto your repositories, and Settings changes it whenever you like.

- "No AI" is no longer offered as a way to set up GitAgent. Every flow still
  runs without a model where one is unreachable, and the second reviewer is
  still off until you turn it on.

- The repository list offers the next step instead of only naming it: a
  repository with uncommitted files shows a "Commit 3 files" button where it
  said "uncommitted", and one with a release due shows a button named after
  the flow that will run it — "Release", or "Deploy VPS" if that is what cuts
  releases here. A click opens the repository and starts that flow, so any
  approval it stops at is right in front of you.

## [0.1.118] - 2026-10-22

### Added

- The panel that opens beside the run map can be resized: drag its left edge
  to widen it as far as the whole view, so a pull request's diff or a step's
  log can be read full width, and back down again. It never gets so narrow it
  is hard to grab, the width you leave it at is kept between launches, and a
  double-click on the edge puts it back to its usual size.
### Changed

- One licence number for every Mayorana app on this computer. Paste it in any
  of them and the others are Pro too the next time they start, with nothing to
  paste again; a number already pasted in one app is shared the same way.
  Removing the licence in the Pro window removes it from all of them.

## [0.1.117] - 2026-10-21

### Added

- Free AI credits every month, without Pro: in the GitAgent Pro window, enter
  your email under Get free AI credits and a licence number is sent to you.
  Paste it like any licence, and the api0 model under Settings → Model
  provider works on those credits. The window shows "Free" and the credits
  left this month. Buy a plan later with the same email and that licence
  becomes Pro, with nothing to paste again.

### Changed

- Without a licence, Settings no longer just says api0 has no key: it points
  to the free AI credits in the GitAgent Pro window, and links to the plans
  for more.
- A licence whose plan doesn't include GitAgent now shows as AI credits only,
  with the credits left this month, instead of as a lapsed Pro licence.
- Settings → Model provider is now split into two tabs, Default and Second
  reviewer, instead of one long panel. The Default tab holds the model every
  step calls — the commit message, the pull request description, the review,
  the release notes — with its focused reviews; the Second reviewer tab holds
  the model that reviews a pull request a second time. Each tab says which
  model it is set to, so you can see whether the second reviewer is on without
  opening it.
- The GitAgent Pro window no longer lists "Apps included: none" for a plan
  that names no apps, and its buttons wrap as whole buttons instead of
  squeezing each label onto two or three lines.
- Activating a licence now just says it worked — **GitAgent Pro is active**,
  with your email and this month's AI credits — and a Done button, instead of
  putting Remove from this computer and Manage subscription in front of you
  the moment you have subscribed. They are still there when you open the
  window again.

### Removed

- The free version's limit of five repositories is gone: GitAgent runs flows
  in every repository, with or without a licence. No repository is greyed out
  with a 🔒 Pro tag any more, and the GitAgent Pro window no longer lists five
  repositories or asks you to give one back.

## [0.1.116] - 2026-10-21

### Added

- api0 is now one of the hosted models under Settings → Model provider and on
  the first-run screen. It needs no API key of its own: your GitAgent Pro
  licence is enough, and the model's use comes out of your plan's monthly AI
  credits. Test connection shows how many credits are left this month.
- When a hosted model refuses a request, the step's log now shows the
  provider's own explanation instead of the raw reply around it.

### Changed

- GitAgent Pro now comes with a monthly plan from mayorana.ch. Subscribing
  emails you a licence number such as `ABCDE-FGHJK-LMNPQ-RSTUV`: paste it in
  the GitAgent Pro window, in any case, with or without the dashes. The window
  shows your plan, the apps it includes, the AI credits left this month and
  when it renews, and Manage subscription opens the page where you change or
  cancel it.
- GitAgent checks your licence when it starts and once a day. Without a
  connection Pro keeps working for seven days after the last check that got
  through. The free version's five repositories work as before, online or not.
- Opening a folder from the home screen now shows that it is being checked:
  the folder's row spins and counts the repositories as each one is reached
  (**Checking 3/12**), the other folders step back until it is done, and
  **Open folder…** and **Refresh** spin while they work. Before, the only sign
  was a line of text under the list, so a slow check looked like nothing was
  happening.

## [0.1.115] - 2026-10-20

### Added

- A Feedback button in the top bar, and Feedback and mayorana.ch links under
  the welcome screen, open the contact form with GitAgent and your version
  already filled in, so an idea or a problem reaches us in two clicks.

## [0.1.114] - 2026-10-20

### Fixed

- Opening a folder now checks that you can reach every repository in it before
  the window opens. Before, a repository you had no access to, such as an Azure
  DevOps project you were never added to, could leave the new window blank
  forever on Windows. Now the welcome screen names each repository it could not
  reach and shows git's reason. You can fix access and try again, or open the
  folder anyway.
- When a merge stops on conflicts, the button that finishes the merge now waits
  until every file is resolved. It names the files git is still waiting on and
  points you to Resolve conflicts above it, including Mark resolved for a file
  you fixed in your editor. Before, the button could be clicked too early and
  only showed git's "unmerged files" error, with no hint of what to do next.

## [0.1.113] - 2026-10-20

### Changed

- Approving a step from its panel over the run map, such as a release, now
  keeps the panel open on the step's log while it runs. The panel closes once
  the step succeeds. If the step fails, the panel stays open on the error.
  Closing the panel yourself or moving to another step still works as before.

### Fixed

- When a run stops because Azure DevOps does not accept your Azure sign-in
  (TF400813, "not authorized"), the error now explains why and what to do:
  check your access to the organization, sign in to the tenant it is
  connected to, enroll in multi-factor authentication if that tenant asks for
  it, or use a personal access token instead.

## [0.1.112] - 2026-10-20

### Fixed

- Approving a step from the repository list now closes that step's panel over
  the run map, as the Approve button in the panel itself does. Before, the
  panel stayed open on a step that had already moved on.
- When a run stops because the GitHub CLI is not signed in, the error now walks
  you through `gh auth login` step by step, offers the token alternative, and
  shows what `gh auth status` reported. It also tells you when one account is
  signed in but another has a stale sign-in, which fails the check too.

## [0.1.111] - 2026-10-19

### Fixed

- The Approve button in the repository list now also shows for a step a
  trusted run stopped at, such as release notes, a script or a deploy. Before,
  those rows only said the run needed you, and approving meant opening the
  run. Hover the button to read why the run stopped before you approve.

## [0.1.110] - 2026-10-19

### Added

- GitAgent now includes a script to check for dynamic C runtime dependencies in
  the Windows executable. This ensures that the application runs smoothly on any
  PC without requiring additional installations of the Visual C++
  Redistributable.

## [0.1.109] - 2026-10-16

### Added

- Changed your mind while resolving a merge? Start over puts a file's
  conflicts back exactly as git left them, so you can pick again, even after
  you saved it or it was marked resolved. Files already resolved are listed
  under the conflicts, each with its own Start over, and Start over on every
  file takes the whole merge back to its first stop. Your choices are thrown
  away, but the merge itself carries on; abandon it if you want to undo it
  entirely. If git remembers resolutions on your machine, the one you undid is
  forgotten too, so it is not quietly applied again next time.

## [0.1.108] - 2026-10-16

### Added

- Merge conflicts can be resolved without leaving GitAgent. When a step stops
  because a merge, rebase, cherry-pick or revert is waiting on files, the
  step's panel now lists those files and shows each conflict as its two sides,
  Current and Incoming, with Accept current, Accept incoming or Accept both,
  as in an editor. Save writes your choices, and once nothing in a file is left
  undecided it is marked resolved, ready for Finish below. A file one side
  deleted, or one that is not text, is settled by keeping one side whole, and a
  file you already fixed in your editor only needs Mark resolved.

### Fixed

- The second opinion works with small local models again. A small model could
  get stuck repeating the same characters in a quoted line of code until it ran
  out of room, and the step failed with "model did not return JSON" followed by
  a wall of unreadable text. Each part of a review now has a length limit the
  model has to respect, so it moves on instead. If an answer is still cut off,
  the step says so plainly and suggests running it again or picking a larger
  model.

## [0.1.107] - 2026-10-16

### Added

- Claude can work on your flows through GitAgent's MCP server, not only run
  them: list the kinds of step a flow can use, read a flow, create or change
  one, delete one, and choose which repositories show it — the same checks as
  Setup, so a flow that would not run is refused with the reason. A step that
  runs a script or a server command always stops for your approval: Claude
  cannot switch that off. `flows.toml` is backed up before every change, and
  the open window shows the change without a restart.
- Claude can also ask for everything GitAgent knows about a repository: its
  branch, uncommitted files, what it needs next and which flow answers that,
  open pull requests with their checks, the release state, and what each flow
  would act on.

## [0.1.106] - 2026-10-15

### Changed

- The buttons for approving, rejecting, or skipping steps are now always visible
  at the bottom of the step's panel, regardless of how long the content above
  them is. This makes it easier to respond to steps without scrolling.
- When a step fails, the fixes on offer under **Fix it from here** — pull and
  replay, bring the base in, close the pull request — sit in that same bar at
  the bottom, just above Retry, Skip and Cancel run, instead of up in the
  panel above the error. Every decision on a step is now in one place.

### Fixed

- A release no longer stops at "this copy has 1 commit that origin/main does
  not" when the commit is GitAgent's own release notes and a pull request was
  merged on GitHub meanwhile. The notes are committed on top of what origin has
  at that moment, and if a merge still lands in between, the release puts the
  notes commit back on top of it by itself and carries on. Only a clash —
  both edited CHANGELOG.md — or a commit of your own still asks first.
- **Back to base** handles the same case after a merge: GitAgent's own release
  notes commit left on your local base is put back on top of origin, or — when
  a later pull request already carried those notes — simply dropped, instead
  of failing with "Not possible to fast-forward". A commit of your own there
  gets a **Replay your local commits on top** button rather than "No automatic
  fix".

## [0.1.105] - 2026-10-15

### Added

- Flows Claude runs through GitAgent's MCP server can be trusted runs: when
  you ask Claude for a trusted run, or have **Trust all** on in the window,
  GitAgent answers the approvals itself by the same rules as its Trusted run —
  stopping at anything risky, at your own scripts and deploys, and at release
  notes — and carries on into the next flow the repository needs. Claude is
  told what went through on its own and where, and why, it stopped; the
  window's live panel marks such a run "trusted".

### Changed

- **Trust all** is remembered between launches, and shared with GitAgent's MCP
  server, instead of switching itself off every time the window opens.
- Approve, Reject and Skip now stay pinned to the bottom of a step's panel, so
  you can answer without scrolling past a long diff first. The same goes for
  Retry, Skip and Cancel run after a step fails, and for skipping a step that
  is still waiting or running.

### Fixed

- **Back to base**, the last step of Review → Merge, no longer fails when you
  have uncommitted edits — the usual case when a review is merged while you
  keep working. It never touches them: on another branch, it brings your base
  branch up to date without switching to it, and you stay where you were; on
  the base branch itself, it leaves everything as it is and says how far
  behind it is, to pull once your edits are committed.
- A deploy flow's tab now counts the commits merged since your last deploy,
  not since the repository's last tag. A flow that deploys without tagging
  used to show a number that only ever grew — a repository deployed on every
  merge read 172 commits pending, all counted from a tag a year old. The count
  goes back to zero after each deploy that goes all the way through, and its
  tooltip says "since the last deploy".

## [0.1.104] - 2026-10-14

### Fixed

- On macOS, a run no longer stalls while GitAgent's window is hidden or
  covered: it used to look stopped and only catch up once you showed the
  window again. GitAgent now tells macOS it is busy while a flow or branch
  action is running, and still lets your Mac go to sleep.

## [0.1.103] - 2026-10-14

### Added

- Settings lists the models your local ollama has installed, with their size,
  under the Model field — and under the second opinion's, when it runs on
  ollama too: click one to use it instead of typing its name exactly. Typing
  still works, for a model you are about to pull. If ollama is not running,
  Settings says so.
- A flow Claude runs through GitAgent's MCP server shows up in the open
  window, live: under the flow tabs of that repository, "Claude is running
  Commit → PR here", each step with where it stands, and the step waiting for
  approval with exactly what it would do. It is read-only — answer approvals
  in Claude, where the run was started — and it goes away once that session
  ends.
- The repository list keeps itself current: a commit, push, pull, branch
  switch or merge made outside the window — from a terminal, or by GitAgent's
  MCP server — updates that repository's row and tab counts within a couple
  of seconds, without a Refresh. Plain file edits still need one.

### Changed

- A flow you made now has an id that reads as its name — **Deploy cvenom** is
  `deploy_cvenom`, not `review_and_merge_copy_copy_copy_copy_2` — so anything
  that sees the id, such as an assistant starting the flow through GitAgent's
  MCP tools, sees what the flow does rather than what it was copied from.
  Existing flows are renamed once, on the first launch, and the flows each
  repository shows or hides follow; Setup keeps the id in step with the name
  from then on. The built-in Commit → PR and Review → Merge keep theirs.
- The button that re-checks one repository, shown when you hover its row in
  the repository list, is now a proper green button with the same icon as
  Refresh, instead of a tiny grey glyph that was easy to miss.

### Fixed

- A local model that thinks before it answers, such as MiniCPM5, no longer
  comes back empty. It used to spend the whole answer on thinking about a
  medium-sized diff and return nothing, so the step failed with "model did not
  return JSON"; GitAgent now asks for the answer only.
- A review finding whose quote is the right code, but written without the
  diff's `+`/`-` line markers or with two lines joined, is no longer thrown
  out as "quoted evidence was not in the diff". A quote that is not in the
  diff is still dropped.

## [0.1.102] - 2026-10-14

### Added

- A new **MCP** button in the top bar shows how to use GitAgent from Claude
  Desktop or Claude Code: the text to paste, with a Copy button, already
  pointing at where GitAgent is installed on your computer. Claude in the
  browser can't connect to it, and the window says so.

## [0.1.101] - 2026-10-14

### Added

- Flow tabs say what their flow would act on right now: **5 files** on the
  commit flow when there are changes to commit (or **2 commits** with no pull
  request yet), **3 PRs** on the review flow, **4 commits** on the release
  flow since the last tag. Hover a count for what it is; a flow with nothing
  to do shows none. It follows what each flow says it handles in Setup, so it
  works whatever the flow is called.

### Fixed

- A fix offered under a failed step that runs and stops no longer looks as if
  nothing happened: its button reads **Run again**, and what it printed is
  shown under "That stopped before finishing".
- When **Bring the base in** stops on conflicts, the Merge step now moves on to
  what comes next instead of keeping its old message. It names the files to
  fix and offers **Finish the merge, push, and merge again** — which refuses
  while a file still has conflict markers — or **Abandon the merge** to put the
  branch back as it was.

## [0.1.100] - 2026-10-14

### Added

- The run view now has the same **Branches**, **Base** and **Trusted run**
  buttons as the list view, at the top next to Play, so you no longer have to
  switch back to the list to open the branches, change the base branch, or
  hand a run over to a trusted run. **Stop asking me** and **Trusting…**
  show there too while a run is going.

## [0.1.99] - 2026-10-14

### Added

- GitAgent now displays failed checks in the pull request brief, allowing you to
  see which checks failed and navigate directly to their logs.
- A repository waiting on an approval now has an **Approve** button right in the
  repository list, so you can approve it without opening the repository first.
  Hovering over the button shows exactly what it will run.

## [0.1.98] - 2026-10-14

### Added

- GitAgent can now be driven from Claude, or any other assistant that speaks
  MCP: point it at `gitagent mcp` and it can list your repositories, run a
  flow, and stop at every approval with the exact files and commands, so
  nothing touches history or the remote until you say yes in the chat. It
  uses the same flows, settings and sign-ins as the app.
- A repository waiting on an approval now has an **Approve** button right in
  the repository list, in place of **needs you**, so you can answer it
  without opening the repository first. Hover it to see exactly what it will
  run. A step a trusted run held back, or a commit with every file
  unchecked, still says **needs you** and wants a look first.

### Changed

- Review flows you copied or built in Setup now work like **Review → Merge**:
  they show the list of open pull requests to pick from, and once a pull
  request has been through, they move on to the next one rather than offering
  to run again.
- Commit flows you copied or built in Setup now work like **Commit → PR**:
  the Start button says what it will do — **Commit 5 files**, **Push 2
  commits & open PR** — and reads **Nothing to commit** on a clean tree,
  instead of a plain **Start run** that would find nothing to do.

## [0.1.97] - 2026-10-13

### Changed

- Once a pull request has been through **Review → Merge** — merged, declined,
  or nothing to do — the flow moves straight on to the next open pull request
  instead of offering **Review #N again**. When none is left, the button says
  **Nothing else to review**.

## [0.1.96] - 2026-10-13

### Added

- Adding a **Second opinion** or **Focused reviews** step in Setup now tells
  you straight away when Settings gives it nothing to run with — no second
  reviewer chosen, or no focused review turned on — rather than leaving you to
  find the step skipped on your first pull request. A button on the notice
  opens Settings on the spot, and the notice goes once that is set up.

### Fixed

- **Second opinion** and **Focused reviews** no longer say "skipped by you"
  when they were skipped because nothing is set up for them in Settings. They
  now say "skipped", with the reason underneath — no second reviewer chosen,
  or no focused review turned on.

## [0.1.95] - 2026-10-13

### Fixed

- A repository with uncommitted changes now opens on **Commit → PR** even when
  colleagues have pull requests open in it. Before, any open pull request with
  passing checks marked the repository as ready to merge and opened it on
  **Review → Merge**, so your changes were never offered for commit. A pull
  request from the branch you have checked out still comes first.

## [0.1.94] - 2026-10-11

### Changed

- The button at the end of the flow tabs that chooses which flows a
  repository shows always says what it is — **0 hidden** when nothing is
  hidden — instead of a bare "⋯".

## [0.1.93] - 2026-10-09

### Added

- Keyboard shortcuts for the Run view's map zoom, the ones a browser uses:
  **⌘+** zooms in, **⌘−** zooms out and **⌘0** goes back to 100% (Ctrl on
  Windows and Linux), around the middle of the view. They only act while the
  Run view is on screen.
- Zooming the Run view's map shows the new percentage for a moment in the
  middle of the map, however you zoomed — buttons, shortcuts, ⌘ + scroll or
  pinch — so you can tell where you are without looking down at the bar.
- When a flow's map is wider than the Run view, the view now scrolls along
  with the run: as each step starts, waits for you or fails, the map glides
  sideways to keep it in sight, and with the step panel open it keeps the
  step you picked clear of the panel. It only moves when the run moves on,
  so it never pulls the map away while you scroll around it yourself.

## [0.1.92] - 2026-10-09

### Changed

- Zooming the Run view's map keeps what you are pointing at under the
  pointer: ⌘ + scroll and pinch zoom around the cursor, and **−** / **+**
  around the middle of the view, instead of leaving the map scrolled to where
  it was and sending what you were looking at off screen.

## [0.1.91] - 2026-10-09

### Added

- The Run view's map now zooms, from 50% to 200%, using **− / +** in its bottom
  bar, or ⌘ + scroll and pinch on the map; click the percentage to go back to
  100%. Zoomed out, a big flow such as Review → Merge with its review circle
  fits on one screen; zoomed in, everything grows and stays sharp, and the map
  scrolls. The rest of the window keeps its size, and GitAgent remembers the
  zoom.

## [0.1.90] - 2026-10-09

### Added

- The repository list on the left can be folded: **‹** in its header shrinks it
  to a strip of status dots, one per repository, still showing which one wants
  you and still clickable; **›** opens it again. GitAgent remembers which way
  you left it.
- The Run view's map zooms, from 50% to 200%: **− / +** in its bottom bar,
  beside the replay speed, or ⌘ + scroll and pinch on the map; click the
  percentage to go back to 100%. Zoomed out, a big flow such as Review →
  Merge with its review circle fits on one screen; zoomed in, everything grows
  and stays sharp, and the map scrolls. The rest of the window keeps its size,
  and GitAgent remembers the zoom.

### Changed

- Steps describe what they do with the run's real branches and pull request
  once it knows them, in the Run view and on the list view's step cards alike:
  **Push branch** reads `git push -u origin feat/your-branch` instead of
  `<branch>`, **Open pull request** reads `feat/your-branch → main`, and the
  review flow names its pull request and base. On the Run view's map a long
  branch name breaks across lines at its slashes and dashes rather than being
  cut off, and hovering a step shows it in full.
- The 0.5× / 1× / 2× buttons are labelled **Replay speed** and moved from
  the Run view's header to its bottom bar, beside the step pills, leaving the
  header to the controls that start and pause a run. They set how fast a
  replay steps through the run and how fast the map animates, never how fast
  the run itself goes.
- Refreshing a repository, or the whole list, also clears its runs that have
  ended — done, nothing to do, or declined — so the Run view shows where the
  repository stands now instead of the last run's steps all marked done.
  Runs still going, waiting for you, or failed with a fix to try are kept.
- The hint at the end of the Run view's bottom bar steps aside when the bar
  is crowded, leaving the room to the step pills and the replay speed, and
  comes back when there is space.

### Fixed

- In the Run view, Play starts the flow again once its last run has
  finished, as Start does in the list view. Before, a finished run left Play
  pressed and unusable, with no way to run the flow again from the Run view;
  Restart still replays the run that finished. When it would start a run,
  the button says what it will do in the list view's words — "Commit 5 files
  again", "Review #104" — instead of a bare "Play".
- In the Run view, a step you skip no longer looks like the place the run is
  waiting. The track into it went grey and its dot stopped short of it; now
  the run passes straight through in blue and waits at the next step that
  was not skipped, or turns green once everything past it is done.
- When **Back to base** or a release can't update the branch because you have
  uncommitted edits to files that changed on origin, the step now offers to set
  your edits aside, update, and put them back, all from the run view. Before,
  you had to leave GitAgent and stash them in a terminal. If your edits clash
  with what came in, both versions stay marked in the file and a copy is kept
  in the stash, so nothing is lost. **Back to base** still finishes and lists
  the files to resolve, and a release waits until they are resolved.

## [0.1.89] - 2026-10-09

### Changed

- Each flow tab now says where its run stands in the repository, under its
  name: "running · 3/8", "needs you · Merge", "failed · Open pull request",
  "done". Following a pull request through Commit → PR and Review → Merge no
  longer means opening each tab to find out where it got to.
- When a flow other than the one on screen needs you — waiting for an
  approval, failed, or running — a line under the tabs says which, for which
  pull request and at which step, with a **Show** button that goes straight
  there.

## [0.1.88] - 2026-10-08

### Added

- The Run view shows the pull request you are validating, as the list view
  does: under the flow tabs, the review flow lists every open pull request as
  a card along one row, with its size and checks, to pick which one to review;
  other flows show the checked-out branch's pull request. A flow that cannot
  run says why there too.
- A **Diff** button under that pull request's card opens its diff in the
  panel beside the map, with which branch it merges into which and how much
  it changes — fetched with git, so reading it costs none of your GitHub API
  budget, and it works the same for Azure DevOps.

## [0.1.87] - 2026-10-08

### Added

- The Run view's step panel folds: › next to ✕ shrinks it to a strip at the
  right edge with the step's name and state, and the map takes the room back.
  Click the strip to unfold it. It stays folded until you do — for the next
  step you open, and after a restart — so a step that comes to need you shows
  on the strip instead of covering the map again.

### Fixed

- With **Trust all** on, Play and Start run the flow you have selected. Before,
  they started whichever flow the repository needed most — usually Commit →
  PR — whatever tab you were on. It still runs trusted and carries on to the
  next flow afterwards; only the **Trusted run** button picks the flow for you.

## [0.1.86] - 2026-10-08

### Changed

- In the Run view, a step that fails opens its panel on its own too, with the
  error and its Retry, Skip and Cancel buttons — even in a trusted run, which
  stops at a failure. It stays open until you retry, skip or close it.
- The Run view's step panel slides in and out smoothly, and the map moves
  aside and back with it, instead of the panel vanishing the moment it closes.

### Fixed

- GitAgent no longer runs your GitHub account out of its hourly API budget.
  Checking each repository asked GitHub for every commit of every open pull
  request, which cost about fifteen times more than everything else it needs,
  so a few launches checking a few dozen repositories used up the hour — and
  then every `gh` call failed with "API rate limit already exceeded", in
  GitAgent and anything else on the account. Checks now cost a fraction of
  that, and the home screen and the folder you open from it share their
  results for two minutes instead of checking the same repositories twice.
  Pull request cards no longer show a commit count.

## [0.1.85] - 2026-10-08

### Added

- A List / Run switch in the top bar. Run, now what GitAgent opens on, replaces
  the step list and detail pane with the selected repository's flow drawn as a
  line map, with the repository list still on the left and the same flow tabs as
  the list above it, to switch flow or choose which are shown. Each track fills
  toward the next step as the run reaches it: blue while that step is working,
  green once it's done, orange if you rejected it. A dot rides the front of each
  track and waits just short of a step that's still working or waiting for you,
  then runs in when it finishes. Steps that run side by side fill in parallel.
  Click a step, its pill or its reviewer and the step opens in a panel from the
  right, with what it will do and the same Approve, Reject, Skip and Retry
  buttons as in the list. A step that stops for your approval opens it on its
  own — including a merge a trusted run held back for you — once: close it and
  it stays closed until the next approval. Approving or skipping closes it, as
  do Escape and ✕. The map slides left so the step stays in view beside the
  panel, and the play controls stay within reach, so a run can be followed and
  answered without leaving the map.
- In the Run view, a step you reject gets an orange arc back over its track,
  marked "sent back". Retry it and the arc stays, counting rounds, until it
  reads "approved in round 3" — so a run that needed a few tries shows it.
- In the Run view, the Run tests step shows a progress bar per kind of test
  — unit, integration, end-to-end, doc — counting up as they pass, for
  projects tested with `cargo test`. With `pytest` there is one bar for the
  whole suite. Other test runners don't print a total to count against, so
  they show none.
- Along the bottom of the Run view, every step is a numbered pill in the
  order the map reads, coloured by where it's got to — done, running, waiting
  for you, failed or sent back. Click one to open that step.
- The Run view has Play, Pause, Restart and speed controls. Before anything
  has run, Play starts the flow, just like Start in the list view, stopping
  for you at every approval. Once it's running, Pause freezes the map where
  it is while the run itself carries on, and Play catches up step by step.
  Restart replays the whole run from its first step, even one that finished
  while you were in another view, at 0.5×, 1× or 2×.
- For flows with a pull request in them, the Run view shows two checkboxes
  by the end of the line: "PR ready" ticks once the pull request exists and
  names it, and "CI green" ticks when its checks pass — or shows them still
  running, failing, or not set up.
- The review flow's Run view has a review circle under the map, with a spoke
  per reviewer: you reading the diff, the model's analysis with its verdict
  and number of findings, and the CI checks. Once the review is sent back, an
  orange Fix spoke appears, and it stays to show the fix was reviewed again.
  Click a reviewer to open its step.
- A second reviewer: choose a second model under Model provider → Second
  reviewer, and the review flow has every pull request reviewed again by it,
  beside the first review. Its verdict shows at the merge under the first,
  holds a trusted run just as the first review's findings do, and appears in
  the review circle under the model's own name. Off by default — it is one
  more model call per review — and with none chosen the step is skipped
  without holding up the merge. Saved review flows get the step once; delete
  it and it stays deleted.
- Focused reviews: turn on Alignment (does the change do what its title
  says, and nothing unrelated), Security (vulnerabilities it introduces) or
  Architecture (layering, coupling, logic in the wrong place) under Model
  provider → Focused reviews, and the review flow checks every pull request
  for each with the main model. Each finding has to quote the diff, like the
  regression review's. Results show at the merge, hold a trusted run, and
  each gets its own spoke in the review circle — saying where to turn it on
  while it's off. All off by default: each is one more model call per
  review.

### Changed

- GitAgent opens on the first repository in the list that has something left
  to do, rather than the most urgent one further down — so it starts where
  you would start reading.

## [0.1.84] - 2026-10-07

### Added

- When open pull requests can't be listed because the `az` or `gh` CLI is
  missing, you're signed out of Azure, or the azure-devops extension isn't
  installed, the Review panel now offers a button that fixes it: install the
  CLI, sign in with `az login`, or add the extension. Preflight offers the
  same install and `az login` fixes.

### Fixed

- On Windows, GitAgent now finds the Azure CLI. It reported "program not
  found" for `az` even when it worked in every terminal, because it only
  looked for `.exe` programs and the Azure CLI installs as `az.cmd`.
- On Windows, a CLI installed while GitAgent is open is found without
  restarting the app.
- A missing CLI is now reported as "not installed", with the command that
  installs it, instead of a bare "program not found".

## [0.1.83] - 2026-10-07

### Changed

- The copyright and branding information have been updated to reflect new
  ownership. The application now displays 'Mayorana' as the company name and
  includes the new copyright year.

## [0.1.82] - 2026-10-06

### Fixed

- **Back to base** no longer fails with "already used by worktree" after a
  merge done from a second worktree of a repository. The base branch is
  brought up to date in the worktree that has it checked out, and the one you
  merged from stays on its branch.

## [0.1.80] - 2026-10-06

### Fixed

- A model step such as **Draft commit message** could sit at "running" with
  an empty log until GitAgent was restarted, when an earlier step had kept the
  local model without ever finishing. Such a step is now passed over after a
  minute, and the step that was waiting goes ahead.
- **Cancel run** now really stops the run. Before, a cancelled run carried on
  out of sight, still calling the model and still able to change the run you
  started next.
- Commit messages, pull request descriptions and reviews no longer assume
  every repository is a Rust desktop application.

### Changed

- While a step waits on the model, its log says so: which step in which
  repository has the model and for how long, then that the request was sent
  and how long the answer is taking. A slow answer no longer looks like a hung
  step.

## [0.1.79] - 2026-10-05

### Added

- A **Refresh** button next to "Recent" on the first screen checks every
  repository in your folders again — including ones cloned since — so the
  waiting work shown for each folder is current without restarting GitAgent.

## [0.1.78] - 2026-10-03

### Added

- The home screen checks the repositories in each recent folder and says what
  is waiting before you open it — uncommitted work, a pull request ready to
  merge or failing its checks, a release due — with a count for each. Hover a
  label to see which repositories it means.

## [0.1.77] - 2026-10-02

### Added

- The diff a step shows has the same **Copy** button in its top-right corner,
  so the whole diff can go on the clipboard in one click.

### Changed

- A step's output log is taller and follows new lines as they arrive, so a
  long-running command no longer needs scrolling to watch. Scroll up to read
  something and it stays where you left it until you scroll back down.

## [0.1.76] - 2026-10-02

### Added

- In the Branches panel, a branch with an open pull request has a **Review &
  merge** button that opens Review → Merge on that pull request.

### Changed

- More finished-with branches are marked "nothing new" with **Clean up**: a
  branch with no commits the base branch lacks, and one still exactly at what
  its merged pull request merged — including one named `main` in a repository
  whose base is `master`. A branch with an open pull request is never offered
  for clean-up.

## [0.1.75] - 2026-10-02

### Changed

- Packaging only — no user-visible change.

## [0.1.74] - 2026-10-01

### Added

- A **Copy** button in the top-right corner of a step's output or error log
  puts the whole log on the clipboard, so you can paste it into an issue or a
  chat without selecting it by hand.

## [0.1.73] - 2026-10-01

### Changed

- Only one flow runs in a repository at a time. While a commit is under way,
  merging a pull request in the same repository waits until it is done, and the
  other way round; before, both could start and step on each other's branch
  switches and pulls in the same folder. The **Branches** panel follows the
  same rule: deleting, cleaning up or opening a pull request for a branch waits
  for a running flow, and no flow starts until that branch is done. Hover the
  greyed-out button to see what is in the way. Other repositories are
  unaffected.
- **Delete all merged** in the **Branches** panel now deletes the branches one
  after another instead of all at once, and lists every branch it could not
  delete rather than only the last one.

## [0.1.72] - 2026-10-01

### Added

- GitAgent now shares anonymous usage statistics, and it is on by default. A
  note at the bottom of the window tells you once, and nothing is sent until
  you have seen it. **Turn off** there, or **Settings** at any time, stops it
  for good. What is shared: which steps ran and
  whether they succeeded, your operating system, the GitAgent version, and
  whether the repository is on GitHub or Azure DevOps. What is never shared:
  repository names, folders, code, commit messages, branch names or error
  text. It is tied to a random number kept on your computer, not to you, and
  turning it off deletes anything that has not been sent yet. It also stays off
  if `DISABLE_UPDATE_CHECK`, `DO_NOT_TRACK` or `GITAGENT_NO_TELEMETRY` is set.

### Fixed

- Setting `DISABLE_UPDATE_CHECK` in your shell profile now also applies when
  GitAgent is opened from the Dock or Finder, not only from a terminal.
- A release no longer runs on an out-of-date copy of the branch. If a pull
  request was merged on GitHub and not yet pulled, the release used to see no
  new commits and no notes, and offered to ship build-only, which would have
  released that change with no notes. GitAgent now pulls those commits first,
  before drafting notes or running your release script. If it can't pull them
  safely, it stops and offers the pull on a button.
- When a release is held up for missing notes while origin has commits this
  copy lacks, GitAgent offers to pull them and release again, instead of
  offering a build-only release.
- When your release script commits and tags a version but its push is
  rejected, GitAgent now explains that nothing reached origin and offers one
  button that undoes the local release commit and tag, pulls, and releases
  again. Before, only Retry, Skip and Cancel were offered, and retrying could
  not succeed.
- "Back to base" no longer stops with "cannot lock ref" when another program,
  such as your editor's background fetch, updates the same branch at the same
  moment. GitAgent tries again on its own. If the other program is still busy,
  GitAgent explains that the repository is fine and offers to fetch again on a
  button, instead of "No automatic fix for this".

## [0.1.71] - 2026-09-30

### Changed

- The window's title bar says when GitAgent Pro is active — "GitAgent ·
  version · ✦ Pro" — and "Pro updates ended" for a licence whose updates have
  run out.
  It updates as soon as a licence is added or removed.

## [0.1.70] - 2026-09-29

### Changed

- Packaging only — no user-visible change.

## [0.1.69] - 2026-09-29

### Changed

- Packaging only — no user-visible change.

## [0.1.68] - 2026-09-29

### Changed

- With GitAgent Pro active, the top-right corner of the window shows a **✦ PRO**
  badge instead of a plain "Pro ✓" button. Hovering it shows who the licence
  is for and until when updates are included; clicking it still opens the
  licence window. A licence whose updates have ended shows **✦ RENEW PRO**.

## [0.1.67] - 2026-09-29

### Changed

- Without GitAgent Pro, GitAgent now works with 5 repositories in all, across
  every folder and every window. The first repositories listed take the 5;
  every other one is shown greyed out with a **🔒 Pro** tag, isn't checked,
  and can't be selected, so no flow or branch action can reach it. Clicking a
  locked repository opens the Pro window, where it can take the place of one
  of your 5, or where you paste your licence key to unlock them all.

## [0.1.66] - 2026-09-29

### Changed

- The refresh button at the top of the repository list is bigger and tinted
  green, so the check you run most often is the easiest one to hit. Its icon
  turns while the repositories are being re-checked.

### Fixed

- **Bring the base in**, offered when a pull request cannot be merged, now
  finishes the job when the base merges in cleanly: it pushes the branch, waits
  for GitHub to recheck the pull request, and asks to merge again. Before, a
  clean merge ended the run without a word and without pushing, so the pull
  request still showed as conflicting. A real conflict still stops before
  anything is pushed, with the files to fix marked.

## [0.1.65] - 2026-09-29

### Changed

- Packaging only — no user-visible change.

## [0.1.64] - 2026-09-29

### Changed

- A flow created in Setup is shown only on the repository you opened Setup
  from, instead of on every repository. In the other repositories the flow
  list button counts it as "1 more", and the list offers it as "only on other
  repositories — tick to add here". Setup says where each flow is shown, with **Show on every
  repository** for a flow meant for all of them, and **Only on** to restrict
  an existing one — which also covers repositories cloned later, unlike hiding
  it one repository at a time.

### Fixed

- When a pull request cannot be merged because it conflicts with the base
  branch, **Bring the base in** now switches to the pull request's own branch
  before merging the base into it. Before, it merged into whichever branch
  happened to be checked out. The button no longer reads "Abandon", and the
  message lists every step from there to a merged pull request.

## [0.1.63] - 2026-09-29

### Added

- GitAgent Pro. The free version now runs flows in 5 repositories of your
  choice: the first run in a repository takes one of the 5, and **Get Pro…**
  in the top bar shows which ones and lets you give one back. Every repository
  stays listed and checked as before; once the 5 are taken, the others show a
  **Pro** tag and a run there opens the Pro window instead of starting. Paste
  the licence key from your purchase email in the same window to run flows in
  every repository. The key is checked on your computer, with no account and
  nothing sent anywhere.

## [0.1.62] - 2026-09-27

### Added

- The Branches panel spots branches that are ahead of the base branch but
  would change no file if merged — old merge commits, or work the base already
  has from a squash merge — and marks them "nothing new". A Clean up button
  closes the branch's open pull request and deletes it on GitHub and locally,
  after a confirmation. Before, such a branch was listed as needing a pull
  request, and one named `main` offered no action at all.

## [0.1.61] - 2026-09-27

### Fixed

- Refresh in the repository list now looks at the folder again: a repository
  cloned into it since it was opened appears, and one deleted or moved away
  disappears. Before, it only re-checked the repositories already listed, and
  a new one needed the folder to be reopened.
- Committing in a repository with no commits yet — straight after `git init` —
  works. Scan changes used to fail with "ambiguous argument 'HEAD'". The first
  commit now goes on the branch you are on and is pushed as that branch, with
  no pull request, since the remote has nothing yet to open one against.

## [0.1.60] - 2026-09-27

### Added

- The update check now considers preferred formats for each operating system,
  ensuring a more tailored download experience.

## [0.1.59] - 2026-09-27

### Changed

- Packaging only — no user-visible change.

## [0.1.58] - 2026-09-27

### Fixed

- A flow's steps are listed in the order they run. Steps added in Setup in
  front of an existing one used to appear at the bottom of the run view, after
  the step they come before, even though Setup showed them in the right place.

## [0.1.57] - 2026-09-27

### Fixed

- A large change no longer makes a local model lose its instructions and
  run until the 15-minute timeout. What a step sends is now sized to the
  model's context window, and every answer has a length limit, with a local
  model or an API key alike.
- When a diff is too large, whole files are left out rather than the diff
  being cut mid-file: lock files first, then docs and CI, then the largest
  code files. The step's log names what the model did not see.
- The context window in Settings now shows how much of a diff it allows.
  Lower it for faster answers on large changes.

## [0.1.56] - 2026-09-26

### Fixed

- A push rejected because GitHub still has a branch of the same name from an
  already merged or closed pull request now says so, and offers to push under
  a new name. The pull request then opens under that name too. Before, the
  only fix offered was pulling the old branch in, which mixed its commits into
  your change.

## [0.1.55] - 2026-09-25

### Added

- A release with no notes in CHANGELOG.md no longer stops at the script. The
  model drafts the notes from what changed since the last release, and you
  read and approve them before they are added and committed. A release that
  only touched CI, scripts or docs gets a "no user-visible change" line
  without asking the model. Flows that run `release.sh` get the two new steps
  added in front of it once; remove them in Setup if you don't want them.

### Fixed

- A commit no longer lands on a branch named like one still on GitHub from an
  earlier, merged pull request. The push used to be rejected as "behind its
  remote counterpart", with pulling the old branch in as the only offered fix.

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
