# Torii

A terminal interface for coding-agent sessions powered by the Pi SDK. (WIP). The Design heavy inspired from Grok Build CLI.

<div align="center">
<img width="725" height="582" alt="screenshot-20260713-145028" src="https://github.com/user-attachments/assets/1e39cff2-1f51-461d-a70c-a2b72b489008" />
</div>

## Features

- Persistent sessions with resume, fork, clone, rename, delete, tree navigation, compaction, import, and export.
- Streaming Markdown responses, reasoning, tool activity, diffs, usage, and subagent transcripts.
- Searchable model, command, session, file, settings, and history pickers.
- Permission prompts for tool execution and project trust controls.
- Editable multiline paste blocks with mouse-based cursor positioning.
- Multiple image attachments from the clipboard, `/paste-image`, keyboard shortcuts, or the `@` file picker.
- Clickable image previews with aspect-ratio-preserving terminal rendering.
- Non-blocking clipboard image decoding with processing, success, and error states.
- Project file references, shell commands, prompt history, plans, MCP tools, and package management.
- Durable multi-agent workflows with frozen model/role routing, checkpoints, parallel read-only review, compact artifacts, and resume recovery.
- On-demand MCP tool discovery that grows the active tool set monotonically and restores it on session resume.
- Interactive and headless operation.

See [Workflow architecture](docs/workflows.md) for authoring, multi-model
routing, context and cache policies, and `/resume` behavior.

## Screenshots
<img width="1337" height="896" alt="screenshot-20260713-185523" src="https://github.com/user-attachments/assets/3512820a-733a-4439-95c0-065cf89da725" />
<img width="930" height="590" alt="screenshot-20260713-075616" src="https://github.com/user-attachments/assets/4c43ef20-b87c-4ea0-88a0-ca70d1f01506" />


## Install

Requirements:

- Rust toolchain
- Node.js 22.19 or newer
- npm

Install the sidecar dependencies and build the release binary:

```bash
git clone https://github.com/fahmiirsyadk/torii.git
cd torii/sidecar
npm install
cd ..
cargo build --release -p torii
```

Run it with the Pi backend:

```bash
./target/release/torii --backend pi
```

Provider authentication:

```bash
./target/release/torii login
./target/release/torii login <provider>
./target/release/torii logout <provider>
```

## Arguments

```text
--backend <mock|pi>       Select the backend. Default: mock
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

## Usage tutorial

### Start, continue, and resume sessions

Start Torii in the current repository:

```bash
torii --backend pi
```

The welcome screen lists saved sessions and their live state. Use `Up`/`Down`
to select one, `Enter` to resume it, or `n` to start a new session. From the
transcript, `/home` returns to this screen and `/resume` opens the searchable
session picker. You can also resume directly from the shell:

```bash
torii --backend pi --continue
torii --backend pi --session <path-or-id>
```

During a running turn, `Enter` queues a follow-up for the next turn and
`Ctrl+Enter` sends an immediate steering message. `Ctrl+C` clears a non-empty
draft first; with an empty composer it cancels the active turn. `Ctrl+P` opens
the command palette, `Ctrl+L` changes model, and `Ctrl+B` opens the task list.

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

An isolated worktree is never merged automatically. After inspecting the result,
ask the parent to apply it with `apply_subagent_worktree`, or discard it with
`remove_subagent_worktree`. Subagents cannot spawn nested subagents. Use a
workflow when the work needs several deterministic roles, checkpoints, retries,
or model routes.

Choose the default subagent model with `/subagent-model`. The setting is separate
from the parent model and persists across sessions.

### Run a built-in workflow

Torii ships with three workflows:

- `production-change` for connector evidence, approval, implementation,
  independent reviews, conditional repair, and final verification.
- `implement-review` for plan, approval, implementation, review, and repair.
- `review` for parallel correctness, security, and test review followed by
  synthesis.

First inspect the resolved agents, exact models, tools, policies, and readiness:

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

Workflows are scheduled by Torii rather than by the parent model. Their resolved
graph, model routes, contracts, budgets, attempts, checkpoints, and artifacts are
journaled. Completed work is not repeated after `/resume`; interrupted
write-capable steps fail closed and require an explicit retry. Persistent executor
roles reopen their child session, while ephemeral reviewers receive clean contexts.

### Use multiple models and MCP connectors safely

Copy the multi-provider example into the global workflow directory, or into a
trusted project's workflow directory:

```bash
mkdir -p .pi/workflows
cp examples/workflows/production-multimodel-github.yaml .pi/workflows/
```

Edit its placeholder `provider/model` routes, then run:

```text
/workflow check production-multimodel-github
```

The example uses a fresh bounded GitHub connector context, an exact planner and
executor, parallel reviewers on different models, and a persistent executor for
repairs. Connector evidence is validated into bounded structured artifacts before
downstream roles receive it. MCP tools are discovered with `tool_search` and only
added to the active set; they are not removed during the session, preserving Pi's
cache-friendly prompt prefix. The loaded set is restored before the next prompt
after `/resume`.

Project workflow definitions are ignored until the project is trusted. External
MCP mutations additionally require an earlier named checkpoint, an exact tool
allowlist, ephemeral execution, and a typed effect receipt.

For a workflow with typed parameters:

```text
/workflow review --params {"target":"src","mode":"deep"} -- Review this change
```

See [Workflow architecture](docs/workflows.md) and the
[`examples/workflows`](examples/workflows) directory for the full schema,
composition, contracts, budgets, provider policies, and guarded GitHub mutation
example.

## Workflows

Torii includes `production-change`, `implement-review`, and `review` workflows.
The production workflow isolates optional MCP evidence, requires plan approval,
and applies context/cache guardrails through implementation and review. Custom YAML or JSON
definitions can be placed in `~/.pi/agent/workflows` (using Pi's resolved agent
directory) or, for trusted projects, `.pi/workflows`. Project definitions
shadow global definitions only after the project is trusted.

The agent controls workflows with `workflow_start`, `workflow_status`,
`workflow_control`, and `artifact_read`. MCP connectors are discovered through
`tool_search` and enabled only when needed. See [Workflow architecture](docs/workflows.md)
for the schema, model routing, trust boundaries, and `/resume` behavior.

Open `/workflow` to search the trusted workflow catalog and inspect its resolved
models, permissions, tools and policies before launch, or start one directly with
`/workflow <name> <task>`. Use `/workflow check <name>` for live model, agent,
tool, MCP, and context-fan-in readiness without launching. Open
`/workflows` for the native execution dashboard, checkpoint controls, retry and
cancel actions, artifact inspection, context budgets, tool-schema/cache-prefix
fingerprints, declarative guardrail violations, and enforceable provider
prompt/output/cache budgets. Optional workflow-wide budgets reserve headroom
across parallel calls and retain cumulative consumption across `/resume`.
Provider concurrency, journal-backed rate limits, and circuit breakers prevent
retry storms without changing a run's frozen model route.
Bounded custom JSON contracts create validated handoff artifacts, while static
workflow composition namespaces and freezes reusable fragments before launch.
Closed typed launch parameters use the `/workflow <name> --params ... -- <task>`
form and are validated, canonicalized, and frozen across `/resume` without
being interpolated into workflow prompts. Composed fragments can be pinned to
an exact declared version and receive only statically bound parameter views, so
incompatible changes or broader-than-declared context fail during preflight.

External MCP mutations require an earlier named checkpoint, an exact fail-closed
tool allowlist, ephemeral execution, and a typed `effect_receipt`. Interrupted
writers never replay automatically after `/resume`; retry is an explicit operator action.
