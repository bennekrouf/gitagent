# GitAgent skill

A skill for AI agents that use **GitAgent**, the desktop app that takes git repositories from uncommitted changes to an open pull request, a merge and a release, with your approval at every step that touches history or the remote. The
skill tells the agent how to use GitAgent's tools well: which to call first, what
needs your yes, sensible defaults, and how to report back.

## What you need

- **GitAgent**, installed from <https://mayorana.ch/en/apps/gitagent> (macOS,
  Windows or Linux). This skill does not include the app.
- A desktop agent connected to GitAgent's MCP server: Claude Desktop, Claude
  Code, Codex CLI or Cursor. `references/connect.md` has the steps. Web-only
  agents such as ChatGPT can't reach an app on your computer.

## What you can ask for

- Run Commit → PR, Review → Merge and Release flows across a folder of repositories
- Walk you through each approval and what a fix button will do
- Explain a failed step from its log and the repository's state
- Edit flows (a Deploy step over ssh, for example) as Setup does
- Draft a scrubbed bug report you send yourself

## What waits for your yes

- Every commit, push, merge and release (GitAgent's own approvals)
- Turning on a trusted run, or saving a change to a flow

## Limits

GitAgent works in every repository without a licence; GitAgent Pro adds monthly AI credits for the api0 model.

## Install

- **Claude Code**: unzip into `~/.claude/skills/` so the folder is
  `~/.claude/skills/gitagent/`.
- **claude.ai or Claude Desktop**: upload the zip under Skills in Settings.
- **Other agents**: put the `gitagent` folder where the agent reads skills.

## Help

Questions, licences and bug reports: <https://mayorana.ch/en/contact>.
