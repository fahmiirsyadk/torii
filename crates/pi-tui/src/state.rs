use pi_harness::{
    AgentEvent, ModelInfo, PermissionDecision, SessionInfo, SessionTreeEntry, ToolResult, Usage,
};
use std::{collections::HashSet, time::Instant};

const COMMANDS: &[&str] = &[
    "Resume session",
    "Model picker",
    "Settings",
    "Cycle mode",
    "Quit",
];
const SETTINGS: &[&str] = &["Appearance", "Permissions", "Input", "Terminal"];
const PERMISSION_OPTIONS: &[&str] = &["Allow once", "Always allow", "Deny"];
const SLASH_COMMANDS: &[&str] = &[
    "/model",
    "/resume",
    "/new",
    "/name",
    "/session",
    "/clone",
    "/tree",
    "/fork",
    "/thinking",
    "/mode",
    "/settings",
    "/clear",
    "/compact",
    "/help",
    "/quit",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayKind {
    None,
    CommandPalette,
    ModelPicker,
    SessionPicker,
    TreePicker,
    ForkPicker,
    LabelEditor,
    Settings,
    Permission,
}

#[derive(Clone, Debug)]
pub struct PendingPermission {
    pub id: String,
    pub tool: String,
    pub reason: String,
}

#[derive(Debug)]
pub enum OverlayAction {
    None,
    Quit,
    Permission {
        request_id: String,
        decision: PermissionDecision,
    },
    SetModel {
        id: String,
    },
    ResumeSession {
        target: String,
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
    },
    ForkSession {
        entry_id: String,
    },
    SetLabel {
        entry_id: String,
        label: Option<String>,
    },
    CycleThinking,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TreeFilter {
    #[default]
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

impl TreeFilter {
    pub fn next(self) -> Self {
        match self {
            Self::Default => Self::NoTools,
            Self::NoTools => Self::UserOnly,
            Self::UserOnly => Self::LabeledOnly,
            Self::LabeledOnly => Self::All,
            Self::All => Self::Default,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NoTools => "no tools",
            Self::UserOnly => "user only",
            Self::LabeledOnly => "labeled",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Entry {
    User {
        text: String,
        timestamp: String,
    },
    Reasoning {
        text: String,
        active: bool,
        expanded: bool,
    },
    Diff {
        path: String,
        lines: Vec<DiffLine>,
        expanded: bool,
    },
    Tool {
        id: String,
        label: String,
        detail: String,
        status: ToolStatus,
        duration: Option<String>,
        started_at: Option<Instant>,
        result: Option<String>,
        expanded: bool,
    },
    Assistant {
        lines: Vec<String>,
        timestamp: String,
    },
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub number: Option<u32>,
    pub text: String,
    pub kind: DiffKind,
}

#[derive(Clone, Copy, Debug)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
}

#[derive(Clone, Copy, Debug)]
pub enum ToolStatus {
    Running,
    Success,
    Error,
}

#[derive(Clone, Copy, Debug)]
pub enum PermissionMode {
    Normal,
    Plan,
    AlwaysApprove,
}

impl PermissionMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Plan => "plan",
            Self::AlwaysApprove => "always approve",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Normal => Self::Plan,
            Self::Plan => Self::AlwaysApprove,
            Self::AlwaysApprove => Self::Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Focus {
    Prompt,
    Scrollback,
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub branch: String,
    pub cwd: String,
    pub context_used: u64,
    pub context_limit: u64,
    pub tasks_complete: usize,
    pub tasks_total: usize,
    pub entries: Vec<Entry>,
    pub prompt: String,
    pub cursor: usize,
    pub prompt_history: Vec<String>,
    pub history_index: Option<usize>,
    pub placeholder: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub status: String,
    pub streaming: bool,
    pub thinking_level: String,
    pub queued_steering: Vec<String>,
    pub queued_follow_up: Vec<String>,
    pub scroll_from_bottom: usize,
    pub focus: Focus,
    pub inside_think_tag: bool,
    pub overlay: OverlayKind,
    pub overlay_query: String,
    pub overlay_selected: usize,
    pub pending_permission: Option<PendingPermission>,
    pub available_models: Vec<ModelInfo>,
    pub available_sessions: Vec<SessionInfo>,
    pub session_tree: Vec<SessionTreeEntry>,
    pub tree_filter: TreeFilter,
    pub tree_show_timestamps: bool,
    pub pending_label_entry: Option<String>,
    pub expanded_tool_groups: HashSet<usize>,
    pub focused_tool: Option<usize>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            branch: "pi-shell".into(),
            cwd: "~/dev/pi-shell".into(),
            context_used: 0,
            context_limit: 200_000,
            tasks_complete: 0,
            tasks_total: 0,
            entries: Vec::new(),
            prompt: String::new(),
            cursor: 0,
            prompt_history: Vec::new(),
            history_index: None,
            placeholder: "Ask anything…".into(),
            model: "Mock model".into(),
            permission_mode: PermissionMode::Normal,
            status: "idle".into(),
            streaming: false,
            thinking_level: "off".into(),
            queued_steering: Vec::new(),
            queued_follow_up: Vec::new(),
            scroll_from_bottom: 0,
            focus: Focus::Prompt,
            inside_think_tag: false,
            overlay: OverlayKind::None,
            overlay_query: String::new(),
            overlay_selected: 0,
            pending_permission: None,
            available_models: vec![ModelInfo {
                id: "mock".into(),
                display_name: "Mock model".into(),
            }],
            available_sessions: Vec::new(),
            session_tree: Vec::new(),
            tree_filter: TreeFilter::default(),
            tree_show_timestamps: false,
            pending_label_entry: None,
            expanded_tool_groups: HashSet::new(),
            focused_tool: None,
        }
    }
}

impl AppState {
    pub fn first_slash_match(&self) -> Option<&'static str> {
        if !self.prompt.starts_with('/') || self.prompt.contains(char::is_whitespace) {
            return None;
        }
        let query = self.prompt.to_ascii_lowercase();
        SLASH_COMMANDS
            .iter()
            .copied()
            .find(|command| command.starts_with(&query))
    }

    pub fn complete_slash_command(&mut self) -> bool {
        let Some(command) = self.first_slash_match() else {
            return false;
        };
        self.prompt = command.to_string();
        self.cursor = self.prompt.chars().count();
        self.history_index = None;
        true
    }

    pub fn activate_slash_command(&mut self) -> Option<OverlayAction> {
        let input = self.prompt.trim();
        let mut parts = input.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default().to_ascii_lowercase();
        let argument = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let action = match command.as_str() {
            "/model" => {
                self.open_overlay(OverlayKind::ModelPicker);
                OverlayAction::None
            }
            "/resume" => {
                self.open_overlay(OverlayKind::SessionPicker);
                OverlayAction::None
            }
            "/new" => OverlayAction::NewSession,
            "/name" if argument.is_some() => OverlayAction::NameSession(argument.unwrap().into()),
            "/session" => OverlayAction::SessionInfo,
            "/clone" => OverlayAction::CloneSession,
            "/tree" => OverlayAction::LoadTree { user_only: false },
            "/fork" => OverlayAction::LoadTree { user_only: true },
            "/thinking" => OverlayAction::CycleThinking,
            "/mode" => {
                self.cycle_permission_mode();
                OverlayAction::None
            }
            "/settings" => {
                self.open_overlay(OverlayKind::Settings);
                OverlayAction::None
            }
            "/help" => {
                self.open_overlay(OverlayKind::CommandPalette);
                OverlayAction::None
            }
            "/clear" => {
                self.entries.clear();
                self.context_used = 0;
                self.scroll_from_bottom = 0;
                OverlayAction::None
            }
            "/compact" => OverlayAction::Compact(argument.map(str::to_string)),
            "/quit" => OverlayAction::Quit,
            _ => return None,
        };
        self.clear_prompt();
        Some(action)
    }

    pub fn open_overlay(&mut self, overlay: OverlayKind) {
        self.overlay = overlay;
        self.overlay_query.clear();
        self.overlay_selected = 0;
    }

    pub fn close_overlay(&mut self) {
        if self.overlay != OverlayKind::Permission {
            self.overlay = OverlayKind::None;
            self.overlay_query.clear();
            self.overlay_selected = 0;
        }
    }

    pub fn overlay_items(&self) -> Vec<String> {
        let source = match self.overlay {
            OverlayKind::CommandPalette => {
                COMMANDS.iter().map(|item| (*item).to_string()).collect()
            }
            OverlayKind::ModelPicker => self
                .available_models
                .iter()
                .map(|model| model.display_name.clone())
                .collect(),
            OverlayKind::SessionPicker => {
                self.available_sessions.iter().map(session_label).collect()
            }
            OverlayKind::TreePicker | OverlayKind::ForkPicker => self
                .filtered_tree()
                .into_iter()
                .map(|entry| tree_label(entry, self.tree_show_timestamps))
                .collect(),
            OverlayKind::Settings => SETTINGS.iter().map(|item| (*item).to_string()).collect(),
            OverlayKind::LabelEditor => Vec::new(),
            OverlayKind::Permission => PERMISSION_OPTIONS
                .iter()
                .map(|item| (*item).to_string())
                .collect(),
            OverlayKind::None => Vec::new(),
        };
        let query = self.overlay_query.to_ascii_lowercase();
        source
            .into_iter()
            .filter(|item| query.is_empty() || item.to_ascii_lowercase().contains(&query))
            .collect()
    }

    pub fn move_overlay_selection(&mut self, delta: isize) {
        let count = self.overlay_items().len();
        if count == 0 {
            self.overlay_selected = 0;
            return;
        }
        self.overlay_selected = if delta < 0 {
            self.overlay_selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.overlay_selected
                .saturating_add(delta as usize)
                .min(count - 1)
        };
    }

    pub fn insert_overlay_char(&mut self, character: char) {
        if matches!(
            self.overlay,
            OverlayKind::CommandPalette
                | OverlayKind::ModelPicker
                | OverlayKind::SessionPicker
                | OverlayKind::TreePicker
                | OverlayKind::ForkPicker
                | OverlayKind::LabelEditor
        ) {
            self.overlay_query.push(character);
            self.overlay_selected = 0;
        }
    }

    pub fn overlay_backspace(&mut self) {
        self.overlay_query.pop();
        self.overlay_selected = 0;
    }

    pub fn activate_overlay(&mut self) -> OverlayAction {
        if self.overlay == OverlayKind::LabelEditor {
            let Some(entry_id) = self.pending_label_entry.take() else {
                return OverlayAction::None;
            };
            let label = (!self.overlay_query.trim().is_empty())
                .then(|| self.overlay_query.trim().to_string());
            self.close_overlay();
            return OverlayAction::SetLabel { entry_id, label };
        }
        let Some(item) = self.overlay_items().get(self.overlay_selected).cloned() else {
            return OverlayAction::None;
        };
        match self.overlay {
            OverlayKind::CommandPalette => match item.as_str() {
                "Resume session" => self.open_overlay(OverlayKind::SessionPicker),
                "Model picker" => self.open_overlay(OverlayKind::ModelPicker),
                "Settings" => self.open_overlay(OverlayKind::Settings),
                "Cycle mode" => {
                    self.cycle_permission_mode();
                    self.close_overlay();
                }
                "Quit" => return OverlayAction::Quit,
                _ => {}
            },
            OverlayKind::ModelPicker => {
                let Some(model) = self
                    .available_models
                    .iter()
                    .find(|model| model.display_name == item)
                    .cloned()
                else {
                    return OverlayAction::None;
                };
                self.model = model.display_name;
                self.close_overlay();
                return OverlayAction::SetModel { id: model.id };
            }
            OverlayKind::SessionPicker => {
                let Some(session) = self
                    .available_sessions
                    .iter()
                    .find(|session| session_label(session) == item)
                else {
                    return OverlayAction::None;
                };
                let target = session.path.clone();
                for session in &mut self.available_sessions {
                    session.current = session.path == target;
                }
                self.close_overlay();
                return OverlayAction::ResumeSession { target };
            }
            OverlayKind::TreePicker | OverlayKind::ForkPicker => {
                let Some(entry) = self
                    .filtered_tree()
                    .into_iter()
                    .find(|entry| tree_label(entry, self.tree_show_timestamps) == item)
                else {
                    return OverlayAction::None;
                };
                let entry_id = entry.id.clone();
                let fork = self.overlay == OverlayKind::ForkPicker;
                self.close_overlay();
                return if fork {
                    OverlayAction::ForkSession { entry_id }
                } else {
                    OverlayAction::NavigateTree {
                        entry_id,
                        summarize: false,
                    }
                };
            }
            OverlayKind::Settings => self.close_overlay(),
            OverlayKind::Permission => {
                let decision = match item.as_str() {
                    "Always allow" => PermissionDecision::AllowAlways,
                    "Deny" => PermissionDecision::Deny,
                    _ => PermissionDecision::AllowOnce,
                };
                if let Some(permission) = self.pending_permission.take() {
                    self.overlay = OverlayKind::None;
                    return OverlayAction::Permission {
                        request_id: permission.id,
                        decision,
                    };
                }
            }
            OverlayKind::LabelEditor | OverlayKind::None => {}
        }
        OverlayAction::None
    }

    pub fn cycle_tree_filter(&mut self) {
        self.tree_filter = self.tree_filter.next();
        self.overlay_selected = 0;
    }

    pub fn toggle_tree_timestamps(&mut self) {
        self.tree_show_timestamps = !self.tree_show_timestamps;
    }

    pub fn begin_tree_label(&mut self) {
        if !matches!(
            self.overlay,
            OverlayKind::TreePicker | OverlayKind::ForkPicker
        ) {
            return;
        }
        let entries = self.filtered_tree();
        let Some(entry) = entries.get(self.overlay_selected) else {
            return;
        };
        let entry_id = entry.id.clone();
        let label = entry.label.clone().unwrap_or_default();
        self.pending_label_entry = Some(entry_id);
        self.overlay_query = label;
        self.overlay = OverlayKind::LabelEditor;
        self.overlay_selected = 0;
    }

    pub fn activate_tree_with_summary(&mut self) -> OverlayAction {
        if self.overlay != OverlayKind::TreePicker {
            return self.activate_overlay();
        }
        let entries = self.filtered_tree();
        let Some(entry) = entries.get(self.overlay_selected) else {
            return OverlayAction::None;
        };
        let entry_id = entry.id.clone();
        self.close_overlay();
        OverlayAction::NavigateTree {
            entry_id,
            summarize: true,
        }
    }

    fn filtered_tree(&self) -> Vec<&SessionTreeEntry> {
        self.session_tree
            .iter()
            .filter(|entry| match self.tree_filter {
                TreeFilter::Default => {
                    !matches!(entry.kind.as_str(), "custom" | "label" | "session_info")
                }
                TreeFilter::NoTools => entry.role.as_deref() != Some("toolResult"),
                TreeFilter::UserOnly => entry.role.as_deref() == Some("user"),
                TreeFilter::LabeledOnly => entry.label.is_some(),
                TreeFilter::All => true,
            })
            .collect()
    }

    fn append_assistant_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let pieces = text.split('\n');
        if let Some(Entry::Assistant { lines, .. }) = self.entries.last_mut() {
            for (index, piece) in pieces.enumerate() {
                if index == 0 {
                    if let Some(last) = lines.last_mut() {
                        last.push_str(piece);
                    } else {
                        lines.push(piece.to_string());
                    }
                } else {
                    lines.push(piece.to_string());
                }
            }
        } else {
            self.entries.push(Entry::Assistant {
                lines: pieces.map(str::to_string).collect(),
                timestamp: String::new(),
            });
        }
    }

    fn append_reasoning_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(Entry::Reasoning {
            text: current,
            active: true,
            ..
        }) = self.entries.last_mut()
        {
            current.push_str(text);
        } else {
            self.entries.push(Entry::Reasoning {
                text: text.to_string(),
                active: true,
                expanded: false,
            });
        }
    }

    fn apply_text_delta(&mut self, text: String) {
        let mut remaining = text.as_str();
        while !remaining.is_empty() {
            if self.inside_think_tag {
                if let Some(end) = remaining.find("</think>") {
                    self.append_reasoning_text(&remaining[..end]);
                    self.inside_think_tag = false;
                    remaining = &remaining[end + "</think>".len()..];
                } else {
                    self.append_reasoning_text(remaining);
                    break;
                }
            } else if let Some(start) = remaining.find("<think>") {
                self.append_assistant_text(&remaining[..start]);
                self.inside_think_tag = true;
                remaining = &remaining[start + "<think>".len()..];
            } else {
                self.append_assistant_text(remaining);
                break;
            }
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Prompt => Focus::Scrollback,
            Focus::Scrollback => Focus::Prompt,
        };
    }

    pub fn focus_prompt(&mut self) {
        self.focus = Focus::Prompt;
    }

    pub fn focus_scrollback(&mut self) {
        self.focus = Focus::Scrollback;
    }

    pub fn insert_char(&mut self, character: char) {
        let byte = char_to_byte(&self.prompt, self.cursor);
        self.prompt.insert(byte, character);
        self.cursor += 1;
        self.history_index = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = char_to_byte(&self.prompt, self.cursor - 1);
        let end = char_to_byte(&self.prompt, self.cursor);
        self.prompt.replace_range(start..end, "");
        self.cursor -= 1;
        self.history_index = None;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.prompt.chars().count() {
            return;
        }
        let start = char_to_byte(&self.prompt, self.cursor);
        let end = char_to_byte(&self.prompt, self.cursor + 1);
        self.prompt.replace_range(start..end, "");
        self.history_index = None;
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = self
            .cursor
            .saturating_add(1)
            .min(self.prompt.chars().count());
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor = self.prompt.chars().count();
    }

    pub fn clear_prompt(&mut self) {
        self.prompt.clear();
        self.cursor = 0;
        self.history_index = None;
    }

    pub fn previous_prompt(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        let index = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => self.prompt_history.len() - 1,
        };
        self.load_history(index);
    }

    pub fn next_prompt(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.prompt_history.len() {
            self.clear_prompt();
        } else {
            self.load_history(index + 1);
        }
    }

    pub fn cycle_permission_mode(&mut self) {
        self.permission_mode = self.permission_mode.next();
    }

    pub fn toggle_latest_reasoning(&mut self) {
        if let Some(Entry::Reasoning { expanded, .. }) = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| matches!(entry, Entry::Reasoning { .. }))
        {
            *expanded = !*expanded;
        }
    }

    pub fn set_all_reasoning_expanded(&mut self, expanded: bool) {
        for entry in &mut self.entries {
            if let Entry::Reasoning {
                expanded: current, ..
            } = entry
            {
                *current = expanded;
            }
        }
    }

    pub fn toggle_latest_tool(&mut self) {
        if let Some(index) = self
            .entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| matches!(entry, Entry::Tool { .. }).then_some(index))
        {
            self.toggle_tool_at(index);
        }
    }

    pub fn toggle_tool_at(&mut self, index: usize) {
        self.focused_tool = Some(index);
        if let Some(Entry::Tool { expanded, .. }) = self.entries.get_mut(index) {
            *expanded = !*expanded;
        }
    }

    pub fn toggle_tool_group(&mut self, start: usize) {
        self.focused_tool = Some(start);
        if !self.expanded_tool_groups.remove(&start) {
            self.expanded_tool_groups.insert(start);
        }
    }

    pub fn toggle_latest_diff(&mut self) {
        if let Some(Entry::Diff { expanded, .. }) = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| matches!(entry, Entry::Diff { .. }))
        {
            *expanded = !*expanded;
        }
    }

    pub fn all_reasoning_expanded(&self) -> bool {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Reasoning { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .all(|expanded| expanded)
    }

    pub fn submit_prompt(&mut self) -> Option<String> {
        let prompt = self.prompt.trim().to_string();
        if prompt.is_empty() {
            return None;
        }
        self.prompt_history.push(prompt.clone());
        self.entries.push(Entry::User {
            text: prompt.clone(),
            timestamp: String::new(),
        });
        self.clear_prompt();
        self.scroll_to_bottom();
        self.status = "queued".into();
        Some(prompt)
    }

    fn load_history(&mut self, index: usize) {
        self.prompt.clone_from(&self.prompt_history[index]);
        self.cursor = self.prompt.chars().count();
        self.history_index = Some(index);
    }

    pub fn scroll_up(&mut self, amount: usize, max_scroll: usize) {
        self.scroll_from_bottom = self
            .scroll_from_bottom
            .saturating_add(amount)
            .min(max_scroll);
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(amount);
    }

    pub fn scroll_to_top(&mut self, max_scroll: usize) {
        self.scroll_from_bottom = max_scroll;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_from_bottom = 0;
    }

    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::SessionReset => {
                self.entries.clear();
                self.context_used = 0;
                self.scroll_from_bottom = 0;
                self.status = "session resumed".into();
                self.streaming = false;
                self.expanded_tool_groups.clear();
                self.focused_tool = None;
            }
            AgentEvent::UserMessage { text } => {
                self.entries.push(Entry::User {
                    text,
                    timestamp: String::new(),
                });
            }
            AgentEvent::ModelChanged {
                id: _,
                display_name,
            } => self.model = display_name,
            AgentEvent::SessionInfo { summary } => {
                self.entries.push(Entry::Assistant {
                    lines: vec![summary],
                    timestamp: String::new(),
                });
                self.status = "session info".into();
            }
            AgentEvent::PromptPrefill { text } => {
                self.prompt = text;
                self.cursor = self.prompt.chars().count();
                self.focus = Focus::Prompt;
            }
            AgentEvent::ThinkingChanged { level } => self.thinking_level = level,
            AgentEvent::QueueChanged {
                steering,
                follow_up,
            } => {
                self.queued_steering = steering;
                self.queued_follow_up = follow_up;
                let count = self.queued_steering.len() + self.queued_follow_up.len();
                if count > 0 {
                    self.status = format!("{count} queued");
                }
            }
            AgentEvent::SessionTree { entries, user_only } => {
                self.session_tree = entries;
                self.open_overlay(if user_only {
                    OverlayKind::ForkPicker
                } else {
                    OverlayKind::TreePicker
                });
            }
            AgentEvent::TextDelta { text } => {
                self.apply_text_delta(text);
                self.status = "generating…".into();
                self.streaming = true;
            }
            AgentEvent::ReasoningDelta { text } => {
                self.append_reasoning_text(&text);
                self.status = "thinking…".into();
                self.streaming = true;
            }
            AgentEvent::ToolCallStart { id, name, args } => {
                if name.eq_ignore_ascii_case("edit")
                    && let Some(diff_lines) = build_edit_diff(&args)
                {
                    let path = args
                        .get("path")
                        .or_else(|| args.get("file_path"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("(unknown path)")
                        .to_string();
                    self.entries.push(Entry::Diff {
                        path,
                        lines: diff_lines,
                        expanded: true,
                    });
                    return;
                }
                let (label, detail) = tool_display(&name, &args);
                self.entries.push(Entry::Tool {
                    id,
                    label,
                    detail,
                    status: ToolStatus::Running,
                    duration: None,
                    started_at: Some(Instant::now()),
                    result: None,
                    expanded: false,
                });
                self.status = "running tool…".into();
            }
            AgentEvent::ToolCallResult {
                id,
                result: ToolResult { content },
                is_error,
                duration_ms,
            } => {
                let mut completed_agent: Option<(String, String, Option<u64>)> = None;
                if let Some(Entry::Tool {
                    status,
                    duration,
                    started_at,
                    label,
                    detail,
                    result: current_result,
                    ..
                }) = self.entries.iter_mut().rev().find(|entry| {
                    matches!(
                        entry,
                        Entry::Tool {
                            id: tool_id,
                            status: ToolStatus::Running,
                            ..
                        } if tool_id == &id
                    )
                }) {
                    let background_agent =
                        label == "Agent" && background_agent_id(&content).is_some();
                    if !background_agent {
                        let local_elapsed = started_at.take().map(|started| started.elapsed());
                        *duration = duration_ms
                            .map(|milliseconds| {
                                format_elapsed(std::time::Duration::from_millis(milliseconds))
                            })
                            .or_else(|| local_elapsed.map(format_elapsed));
                    }
                    if label == "Agent result" {
                        completed_agent = Some((detail.clone(), content.clone(), duration_ms));
                    }
                    *current_result = Some(content);
                    if label == "Search"
                        && !detail.contains(" matches)")
                        && let Some(count) =
                            search_match_count(current_result.as_deref().unwrap_or_default())
                    {
                        detail.push_str(&format!(" ({count} matches)"));
                    }
                    if !background_agent {
                        *status = if is_error {
                            ToolStatus::Error
                        } else {
                            ToolStatus::Success
                        };
                    } else {
                        *duration = None;
                    }
                }
                if let Some((agent_id, report, agent_duration_ms)) = completed_agent
                    && let Some(Entry::Tool {
                        status,
                        duration,
                        started_at,
                        result,
                        ..
                    }) = self.entries.iter_mut().rev().find(|entry| {
                        matches!(
                            entry,
                            Entry::Tool {
                                label,
                                status: ToolStatus::Running,
                                result: Some(spawn_result),
                                ..
                            } if label == "Agent"
                                && background_agent_id(spawn_result).as_deref() == Some(agent_id.as_str())
                        )
                    })
                {
                    let local_elapsed = started_at.take().map(|started| started.elapsed());
                    *duration = agent_duration_ms
                        .map(|milliseconds| {
                            format_elapsed(std::time::Duration::from_millis(milliseconds))
                        })
                        .or_else(|| local_elapsed.map(format_elapsed));
                    *status = if is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Success
                    };
                    *result = Some(report);
                }
            }
            AgentEvent::TurnComplete {
                usage:
                    Usage {
                        input_tokens,
                        output_tokens,
                    },
                ..
            } => {
                self.context_used = input_tokens + output_tokens;
                self.status = "idle".into();
                self.streaming = false;
                for entry in &mut self.entries {
                    if let Entry::Reasoning { active, .. } = entry {
                        *active = false;
                    }
                }
            }
            AgentEvent::Error { message, .. } => {
                self.entries.push(Entry::Tool {
                    id: String::new(),
                    label: "Error".into(),
                    detail: "Agent error".into(),
                    status: ToolStatus::Error,
                    duration: None,
                    started_at: None,
                    result: Some(message),
                    expanded: true,
                });
                self.status = "error".into();
                self.streaming = false;
            }
            AgentEvent::PermissionRequest {
                id, tool, reason, ..
            } => {
                self.pending_permission = Some(PendingPermission { id, tool, reason });
                self.open_overlay(OverlayKind::Permission);
            }
            _ => {}
        }
    }
}

fn format_elapsed(duration: std::time::Duration) -> String {
    if duration.as_secs() >= 60 {
        format!(
            "{}m {:02}s",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    } else if duration.as_secs() >= 1 {
        format!("{:.1}s", duration.as_secs_f32())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn char_to_byte(value: &str, character_index: usize) -> usize {
    value
        .char_indices()
        .nth(character_index)
        .map_or(value.len(), |(index, _)| index)
}

pub(crate) fn session_label(session: &SessionInfo) -> String {
    let title = session
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(&session.first_message);
    let title = if title.is_empty() {
        "Untitled session"
    } else {
        title
    };
    let title = title.chars().take(20).collect::<String>();
    let modified = session.modified.get(..16).unwrap_or(&session.modified);
    format!("{title}  · {modified}  · {} msg", session.message_count)
}

pub(crate) fn tree_label(entry: &SessionTreeEntry, show_timestamp: bool) -> String {
    let branch = if entry.active { "●" } else { "○" };
    let indent = "  ".repeat(entry.depth.min(8));
    let role = entry.role.as_deref().unwrap_or(&entry.kind);
    let label = entry
        .label
        .as_deref()
        .map(|value| format!(" [{value}]"))
        .unwrap_or_default();
    let timestamp = if show_timestamp {
        format!("  {}", entry.timestamp)
    } else {
        String::new()
    };
    format!("{branch} {indent}{role}: {}{label}{timestamp}", entry.text)
}

fn tool_display(name: &str, args: &serde_json::Value) -> (String, String) {
    let normalized = name.to_ascii_lowercase();
    let label = match normalized.as_str() {
        "bash" | "shell" | "run" => "Run".to_string(),
        "read" => "Read".to_string(),
        "edit" => "Edit".to_string(),
        "write" => "Write".to_string(),
        "search" | "grep" => "Search".to_string(),
        "agent" | "spawn_agent" => "Agent".to_string(),
        "get_subagent_result" => "Agent result".to_string(),
        _ => {
            let mut characters = name.chars();
            characters
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                .unwrap_or_default()
        }
    };
    let mut detail = [
        "path",
        "file_path",
        "command",
        "cmd",
        "description",
        "agent_id",
        "query",
        "url",
    ]
    .iter()
    .find_map(|key| args.get(key).and_then(serde_json::Value::as_str))
    .map_or_else(|| args.to_string(), str::to_string);
    if label == "Search" {
        let query = args
            .get("query")
            .or_else(|| args.get("pattern"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&detail);
        let path = args
            .get("path")
            .or_else(|| args.get("directory"))
            .and_then(serde_json::Value::as_str);
        detail = path.map_or_else(
            || format!("\"{query}\""),
            |path| format!("\"{query}\" in {path}"),
        );
    } else if matches!(label.as_str(), "Read" | "Edit") {
        let offset = args.get("offset").and_then(serde_json::Value::as_u64);
        let limit = args.get("limit").and_then(serde_json::Value::as_u64);
        if let Some(offset) = offset {
            let range = limit.map_or_else(
                || offset.to_string(),
                |limit| {
                    format!(
                        "{offset}-{}",
                        offset.saturating_add(limit).saturating_sub(1)
                    )
                },
            );
            detail.push_str(&format!(" ({range})"));
        }
    }
    (label, detail)
}

fn search_match_count(result: &str) -> Option<usize> {
    if let Some(start) = result.rfind('(')
        && let Some(value) = result[start + 1..].split_whitespace().next()
        && result[start..].contains("match")
        && let Ok(count) = value.parse()
    {
        return Some(count);
    }
    let count = result
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    (count > 0).then_some(count)
}

fn build_edit_diff(args: &serde_json::Value) -> Option<Vec<DiffLine>> {
    let object = args.as_object()?;
    let old_text = object
        .get("old_text")
        .or_else(|| object.get("old_string"))
        .and_then(serde_json::Value::as_str)?;
    let new_text = object
        .get("new_text")
        .or_else(|| object.get("new_string"))
        .and_then(serde_json::Value::as_str)?;
    if old_text == new_text {
        return None;
    }
    let start = object
        .get("start_line")
        .or_else(|| object.get("offset"))
        .or_else(|| object.get("line"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as u32);
    let mut diff_lines = Vec::new();
    let old_lines: Vec<&str> = old_text.split_terminator('\n').collect();
    let new_lines: Vec<&str> = new_text.split_terminator('\n').collect();
    if !old_lines.is_empty() {
        for (index, line) in old_lines.iter().enumerate() {
            let number = start.map(|base| base + index as u32);
            diff_lines.push(DiffLine {
                number,
                text: (*line).to_string(),
                kind: DiffKind::Removed,
            });
        }
    }
    if !new_lines.is_empty() {
        let base = start.unwrap_or(0) + old_lines.len() as u32;
        for (index, line) in new_lines.iter().enumerate() {
            let number = if start.is_some() {
                Some(base + index as u32)
            } else {
                None
            };
            diff_lines.push(DiffLine {
                number,
                text: (*line).to_string(),
                kind: DiffKind::Added,
            });
        }
    }
    if diff_lines.is_empty() {
        None
    } else {
        Some(diff_lines)
    }
}

fn background_agent_id(result: &str) -> Option<String> {
    for marker in ["Agent ID:", "agent_id\":\"", "agent_id': '"] {
        let Some((_, remainder)) = result.split_once(marker) else {
            continue;
        };
        let id = remainder
            .trim_start()
            .trim_matches('"')
            .split(|character: char| {
                character.is_whitespace() || matches!(character, '"' | '\'' | ',' | '}')
            })
            .next()
            .unwrap_or_default();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}
