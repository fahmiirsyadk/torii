use pi_harness::{
    AgentEvent, ModelInfo, PermissionDecision, RuntimeCommand, RuntimeSettings, SessionInfo,
    SessionTreeEntry, ToolResult, Usage,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    time::Instant,
};

const COMMANDS: &[&str] = &[
    "Resume session",
    "Model picker",
    "Settings",
    "Cycle mode",
    "Quit",
];
pub type TranscriptHitRegion = (String, usize, u16, u16, bool);
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
    "/context",
    "/reload",
    "/scoped-models",
    "/trust",
    "/export",
    "/import",
    "/copy",
    "/login",
    "/rewind",
    "/plan",
    "/trace",
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
    FilePicker,
    ScopedModels,
    OauthPrompt,
    OauthSelect,
    RewindPicker,
    Settings,
    Permission,
}

#[derive(Clone, Debug)]
pub struct PendingPermission {
    pub id: String,
    pub tool: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct PendingOauth {
    pub id: String,
    pub message: String,
    pub options: Vec<pi_harness::AuthChoice>,
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
    OauthReply {
        id: String,
        value: Option<String>,
    },
    BeginOauth(String),
    SetPermissionMode(String),
    LoadRewinds,
    RewindFile(String),
    ExportTrace(Option<String>),
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
        id: String,
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
    Compaction {
        summary: String,
        tokens_before: Option<u64>,
        tokens_after: Option<u64>,
        active: bool,
        error: Option<String>,
        /// Wall-clock time the compaction entered the Start phase. Used by
        /// the banner above the composer to render an elapsed-time counter
        /// (e.g. "3.2s") that ticks while the compaction is running.
        started_at: Option<Instant>,
    },
    /// A slim "this session was previously compacted" line emitted on
    /// session load for each stored compaction/branch_summary entry.
    /// Not interactive — the user just sees the diamond glyph and the
    /// pre-compaction token count.
    CompactionIndicator {
        reason: String,
        tokens_before: Option<u64>,
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

    pub fn wire_value(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Plan => "plan",
            Self::AlwaysApprove => "always_approve",
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
    pub plan_entries: Vec<pi_harness::PlanEntry>,
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
    /// Wall-clock time the LLM started the current turn (the first TextDelta
    /// or ReasoningDelta of a turn). Used by the working banner above the
    /// composer to render a ticking elapsed-time counter while the model is
    /// generating. Cleared on TurnComplete, error, or compaction.
    pub turn_started_at: Option<Instant>,
    /// Snapshot of `context_used` (i.e. the input token count) at the
    /// moment the current turn started. The working banner shows this as
    /// the ↑ input figure.
    pub turn_input_tokens: u64,
    /// Cumulative characters streamed during the current turn. The working
    /// banner divides this by 4 to get an approximate output-token count
    /// and shows it as the ↓ output figure. Replaced by the wire's real
    /// `output_tokens` on TurnComplete.
    pub turn_output_chars: u64,
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
    pub pending_oauth: Option<PendingOauth>,
    pub available_models: Vec<ModelInfo>,
    pub available_sessions: Vec<SessionInfo>,
    pub available_files: Vec<String>,
    pub runtime_commands: Vec<RuntimeCommand>,
    pub context_files: Vec<String>,
    pub runtime_settings: RuntimeSettings,
    pub session_tree: Vec<SessionTreeEntry>,
    pub rewind_checkpoints: Vec<pi_harness::RewindCheckpoint>,
    pub tree_filter: TreeFilter,
    pub tree_show_timestamps: bool,
    pub pending_label_entry: Option<String>,
    pub expanded_tool_groups: HashSet<usize>,
    pub focused_tool: Option<usize>,
    pub focused_entry: Option<usize>,
    pub focused_section: Option<usize>,
    pub focused_target_id: Option<String>,
    pub hovered_entry: Option<usize>,
    pub hovered_target_id: Option<String>,
    pub transcript_rect: Cell<Option<(u16, u16, u16, u16)>>,
    pub transcript_hit_regions: RefCell<Vec<TranscriptHitRegion>>,
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
            plan_entries: Vec::new(),
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
            turn_started_at: None,
            turn_input_tokens: 0,
            turn_output_chars: 0,
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
            pending_oauth: None,
            available_models: vec![ModelInfo {
                id: "mock".into(),
                display_name: "Mock model".into(),
            }],
            available_sessions: Vec::new(),
            available_files: Vec::new(),
            runtime_commands: Vec::new(),
            context_files: Vec::new(),
            runtime_settings: RuntimeSettings::default(),
            session_tree: Vec::new(),
            rewind_checkpoints: Vec::new(),
            tree_filter: TreeFilter::default(),
            tree_show_timestamps: false,
            pending_label_entry: None,
            expanded_tool_groups: HashSet::new(),
            focused_tool: None,
            focused_entry: None,
            focused_section: None,
            focused_target_id: None,
            hovered_entry: None,
            hovered_target_id: None,
            transcript_rect: Cell::new(None),
            transcript_hit_regions: RefCell::new(Vec::new()),
        }
    }
}

impl AppState {
    pub fn first_slash_match(&self) -> Option<String> {
        if !self.prompt.starts_with('/') || self.prompt.contains(char::is_whitespace) {
            return None;
        }
        let query = self.prompt.to_ascii_lowercase();
        SLASH_COMMANDS
            .iter()
            .map(|command| (*command).to_string())
            .chain(
                self.runtime_commands
                    .iter()
                    .map(|command| command.name.clone()),
            )
            .find(|command| command.starts_with(&query))
    }

    pub fn complete_slash_command(&mut self) -> bool {
        let Some(command) = self.first_slash_match() else {
            return false;
        };
        self.prompt = command;
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
            "/reload" => OverlayAction::ReloadResources,
            "/scoped-models" => {
                self.open_overlay(OverlayKind::ScopedModels);
                OverlayAction::None
            }
            "/trust" => OverlayAction::SetProjectTrust(!self.runtime_settings.project_trusted),
            "/export" => OverlayAction::ExportSession(argument.map(str::to_string)),
            "/import" if argument.is_some() => {
                OverlayAction::ImportSession(argument.unwrap().to_string())
            }
            "/copy" => OverlayAction::CopyLast,
            "/login" if argument.is_some() => {
                OverlayAction::BeginOauth(argument.unwrap().to_string())
            }
            "/rewind" => OverlayAction::LoadRewinds,
            "/plan" => {
                self.permission_mode = PermissionMode::Plan;
                OverlayAction::SetPermissionMode("plan".into())
            }
            "/trace" => OverlayAction::ExportTrace(argument.map(str::to_string)),
            "/context" => {
                let lines = if self.context_files.is_empty() {
                    vec!["No Pi context files loaded".into()]
                } else {
                    std::iter::once("Loaded Pi context files:".into())
                        .chain(self.context_files.iter().map(|path| format!("• {path}")))
                        .collect()
                };
                self.entries.push(Entry::Assistant {
                    lines,
                    timestamp: String::new(),
                });
                OverlayAction::None
            }
            "/mode" => {
                self.cycle_permission_mode();
                OverlayAction::SetPermissionMode(self.permission_mode.wire_value().into())
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

    pub fn slash_suggestions(&self) -> Vec<(String, String)> {
        let query = self.prompt.trim_start_matches('/').to_ascii_lowercase();
        let builtins = SLASH_COMMANDS
            .iter()
            .map(|name| ((*name).to_string(), builtin_description(name).to_string()));
        builtins
            .chain(self.runtime_commands.iter().map(|command| {
                (
                    command.name.clone(),
                    format!("{} · {}", command.description, command.source),
                )
            }))
            .filter(|(name, _)| name.trim_start_matches('/').starts_with(&query))
            .take(5)
            .collect()
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

    pub fn cancel_oauth(&mut self) -> OverlayAction {
        if !matches!(
            self.overlay,
            OverlayKind::OauthPrompt | OverlayKind::OauthSelect
        ) {
            self.close_overlay();
            return OverlayAction::None;
        }
        let Some(pending) = self.pending_oauth.take() else {
            self.close_overlay();
            return OverlayAction::None;
        };
        self.close_overlay();
        OverlayAction::OauthReply {
            id: pending.id,
            value: None,
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
            OverlayKind::Settings => vec![
                format!("Steering delivery: {}", self.runtime_settings.steering_mode),
                format!(
                    "Follow-up delivery: {}",
                    self.runtime_settings.follow_up_mode
                ),
                format!(
                    "Auto compaction: {}",
                    if self.runtime_settings.auto_compaction {
                        "on"
                    } else {
                        "off"
                    }
                ),
                format!(
                    "Default project trust: {}",
                    self.runtime_settings.default_project_trust
                ),
                format!(
                    "Current project trusted: {}",
                    if self.runtime_settings.project_trusted {
                        "yes"
                    } else {
                        "no"
                    }
                ),
                format!(
                    "Scoped models: {}",
                    self.runtime_settings.enabled_models.len()
                ),
            ],
            OverlayKind::ScopedModels => self
                .available_models
                .iter()
                .map(|model| {
                    let mark = if self.runtime_settings.enabled_models.contains(&model.id) {
                        "✓"
                    } else {
                        " "
                    };
                    format!("[{mark}] {}", model.display_name)
                })
                .collect(),
            OverlayKind::OauthPrompt => Vec::new(),
            OverlayKind::OauthSelect => self
                .pending_oauth
                .as_ref()
                .map(|pending| {
                    pending
                        .options
                        .iter()
                        .map(|option| option.label.clone())
                        .collect()
                })
                .unwrap_or_default(),
            OverlayKind::RewindPicker => self
                .rewind_checkpoints
                .iter()
                .map(|checkpoint| {
                    format!(
                        "{}  · {}  · {}",
                        checkpoint.path, checkpoint.tool, checkpoint.timestamp
                    )
                })
                .collect(),
            OverlayKind::LabelEditor => Vec::new(),
            OverlayKind::FilePicker => self.available_files.clone(),
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
                | OverlayKind::FilePicker
                | OverlayKind::ScopedModels
                | OverlayKind::OauthPrompt
                | OverlayKind::OauthSelect
                | OverlayKind::RewindPicker
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
        if self.overlay == OverlayKind::OauthPrompt {
            let Some(pending) = self.pending_oauth.take() else {
                return OverlayAction::None;
            };
            let value = Some(self.overlay_query.clone());
            self.close_overlay();
            return OverlayAction::OauthReply {
                id: pending.id,
                value,
            };
        }
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
                    return OverlayAction::SetPermissionMode(
                        self.permission_mode.wire_value().into(),
                    );
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
            OverlayKind::Settings => {
                let action = match self.overlay_selected {
                    0 => OverlayAction::SetRuntimeSetting {
                        key: "steering_mode".into(),
                        value: serde_json::json!(if self.runtime_settings.steering_mode == "all" {
                            "one-at-a-time"
                        } else {
                            "all"
                        }),
                    },
                    1 => OverlayAction::SetRuntimeSetting {
                        key: "follow_up_mode".into(),
                        value: serde_json::json!(
                            if self.runtime_settings.follow_up_mode == "all" {
                                "one-at-a-time"
                            } else {
                                "all"
                            }
                        ),
                    },
                    2 => OverlayAction::SetRuntimeSetting {
                        key: "auto_compaction".into(),
                        value: serde_json::json!(!self.runtime_settings.auto_compaction),
                    },
                    3 => {
                        let next = match self.runtime_settings.default_project_trust.as_str() {
                            "ask" => "always",
                            "always" => "never",
                            _ => "ask",
                        };
                        OverlayAction::SetRuntimeSetting {
                            key: "default_project_trust".into(),
                            value: serde_json::json!(next),
                        }
                    }
                    4 => OverlayAction::SetProjectTrust(!self.runtime_settings.project_trusted),
                    _ => {
                        self.open_overlay(OverlayKind::ScopedModels);
                        return OverlayAction::None;
                    }
                };
                self.apply_runtime_setting(&action);
                self.close_overlay();
                return action;
            }
            OverlayKind::ScopedModels => {
                let models = self.runtime_settings.enabled_models.clone();
                self.close_overlay();
                return OverlayAction::SetScopedModels(models);
            }
            OverlayKind::OauthSelect => {
                let Some(pending) = self.pending_oauth.take() else {
                    return OverlayAction::None;
                };
                let value = pending
                    .options
                    .iter()
                    .find(|option| option.label == item)
                    .map(|option| option.id.clone());
                self.close_overlay();
                return OverlayAction::OauthReply {
                    id: pending.id,
                    value,
                };
            }
            OverlayKind::RewindPicker => {
                let Some(checkpoint) = self.rewind_checkpoints.iter().find(|checkpoint| {
                    format!(
                        "{}  · {}  · {}",
                        checkpoint.path, checkpoint.tool, checkpoint.timestamp
                    ) == item
                }) else {
                    return OverlayAction::None;
                };
                let id = checkpoint.id.clone();
                self.close_overlay();
                return OverlayAction::RewindFile(id);
            }
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
            OverlayKind::OauthPrompt | OverlayKind::LabelEditor | OverlayKind::None => {}
            OverlayKind::FilePicker => {
                self.insert_file_reference(&item);
                self.close_overlay();
            }
        }
        OverlayAction::None
    }

    pub fn toggle_scoped_model(&mut self) {
        if self.overlay != OverlayKind::ScopedModels {
            return;
        }
        let Some(model) = self.available_models.get(self.overlay_selected) else {
            return;
        };
        if let Some(index) = self
            .runtime_settings
            .enabled_models
            .iter()
            .position(|id| id == &model.id)
        {
            self.runtime_settings.enabled_models.remove(index);
        } else {
            self.runtime_settings.enabled_models.push(model.id.clone());
        }
    }

    fn apply_runtime_setting(&mut self, action: &OverlayAction) {
        match action {
            OverlayAction::SetRuntimeSetting { key, value } if key == "steering_mode" => {
                self.runtime_settings.steering_mode = value.as_str().unwrap_or_default().into()
            }
            OverlayAction::SetRuntimeSetting { key, value } if key == "follow_up_mode" => {
                self.runtime_settings.follow_up_mode = value.as_str().unwrap_or_default().into()
            }
            OverlayAction::SetRuntimeSetting { key, value } if key == "auto_compaction" => {
                self.runtime_settings.auto_compaction = value.as_bool().unwrap_or(false)
            }
            OverlayAction::SetRuntimeSetting { key, value } if key == "default_project_trust" => {
                self.runtime_settings.default_project_trust =
                    value.as_str().unwrap_or_default().into()
            }
            OverlayAction::SetProjectTrust(trusted) => {
                self.runtime_settings.project_trusted = *trusted
            }
            _ => {}
        }
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

    fn insert_file_reference(&mut self, path: &str) {
        let byte = char_to_byte(&self.prompt, self.cursor);
        self.prompt.insert_str(byte, path);
        self.prompt.insert(byte + path.len(), ' ');
        self.cursor += path.chars().count() + 1;
        self.focus = Focus::Prompt;
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
        self.focused_entry = Some(index);
        self.focused_section = None;
        self.focused_tool = Some(index);
        if let Some(Entry::Tool { expanded, .. }) = self.entries.get_mut(index) {
            *expanded = !*expanded;
        }
    }

    pub fn toggle_tool_group(&mut self, start: usize) {
        self.focused_entry = Some(start);
        self.focused_section = None;
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

    pub fn toggle_entry_at(&mut self, index: usize) {
        self.focused_entry = Some(index);
        self.focused_section = None;
        match self.entries.get_mut(index) {
            Some(Entry::Reasoning { expanded, .. } | Entry::Diff { expanded, .. }) => {
                *expanded = !*expanded;
            }
            Some(Entry::Tool { .. }) => self.toggle_tool_at(index),
            _ => {}
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

    pub fn submit_bash(&mut self) -> Option<(String, bool)> {
        let input = self.prompt.trim();
        if !input.starts_with('!') {
            return None;
        }
        let exclude = input.starts_with("!!");
        let command = input.trim_start_matches('!').trim().to_string();
        if command.is_empty() {
            return None;
        }
        self.prompt_history.push(self.prompt.clone());
        self.clear_prompt();
        self.scroll_to_bottom();
        self.status = "running shell…".into();
        Some((command, exclude))
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

    /// Returns the `started_at` of the most recent active compaction, if any.
    /// The banner above the composer uses this to render a ticking elapsed-time
    /// counter (e.g. "3.2s") while compaction is in flight.
    pub fn active_compaction_started_at(&self) -> Option<Instant> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Compaction {
                active: true,
                started_at,
                ..
            } => *started_at,
            _ => None,
        })
    }

    /// Records the start of a new LLM turn. Called by the TextDelta and
    /// ReasoningDelta arms on the first delta of a turn. Snapshots
    /// `context_used` as the input-token count and resets the output-char
    /// accumulator so the working banner can show a meaningful ↑/↓ token
    /// tally while the model is generating.
    pub fn begin_turn_if_needed(&mut self) {
        if self.turn_started_at.is_none() {
            self.turn_started_at = Some(Instant::now());
            self.turn_input_tokens = self.context_used;
            self.turn_output_chars = 0;
        }
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
                self.turn_started_at = None;
                self.turn_input_tokens = 0;
                self.turn_output_chars = 0;
                self.expanded_tool_groups.clear();
                self.focused_tool = None;
                self.focused_entry = None;
                self.focused_section = None;
                self.focused_target_id = None;
                self.hovered_entry = None;
                self.hovered_target_id = None;
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
            AgentEvent::ResourcesChanged { resources } => {
                self.runtime_commands = resources.commands;
                self.context_files = resources.context_files;
                self.status = "resources reloaded".into();
            }
            AgentEvent::OauthRequest {
                id,
                kind,
                message,
                url,
                user_code,
                verification_uri,
                options,
                ..
            } => {
                if kind == "auth" || kind == "device_code" {
                    let summary = if kind == "auth" {
                        format!(
                            "Open this URL to authenticate:\n{}",
                            url.unwrap_or_default()
                        )
                    } else {
                        format!(
                            "Open {} and enter code {}",
                            verification_uri.unwrap_or_default(),
                            user_code.unwrap_or_default()
                        )
                    };
                    self.entries.push(Entry::Assistant {
                        lines: vec![summary],
                        timestamp: String::new(),
                    });
                } else {
                    self.pending_oauth = Some(PendingOauth {
                        id,
                        message: message.unwrap_or_else(|| "OAuth input".into()),
                        options: options.unwrap_or_default(),
                    });
                    self.open_overlay(if kind == "select" {
                        OverlayKind::OauthSelect
                    } else {
                        OverlayKind::OauthPrompt
                    });
                }
            }
            AgentEvent::OauthComplete { provider } => {
                self.status = format!("logged in to {provider}");
                self.close_overlay();
            }
            AgentEvent::RewindList { checkpoints } => {
                self.rewind_checkpoints = checkpoints;
                self.open_overlay(OverlayKind::RewindPicker);
            }
            AgentEvent::PlanUpdate { entries } => {
                self.tasks_total = entries.len();
                self.tasks_complete = entries
                    .iter()
                    .filter(|entry| entry.status == "completed")
                    .count();
                self.plan_entries = entries;
                self.status = "plan updated".into();
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
                self.begin_turn_if_needed();
                let added = text.chars().count() as u64;
                self.apply_text_delta(text);
                self.turn_output_chars = self.turn_output_chars.saturating_add(added);
                self.status = "generating…".into();
                self.streaming = true;
            }
            AgentEvent::ReasoningDelta { text } => {
                self.begin_turn_if_needed();
                let added = text.chars().count() as u64;
                self.append_reasoning_text(&text);
                self.turn_output_chars = self.turn_output_chars.saturating_add(added);
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
                        id,
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
                result: ToolResult { content, details },
                is_error,
                duration_ms,
            } => {
                if let Some(Entry::Diff { lines, .. }) = self.entries.iter_mut().rev().find(
                    |entry| matches!(entry, Entry::Diff { id: diff_id, .. } if diff_id == &id),
                ) {
                    if !is_error && let Some(diff) = result_diff(details.as_ref()) {
                        *lines = parse_unified_diff(diff);
                    }
                    self.status = if is_error {
                        "tool failed"
                    } else {
                        "tool complete"
                    }
                    .into();
                    return;
                }
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
                self.turn_input_tokens = input_tokens;
                self.turn_output_chars = 0;
                self.turn_started_at = None;
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
                self.turn_started_at = None;
            }
            AgentEvent::Compaction {
                phase,
                reason,
                summary,
                tokens_before,
                tokens_after,
                error,
            } => {
                apply_compaction(
                    self,
                    phase,
                    reason,
                    summary,
                    tokens_before,
                    tokens_after,
                    error,
                );
            }
            AgentEvent::CompactionIndicator {
                reason,
                tokens_before,
            } => {
                self.entries.push(Entry::CompactionIndicator {
                    reason,
                    tokens_before,
                });
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

fn builtin_description(command: &str) -> &'static str {
    match command {
        "/model" => "Switch model",
        "/resume" => "Resume a saved session",
        "/new" => "Start a new session",
        "/name" => "Name the current session",
        "/session" => "Show session statistics",
        "/clone" => "Clone the active branch",
        "/tree" => "Navigate session history",
        "/fork" => "Fork from an earlier prompt",
        "/thinking" => "Cycle thinking level",
        "/context" => "Show loaded context files",
        "/reload" => "Reload Pi resources",
        "/scoped-models" => "Choose models for cycling",
        "/trust" => "Save project trust decision",
        "/export" => "Export session to HTML",
        "/import" => "Import a JSONL session",
        "/copy" => "Copy last assistant message",
        "/login" => "Log in to an OAuth provider",
        "/rewind" => "Restore a file edit checkpoint",
        "/plan" => "Enter Plan mode",
        "/trace" => "Export session trace archive",
        "/mode" => "Cycle permission mode",
        "/settings" => "Open settings",
        "/clear" => "Clear visible conversation",
        "/compact" => "Compact context",
        "/help" => "Show commands",
        "/quit" => "Quit pi-shell",
        _ => "Built-in command",
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
        "search" | "grep" | "web_search" => "Search".to_string(),
        "web_fetch" => "Fetch".to_string(),
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
    if let Some(edits) = object.get("edits").and_then(serde_json::Value::as_array) {
        let mut lines = Vec::new();
        for edit in edits {
            let edit = edit.as_object()?;
            let old = edit.get("oldText").and_then(serde_json::Value::as_str)?;
            let new = edit.get("newText").and_then(serde_json::Value::as_str)?;
            lines.extend(replacement_diff(old, new, None));
        }
        return (!lines.is_empty()).then_some(lines);
    }
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
    Some(replacement_diff(old_text, new_text, start))
}

fn replacement_diff(old_text: &str, new_text: &str, start: Option<u32>) -> Vec<DiffLine> {
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
    diff_lines
}

fn result_diff(details: Option<&serde_json::Value>) -> Option<&str> {
    let details = details?.as_object()?;
    details
        .get("diff")
        .or_else(|| details.get("patch"))
        .and_then(serde_json::Value::as_str)
}

fn parse_unified_diff(diff: &str) -> Vec<DiffLine> {
    let mut old_line = 0_u32;
    let mut new_line = 0_u32;
    let mut lines = Vec::new();
    for line in diff.lines() {
        if let Some(header) = line.strip_prefix("@@ ") {
            let mut ranges = header.split_whitespace();
            old_line = ranges.next().and_then(parse_diff_range).unwrap_or(0);
            new_line = ranges.next().and_then(parse_diff_range).unwrap_or(0);
        } else if line.starts_with("---") || line.starts_with("+++") {
            continue;
        } else if let Some(text) = line.strip_prefix('-') {
            lines.push(DiffLine {
                number: Some(old_line),
                text: text.into(),
                kind: DiffKind::Removed,
            });
            old_line = old_line.saturating_add(1);
        } else if let Some(text) = line.strip_prefix('+') {
            lines.push(DiffLine {
                number: Some(new_line),
                text: text.into(),
                kind: DiffKind::Added,
            });
            new_line = new_line.saturating_add(1);
        } else if let Some(text) = line.strip_prefix(' ') {
            lines.push(DiffLine {
                number: Some(new_line),
                text: text.into(),
                kind: DiffKind::Context,
            });
            old_line = old_line.saturating_add(1);
            new_line = new_line.saturating_add(1);
        }
    }
    lines
}

fn parse_diff_range(range: &str) -> Option<u32> {
    range
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()?
        .parse()
        .ok()
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

fn apply_compaction(
    state: &mut AppState,
    phase: pi_harness::CompactionPhase,
    reason: Option<String>,
    summary: Option<String>,
    tokens_before: Option<u64>,
    tokens_after: Option<u64>,
    error: Option<String>,
) {
    let reason_label = reason
        .as_deref()
        .map(|value| match value {
            "manual" => "manual",
            "threshold" => "auto (threshold)",
            "overflow" => "auto (overflow)",
            "branch" => "branch summary",
            other => other,
        })
        .unwrap_or("compaction");
    match phase {
        pi_harness::CompactionPhase::Start => {
            state.entries.push(Entry::Compaction {
                summary: format!("Compacting context ({reason_label})…"),
                tokens_before,
                tokens_after: None,
                active: true,
                error: None,
                started_at: Some(Instant::now()),
            });
            state.status = "compacting…".into();
        }
        pi_harness::CompactionPhase::End => {
            let final_summary = summary.unwrap_or_else(|| match &error {
                Some(message) => format!("Compaction failed: {message}"),
                None => "Compaction finished without a summary.".to_string(),
            });
            let latest_error = error.clone();
            if let Some(Entry::Compaction {
                summary: slot,
                tokens_after: slot_after,
                active,
                error: slot_error,
                started_at: slot_started_at,
                ..
            }) = state
                .entries
                .iter_mut()
                .rev()
                .find(|entry| matches!(entry, Entry::Compaction { active: true, .. }))
            {
                *slot = final_summary;
                *slot_after = tokens_after;
                *slot_error = error;
                *active = false;
                *slot_started_at = None;
            } else {
                state.entries.push(Entry::Compaction {
                    summary: final_summary,
                    tokens_before,
                    tokens_after,
                    active: false,
                    error,
                    started_at: None,
                });
            }
            if let Some(after) = tokens_after {
                state.context_used = after;
            }
            state.status = if latest_error.is_some() {
                "compaction failed".into()
            } else {
                "compacted".into()
            };
        }
    }
}
