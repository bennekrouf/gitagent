---
name: gitagent
description: Help someone use GitAgent, the desktop app that takes git repositories from uncommitted changes to an open pull request, a merge and a release, with an approval at every step that touches history or the remote. Use this whenever the user mentions GitAgent, or is using it and asks about flows (Commit → PR, Review → Merge, Release), approvals, trusted runs, the Branches panel, Setup or flows.toml, the model that drafts commit messages (Ollama, DeepSeek, OpenAI and other APIs), GitHub or Azure DevOps connections, GitAgent Pro, or a step that failed with a fix offered on a button. Also use it when they want to report a GitAgent bug or ask the author for help.
---

# GitAgent

GitAgent is a desktop app (macOS, Windows, Linux) that runs git work as a
graph of steps: scan the changes, draft a commit message with a model, commit,
push, open the pull request, review and merge it, release. Every step that
touches git history or the remote waits for the user to approve it, and shows
the exact commands it will run before it does.

The person you are helping is a developer using it on their own repositories,
often client work. When you can, look at the repository itself (`git status`,
`git branch -vv`, `git log --oneline -5`, `git remote -v`): what state it is
really in usually explains what GitAgent shows, and is faster than guessing
from a description.

## How it is organised

- **Folder of repositories.** GitAgent opens a folder; every git repository
  directly inside it is listed in the sidebar, with what each one needs next:
  changes to commit, a branch to push, an open pull request, a release due, an
  unfinished rebase or merge. **Refresh** re-reads the folder.
- **Flows** are sequences of steps. The shipped ones:
  - **Commit → PR**: scan changes, draft the commit message, run tests, commit
    (on a new branch if needed), push, draft and open the pull request.
  - **Review → Merge**: find the pull request, fetch its diff, analyse it with
    the model, check CI and mergeability, merge (squash, delete the branch),
    back to the base branch.
  - **Release**: preflight, draft release notes into `CHANGELOG.md` if the
    project has none for this version, then run the project's release script.
  Users can add their own (for example a "Deploy" flow that runs a command over
  ssh) in **Setup**.
- **Approvals.** A gated step stops and shows what it will do: the exact
  `git add` file list, the commit message, the `gh pr create` arguments. The
  user approves, skips or rejects it. Only tracked files are committed;
  untracked files are listed and left alone, and there is no `git add -A`.
- **Trusted runs.** **Trusted run** answers approvals automatically and works
  through the repository (commit, then review, then release), stopping only
  where a person is needed, and saying why. **Stop asking me** on an approval
  turns a run already started into a trusted run. **Trust all** in the top bar
  does it for every repository until turned off. A trusted run still stops at
  a merge that the analysis or CI is unhappy about.
- **Branches panel**: every branch with its state. Open a pull request for it,
  **Review & merge** one that has a pull request, delete merged branches, and
  **Clean up** branches marked "nothing new" (no change the base lacks).
- **One flow per repository at a time.** A flow or branch action waits for
  the one running in that repository; hover a greyed-out button to see what's
  in the way.

## Setup and settings

- **First run** asks what writes commit messages: a model on this machine
  (Ollama), a hosted API (DeepSeek, OpenAI, Mistral, Cohere, Groq, OpenRouter,
  or any OpenAI-compatible endpoint), or **no model**, in which case GitAgent
  builds a plain message from the changed files. It can be changed any time in
  **Settings**.
- **API keys** are read from the environment (`DEEPSEEK_API_KEY`,
  `OPENAI_API_KEY`, `MISTRAL_API_KEY`, `COHERE_API_KEY`, `GROQ_API_KEY`,
  `OPENROUTER_API_KEY`) or typed in for the session. They are never written to
  disk. To keep one between launches, export it in the shell profile.
- **Context window** (Settings): how much of a diff the model is sent. Lower
  is faster on large changes; when a diff doesn't fit, whole files are left
  out (lock files first, then docs and CI, then the largest code files) and
  the step's log names them.
- **Setup** edits the flows, stored in `flows.toml`. Edits are a draft until
  **Save**. A new flow shows only on the repository Setup was opened from;
  **Show on every repository** or **Only on** changes that.
- **Forges.** GitHub through the `gh` CLI (`gh auth login`); Azure DevOps
  through `az` with the `azure-devops` extension (`az login`,
  `az extension add --name azure-devops`). GitAgent uses whatever account
  those tools are signed in with.
- **GitAgent Pro.** The free version runs flows in 5 repositories; others show
  a **🔒 Pro** tag. **Get Pro…** shows which 5 are in use and lets one be given
  back. A licence key is pasted in the same window and checked offline.
- **Usage statistics** are anonymous and on by default (steps run and whether
  they succeeded, OS, version, GitHub or Azure DevOps; never names, code,
  commit messages or errors). Turn off in Settings, or set `DO_NOT_TRACK`,
  `GITAGENT_NO_TELEMETRY` or `DISABLE_UPDATE_CHECK`.

Settings and the repository list live in the app's data folder:
`~/Library/Application Support/gitagent/` (macOS), `%LOCALAPPDATA%\gitagent\`
(Windows), `~/.local/share/gitagent/` (Linux). Release notes for every
version: <https://mayorana.ch/en/apps/gitagent/releases>.

## When a step fails

GitAgent usually explains a failure and offers a fix on a button (pull first,
push under a new name, bring the base in, finish or abandon a rebase). Those
fixes are usually right; help the user understand what each one will do
before they click it, especially anything that rewrites or discards work.

1. **Get the step's log.** The **Copy** button in the corner of a step's
   output copies all of it. Ask for that rather than a paraphrase.
2. **Look at the repository** if you can (commands above). Many failures are
   the repository being in a state the user didn't expect: an unfinished
   rebase, a branch behind its remote, a stale branch on GitHub with the same
   name.
3. **Look up the symptom** in `references/troubleshooting.md`, which lists
   the known failures with their cause and the fix GitAgent offers.
4. If it's still unexplained, it may be a GitAgent bug. Check the version in
   the window title first (`GitAgent 0.1.76`): the release notes say which
   version fixed what. Then offer to draft a report (below).

Never suggest getting around an approval by running the git commands in a
terminal behind GitAgent's back while a flow is mid-way: the flow then works
from a state that no longer exists. Cancel the run first, or let it finish.

## Reporting a problem to the author

When the problem looks like a GitAgent bug, or the user wants to send
feedback, help them write a report they can send. The user sends it, not you:
never create an issue, send an email or submit a form on their behalf.

Step logs from client repositories can contain repository and branch names,
file paths, commit messages, diff content, organisation or project names and
tokens. Before showing the draft, replace anything like that with neutral
placeholders (`<repo>`, `<branch>`, `<org>`, `<file>`), then tell the user
what you replaced and ask them to check the rest. Keep only the log lines that
show the failure.

Use this structure:

~~~markdown
**GitAgent version:** 0.1.76
**OS:** macOS 15.1 / Windows 11 / Ubuntu 24.04
**Forge:** GitHub / Azure DevOps
**Model:** Ollama <model> / <hosted API> / none

**Flow and step**
Commit → PR, step "Push branch"

**What I did**
1. …

**What I expected**
…

**What happened instead**
…

**Step log** (from the step's Copy button)
```
…
```

**Workaround found, if any**
…
~~~

Then give them the two ways to send it:

- A GitHub issue at <https://github.com/bennekrouf/gitagent/issues/new>:
  they paste the title and body and submit it themselves. Issues there are
  public, which is one more reason the draft must be scrubbed.
- The contact form at <https://mayorana.ch/en/contact>, for anything they
  would rather not post publicly, and for licence questions.
