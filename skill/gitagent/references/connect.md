# Connecting GitAgent to an agent

GitAgent is a desktop app. Its MCP server is the app itself, started with one
argument, `mcp`: it talks over stdio and opens no window. The agent starts it
when it needs it; the app does not have to be open (some features need it
open, as the skill says).

The **MCP** button in GitAgent's window shows the exact config and commands below
with the real path to the app on this computer. Prefer copying from there.
Below, `<app>` stands for that path.

Do not edit these files or run these commands for the user without asking:
they change the agent's configuration.

## Claude Desktop

Settings → Developer → Edit Config opens `claude_desktop_config.json`
(`~/Library/Application Support/Claude/` on macOS, `%APPDATA%\Claude\` on
Windows, `~/.config/Claude/` on Linux). Add the entry, save, then quit Claude
and open it again:

```json
{
  "mcpServers": {
    "gitagent": { "command": "<app>", "args": ["mcp"] }
  }
}
```

If the file already has an `mcpServers` section, add only the `"gitagent"` entry
to it.

## Claude Code

```bash
claude mcp add gitagent --scope user -- "<app>" mcp
```

## Codex CLI

```bash
codex mcp add gitagent -- "<app>" mcp
```

## Cursor and other MCP clients

Any client that starts a local stdio server works with the same command and
argument. In Cursor, add the same `mcpServers` entry as for Claude Desktop to
`~/.cursor/mcp.json` (all projects) or `.cursor/mcp.json` (one project).

## Agents that only reach remote servers

ChatGPT and other web-only agents cannot start a program on the user's
computer, so they cannot use GitAgent's tools. Tell the user this skill needs a
desktop agent (Claude Desktop, Claude Code, Codex CLI, Cursor) on the same
computer as GitAgent.

## Checking the connection

After restarting the client, ask it to list its tools: the `gitagent` server's
tools should be there. If they are missing:

- The path is wrong (the app was moved or reinstalled): copy it again from
  the **MCP** button.
- On macOS, the app was never opened once: open it from Applications first
  so macOS lets it run.
- The client was not fully restarted: quit it, not just close the window.
