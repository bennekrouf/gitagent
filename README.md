# GitAgent

An agentic graph for commit and deploy workflows. Desktop app, Dioxus, same
shape as the rest of the `ais-*` family.

Version one runs one hardcoded flow — turn a dirty working tree into an open
pull request — but the engine underneath it is not hardcoded, and that is the
point. Making the app generic means replacing `services/flow.rs`, not
rewriting anything else.

## The flow

```text
                           ┌──> commit ──> push ──┐
  scan ──> draft_commit ──>┤                      ├──> open_pr
                           └──> draft_pr ─────────┘
```

Six nodes. Three are deterministic shell work, two are model calls, and three
are gated on a human saying yes.

`draft_pr` reads the diff and the commit message; it never reads anything
`commit` or `push` produce, so there is no edge between them. The executor
still runs one node at a time, but the dependency is honest — running the ready
set concurrently is a change to `drive()` alone.

| Node | Kind | Gated | Writes |
|---|---|---|---|
| `scan` | code | | `branch` `base` `stat` `diff` `commit_paths` `untracked` |
| `draft_commit` | model | | `branch_name` `commit_subject` `commit_body` |
| `commit` | code | ✓ | `work_branch` `commit_sha` |
| `draft_pr` | model | | `pr_title` `pr_body` |
| `push` | code | ✓ | `push_output` |
| `open_pr` | code | ✓ | `pr_url` |

## Approvals

Nothing touches git history or the remote without an explicit yes. A gated node
parks in `AwaitingApproval` and renders the exact commands it will run —
the real `git add -- …` path list, the real commit message, the real
`gh pr create` arguments. Reject blocks everything downstream of it and the run
ends cleanly.

Two consequences worth knowing:

* Only **tracked** changes are committed. Untracked files are listed in the
  scan output and deliberately left alone.
* The file list shown at the approval step is the exact argument list passed to
  `git add`. There is no `git add -A` anywhere in the codebase.

## Model providers

Local **ollama** or remote **DeepSeek**, switchable in Settings.

* ollama sends an explicit `num_ctx` (default 16384). ollama's own default is
  4096 whatever the model claims to support, which truncates a real diff
  without telling you.
* DeepSeek is OpenAI-compatible, so that client also covers OpenAI, vLLM,
  LM Studio and OpenRouter later — only the base URL and model change.
* Both run at `temperature: 0`. The same diff should produce the same commit
  message twice, or approving one is meaningless.

The DeepSeek key is read from `DEEPSEEK_API_KEY` at call time and is never
written to disk:

```bash
export DEEPSEEK_API_KEY=sk-...
```

## Running it

```bash
cargo run
```

Add a repository, press **Start run**, approve or reject each gated step.

State lives in `~/Library/Application Support/gitagent/` — `repos.json` for the
registry, `settings.json` for the model config.

## Layout

```
src/services/graph.rs   engine: nodes, edges, state, scheduling. No IO.
src/services/flow.rs    the hardcoded flow + what each node does
src/services/llm.rs     ollama and DeepSeek behind one complete_json()
src/services/git.rs     git and gh as subprocesses
src/services/store.rs   registry and settings on disk
src/screens/            welcome (repo list), run_screen (executor + layout)
src/components/         node_card, detail_pane, settings_panel
```

`graph.rs` knows nothing about git, models, or this flow in particular. That
separation is what the generic version depends on.
