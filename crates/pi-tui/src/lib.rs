mod actions;
mod agent_layout;
mod effects;
mod fixtures;
mod markdown;
mod overlay;
mod picker;
mod prompt;
mod scrollback;
mod state;
mod theme;
mod ui;

use std::{
    fs::{self, File},
    io,
    path::PathBuf,
    time::SystemTime,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use clipboard_rs::{Clipboard as ClipboardRs, ClipboardContext};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use effects::{TerminalGuard, display_path, git_branch};
pub use fixtures::Story;
use futures_util::StreamExt;
use ratatui::Terminal;
use state::OverlayAction;
pub use state::{AppState, Focus, OverlayKind, View};
pub use theme::ThemeMode;
use tokio::sync::{broadcast, mpsc};

fn read_clipboard_image() -> Result<state::ImageAttachment> {
    let mut clipboard = arboard::Clipboard::new()?;
    if let Ok(image) = clipboard.get_image() {
        let directory = std::env::temp_dir().join("pi-shell-attachments");
        fs::create_dir_all(&directory)?;
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let name = format!("screenshot-{timestamp}.png");
        let path: PathBuf = directory.join(&name);
        let (preview_width, preview_height, preview_rgba) =
            image_preview(image.width, image.height, image.bytes.as_ref());
        let file = File::create(&path)?;
        let mut encoder = png::Encoder::new(file, image.width as u32, image.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(image.bytes.as_ref())?;
        writer.finish()?;
        return Ok(state::ImageAttachment {
            id: 0,
            path: path.to_string_lossy().into_owned(),
            name,
            width: image.width,
            height: image.height,
            mime_type: "image/png".into(),
            temporary: true,
            preview_width,
            preview_height,
            preview_rgba,
        });
    }

    let file_clipboard = ClipboardContext::new().ok();
    let file_path = file_clipboard.as_ref().and_then(clipboard_file_path);
    let text_path = || {
        clipboard
            .get_text()
            .ok()
            .and_then(|text| text.lines().map(str::trim).find_map(clipboard_image_path))
    };
    let path = file_path.or_else(text_path).ok_or_else(|| {
        let formats = file_clipboard
            .as_ref()
            .and_then(|clipboard| clipboard.available_formats().ok())
            .filter(|formats| !formats.is_empty())
            .map(|formats| formats.join(", "))
            .unwrap_or_else(|| "none reported".into());
        anyhow!("no image pixels or image file found (clipboard formats: {formats})")
    })?;
    image_attachment_from_path(path)
}

fn read_clipboard_text() -> Result<String> {
    let text = ClipboardContext::new()
        .and_then(|clipboard| clipboard.get_text())
        .map_err(|error| anyhow!(error.to_string()))?;
    if text.is_empty() {
        return Err(anyhow!("clipboard contains no text"));
    }
    Ok(text)
}

enum ClipboardPayload {
    Image(state::ImageAttachment),
    Text(String),
}

fn begin_clipboard_image_load(
    state: &mut AppState,
    sender: &mpsc::UnboundedSender<Result<ClipboardPayload, String>>,
) {
    if state.image_processing_started_at.is_some() {
        state.status = "image processing already in progress…".into();
        return;
    }
    state.image_processing_started_at = Some(Instant::now());
    state.status = "processing image from clipboard…".into();
    let sender = sender.clone();
    tokio::task::spawn_blocking(move || {
        let result = read_clipboard_image()
            .map(ClipboardPayload::Image)
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(result);
    });
}

fn begin_clipboard_load(
    state: &mut AppState,
    sender: &mpsc::UnboundedSender<Result<ClipboardPayload, String>>,
) {
    if state.image_processing_started_at.is_some() {
        state.status = "clipboard processing already in progress…".into();
        return;
    }
    state.image_processing_started_at = Some(Instant::now());
    state.status = "reading clipboard…".into();
    let sender = sender.clone();
    tokio::task::spawn_blocking(move || {
        let result = read_clipboard_image()
            .map(ClipboardPayload::Image)
            .or_else(|_| read_clipboard_text().map(ClipboardPayload::Text))
            .map_err(|error| format!("{error:#}"));
        let _ = sender.send(result);
    });
}

fn paste_clipboard_text(state: &mut AppState, editor: bool) {
    match read_clipboard_text() {
        Ok(text) if editor => state.insert_paste_editor_text(text),
        Ok(text) => {
            let length = text.chars().count();
            state.insert_paste(text);
            state.status = format!("pasted {length} characters");
        }
        Err(error) => state.status = format!("clipboard text unavailable: {error:#}"),
    }
}

fn clipboard_file_path(clipboard: &ClipboardContext) -> Option<PathBuf> {
    if let Some(path) = clipboard.get_files().ok().and_then(|files| {
        files
            .iter()
            .find_map(|file| clipboard_image_path(file.trim()))
    }) {
        return Some(path);
    }

    ["x-special/gnome-copied-files", "text/uri-list"]
        .iter()
        .filter_map(|format| clipboard.get_buffer(format).ok())
        .filter_map(|buffer| String::from_utf8(buffer).ok())
        .flat_map(|contents| {
            contents
                .lines()
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .find_map(|line| clipboard_image_path(&line))
}

fn image_attachment_from_path(path: PathBuf) -> Result<state::ImageAttachment> {
    let decoded = image::open(&path)?.to_rgba8();
    let width = decoded.width() as usize;
    let height = decoded.height() as usize;
    let (preview_width, preview_height, preview_rgba) =
        image_preview(width, height, decoded.as_raw());
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("clipboard-image")
        .to_string();
    let mime_type = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/png",
    };
    Ok(state::ImageAttachment {
        id: 0,
        path: path.to_string_lossy().into_owned(),
        name,
        width,
        height,
        mime_type: mime_type.into(),
        temporary: false,
        preview_width,
        preview_height,
        preview_rgba,
    })
}

fn clipboard_image_path(value: &str) -> Option<PathBuf> {
    let value = value.trim_matches(|character: char| {
        character == '\0' || character == '\r' || character == '\n'
    });
    if value == "copy" || value == "cut" || value.is_empty() {
        return None;
    }
    let decoded = value.strip_prefix("file://").map_or_else(
        || value.to_string(),
        |uri| percent_decode_path(uri).unwrap_or_else(|| uri.to_string()),
    );
    let path = PathBuf::from(decoded);
    path.is_absolute().then_some(path)
}

fn percent_decode_path(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            output.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn image_preview(width: usize, height: usize, rgba: &[u8]) -> (u16, u16, Vec<u8>) {
    let (preview_width, preview_height) = if width <= 100 && height <= 60 {
        (width.max(1), height.max(1))
    } else if width.saturating_mul(60) > height.saturating_mul(100) {
        (100, (height.saturating_mul(100) / width).max(1))
    } else {
        ((width.saturating_mul(60) / height).max(1), 60)
    };
    let mut preview_rgba = Vec::with_capacity(preview_width * preview_height * 4);
    for y in 0..preview_height {
        let source_y = y * height / preview_height;
        for x in 0..preview_width {
            let source_x = x * width / preview_width;
            let offset = (source_y * width + source_x) * 4;
            preview_rgba.extend_from_slice(&rgba[offset..offset + 4]);
        }
    }
    (preview_width as u16, preview_height as u16, preview_rgba)
}

#[derive(Debug)]
pub enum UiCommand {
    InstallUpdate,
    Submit {
        text: String,
        delivery: Option<pi_harness::MessageDelivery>,
        images: Vec<pi_harness::MessageImage>,
    },
    Permission {
        request_id: String,
        decision: pi_harness::PermissionDecision,
    },
    SetModel(String),
    ResumeSession(String),
    RenameSession {
        target: String,
        name: String,
    },
    DeleteSession(String),
    RefreshSessions,
    LoadWorkflowCatalog,
    PreviewWorkflow(String),
    StartWorkflow {
        workflow: String,
        input: String,
        expected_definition_hash: Option<String>,
    },
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
        instructions: Option<String>,
    },
    ForkSession {
        entry_id: String,
    },
    SetLabel {
        entry_id: String,
        label: Option<String>,
    },
    CycleThinking,
    SetThinking(String),
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
    SetExtensionEnabled {
        path: String,
        enabled: bool,
    },
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
    StopResident(String),
    CloseResident(String),
    KillTask(String),
    WorkflowControl {
        run_id: String,
        action: String,
        step_id: Option<String>,
    },
    ReadWorkflowArtifact {
        run_id: String,
        artifact_id: String,
    },
}

enum LoopWake {
    Input(Option<io::Result<Event>>),
    Agent(Result<pi_harness::AgentEvent, broadcast::error::RecvError>),
    Clipboard(Result<ClipboardPayload, String>),
    Animation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActionOutcome {
    Handled,
    Quit,
}

fn dispatch_registered_action(
    action: actions::ActionId,
    state: &mut AppState,
    commands: &Option<mpsc::UnboundedSender<UiCommand>>,
) -> ActionOutcome {
    use actions::ActionId;
    match action {
        ActionId::CommandPalette => state.open_overlay(OverlayKind::CommandPalette),
        ActionId::ModelPicker => state.open_overlay(OverlayKind::ModelPicker),
        ActionId::SessionPicker => state.open_overlay(OverlayKind::SessionPicker),
        ActionId::Settings => state.open_overlay(OverlayKind::Settings),
        ActionId::ToggleTasks => {
            state.view = if state.view == View::Tasks {
                View::Transcript
            } else {
                View::Tasks
            };
        }
        ActionId::ToggleQueue => state.queue_visible = !state.queue_visible,
        ActionId::CycleMode => {
            state.cycle_permission_mode();
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetPermissionMode(
                    state.permission_mode.wire_value().into(),
                ));
            }
        }
        ActionId::CancelTurn => {
            if !state.prompt.is_empty() || !state.image_attachments.is_empty() {
                state.clear_prompt();
            } else if state.streaming
                && let Some(sender) = commands
            {
                let _ = sender.send(UiCommand::AbortAndRestoreQueue);
            }
        }
        ActionId::Quit => return ActionOutcome::Quit,
        _ => return ActionOutcome::Handled,
    }
    ActionOutcome::Handled
}

fn dispatch_pane_action(
    action: actions::ActionId,
    state: &mut AppState,
    width: u16,
    height: u16,
) -> bool {
    use actions::ActionId;
    match action {
        ActionId::ClearPrompt => {
            if state.prompt.is_empty() && state.image_attachments.is_empty() {
                return false;
            }
            state.clear_prompt();
        }
        ActionId::FocusScrollback => {
            if state.complete_slash_command() {
                return true;
            }
            ui::focus_scrollback(state, width, height);
        }
        ActionId::FocusPrompt => state.focus_prompt(),
        ActionId::ScrollUp => ui::move_section_focus(state, width, height, -1),
        ActionId::ScrollDown => ui::move_section_focus(state, width, height, 1),
        ActionId::ToggleFold => {
            if let Some(index) = state.focused_entry {
                if state
                    .focused_target_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("tool-group:"))
                {
                    state.toggle_tool_group(index);
                } else {
                    state.activate_entry_at(index);
                }
            }
        }
        ActionId::OpenBlockViewer => {
            if state.focused_entry.is_some() {
                state.viewed_entry = state.focused_entry;
                state.view = View::BlockViewer;
                state.scroll_from_bottom = 0;
            }
        }
        ActionId::ToggleMultiline => {
            state.multiline_mode = !state.multiline_mode;
            state.status = if state.multiline_mode {
                "multiline input enabled".into()
            } else {
                "multiline input disabled".into()
            };
        }
        ActionId::SendPrompt | ActionId::SendNow => return false,
        _ => return false,
    }
    true
}

fn handle_agent_escape(state: &mut AppState, commands: &Option<mpsc::UnboundedSender<UiCommand>>) {
    if state.streaming {
        state.escape_armed_at = None;
        return;
    }
    let now = Instant::now();
    let confirmed = state
        .escape_armed_at
        .is_some_and(|armed| now.duration_since(armed) <= Duration::from_millis(800));
    if !confirmed {
        state.escape_armed_at = Some(now);
        if !state.prompt.is_empty() || !state.image_attachments.is_empty() {
            state.status = "press Esc again to clear the draft".into();
        }
        return;
    }
    state.escape_armed_at = None;
    if !state.prompt.is_empty() || !state.image_attachments.is_empty() {
        state.clear_prompt();
    } else if !state.entries.is_empty() {
        state.open_overlay(OverlayKind::RewindPicker);
        state.status = "loading rewind checkpoints…".into();
        if let Some(sender) = commands {
            let _ = sender.send(UiCommand::LoadRewinds);
        }
    }
}

pub struct TuiBootstrap {
    pub models: Vec<pi_harness::ModelInfo>,
    pub auth_providers: Vec<pi_harness::ModelInfo>,
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
        auth_providers,
        sessions,
        files,
        resources,
        settings,
        open_resume,
    } = bootstrap;
    let mut state = AppState {
        theme_mode: theme::load_preference(),
        ..AppState::default()
    };
    if let Ok(cwd) = std::env::current_dir() {
        state.cwd = display_path(&cwd);
        if let Some(branch) = git_branch(&cwd) {
            state.branch = branch;
        }
    }
    if !models.is_empty() {
        state.model = models[0].display_name.clone();
        state.available_models = models;
    }
    state.available_sessions = sessions;
    state.available_auth_providers = auth_providers;
    state.available_files = files;
    state.runtime_commands = resources.commands;
    state.context_files = resources.context_files;
    state.runtime_extensions = resources.extensions;
    state.runtime_settings = settings;
    state.view = View::Dashboard;
    if open_resume {
        state.open_overlay(OverlayKind::SessionPicker);
    }
    run_app(state, Some(events), Some(commands)).await
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
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut input = EventStream::new();
    let (image_sender, mut image_receiver) = mpsc::unbounded_channel();
    let mut dirty = true;
    let mut last_draw_at = None;
    const ANIMATION_FRAME_INTERVAL: Duration = Duration::from_millis(33);

    loop {
        let animated = state.streaming
            || state.scroll_drag.is_some()
            || state.scrollbar_dragging
            || state.image_processing_started_at.is_some()
            || state.active_compaction_started_at().is_some()
            || state.has_background_work()
            || state
                .runtime_sessions
                .values()
                .any(|session| session.status == "running");
        let animation_due = animated
            && last_draw_at
                .is_none_or(|last_draw: Instant| last_draw.elapsed() >= ANIMATION_FRAME_INTERVAL);
        if (dirty && !animated) || animation_due {
            terminal.draw(|frame| ui::render(frame, &state))?;
            last_draw_at = Some(Instant::now());
            dirty = false;
        }

        let animation_wait = if animated {
            last_draw_at.map_or(Duration::ZERO, |last_draw: Instant| {
                ANIMATION_FRAME_INTERVAL.saturating_sub(last_draw.elapsed())
            })
        } else {
            Duration::from_secs(3_600)
        };

        let wake = tokio::select! {
            input_event = input.next() => LoopWake::Input(input_event),
            agent_event = next_agent_event(&mut events) => LoopWake::Agent(agent_event),
            clipboard = image_receiver.recv(), if state.image_processing_started_at.is_some() => {
                LoopWake::Clipboard(clipboard.expect("clipboard worker sender remains alive"))
            },
            _ = tokio::time::sleep(animation_wait) => LoopWake::Animation,
        };
        let input_event = match wake {
            LoopWake::Input(Some(input_event)) => input_event?,
            LoopWake::Input(None) => break,
            LoopWake::Agent(Ok(agent_event)) => {
                state.apply(agent_event);
                if let Some(receiver) = &mut events {
                    while let Ok(agent_event) = receiver.try_recv() {
                        state.apply(agent_event);
                    }
                }
                dirty = true;
                continue;
            }
            LoopWake::Agent(Err(broadcast::error::RecvError::Lagged(_))) => {
                dirty = true;
                continue;
            }
            LoopWake::Agent(Err(broadcast::error::RecvError::Closed)) => {
                events = None;
                continue;
            }
            LoopWake::Clipboard(result) => {
                state.image_processing_started_at = None;
                match result {
                    Ok(ClipboardPayload::Image(image)) => state.attach_image(image),
                    Ok(ClipboardPayload::Text(text)) => {
                        let length = text.chars().count();
                        state.insert_paste(text);
                        state.status = format!("pasted {length} characters");
                    }
                    Err(error) => state.status = format!("clipboard unavailable: {error}"),
                }
                dirty = true;
                continue;
            }
            LoopWake::Animation => continue,
        };

        dirty = true;
        let size = terminal.size()?;
        let max_scroll = ui::max_scroll(&state, size.width, size.height);
        let page = usize::from(
            agent_layout::AgentLayout::compute(
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
                &state,
            )
            .scrollback
            .height,
        )
        .max(1);

        match input_event {
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
                    KeyCode::Left if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_cursor(-1);
                    }
                    KeyCode::Right if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_cursor(1);
                    }
                    KeyCode::Home
                        if state.overlay == OverlayKind::PasteEditor
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        state.overlay_cursor = 0;
                    }
                    KeyCode::End
                        if state.overlay == OverlayKind::PasteEditor
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        state.overlay_cursor = state.overlay_query.chars().count();
                    }
                    KeyCode::Home if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_line_edge(false);
                    }
                    KeyCode::End if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_line_edge(true);
                    }
                    KeyCode::PageUp if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_vertical(-10);
                    }
                    KeyCode::PageDown if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_vertical(10);
                    }
                    KeyCode::Up if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_vertical(-1);
                    }
                    KeyCode::Down if state.overlay == OverlayKind::PasteEditor => {
                        state.move_paste_editor_vertical(1);
                    }
                    KeyCode::Up => state.move_overlay_selection(-1),
                    KeyCode::Down => state.move_overlay_selection(1),
                    KeyCode::PageUp
                        if !matches!(
                            state.overlay,
                            OverlayKind::PasteEditor | OverlayKind::TreePicker
                        ) =>
                    {
                        state.move_overlay_selection(-(page as isize));
                    }
                    KeyCode::PageDown
                        if !matches!(
                            state.overlay,
                            OverlayKind::PasteEditor | OverlayKind::TreePicker
                        ) =>
                    {
                        state.move_overlay_selection(page as isize);
                    }
                    KeyCode::Home if state.overlay != OverlayKind::PasteEditor => {
                        state.overlay_hovered = None;
                        state.overlay_selected = 0;
                    }
                    KeyCode::End if state.overlay != OverlayKind::PasteEditor => {
                        state.overlay_hovered = None;
                        let count = state.overlay_items().len();
                        state.overlay_selected = count.saturating_sub(1);
                    }
                    KeyCode::Left if state.overlay == OverlayKind::Permission => {
                        state.move_overlay_selection(-1);
                    }
                    KeyCode::Right if state.overlay == OverlayKind::Permission => {
                        state.move_overlay_selection(1);
                    }
                    KeyCode::Left
                        if state.overlay == OverlayKind::TreePicker
                            && key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        state.fold_or_move_tree(false);
                    }
                    KeyCode::Right
                        if state.overlay == OverlayKind::TreePicker
                            && key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        state.fold_or_move_tree(true);
                    }
                    KeyCode::Left | KeyCode::PageUp if state.overlay == OverlayKind::TreePicker => {
                        state.move_tree_page(-1, page);
                    }
                    KeyCode::Right | KeyCode::PageDown
                        if state.overlay == OverlayKind::TreePicker =>
                    {
                        state.move_tree_page(1, page);
                    }
                    KeyCode::Char(' ') if state.overlay == OverlayKind::ScopedModels => {
                        state.toggle_scoped_model();
                    }
                    KeyCode::Char(' ') if state.overlay == OverlayKind::Extensions => {
                        let action = state.activate_overlay();
                        if dispatch_overlay_action(action, &commands) {
                            break;
                        }
                    }
                    KeyCode::Char(' ') if state.overlay == OverlayKind::Settings => {
                        let action = state.activate_overlay();
                        if dispatch_overlay_action(action, &commands) {
                            break;
                        }
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
                    KeyCode::Char('p')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.overlay == OverlayKind::SessionPicker =>
                    {
                        state.toggle_session_paths();
                    }
                    KeyCode::Char('s')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.overlay == OverlayKind::SessionPicker =>
                    {
                        state.cycle_session_sort();
                    }
                    KeyCode::Char('v')
                        if state.overlay == OverlayKind::PasteEditor
                            && key.modifiers == KeyModifiers::CONTROL =>
                    {
                        paste_clipboard_text(&mut state, true);
                    }
                    KeyCode::Char('n')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.overlay == OverlayKind::SessionPicker =>
                    {
                        state.toggle_named_sessions();
                    }
                    KeyCode::Char('r')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.overlay == OverlayKind::SessionPicker =>
                    {
                        state.begin_session_rename();
                    }
                    KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && state.overlay == OverlayKind::SessionPicker =>
                    {
                        state.begin_session_delete();
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
                    KeyCode::Delete if state.overlay == OverlayKind::ImageViewer => {
                        state.remove_viewed_image();
                    }
                    KeyCode::Enter if state.overlay == OverlayKind::ImageViewer => {
                        state.close_overlay();
                    }
                    KeyCode::Enter
                        if state.overlay == OverlayKind::PasteEditor
                            && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        state.insert_paste_editor_text("\n".into());
                    }
                    KeyCode::Char('j' | 'm')
                        if state.overlay == OverlayKind::PasteEditor
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        let action = state.activate_overlay();
                        if dispatch_overlay_action(action, &commands) {
                            break;
                        }
                    }
                    KeyCode::Char('s')
                        if state.overlay == OverlayKind::PasteEditor
                            && key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        let action = state.activate_overlay();
                        if dispatch_overlay_action(action, &commands) {
                            break;
                        }
                    }
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
            Event::Paste(text) if state.overlay == OverlayKind::PasteEditor => {
                state.insert_paste_editor_text(text);
            }
            Event::Paste(text)
                if state.focus == Focus::Prompt && state.overlay == OverlayKind::None =>
            {
                state.insert_paste(text);
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if key.code == KeyCode::Char('v')
                    && key.modifiers == KeyModifiers::CONTROL
                    && state.focus == Focus::Prompt
                    && state.overlay == OverlayKind::None
                {
                    paste_clipboard_text(&mut state, false);
                    continue;
                }
                if let Some(action) = actions::lookup(&key, actions::ActionContext::Global)
                    && dispatch_registered_action(action, &mut state, &commands)
                        == ActionOutcome::Quit
                {
                    break;
                }
                if state.view == View::Transcript {
                    let context = match state.focus {
                        Focus::Prompt => actions::ActionContext::Prompt,
                        Focus::Scrollback => actions::ActionContext::Scrollback,
                    };
                    if let Some(action) = actions::lookup(&key, context)
                        && dispatch_pane_action(action, &mut state, size.width, size.height)
                    {
                        continue;
                    }
                }
                if matches!(state.view, View::Transcript | View::Tasks)
                    && let Some(action) = actions::lookup(&key, actions::ActionContext::Agent)
                {
                    if dispatch_registered_action(action, &mut state, &commands)
                        == ActionOutcome::Quit
                    {
                        break;
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        state.view = if state.view == View::Tasks {
                            View::Transcript
                        } else {
                            View::Tasks
                        };
                    }
                    KeyCode::Esc | KeyCode::Char('q') if state.view == View::BlockViewer => {
                        state.view = View::Transcript;
                        state.scroll_from_bottom = 0;
                    }
                    KeyCode::Up if state.view == View::BlockViewer => {
                        state.scroll_from_bottom = state.scroll_from_bottom.saturating_add(3);
                    }
                    KeyCode::Down if state.view == View::BlockViewer => state.scroll_down(3),
                    _ if state.view == View::BlockViewer => {}
                    KeyCode::Esc | KeyCode::Char('q') if state.view == View::Subagent => {
                        state.view = View::Tasks;
                        state.scroll_from_bottom = 0;
                    }
                    KeyCode::Esc | KeyCode::Char('q') if state.view == View::Workflows => {
                        state.view = View::Transcript;
                    }
                    KeyCode::Esc | KeyCode::Char('q') if state.view == View::WorkflowArtifact => {
                        state.view = View::Workflows;
                        state.scroll_from_bottom = 0;
                    }
                    KeyCode::Up if state.view == View::WorkflowArtifact => {
                        state.scroll_from_bottom = state.scroll_from_bottom.saturating_add(3);
                    }
                    KeyCode::Down if state.view == View::WorkflowArtifact => state.scroll_down(3),
                    _ if state.view == View::WorkflowArtifact => {}
                    KeyCode::Up if state.view == View::Workflows => {
                        state.workflow_selected = state.workflow_selected.saturating_sub(1);
                    }
                    KeyCode::Down if state.view == View::Workflows => {
                        state.workflow_selected = (state.workflow_selected + 1)
                            .min(state.workflow_runs.len().saturating_sub(1));
                    }
                    KeyCode::Char(action @ ('a' | 'd' | 'r' | 'x'))
                        if state.view == View::Workflows =>
                    {
                        let action = match action {
                            'a' => "approve",
                            'd' => "reject",
                            'r' => "retry",
                            _ => "cancel",
                        };
                        if let Some((run_id, step_id)) = state.selected_workflow_control(action)
                            && let Some(sender) = &commands
                        {
                            let _ = sender.send(UiCommand::WorkflowControl {
                                run_id,
                                action: action.into(),
                                step_id,
                            });
                        }
                    }
                    KeyCode::Char('v') | KeyCode::Enter if state.view == View::Workflows => {
                        if let Some((run_id, artifact_id)) = state.selected_workflow_artifact()
                            && let Some(sender) = &commands
                        {
                            let _ = sender.send(UiCommand::ReadWorkflowArtifact {
                                run_id,
                                artifact_id,
                            });
                        }
                    }
                    _ if state.view == View::Workflows => {}
                    KeyCode::Esc if state.view == View::Tasks => {
                        state.view = View::Transcript;
                    }
                    KeyCode::Up if state.view == View::Tasks => {
                        state.task_selected = state.task_selected.saturating_sub(1);
                    }
                    KeyCode::Down if state.view == View::Tasks => {
                        state.task_selected = (state.task_selected + 1)
                            .min(state.subagent_tasks.len().saturating_sub(1));
                    }
                    KeyCode::Enter if state.view == View::Tasks => state.open_selected_subagent(),
                    KeyCode::Char('k') if state.view == View::Tasks => {
                        if let Some(task_id) = state.selected_subagent_id()
                            && let Some(sender) = &commands
                        {
                            let _ = sender.send(UiCommand::KillTask(task_id));
                        }
                    }
                    _ if state.view == View::Tasks => {}
                    KeyCode::Esc if state.view == View::Dashboard => {
                        state.view = View::Transcript;
                    }
                    KeyCode::Up if state.view == View::Dashboard => {
                        state.dashboard_selected = state.dashboard_selected.saturating_sub(1);
                    }
                    KeyCode::Down if state.view == View::Dashboard => {
                        state.dashboard_selected = (state.dashboard_selected + 1)
                            .min(state.available_sessions.len().saturating_sub(1));
                    }
                    KeyCode::PageUp if state.view == View::Dashboard => {
                        state.dashboard_selected = state.dashboard_selected.saturating_sub(page);
                    }
                    KeyCode::PageDown if state.view == View::Dashboard => {
                        state.dashboard_selected = state
                            .dashboard_selected
                            .saturating_add(page)
                            .min(state.available_sessions.len().saturating_sub(1));
                    }
                    KeyCode::Home if state.view == View::Dashboard => {
                        state.dashboard_selected = 0;
                    }
                    KeyCode::End if state.view == View::Dashboard => {
                        state.dashboard_selected = state.available_sessions.len().saturating_sub(1);
                    }
                    KeyCode::Enter if state.view == View::Dashboard => {
                        if let Some(path) = state.activate_dashboard_session()
                            && let Some(sender) = &commands
                        {
                            let _ = sender.send(UiCommand::ResumeSession(path));
                        }
                        if !state.available_sessions.is_empty() {
                            state.view = View::Transcript;
                        }
                    }
                    KeyCode::Char('n') if state.view == View::Dashboard => {
                        if let Some(sender) = &commands {
                            let _ = sender.send(UiCommand::NewSession);
                        }
                        state.view = View::Transcript;
                    }
                    KeyCode::Char('u')
                        if state.view == View::Dashboard
                            && matches!(
                                state.app_update,
                                Some(pi_harness::AppUpdateStatus::Available { .. })
                            ) =>
                    {
                        if let Some(pi_harness::AppUpdateStatus::Available {
                            version,
                            size_bytes,
                        }) = state.app_update.clone()
                        {
                            state.app_update = Some(pi_harness::AppUpdateStatus::Downloading {
                                version,
                                downloaded_bytes: 0,
                                total_bytes: size_bytes,
                            });
                        }
                        if let Some(sender) = &commands {
                            let _ = sender.send(UiCommand::InstallUpdate);
                        }
                    }
                    KeyCode::Char('l')
                        if state.view == View::Dashboard && state.app_update.is_some() =>
                    {
                        state.app_update = None;
                    }
                    KeyCode::Char('r') if state.view == View::Dashboard => {
                        state.begin_dashboard_rename();
                    }
                    KeyCode::Char('d') if state.view == View::Dashboard => {
                        state.begin_dashboard_delete();
                    }
                    KeyCode::Char('s') if state.view == View::Dashboard => {
                        if state.dashboard_actions().stop
                            && let Some(path) = state.dashboard_selected_path()
                            && let Some(sender) = &commands
                        {
                            let _ = sender.send(UiCommand::StopResident(path));
                        }
                    }
                    KeyCode::Char('x') if state.view == View::Dashboard => {
                        if state.dashboard_actions().close
                            && let Some(path) = state.dashboard_selected_path()
                            && let Some(sender) = &commands
                        {
                            let _ = sender.send(UiCommand::CloseResident(path));
                        }
                    }
                    _ if state.view == View::Dashboard => {}
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
                    KeyCode::Char('v')
                        if key.modifiers.contains(KeyModifiers::ALT)
                            || key
                                .modifiers
                                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT) =>
                    {
                        begin_clipboard_image_load(&mut state, &image_sender);
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
                            if state.focus == Focus::Prompt {
                                ui::focus_scrollback(&mut state, size.width, size.height);
                            } else {
                                state.focus_prompt();
                            }
                        }
                    }
                    KeyCode::Enter if state.focus == Focus::Prompt => {
                        let modified_send = key
                            .modifiers
                            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
                        let control_send = key.modifiers.contains(KeyModifiers::CONTROL);
                        if !control_send
                            && ((!state.multiline_mode && modified_send)
                                || (state.multiline_mode && !modified_send))
                        {
                            state.insert_char('\n');
                            continue;
                        }
                        if matches!(state.prompt.trim(), "/paste" | "/paste-image") {
                            state.clear_prompt_text();
                            begin_clipboard_load(&mut state, &image_sender);
                        } else if let Some(action) = state.activate_slash_command() {
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
                        } else if let Some((prompt, images)) = state.submit_message()
                            && let Some(sender) = &commands
                        {
                            let delivery = state.streaming.then_some(
                                if key.modifiers.contains(KeyModifiers::CONTROL) {
                                    pi_harness::MessageDelivery::Steer
                                } else {
                                    pi_harness::MessageDelivery::FollowUp
                                },
                            );
                            let _ = sender.send(UiCommand::Submit {
                                text: prompt,
                                delivery,
                                images,
                            });
                        }
                    }
                    KeyCode::Enter if state.focus == Focus::Scrollback => {
                        if let Some(index) = state.focused_entry {
                            if state
                                .focused_target_id
                                .as_deref()
                                .is_some_and(|id| id.starts_with("tool-group:"))
                            {
                                state.toggle_tool_group(index);
                            } else {
                                state.activate_entry_at(index);
                            }
                        }
                    }
                    KeyCode::Backspace if state.focus == Focus::Prompt => state.backspace(),
                    KeyCode::Delete if state.focus == Focus::Prompt => state.delete(),
                    KeyCode::Left if state.focus == Focus::Prompt => state.move_cursor_left(),
                    KeyCode::Right if state.focus == Focus::Prompt => state.move_cursor_right(),
                    KeyCode::Home if state.focus == Focus::Prompt => state.move_cursor_home(),
                    KeyCode::End if state.focus == Focus::Prompt => state.move_cursor_end(),
                    KeyCode::Up
                        if state.focus == Focus::Prompt
                            && !state.slash_suggestions().is_empty() =>
                    {
                        state.move_slash_selection(-1);
                    }
                    KeyCode::Down
                        if state.focus == Focus::Prompt
                            && !state.slash_suggestions().is_empty() =>
                    {
                        state.move_slash_selection(1);
                    }
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
                        state.toggle_all_tools();
                    }
                    KeyCode::Char('d') if state.focus == Focus::Scrollback => {
                        state.toggle_all_diffs();
                    }
                    KeyCode::Esc => handle_agent_escape(&mut state, &commands),
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
                }
            }
            Event::Mouse(mouse) if state.overlay != OverlayKind::None => match mouse.kind {
                MouseEventKind::ScrollUp if state.overlay == OverlayKind::PasteEditor => {
                    state.scroll_paste_editor(-3);
                    state.overlay_hovered = None;
                }
                MouseEventKind::ScrollDown if state.overlay == OverlayKind::PasteEditor => {
                    state.scroll_paste_editor(3);
                    state.overlay_hovered = None;
                }
                MouseEventKind::ScrollUp => {
                    state.overlay_hovered = None;
                    state.move_overlay_selection(-3);
                }
                MouseEventKind::ScrollDown => {
                    state.overlay_hovered = None;
                    state.move_overlay_selection(3);
                }
                MouseEventKind::Moved => {
                    state.overlay_close_hovered = crate::overlay::close_at_position(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    );
                    if state.overlay == OverlayKind::PasteEditor {
                        state.paste_editor_action_hover =
                            state.paste_editor_action_at(mouse.column, mouse.row);
                    } else if state.overlay == OverlayKind::ImageViewer {
                        state.image_view_action_hover =
                            state.image_view_action_at(mouse.column, mouse.row);
                    }
                    state.overlay_hovered = crate::overlay::item_at_position(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    );
                }
                MouseEventKind::Down(_) => {
                    if crate::overlay::close_at_position(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    ) {
                        let action = state.cancel_oauth();
                        if dispatch_overlay_action(action, &commands) {
                            break;
                        }
                        continue;
                    }
                    if state.overlay == OverlayKind::ImageViewer
                        && let Some(action) = state.image_view_action_at(mouse.column, mouse.row)
                    {
                        if action == 0 {
                            state.remove_viewed_image();
                        } else {
                            state.close_overlay();
                        }
                        continue;
                    }
                    if state.overlay == OverlayKind::PasteEditor
                        && let Some(action) = state.paste_editor_action_at(mouse.column, mouse.row)
                    {
                        let action = if action == 0 {
                            state.activate_overlay()
                        } else {
                            state.cancel_oauth()
                        };
                        if dispatch_overlay_action(action, &commands) {
                            break;
                        }
                        continue;
                    }
                    if state.overlay == OverlayKind::PasteEditor
                        && state.click_paste_editor(mouse.column, mouse.row)
                    {
                        continue;
                    }
                    if let Some(index) = crate::overlay::item_at_position(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    ) {
                        state.overlay_selected = index;
                        let action = state.activate_overlay();
                        if dispatch_overlay_action(action, &commands) {
                            break;
                        }
                        continue;
                    }
                    continue;
                }
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left)
                    if state.view == View::Transcript
                        && state.transcript_scrollbar_contains(mouse.column, mouse.row) =>
                {
                    state.focus_scrollback();
                    state.scrollbar_dragging = true;
                    state.scroll_drag = None;
                    state.pending_transcript_click = None;
                    state.drag_scrollbar_to(mouse.row, max_scroll);
                }
                MouseEventKind::Drag(MouseButton::Left) if state.scrollbar_dragging => {
                    state.pending_transcript_click = None;
                    state.drag_scrollbar_to(mouse.row, max_scroll);
                }
                MouseEventKind::Drag(MouseButton::Left) if state.scroll_drag.is_some() => {
                    state.pending_transcript_click = None;
                    state.drag_scrollback_to(mouse.row, max_scroll);
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    let released = ui::section_hit_at(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    )
                    .map(|hit| (hit.index, hit.id));
                    state.finish_transcript_click(released);
                }
                MouseEventKind::ScrollUp
                    if state.focus == Focus::Prompt && !state.slash_suggestions().is_empty() =>
                {
                    state.overlay_hovered = None;
                    state.overlay_selected = state.overlay_selected.saturating_sub(1);
                }
                MouseEventKind::ScrollDown
                    if state.focus == Focus::Prompt && !state.slash_suggestions().is_empty() =>
                {
                    state.overlay_hovered = None;
                    state.overlay_selected = (state.overlay_selected + 1)
                        .min(state.slash_suggestions().len().saturating_sub(1));
                }
                MouseEventKind::Moved
                    if state.focus == Focus::Prompt && !state.slash_suggestions().is_empty() =>
                {
                    state.overlay_hovered = crate::overlay::slash_item_at(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    );
                }
                MouseEventKind::Down(_)
                    if state.focus == Focus::Prompt && !state.slash_suggestions().is_empty() =>
                {
                    if let Some(index) = crate::overlay::slash_item_at(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    ) {
                        state.overlay_selected = index;
                        if let Some(action) = state.activate_slash_command()
                            && dispatch_overlay_action(action, &commands)
                        {
                            break;
                        }
                    }
                }
                MouseEventKind::ScrollUp if state.view == View::Tasks => {
                    state.task_selected = state.task_selected.saturating_sub(1);
                }
                MouseEventKind::ScrollDown if state.view == View::Tasks => {
                    state.task_selected =
                        (state.task_selected + 1).min(state.subagent_tasks.len().saturating_sub(1));
                }
                MouseEventKind::Moved if state.view == View::Tasks => {
                    if let Some((x, y, width, height)) = state.task_list_rect.get()
                        && mouse.column >= x
                        && mouse.column < x.saturating_add(width)
                        && mouse.row >= y
                        && mouse.row < y.saturating_add(height)
                    {
                        let index = state.task_list_offset.get() + usize::from(mouse.row - y) / 2;
                        if index < state.subagent_tasks.len() {
                            state.task_selected = index;
                        }
                    }
                }
                MouseEventKind::Down(_) if state.view == View::Tasks => {
                    if let Some((x, y, width, height)) = state.task_list_rect.get()
                        && mouse.column >= x
                        && mouse.column < x.saturating_add(width)
                        && mouse.row >= y
                        && mouse.row < y.saturating_add(height)
                    {
                        let index = state.task_list_offset.get() + usize::from(mouse.row - y) / 2;
                        if index < state.subagent_tasks.len() {
                            state.task_selected = index;
                            state.open_selected_subagent();
                        }
                    }
                }
                MouseEventKind::ScrollUp if state.view == View::Dashboard => {
                    state.dashboard_selected = state.dashboard_selected.saturating_sub(1);
                }
                MouseEventKind::ScrollDown if state.view == View::Dashboard => {
                    state.dashboard_selected = (state.dashboard_selected + 1)
                        .min(state.available_sessions.len().saturating_sub(1));
                }
                MouseEventKind::Moved if state.view == View::Dashboard => {
                    if let Some((x, y, width, height)) = state.dashboard_list_rect.get()
                        && mouse.column >= x
                        && mouse.column < x.saturating_add(width)
                        && mouse.row >= y
                        && mouse.row < y.saturating_add(height)
                    {
                        let row = usize::from(mouse.row - y);
                        let visible = usize::from(height);
                        if let Some(index) = state.dashboard_session_at_row(row, visible) {
                            state.dashboard_selected = index;
                        }
                    }
                }
                MouseEventKind::Down(_) if state.view == View::Dashboard => {
                    if let Some((x, y, width, height)) = state.dashboard_list_rect.get()
                        && mouse.column >= x
                        && mouse.column < x.saturating_add(width)
                        && mouse.row >= y
                        && mouse.row < y.saturating_add(height)
                        && let Some(index) = state.dashboard_session_at_row(
                            usize::from(mouse.row - y),
                            usize::from(height),
                        )
                    {
                        state.dashboard_selected = index;
                        if let Some(path) = state.activate_dashboard_session()
                            && let Some(sender) = &commands
                        {
                            let _ = sender.send(UiCommand::ResumeSession(path));
                        }
                        state.view = View::Transcript;
                    }
                }
                MouseEventKind::Down(_)
                    if state.view == View::Subagent
                        && ui::subagent_close_at(
                            size.width,
                            size.height,
                            mouse.column,
                            mouse.row,
                        ) =>
                {
                    state.view = View::Tasks;
                    state.scroll_from_bottom = 0;
                }
                MouseEventKind::Moved | MouseEventKind::Down(_) if state.view == View::Subagent => {
                }
                MouseEventKind::ScrollUp => state.scroll_up(1, max_scroll),
                MouseEventKind::ScrollDown => state.scroll_down(1),
                MouseEventKind::Moved => {
                    let header_hover = state.header_target_at(mouse.column, mouse.row);
                    let composer_hover = state
                        .composer_targets
                        .borrow()
                        .iter()
                        .find(|(_, start, end, row)| {
                            mouse.row == *row && mouse.column >= *start && mouse.column < *end
                        })
                        .map(|(kind, _, _, _)| *kind);
                    let paste_hover = state
                        .paste_targets
                        .borrow()
                        .iter()
                        .find(|(_, start, end, row)| {
                            mouse.row == *row && mouse.column >= *start && mouse.column < *end
                        })
                        .map(|(id, _, _, _)| *id);
                    let image_hover = state
                        .image_targets
                        .borrow()
                        .iter()
                        .find(|(_, start, end, row)| {
                            mouse.row == *row && mouse.column >= *start && mouse.column < *end
                        })
                        .map(|(id, _, _, _)| *id);
                    let composer_changed = state.header_hover != header_hover
                        || state.composer_hover != composer_hover
                        || state.paste_hover != paste_hover
                        || state.image_hover != image_hover;
                    state.header_hover = header_hover;
                    state.composer_hover = composer_hover;
                    state.paste_hover = paste_hover;
                    state.image_hover = image_hover;
                    let hovered = ui::section_hit_at(
                        &state,
                        size.width,
                        size.height,
                        mouse.column,
                        mouse.row,
                    );
                    let transcript_changed =
                        state.set_hovered_transcript_target(hovered.map(|hit| (hit.index, hit.id)));
                    dirty = composer_changed || transcript_changed;
                }
                MouseEventKind::Down(_) => {
                    let header_target = state.header_target_at(mouse.column, mouse.row);
                    let composer_target = state.composer_hover.or_else(|| {
                        state
                            .composer_targets
                            .borrow()
                            .iter()
                            .find(|(_, start, end, row)| {
                                mouse.row == *row && mouse.column >= *start && mouse.column < *end
                            })
                            .map(|(kind, _, _, _)| *kind)
                    });
                    let paste_target = state
                        .paste_targets
                        .borrow()
                        .iter()
                        .find(|(_, start, end, row)| {
                            mouse.row == *row && mouse.column >= *start && mouse.column < *end
                        })
                        .map(|(id, _, _, _)| *id);
                    let image_target = state
                        .image_targets
                        .borrow()
                        .iter()
                        .find(|(_, start, end, row)| {
                            mouse.row == *row && mouse.column >= *start && mouse.column < *end
                        })
                        .map(|(id, _, _, _)| *id);
                    if let Some(target) = header_target {
                        match target {
                            0 => match ClipboardContext::new()
                                .and_then(|clipboard| clipboard.set_text(state.cwd.clone()))
                            {
                                Ok(()) => state.status = "working directory copied".into(),
                                Err(error) => {
                                    state.status = format!("copy working directory failed: {error}")
                                }
                            },
                            1 => state.view = View::Tasks,
                            2 => {
                                if let Some(index) = state
                                    .entries
                                    .iter()
                                    .rposition(|entry| matches!(entry, state::Entry::Plan { .. }))
                                {
                                    state.focus_scrollback();
                                    state.focused_entry = Some(index);
                                    state.scroll_from_bottom = 0;
                                }
                            }
                            3 => state.queue_visible = !state.queue_visible,
                            _ => state.show_context_info(),
                        }
                    } else if let Some(id) = image_target {
                        state.begin_image_view(id);
                    } else if let Some(id) = paste_target {
                        state.begin_paste_edit(id);
                    } else if let Some(target) = composer_target {
                        if target == 2 {
                            state.cycle_permission_mode();
                            if let Some(sender) = &commands {
                                let _ = sender.send(UiCommand::SetPermissionMode(
                                    state.permission_mode.wire_value().into(),
                                ));
                            }
                        } else {
                            state.open_overlay(if target == 0 {
                                OverlayKind::ModelPicker
                            } else {
                                OverlayKind::ThinkingPicker
                            });
                        }
                    } else if agent_layout::AgentLayout::compute(
                        ratatui::layout::Rect::new(0, 0, size.width, size.height),
                        &state,
                    )
                    .prompt
                    .contains((mouse.column, mouse.row).into())
                    {
                        state.focus_prompt();
                    } else if let Some(hit) =
                        ui::section_hit_at(&state, size.width, size.height, mouse.column, mouse.row)
                    {
                        state.focus_scrollback();
                        state.focused_entry = Some(hit.index);
                        state.focused_target_id = Some(hit.id.clone());
                        state.focused_tool = matches!(
                            state.entries.get(hit.index),
                            Some(state::Entry::Tool { .. })
                        )
                        .then_some(hit.index);
                        state.pending_transcript_click = Some((hit.index, hit.id));
                        state.begin_scrollback_drag(mouse.row);
                    } else if state.transcript_contains(mouse.column, mouse.row) {
                        state.focus_scrollback();
                        state.pending_transcript_click = None;
                        state.begin_scrollback_drag(mouse.row);
                    } else {
                        state.pending_transcript_click = None;
                        state.focus_scrollback();
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

async fn next_agent_event(
    events: &mut Option<broadcast::Receiver<pi_harness::AgentEvent>>,
) -> Result<pi_harness::AgentEvent, broadcast::error::RecvError> {
    match events {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

fn dispatch_overlay_action(
    action: OverlayAction,
    commands: &Option<mpsc::UnboundedSender<UiCommand>>,
) -> bool {
    match action {
        OverlayAction::None => false,
        OverlayAction::SetTheme(mode) => {
            let _ = theme::save_preference(mode);
            false
        }
        OverlayAction::RefreshSessions => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::RefreshSessions);
            }
            false
        }
        OverlayAction::LoadWorkflowCatalog => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::LoadWorkflowCatalog);
            }
            false
        }
        OverlayAction::PreviewWorkflow { workflow } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::PreviewWorkflow(workflow));
            }
            false
        }
        OverlayAction::StartWorkflow {
            workflow,
            input,
            expected_definition_hash,
        } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::StartWorkflow {
                    workflow,
                    input,
                    expected_definition_hash,
                });
            }
            false
        }
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
        OverlayAction::RenameSession { target, name } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::RenameSession { target, name });
            }
            false
        }
        OverlayAction::DeleteSession { target } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::DeleteSession(target));
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
            instructions,
        } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::NavigateTree {
                    entry_id,
                    summarize,
                    instructions,
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
        OverlayAction::SetThinking(level) => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetThinking(level));
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
        OverlayAction::SetExtensionEnabled { path, enabled } => {
            if let Some(sender) = commands {
                let _ = sender.send(UiCommand::SetExtensionEnabled { path, enabled });
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

    use super::{buffer_text, fixtures, overlay, theme::Theme, ui};

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
            assert!(output.contains("Shift+Tab: Mode"));
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
                .any(|cell| cell.fg == Theme::GROK_NIGHT.success)
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

        assert!(output.contains("* Thinking…"));
        assert!(!output.contains('⠹'));
        assert!(output.contains("Enter: Queue"));
        assert!(output.contains("Ctrl+Enter: Send now"));
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
    fn image_selected_from_file_reference_picker_becomes_an_attachment() {
        let path =
            std::env::temp_dir().join(format!("pi-shell-at-image-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(2, 3, image::Rgba([10, 20, 30, 255]))
            .save(&path)
            .unwrap();
        let path_text = path.to_string_lossy().into_owned();
        let mut state = super::AppState {
            prompt: "inspect @".into(),
            cursor: 9,
            available_files: vec![path_text],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::FilePicker);

        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.prompt, "inspect ");
        assert_eq!(state.image_attachments.len(), 1);
        assert_eq!(
            (
                state.image_attachments[0].width,
                state.image_attachments[0].height
            ),
            (2, 3)
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn multiline_paste_is_inserted_inline_and_expands_on_submit() {
        let mut state = super::AppState {
            prompt: "check this  some text".into(),
            cursor: 11,
            ..super::AppState::default()
        };
        state.insert_paste("one\r\ntwo\rthree".into());
        assert_eq!(state.prompt, "check this one\ntwo\nthree some text");
        assert_eq!(state.paste_blocks.len(), 1);
        assert_eq!(state.paste_blocks[0].end - state.paste_blocks[0].start, 13);
        state.delete();
        assert_eq!(state.prompt, "check this  some text");
        assert!(state.paste_blocks.is_empty());

        state.cursor = 11;
        state.insert_paste("one\ntwo".into());
        assert_eq!(
            state.submit_prompt().as_deref(),
            Some("check this one\ntwo some text")
        );
        assert!(state.prompt.is_empty());
        assert!(state.paste_blocks.is_empty());
    }

    #[test]
    fn paste_editor_opens_with_full_content_and_applies_multiline_changes() {
        let mut state = super::AppState::default();
        state.insert_paste("one\ntwo".into());
        let id = state.paste_blocks[0].id;

        assert!(state.begin_paste_edit(id));
        assert_eq!(state.overlay, super::OverlayKind::PasteEditor);
        assert_eq!(state.overlay_query, "one\ntwo");
        state.move_paste_editor_cursor(-3);
        state.insert_paste_editor_text(" edited".into());
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));

        assert_eq!(state.prompt, "one\n editedtwo");
        assert_eq!(state.overlay, super::OverlayKind::None);
        assert_eq!(state.paste_blocks[0].end, state.prompt.chars().count());
    }

    #[test]
    fn paste_editor_cancel_preserves_original_content() {
        let mut state = super::AppState::default();
        state.insert_paste("original".into());
        let id = state.paste_blocks[0].id;
        assert!(state.begin_paste_edit(id));
        state.overlay_query = "changed".into();

        assert!(matches!(
            state.cancel_oauth(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.prompt, "original");
        assert_eq!(state.overlay, super::OverlayKind::None);
    }

    #[test]
    fn editing_one_paste_keeps_later_paste_ranges_aligned() {
        let mut state = super::AppState::default();
        state.insert_paste("first".into());
        let first = state.paste_blocks[0].id;
        state.insert_char(' ');
        state.insert_paste("second".into());
        let second = state.paste_blocks[1].id;

        assert!(state.replace_paste(first, "a much longer first".into()));
        assert!(state.focus_paste(second));
        state.delete();

        assert_eq!(state.prompt, "a much longer first ");
        assert_eq!(state.paste_blocks.len(), 1);
    }

    #[test]
    fn paste_editor_navigates_visual_rows_and_clicks_to_position() {
        let mut state = super::AppState::default();
        state.insert_paste("abcd\nx".into());
        let id = state.paste_blocks[0].id;
        assert!(state.begin_paste_edit(id));
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();

        state.move_paste_editor_vertical(-1);
        assert_eq!(state.overlay_cursor, 1);
        state.move_paste_editor_line_edge(true);
        assert_eq!(state.overlay_cursor, 4);

        let target = state
            .paste_editor_targets
            .borrow()
            .iter()
            .find(|(_, _, offset)| *offset == 0)
            .copied()
            .unwrap();
        assert!(state.click_paste_editor(target.0, target.1));
        assert_eq!(state.overlay_cursor, 0);
    }

    #[test]
    fn image_attachment_renders_as_filename_chip_and_submits_natively() {
        let mut state = super::AppState::default();
        state.attach_image(super::state::ImageAttachment {
            id: 0,
            path: "/tmp/screenshot.png".into(),
            name: "a-very-long-screenshot-filename.png".into(),
            width: 1280,
            height: 720,
            mime_type: "image/png".into(),
            temporary: true,
            preview_width: 2,
            preview_height: 2,
            preview_rgba: vec![255; 16],
        });
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 80, 20);
        assert!(output.contains("[a-very-long-screensho…]"));
        let id = state.image_attachments[0].id;
        assert!(state.begin_image_view(id));
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let viewer = buffer_text(terminal.backend().buffer(), 80, 20);
        assert!(viewer.contains("Image attachment"));
        assert!(viewer.contains("1280×720"));
        assert!(viewer.contains("[ Remove ]"));
        state.close_overlay();

        let (text, images) = state.submit_message().unwrap();
        assert!(text.is_empty());
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].path, "/tmp/screenshot.png");
        assert!(state.image_attachments.is_empty());
    }

    #[test]
    fn copied_file_uri_is_decoded_and_portrait_preview_keeps_aspect_ratio() {
        assert_eq!(
            super::percent_decode_path("/home/void/Pictures/flower%202.jpg"),
            Some("/home/void/Pictures/flower 2.jpg".into())
        );
        assert_eq!(
            super::percent_decode_path("/home/void/Pictures/flower2.jpg"),
            Some("/home/void/Pictures/flower2.jpg".into())
        );
        let pixels = vec![255; 1060 * 1500 * 4];
        let (width, height, _) = super::image_preview(1060, 1500, &pixels);
        assert_eq!((width, height), (42, 60));
    }

    #[test]
    fn clearing_paste_command_preserves_existing_attachments() {
        let mut state = super::AppState::default();
        state.attach_image(super::state::ImageAttachment {
            id: 0,
            path: "/tmp/first.png".into(),
            name: "first.png".into(),
            width: 1,
            height: 1,
            mime_type: "image/png".into(),
            temporary: true,
            preview_width: 1,
            preview_height: 1,
            preview_rgba: vec![255; 4],
        });
        state.prompt = "/paste".into();
        state.cursor = state.prompt.chars().count();

        state.clear_prompt_text();

        assert!(state.prompt.is_empty());
        assert_eq!(state.image_attachments.len(), 1);
        assert_eq!(state.image_attachments[0].name, "first.png");
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
        state.multiline_mode = true;
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        // Composer title now packs all three status fields into one line.
        assert!(output.contains("grok-4 fast"));
        assert!(output.contains("thinking high"));
        assert!(output.contains("plan"));
        assert!(output.contains("multiline"));
        let targets = state.composer_targets.borrow();
        assert_eq!(targets.len(), 3);
        assert!(
            targets[0].1 >= width / 2,
            "composer controls should occupy the bottom-right border: {targets:?}"
        );
        assert!(targets.windows(2).all(|pair| pair[0].2 == pair[1].1));
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
    fn enter_on_plain_transcript_block_opens_viewer() {
        let mut state = super::AppState::default();
        state.entries.push(super::state::Entry::Assistant {
            lines: vec!["inspect me".into()],
            timestamp: String::new(),
        });
        state.focus_scrollback();
        state.activate_entry_at(0);
        assert_eq!(state.view, super::View::BlockViewer);
        assert_eq!(state.viewed_entry, Some(0));
    }

    #[test]
    fn scrollback_drag_tracks_pointer_distance_and_clamps() {
        let mut state = super::AppState {
            scroll_from_bottom: 4,
            ..super::AppState::default()
        };
        state.begin_scrollback_drag(10);
        state.drag_scrollback_to(15, 20);
        assert_eq!(state.scroll_from_bottom, 9);
        state.drag_scrollback_to(2, 20);
        assert_eq!(state.scroll_from_bottom, 0);

        state.transcript_scrollbar_rect.set(Some((99, 2, 1, 10)));
        state.drag_scrollbar_to(2, 100);
        assert_eq!(state.scroll_from_bottom, 100);
        state.drag_scrollbar_to(11, 100);
        assert_eq!(state.scroll_from_bottom, 0);
    }

    #[test]
    fn deferred_transcript_click_expands_resumed_reasoning_but_drag_cancels_it() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ReasoningDelta {
            text: "restored thought".into(),
        });
        let target_id = state.entry_target_id(0).unwrap();

        state.pending_transcript_click = Some((0, target_id.clone()));
        state.finish_transcript_click(Some((0, target_id.clone())));
        assert!(matches!(
            state.entries.first(),
            Some(super::state::Entry::Reasoning { expanded: true, .. })
        ));

        state.pending_transcript_click = Some((0, target_id.clone()));
        state.pending_transcript_click = None; // promoted to a drag
        state.finish_transcript_click(Some((0, target_id)));
        assert!(matches!(
            state.entries.first(),
            Some(super::state::Entry::Reasoning { expanded: true, .. })
        ));
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

        state.apply(AgentEvent::ToolCallStart {
            id: "read-between-thoughts".into(),
            name: "read".into(),
            args: serde_json::json!({"path":"README.md"}),
        });
        state.apply(AgentEvent::ReasoningDelta {
            text: " third".into(),
        });
        assert_eq!(
            state
                .entries
                .iter()
                .filter(|entry| matches!(entry, super::state::Entry::Reasoning { .. }))
                .count(),
            1,
            "tool activity within one turn should not create repeated Thinking rows"
        );
        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Reasoning { text, .. } if text == "first second third"
        ));
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
        let top_rows = (0..height)
            .filter(|row| top_buffer[(3, *row)].bg == Theme::GROK_NIGHT.user_background)
            .count();
        assert!(top_rows >= 3);

        state.scroll_to_bottom();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let sticky_buffer = terminal.backend().buffer();
        let sticky_rows = (0..height)
            .filter(|row| sticky_buffer[(3, *row)].bg == Theme::GROK_NIGHT.user_background)
            .count();
        assert!(sticky_rows >= 3);
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
    fn full_screen_block_viewer_renders_selected_tool_output() {
        let mut state = fixtures::tools();
        let index = state
            .entries
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    super::state::Entry::Tool {
                        result: Some(_),
                        ..
                    }
                )
            })
            .unwrap();
        state.viewed_entry = Some(index);
        state.view = super::View::BlockViewer;
        let backend = TestBackend::new(100, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 100, 28);
        assert!(output.contains("tool"));
        assert!(output.contains("Esc/q close"));
    }

    #[test]
    fn home_alias_opens_welcome_dashboard() {
        let mut state = fixtures::conversation();
        state.prompt = "/home".into();
        state.cursor = state.prompt.chars().count();
        state.activate_slash_command();
        assert_eq!(state.view, super::View::Dashboard);
    }

    #[test]
    fn resume_reset_drops_transient_input_queue_and_permissions() {
        let mut state = fixtures::permission();
        state.prompt = "draft from old session".into();
        state.cursor = state.prompt.chars().count();
        state.queued_follow_up.push("later".into());
        state.apply(pi_harness::AgentEvent::SessionReset);
        assert!(state.prompt.is_empty());
        assert!(state.queued_follow_up.is_empty());
        assert!(state.pending_permission.is_none());
        assert_eq!(state.overlay, super::OverlayKind::None);
    }

    #[test]
    fn tool_and_diff_shortcuts_toggle_every_matching_entry() {
        let mut state = fixtures::tools();

        state.toggle_all_tools();
        assert!(
            state
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    super::state::Entry::Tool { expanded, .. } => Some(*expanded),
                    _ => None,
                })
                .all(|expanded| expanded)
        );
        assert!(!state.expanded_tool_groups.is_empty());
        state.toggle_all_tools();
        assert!(
            state
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    super::state::Entry::Tool { expanded, .. } => Some(*expanded),
                    _ => None,
                })
                .all(|expanded| !expanded)
        );
        assert!(state.expanded_tool_groups.is_empty());

        if let Some(super::state::Entry::Diff { expanded, .. }) = state
            .entries
            .iter_mut()
            .find(|entry| matches!(entry, super::state::Entry::Diff { .. }))
        {
            *expanded = false;
        }
        state.toggle_all_diffs();
        assert!(
            state
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    super::state::Entry::Diff { expanded, .. } => Some(*expanded),
                    _ => None,
                })
                .all(|expanded| expanded)
        );
        state.toggle_all_diffs();
        assert!(
            state
                .entries
                .iter()
                .filter_map(|entry| match entry {
                    super::state::Entry::Diff { expanded, .. } => Some(*expanded),
                    _ => None,
                })
                .all(|expanded| !expanded)
        );
    }

    #[test]
    fn scrollbar_thumb_is_dynamic_and_reaches_both_ends() {
        assert_eq!(ui::scrollbar_geometry(100, 25, 20, 0), Some((0, 5)));
        assert_eq!(ui::scrollbar_geometry(100, 25, 20, 75), Some((15, 5)));
        assert_eq!(ui::scrollbar_geometry(50, 25, 20, 0), Some((0, 10)));
        assert_eq!(ui::scrollbar_geometry(25, 25, 20, 0), None);
    }

    #[test]
    fn tools_use_compact_portable_headers() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = fixtures::tools();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Run 3 calls"));
        assert!(output.contains("+ Edit crates/pi-tui/src/ui.rs"));
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
        assert!(expanded.contains("|- + Run"));
        assert_eq!(
            ui::section_hit_at(&state, width, height, 5, 9).map(|hit| hit.index),
            Some(2)
        );
        state.toggle_tool_at(2);
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let focused = buffer_text(terminal.backend().buffer(), width, height);
        assert!(!focused.contains('▌'));
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
            read.chars().position(|character| character == '+'),
            run.chars().position(|character| character == '+')
        );
        let detail = output.lines().find(|line| line.contains("└ done")).unwrap();
        assert_eq!(
            run.chars().position(|character| character == 'R'),
            detail.chars().position(|character| character == '└')
        );
    }

    #[test]
    fn historical_tool_without_duration_does_not_show_replay_time() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "historical".into(),
            name: "read".into(),
            args: serde_json::json!({"path": "README.md"}),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "historical".into(),
            result: pi_harness::ToolResult {
                content: "done".into(),
                details: None,
            },
            is_error: false,
            duration_ms: None,
        });

        match &state.entries[0] {
            super::state::Entry::Tool { duration, .. } => assert_eq!(duration, &None),
            other => panic!("expected tool entry, got {other:?}"),
        }
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
        state.hovered_target_id = Some("tool-group:tool-test".into());
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("> Run 3 calls"));
        let buffer = terminal.backend().buffer();
        let row = (0..height)
            .find(|row| {
                (0..width)
                    .map(|column| buffer[(column, *row)].symbol())
                    .collect::<String>()
                    .contains("> Run 3 calls")
            })
            .unwrap();
        assert_eq!(buffer[(10, row)].bg, Theme::GROK_NIGHT.bg_hover);
        assert!(buffer.content.iter().all(|cell| {
            !matches!(cell.symbol(), "│" | "┆" | "┌" | "┐" | "└" | "┘")
                || cell.fg != Theme::GROK_NIGHT.hover_border
        }));
    }

    #[test]
    fn hovered_edit_uses_pointer_and_moves_border_without_scrolling() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.focus_scrollback();
        state.focused_target_id = Some("tool-group:tool-test".into());
        state.hovered_entry = Some(4);
        state.hovered_target_id = Some("diff:fixture-diff-2".into());
        let scroll_before = state.scroll_from_bottom;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        let edit = output.lines().find(|line| line.contains("> Edit")).unwrap();
        assert_eq!(edit.chars().nth(1), Some('│'));
        assert_eq!(state.scroll_from_bottom, scroll_before);
        assert_eq!(
            state.focused_target_id.as_deref(),
            Some("tool-group:tool-test")
        );
    }

    #[test]
    fn clearing_hover_removes_every_preview_rail_cell() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.hovered_target_id = Some("diff:fixture-diff-2".into());
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "│" && cell.fg == Theme::GROK_NIGHT.hover_border)
        );

        state.hovered_target_id = None;
        state.hovered_entry = None;
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        assert!(terminal.backend().buffer().content.iter().all(|cell| {
            !matches!(cell.symbol(), "│" | "┆" | "┌" | "┐" | "└" | "┘")
                || cell.fg != Theme::GROK_NIGHT.hover_border
        }));
    }

    #[test]
    fn unchanged_hover_target_does_not_request_another_frame() {
        let mut state = fixtures::tools();
        assert!(state.set_hovered_transcript_target(Some((1, "tool:one".into()))));
        assert!(!state.set_hovered_transcript_target(Some((1, "tool:one".into()))));
        assert!(state.set_hovered_transcript_target(None));
        assert!(!state.set_hovered_transcript_target(None));
    }

    #[test]
    fn focused_section_shows_brackets_at_both_transcript_edges() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.focus_scrollback();
        state.focused_target_id = Some("tool-group:tool-test".into());
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        let lines = output.lines().collect::<Vec<_>>();
        let row = lines
            .iter()
            .position(|line| line.contains("Run 3 calls"))
            .unwrap();
        assert_eq!(lines[row].chars().nth(1), Some('│'));
        assert_eq!(lines[row].chars().nth(97), Some('│'));
        assert_eq!(lines[row - 1].chars().nth(1), Some('┌'));
        assert_eq!(lines[row - 1].chars().nth(97), Some('┐'));
        assert_eq!(lines[row + 1].chars().nth(1), Some('└'));
        assert_eq!(lines[row + 1].chars().nth(97), Some('┘'));
    }

    #[test]
    fn focus_brackets_span_dynamic_section_height() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.focus_scrollback();
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
        assert_eq!(buffer[(1, edit_row)].symbol(), "│");
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
    fn oversized_focus_anchors_top_and_draws_bottom_continuation() {
        let (width, height) = (80, 18);
        let mut state = super::AppState {
            entries: vec![super::state::Entry::Diff {
                id: "long-diff".into(),
                path: "src/long.rs".into(),
                lines: (1..=40)
                    .map(|number| super::state::DiffLine {
                        number: Some(number),
                        text: format!("changed line {number}"),
                        kind: super::state::DiffKind::Added,
                    })
                    .collect(),
                expanded: true,
            }],
            focus: super::Focus::Scrollback,
            ..super::AppState::default()
        };
        ui::move_section_focus(&mut state, width, height, 1);
        assert_eq!(
            state.scroll_from_bottom,
            ui::max_scroll(&state, width, height)
        );

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        let layout = super::agent_layout::AgentLayout::compute(
            ratatui::layout::Rect::new(0, 0, width, height),
            &state,
        );
        let first_row: String = (0..width)
            .map(|column| buffer[(column, layout.scrollback.y)].symbol())
            .collect();
        let bottom_row = layout.scrollback.bottom().saturating_sub(1);
        assert!(first_row.contains("Edit src/long.rs"));
        for row in bottom_row - 2..=bottom_row {
            let expected = if row == bottom_row { "┆" } else { "│" };
            assert_eq!(buffer[(layout.scrollback.x, row)].symbol(), expected);
            assert_eq!(
                buffer[(layout.scrollback.right().saturating_sub(2), row)].symbol(),
                expected
            );
        }
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
    fn activating_scrollback_never_focuses_a_hidden_group_child() {
        let mut state = fixtures::tools();
        state.entries.truncate(4);
        state.focus = super::Focus::Prompt;
        state.focused_entry = None;
        state.focused_target_id = None;

        ui::focus_scrollback(&mut state, 100, 32);

        assert_eq!(state.focus, super::Focus::Scrollback);
        assert_eq!(state.focused_entry, Some(1));
        assert_eq!(
            state.focused_target_id.as_deref(),
            Some("tool-group:tool-test")
        );
    }

    #[test]
    fn expanded_tool_child_gets_its_own_grok_selection_box() {
        let (width, height) = (100, 32);
        let mut state = fixtures::tools();
        state.toggle_tool_group(1);
        state.focus = super::Focus::Scrollback;
        state.focused_entry = Some(2);
        state.focused_tool = Some(2);
        state.focused_target_id = Some("tool:tool-clippy".into());

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let buffer = terminal.backend().buffer();
        let child_row = (0..height)
            .find(|row| {
                (0..width)
                    .map(|column| buffer[(column, *row)].symbol())
                    .collect::<String>()
                    .contains("cargo clippy --workspace")
            })
            .unwrap();

        assert_eq!(buffer[(1, child_row)].symbol(), "│");
        assert_eq!(buffer[(97, child_row)].symbol(), "│");
        assert_eq!(
            buffer[(1, child_row)].fg,
            Theme::GROK_NIGHT.selection_border
        );
        assert_eq!(
            buffer[(97, child_row)].fg,
            Theme::GROK_NIGHT.selection_border
        );
        let group_row = (0..height)
            .find(|row| {
                (0..width)
                    .map(|column| buffer[(column, *row)].symbol())
                    .collect::<String>()
                    .contains("Run 3 calls")
            })
            .unwrap();
        assert!(
            group_row < child_row,
            "group header must remain outside child box"
        );
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
    fn semantic_entry_identity_survives_streaming_and_manual_folds_are_pinned() {
        let mut state = super::AppState::default();
        state.apply(pi_harness::AgentEvent::ReasoningDelta {
            text: "first".into(),
        });
        let id = state.entry_target_id(0).unwrap();
        state.apply(pi_harness::AgentEvent::ReasoningDelta {
            text: " second".into(),
        });
        assert_eq!(state.entry_target_id(0).as_deref(), Some(id.as_str()));
        state.toggle_entry_at(0);
        assert!(state.pinned_entry_modes.contains(&id));
    }

    #[test]
    fn session_reset_allocates_fresh_semantic_entry_ids() {
        let mut state = super::AppState::default();
        state.apply(pi_harness::AgentEvent::ReasoningDelta { text: "old".into() });
        let old = state.entry_target_id(0).unwrap();
        state.apply(pi_harness::AgentEvent::SessionReset);
        state.apply(pi_harness::AgentEvent::ReasoningDelta { text: "new".into() });
        assert_ne!(state.entry_target_id(0).unwrap(), old);
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
    fn internal_subagent_polling_tools_stay_out_of_transcript() {
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
        state.apply(AgentEvent::ToolCallStart {
            id: "custom-agent".into(),
            name: "spawn_agent".into(),
            args: serde_json::json!({"description": "External dynamic agent tool"}),
        });

        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Tool { label, detail, .. }
                if label == "Agent" && detail == "Scout Rust crate public APIs"
        ));
        assert_eq!(state.entries.len(), 2);
        assert!(!state.entries.iter().any(|entry| {
            matches!(entry, super::state::Entry::Tool { label, .. } if label == "Agent result")
        }));
        assert!(state.entries.iter().any(|entry| {
            matches!(entry, super::state::Entry::Tool { id, .. } if id == "custom-agent")
        }));
    }

    #[test]
    fn grok_batch_file_tools_render_semantic_summaries() {
        let mut state = super::AppState::default();
        for (id, name, args) in [
            (
                "find",
                "Find_files",
                serde_json::json!({"queries":[
                    {"pattern":"package.json"},
                    {"pattern":"README*"},
                    {"pattern":"Cargo.toml"},
                    {"pattern":"go.mod"}
                ]}),
            ),
            (
                "grep",
                "Grep_files",
                serde_json::json!({"queries":[
                    {"pattern":"TODO|FIXME", "path":".", "glob":"!node_modules/**"},
                    {"pattern":"test|describe", "path":"src"}
                ]}),
            ),
            (
                "read",
                "Read_files",
                serde_json::json!({
                    "paths":["README.md","Cargo.toml","sidecar/package.json","docs/workflows.md"],
                    "limit":260
                }),
            ),
        ] {
            state.apply(AgentEvent::ToolCallStart {
                id: id.into(),
                name: name.into(),
                args,
            });
        }

        assert!(matches!(
            &state.entries[0],
            super::state::Entry::Tool { label, detail, .. }
                if label == "Find"
                    && detail == "4 patterns · \"package.json\"; \"README*\"; …"
        ));
        assert!(matches!(
            &state.entries[1],
            super::state::Entry::Tool { label, detail, .. }
                if label == "Search"
                    && detail == "2 queries · \"TODO|FIXME\"; \"test|describe\" in src"
        ));
        assert!(matches!(
            &state.entries[2],
            super::state::Entry::Tool { label, detail, .. }
                if label == "Read"
                    && detail == "4 files · README.md, Cargo.toml, sidecar/package.json, …"
        ));

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 120, 24);
        assert!(output.contains("Find 4 patterns"), "{output}");
        assert!(output.contains("Search 2 queries"));
        assert!(output.contains("Read 4 files"));
        assert!(!output.contains("Find_files") && !output.contains("queries\":"));
    }

    #[test]
    fn grouped_batch_tools_report_calls_units_and_failures_truthfully() {
        let mut state = super::AppState::default();
        for index in 0..3 {
            state.apply(AgentEvent::ToolCallStart {
                id: format!("read-{index}"),
                name: "read_files".into(),
                args: serde_json::json!({
                    "paths":[
                        format!("src/{index}/a.rs"),
                        format!("src/{index}/b.rs"),
                        format!("src/{index}/c.rs"),
                        format!("src/{index}/d.rs")
                    ]
                }),
            });
        }
        for index in 0..4 {
            state.apply(AgentEvent::ToolCallStart {
                id: format!("run-{index}"),
                name: "bash".into(),
                args: serde_json::json!({"command":format!("check-{index}")}),
            });
            state.apply(AgentEvent::ToolCallResult {
                id: format!("run-{index}"),
                result: pi_harness::ToolResult {
                    content: String::new(),
                    details: None,
                },
                is_error: index == 1 || index == 3,
                duration_ms: Some(10),
            });
        }

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 120, 24);
        assert!(output.contains("Read 3 calls · 12 files >"), "{output}");
        assert!(output.contains("Run 4 calls · 2 failed >"), "{output}");
        assert!(!output.contains('◆') && !output.contains('◐') && !output.contains('▯'));
    }

    #[test]
    fn native_subagent_updates_one_live_lifecycle_entry_and_keeps_child_transcript() {
        let mut state = super::AppState::default();
        let mut task = pi_harness::SubagentTask {
            task_id: "task-native".into(),
            parent_session_id: "parent".into(),
            child_session_id: Some("child".into()),
            child_session_path: Some("/tmp/child.jsonl".into()),
            description: "Inspect modal".into(),
            subagent_type: "explore".into(),
            capability_mode: "execute".into(),
            isolation: "none".into(),
            background: true,
            status: "running".into(),
            activity: "Thinking".into(),
            started_at_ms: 1,
            completed_at_ms: None,
            duration_ms: 25,
            output: None,
            error: None,
            failure_kind: None,
            model: Some("test/model".into()),
            thinking_level: Some("high".into()),
            worktree_path: None,
            cwd: None,
            workflow_run_id: None,
        };
        state.apply(AgentEvent::SubagentUpdate {
            task: Box::new(task.clone()),
        });
        state.apply(AgentEvent::SubagentTranscript {
            task_id: task.task_id.clone(),
            event: Box::new(AgentEvent::TextDelta {
                text: "evidence".into(),
            }),
        });
        state.apply(AgentEvent::SubagentTranscript {
            task_id: task.task_id.clone(),
            event: Box::new(AgentEvent::ToolCallStart {
                id: "child-read".into(),
                name: "read_file".into(),
                args: serde_json::json!({"path":"missing.rs"}),
            }),
        });
        state.apply(AgentEvent::SubagentTranscript {
            task_id: task.task_id.clone(),
            event: Box::new(AgentEvent::ToolCallResult {
                id: "child-read".into(),
                result: pi_harness::ToolResult {
                    content: "not found".into(),
                    details: None,
                },
                is_error: true,
                duration_ms: Some(1_234),
            }),
        });
        task.status = "completed".into();
        task.activity = "Completed".into();
        task.duration_ms = 200;
        task.output = Some("report".into());
        state.apply(AgentEvent::SubagentUpdate {
            task: Box::new(task),
        });

        assert_eq!(
            state
                .entries
                .iter()
                .filter(
                    |entry| matches!(entry, super::state::Entry::Tool { id, .. } if id == "subagent:task-native")
                )
                .count(),
            1
        );
        assert_eq!(state.subagent_transcripts["task-native"].len(), 3);
        assert_eq!(state.subagent_tasks["task-native"].status, "completed");

        state.view = super::View::Tasks;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let tasks = buffer_text(terminal.backend().buffer(), 100, 24);
        assert!(tasks.contains("Tasks · Subagents"));
        assert!(tasks.contains("Inspect modal"));

        state.open_selected_subagent();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let transcript = buffer_text(terminal.backend().buffer(), 100, 24);
        assert!(transcript.contains("evidence"));
        assert!(transcript.contains("read_file"));
        assert!(transcript.contains("failed 1.2s"), "{transcript}");
    }

    #[test]
    fn subagent_updates_preserve_user_selected_tool_group_expansion() {
        let mut state = super::AppState::default();
        for index in 0..2 {
            state.apply(AgentEvent::ToolCallStart {
                id: format!("read-{index}"),
                name: "read_file".into(),
                args: serde_json::json!({"path":format!("file-{index}.rs")}),
            });
        }
        state.expanded_tool_groups.insert(0);
        let mut task = pi_harness::SubagentTask {
            task_id: "worker".into(),
            parent_session_id: "parent".into(),
            child_session_id: None,
            child_session_path: None,
            description: "Worker".into(),
            subagent_type: "executor".into(),
            capability_mode: "execute".into(),
            isolation: "none".into(),
            background: true,
            status: "running".into(),
            activity: "Thinking".into(),
            started_at_ms: 1,
            completed_at_ms: None,
            duration_ms: 10,
            output: None,
            error: None,
            failure_kind: None,
            model: None,
            thinking_level: None,
            worktree_path: None,
            cwd: None,
            workflow_run_id: None,
        };
        state.apply(AgentEvent::SubagentUpdate {
            task: Box::new(task.clone()),
        });
        assert!(state.expanded_tool_groups.contains(&0));
        assert!(state.expanded_tool_groups.remove(&2));

        task.activity = "Reading file-0.rs".into();
        task.duration_ms = 20;
        state.apply(AgentEvent::SubagentUpdate {
            task: Box::new(task),
        });

        assert!(state.expanded_tool_groups.contains(&0));
        assert!(!state.expanded_tool_groups.contains(&2));
    }

    #[test]
    fn orchestration_cohort_survives_reasoning_and_internal_polling() {
        let make_task =
            |id: &str, description: &str, started_at_ms: u64| pi_harness::SubagentTask {
                task_id: id.into(),
                parent_session_id: "parent".into(),
                child_session_id: Some(format!("child-{id}")),
                child_session_path: None,
                description: description.into(),
                subagent_type: "executor".into(),
                capability_mode: "execute".into(),
                isolation: "none".into(),
                background: true,
                status: "running".into(),
                activity: "read C:/Users/nier/Documents/orces/torii/docs/workflows.md".into(),
                started_at_ms,
                completed_at_ms: None,
                duration_ms: 20,
                output: None,
                error: None,
                failure_kind: None,
                model: Some("openai/gpt-test".into()),
                thinking_level: Some("high".into()),
                worktree_path: None,
                cwd: None,
                workflow_run_id: None,
            };
        let mut state = super::AppState::default();
        state.cwd = "C:/Users/nier/Documents/orces/torii".into();
        state.apply(AgentEvent::UserMessage {
            text: "Orchestrate. Objective: Analyze this codebase".into(),
        });
        state.apply(AgentEvent::ReasoningDelta {
            text: "planning delegation".into(),
        });
        state.apply(AgentEvent::SubagentUpdate {
            task: Box::new(make_task("planner", "Architecture planner", 1)),
        });
        state.apply(AgentEvent::ReasoningDelta {
            text: "delegating execution".into(),
        });
        state.apply(AgentEvent::ToolCallStart {
            id: "poll".into(),
            name: "Get_command_or_subagent_output".into(),
            args: serde_json::json!({"task_id":"executor"}),
        });
        state.apply(AgentEvent::SubagentUpdate {
            task: Box::new(make_task("executor", "Analysis executor", 2)),
        });

        let agent_indices: Vec<_> = state
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                matches!(entry, super::state::Entry::Tool { id, .. } if id.starts_with("subagent:"))
                    .then_some(index)
            })
            .collect();
        assert_eq!(agent_indices.len(), 2);
        assert_eq!(agent_indices[1], agent_indices[0] + 1);
        assert_eq!(
            state
                .entries
                .iter()
                .filter(|entry| matches!(entry, super::state::Entry::Reasoning { .. }))
                .count(),
            1
        );
        assert!(
            !state
                .entries
                .iter()
                .any(|entry| matches!(entry, super::state::Entry::Tool { id, .. } if id == "poll"))
        );

        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 120, 32);
        assert!(output.contains("Analyze this codebase"));
        assert!(output.contains("Architecture planner"));
        assert!(output.contains("Analysis executor"));
        assert!(
            output.contains("Reading docs/workflows.md"),
            "orchestration activity should be compact: {output}"
        );
        assert!(output.contains("openai/gpt-test · high · executor"));

        state.apply(AgentEvent::UserMessage {
            text: "Write into ISSUE.md".into(),
        });
        state.apply(AgentEvent::ToolCallStart {
            id: "write-later".into(),
            name: "write".into(),
            args: serde_json::json!({"path":"ISSUE.md"}),
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let resumed = buffer_text(terminal.backend().buffer(), 120, 32);
        let orchestration_line = resumed
            .lines()
            .find(|line| line.contains("0/2 complete"))
            .expect("orchestration summary");
        assert!(orchestration_line.contains("Analyze this codebase"));
        assert!(!orchestration_line.contains("Write into ISSUE.md"));
        assert!(
            resumed.find("0/2 complete").unwrap() < resumed.rfind("Write into ISSUE.md").unwrap(),
            "restored orchestration must remain before a later write turn: {resumed}"
        );

        state.activate_entry_at(agent_indices[1]);
        assert_eq!(state.view, super::View::Subagent);
        assert_eq!(state.inspected_subagent.as_deref(), Some("executor"));

        for index in 2..20 {
            let task = make_task(
                &format!("worker-{index:02}"),
                &format!("Worker {index:02}"),
                index,
            );
            state.subagent_tasks.insert(task.task_id.clone(), task);
        }
        state.view = super::View::Tasks;
        state.task_selected = 19;
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let tasks = buffer_text(terminal.backend().buffer(), 120, 32);
        assert!(state.task_list_offset.get() > 0);
        assert!(tasks.contains("Architecture planner"));
    }

    #[test]
    fn workflow_dashboard_exposes_checkpoint_controls_and_artifacts() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::WorkflowUpdate {
            workflow: Box::new(pi_harness::WorkflowRunSnapshot {
                run_id: "wf-1".into(),
                name: "implement-review".into(),
                description: Some("Plan, implement, review".into()),
                status: "paused".into(),
                current_step: Some("approve-plan".into()),
                completed_steps: 1,
                total_steps: 3,
                artifact_ids: vec!["artifact-plan".into()],
                budget: Some(pi_harness::WorkflowBudgetSnapshot {
                    max_agent_attempts: Some(8),
                    max_prompt_tokens: Some(100_000),
                    max_output_tokens: Some(10_000),
                    max_cache_write_tokens: Some(20_000),
                    agent_attempts: 1,
                    prompt_tokens: 400,
                    output_tokens: 20,
                    cache_write_tokens: 0,
                    reserved_prompt_tokens: 0,
                    reserved_output_tokens: 0,
                    reserved_cache_write_tokens: 0,
                    unknown_usage_attempts: 0,
                }),
                provider_states: vec![pi_harness::WorkflowProviderStateSnapshot {
                    provider: "test".into(),
                    max_concurrency: Some(2),
                    max_starts: Some(10),
                    window_ms: Some(60_000),
                    failure_threshold: Some(3),
                    cooldown_ms: Some(30_000),
                    active_attempts: 0,
                    starts_in_window: 1,
                    consecutive_failures: 0,
                    circuit: "closed".into(),
                    retry_at_ms: None,
                    rate_retry_at_ms: None,
                }],
                steps: vec![
                    pi_harness::WorkflowStepSnapshot {
                        id: "plan".into(),
                        r#type: "agent".into(),
                        status: "completed".into(),
                        role: Some("planner".into()),
                        model: Some("test/planner".into()),
                        task_ids: vec!["task-1".into()],
                        artifact_ids: vec!["artifact-plan".into()],
                        error: None,
                        attempt_count: 1,
                        timeout_ms: Some(60_000),
                        max_attempts: None,
                        output_contract: None,
                        condition: None,
                        children: Vec::new(),
                        observability: Some(pi_harness::WorkflowAttemptObservability {
                            model: Some("test/planner".into()),
                            thinking: Some("high".into()),
                            capability: "read-only".into(),
                            session: "ephemeral".into(),
                            session_key: None,
                            root_input_bytes: 100,
                            prompt_bytes: 4_096,
                            artifact_count: 1,
                            artifact_bytes: 2_048,
                            truncated_artifact_count: 1,
                            requested_tools: vec!["read".into()],
                            active_tools: Some(vec!["read".into(), "search".into()]),
                            tool_schema_fingerprint: Some("schema123456789".into()),
                            cache_prefix_fingerprint: Some("prefix123456789".into()),
                            cache_prefix_changed: Some(true),
                            system_prompt_bytes: Some(8_192),
                            input_tokens: Some(100),
                            output_tokens: Some(20),
                            cache_read_tokens: Some(300),
                            cache_write_tokens: Some(0),
                            cache_hit_rate: Some(0.75),
                            policy_action: Some("warn".into()),
                            policy_violations: vec!["cache prefix changed during execution".into()],
                            provider_outcome: Some("success".into()),
                            provider_failure_kind: None,
                        }),
                    },
                    pi_harness::WorkflowStepSnapshot {
                        id: "approve-plan".into(),
                        r#type: "checkpoint".into(),
                        status: "waiting".into(),
                        role: None,
                        model: None,
                        task_ids: Vec::new(),
                        artifact_ids: Vec::new(),
                        error: None,
                        attempt_count: 0,
                        timeout_ms: None,
                        max_attempts: None,
                        output_contract: None,
                        condition: None,
                        children: Vec::new(),
                        observability: None,
                    },
                    pi_harness::WorkflowStepSnapshot {
                        id: "repair".into(),
                        r#type: "agent".into(),
                        status: "skipped".into(),
                        role: Some("executor".into()),
                        model: Some("test/executor".into()),
                        task_ids: Vec::new(),
                        artifact_ids: Vec::new(),
                        error: Some("condition not met".into()),
                        attempt_count: 0,
                        timeout_ms: Some(60_000),
                        max_attempts: None,
                        output_contract: None,
                        condition: Some("review.verdict needs_changes (any)".into()),
                        children: Vec::new(),
                        observability: None,
                    },
                ],
                created_at_ms: 1,
                updated_at_ms: 2,
                error: None,
            }),
        });
        state.view = super::View::Workflows;
        assert_eq!(
            state.selected_workflow_control("approve"),
            Some(("wf-1".into(), Some("approve-plan".into())))
        );
        assert_eq!(
            state.selected_workflow_artifact(),
            Some(("wf-1".into(), "artifact-plan".into()))
        );

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 120, 30);
        assert!(output.contains("implement-review"));
        assert!(output.contains("approve-plan"));
        assert!(output.contains("repair  skipped"));
        assert!(output.contains("cache hit 75%"));
        assert!(output.contains("prefix1234 (changed)"));
        assert!(output.contains("guardrail warn"));
        assert!(output.contains("attempts 1/8"));
        assert!(output.contains("prompt 400+0 reserved/100000"));
        assert!(output.contains("Provider test"));
        assert!(output.contains("circuit closed"));
        assert!(output.contains("a approve"));

        state.apply(AgentEvent::WorkflowArtifact {
            artifact: Box::new(pi_harness::WorkflowArtifactSnapshot {
                run_id: "wf-1".into(),
                artifact_id: "artifact-plan".into(),
                step_id: "plan".into(),
                summary: "implementation plan".into(),
                producer_role: "planner".into(),
                producer_model: Some("test/planner".into()),
                content: "Evidence retained outside parent context".into(),
                truncated: false,
            }),
        });
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 120, 30);
        assert!(output.contains("artifact-plan"));
        assert!(output.contains("Evidence retained outside parent context"));
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
    fn compact_pi_edit_diff_uses_source_numbers_without_a_fake_index() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "edit-numbered".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/lib.rs",
                "edits": [{"oldText": "old", "newText": "new"}]
            }),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "edit-numbered".into(),
            result: pi_harness::ToolResult {
                content: "Edited src/lib.rs".into(),
                details: Some(serde_json::json!({
                    "diff": "      ...\n 1224   let listed = sessions();\n-1225   return listed;\n+1225   return filtered;\n      ..."
                })),
            },
            is_error: false,
            duration_ms: Some(10),
        });

        match &state.entries[0] {
            super::state::Entry::Diff { lines, .. } => {
                assert_eq!(lines[0].number, None);
                assert_eq!(lines[0].text, "...");
                assert_eq!(lines[1].number, Some(1224));
                assert_eq!(lines[1].text, "  let listed = sessions();");
                assert_eq!(lines[2].number, Some(1225));
                assert!(matches!(lines[2].kind, super::state::DiffKind::Removed));
                assert_eq!(lines[3].number, Some(1225));
                assert!(matches!(lines[3].kind, super::state::DiffKind::Added));
            }
            other => panic!("expected diff entry, got {other:?}"),
        }
    }

    #[test]
    fn padded_pi_edit_diff_does_not_invent_zero_based_display_numbers() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "edit-padded".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/state.rs",
                "edits": [{"oldText": "old", "newText": "new"}]
            }),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "edit-padded".into(),
            result: pi_harness::ToolResult {
                content: "Edited src/state.rs".into(),
                details: Some(serde_json::json!({
                    "diff": "      ...\n  602         self.history_index = None;\n  603         true\n+ 606     pub fn move_slash_selection(&mut self) {\n+ 607         if !self.prompt.starts_with('/') {\n      ..."
                })),
            },
            is_error: false,
            duration_ms: Some(10),
        });

        match &state.entries[0] {
            super::state::Entry::Diff { lines, .. } => {
                let numbers = lines
                    .iter()
                    .filter_map(|line| line.number)
                    .collect::<Vec<_>>();
                assert_eq!(numbers, vec![602, 603, 606, 607]);
                assert!(!numbers.contains(&0));
                assert!(matches!(lines[3].kind, super::state::DiffKind::Added));
            }
            other => panic!("expected diff entry, got {other:?}"),
        }
    }

    #[test]
    fn mixed_pi_edit_diff_preserves_indented_unnumbered_replacements() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "edit-mixed-numbering".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/pi-adapter.ts",
                "edits": [{"oldText": "old", "newText": "new"}]
            }),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "edit-mixed-numbering".into(),
            result: pi_harness::ToolResult {
                content: "Edited src/pi-adapter.ts".into(),
                details: Some(serde_json::json!({
                    "diff": "  1578     await active.session.compact(instructions);\n-     // Pi's compactor uses the model currently held by AgentSession and does not\n-     // expose a per-compaction model override. Change only the in-memory agent\n+     // Compaction uses the active model.\n  1581     export function setApiKey(active: ActiveSession, provider: string, key: string): void {"
                })),
            },
            is_error: false,
            duration_ms: Some(10),
        });

        match &state.entries[0] {
            super::state::Entry::Diff { lines, .. } => {
                assert_eq!(lines.len(), 5);
                assert_eq!(
                    lines[1].text,
                    "     // Pi's compactor uses the model currently held by AgentSession and does not"
                );
                assert_eq!(
                    lines[2].text,
                    "     // expose a per-compaction model override. Change only the in-memory agent"
                );
                assert_eq!(lines[3].text, "     // Compaction uses the active model.");
                assert!(matches!(lines[1].kind, super::state::DiffKind::Removed));
                assert!(matches!(lines[3].kind, super::state::DiffKind::Added));
            }
            other => panic!("expected diff entry, got {other:?}"),
        }
    }

    #[test]
    fn ellipsis_does_not_hide_unnumbered_replacement_lines() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "edit-ellipsis".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/lib.rs",
                "edits": [{"oldText": "old", "newText": "new"}]
            }),
        });
        state.apply(AgentEvent::ToolCallResult {
            id: "edit-ellipsis".into(),
            result: pi_harness::ToolResult {
                content: "Edited src/lib.rs".into(),
                details: Some(serde_json::json!({
                    "diff": "      ...\n-old value\n+new value\n      ..."
                })),
            },
            is_error: false,
            duration_ms: Some(10),
        });

        match &state.entries[0] {
            super::state::Entry::Diff { lines, .. } => {
                assert_eq!(lines.len(), 2);
                assert_eq!(lines[0].text, "old value");
                assert!(matches!(lines[0].kind, super::state::DiffKind::Removed));
                assert_eq!(lines[1].text, "new value");
                assert!(matches!(lines[1].kind, super::state::DiffKind::Added));
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
                // The preview is a real line-level diff: only the inserted line
                // is Added, the unchanged surrounding lines stay Context.
                assert_eq!(lines.len(), 4);
                assert_eq!(lines[0].text, "fn handle(req: Request) -> Response {");
                assert!(matches!(lines[0].kind, super::state::DiffKind::Context));
                assert_eq!(lines[1].text, "    validate(&req)?;");
                assert!(matches!(lines[1].kind, super::state::DiffKind::Added));
                assert_eq!(lines[2].text, "    process(req)");
                assert!(matches!(lines[2].kind, super::state::DiffKind::Context));
                assert_eq!(lines[3].text, "}");
                assert!(matches!(lines[3].kind, super::state::DiffKind::Context));
            }
            other => panic!("expected Diff entry, got {other:?}"),
        }
    }

    #[test]
    fn expanded_edit_diff_still_shows_the_change_count_in_the_header() {
        let (width, height) = (80, 12);
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "count".into(),
            name: "edit".into(),
            args: serde_json::json!({
                "path": "src/lib.rs",
                "old_text": "let border = theme.border;\n",
                "new_text": "let border = focused_border(state, theme);\n",
            }),
        });
        assert!(
            matches!(&state.entries[0], super::state::Entry::Diff { expanded, .. } if *expanded),
            "edit diffs render expanded by default"
        );

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let rendered = buffer_text(terminal.backend().buffer(), width, height);
        assert!(
            rendered.contains("+1 -1"),
            "expanded diff header must show the +/- count, got:\n{rendered}"
        );
    }

    #[test]
    fn command_palette_filters_and_opens_selected_action() {
        let mut state = fixtures::markdown();
        state.prompt = "draft stays here".into();
        state.open_overlay(super::OverlayKind::CommandPalette);
        for character in "sett".chars() {
            state.insert_overlay_char(character);
        }

        assert_eq!(
            state.overlay_items(),
            vec![
                "Settings  ·  F2  ·  Open Torii settings",
                "/settings  ·  Open settings"
            ]
        );
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::Settings);
        assert_eq!(state.prompt, "draft stays here");

        state.open_overlay(super::OverlayKind::CommandPalette);
        state.overlay_query = "settings".into();
        state.overlay_selected = 1;
        state.activate_overlay();
        assert_eq!(state.prompt, "/settings ");
        assert_eq!(state.overlay, super::OverlayKind::None);
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
    fn permission_request_renders_inline_without_covering_scrollback() {
        let state = fixtures::Story::Permission.state();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), 100, 30);
        assert!(output.contains("Permission required"));
        assert!(output.contains("Run cargo test --workspace"));
        assert!(output.contains("Allow once"));
        assert!(output.contains("Run the checks and show me what changed."));
    }

    #[test]
    fn slash_completion_works_at_cursor_inside_a_prompt_and_valid_tokens_are_colored() {
        let mut state = super::AppState {
            prompt: "please /mo then continue".into(),
            cursor: 10,
            ..super::AppState::default()
        };
        assert!(state.complete_slash_command());
        assert_eq!(state.prompt, "please /model then continue");
        assert_eq!(state.cursor, 13);
        assert!(state.is_valid_slash_command("/model"));

        let mut executable = super::AppState {
            prompt: "something /model".into(),
            cursor: 16,
            ..super::AppState::default()
        };
        assert!(matches!(
            executable.activate_slash_command(),
            Some(super::state::OverlayAction::None)
        ));
        assert_eq!(executable.overlay, super::OverlayKind::ModelPicker);
        assert!(executable.prompt.is_empty());

        assert!(state.is_valid_slash_command("/model"));
        assert!(!state.is_valid_slash_command("/modeling"));

        let (width, height) = (100, 24);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("please /model then continue"));
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

        let mut many = super::AppState {
            prompt: "/".into(),
            cursor: 1,
            runtime_commands: (0..20)
                .map(|index| pi_harness::RuntimeCommand {
                    name: format!("/command-{index:02}"),
                    description: format!("Runtime command {index}"),
                    source: "test".into(),
                })
                .collect(),
            ..super::AppState::default()
        };
        many.overlay_selected = many
            .slash_suggestions()
            .iter()
            .position(|(command, _)| command == "/command-15")
            .unwrap();
        terminal.draw(|frame| ui::render(frame, &many)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("/command-15"));
        assert!(!output.contains("/command-00"));
    }

    #[test]
    fn model_picker_returns_the_backend_model_id() {
        let mut state = super::AppState {
            available_models: vec![
                pi_harness::ModelInfo {
                    id: "opencode-go/glm-5.2".into(),
                    display_name: "GLM-5.2".into(),
                    context_window: None,
                },
                pi_harness::ModelInfo {
                    id: "opencode-go/minimax-m3".into(),
                    display_name: "MiniMax-M3".into(),
                    context_window: None,
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
                    context_window: None,
                },
                pi_harness::ModelInfo {
                    id: "opencode-go/minimax-m3".into(),
                    display_name: "MiniMax-M3".into(),
                    context_window: None,
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::ModelPicker);

        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        let current = output
            .lines()
            .find(|line| line.contains("MiniMax-M3"))
            .expect("current model row");
        let other = output
            .lines()
            .find(|line| line.contains("GLM-5.2"))
            .expect("other model row");
        assert!(current.contains("✓ current"));
        assert!(!other.contains("✓ current"));
    }

    #[test]
    fn long_model_picker_follows_selection_and_hover_uses_visible_rows() {
        let (width, height) = (100, 24);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState {
            available_models: (0..30)
                .map(|index| pi_harness::ModelInfo {
                    id: format!("provider-{}/model-{index:02}", index / 10),
                    display_name: format!("Model {index:02}"),
                    context_window: None,
                })
                .collect(),
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::ModelPicker);
        state.overlay_selected = 25;

        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(
            output.contains("Model 25"),
            "selected model must stay visible: {output}"
        );
        assert!(
            !output.contains("Model 00"),
            "viewport should follow selection: {output}"
        );
        let selected_row = output
            .lines()
            .position(|line| line.contains("Model 25"))
            .unwrap() as u16;
        assert_eq!(
            overlay::item_at_position(&state, width, height, width / 2, selected_row),
            Some(25)
        );
        assert_eq!(
            overlay::item_at_position(&state, width, height, 0, selected_row),
            None,
            "hover outside the modal must not select a row"
        );
    }

    #[test]
    fn subagent_model_picker_filters_by_provider_and_groups_provider_rows() {
        let (width, height) = (100, 24);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState {
            available_models: vec![
                pi_harness::ModelInfo {
                    id: "zeta/model-b".into(),
                    display_name: "Shared B".into(),
                    context_window: None,
                },
                pi_harness::ModelInfo {
                    id: "alpha/model-a".into(),
                    display_name: "Shared A".into(),
                    context_window: None,
                },
                pi_harness::ModelInfo {
                    id: "alpha/model-c".into(),
                    display_name: "Shared C".into(),
                    context_window: None,
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::SubagentModelPicker);
        for character in "alpha".chars() {
            state.insert_overlay_char(character);
        }

        assert_eq!(state.overlay_items(), vec!["Shared A", "Shared C"]);
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        let a = output.find("Shared A  alpha").unwrap();
        let c = output.find("Shared C  alpha").unwrap();
        assert!(
            a < c,
            "models should be grouped and sorted by provider: {output}"
        );
        assert!(!output.contains("Shared B"));
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
                    modified_at_ms: 1_752_220_600_000,
                    message_count: 12,
                    current: true,
                    cwd: "/work/pi-shell".into(),
                    parent_session_path: None,
                },
                pi_harness::SessionInfo {
                    id: "two".into(),
                    path: "/sessions/two.jsonl".into(),
                    name: None,
                    first_message: "Fix the model picker".into(),
                    modified_at_ms: 1_752_215_200_000,
                    message_count: 4,
                    current: false,
                    cwd: "/work/pi-shell".into(),
                    parent_session_path: Some("/sessions/one.jsonl".into()),
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
        assert!(output.contains("Ctrl+S sort"));

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
        assert_eq!(state.view, super::View::Transcript);
    }

    #[test]
    fn session_picker_supports_pi_management_controls() {
        let mut state = super::AppState {
            available_sessions: vec![
                pi_harness::SessionInfo {
                    id: "one".into(),
                    path: "/sessions/one.jsonl".into(),
                    name: Some("Named session".into()),
                    first_message: "first".into(),
                    modified_at_ms: 1_752_220_600_000,
                    message_count: 12,
                    current: true,
                    cwd: "/work".into(),
                    parent_session_path: None,
                },
                pi_harness::SessionInfo {
                    id: "two".into(),
                    path: "/sessions/two.jsonl".into(),
                    name: None,
                    first_message: "second".into(),
                    modified_at_ms: 1_752_215_200_000,
                    message_count: 4,
                    current: false,
                    cwd: "/work".into(),
                    parent_session_path: Some("/sessions/one.jsonl".into()),
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::SessionPicker);

        state.toggle_session_paths();
        assert!(state.overlay_items()[0].contains("/sessions/one.jsonl"));
        state.cycle_session_sort();
        assert_eq!(state.session_sort.label(), "Recent");
        state.toggle_named_sessions();
        assert_eq!(state.overlay_items().len(), 1);
        state.toggle_named_sessions();

        state.overlay_selected = 1;
        state.begin_session_rename();
        assert_eq!(state.overlay, super::OverlayKind::SessionRename);
        state.overlay_query = "Renamed child".into();
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::RenameSession { target, name }
                if target == "/sessions/two.jsonl" && name == "Renamed child"
        ));

        state.overlay_selected = 1;
        state.begin_session_delete();
        assert_eq!(state.overlay, super::OverlayKind::SessionDeleteConfirm);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::DeleteSession { target }
                if target == "/sessions/two.jsonl"
        ));

        state.overlay_selected = 0;
        state.begin_session_delete();
        assert_eq!(state.overlay, super::OverlayKind::SessionPicker);
        assert!(state.status.contains("active session"));
    }

    #[test]
    fn dashboard_delete_requires_confirmation_and_refuses_active_session() {
        let mut state = super::AppState {
            view: super::View::Dashboard,
            available_sessions: vec![
                pi_harness::SessionInfo {
                    id: "active".into(),
                    path: "/sessions/active.jsonl".into(),
                    name: Some("Active".into()),
                    first_message: "active".into(),
                    modified_at_ms: 0,
                    message_count: 1,
                    current: true,
                    cwd: "/work".into(),
                    parent_session_path: None,
                },
                pi_harness::SessionInfo {
                    id: "old".into(),
                    path: "/sessions/old.jsonl".into(),
                    name: Some("Old".into()),
                    first_message: "old".into(),
                    modified_at_ms: 0,
                    message_count: 1,
                    current: false,
                    cwd: "/work".into(),
                    parent_session_path: None,
                },
            ],
            ..super::AppState::default()
        };

        state.begin_dashboard_delete();
        assert_eq!(state.overlay, super::OverlayKind::None);
        assert!(state.status.contains("active session"));

        state.dashboard_selected = 1;
        state.begin_dashboard_delete();
        assert_eq!(state.overlay, super::OverlayKind::SessionDeleteConfirm);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::DeleteSession { target }
                if target == "/sessions/old.jsonl"
        ));
        assert_eq!(state.overlay, super::OverlayKind::None);
        assert_eq!(state.view, super::View::Dashboard);
    }

    #[test]
    fn session_reset_replaces_the_visible_transcript() {
        let mut state = fixtures::conversation();
        state.view = super::View::Dashboard;
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
        assert_eq!(state.view, super::View::Transcript);
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
            label_timestamp: Some("2026-07-11T01:00:00Z".into()),
            depth: 0,
            active: false,
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
            super::state::OverlayAction::NavigateTree { entry_id, summarize: true, .. }
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
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::TreeSummaryPicker);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::NavigateTree { entry_id, summarize: false, instructions: None }
                if entry_id == "entry-1"
        ));

        state.apply(AgentEvent::SessionTree {
            entries: entries.clone(),
            user_only: false,
        });
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        state.overlay_selected = 2;
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::TreeSummaryEditor);
        state.overlay_query = "preserve the benchmark results".into();
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::NavigateTree {
                entry_id,
                summarize: true,
                instructions: Some(instructions),
            } if entry_id == "entry-1" && instructions == "preserve the benchmark results"
        ));

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
    fn tree_picker_renders_pi_topology_active_path_and_label_time() {
        let entries = tree_picker_entries();
        let mut state = super::AppState::default();
        state.apply(AgentEvent::SessionTree {
            entries,
            user_only: false,
        });
        state.toggle_tree_timestamps();

        let (width, height) = (120, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Session Tree"));
        assert!(output.contains("Ctrl+←/→ fold/jump"));
        assert!(
            output.contains("├─ • [checkpoint] 07-11 01:00 user: active approach"),
            "{output}"
        );
        assert!(output.contains("└─ user: abandoned approach"));
        assert!(
            output
                .lines()
                .any(|line| { line.contains('›') && line.contains("active approach") })
        );
        let selected_row = output
            .lines()
            .position(|line| line.contains('›') && line.contains("active approach"))
            .unwrap() as u16;
        assert_eq!(
            overlay::item_at(&state, width, height, selected_row),
            Some(2)
        );
    }

    #[test]
    fn folded_tree_hides_all_descendants_without_hiding_the_fold_point() {
        let mut state = super::AppState::default();
        state.apply(AgentEvent::SessionTree {
            entries: tree_picker_entries(),
            user_only: false,
        });
        state.tree_folded.insert("answer".into());

        let visible = state
            .filtered_tree()
            .into_iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(visible, ["root", "answer"]);
    }

    #[test]
    fn fork_picker_is_a_dedicated_latest_user_message_selector() {
        let mut state = super::AppState::default();
        let entries = tree_picker_entries()
            .into_iter()
            .filter(|entry| entry.role.as_deref() == Some("user"))
            .collect();
        state.apply(AgentEvent::SessionTree {
            entries,
            user_only: true,
        });

        let (width, height) = (120, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Fork from Message"));
        assert!(output.contains("Message 3 of 3"));
        assert!(!output.contains("Filter:"));
        assert!(
            output
                .lines()
                .any(|line| { line.contains('›') && line.contains("abandoned approach") })
        );
        let selected_row = output
            .lines()
            .position(|line| line.contains('›') && line.contains("abandoned approach"))
            .unwrap() as u16;
        assert_eq!(
            overlay::item_at(&state, width, height, selected_row),
            Some(2)
        );
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::ForkSession { entry_id } if entry_id == "branch-b"
        ));
    }

    #[test]
    fn long_linear_tree_navigation_stays_interactive_without_folds() {
        let entries = (0..10_000)
            .map(|index| pi_harness::SessionTreeEntry {
                id: format!("entry-{index}"),
                parent_id: (index > 0).then(|| format!("entry-{}", index - 1)),
                kind: "message".into(),
                role: Some(if index % 2 == 0 { "user" } else { "assistant" }.into()),
                text: format!("message {index}"),
                timestamp: "2026-07-11T00:00:00Z".into(),
                label: None,
                label_timestamp: None,
                depth: index,
                active: true,
            })
            .collect();
        let mut state = super::AppState::default();
        state.apply(AgentEvent::SessionTree {
            entries,
            user_only: false,
        });

        let started = std::time::Instant::now();
        for _ in 0..20 {
            state.move_overlay_selection(-1);
        }
        let elapsed = started.elapsed();

        assert_eq!(state.overlay_selected, 9_979);
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "20 navigation steps took {elapsed:?}"
        );
    }

    fn tree_picker_entries() -> Vec<pi_harness::SessionTreeEntry> {
        vec![
            pi_harness::SessionTreeEntry {
                id: "root".into(),
                parent_id: None,
                kind: "message".into(),
                role: Some("user".into()),
                text: "start here".into(),
                timestamp: "2026-07-11T00:00:00Z".into(),
                label: None,
                label_timestamp: None,
                depth: 0,
                active: true,
            },
            pi_harness::SessionTreeEntry {
                id: "answer".into(),
                parent_id: Some("root".into()),
                kind: "message".into(),
                role: Some("assistant".into()),
                text: "choose an approach".into(),
                timestamp: "2026-07-11T00:01:00Z".into(),
                label: None,
                label_timestamp: None,
                depth: 1,
                active: true,
            },
            pi_harness::SessionTreeEntry {
                id: "branch-a".into(),
                parent_id: Some("answer".into()),
                kind: "message".into(),
                role: Some("user".into()),
                text: "active approach".into(),
                timestamp: "2026-07-11T00:02:00Z".into(),
                label: Some("checkpoint".into()),
                label_timestamp: Some("2026-07-11T01:00:00Z".into()),
                depth: 2,
                active: true,
            },
            pi_harness::SessionTreeEntry {
                id: "branch-b".into(),
                parent_id: Some("answer".into()),
                kind: "message".into(),
                role: Some("user".into()),
                text: "abandoned approach".into(),
                timestamp: "2026-07-11T00:03:00Z".into(),
                label: None,
                label_timestamp: None,
                depth: 2,
                active: false,
            },
        ]
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

        state.prompt = "/pa".into();
        state.cursor = state.prompt.chars().count();
        let paste = state.slash_suggestions();
        assert!(paste.iter().any(|(name, description)| {
            name == "/paste" && description.contains("text or attach an image")
        }));
        assert!(!paste.iter().any(|(name, _)| name == "/paste-image"));

        state.prompt = "/dashboard".into();
        state.cursor = state.prompt.chars().count();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::RefreshSessions)
        ));
        assert_eq!(state.view, super::View::Dashboard);

        state.prompt = "/workflow implement-review Fix the resume bug".into();
        state.cursor = state.prompt.chars().count();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::StartWorkflow { workflow, input, .. })
                if workflow == "implement-review" && input == "Fix the resume bug"
        ));

        state.prompt =
            "/workflow review --params {\"target\":\"src\",\"depth\":2} -- Inspect the patch"
                .into();
        state.cursor = state.prompt.chars().count();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::None)
        ));

        state.prompt = "/workflow".into();
        state.cursor = state.prompt.chars().count();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::LoadWorkflowCatalog)
        ));
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

        for (command, usage) in [
            ("/name", "Usage: /name <session name>"),
            ("/import", "Usage: /import <session.jsonl>"),
        ] {
            state.prompt = command.into();
            assert!(matches!(
                state.activate_slash_command(),
                Some(super::state::OverlayAction::None)
            ));
            assert!(state.prompt.is_empty());
            assert!(matches!(
                state.entries.last(),
                Some(super::state::Entry::Assistant { lines, .. }) if lines == &vec![usage.to_string()]
            ));
        }

        state.available_auth_providers = vec![
            pi_harness::ModelInfo {
                id: "openai".into(),
                display_name: "OpenAI".into(),
                context_window: None,
            },
            pi_harness::ModelInfo {
                id: "anthropic".into(),
                display_name: "Anthropic".into(),
                context_window: None,
            },
        ];
        state.prompt = "/login".into();
        assert!(state.activate_slash_command().is_some());
        assert_eq!(state.overlay, super::OverlayKind::LoginProvider);
        for character in "anth".chars() {
            state.insert_overlay_char(character);
        }
        assert_eq!(state.overlay_items(), vec!["Anthropic"]);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::BeginOauth(provider) if provider == "anthropic"
        ));
    }

    #[cfg(any())]
    #[test]
    fn workflow_catalog_opens_resolved_preflight_and_keeps_invalid_entries_visible() {
        let workflows = vec![
            pi_harness::WorkflowCatalogEntry {
                name: "review".into(),
                description: Some("Review changes".into()),
                source: "builtin".into(),
                valid: true,
                error: None,
            },
            pi_harness::WorkflowCatalogEntry {
                name: "broken".into(),
                description: None,
                source: "global".into(),
                valid: false,
                error: Some("steps must be an array".into()),
            },
        ];
        let mut state = super::AppState::default();
        state.prompt = "/workflow check review".into();
        state.cursor = state.prompt.chars().count();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::PreviewWorkflow { workflow }) if workflow == "review"
        ));
        state.apply(pi_harness::AgentEvent::WorkflowCatalog {
            workflows: workflows.clone(),
        });
        assert_eq!(state.overlay, super::OverlayKind::WorkflowPicker);
        assert!(state.overlay_items()[1].contains("[invalid]"));

        state.overlay_query = "review".into();
        assert_eq!(state.overlay_items().len(), 1);
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::PreviewWorkflow { workflow } if workflow == "review"
        ));
        assert_eq!(state.overlay, super::OverlayKind::None);

        let preview = pi_harness::WorkflowPreview {
            name: "review".into(),
            version: Some(serde_json::json!(1)),
            description: Some("Review changes".into()),
            definition_hash: "abc123def456".into(),
            resolved_at_ms: 10,
            budget: Some(pi_harness::WorkflowBudgetSnapshot {
                max_agent_attempts: Some(5),
                max_prompt_tokens: Some(100_000),
                max_output_tokens: Some(10_000),
                max_cache_write_tokens: Some(20_000),
                agent_attempts: 0,
                prompt_tokens: 0,
                output_tokens: 0,
                cache_write_tokens: 0,
                reserved_prompt_tokens: 0,
                reserved_output_tokens: 0,
                reserved_cache_write_tokens: 0,
                unknown_usage_attempts: 0,
            }),
            provider_policies: vec![pi_harness::WorkflowProviderPolicySnapshot {
                provider: "openai".into(),
                max_concurrency: Some(2),
                max_starts: Some(10),
                window_ms: Some(60_000),
                failure_threshold: Some(3),
                cooldown_ms: Some(30_000),
            }],
            steps: vec![pi_harness::WorkflowPreviewStep {
                id: "inspect".into(),
                r#type: "agent".into(),
                description: None,
                role: Some("reviewer".into()),
                agent: Some("review".into()),
                model: Some("openai/gpt-5".into()),
                model_route: Some("reviewer".into()),
                model_candidates: Some(vec!["openai/gpt-5".into(), "fallback/model".into()]),
                thinking: Some("high".into()),
                capability: Some("read-only".into()),
                isolation: Some("none".into()),
                session: Some("ephemeral".into()),
                session_key: None,
                tools: vec!["read".into(), "search".into()],
                forced_read_only: true,
                reports: Some("previous".into()),
                timeout_ms: Some(1_200_000),
                max_attempts: Some(2),
                retry_backoff_ms: Some(1_000),
                retry_on: vec!["failed".into(), "timeout".into()],
                output_contract: Some("review_verdict".into()),
                condition: None,
                guardrails: Some(pi_harness::WorkflowGuardrailsPreview {
                    max_prompt_bytes: Some(65_536),
                    max_artifact_bytes: Some(49_152),
                    max_artifacts: Some(4),
                    max_prompt_tokens: Some(20_000),
                    max_output_tokens: Some(2_000),
                    max_cache_write_tokens: Some(5_000),
                    min_cache_hit_rate: Some(0.5),
                    allowed_models: Some(vec!["openai/gpt-5".into()]),
                    allowed_tools: Some(vec!["read".into(), "search".into()]),
                    require_stable_cache_prefix: true,
                    on_violation: "fail".into(),
                }),
                external_effects: Some(pi_harness::WorkflowExternalEffectsPreview {
                    approved_by: "approve-review".into(),
                }),
                source: Some("shared-review:inspect via audit".into()),
                parameter_scope: Some("audit".into()),
                parameter_keys: vec!["target".into()],
                children: vec![],
            }],
            contracts: vec![pi_harness::WorkflowContractPreview {
                name: "audit.plan".into(),
                description: Some("Bounded implementation plan".into()),
                max_bytes: 16_384,
                schema_hash: "schema1234567890".into(),
            }],
            parameters: Some(pi_harness::WorkflowParameterPreview {
                description: Some("Review scope".into()),
                max_bytes: 4096,
                schema_hash: "params123456789".into(),
                required: vec!["target".into()],
                defaults: serde_json::json!({"depth": 2}),
            }),
            components: vec![pi_harness::WorkflowComponentPreview {
                invocation: "audit".into(),
                workflow: "shared-review".into(),
                version: Some(serde_json::json!(1)),
                definition_hash: "component123456789".into(),
                parameter_binding_hash: Some("bindings123456789".into()),
                parameter_bindings: std::collections::BTreeMap::from([(
                    "target".into(),
                    "root:[\"target\"]".into(),
                )]),
            }],
            readiness: pi_harness::WorkflowReadiness {
                status: "warning".into(),
                issues: vec![pi_harness::WorkflowReadinessIssue {
                    severity: "warning".into(),
                    code: "model_route_fallback".into(),
                    message: "route reviewer selected fallback openai/gpt-5".into(),
                    step_id: Some("inspect".into()),
                }],
            },
        };
        state.apply(pi_harness::AgentEvent::WorkflowPreview {
            preview: Box::new(preview),
        });
        assert_eq!(state.overlay, super::OverlayKind::WorkflowPreview);
        let preview_lines = state.overlay_items();
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("openai/gpt-5"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("capability=read-only (forced)"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("retry=failed/timeout"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("guardrails=fail"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("Workflow budget: attempts<=5"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("Provider openai: concurrency<=2"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("source=shared-review:inspect via audit"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("parameters=audit [target]"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("Contract audit.plan"))
        );
        assert!(preview_lines.iter().any(|line| {
            line.contains("Parameters: max 4096B")
                && line.contains("required target")
                && line.contains("\"depth\":2")
        }));
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("Component audit: shared-review@1")
                    && line.contains("bindings bindings123"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("parameter map: target=root:[\"target\"]"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("allow models=openai/gpt-5"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("Readiness: warning"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("route=reviewer"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line.contains("model_route_fallback"))
        );
        assert!(
            preview_lines
                .iter()
                .any(|line| line
                    .contains("external effects · approved by checkpoint approve-review"))
        );
        let mut blocked_preview = state.workflow_preview.clone().unwrap();
        blocked_preview.readiness = pi_harness::WorkflowReadiness {
            status: "blocked".into(),
            issues: vec![pi_harness::WorkflowReadinessIssue {
                severity: "blocker".into(),
                code: "model_unavailable".into(),
                message: "model fallback/model is not available".into(),
                step_id: Some("inspect".into()),
            }],
        };
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.prompt, "/workflow review ");
        state.prompt.push_str("Inspect the patch");
        state.cursor = state.prompt.chars().count();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::StartWorkflow {
                workflow,
                input,
                parameters: _,
                expected_definition_hash: Some(hash),
            }) if workflow == "review" && input == "Inspect the patch" && hash == "abc123def456"
        ));

        state.apply(pi_harness::AgentEvent::WorkflowCatalog { workflows });
        state.overlay_selected = 1;
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::WorkflowPicker);
        assert!(state.status.contains("steps must be an array"));

        state.apply(pi_harness::AgentEvent::WorkflowPreview {
            preview: Box::new(blocked_preview),
        });
        let prompt_before = state.prompt.clone();
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::WorkflowPreview);
        assert_eq!(state.prompt, prompt_before);
        assert!(state.status.contains("readiness is blocked"));
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
        state.cursor = state.prompt.chars().count();
        assert!(state.activate_slash_command().is_some());
        assert!(
            matches!(state.entries.last(), Some(super::state::Entry::Assistant { lines, .. }) if lines.iter().any(|line| line.contains("AGENTS.md")))
        );

        state.prompt = "/reload".into();
        state.cursor = state.prompt.chars().count();
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
                    context_window: None,
                },
                pi_harness::ModelInfo {
                    id: "two/b".into(),
                    display_name: "Model B".into(),
                    context_window: None,
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::Settings);
        assert!(state.overlay_items()[0].contains("one-at-a-time"));
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::SetRuntimeSetting { key, value } if key == "steering_mode" && value == serde_json::json!("all"))
        );
        assert_eq!(state.overlay, super::OverlayKind::Settings);

        state.overlay_selected = 6;
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::ScopedModels);
        state.close_overlay();
        assert_eq!(state.overlay, super::OverlayKind::Settings);
        assert_eq!(state.overlay_selected, 6);

        state.open_overlay(super::OverlayKind::ScopedModels);
        state.toggle_scoped_model();
        assert_eq!(state.runtime_settings.enabled_models, vec!["one/a"]);
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::SetScopedModels(models) if models == vec!["one/a"])
        );

        state.open_overlay(super::OverlayKind::SubagentModelPicker);
        state.overlay_selected = 2;
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::SetRuntimeSetting { key, value } if key == "subagent_model" && value == serde_json::json!("two/b"))
        );
        assert_eq!(
            state.runtime_settings.subagent_model.as_deref(),
            Some("two/b")
        );

        state.open_overlay(super::OverlayKind::SubagentModelPicker);
        assert!(
            matches!(state.activate_overlay(), super::state::OverlayAction::SetRuntimeSetting { key, value } if key == "subagent_model" && value.is_null())
        );
        assert_eq!(state.runtime_settings.subagent_model, None);

        state.prompt = "/trust".into();
        assert!(matches!(
            state.activate_slash_command(),
            Some(super::state::OverlayAction::SetProjectTrust(true))
        ));
    }

    #[test]
    fn settings_switches_between_dark_and_light_palettes() {
        let (width, height) = (100, 30);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.open_overlay(super::OverlayKind::Settings);
        state.overlay_selected = 8;

        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::SetTheme(crate::theme::ThemeMode::Light)
        ));
        assert_eq!(state.theme_mode, crate::theme::ThemeMode::Light);
        assert_eq!(state.overlay, super::OverlayKind::Settings);
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, 0)].bg,
            Theme::GROK_LIGHT.background
        );
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("Theme"));
        assert!(output.contains("light"));
    }

    #[test]
    fn pi_extensions_menu_reports_and_toggles_resolved_sdk_extensions() {
        let mut state = super::AppState {
            runtime_extensions: vec![
                pi_harness::RuntimeExtension {
                    path: "/home/user/.pi/agent/extensions/review.ts".into(),
                    label: "review.ts".into(),
                    source: "local".into(),
                    scope: "user".into(),
                    enabled: true,
                    loaded: true,
                },
                pi_harness::RuntimeExtension {
                    path: "/project/.pi/extensions/legacy.ts".into(),
                    label: "legacy.ts".into(),
                    source: "local".into(),
                    scope: "project".into(),
                    enabled: false,
                    loaded: false,
                },
            ],
            ..super::AppState::default()
        };
        state.open_overlay(super::OverlayKind::Settings);
        state.overlay_selected = 5;
        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::None
        ));
        assert_eq!(state.overlay, super::OverlayKind::Extensions);
        assert_eq!(
            state.overlay_items(),
            vec!["[✓] review.ts", "[ ] legacy.ts"]
        );

        assert!(matches!(
            state.activate_overlay(),
            super::state::OverlayAction::SetExtensionEnabled { path, enabled }
                if path.ends_with("review.ts") && !enabled
        ));
        assert!(state.runtime_extensions[0].enabled);
    }

    #[test]
    fn header_exposes_path_and_context_hit_targets() {
        let (width, height) = (100, 30);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = super::AppState {
            cwd: "~/work/torii".into(),
            branch: "main".into(),
            context_used: 50_000,
            context_limit: 200_000,
            ..super::AppState::default()
        };
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let targets = state.header_targets.borrow();
        assert!(targets.iter().any(|(kind, ..)| *kind == 0));
        assert!(targets.iter().any(|(kind, ..)| *kind == 4));
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
        assert!(matches!(
            state.entries.last(),
            Some(super::state::Entry::Assistant { lines, .. })
                if lines.iter().any(|line| line.contains("Authentication successful"))
        ));
        state.apply(AgentEvent::ModelsChanged {
            models: vec![pi_harness::ModelInfo {
                id: "example/new-model".into(),
                display_name: "New Model".into(),
                context_window: Some(128_000),
            }],
        });
        assert_eq!(state.available_models[0].id, "example/new-model");
        state.apply(AgentEvent::ModelChanged {
            id: "example/new-model".into(),
            display_name: "New Model".into(),
        });
        assert_eq!(state.context_limit, 128_000);
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
        let (width, height) = (100, 24);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::AppState::default();
        state.apply(AgentEvent::ToolCallStart {
            id: "plan-tool".into(),
            name: "update_plan".into(),
            args: serde_json::json!({"entries": []}),
        });
        assert!(
            state.entries.is_empty(),
            "raw update_plan tool should stay hidden"
        );

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
        assert!(matches!(
            state.entries.as_slice(),
            [super::state::Entry::Plan { expanded: true, .. }]
        ));
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("+ Plan  1/2"));
        assert!(output.contains("✓ Inspect"));
        assert!(output.contains("> Implement"));
        assert!(output.contains("Plan 1/2"));

        state.apply(AgentEvent::PlanUpdate {
            entries: vec![
                pi_harness::PlanEntry {
                    step: "Inspect".into(),
                    status: "completed".into(),
                },
                pi_harness::PlanEntry {
                    step: "Implement".into(),
                    status: "completed".into(),
                },
            ],
        });
        assert!(matches!(
            state.entries.as_slice(),
            [super::state::Entry::Plan {
                expanded: false,
                ..
            }]
        ));

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

    #[test]
    fn dashboard_renders_torii_version_and_truthful_session_states() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let now_ms: u64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .try_into()
            .unwrap();
        let mut state = super::AppState {
            view: super::View::Dashboard,
            available_sessions: vec![
                pi_harness::SessionInfo {
                    id: "current".into(),
                    path: "/tmp/current.jsonl".into(),
                    name: Some("Active refactor".into()),
                    first_message: String::new(),
                    modified_at_ms: now_ms,
                    message_count: 4,
                    current: true,
                    cwd: String::new(),
                    parent_session_path: None,
                },
                pi_harness::SessionInfo {
                    id: "saved".into(),
                    path: "/tmp/saved.jsonl".into(),
                    name: None,
                    first_message: "Previous investigation".into(),
                    modified_at_ms: now_ms - 2 * 60 * 60 * 1_000,
                    message_count: 8,
                    current: false,
                    cwd: String::new(),
                    parent_session_path: None,
                },
            ],
            ..super::AppState::default()
        };
        state.runtime_sessions.insert(
            "/tmp/current.jsonl".into(),
            pi_harness::RuntimeSessionInfo {
                path: "/tmp/current.jsonl".into(),
                status: "running".into(),
                started_at_ms: Some(now_ms),
            },
        );
        state.streaming = true;
        state.turn_started_at = Some(std::time::Instant::now());
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);

        assert!(output.contains("Torii"));
        assert!(output.contains(env!("CARGO_PKG_VERSION")));
        assert!(output.contains("Running"));
        assert!(output.contains("Inactive"));
        assert!(output.contains("Active refactor"));
        assert!(output.contains("2h ago"));
        assert!(output.contains("s stop"));
        assert!(output.contains("x close"));
        assert!(!output.contains("d delete"));
        assert!(!output.contains("Messages total"));

        state.dashboard_selected = 1;
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("d delete"));
        assert!(!output.contains("s stop"));
        assert!(!output.contains("x close"));
    }

    #[test]
    fn session_age_uses_relative_units_without_leaking_wire_dates() {
        let now = 2_000_000_000;
        assert_eq!(super::state::format_session_age(now, now), "now");
        assert_eq!(
            super::state::format_session_age(now - 5 * 60_000, now),
            "5m ago"
        );
        assert_eq!(
            super::state::format_session_age(now - 3 * 3_600_000, now),
            "3h ago"
        );
        assert_eq!(
            super::state::format_session_age(now - 2 * 86_400_000, now),
            "2d ago"
        );
    }

    #[test]
    fn dashboard_renders_update_state_above_contextual_actions() {
        let (width, height) = (100, 32);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = super::AppState {
            view: super::View::Dashboard,
            app_update: Some(pi_harness::AppUpdateStatus::Available {
                version: "0.2.0".into(),
                size_bytes: 38 * 1024 * 1024,
            }),
            ..super::AppState::default()
        };
        terminal.draw(|frame| ui::render(frame, &state)).unwrap();
        let output = buffer_text(terminal.backend().buffer(), width, height);
        assert!(output.contains("Torii v0.2.0 available"));
        assert!(output.contains("38.0 MiB"));
        assert!(output.contains("u update"));
        assert!(output.contains("l later"));
        assert!(output.contains("Enter open"));
    }
}
