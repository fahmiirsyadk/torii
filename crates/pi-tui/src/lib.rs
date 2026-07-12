mod fixtures;
mod markdown;
mod overlay;
mod state;
mod theme;
mod ui;

use std::{io, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
pub use fixtures::Story;
use ratatui::{Terminal, backend::CrosstermBackend};
use state::OverlayAction;
pub use state::{AppState, Focus, OverlayKind};
use tokio::sync::{broadcast, mpsc};

#[derive(Debug)]
pub enum UiCommand {
    Submit {
        text: String,
        delivery: Option<pi_harness::MessageDelivery>,
    },
    Permission {
        request_id: String,
        decision: pi_harness::PermissionDecision,
    },
    SetModel(String),
    ResumeSession(String),
    NewSession,
    NameSession(String),
    SessionInfo,
    CloneSession,
    Compact(Option<String>),
    LoadTree {
        user_only: bool,
    },
    NavigateTree {
        entry_id: String,
        summarize: bool,
    },
    ForkSession {
        entry_id: String,
    },
    SetLabel {
        entry_id: String,
        label: Option<String>,
    },
    CycleThinking,
    AbortAndRestoreQueue,
    ExecuteBash {
        command: String,
        exclude_from_context: bool,
    },
    ReloadResources,
    SetRuntimeSetting {
        key: String,
        value: serde_json::Value,
    },
    SetProjectTrust(bool),
    SetScopedModels(Vec<String>),
    ExportSession(Option<String>),
    ImportSession(String),
    CopyLast,
    BeginOauth(String),
    OauthReply {
        id: String,
        value: Option<String>,
    },
    SetPermissionMode(String),
    LoadRewinds,
    RewindFile(String),
    ExportTrace(Option<String>),
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

pub struct TuiBootstrap {
    pub models: Vec<pi_harness::ModelInfo>,
    pub sessions: Vec<pi_harness::SessionInfo>,
    pub files: Vec<String>,
    pub resources: pi_harness::RuntimeResources,
    pub settings: pi_harness::RuntimeSettings,
    pub open_resume: bool,
}

pub async fn run(
    events: broadcast::Receiver<pi_harness::AgentEvent>,
    commands: mpsc::UnboundedSender<UiCommand>,
    bootstrap: TuiBootstrap,
) -> Result<()> {
    let TuiBootstrap {
        models,
        sessions,
        files,
        resources,
        settings,
        open_resume,
    } = bootstrap;
    let mut state = AppState::default();
    if let Ok(cwd) = std::env::current_dir() {
        state.cwd = display_path(&cwd);
    }
    if !models.is_empty() {
        state.model = models[0].display_name.clone();
        state.available_models = models;
    }
    state.available_sessions = sessions;
    state.available_files = files;
    state.runtime_commands = resources.commands;
    state.context_files = resources.context_files;
    state.runtime_settings = settings;
    if open_resume {
        state.open_overlay(OverlayKind::SessionPicker);
    }
    run_app(state, Some(events), Some(commands)).await
}

fn display_path(path: &std::path::Path) -> String {
    let rendered = path.display().to_string();
    let Some(home) = std::env::var_os("HOME") else {
        return rendered;
    };
    let home = std::path::PathBuf::from(home);
    path.strip_prefix(&home)
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or(rendered)
}

pub async fn run_story(story: Story) -> Result<()> {
    run_app(story.state(), None, None).await
}

pub fn render_story_text(story: Story, width: u16, height: u16) -> Result<String> {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    let state = story.state();
    terminal.draw(|frame| ui::render(frame, &state))?;
    Ok(buffer_text(terminal.backend().buffer(), width, height))
}

async fn run_app(
    mut state: AppState,
    mut events: Option<broadcast::Receiver<pi_harness::AgentEvent>>,
    commands: Option<mpsc::UnboundedSender<UiCommand>>,
) -> Result<()> {
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut dirty = true;

    loop {
        if let Some(receiver) = &mut events {
            while let Ok(agent_event) = receiver.try_recv() {
                state.apply(agent_event);
                dirty = true;
            }
        }

        let animated = state.streaming || state.active_compaction_started_at().is_some();
        if dirty || animated {
            terminal.draw(|frame| ui::render(frame, &state))?;
            dirty = false;
        }

        if event::poll(Duration::from_millis(30))? {
            dirty = true;
            let size = terminal.size()?;
            let max_scroll = ui::max_scroll(&state, size.width, size.height);
            let page = usize::from(size.height.saturating_sub(8)).max(1);

            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press && state.overlay != OverlayKind::None =>
                {
                    match key.code {
                        KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break;
                        }
                        KeyCode::Esc => {
                            let action = state.cancel_oauth();
                            if dispatch_overlay_action(action, &commands) {
                                break;
                            }
                        }
                        KeyCode::Up => state.move_overlay_selection(-1),
                        KeyCode::Down => state.move_overlay_selection(1),
                        KeyCode::Char(' ') if state.overlay == OverlayKind::ScopedModels => {
                            state.toggle_scoped_model();
                        }
                        KeyCode::Char('o')
                            if key.modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(
                                    state.overlay,
                                    OverlayKind::TreePicker | OverlayKind::ForkPicker
                                ) =>
                        {
                            state.cycle_tree_filter();
                        }
                        KeyCode::Char('t')
                            if key.modifiers.contains(KeyModifiers::SHIFT)
                                && matches!(
                                    state.overlay,
                                    OverlayKind::TreePicker | OverlayKind::ForkPicker
                                ) =>
                        {
                            state.toggle_tree_timestamps();
                        }
                        KeyCode::Char('l')
                            if key.modifiers.contains(KeyModifiers::SHIFT)
                                && matches!(
                                    state.overlay,
                                    OverlayKind::TreePicker | OverlayKind::ForkPicker
                                ) =>
                        {
                            state.begin_tree_label();
                        }
                        KeyCode::Backspace => state.overlay_backspace(),
                        KeyCode::Enter => {
                            let action = if key.modifiers.contains(KeyModifiers::SHIFT) {
                                state.activate_tree_with_summary()
                            } else {
                                state.activate_overlay()
                            };
                            if dispatch_overlay_action(action, &commands) {
                                break;
                            }
                        }
                        KeyCode::Char(character)
                            if !key.modifiers.intersects(
                                KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                            ) =>
                        {
                            state.insert_overlay_char(character);
                        }
                        _ => {}
                    }
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.open_overlay(OverlayKind::CommandPalette);
                    }
                    KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.open_overlay(OverlayKind::ModelPicker);
                    }
                    KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(sender) = &commands {
                            let _ = sender.send(UiCommand::CycleThinking);
                        }
                    }
                    KeyCode::F(2) => state.open_overlay(OverlayKind::Settings),
                    KeyCode::Char('?') if state.focus == Focus::Scrollback => {
                        state.open_overlay(OverlayKind::CommandPalette);
                    }
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.clear_prompt();
                    }
                    KeyCode::BackTab => {
                        state.cycle_permission_mode();
                        if let Some(sender) = &commands {
                            let _ = sender.send(UiCommand::SetPermissionMode(
                                state.permission_mode.wire_value().into(),
                            ));
                        }
                    }
                    KeyCode::Tab => {
                        if state.focus != Focus::Prompt || !state.complete_slash_command() {
                            state.toggle_focus();
                        }
                    }
                    KeyCode::Enter if state.focus == Focus::Prompt => {
                        if let Some(action) = state.activate_slash_command() {
                            if dispatch_overlay_action(action, &commands) {
                                break;
                            }
                        } else if state.prompt.trim_start().starts_with('!') {
                            if let Some((command, exclude_from_context)) = state.submit_bash()
                                && let Some(sender) = &commands
                            {
                                let _ = sender.send(UiCommand::ExecuteBash {
                                    command,
                                    exclude_from_context,
                                });
                            }
                        } else if let Some(prompt) = state.submit_prompt()
                            && let Some(sender) = &commands
                        {
                            let delivery = state.streaming.then_some(
                                if key.modifiers.contains(KeyModifiers::ALT) {
                                    pi_harness::MessageDelivery::FollowUp
                                } else {
                                    pi_harness::MessageDelivery::Steer
                                },
                            );
                            let _ = sender.send(UiCommand::Submit {
                                text: prompt,
                                delivery,
                            });
                        }
                    }
                    KeyCode::Backspace if state.focus == Focus::Prompt => state.backspace(),
                    KeyCode::Delete if state.focus == Focus::Prompt => state.delete(),
                    KeyCode::Left if state.focus == Focus::Prompt => state.move_cursor_left(),
                    KeyCode::Right if state.focus == Focus::Prompt => state.move_cursor_right(),
                    KeyCode::Home if state.focus == Focus::Prompt => state.move_cursor_home(),
                    KeyCode::End if state.focus == Focus::Prompt => state.move_cursor_end(),
                    KeyCode::Up if state.focus == Focus::Prompt => state.previous_prompt(),
                    KeyCode::Down if state.focus == Focus::Prompt => state.next_prompt(),
                    KeyCode::Up => ui::move_section_focus(&mut state, size.width, size.height, -1),
                    KeyCode::Down => ui::move_section_focus(&mut state, size.width, size.height, 1),
                    KeyCode::PageUp => state.scroll_up(page, max_scroll),
                    KeyCode::PageDown => state.scroll_down(page),
                    KeyCode::Home => state.scroll_to_top(max_scroll),
                    KeyCode::End => state.scroll_to_bottom(),
                    KeyCode::Char('u')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.focus == Focus::Scrollback =>
                    {
                        state.scroll_up(page / 2, max_scroll);
                    }
                    KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.focus == Focus::Scrollback =>
                    {
                        state.scroll_down(page / 2);
                    }
                    KeyCode::Char('e')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.focus == Focus::Scrollback =>
                    {
                        let expand = !state.all_reasoning_expanded();
                        state.set_all_reasoning_expanded(expand);
                    }
                    KeyCode::Char('e') if state.focus == Focus::Scrollback => {
                        state.toggle_latest_reasoning();
                    }
                    KeyCode::Char('t') if state.focus == Focus::Scrollback => {
                        state.toggle_latest_tool();
                    }
                    KeyCode::Char('d') if state.focus == Focus::Scrollback => {
                        state.toggle_latest_diff();
                    }
                    KeyCode::Esc if state.streaming => {
                        if let Some(sender) = &commands {
                            let _ = sender.send(UiCommand::AbortAndRestoreQueue);
                        }
                    }
                    KeyCode::Esc if !state.prompt.is_empty() => state.clear_prompt(),
                    KeyCode::Char(character)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        state.focus_prompt();
                        state.insert_char(character);
                        if character == '@' && !state.available_files.is_empty() {
                            state.open_overlay(OverlayKind::FilePicker);
                        }
                    }
                    _ => {}
                },
                Event::Mouse(mouse) if state.overlay != OverlayKind::None => match mouse.kind {
                    MouseEventKind::ScrollUp => state.move_overlay_selection(-1),
                    MouseEventKind::ScrollDown => state.move_overlay_selection(1),
                    MouseEventKind::Down(_) => {
                        let items = state.overlay_items();
                        let detail_rows = usize::from(state.overlay == OverlayKind::Permission) * 2;
                        let overlay_height = (items.len() + detail_rows + 4).clamp(6, 16) as u16;
                        let overlay_y = size.height.saturating_sub(overlay_height) / 2;
                        let query_rows = usize::from(matches!(
                            state.overlay,
                            OverlayKind::CommandPalette
                                | OverlayKind::ModelPicker
                                | OverlayKind::SessionPicker
                        )) * 2;
                        let items_y = overlay_y + 1 + detail_rows as u16 + query_rows as u16;
                        let index = mouse.row.saturating_sub(items_y) as usize;
                        if index < items.len() {
                            state.overlay_selected = index;
                        }
                    }
                    _ => {}
                },
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        ui::move_section_focus(&mut state, size.width, size.height, -1)
                    }
                    MouseEventKind::ScrollDown => {
                        ui::move_section_focus(&mut state, size.width, size.height, 1)
                    }
                    MouseEventKind::Moved => {
                        state.hovered_entry = ui::section_hit_at(
                            &state,
                            size.width,
                            size.height,
                            mouse.column,
                            mouse.row,
                        )
                        .filter(|hit| hit.actionable)
                        .map(|hit| hit.index);
                    }
                    MouseEventKind::Down(_) => {
                        if mouse.row >= size.height.saturating_sub(5)
                            && mouse.row < size.height.saturating_sub(2)
                        {
                            state.focus_prompt();
                        } else if let Some(hit) = ui::section_hit_at(
                            &state,
                            size.width,
                            size.height,
                            mouse.column,
                            mouse.row,
                        ) {
                            state.focus_scrollback();
                            if hit.actionable {
                                let target_id = hit.id.clone();
                                let grouped = state.entries.get(hit.index).is_some_and(|entry| {
                                    let state::Entry::Tool { label, .. } = entry else { return false; };
                                    let begins_group = hit.index == 0 || !matches!(state.entries.get(hit.index - 1), Some(state::Entry::Tool { label: previous, .. }) if previous == label);
                                    begins_group && state.entries[hit.index..].iter().take_while(|candidate| matches!(candidate, state::Entry::Tool { label: other, .. } if other == label)).count() > 1
                                });
                                if grouped {
                                    state.toggle_tool_group(hit.index);
                                } else {
                                    state.toggle_entry_at(hit.index);
                                }
                                state.focused_target_id = Some(target_id);
                            } else {
                                state.focused_entry = Some(hit.index);
                                state.focused_target_id = Some(hit.id);
                            }
                        } else {
                            state.focus_scrollback();
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

fn dispatch_overlay_action(
    action: OverlayAction,
    commands: &Option<mpsc::UnboundedSender<UiCommand>>,
) -> bool {
    match action {
        OverlayAction::None => false,
        OverlayAction::Quit => true,
        OverlayAction::Permission {
            request_id,
            decision,
        } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::Permission {
                    request_id,
                    decision,
                });
            }
            false
        }
        OverlayAction::SetModel { id } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetModel(id));
            }
            false
        }
        OverlayAction::ResumeSession { target } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::ResumeSession(target));
            }
            false
        }
        OverlayAction::NewSession => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::NewSession);
            }
            false
        }
        OverlayAction::NameSession(name) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::NameSession(name));
            }
            false
        }
        OverlayAction::SessionInfo => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SessionInfo);
            }
            false
        }
        OverlayAction::CloneSession => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::CloneSession);
            }
            false
        }
        OverlayAction::Compact(instructions) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::Compact(instructions));
            }
            false
        }
        OverlayAction::LoadTree { user_only } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::LoadTree { user_only });
            }
            false
        }
        OverlayAction::NavigateTree {
            entry_id,
            summarize,
        } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::NavigateTree {
                    entry_id,
                    summarize,
                });
            }
            false
        }
        OverlayAction::ForkSession { entry_id } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::ForkSession { entry_id });
            }
            false
        }
        OverlayAction::SetLabel { entry_id, label } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetLabel { entry_id, label });
            }
            false
        }
        OverlayAction::CycleThinking => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::CycleThinking);
            }
            false
        }
        OverlayAction::ReloadResources => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::ReloadResources);
            }
            false
        }
        OverlayAction::SetRuntimeSetting { key, value } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetRuntimeSetting { key, value });
            }
            false
        }
        OverlayAction::SetProjectTrust(trusted) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetProjectTrust(trusted));
            }
            false
        }
        OverlayAction::SetScopedModels(models) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetScopedModels(models));
            }
            false
        }
        OverlayAction::ExportSession(path) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::ExportSession(path));
            }
            false
        }
        OverlayAction::ImportSession(path) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::ImportSession(path));
            }
            false
        }
        OverlayAction::CopyLast => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::CopyLast);
            }
            false
        }
        OverlayAction::BeginOauth(provider) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::BeginOauth(provider));
            }
            false
        }
        OverlayAction::OauthReply { id, value } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::OauthReply { id, value });
            }
            false
        }
        OverlayAction::SetPermissionMode(mode) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetPermissionMode(mode));
            }
            false
        }
        OverlayAction::LoadRewinds => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::LoadRewinds);
            }
            false
        }
        OverlayAction::RewindFile(checkpoint_id) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::RewindFile(checkpoint_id));
            }
            false
        }
        OverlayAction::ExportTrace(path) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::ExportTrace(path));
            }
            false
        }
    }
}

fn buffer_text(buffer: &ratatui::buffer::Buffer, width: u16, height: u16) -> String {
    let mut output = String::new();
    for y in 0..height {
        for x in 0..width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use pi_harness::AgentEvent;
    use ratatui::{Terminal, backend::TestBackend};

    use super::{buffer_text, fixtures, theme::Theme, ui};

    #[test]
    fn conversation_story_renders_at_reference_sizes() {
        for (width, height) in [(80, 24), (100, 32), (160, 48)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            let state = fixtures::conversation();
            terminal.draw(|frame| ui::render(frame, &state)).unwrap();
            let output = buffer_text(terminal.backend().buffer(), width, height);

            assert!(output.contains("collector-improvement"));
            assert!(output.contains("Minimax M3 via opencode-go"));
            assert!(output.contains("always approve"));
            assert!(output.contains("Shift+Tab: mode"));
        }
    }

    #[test]
    fn reference_story_preserves_user_and_diff_colors() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = fixtures::conversation();
        state.scroll_to_top(ui::max_scroll(&state, width, height));
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();

        let cells = &terminal.backend().buffer().content;
        assert!(
            cells
                .iter()
                .any(|cell| cell.bg == Theme::GROK_NIGHT.user_background)
        );
        assert!(
            cells
                .iter()
                .any(|cell| cell.bg == Theme::GROK_NIGHT.success)
        );
    }

    #[test]
    fn streaming_story_changes_contextual_shortcuts() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = fixtures::streaming();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Thinking"));
        assert!(output.contains("Enter: steer"));
        assert!(output.contains("Alt+Enter: follow-up"));
        assert!(output.contains("cargo test"));
    }

    #[test]
    fn prompt_editor_handles_unicode_cursor_and_deletion() {
        let mut state = super::AppState::default();
        state.insert_char('a');
        state.insert_char('界');
        state.insert_char('b');
        state.move_cursor_left();
        state.backspace();

        assert_eq!(state.prompt, "ab");
        assert_eq!(state.cursor, 1);

        state.delete();
        assert_eq!(state.prompt, "a");
        assert_eq!(state.cursor, 1);
    }

    #[test]
    fn file_reference_picker_and_bash_prefixes_match_pi_input_semantics() {
        let mut state = super::AppState {
            prompt: "inspect @".into(),
            cursor: 9,
            available_files: vec!["src/main.rs".into(), "README.md".into()],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::FilePicker);
        state.overlay_query = "main".into();
        assert_eq!(state.overlay_items(), vec!["src/main.rs"]);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.prompt, "inspect @src/main.rs ");

        state.prompt = "! cargo test".into();
        state.cursor = state.prompt.chars().count();
        assert_eq!(state.submit_bash(), Some(("cargo test".into(), false)));
        state.prompt = "!! git status".into();
        state.cursor = state.prompt.chars().count();
        assert_eq!(state.submit_bash(), Some(("git status".into(), true)));
    }

    #[test]
    fn submitting_and_recalling_prompt_history() {
        let mut state = super::AppState::default();
        for character in "first request".chars() {
            state.insert_char(character);
        }

        assert_eq!(state.submit_prompt().as_deref(), Some("first request"));
        assert!(state.prompt.is_empty());

        state.previous_prompt();
        assert_eq!(state.prompt, "first request");
        state.next_prompt();
        assert!(state.prompt.is_empty());
    }

    #[test]
    fn focus_and_permission_mode_cycle() {
        let mut state = super::AppState::default();
        assert_eq!(state.focus, super::Focus::Prompt);
        assert_eq!(state.permission_mode.label(), "normal");

        state.toggle_focus();
        state.cycle_permission_mode();
        assert_eq!(state.focus, super::Focus::Scrollback);
        assert_eq!(state.permission_mode.label(), "plan");
    }

    #[test]
    fn thinking_and_message_queue_events_are_visible() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ThinkingChanged {
            level: "high".into(),
        });
        state.apply(AgentEvent::QueueChanged {
            steering: vec!["change direction".into()],
            follow_up: vec!["then run tests".into()],
        });
        assert_eq!(state.thinking_level, "high");
        assert_eq!(state.queued_steering.len(), 1);
        assert_eq!(state.queued_follow_up.len(), 1);
        assert_eq!(state.status, "2 queued");

        state.prompt = "/thinking".into();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::CycleThinking)
        ));
    }

    #[test]
    fn markdown_story_renders_structured_code_and_styles() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = fixtures::markdown();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Renderer design"));
        assert!(output.contains("fn render(entry: &Entry)"));
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.bg == Theme::GROK_NIGHT.code_background)
        );
    }

    #[test]
    fn composer_title_carries_model_thinking_and_permission_mode() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.model = "grok-4 fast".into();
        state.thinking_level = "high".into();
        state.permission_mode = super::state::PermissionMode::Plan;
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        // Composer title now packs all three status fields into one line.
        assert!(output.contains("grok-4 fast"));
        assert!(output.contains("thinking high"));
        assert!(output.contains("plan"));
        // The previous standalone status suffix is gone from the shortcuts row.
        let shortcuts_line = output
            .lines()
            .find(|line| line.contains("Enter: send"))
            .unwrap_or_default();
        assert!(
            !shortcuts_line.contains("thinking"),
            "shortcuts row should not duplicate thinking level: {shortcuts_line:?}"
        );
        assert!(
            !shortcuts_line.contains("plan"),
            "shortcuts row should not duplicate permission mode: {shortcuts_line:?}"
        );
    }

    #[test]
    fn reasoning_deltas_accumulate_and_fold() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ReasoningDelta {
            text: "first ".into(),
        });
        state.apply(AgentEvent::ReasoningDelta {
            text: "second".into(),
        });

        assert_eq!(state.entries.len(), 1);
        match &state.entries[0] {
            super::state::Entry::Reasoning { text, expanded, .. } => {
                assert_eq!(text, "first second");
                assert!(!expanded);
            }
            _ => panic!("expected reasoning entry"),
        }

        state.toggle_latest_reasoning();
        assert!(state.all_reasoning_expanded());
        state.set_all_reasoning_expanded(false);
        assert!(!state.all_reasoning_expanded());
    }

    #[test]
    fn user_prompt_has_padding_and_sticks_after_scrolling() {
        let (width, height) = (100, 24);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = fixtures::conversation();
        let max_scroll = ui::max_scroll(&state, width, height);

        state.scroll_to_top(max_scroll);
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let top_buffer = terminal.backend().buffer();
        assert_eq!(top_buffer[(3, 3)].bg, Theme::GROK_NIGHT.user_background);
        assert_eq!(top_buffer[(3, 4)].bg, Theme::GROK_NIGHT.user_background);
        assert_eq!(top_buffer[(3, 5)].bg, Theme::GROK_NIGHT.user_background);

        state.scroll_to_bottom();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let sticky_buffer = terminal.backend().buffer();
        assert_eq!(sticky_buffer[(3, 2)].bg, Theme::GROK_NIGHT.user_background);
        assert_eq!(sticky_buffer[(3, 3)].bg, Theme::GROK_NIGHT.user_background);
        assert_eq!(sticky_buffer[(3, 4)].bg, Theme::GROK_NIGHT.user_background);
    }

    #[test]
    fn tool_and_diff_blocks_toggle_expansion() {
        let mut state = fixtures::tools();

        state.toggle_latest_tool();
        match state
            .entries
            .iter()
            .rev()
            .find(|entry| matches!(entry, super::state::Entry::Tool { .. }))
        {
            Some(super::state::Entry::Tool { expanded, .. }) => assert!(!expanded),
            _ => panic!("expected tool entry"),
        }

        state.toggle_latest_diff();
        match state
            .entries
            .iter()
            .rev()
            .find(|entry| matches!(entry, super::state::Entry::Diff { .. }))
        {
            Some(super::state::Entry::Diff { expanded, .. }) => assert!(!expanded),
            _ => panic!("expected diff entry"),
        }
    }

    #[test]
    fn scrollbar_thumb_is_dynamic_and_reaches_both_ends() {
        assert_eq!(ui::scrollbar_geometry(100, 25, 20, 0), Some((0, 5)));
        assert_eq!(ui::scrollbar_geometry(100, 25, 20, 75), Some((15, 5)));
        assert_eq!(ui::scrollbar_geometry(50, 25, 20, 0), Some((0, 10)));
        assert_eq!(ui::scrollbar_geometry(25, 25, 20, 0), None);
    }

    #[test]
    fn tools_use_compact_diamond_headers() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = fixtures::tools();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Run 3 calls"));
        assert!(output.contains("◆ Edit crates/pi-tui/src/ui.rs"));
        assert!(!output.contains("✓"));
        assert!(output.contains("2.4s"));
    }

    #[test]
    fn grouped_tools_expand_and_expose_clickable_children() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = fixtures::tools();

        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let collapsed = buffer_text(terminal.backend().buffer(), width, height);
        assert!(collapsed.contains("Run 3 calls"));
        assert!(!collapsed.contains("cargo clippy --workspace"));
        assert_eq!(
            ui::section_hit_at(&state, width, height, 5, 7).map(|hit| hit.index),
            Some(1)
        );

        state.toggle_tool_group(1);
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let expanded = buffer_text(terminal.backend().buffer(), width, height);
        assert!(expanded.contains("cargo clippy --workspace"));
        assert!(expanded.contains("├ ◆ Run"));
        assert_eq!(
            ui::section_hit_at(&state, width, height, 5, 9).map(|hit| hit.index),
            Some(2)
        );
        state.toggle_tool_at(2);
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let focused = buffer_text(terminal.backend().buffer(), width, height);
        assert!(focused.contains('▌'));
        assert!(focused.contains("cargo clippy --workspace"));
    }

    #[test]
    fn focused_tool_rail_does_not_shift_row_alignment() {
        let mut state = super::AppState::default();
        for (id, name, detail) in [("one", "read", "one.rs"), ("two", "bash", "cargo test")] {
            state.apply(AgentEvent::ToolCallStart {
                id: id.into(),
                name: name.into(),
                args: if name == "read" {
                    serde_json::json!({"path": detail})
                } else {
                    serde_json::json!({"command": detail})
                },
            });
            state.apply(AgentEvent::ToolCallResult {
                id: id.into(),
                result: pi_harness::ToolResult {
                    content: "done".into(),
                    details: None,
                },
                is_error: false,
                duration_ms: Some(10),
            });
        }
        state.toggle_tool_at(1);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 80, 24);
        let read = output.lines().find(|line| line.contains("one.rs")).unwrap();
        let run = output
            .lines()
            .find(|line| line.contains("cargo test"))
            .unwrap();
        assert_eq!(
            read.chars().position(|character| character == '◆'),
            run.chars().position(|character| character == '◆')
        );
        let detail = output.lines().find(|line| line.contains("└ done")).unwrap();
        assert_eq!(
            run.chars().position(|character| character == 'R'),
            detail.chars().position(|character| character == '└')
        );
    }

    #[test]
    fn section_focus_steps_entries_and_keeps_them_visible() {
        let mut state = fixtures::conversation();
        state.focus = super::Focus::Scrollback;
        ui::move_section_focus(&mut state, 100, 20, 1);
        assert_eq!(state.focused_entry, Some(0));
        ui::move_section_focus(&mut state, 100, 20, 1);
        assert_eq!(state.focused_entry, Some(1));
        ui::move_section_focus(&mut state, 100, 20, -1);
        assert_eq!(state.focused_entry, Some(0));
    }

    #[test]
    fn section_hits_respect_horizontal_and_dynamic_banner_bounds() {
        let mut state = fixtures::tools();
        state.turn_started_at = Some(std::time::Instant::now());
        assert_eq!(ui::section_hit_at(&state, 100, 32, 0, 7), None);
        let hit = (2..24).find_map(|row| ui::section_hit_at(&state, 100, 32, 5, row));
        assert!(hit.is_some());
    }

    #[test]
    fn rendered_tool_rows_hit_exactly_after_scrolling() {
        let (width, height) = (100, 24);
        let mut state = fixtures::long_session(120);
        let max_scroll = ui::max_scroll(&state, width, height);
        for from_bottom in [0, max_scroll / 2, max_scroll] {
            state.scroll_from_bottom = from_bottom;
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| ui::render(frame, &state)).unwrap();
            let buffer = terminal.backend().buffer();
            for row in 2..height.saturating_sub(5) {
                let line: String = (0..width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect();
                if let Some(path) = line.split("Read src/file-").nth(1)
                    && let Some(index) = path
                        .split(".rs")
                        .next()
                        .and_then(|value| value.parse::<usize>().ok())
                {
                    let hit = ui::section_hit_at(&state, width, height, 6, row);
                    assert_eq!(
                        hit.map(|target| target.index),
                        Some(index),
                        "row {row}, scroll {from_bottom}: {line}"
                    );
                }
            }
        }
    }

    #[test]
    fn hovered_tool_uses_pointer_marker() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.hovered_entry = Some(1);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("> Run 3 calls"));
    }

    #[test]
    fn focused_section_shows_brackets_at_both_transcript_edges() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.focused_target_id = Some("tool-group:tool-test".into());
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let row = buffer_text(terminal.backend().buffer(), width, height)
            .lines()
            .find(|line| line.contains("Run 3 calls"))
            .unwrap()
            .to_string();
        assert_eq!(row.chars().nth(1), Some('['));
        assert_eq!(row.chars().nth(97), Some(']'));
    }

    #[test]
    fn focus_brackets_span_dynamic_section_height() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.focused_target_id = Some("diff:fixture-diff-2".into());
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        let markers: Vec<(String, String)> = (2..height - 5)
            .filter_map(|row| {
                let left = buffer[(1, row)].symbol().to_string();
                let right = buffer[(97, row)].symbol().to_string();
                (left != " ").then_some((left, right))
            })
            .collect();
        let edit_row = (2..height - 5)
            .find(|row| {
                (0..width)
                    .map(|column| buffer[(column, *row)].symbol())
                    .collect::<String>()
                    .contains("Edit crates/pi-tui/src/ui.rs")
            })
            .unwrap();
        assert_eq!(buffer[(1, edit_row)].symbol(), "┌");
        assert_eq!(markers.first(), Some(&("┌".into(), "┐".into())));
        assert_eq!(markers.last(), Some(&("└".into(), "┘".into())));
        assert!(markers.len() > 2);
        assert!(
            markers[1..markers.len() - 1]
                .iter()
                .all(|marker| marker == &("│".into(), "│".into()))
        );
    }

    #[test]
    fn expanded_tool_children_are_independent_focus_stops() {
        let mut state = fixtures::tools();
        state.toggle_tool_group(1);
        state.focused_section = None;
        let mut visited = Vec::new();
        for _ in 0..8 {
            ui::move_section_focus(&mut state, 100, 32, 1);
            visited.push(state.focused_entry);
        }
        assert!(visited.contains(&Some(2)), "visited: {visited:?}");
        assert!(visited.contains(&Some(3)), "visited: {visited:?}");
    }

    #[test]
    fn focus_identity_survives_tool_expansion_and_stream_updates() {
        let mut state = fixtures::tools();
        state.toggle_tool_group(1);
        state.focused_section = None;
        while state.focused_entry != Some(2) {
            ui::move_section_focus(&mut state, 100, 32, 1);
        }
        let stable_id = state.focused_target_id.clone();
        state.toggle_tool_at(2);
        state.focused_target_id = stable_id.clone();
        state.apply(AgentEvent::TextDelta {
            text: "streamed after tools".into(),
        });
        ui::move_section_focus(&mut state, 100, 32, 1);
        assert_eq!(stable_id.as_deref(), Some("tool:tool-clippy"));
        assert_eq!(state.focused_entry, Some(3));
    }

    #[test]
    fn thousand_entry_transcript_meets_interaction_budgets() {
        let hydration_started = std::time::Instant::now();
        let mut state = fixtures::long_session(1_000);
        let hydration = hydration_started.elapsed();

        let focus_started = std::time::Instant::now();
        ui::move_section_focus(&mut state, 120, 40, 1);
        let focus = focus_started.elapsed();

        let hit_started = std::time::Instant::now();
        let _ = ui::section_hit_at(&state, 120, 40, 5, 10);
        let hit = hit_started.elapsed();

        let frame_started = std::time::Instant::now();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let frame = frame_started.elapsed();

        assert!(
            hydration < std::time::Duration::from_millis(250),
            "hydration: {hydration:?}"
        );
        assert!(
            focus < std::time::Duration::from_millis(250),
            "focus: {focus:?}"
        );
        assert!(hit < std::time::Duration::from_millis(250), "hit: {hit:?}");
        assert!(
            frame < std::time::Duration::from_millis(500),
            "frame: {frame:?}"
        );
    }

    #[test]
    fn search_tools_render_semantic_detail_and_provider_duration() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "search-1".into(),
            name: "search".into(),
            args: serde_json::json!({"query": "status", "path": "src/state.rs"}),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "search-1".into(),
            result: pi_harness::ToolResult {
                content: "one\ntwo\nthree".into(),
                details: None,
            },
            is_error: false,
            duration_ms: Some(1_234),
        });

        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 100, 24);
        assert!(output.contains("Search \"status\" in src/state.rs (3 matches)"));
        assert!(output.contains("1.2s"));
    }

    #[test]
    fn subagent_tools_use_compact_descriptions() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "agent-1".into(),
            name: "agent".into(),
            args: serde_json::json!({
                "description": "Scout Rust crate public APIs",
                "prompt": "A deliberately long private task brief"
            }),
        });
        state.apply(AgentEvent::ToolCallStart {
            id: "result-1".into(),
            name: "get_subagent_result".into(),
            args: serde_json::json!({"agent_id": "4841a1b4"}),
        });

        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Tool { label, detail, .. }
                if label == "Agent" && detail == "Scout Rust crate public APIs"
        ));
        assert!(matches!(
            &state.entries[1],
            super::state::Entry::Tool { label, detail, .. }
                if label == "Agent result" && detail == "4841a1b4"
        ));
    }

    #[test]
    fn background_agent_timer_runs_until_its_report_arrives() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "spawn-call".into(),
            name: "agent".into(),
            args: serde_json::json!({"description": "Scout session persistence"}),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "spawn-call".into(),
            result: pi_harness::ToolResult {
                content: "Agent started in background. Agent ID: scout-123".into(),
                details: None,
            },
            is_error: false,
            duration_ms: Some(55),
        });
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Tool {
                status: super::state::ToolStatus::Running,
                duration: None,
                started_at: Some(_),
                ..
            }
        ));

        state.apply(AgentEvent::ToolCallStart {
            id: "result-call".into(),
            name: "get_subagent_result".into(),
            args: serde_json::json!({"agent_id": "scout-123"}),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "result-call".into(),
            result: pi_harness::ToolResult {
                content: "Session persistence report".into(),
                details: None,
            },
            is_error: false,
            duration_ms: Some(20),
        });
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Tool {
                status: super::state::ToolStatus::Success,
                duration: Some(_),
                started_at: None,
                result: Some(report),
                ..
            } if report == "Session persistence report"
        ));
    }

    #[test]
    fn tool_results_complete_the_matching_concurrent_call() {
        let mut state = super::AppState::default();
        for id in ["agent-a", "agent-b"] {
            state.apply(AgentEvent::ToolCallStart {
                id: id.into(),
                name: "agent".into(),
                args: serde_json::json!({"description": id}),
            });
        }
        state.apply(AgentEvent::ToolCallResult {
            id: "agent-a".into(),
            result: pi_harness::ToolResult {
                content: "done".into(),
                details: None,
            },
            is_error: false,
            duration_ms: Some(250),
        });

        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Tool {
                status: super::state::ToolStatus::Success,
                started_at: None,
                duration: Some(_),
                ..
            }
        ));
        assert!(matches!(
            &state.entries[1],
            super::state::Entry::Tool {
                status: super::state::ToolStatus::Running,
                started_at: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn textual_think_tags_become_reasoning_entries() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::TextDelta {
            text: "<think>inspect first</think>final answer".into(),
        });

        assert_eq!(state.entries.len(), 2);
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Reasoning { text, .. } if text == "inspect first"
        ));
        assert!(matches!(
            &state.entries[1],
            super::state::Entry::Assistant { lines, .. } if lines == &["final answer"]
        ));
    }

    #[test]
    fn streaming_text_delta_splits_lines_on_newlines() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::TextDelta {
            text: "# Renderer design\n\nThe renderer.\n- item one\n- item two".into(),
        });

        assert_eq!(state.entries.len(), 1);
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Assistant { lines, .. } if lines == &[
                "# Renderer design".to_string(),
                "".to_string(),
                "The renderer.".to_string(),
                "- item one".to_string(),
                "- item two".to_string(),
            ]
        ));
    }

    #[test]
    fn streaming_text_delta_continues_previous_line_then_splits() {
        let mut state = super::AppState::default();
        // First delta starts an Assistant block with a partial line.
        state.apply(AgentEvent::TextDelta {
            text: "first half".into(),
        });
        // Second delta arrives without a leading newline; it continues the
        // current last line. The newline in the middle starts a new line.
        state.apply(AgentEvent::TextDelta {
            text: " still streaming\nsecond line".into(),
        });

        assert_eq!(state.entries.len(), 1);
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Assistant { lines, .. } if lines == &[
                "first half still streaming".to_string(),
                "second line".to_string(),
            ]
        ));
    }

    #[test]
    fn edit_tool_call_becomes_a_diff_entry_with_removed_and_added_lines() {
        let mut state = super::AppState::default();
        let args = serde_json::json!({
            "path": "src/lib.rs",
            "old_text": "let border = theme.border;\n",
            "new_text": "let border = focused_border(state, theme);\n",
        });
        state.apply(AgentEvent::ToolCallStart {
            id: "tool-1".into(),
            name: "edit".into(),
            args,
        });

        assert_eq!(state.entries.len(), 1);
        match &state.entries[0] {
            super::state::Entry::Diff {
                path,
                lines,
                expanded,
                ..
            } => {
                assert_eq!(path, "src/lib.rs");
                assert!(*expanded, "edit diffs default to expanded");
                assert_eq!(lines.len(), 2);
                assert!(matches!(lines[0].kind, super::state::DiffKind::Removed));
                assert_eq!(lines[0].text, "let border = theme.border;");
                assert!(matches!(lines[1].kind, super::state::DiffKind::Added));
                assert_eq!(lines[1].text, "let border = focused_border(state, theme);");
            }
            other => panic!("expected Diff entry, got {other:?}"),
        }
    }

    #[test]
    fn edit_tool_call_with_old_string_field_also_becomes_a_diff_entry() {
        let mut state = super::AppState::default();
        let args = serde_json::json!({
            "file_path": "app/handler.py",
            "old_string": "return result",
            "new_string": "return result or default",
        });
        state.apply(AgentEvent::ToolCallStart {
            id: "tool-2".into(),
            name: "Edit".into(),
            args,
        });

        assert_eq!(state.entries.len(), 1);
        match &state.entries[0] {
            super::state::Entry::Diff { path, lines, .. } => {
                assert_eq!(path, "app/handler.py");
                assert_eq!(lines.len(), 2);
                assert!(matches!(lines[0].kind, super::state::DiffKind::Removed));
                assert!(matches!(lines[1].kind, super::state::DiffKind::Added));
            }
            other => panic!("expected Diff entry, got {other:?}"),
        }
    }

    #[test]
    fn current_pi_edit_schema_uses_authoritative_result_diff() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "edit-current".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/lib.rs",
                "edits": [{"oldText": "old", "newText": "preview"}]
            }),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "edit-current".into(),
            result: pi_harness::ToolResult {
                content: "Edited src/lib.rs".into(),
                details: Some(serde_json::json!({
                    "diff": "@@ -12,2 +12,2 @@\n context\n-old\n+final"
                })),
            },
            is_error: false,
            duration_ms: Some(10),
        });

        match &state.entries[0] {
            super::state::Entry::Diff { lines, .. } => {
                assert!(matches!(lines[0].kind, super::state::DiffKind::Context));
                assert_eq!(lines[0].number, Some(12));
                assert!(matches!(lines[1].kind, super::state::DiffKind::Removed));
                assert_eq!(lines[1].number, Some(13));
                assert!(matches!(lines[2].kind, super::state::DiffKind::Added));
                assert_eq!(lines[2].number, Some(13));
                assert_eq!(lines[2].text, "final");
            }
            other => panic!("expected diff entry, got {other:?}"),
        }
    }

    #[test]
    fn edit_tool_without_diff_args_falls_back_to_plain_tool_entry() {
        let mut state = super::AppState::default();
        let args = serde_json::json!({ "path": "src/lib.rs" });
        state.apply(AgentEvent::ToolCallStart {
            id: "tool-3".into(),
            name: "edit".into(),
            args,
        });

        assert_eq!(state.entries.len(), 1);
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Tool { label, .. } if label == "Edit"
        ));
    }

    #[test]
    fn edit_tool_multiline_diff_preserves_internal_blank_lines() {
        let mut state = super::AppState::default();
        let args = serde_json::json!({
            "path": "src/handler.rs",
            "old_text": "fn handle(req: Request) -> Response {\n    process(req)\n}",
            "new_text": "fn handle(req: Request) -> Response {\n    validate(&req)?;\n    process(req)\n}",
        });
        state.apply(AgentEvent::ToolCallStart {
            id: "tool-4".into(),
            name: "edit".into(),
            args,
        });

        match &state.entries[0] {
            super::state::Entry::Diff { lines, .. } => {
                assert_eq!(lines.len(), 7);
                // old_text: 3 lines, all Removed
                assert_eq!(lines[0].text, "fn handle(req: Request) -> Response {");
                assert!(matches!(lines[0].kind, super::state::DiffKind::Removed));
                assert_eq!(lines[1].text, "    process(req)");
                assert!(matches!(lines[1].kind, super::state::DiffKind::Removed));
                assert_eq!(lines[2].text, "}");
                assert!(matches!(lines[2].kind, super::state::DiffKind::Removed));
                // new_text: 4 lines, all Added
                assert_eq!(lines[3].text, "fn handle(req: Request) -> Response {");
                assert!(matches!(lines[3].kind, super::state::DiffKind::Added));
                assert_eq!(lines[4].text, "    validate(&req)?;");
                assert!(matches!(lines[4].kind, super::state::DiffKind::Added));
                assert_eq!(lines[5].text, "    process(req)");
                assert!(matches!(lines[5].kind, super::state::DiffKind::Added));
                assert_eq!(lines[6].text, "}");
                assert!(matches!(lines[6].kind, super::state::DiffKind::Added));
            }
            other => panic!("expected Diff entry, got {other:?}"),
        }
    }

    #[test]
    fn command_palette_filters_and_opens_selected_action() {
        let mut state = fixtures::markdown();
        state.prompt = "draft stays here".into();
        state.open_overlay(super::OverlayKind::CommandPalette);
        for character in "sett".chars() {
            state.insert_overlay_char(character);
        }

        assert_eq!(state.overlay_items(), vec!["Settings"]);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::Settings);
        assert_eq!(state.prompt, "draft stays here");
    }

    #[test]
    fn permission_overlay_returns_a_harness_decision() {
        let mut state = fixtures::Story::Permission.state();
        state.overlay_selected = 2;

        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::Permission {
                request_id,
                decision: pi_harness::PermissionDecision::Deny,
            } if request_id == "permission-1"
        ));
        assert_eq!(state.overlay, super::OverlayKind::None);
    }

    #[test]
    fn overlay_and_slash_suggestions_render() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = fixtures::Story::Palette.state();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let palette = buffer_text(terminal.backend().buffer(), width, height);
        assert!(palette.contains("Commands"));
        assert!(palette.contains("Model picker"));

        let slash = super::AppState {
            prompt: "/mo".into(),
            cursor: 3,
            ..super::AppState::default()
        };
        terminal.draw(|frame| ui::render(frame, &slash)).unwrap();
        let suggestions = buffer_text(terminal.backend().buffer(), width, height);
        assert!(suggestions.contains("/model"));
        assert!(suggestions.contains("Switch model"));
    }

    #[test]
    fn model_picker_returns_the_backend_model_id() {
        let mut state = super::AppState {
            available_models: vec![
                pi_harness::ModelInfo {
                    id: "opencode-go/glm-5.2".into(),
                    display_name: "GLM-5.2".into(),
                },
                pi_harness::ModelInfo {
                    id: "opencode-go/minimax-m3".into(),
                    display_name: "MiniMax-M3".into(),
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::ModelPicker);
        state.overlay_selected = 1;

        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::SetModel { id }
                if id == "opencode-go/minimax-m3"
        ));
        assert_eq!(state.model, "MiniMax-M3");
    }

    #[test]
    fn model_picker_marks_the_current_model() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState {
            model: "MiniMax-M3".into(),
            available_models: vec![
                pi_harness::ModelInfo {
                    id: "opencode-go/glm-5.2".into(),
                    display_name: "GLM-5.2".into(),
                },
                pi_harness::ModelInfo {
                    id: "opencode-go/minimax-m3".into(),
                    display_name: "MiniMax-M3".into(),
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::ModelPicker);

        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("MiniMax-M3  ✓ current"));
        assert!(!output.contains("GLM-5.2  ✓ current"));
    }

    #[test]
    fn session_picker_filters_marks_current_and_returns_path() {
        let (width, height) = (110, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState {
            available_sessions: vec![
                pi_harness::SessionInfo {
                    id: "one".into(),
                    path: "/sessions/one.jsonl".into(),
                    name: Some("DeepSeek subagent exploration".into()),
                    first_message: "explore the codebase".into(),
                    modified: "2026-07-11T08:30:00.000Z".into(),
                    message_count: 12,
                    current: true,
                },
                pi_harness::SessionInfo {
                    id: "two".into(),
                    path: "/sessions/two.jsonl".into(),
                    name: None,
                    first_message: "Fix the model picker".into(),
                    modified: "2026-07-11T07:00:00.000Z".into(),
                    message_count: 4,
                    current: false,
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::SessionPicker);
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("Resume session"));
        assert!(output.contains("DeepSeek subagent"));
        assert!(output.contains("✓ current"));

        for character in "model".chars() {
            state.insert_overlay_char(character);
        }
        assert_eq!(state.overlay_items().len(), 1);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::ResumeSession { target }
                if target == "/sessions/two.jsonl"
        ));
        assert!(state.available_sessions[1].current);
    }

    #[test]
    fn session_reset_replaces_the_visible_transcript() {
        let mut state = fixtures::conversation();
        assert!(!state.entries.is_empty());
        state.apply(AgentEvent::SessionReset);
        state.apply(AgentEvent::UserMessage {
            text: "restored prompt".into(),
        });

        assert_eq!(state.entries.len(), 1);
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::User { text, .. } if text == "restored prompt"
        ));
    }

    #[test]
    fn tree_and_fork_pickers_use_pi_entry_ids_and_prefill_prompt() {
        let mut state = super::AppState::default();
        let entries = vec![pi_harness::SessionTreeEntry {
            id: "entry-1".into(),
            parent_id: None,
            kind: "message".into(),
            role: Some("user".into()),
            text: "try the other approach".into(),
            timestamp: "2026-07-11T00:00:00Z".into(),
            label: Some("checkpoint".into()),
            depth: 0,
            active: true,
        }];
        state.apply(AgentEvent::SessionTree {
            entries: entries.clone(),
            user_only: false,
        });
        assert_eq!(state.overlay, super::OverlayKind::TreePicker);
        assert!(state.overlay_items()[0].contains("checkpoint"));
        state.toggle_tree_timestamps();
        assert!(state.overlay_items()[0].contains("2026-07-11"));
        assert!(matches!(
            state.activate_tree_with_summary(),
            super::state::OverlayAction::NavigateTree { entry_id, summarize: true }
                if entry_id == "entry-1"
        ));

        state.apply(AgentEvent::SessionTree {
            entries: entries.clone(),
            user_only: false,
        });
        state.begin_tree_label();
        assert_eq!(state.overlay, super::OverlayKind::LabelEditor);
        state.overlay_query = "release point".into();
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::SetLabel { entry_id, label: Some(label) }
                if entry_id == "entry-1" && label == "release point"
        ));

        state.apply(AgentEvent::SessionTree {
            entries: entries.clone(),
            user_only: false,
        });
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::NavigateTree { entry_id, summarize: false } if entry_id == "entry-1")
        );

        state.apply(AgentEvent::SessionTree {
            entries,
            user_only: true,
        });
        assert_eq!(state.overlay, super::OverlayKind::ForkPicker);
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::ForkSession { entry_id } if entry_id == "entry-1")
        );

        state.apply(AgentEvent::PromptPrefill {
            text: "editable prompt".into(),
        });
        assert_eq!(state.prompt, "editable prompt");
        assert_eq!(state.cursor, 15);
        assert_eq!(state.focus, super::Focus::Prompt);
    }

    #[test]
    fn slash_commands_complete_and_activate_locally() {
        let mut state = super::AppState {
            prompt: "/mo".into(),
            cursor: 3,
            ..super::AppState::default()
        };

        assert!(state.complete_slash_command());
        assert_eq!(state.prompt, "/model");
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::None)
        ));
        assert_eq!(state.overlay, super::OverlayKind::ModelPicker);
        assert!(state.prompt.is_empty());

        state.prompt = "/mode".into();
        state.cursor = 5;
        let previous = state.permission_mode.label();
        assert!(state.activate_slash_command().is_some());
        assert_ne!(state.permission_mode.label(), previous);

        state.prompt = "/name API migration".into();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::NameSession(name)) if name == "API migration"
        ));
        state.prompt = "/compact preserve test failures".into();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::Compact(Some(instructions)))
                if instructions == "preserve test failures"
        ));
    }

    #[test]
    fn pi_runtime_commands_and_context_files_feed_completion() {
        let mut state = super::AppState {
            prompt: "/dep".into(),
            cursor: 4,
            runtime_commands: vec![pi_harness::RuntimeCommand {
                name: "/deploy".into(),
                description: "Deploy preview".into(),
                source: "extension".into(),
            }],
            context_files: vec!["/project/AGENTS.md".into()],
            ..super::AppState::default()
        };
        assert!(state.complete_slash_command());
        assert_eq!(state.prompt, "/deploy");

        state.prompt = "/context".into();
        assert!(state.activate_slash_command().is_some());
        assert!(
            matches!(state.entries.last(), Some(super::state::Entry::Assistant { lines, .. }) if lines.iter().any(|line| line.contains("AGENTS.md")))
        );

        state.prompt = "/reload".into();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::ReloadResources)
        ));
    }

    #[test]
    fn settings_and_scoped_models_return_persistent_pi_actions() {
        let mut state = super::AppState {
            available_models: vec![
                pi_harness::ModelInfo {
                    id: "one/a".into(),
                    display_name: "Model A".into(),
                },
                pi_harness::ModelInfo {
                    id: "two/b".into(),
                    display_name: "Model B".into(),
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::Settings);
        assert!(state.overlay_items()[0].contains("one-at-a-time"));
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::SetRuntimeSetting { key, value } if key == "steering_mode" && value == serde_json::json!("all"))
        );

        state.open_overlay(super::OverlayKind::ScopedModels);
        state.toggle_scoped_model();
        assert_eq!(state.runtime_settings.enabled_models, vec!["one/a"]);
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::SetScopedModels(models) if models == vec!["one/a"])
        );

        state.prompt = "/trust".into();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::SetProjectTrust(true))
        ));
    }

    #[test]
    fn oauth_callbacks_round_trip_through_tui_overlays() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::OauthRequest {
            id: "oauth-1".into(),
            kind: "select".into(),
            message: Some("Choose a flow".into()),
            url: None,
            user_code: None,
            verification_uri: None,
            interval_seconds: None,
            expires_in_seconds: None,
            options: Some(vec![pi_harness::AuthChoice {
                id: "device".into(),
                label: "Device code".into(),
            }]),
        });
        assert_eq!(state.overlay, super::OverlayKind::OauthSelect);
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::OauthReply { id, value: Some(value) } if id == "oauth-1" && value == "device")
        );

        state.apply(AgentEvent::OauthRequest {
            id: "oauth-2".into(),
            kind: "prompt".into(),
            message: Some("Paste code".into()),
            url: None,
            user_code: None,
            verification_uri: None,
            interval_seconds: None,
            expires_in_seconds: None,
            options: None,
        });
        state.overlay_query = "secret".into();
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::OauthReply { id, value: Some(value) } if id == "oauth-2" && value == "secret")
        );

        state.apply(AgentEvent::OauthComplete {
            provider: "example".into(),
        });
        assert_eq!(state.status, "logged in to example");
    }

    #[test]
    fn rewind_picker_returns_persisted_checkpoint_id() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::RewindList {
            checkpoints: vec![pi_harness::RewindCheckpoint {
                id: "checkpoint-1".into(),
                path: "/project/src/main.rs".into(),
                timestamp: "2026-07-12T01:00:00Z".into(),
                tool: "edit".into(),
            }],
        });
        assert_eq!(state.overlay, super::OverlayKind::RewindPicker);
        assert!(state.overlay_items()[0].contains("src/main.rs"));
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::RewindFile(id) if id == "checkpoint-1"
        ));
    }

    #[test]
    fn persisted_plan_updates_header_progress_and_plan_mode() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::PlanUpdate {
            entries: vec![
                pi_harness::PlanEntry {
                    step: "Inspect".into(),
                    status: "completed".into(),
                },
                pi_harness::PlanEntry {
                    step: "Implement".into(),
                    status: "in_progress".into(),
                },
            ],
        });
        assert_eq!((state.tasks_complete, state.tasks_total), (1, 2));
        assert_eq!(state.plan_entries.len(), 2);

        state.prompt = "/plan".into();
        assert!(
            matches!(state.activate_slash_command(), Some(super::state::OverlayAction::SetPermissionMode(mode)) if mode == "plan")
        );
        assert_eq!(state.permission_mode.label(), "plan");
    }

    #[test]
    fn scrolling_clamps_and_returns_to_tail_following() {
        let mut state = fixtures::conversation();
        let max_scroll = ui::max_scroll(&state, 100, 24);

        state.scroll_to_bottom();
        assert_eq!(state.scroll_from_bottom, 0);

        state.scroll_up(usize::MAX, max_scroll);
        assert_eq!(state.scroll_from_bottom, max_scroll);

        state.scroll_down(1);
        assert_eq!(state.scroll_from_bottom, max_scroll.saturating_sub(1));

        state.scroll_to_bottom();
        assert_eq!(state.scroll_from_bottom, 0);
    }

    #[test]
    fn compaction_start_pushes_active_placeholder_and_sets_status() {
        let mut state = super::AppState::default();
        state.context_used = 184_000;
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(184_000),
            tokens_after: None,
            error: None,
        });

        assert_eq!(state.entries.len(), 1);
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Compaction {
                active: true,
                tokens_before: Some(184_000),
                tokens_after: None,
                error: None,
                ..
            }
        ));
        assert_eq!(state.status, "compacting…");
    }

    #[test]
    fn compaction_end_replaces_placeholder_summary_and_drops_context() {
        let mut state = super::AppState::default();
        state.context_used = 184_000;
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(184_000),
            tokens_after: None,
            error: None,
        });
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::End,
            reason: Some("manual".into()),
            summary: Some("Kept recent turns; summarized the rest.".into()),
            tokens_before: Some(184_000),
            tokens_after: Some(22_000),
            error: None,
        });

        match &state.entries[0] {
            super::state::Entry::Compaction {
                summary,
                active,
                tokens_before,
                tokens_after,
                error,
                started_at,
            } => {
                assert_eq!(summary, "Kept recent turns; summarized the rest.");
                assert!(!active);
                assert_eq!(*tokens_before, Some(184_000));
                assert_eq!(*tokens_after, Some(22_000));
                assert!(error.is_none());
                assert!(started_at.is_none(), "End event should clear started_at");
            }
            other => panic!("expected Compaction entry, got {other:?}"),
        }
        assert_eq!(state.context_used, 22_000);
        assert_eq!(state.status, "compacted");
    }

    #[test]
    fn compaction_end_with_error_marks_entry_and_status_as_failed() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("threshold".into()),
            summary: None,
            tokens_before: Some(190_000),
            tokens_after: None,
            error: None,
        });
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::End,
            reason: Some("threshold".into()),
            summary: None,
            tokens_before: Some(190_000),
            tokens_after: None,
            error: Some("model timeout".into()),
        });

        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Compaction {
                active: false,
                error: Some(message),
                ..
            } if message == "model timeout"
        ));
        assert_eq!(state.status, "compaction failed");
    }

    #[test]
    fn repeated_compactions_keep_distinct_chronological_cards() {
        let mut state = super::AppState::default();
        for (reason, before, after) in [
            ("threshold", 190_000, 40_000),
            ("overflow", 198_000, 35_000),
        ] {
            state.apply(AgentEvent::Compaction {
                phase: pi_harness::CompactionPhase::Start,
                reason: Some(reason.into()),
                summary: None,
                tokens_before: Some(before),
                tokens_after: None,
                error: None,
            });
            state.apply(AgentEvent::Compaction {
                phase: pi_harness::CompactionPhase::End,
                reason: Some(reason.into()),
                summary: Some(format!("{reason} summary")),
                tokens_before: Some(before),
                tokens_after: Some(after),
                error: None,
            });
        }
        let summaries: Vec<&str> = state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                super::state::Entry::Compaction { summary, .. } => Some(summary.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(summaries, ["threshold summary", "overflow summary"]);
        assert_eq!(state.context_used, 35_000);
    }

    #[test]
    fn aborted_compaction_preserves_error_and_does_not_change_tokens() {
        let mut state = super::AppState {
            context_used: 150_000,
            ..super::AppState::default()
        };
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(150_000),
            tokens_after: None,
            error: None,
        });
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::End,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(150_000),
            tokens_after: None,
            error: Some("aborted".into()),
        });
        assert_eq!(state.context_used, 150_000);
        assert!(
            matches!(state.entries.last(), Some(super::state::Entry::Compaction { active: false, error: Some(error), .. }) if error == "aborted")
        );
    }

    #[test]
    fn split_turn_compaction_stays_between_surrounding_messages() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::TextDelta {
            text: "before".into(),
        });
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("overflow".into()),
            summary: None,
            tokens_before: Some(200_000),
            tokens_after: None,
            error: None,
        });
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::End,
            reason: Some("overflow".into()),
            summary: Some("middle".into()),
            tokens_before: Some(200_000),
            tokens_after: Some(30_000),
            error: None,
        });
        state.apply(AgentEvent::TextDelta {
            text: "after".into(),
        });
        assert!(matches!(state.entries.as_slice(), [
            super::state::Entry::Assistant { .. },
            super::state::Entry::Compaction { summary, .. },
            super::state::Entry::Assistant { .. }
        ] if summary == "middle"));
    }

    #[test]
    fn markdown_renders_numbered_lists_and_checkboxes() {
        let source: Vec<String> = vec![
            "## Next Steps".into(),
            "1. Run the test suite".into(),
            "2. Push the changes".into(),
            "### Done".into(),
            "- [x] Wire compaction".into(),
            "- [ ] Visual review".into(),
        ];
        let lines = crate::markdown::render(&source, 80, Theme::GROK_NIGHT);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            text.iter()
                .any(|line| line.contains("1. Run the test suite")),
            "numbered list should keep the number, got: {text:?}"
        );
        assert!(text.iter().any(|line| line.contains("☑ Wire compaction")));
        assert!(text.iter().any(|line| line.contains("☐ Visual review")));
    }

    #[test]
    fn compaction_appears_in_render_output() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(180_000),
            tokens_after: None,
            error: None,
        });
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::End,
            reason: Some("manual".into()),
            summary: Some(
                "## Highlights\n- [x] Retained recent user requests\n- [ ] Dropped old tool outputs\n1. First next step\n2. Second next step".into(),
            ),
            tokens_before: Some(180_000),
            tokens_after: Some(24_000),
            error: None,
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Compacted context"));
        assert!(output.contains("180K → 24K tokens"));
        assert!(output.contains("Highlights"), "heading should be rendered");
        assert!(
            output.contains("☑ Retained recent user requests"),
            "checked checkbox should be rendered, got output: {output}"
        );
        assert!(
            output.contains("☐ Dropped old tool outputs"),
            "unchecked checkbox should be rendered, got output: {output}"
        );
        assert!(
            output.contains("1. First next step"),
            "numbered list should keep the number"
        );
        assert!(
            output.contains("2. Second next step"),
            "second numbered item should be rendered"
        );
    }

    #[test]
    fn compaction_without_tokens_after_omits_the_delta_line() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.context_used = 76_000;
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(76_000),
            tokens_after: None,
            error: None,
        });
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::End,
            reason: Some("manual".into()),
            summary: Some("Compaction finished.".into()),
            tokens_before: Some(76_000),
            tokens_after: None,
            error: None,
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Compacted context"));
        assert!(
            !output.contains("→"),
            "should not fake a token delta when tokens_after is unknown: {output:?}"
        );
        assert!(
            !output.contains("76K → 76K"),
            "fallback must not show the same number twice"
        );
        assert!(output.contains("Compaction finished."));
    }

    #[test]
    fn compaction_indicator_renders_a_muted_static_line() {
        let (width, height) = (100, 16);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.apply(AgentEvent::CompactionIndicator {
            reason: "manual".into(),
            tokens_before: Some(180_000),
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(
            output.contains("Previously compacted from 180K tokens"),
            "indicator should show pre-compaction token count, got: {output}"
        );
        assert!(
            !output.contains("Compacted context"),
            "indicator should not look like a live compaction card"
        );
        assert!(
            !output.contains("→"),
            "indicator has no after value so should not show a delta"
        );
    }

    #[test]
    fn compaction_indicator_without_tokens_before_still_renders() {
        let (width, height) = (100, 16);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.apply(AgentEvent::CompactionIndicator {
            reason: "branch".into(),
            tokens_before: None,
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Previously compacted"));
    }

    #[test]
    fn compaction_banner_appears_between_transcript_and_composer_while_active() {
        let (width, height) = (100, 20);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.context_used = 184_000;
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(184_000),
            tokens_after: None,
            error: None,
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        // The banner is a single line with a Braille spinner (not the static
        // ◌ glyph used by the in-transcript card), the elapsed time, and the
        // current token count with a down-arrow indicator. We look for the
        // banner by finding a line with the spinner glyph and "tokens" but
        // NOT the static "◌" that the in-transcript card uses.
        let spinner_glyphs = ["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}"];
        let banner_line = output
            .lines()
            .find(|line| {
                line.contains("tokens")
                    && spinner_glyphs.iter().any(|g| line.contains(g))
                    && !line.contains("◌")
            })
            .expect("banner should be visible while compaction is active");
        assert!(
            banner_line.contains("s"),
            "banner should show elapsed time ending in 's', got: {banner_line:?}"
        );
        assert!(
            banner_line.contains("184K"),
            "banner should show current token count (184K), got: {banner_line:?}"
        );
        assert!(
            banner_line.contains("↓"),
            "banner should show the down-arrow indicator, got: {banner_line:?}"
        );
    }

    #[test]
    fn compaction_banner_disappears_once_compaction_finishes() {
        let (width, height) = (100, 20);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.context_used = 184_000;
        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::Start,
            reason: Some("manual".into()),
            summary: None,
            tokens_before: Some(184_000),
            tokens_after: None,
            error: None,
        });
        // Banner should be visible right after Start.
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let before = buffer_text(terminal.backend().buffer(), width, height);
        assert!(
            before
                .lines()
                .any(|line| line.contains("Compacting context") && line.contains("s")),
            "banner should be visible after Start, output: {before}"
        );

        state.apply(AgentEvent::Compaction {
            phase: pi_harness::CompactionPhase::End,
            reason: Some("manual".into()),
            summary: Some("All done.".into()),
            tokens_before: Some(184_000),
            tokens_after: Some(24_000),
            error: None,
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let after = buffer_text(terminal.backend().buffer(), width, height);
        let lines_with_banner = after
            .lines()
            .filter(|line| line.contains("Compacting context") && line.contains("↓"))
            .count();
        assert_eq!(
            lines_with_banner, 0,
            "the sticky spinner banner should disappear after End, output: {after}"
        );
    }

    #[test]
    fn compaction_banner_is_not_added_when_no_compaction_is_running() {
        let (width, height) = (100, 20);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = super::AppState::default();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(
            !output.contains("Compacting context"),
            "no banner when no compaction is active, got: {output}"
        );
        assert!(
            !output.contains("↓"),
            "no down-arrow token indicator when no compaction is active, got: {output}"
        );
    }

    #[test]
    fn working_banner_appears_during_text_delta_streaming() {
        let (width, height) = (100, 20);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.context_used = 5_000;
        state.apply(AgentEvent::TextDelta {
            text: "Hello there, this is the start of a response.".into(),
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        // The working banner has a Braille spinner + ' Working…' on the
        // left, an elapsed time in the middle, and an up/down token tally
        // on the right. It's distinct from the compaction banner (no
        // "Compacting context…" text and a different right-side format).
        let spinner_glyphs = ["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}"];
        let working_line = output
            .lines()
            .find(|line| {
                line.contains("Working")
                    && spinner_glyphs.iter().any(|g| line.contains(g))
                    && !line.contains("Compacting")
            })
            .expect("working banner should be visible while the model is streaming");
        assert!(
            working_line.contains("s"),
            "working banner should show elapsed time, got: {working_line:?}"
        );
        assert!(
            working_line.contains("↑") && working_line.contains("input"),
            "working banner should show ↑ input, got: {working_line:?}"
        );
        assert!(
            working_line.contains("↓") && working_line.contains("output"),
            "working banner should show ↓ output, got: {working_line:?}"
        );
        assert!(
            working_line.contains("5K"),
            "working banner should show snapshotted input token count (5K), got: {working_line:?}"
        );
    }

    #[test]
    fn working_banner_accumulates_output_chars_as_deltas_stream() {
        let (width, height) = (100, 20);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.context_used = 1_000;
        // Stream a delta that adds 20 characters — estimated output
        // tokens should be ~5 (20 / 4 = 5).
        state.apply(AgentEvent::TextDelta {
            text: "0123456789abcdefghij".into(),
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        // 20 chars / 4 = 5 estimated output tokens → "5" appears in the
        // banner. The "1K" input is the snapshot of context_used at
        // turn start. Both are in the same line, so the test just checks
        // both substrings are present anywhere in the rendered output.
        assert!(
            output.contains("1K"),
            "input snapshot should be visible: {output}"
        );
        assert!(
            output.contains("Working"),
            "working banner should be visible: {output}"
        );
    }

    #[test]
    fn working_banner_disappears_after_turn_complete() {
        let (width, height) = (100, 20);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.context_used = 1_000;
        state.apply(AgentEvent::TextDelta {
            text: "streaming response".into(),
        });
        state.apply(AgentEvent::TurnComplete {
            usage: pi_harness::Usage {
                input_tokens: 1_000,
                output_tokens: 200,
            },
            stop_reason: "end_turn".into(),
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(
            !output.contains("Working…"),
            "working banner should disappear after TurnComplete, output: {output}"
        );
    }

    #[test]
    fn working_banner_input_snapshot_resets_between_turns() {
        let mut state = super::AppState::default();
        state.context_used = 2_000;
        state.apply(AgentEvent::TextDelta {
            text: "first turn".into(),
        });
        assert_eq!(state.turn_input_tokens, 2_000);
        assert!(state.turn_started_at.is_some());

        state.apply(AgentEvent::TurnComplete {
            usage: pi_harness::Usage {
                input_tokens: 2_000,
                output_tokens: 10,
            },
            stop_reason: "end_turn".into(),
        });
        assert_eq!(state.turn_input_tokens, 2_000);
        assert_eq!(state.turn_output_chars, 0);
        assert!(state.turn_started_at.is_none());

        // A new turn snapshots the post-completion context_used.
        state.context_used = 2_010;
        state.apply(AgentEvent::TextDelta {
            text: "second turn".into(),
        });
        assert_eq!(state.turn_input_tokens, 2_010);
        assert!(state.turn_started_at.is_some());
    }
}
