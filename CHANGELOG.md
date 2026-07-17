# Changelog

## 0.1.4 - 2026-07-17

### Sessions and interaction

- Sending a foreground message immediately enters a locally owned working
  state instead of waiting for the first runtime response.
- Pi's `null` and missing parent-session representations are normalized at the
  SDK boundary so saved top-level sessions remain visible after restart.
- Clicking ordinary transcript messages selects them without opening the
  full-screen block viewer.
- Streaming redraws use a stable software cursor instead of repeatedly moving
  and showing the terminal hardware cursor.

### Markdown and copy

- The transcript uses a semantic GFM parser with responsive tables,
  blockquotes, links, emphasis, strikethrough, task lists, math, nested lists,
  thematic breaks, and fenced or indented code blocks.
- Selected blocks support rendered-text, raw-Markdown, fenced-code, and
  complete-turn clipboard actions.

### Updates

- Update downloads resume partial files, retry transient failures, distinguish
  stalled transfers from slow valid downloads, and retry checksum failures.
- Update errors retain their complete cause chain and packaged health checks
  allow for slower antivirus inspection on Windows.
- GitHub releases publish only the matching version section from this
  changelog.

## 0.1.3 - 2026-07-17

### Authentication

- Dashboard login preserves Pi's typed provider authentication metadata instead
  of routing every provider through OAuth.
- API-based providers such as `opencode-go` use a masked API-key prompt, while
  OAuth providers retain their existing authorization flow.
- Credential changes are shown only after Pi confirms them, and storage
  failures surface as authentication errors.

## 0.1.2 - 2026-07-17

### Workflow visibility

- The transcript now keeps an actionable workflow status row above the
  composer. Approval, failure, and interruption states take priority, and the
  row opens the exact workflow run when clicked.
- Closing a workflow-owned agent detail returns to its workflow. Other agent
  details return to the transcript or task list that opened them.

### Rendering and orchestration

- Subagent presentation events retain only visible tool metadata, coalesce
  streamed deltas, and omit full tool result and write bodies from parent UI
  state.
- Runtime-session snapshots are emitted only when resident status changes.
- Animation redraws match the visible spinner cadence instead of rebuilding
  active views thirty times per second.

### Distribution

- Linux and Windows installers are immutable, checksum-listed release assets
  instead of mutable branch files.

## 0.1.1 - 2026-07-17

### Pi runtime

- Pi is the default backend; the mock backend remains available through
  `--backend mock`.
- Resumed sessions restore Pi's authoritative context usage and live working
  state instead of reconstructing them from incomplete transcript events.
- Settings now lists resolved Pi extensions and persists enable or disable
  choices through Pi's package configuration.

### Dashboard and updates

- Release binaries target Linux x64 and Windows x64.
- Dashboard sessions reuse the bounded picker pager, with keyboard paging and
  consistent scrollbar behavior.
- Resumed session attention is derived from unresolved current work rather than
  stale historical failures.
- Background update-check failures are reported instead of appearing current.
- Release builds run the Rust and TypeScript test suites before packaging.

## 0.1.0 - 2026-07-17

First public release.

### Agent workspace

- Persistent Pi sessions with resume, fork, clone, compaction, tree navigation,
  import, export, and model or provider authentication.
- Streaming Markdown, reasoning, tool activity, diffs, usage, image attachments,
  permission controls, and responsive terminal interaction.
- Rust-owned resident sessions, subagent lifecycle, durable workflows,
  checkpoints, bounded artifacts, recovery, and worktree isolation.
- MCP discovery, project trust, scoped models, package management, and
  interactive or headless operation.

### Distribution and updates

- Standalone Rust host and Bun-compiled SDK sidecar with no runtime Node.js,
  npm, Bun, or Rust dependency.
- Versioned installation through POSIX shell or PowerShell.
- Background stable-release detection, digest-verified downloads, staged health
  checks, atomic activation, and automatic rollback to the previous version.
- Linux, macOS, and Windows release archives for x86-64 and Arm64.

### Dashboard

- Relative session timestamps with numeric chronological sorting.
- Session actions derived from the selected session: stop and close for
  residents, delete for inactive saved sessions.
- Non-blocking available, downloading, ready, failed, and rollback update
  states in the existing dashboard footer.
