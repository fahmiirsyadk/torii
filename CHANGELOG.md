# Changelog

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
