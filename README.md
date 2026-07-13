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
- Interactive and headless operation.

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
git clone https://github.com/fahmiirsyadk/pi-shell.git
cd pi-shell/sidecar
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
