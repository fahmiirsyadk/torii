# pi-shell

Grok-style terminal UX on top of a runtime-neutral agent harness.

The first vertical slice contains:

- a typed `AgentHarness` boundary;
- a mock harness that streams reasoning, text, tool, and completion events;
- a minimal ratatui interface;
- a headless JSONL mode for protocol testing.

## Run

```bash
cargo run -p pi-shell
```

Press `Ctrl+Q` to exit. For a non-interactive smoke test:

```bash
cargo run -p pi-shell -- --headless
```

The mock backend remains the default. To launch the TUI against the official
Pi SDK sidecar:

```bash
cargo run -p pi-shell -- --backend pi
```

Validate Node launch, protocol health, local Pi resource loading, and in-memory
session creation without sending an inference request:

```bash
cargo run -p pi-shell -- --check-pi
```

Using `--backend pi --headless` sends the built-in headless prompt to the
configured model and may incur provider usage; `--check-pi` does not.
Use `--prompt "..."` to provide an explicit one-shot headless prompt.
Use `--model provider/model-id` to select a model before entering the TUI or
sending a headless prompt. It can also be combined with `--check-pi` to verify
model switching without inference.

Pi-backed conversations are persistent by default and use Pi's normal session
store under `~/.pi/agent/sessions/`. Resume or open a saved session with:

```bash
cargo run -p pi-shell -- --backend pi --continue
cargo run -p pi-shell -- --backend pi --session <path-or-partial-id>
cargo run -p pi-shell -- --backend pi --fork <path-or-partial-id>
cargo run -p pi-shell -- --backend pi -r
cargo run -p pi-shell -- --backend pi --no-session
```

`--continue` restores the latest session for the current working directory,
including its transcript, reasoning, tools, subagent calls, usage, and selected
model. `--no-session` keeps the conversation in memory only. Login, logout, and
`--check-pi` always use temporary sessions and do not create empty history.

Inside the TUI, enter `/resume` to open a searchable current-project session
picker. The picker shows the session name or first prompt, last-modified time,
message count, and a `✓ current` marker. Selecting a session replaces the
transcript and active Pi runtime without restarting pi-shell.

Pi-native session operations are also available in the TUI: `/new` starts a
fresh persistent session, `/name <name>` updates its display name, `/session`
shows its file, message/token counts, and cost, `/clone` copies the current
active branch into a new session file, and `/compact [instructions]` invokes
Pi's native context compaction. Session files remain Pi's authoritative JSONL
tree; pi-shell does not maintain a parallel conversation format.
`/tree` opens the complete Pi session tree and marks entries on the active
branch; selecting an earlier user message rewinds to its parent and places the
message back in the composer for editing. `/fork` uses a user-message-only
picker and creates a separate session containing the history before that
prompt. `-r` opens the resume picker on startup, while `--fork` copies an
existing session into the current project using Pi's native lineage metadata.
Inside the tree picker, `Ctrl+O` cycles Pi-compatible filters, `Shift+T`
toggles timestamps, and `Shift+L` edits or clears the selected entry label.
Enter opens Pi's branch-summary choice (no summary, default summary, or custom
summary instructions); Shift+Enter remains a direct summarize shortcut.

Update a provider API key through Pi's credential store:

```bash
cargo run -p pi-shell -- login
cargo run -p pi-shell -- login opencode-go
cargo run -p pi-shell -- logout opencode-go
```

With no provider argument, `login` shows an interactive provider chooser. API
keys are read only from an interactive terminal, displayed as bullets, and
persisted by Pi's `AuthStorage` in its normal `auth.json` store. They are never
accepted as command-line arguments.

OAuth providers now use Pi's native login implementation. Browser URLs and
device codes are shown without forcing a GUI launch; prompt and selection
callbacks round-trip through the sidecar in both the CLI and TUI. TUI OAuth
values are masked. Use `pi login <provider>` or `/login <provider>`.

Pi's SDK creates settings lock files under `~/.pi/agent`. In restricted
containers, that directory must be writable or Pi will load an empty settings
view and report no configured models.

UI development uses deterministic stories modeled on captured Grok Build
states:

```bash
cargo run -p pi-shell -- --story conversation
cargo run -p pi-shell -- --story streaming
cargo run -p pi-shell -- --story markdown
cargo run -p pi-shell -- --story tools
cargo run -p pi-shell -- --story palette
cargo run -p pi-shell -- --story model-picker
cargo run -p pi-shell -- --story settings
cargo run -p pi-shell -- --story permission
cargo run -p pi-shell -- --story tree
cargo run -p pi-shell -- --story fork
```

Add `--headless` to render a `100×32` plain-text reference frame without
entering the alternate screen.

Transcript scrolling supports the mouse wheel, Up/Down, Page Up/Page Down,
Ctrl+U/Ctrl+D, Home, and End. Reaching the bottom resumes tail-following.

The composer supports normal text entry, Unicode-aware cursor movement and
deletion, prompt history, and mouse/Tab focus switching. Press Enter to submit
to the mock harness, Shift+Tab to cycle permission mode, Ctrl+C to clear the
draft, and Ctrl+Q to quit.

Typing `@` opens a fuzzy project-file picker and inserts the selected reference
at the cursor. Inputs beginning with `!` execute immediately through Pi's bash
executor and persist their output in model context; `!!` runs the same command
but marks its persisted `BashExecutionMessage` as excluded from model context.

Slash completion also includes Pi's live extension commands, prompt templates,
and `/skill:name` entries. `/context` lists the project context files loaded by
Pi, and `/reload` reloads extensions, skills, prompts, themes, and context files
then refreshes completion without restarting pi-shell.
The sidecar is built around Pi's `AgentSessionRuntime`, so extension command
contexts and lifecycle hooks are rebound after resume, new, clone, fork,
import, tree navigation, and resource reload rather than disappearing after a
session replacement.

`/settings` now edits Pi's persisted steering and follow-up delivery modes,
automatic compaction, and default project-trust policy. `/scoped-models` selects
the models used for cycling and persists them through Pi settings. `/trust`
toggles the current directory in Pi's `trust.json`; as in official Pi, restart
or reload is required before newly trusted project extensions become active.

Session portability commands use Pi directly: `/export [file]` writes the
current session as HTML, `/import <file.jsonl>` copies an imported JSONL branch
into the current project and switches to it, and `/copy` copies the latest
assistant text through Pi's platform clipboard helper.

Package commands delegate to the pinned official Pi CLI, preserving Pi's
package manifests, project-trust checks, npm/git installation, filtering, and
update behavior: `pi install`, `pi remove`/`uninstall`, `pi update`, `pi list`,
and `pi config`. Set `PI_SHELL_PI_CLI` only when using a nonstandard sidecar
installation.

While Pi is working, Enter queues a steering message and Alt+Enter queues a
follow-up. Escape aborts the active operation, clears Pi's queue, and restores
queued text to the composer. Ctrl+T or `/thinking` cycles the current model's
supported thinking levels; the effective level is shown in the footer and is
restored from persistent sessions.

Assistant output supports wrapped Markdown headings, lists, bold text, inline
code, and fenced code blocks. With scrollback focused, press `e` to fold the
latest reasoning block or Ctrl+E to expand/collapse all reasoning blocks.
Tool results and diffs are expandable from scrollback focus with `t` and `d`.
Consecutive calls of the same kind collapse into clickable groups such as
`Read 3 files`; clicking the group reveals independently expandable child
calls. The focused row uses a green rail, file paths are yellow, and search
queries are green with muted match counts.
User prompt cards use vertical padding and remain pinned at the top after their
original transcript position scrolls out of view.
Tool headers follow Grok's compact `◆ Read`, `◆ Edit`, and `◆ Run` treatment.
Running calls animate and count elapsed milliseconds; completed and restored
calls show Pi's measured duration. Background `Agent` calls remain active until
their matching `get_subagent_result` report arrives, so their label describes
the assigned scout work and their timer covers the full task lifetime rather
than only the spawn request. Literal `<think>` output is
converted into foldable reasoning instead of being shown as raw tags.

Use Ctrl+P for the searchable command palette, Ctrl+M for the model picker,
and F2 for settings. Typing `/` opens slash-command suggestions. Permission
requests open a blocking modal with Allow once, Always allow, and Deny choices;
the current composer draft remains intact while overlays are open.
An inline Pi `tool_call` extension enforces the modal: `bash`, `write`, and
`edit` pause before execution. Deny blocks the tool, Allow once resumes it, and
Always allow remembers the exact tool/argument pattern for the process lifetime.

Successful `write` and `edit` calls also append Grok-style file checkpoints as
Pi custom session entries. `/rewind` lists checkpoints across restarts and
restores the exact pre-edit file contents (or removes a file created by the
original write) without changing the conversation branch.

The Grok capability extension registers `web_fetch` and `web_search` as native
Pi tools. Fetch accepts only HTTP(S), converts HTML to bounded readable text,
and honors cancellation. Search uses Brave when `BRAVE_SEARCH_API_KEY` is set
and otherwise falls back to DuckDuckGo HTML results. Both flow through the
normal grouped/timed tool renderer.

MCP servers can be configured in `~/.pi/agent/mcp.json` or a trusted project's
`.mcp.json` using the common `{ "mcpServers": { ... } }` shape. Entries may use
`command`/`args`/`env` for stdio or `url` with optional `type: "sse"` for SSE;
other URLs use Streamable HTTP. Discovered tools are registered as
`mcp__<server>__<tool>`, and clients reconnect cleanly on `/reload`. Project MCP
processes are never launched before Pi marks the project trusted.

`/plan` enters an enforced read-only Plan mode. The bundled `update_plan` tool
lets the model publish pending, in-progress, and completed steps; updates are
stored as Pi custom entries, restored on resume, and summarized as progress in
the header. At most one step may be in progress.

The official Pi SDK is TypeScript. The next slice adds a small Node sidecar
that converts Pi SDK events to the `AgentEvent` JSONL protocol consumed by
the Rust harness.

## Pi SDK sidecar

The sidecar is pinned to `@earendil-works/pi-coding-agent` and requires Node
22.19 or newer.

```bash
cd sidecar
npm install
npm run check
npm run smoke
```

It supports health, sessions, prompts, cancellation, model selection,
permission responses, and API-key credential management. The Rust
`pi-harness-pi` crate supervises the process, correlates requests, routes
per-session events, reports crashes/timeouts, and shuts the child down with the
harness.
