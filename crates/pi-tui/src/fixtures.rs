use crate::state::{
    AppState, DiffKind, DiffLine, Entry, OverlayKind, PendingPermission, PermissionMode, ToolStatus,
};

#[derive(Clone, Copy, Debug)]
pub enum Story {
    Conversation,
    Streaming,
    Markdown,
    Tools,
    Palette,
    ModelPicker,
    Settings,
    Permission,
}

impl Story {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "conversation" => Some(Self::Conversation),
            "streaming" => Some(Self::Streaming),
            "markdown" => Some(Self::Markdown),
            "tools" => Some(Self::Tools),
            "palette" => Some(Self::Palette),
            "model-picker" => Some(Self::ModelPicker),
            "settings" => Some(Self::Settings),
            "permission" => Some(Self::Permission),
            _ => None,
        }
    }

    pub fn state(self) -> AppState {
        match self {
            Self::Conversation => conversation(),
            Self::Streaming => streaming(),
            Self::Markdown => markdown(),
            Self::Tools => tools(),
            Self::Palette => with_overlay(OverlayKind::CommandPalette),
            Self::ModelPicker => with_overlay(OverlayKind::ModelPicker),
            Self::Settings => with_overlay(OverlayKind::Settings),
            Self::Permission => permission(),
        }
    }
}

pub fn conversation() -> AppState {
    AppState {
        branch: "collector-improvement".into(),
        cwd: "~/dev/mynet_revamp/app".into(),
        context_used: 126_000,
        context_limit: 200_000,
        tasks_complete: 7,
        tasks_total: 8,
        entries: vec![
            Entry::User {
                text: "okay commit and create a pr explaining what changes.".into(),
                timestamp: "5:01 PM".into(),
            },
            Entry::Diff {
                id: "fixture-diff-1".into(),
                path: "app/Livewire/History.php".into(),
                lines: vec![
                    diff(
                        155,
                        "Within seconds (Reverb). If Reverb is unreachable, the",
                    ),
                    diff(156, "wire:poll.60s fallback keeps it fresh."),
                    diff(
                        157,
                        "Apply a filter that returns no rows — the empty state should",
                    ),
                    diff(
                        158,
                        "read ‘Tidak ada transaksi yang cocok dengan filter saat",
                    ),
                    diff(
                        159,
                        "ini.’ Clear the filter and the empty state should switch",
                    ),
                    diff(160, "back to ‘Belum ada pembayaran masuk bulan ini.’"),
                    DiffLine {
                        number: Some(165),
                        text: "Co-authored by Grok.".into(),
                        kind: DiffKind::Added,
                    },
                ],
                expanded: true,
            },
            Entry::Tool {
                id: "tool-pr".into(),
                label: "Run".into(),
                detail: "Open the pull request on GitHub".into(),
                status: ToolStatus::Success,
                duration: Some("1.2s".into()),
                started_at: None,
                result: Some("Pull request #103 opened successfully.".into()),
                expanded: false,
            },
            Entry::Tool {
                id: "tool-verify".into(),
                label: "Run".into(),
                detail: "Verify the PR was created".into(),
                status: ToolStatus::Success,
                duration: Some("640ms".into()),
                started_at: None,
                result: Some("State: OPEN · reviewer requested".into()),
                expanded: false,
            },
            Entry::Assistant {
                timestamp: "5:03 PM".into(),
                lines: vec![
                    "Done. Here's the summary:".into(),
                    "".into(),
                    "⚑ PR opened".into(),
                    "".into(),
                    "#103 — feat(collector): searchbar + filters on History (Riwayat) page".into(),
                    "".into(),
                    "• Base: main".into(),
                    "• Head: collector-improvement".into(),
                    "• State: OPEN".into(),
                    "• Diffs: 3 files, +604 / -2".into(),
                    "• Reviewer requested: fahmiirsyad".into(),
                    "".into(),
                    "What landed in the commit".into(),
                    "".into(),
                    "One commit (68cb7b) on collector-improvement:".into(),
                    "".into(),
                    "feat(collector): searchbar + filters on History (Riwayat) page".into(),
                ],
            },
        ],
        model: "Minimax M3 via opencode-go".into(),
        permission_mode: PermissionMode::AlwaysApprove,
        scroll_from_bottom: 12,
        ..AppState::default()
    }
}

#[cfg(test)]
pub fn long_session(count: usize) -> AppState {
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        match index % 4 {
            0 => entries.push(Entry::User {
                text: format!(
                    "Prompt {index}: inspect the current implementation and report findings."
                ),
                timestamp: "12:00 PM".into(),
            }),
            1 => entries.push(Entry::Assistant {
                lines: vec![
                    format!("Result {index}"),
                    "A compact response with **markdown** output.".into(),
                ],
                timestamp: "12:00 PM".into(),
            }),
            2 => entries.push(Entry::Reasoning {
                text: "Reviewing the relevant files and checking the implementation details."
                    .into(),
                active: false,
                expanded: false,
            }),
            _ => entries.push(Entry::Tool {
                id: format!("tool-{index}"),
                label: "Read".into(),
                detail: format!("src/file-{index}.rs"),
                status: ToolStatus::Success,
                duration: Some("12ms".into()),
                started_at: None,
                result: Some(
                    "A sufficiently large stored tool result\nwith a second output line.".into(),
                ),
                expanded: index % 16 == 3,
            }),
        }
    }
    AppState {
        entries,
        focus: crate::state::Focus::Scrollback,
        ..AppState::default()
    }
}

pub fn streaming() -> AppState {
    let mut state = conversation();
    state.entries.truncate(1);
    state.entries.extend([
        Entry::Reasoning {
            text: "Comparing the implementation with the requested interface.".into(),
            active: true,
            expanded: false,
        },
        Entry::Tool {
            id: "tool-streaming".into(),
            label: "Run".into(),
            detail: "cargo test · 1.8s".into(),
            status: ToolStatus::Running,
            duration: Some("1.8s".into()),
            started_at: None,
            result: None,
            expanded: false,
        },
    ]);
    state.placeholder = "Type the next request while this turn is running…".into();
    state.status = "generating…".into();
    state.streaming = true;
    state
}

pub fn markdown() -> AppState {
    AppState {
        branch: "ui-markdown".into(),
        cwd: "~/dev/pi-shell".into(),
        context_used: 18_400,
        context_limit: 200_000,
        entries: vec![
            Entry::User {
                text: "Show the renderer design and a small Rust example.".into(),
                timestamp: "12:14 AM".into(),
            },
            Entry::Reasoning {
                text: "I’ll describe the rendering pipeline, then include a compact implementation example. The block can be folded after streaming completes.".into(),
                active: false,
                expanded: true,
            },
            Entry::Assistant {
                timestamp: "12:14 AM".into(),
                lines: vec![
                    "# Renderer design".into(),
                    "".into(),
                    "The renderer converts **typed entries** into styled terminal lines.".into(),
                    "".into(),
                    "- Wrap prose to the viewport width".into(),
                    "- Preserve `inline code` styling".into(),
                    "- Render fenced blocks with a separate background".into(),
                    "".into(),
                    "```rust".into(),
                    "fn render(entry: &Entry) -> Vec<Line<'static>> {".into(),
                    "    match entry {".into(),
                    "        Entry::Assistant { lines, .. } => markdown::render(lines),".into(),
                    "        _ => Vec::new(),".into(),
                    "    }".into(),
                    "}".into(),
                    "```".into(),
                ],
            },
        ],
        model: "Mock model".into(),
        ..AppState::default()
    }
}

pub fn tools() -> AppState {
    AppState {
        branch: "ui-tools".into(),
        cwd: "~/dev/pi-shell".into(),
        entries: vec![
            Entry::User {
                text: "Run the checks and show me what changed.".into(),
                timestamp: "12:26 AM".into(),
            },
            Entry::Tool {
                id: "tool-test".into(),
                label: "Run".into(),
                detail: "cargo test --workspace".into(),
                status: ToolStatus::Running,
                duration: Some("2.4s".into()),
                started_at: None,
                result: None,
                expanded: false,
            },
            Entry::Tool {
                id: "tool-clippy".into(),
                label: "Run".into(),
                detail: "cargo clippy --workspace".into(),
                status: ToolStatus::Success,
                duration: Some("810ms".into()),
                started_at: None,
                result: Some("Finished dev profile. No warnings.".into()),
                expanded: true,
            },
            Entry::Tool {
                id: "tool-broken".into(),
                label: "Run".into(),
                detail: "cargo test broken-package".into(),
                status: ToolStatus::Error,
                duration: Some("320ms".into()),
                started_at: None,
                result: Some("error: package ID specification did not match any packages".into()),
                expanded: true,
            },
            Entry::Diff {
                id: "fixture-diff-2".into(),
                path: "crates/pi-tui/src/ui.rs".into(),
                lines: vec![
                    DiffLine {
                        number: Some(141),
                        text: "let border = theme.border;".into(),
                        kind: DiffKind::Removed,
                    },
                    DiffLine {
                        number: Some(141),
                        text: "let border = focused_border(state, theme);".into(),
                        kind: DiffKind::Added,
                    },
                    DiffLine {
                        number: Some(142),
                        text: "render_widget(composer, area);".into(),
                        kind: DiffKind::Context,
                    },
                ],
                expanded: true,
            },
        ],
        model: "Mock model".into(),
        ..AppState::default()
    }
}

fn with_overlay(overlay: OverlayKind) -> AppState {
    let mut state = markdown();
    state.open_overlay(overlay);
    state
}

fn permission() -> AppState {
    let mut state = tools();
    state.pending_permission = Some(PendingPermission {
        id: "permission-1".into(),
        tool: "bash".into(),
        reason: "Run cargo test --workspace".into(),
    });
    state.open_overlay(OverlayKind::Permission);
    state
}

fn diff(number: u32, text: &str) -> DiffLine {
    DiffLine {
        number: Some(number),
        text: text.into(),
        kind: DiffKind::Context,
    }
}
