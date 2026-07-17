# Torii

A terminal workspace for coding-agent sessions powered by the Pi SDK, with
interaction and visual design inspired by Grok Build CLI.

<div align="center">
<img width="725" height="582" alt="screenshot-20260713-145028" src="https://github.com/user-attachments/assets/1e39cff2-1f51-461d-a70c-a2b72b489008" />
</div>

## Features

- Persistent sessions with resume, fork, clone, rename, delete, tree navigation, compaction, import, and export.
- Streaming Markdown responses, reasoning, tool activity, diffs, usage, and subagent transcripts.
- Searchable model, command, session, file, settings, and history pickers.
- Permission prompts for tool execution and project trust controls.
- Editable multiline paste blocks with mouse-based cursor positioning.
- Clipboard text and multiple image attachments via `/paste`, keyboard shortcuts, or the `@` file picker.
- Clickable image previews with aspect-ratio-preserving terminal rendering.
- Non-blocking clipboard image decoding with processing, success, and error states.
- Project file references, shell commands, prompt history, plans, MCP tools, and package management.
- Durable multi-agent workflows with frozen model/role routing, checkpoints, parallel read-only review, compact artifacts, and resume recovery.
- On-demand MCP tool discovery that grows the active tool set monotonically and restores it on session resume.
- Pi-authoritative context usage and working state, including after session resume
  and compaction.
- Searchable Pi extension controls that show resolved source and scope and
  persist enable or disable choices.
- A paged session dashboard with live resident state and background OTA status.
- Interactive and headless operation.

See [Workflow architecture](docs/workflows.md) for authoring, multi-model
routing, context and cache policies, and `/resume` behavior.

## Screenshots
<img width="1337" height="896" alt="screenshot-20260713-185523" src="https://github.com/user-attachments/assets/3512820a-733a-4439-95c0-065cf89da725" />
<img width="930" height="590" alt="screenshot-20260713-075616" src="https://github.com/user-attachments/assets/4c43ef20-b87c-4ea0-88a0-ca70d1f01506" />


## Install

Prebuilt releases currently support Linux x86-64 and Windows x86-64. See the
[latest release](https://github.com/fahmiirsyadk/torii/releases/latest) for
archives and SHA-256 checksums.

Linux x86-64:

```bash
curl --proto '=https' --tlsv1.2 -fsSL \
  https://raw.githubusercontent.com/fahmiirsyadk/torii/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/fahmiirsyadk/torii/main/install.ps1 | iex
```

The installer verifies the platform release digest and installs a stable
launcher plus a versioned Rust host and compiled SDK sidecar. Node.js, npm,
Bun, and the Rust toolchain are not runtime requirements.

Provider authentication:

```bash
torii login
torii login <provider>
torii logout <provider>
```

Other platforms can build from source. Install the sidecar dependencies with
`npm ci --prefix sidecar`, then run `cargo build --release --locked -p torii`.
Source-tree execution requires Node.js for the TypeScript SDK sidecar.

## Arguments

```text
--backend <mock|pi>       Select the backend. Default: pi
--model <provider/model>  Select a model before opening the session
--headless                Print JSONL events without opening the TUI
--prompt <text>           Prompt used in headless mode
--check-pi                Validate the Pi sidecar, resources, and model access

-c, --continue            Resume the latest session for the current directory
-r, --resume              Open the session picker at startup
--session <path-or-id>    Resume a specific session
--fork <path-or-id>       Fork a specific session
--no-session              Use an in-memory session

--story <name>            Open a deterministic UI story
```

Session persistence arguments are mutually exclusive. `--headless` with the Pi
backend sends a model request; `--check-pi` does not.

Package commands are forwarded to the bundled Pi CLI:

```text
torii install <package>
torii remove <package>
torii uninstall <package>
torii update [package]
torii list
torii config
```

Torii checks GitHub's latest stable release in the background at most once
every 24 hours. The dashboard reports available, downloading, ready, failed,
and rollback states. Updates are downloaded, size- and digest-verified,
unpacked into a new version directory, and validated against the bundled Pi
sidecar before activation:

```text
torii self check
torii self update
torii self version
```

The running process is never replaced. A verified update becomes active on the
next launch through an atomic version-pointer replacement. The launcher health
checks that pending version and automatically restores the previous version if
startup validation fails.

## Usage tutorial

### Start, continue, and resume sessions

Start Torii in the current repository:

```bash
torii
```

Pi is the default backend; use `--backend mock` only for deterministic local
testing. The welcome screen lists saved sessions and their live state. Use
`Up`/`Down` to select one, `PageUp`/`PageDown` to move by a page,
`Home`/`End` to jump to either boundary, `Enter` to resume, or `n` to start a
new session. From the transcript, `/home` returns to this screen and `/resume`
opens the searchable session picker. You can also resume directly from the
shell:

```bash
torii --continue
torii --session <path-or-id>
```

During a running turn, `Enter` queues a follow-up for the next turn and
`Ctrl+Enter` sends an immediate steering message. `Ctrl+C` clears a non-empty
draft first; with an empty composer it cancels the active turn. `Ctrl+P` opens
the command palette, `Ctrl+L` changes model, and `Ctrl+B` opens the task list.

### Manage Pi extensions

Open settings with `F2` or `/settings`, then select **Pi extensions**. The
searchable list shows every extension resolved by Pi, including disabled
entries, its source, and whether it belongs to user or project configuration.
Press `Space` or `Enter` to enable or disable the selected extension.

Changes are written through Pi's package configuration and the active session
is reloaded before the checkbox changes. Temporary extensions injected into the
current process are shown for visibility but cannot be disabled because they
have no persistent configuration entry.

### Delegate an ad-hoc task to a subagent

Ask the parent agent to delegate a bounded task. State the role, capability,
and whether it should work in an isolated Git worktree:

```text
Use a read-only explore subagent in the background to inspect the authentication
flow and report the relevant files. Do not modify anything.
```

For independent implementation work:

```text
Start a general-purpose subagent with read-write capability in an isolated
worktree. Implement the parser fix and run its focused tests. Do not apply the
worktree until I approve the result.
```

Torii gives each subagent its own context and persisted transcript. Background
tasks return a task ID and do not bloat the parent transcript with the entire
child conversation. Open the task dashboard with `Ctrl+B`, select a task with
`Up`/`Down`, press `Enter` to inspect its transcript, or press `k` to cancel it.

An isolated worktree is never merged or removed automatically. Inspect and apply
or discard it with ordinary Git commands. Subagents cannot spawn nested
subagents. Use a workflow when work needs explicit dependencies or checkpoints.

Choose the default subagent model with `/subagent-model`. The setting is separate
from the parent model and persists across sessions.

### Run a built-in workflow

Torii ships with three workflows:

- `production-change` for connector evidence, approval, implementation,
  independent reviews, conditional repair, and final verification.
- `implement-review` for plan, approval, implementation, review, and repair.
- `review` for parallel correctness, security, and test review followed by
  synthesis.

First validate and inspect the dependency graph:

```text
/workflow
/workflow check implement-review
```

Then launch a background run with a complete root task:

```text
/workflow implement-review Add pagination to the session picker and verify it
at narrow and wide terminal sizes.
```

Open `/workflows` to monitor runs. In the dashboard:

- `Up`/`Down` selects a run.
- `a` approves a waiting checkpoint; `d` rejects it.
- `r` explicitly retries a failed or interrupted step.
- `x` cancels an active run.
- `v` or `Enter` opens the latest bounded artifact.
- `Esc` returns to the transcript.

Workflows are scheduled and persisted by Rust rather than by the parent model or
TypeScript sidecar. Independent ready steps run concurrently, while every pair
of write-capable steps must be ordered by a dependency path. Interrupted work
fails closed and requires an explicit retry.

### Add a project workflow

Copy the dependency-graph example into a trusted project's workflow directory:

```bash
mkdir -p .pi/workflows
cp examples/workflows/focused-review.yaml .pi/workflows/
```

Project definitions are ignored until the project is trusted. Parallelism comes
from dependency readiness; there is no nested parallel syntax or policy DSL.

See [Workflow architecture](docs/workflows.md) and the
[`examples/workflows`](examples/workflows) directory for the complete schema.

## Workflows

Torii includes `production-change`, `implement-review`, and `review` workflows.
The production workflow requires plan approval before implementation. Custom
YAML or JSON definitions can be placed in a trusted project's `.pi/workflows`
directory.

The agent controls workflows with `workflow_start`, `workflow_status`,
`workflow_control`, and `artifact_read`. See
[Workflow architecture](docs/workflows.md) for the schema, trust boundary,
persistence, and restart behavior.

Open `/workflow` to search the trusted workflow catalog and inspect its dependency
graph before launch, or start one directly with `/workflow <name> <task>`. Use
`/workflow check <name>` to validate the graph without launching. Open
`/workflows` for checkpoint controls, explicit retry and cancellation, and
bounded artifact inspection.

Interrupted writers never replay automatically after `/resume`; retry is an
explicit operator action.
