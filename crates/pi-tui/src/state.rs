use pi_harness::{
    AgentEvent, AppUpdateStatus, AuthProviderInfo, AuthType, ModelInfo, PermissionDecision,
    RuntimeCommand, RuntimeSettings, SessionInfo, SessionTreeEntry, SubagentTask, ToolResult,
    Usage, WorkflowArtifactSnapshot, WorkflowCatalogEntry, WorkflowPreview, WorkflowPreviewStep,
    WorkflowRunSnapshot,
};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    time::Instant,
};

use crate::theme::{Theme, ThemeMode};

pub type TranscriptHitRegion = (String, usize, u16, u16, bool);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollDrag {
    pub start_row: u16,
    pub start_from_bottom: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DashboardActions {
    pub delete: bool,
    pub stop: bool,
    pub close: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CacheMissNotice {
    pub missed_tokens: u64,
    pub missed_cost: f64,
    pub idle_ms: u64,
    pub model_changed: bool,
}
const PERMISSION_OPTIONS: &[&str] = &["Allow once", "Always allow", "Deny"];
const SLASH_COMMANDS: &[&str] = &[
    "/dashboard",
    "/home",
    "/welcome",
    "/workflow",
    "/workflows",
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
    "/paste",
    "/scoped-models",
    "/subagent-model",
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum View {
    Dashboard,
    BlockViewer,
    Workflows,
    WorkflowArtifact,
    Tasks,
    Subagent,
    #[default]
    Transcript,
}

#[cfg(any())]
fn append_workflow_preview_step_legacy(
    lines: &mut Vec<String>,
    step: &WorkflowPreviewStep,
    number: &str,
    indent: &str,
) {
    lines.push(format!("{indent}{number}. {} [{}]", step.id, step.r#type));
    if let Some(source) = step.source.as_deref() {
        lines.push(format!("{indent}   source={source}"));
    }
    if let Some(scope) = step.parameter_scope.as_deref() {
        lines.push(format!(
            "{indent}   parameters={} [{}]",
            scope,
            step.parameter_keys.join(", ")
        ));
    }
    if step.r#type == "checkpoint" {
        lines.push(format!(
            "{indent}   {}",
            step.description.as_deref().unwrap_or("Manual approval")
        ));
        return;
    }
    if step.r#type == "parallel" {
        lines.push(format!(
            "{indent}   {} read-only member(s)",
            step.children.len()
        ));
        for (index, child) in step.children.iter().enumerate() {
            append_workflow_preview_step(lines, child, &format!("{number}.{}", index + 1), "  ");
        }
        return;
    }
    lines.push(format!(
        "{indent}   role={} · agent={} · model={}",
        step.role.as_deref().unwrap_or("default"),
        step.agent.as_deref().unwrap_or("default"),
        step.model.as_deref().unwrap_or("parent")
    ));
    if let Some(route) = step.model_route.as_deref() {
        lines.push(format!(
            "{indent}   route={} · candidates={}",
            route,
            step.model_candidates
                .as_ref()
                .map(|models| models.join(" -> "))
                .unwrap_or_else(|| "none".into())
        ));
    }
    lines.push(format!(
        "{indent}   capability={}{} · isolation={} · session={}{}",
        step.capability.as_deref().unwrap_or("all"),
        if step.forced_read_only {
            " (forced)"
        } else {
            ""
        },
        step.isolation.as_deref().unwrap_or("none"),
        step.session.as_deref().unwrap_or("ephemeral"),
        step.session_key
            .as_deref()
            .map(|key| format!(" ({key})"))
            .unwrap_or_default()
    ));
    lines.push(format!(
        "{indent}   thinking={} · tools={} · reports={}",
        step.thinking.as_deref().unwrap_or("default"),
        if step.tools.is_empty() {
            "role defaults".into()
        } else {
            step.tools.join(", ")
        },
        step.reports.as_deref().unwrap_or("previous")
    ));
    if let Some(policy) = step.guardrails.as_ref() {
        let limits = [
            policy
                .max_prompt_bytes
                .map(|value| format!("prompt<={value}B")),
            policy
                .max_artifact_bytes
                .map(|value| format!("artifacts<={value}B")),
            policy.max_artifacts.map(|value| format!("count<={value}")),
            policy
                .max_prompt_tokens
                .map(|value| format!("prompt_tokens<={value}")),
            policy
                .max_output_tokens
                .map(|value| format!("output_tokens<={value}")),
            policy
                .max_cache_write_tokens
                .map(|value| format!("cache_write<={value}")),
            policy
                .min_cache_hit_rate
                .map(|value| format!("cache_hit>={:.0}%", value * 100.0)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        lines.push(format!(
            "{indent}   guardrails={} · cache={} · {}",
            policy.on_violation,
            if policy.require_stable_cache_prefix {
                "stable"
            } else {
                "observe"
            },
            if limits.is_empty() {
                "runtime routing only".into()
            } else {
                limits.join(", ")
            }
        ));
        if policy.allowed_models.is_some() || policy.allowed_tools.is_some() {
            lines.push(format!(
                "{indent}   allow models={} · tools={}",
                policy
                    .allowed_models
                    .as_ref()
                    .map(|models| models.join(", "))
                    .unwrap_or_else(|| "any".into()),
                policy
                    .allowed_tools
                    .as_ref()
                    .map(|tools| tools.join(", "))
                    .unwrap_or_else(|| "any".into())
            ));
        }
    }
    if let Some(effects) = step.external_effects.as_ref() {
        lines.push(format!(
            "{indent}   external effects · approved by checkpoint {}",
            effects.approved_by
        ));
    }
    lines.push(format!(
        "{indent}   timeout={}ms · attempts={}{}",
        step.timeout_ms.unwrap_or_default(),
        step.max_attempts.unwrap_or(1),
        if step.retry_on.is_empty() {
            String::new()
        } else {
            format!(
                " · retry={} after {}ms",
                step.retry_on.join("/"),
                step.retry_backoff_ms.unwrap_or_default()
            )
        }
    ));
    if step.output_contract.is_some() || step.condition.is_some() {
        lines.push(format!(
            "{indent}   output={} · when={}",
            step.output_contract.as_deref().unwrap_or("text"),
            step.condition.as_deref().unwrap_or("always")
        ));
    }
}

fn append_workflow_preview_step(
    lines: &mut Vec<String>,
    step: &WorkflowPreviewStep,
    number: &str,
    indent: &str,
) {
    lines.push(format!("{indent}{number}. {} [{}]", step.id, step.r#type));
    if step.r#type == "checkpoint" {
        lines.push(format!(
            "{indent}   {}",
            step.description.as_deref().unwrap_or("Manual approval")
        ));
        return;
    }
    lines.push(format!(
        "{indent}   role={} · model={} · capability={}",
        step.role.as_deref().unwrap_or("default"),
        step.model.as_deref().unwrap_or("parent"),
        step.capability.as_deref().unwrap_or("read-only"),
    ));
    lines.push(format!(
        "{indent}   thinking={} · depends_on={}",
        step.thinking.as_deref().unwrap_or("default"),
        step.reports
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or("none"),
    ));
    for (index, child) in step.children.iter().enumerate() {
        append_workflow_preview_step(lines, child, &format!("{number}.{}", index + 1), "  ");
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayKind {
    None,
    CommandPalette,
    ModelPicker,
    WorkflowPicker,
    WorkflowPreview,
    SessionPicker,
    SessionRename,
    SessionDeleteConfirm,
    TreePicker,
    ForkPicker,
    TreeSummaryPicker,
    TreeSummaryEditor,
    PasteEditor,
    ImageViewer,
    LabelEditor,
    FilePicker,
    ScopedModels,
    Extensions,
    SubagentModelPicker,
    ApiKeyPrompt,
    OauthPrompt,
    OauthSelect,
    LoginProvider,
    ThinkingPicker,
    RewindPicker,
    Settings,
    Permission,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlaySnapshot {
    pub kind: OverlayKind,
    pub query: String,
    pub cursor: usize,
    pub selected: usize,
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
    RefreshSessions,
    LoadWorkflowCatalog,
    PreviewWorkflow {
        workflow: String,
    },
    StartWorkflow {
        workflow: String,
        input: String,
        expected_definition_hash: Option<String>,
    },
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
    RenameSession {
        target: String,
        name: String,
    },
    DeleteSession {
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
    SetTheme(ThemeMode),
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
    SetApiKey {
        provider: String,
        key: String,
    },
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

#[derive(Clone, Debug)]
enum CommandPaletteTarget {
    Action(crate::actions::ActionId),
    Slash(String),
}

#[derive(Clone, Debug)]
struct CommandPaletteEntry {
    target: CommandPaletteTarget,
    label: String,
    description: String,
    key: Option<String>,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SessionSort {
    #[default]
    Threaded,
    Recent,
    Relevance,
}

impl SessionSort {
    fn next(self) -> Self {
        match self {
            Self::Threaded => Self::Recent,
            Self::Recent => Self::Relevance,
            Self::Relevance => Self::Threaded,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Threaded => "Threaded",
            Self::Recent => "Recent",
            Self::Relevance => "Fuzzy",
        }
    }
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
    Plan {
        entries: Vec<pi_harness::PlanEntry>,
        expanded: bool,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasteBlock {
    pub id: u64,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageAttachment {
    pub id: u64,
    pub path: String,
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub mime_type: String,
    pub temporary: bool,
    pub preview_width: u16,
    pub preview_height: u16,
    pub preview_rgba: Vec<u8>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

    pub fn from_wire(value: &str) -> Self {
        match value {
            "plan" => Self::Plan,
            "always_approve" => Self::AlwaysApprove,
            _ => Self::Normal,
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
    pub view: View,
    pub dashboard_selected: usize,
    pub task_selected: usize,
    pub workflow_selected: usize,
    pub inspected_subagent: Option<String>,
    pub subagent_return_view: View,
    pub viewed_entry: Option<usize>,
    pub dashboard_list_rect: Cell<Option<(u16, u16, u16, u16)>>,
    pub task_list_rect: Cell<Option<(u16, u16, u16, u16)>>,
    pub task_list_offset: Cell<usize>,
    pub header_targets: RefCell<Vec<(u8, u16, u16, u16)>>,
    pub header_hover: Option<u8>,
    pub composer_targets: RefCell<Vec<(u8, u16, u16, u16)>>,
    pub composer_hover: Option<u8>,
    pub workflow_widget_rect: Cell<Option<(u16, u16, u16, u16)>>,
    pub workflow_widget_hovered: bool,
    pub paste_hover: Option<u64>,
    pub paste_targets: RefCell<Vec<(u64, u16, u16, u16)>>,
    pub branch: String,
    pub cwd: String,
    /// Absolute working directory used when the dashboard dispatches a new
    /// session. Unlike `cwd`, this is never shortened for display.
    pub workspace_cwd: String,
    pub theme_mode: ThemeMode,
    pub context_used: u64,
    pub context_known: bool,
    pub context_limit: u64,
    pub tasks_complete: usize,
    pub tasks_total: usize,
    pub plan_entries: Vec<pi_harness::PlanEntry>,
    pub entries: Vec<Entry>,
    pub(crate) entry_ids: RefCell<Vec<u64>>,
    pub(crate) next_entry_id: Cell<u64>,
    pub pinned_entry_modes: HashSet<String>,
    pub prompt: String,
    pub cursor: usize,
    pub paste_blocks: Vec<PasteBlock>,
    pub next_paste_id: u64,
    pub image_attachments: Vec<ImageAttachment>,
    pub next_image_id: u64,
    pub focused_image: Option<u64>,
    pub image_hover: Option<u64>,
    pub image_targets: RefCell<Vec<(u64, u16, u16, u16)>>,
    pub pending_image_id: Option<u64>,
    pub image_view_actions: RefCell<Vec<(u8, u16, u16, u16)>>,
    pub image_view_action_hover: Option<u8>,
    pub pending_paste_id: Option<u64>,
    pub overlay_cursor: usize,
    pub paste_editor_preferred_column: Option<usize>,
    pub paste_editor_scroll: Cell<usize>,
    pub paste_editor_follow_cursor: Cell<bool>,
    pub paste_editor_rows: RefCell<Vec<Vec<(usize, usize)>>>,
    pub paste_editor_targets: RefCell<Vec<(u16, u16, usize)>>,
    pub paste_editor_actions: RefCell<Vec<(u8, u16, u16, u16)>>,
    pub paste_editor_action_hover: Option<u8>,
    pub prompt_history: Vec<String>,
    pub history_index: Option<usize>,
    pub placeholder: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub status: String,
    pub app_update: Option<AppUpdateStatus>,
    pub cache_miss_notice: Option<CacheMissNotice>,
    /// Opt-in render diagnostics. `render_fps_tenths` is stored as an integer
    /// so stories and state snapshots remain deterministic and easy to compare.
    pub perf_visible: bool,
    pub render_fps_tenths: u32,
    pub render_time_micros: u64,
    pub streaming: bool,
    /// A locally submitted foreground message that has not yet been
    /// acknowledged by a non-idle runtime event.
    pub submission_pending: bool,
    pub escape_armed_at: Option<Instant>,
    /// Wall-clock time the LLM started the current turn (the first TextDelta
    /// or ReasoningDelta of a turn). Used by the working banner above the
    /// composer to render a ticking elapsed-time counter while the model is
    /// generating. Cleared on TurnComplete, error, or compaction.
    pub turn_started_at: Option<Instant>,
    pub image_processing_started_at: Option<Instant>,
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
    pub multiline_mode: bool,
    pub thinking_levels: Vec<String>,
    pub pending_thinking_picker: bool,
    pub queued_steering: Vec<String>,
    pub queued_follow_up: Vec<String>,
    pub queue_visible: bool,
    pub scroll_from_bottom: usize,
    pub focus: Focus,
    pub inside_think_tag: bool,
    pub overlay: OverlayKind,
    pub overlay_stack: Vec<OverlaySnapshot>,
    pub overlay_query: String,
    pub overlay_selected: usize,
    pub overlay_hovered: Option<usize>,
    pub overlay_close_hovered: bool,
    pub pending_permission: Option<PendingPermission>,
    pub pending_oauth: Option<PendingOauth>,
    pub pending_api_key_provider: Option<String>,
    pub available_models: Vec<ModelInfo>,
    pub available_auth_providers: Vec<AuthProviderInfo>,
    pub available_sessions: Vec<SessionInfo>,
    pub runtime_sessions: HashMap<String, pi_harness::RuntimeSessionInfo>,
    pub subagent_tasks: HashMap<String, SubagentTask>,
    pub subagent_transcripts: HashMap<String, Vec<AgentEvent>>,
    pub active_subagent_task_ids: Vec<String>,
    pub workflow_runs: HashMap<String, WorkflowRunSnapshot>,
    pub workflow_catalog: Vec<WorkflowCatalogEntry>,
    pub workflow_preview: Option<WorkflowPreview>,
    pub workflow_artifact: Option<WorkflowArtifactSnapshot>,
    pub session_sort: SessionSort,
    pub session_named_only: bool,
    pub session_show_path: bool,
    pub pending_session_path: Option<String>,
    pub available_files: Vec<String>,
    pub runtime_commands: Vec<RuntimeCommand>,
    pub context_files: Vec<String>,
    pub runtime_extensions: Vec<pi_harness::RuntimeExtension>,
    pub runtime_settings: RuntimeSettings,
    pub session_tree: Vec<SessionTreeEntry>,
    pub rewind_checkpoints: Vec<pi_harness::RewindCheckpoint>,
    pub tree_filter: TreeFilter,
    pub tree_show_timestamps: bool,
    pub tree_folded: HashSet<String>,
    pub pending_tree_entry: Option<String>,
    pub pending_label_entry: Option<String>,
    pub expanded_tool_groups: HashSet<usize>,
    pub focused_tool: Option<usize>,
    pub focused_entry: Option<usize>,
    pub focused_section: Option<usize>,
    pub focused_target_id: Option<String>,
    pub hovered_entry: Option<usize>,
    pub hovered_target_id: Option<String>,
    pub transcript_rect: Cell<Option<(u16, u16, u16, u16)>>,
    pub transcript_scrollbar_rect: Cell<Option<(u16, u16, u16, u16)>>,
    pub transcript_hit_regions: RefCell<Vec<TranscriptHitRegion>>,
    pub scroll_drag: Option<ScrollDrag>,
    pub scrollbar_dragging: bool,
    pub pending_transcript_click: Option<(usize, String)>,
}

fn parse_workflow_start_argument(value: &str) -> Result<(String, String), ()> {
    let mut parts = value.splitn(2, char::is_whitespace);
    let workflow = parts.next().unwrap_or_default().trim();
    let remainder = parts.next().unwrap_or_default().trim();
    if workflow.is_empty() || remainder.is_empty() {
        return Err(());
    }
    if remainder.starts_with("--params ") {
        return Err(());
    }
    Ok((workflow.into(), remainder.into()))
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            view: View::Transcript,
            dashboard_selected: 0,
            task_selected: 0,
            workflow_selected: 0,
            inspected_subagent: None,
            subagent_return_view: View::Transcript,
            viewed_entry: None,
            dashboard_list_rect: Cell::new(None),
            task_list_rect: Cell::new(None),
            task_list_offset: Cell::new(0),
            header_targets: RefCell::new(Vec::new()),
            header_hover: None,
            composer_targets: RefCell::new(Vec::new()),
            composer_hover: None,
            workflow_widget_rect: Cell::new(None),
            workflow_widget_hovered: false,
            paste_hover: None,
            paste_targets: RefCell::new(Vec::new()),
            branch: "torii".into(),
            cwd: "~/dev/torii".into(),
            workspace_cwd: String::new(),
            theme_mode: ThemeMode::Dark,
            context_used: 0,
            context_known: false,
            context_limit: 200_000,
            tasks_complete: 0,
            tasks_total: 0,
            plan_entries: Vec::new(),
            entries: Vec::new(),
            entry_ids: RefCell::new(Vec::new()),
            next_entry_id: Cell::new(1),
            pinned_entry_modes: HashSet::new(),
            prompt: String::new(),
            cursor: 0,
            paste_blocks: Vec::new(),
            next_paste_id: 1,
            image_attachments: Vec::new(),
            next_image_id: 1,
            focused_image: None,
            image_hover: None,
            image_targets: RefCell::new(Vec::new()),
            pending_image_id: None,
            image_view_actions: RefCell::new(Vec::new()),
            image_view_action_hover: None,
            pending_paste_id: None,
            overlay_cursor: 0,
            paste_editor_preferred_column: None,
            paste_editor_scroll: Cell::new(0),
            paste_editor_follow_cursor: Cell::new(true),
            paste_editor_rows: RefCell::new(Vec::new()),
            paste_editor_targets: RefCell::new(Vec::new()),
            paste_editor_actions: RefCell::new(Vec::new()),
            paste_editor_action_hover: None,
            prompt_history: Vec::new(),
            history_index: None,
            placeholder: "Ask anything…".into(),
            model: "Mock model".into(),
            permission_mode: PermissionMode::Normal,
            status: "idle".into(),
            app_update: None,
            cache_miss_notice: None,
            perf_visible: false,
            render_fps_tenths: 0,
            render_time_micros: 0,
            streaming: false,
            submission_pending: false,
            escape_armed_at: None,
            turn_started_at: None,
            image_processing_started_at: None,
            turn_input_tokens: 0,
            turn_output_chars: 0,
            thinking_level: "off".into(),
            multiline_mode: false,
            thinking_levels: Vec::new(),
            pending_thinking_picker: false,
            queued_steering: Vec::new(),
            queued_follow_up: Vec::new(),
            queue_visible: true,
            scroll_from_bottom: 0,
            focus: Focus::Prompt,
            inside_think_tag: false,
            overlay: OverlayKind::None,
            overlay_stack: Vec::new(),
            overlay_query: String::new(),
            overlay_selected: 0,
            overlay_hovered: None,
            overlay_close_hovered: false,
            pending_permission: None,
            pending_oauth: None,
            pending_api_key_provider: None,
            available_models: vec![ModelInfo {
                id: "mock".into(),
                display_name: "Mock model".into(),
                context_window: Some(200_000),
            }],
            available_auth_providers: Vec::new(),
            available_sessions: Vec::new(),
            runtime_sessions: HashMap::new(),
            subagent_tasks: HashMap::new(),
            subagent_transcripts: HashMap::new(),
            active_subagent_task_ids: Vec::new(),
            workflow_runs: HashMap::new(),
            workflow_catalog: Vec::new(),
            workflow_preview: None,
            workflow_artifact: None,
            session_sort: SessionSort::default(),
            session_named_only: false,
            session_show_path: false,
            pending_session_path: None,
            available_files: Vec::new(),
            runtime_commands: Vec::new(),
            context_files: Vec::new(),
            runtime_extensions: Vec::new(),
            runtime_settings: RuntimeSettings::default(),
            session_tree: Vec::new(),
            rewind_checkpoints: Vec::new(),
            tree_filter: TreeFilter::default(),
            tree_show_timestamps: false,
            tree_folded: HashSet::new(),
            pending_tree_entry: None,
            pending_label_entry: None,
            expanded_tool_groups: HashSet::new(),
            focused_tool: None,
            focused_entry: None,
            focused_section: None,
            focused_target_id: None,
            hovered_entry: None,
            hovered_target_id: None,
            transcript_rect: Cell::new(None),
            transcript_scrollbar_rect: Cell::new(None),
            transcript_hit_regions: RefCell::new(Vec::new()),
            scroll_drag: None,
            scrollbar_dragging: false,
            pending_transcript_click: None,
        }
    }
}

impl AppState {
    pub const fn theme(&self) -> Theme {
        Theme::for_mode(self.theme_mode)
    }

    pub fn header_target_at(&self, column: u16, row: u16) -> Option<u8> {
        self.header_targets
            .borrow()
            .iter()
            .find(|(_, start, end, target_row)| {
                row == *target_row && column >= *start && column < *end
            })
            .map(|(kind, _, _, _)| *kind)
    }

    pub fn show_context_info(&mut self) {
        let mut lines = vec![format!(
            "Context: {} / {} tokens",
            self.context_used, self.context_limit
        )];
        if self.context_files.is_empty() {
            lines.push("No Pi context files loaded".into());
        } else {
            lines.push("Loaded Pi context files:".into());
            lines.extend(self.context_files.iter().map(|path| format!("• {path}")));
        }
        self.entries.push(Entry::Assistant {
            lines,
            timestamp: String::new(),
        });
        self.scroll_from_bottom = 0;
        self.status = "context details".into();
    }

    fn command_palette_entries(&self) -> Vec<CommandPaletteEntry> {
        let query = self.overlay_query.trim().to_ascii_lowercase();
        let mut entries = crate::actions::palette(&query)
            .into_iter()
            .map(|action| CommandPaletteEntry {
                target: CommandPaletteTarget::Action(action.id),
                label: action.label.into(),
                description: action.description.into(),
                key: Some(action.primary.display()),
            })
            .collect::<Vec<_>>();
        let mut slash = SLASH_COMMANDS
            .iter()
            .map(|command| {
                (
                    (*command).to_string(),
                    builtin_description(command).to_string(),
                )
            })
            .chain(self.runtime_commands.iter().map(|command| {
                let name = if command.name.starts_with('/') {
                    command.name.clone()
                } else {
                    format!("/{}", command.name)
                };
                (name, command.description.clone())
            }))
            .collect::<Vec<_>>();
        slash.sort_by(|left, right| left.0.cmp(&right.0));
        slash.dedup_by(|left, right| left.0 == right.0);
        entries.extend(
            slash
                .into_iter()
                .filter(|(label, description)| {
                    query.is_empty()
                        || label.to_ascii_lowercase().contains(&query)
                        || description.to_ascii_lowercase().contains(&query)
                })
                .map(|(label, description)| CommandPaletteEntry {
                    target: CommandPaletteTarget::Slash(label.clone()),
                    label,
                    description,
                    key: None,
                }),
        );
        entries
    }

    pub fn stable_entry_id(&self, index: usize) -> u64 {
        let mut ids = self.entry_ids.borrow_mut();
        while ids.len() <= index {
            let id = self.next_entry_id.get();
            self.next_entry_id.set(id.saturating_add(1));
            ids.push(id);
        }
        ids[index]
    }

    pub fn entry_target_id(&self, index: usize) -> Option<String> {
        let entry = self.entries.get(index)?;
        let id = match entry {
            Entry::Tool { id, .. } => format!("tool:{id}"),
            Entry::Diff { id, .. } => format!("diff:{id}"),
            Entry::User { .. } => format!("user:{}", self.stable_entry_id(index)),
            Entry::Reasoning { .. } => format!("reasoning:{}", self.stable_entry_id(index)),
            Entry::Assistant { .. } => format!("assistant:{}", self.stable_entry_id(index)),
            Entry::Plan { .. } => format!("plan:{}", self.stable_entry_id(index)),
            Entry::Compaction { .. } => format!("compaction:{}", self.stable_entry_id(index)),
            Entry::CompactionIndicator { .. } => {
                format!("compaction-indicator:{}", self.stable_entry_id(index))
            }
        };
        Some(id)
    }

    fn reset_entry_ids(&mut self) {
        self.entry_ids.borrow_mut().clear();
        self.pinned_entry_modes.clear();
    }

    pub fn sorted_workflows(&self) -> Vec<&WorkflowRunSnapshot> {
        let mut workflows: Vec<_> = self.workflow_runs.values().collect();
        workflows.sort_by_key(|workflow| std::cmp::Reverse(workflow.updated_at_ms));
        workflows
    }

    pub fn selected_workflow(&self) -> Option<&WorkflowRunSnapshot> {
        self.sorted_workflows().get(self.workflow_selected).copied()
    }

    pub fn workflow_widget(&self) -> Option<&WorkflowRunSnapshot> {
        let workflows = self.sorted_workflows();
        workflows
            .iter()
            .copied()
            .find(|workflow| {
                matches!(
                    workflow.status.as_str(),
                    "paused" | "failed" | "interrupted"
                )
            })
            .or_else(|| {
                workflows
                    .into_iter()
                    .find(|workflow| matches!(workflow.status.as_str(), "pending" | "running"))
            })
    }

    fn select_workflow(&mut self, run_id: &str) {
        if let Some(index) = self
            .sorted_workflows()
            .iter()
            .position(|workflow| workflow.run_id == run_id)
        {
            self.workflow_selected = index;
        }
    }

    pub fn open_workflow(&mut self, run_id: &str) {
        self.select_workflow(run_id);
        self.workflow_widget_hovered = false;
        self.view = View::Workflows;
    }

    pub fn selected_workflow_control(&self, action: &str) -> Option<(String, Option<String>)> {
        let workflow = self.selected_workflow()?;
        let allowed = match action {
            "approve" | "reject" => workflow.status == "paused",
            "cancel" => matches!(
                workflow.status.as_str(),
                "pending" | "running" | "paused" | "interrupted"
            ),
            "retry" => matches!(workflow.status.as_str(), "failed" | "interrupted"),
            _ => false,
        };
        allowed.then(|| (workflow.run_id.clone(), workflow.current_step.clone()))
    }

    pub fn selected_workflow_artifact(&self) -> Option<(String, String)> {
        let workflow = self.selected_workflow()?;
        workflow
            .artifact_ids
            .last()
            .map(|artifact_id| (workflow.run_id.clone(), artifact_id.clone()))
    }

    pub fn sorted_subagent_tasks(&self) -> Vec<&SubagentTask> {
        let mut tasks: Vec<_> = self.subagent_tasks.values().collect();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.started_at_ms));
        tasks
    }

    pub fn selected_subagent_id(&self) -> Option<String> {
        self.sorted_subagent_tasks()
            .get(self.task_selected)
            .map(|task| task.task_id.clone())
    }

    pub fn open_selected_subagent(&mut self) {
        if let Some(task_id) = self.selected_subagent_id() {
            self.inspected_subagent = Some(task_id);
            self.subagent_return_view = self.view;
            self.view = View::Subagent;
            self.scroll_from_bottom = 0;
        }
    }

    pub fn close_subagent(&mut self) {
        self.view = self.subagent_return_view;
        self.inspected_subagent = None;
        self.scroll_from_bottom = 0;
    }

    pub fn dashboard_selected_path(&self) -> Option<String> {
        self.available_sessions
            .get(self.dashboard_selected)
            .map(|session| session.path.clone())
    }

    pub fn set_workspace_cwd(&mut self, cwd: &std::path::Path) {
        self.workspace_cwd = cwd.display().to_string();
        self.cwd = crate::effects::display_path(cwd);
    }

    pub fn sync_current_session_workspace(&mut self) {
        let cwd = self
            .available_sessions
            .iter()
            .find(|session| session.current)
            .map(|session| session.cwd.trim())
            .filter(|cwd| !cwd.is_empty())
            .map(std::path::PathBuf::from);
        if let Some(cwd) = cwd {
            self.set_workspace_cwd(&cwd);
        }
    }

    pub fn change_workspace(&mut self, value: &str) -> Result<(), String> {
        let value = value.trim();
        if value.is_empty() {
            return Err("usage: /cd <workspace>".into());
        }
        let expanded = if value == "~" || value.starts_with("~/") || value.starts_with("~\\") {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or_else(|| "home directory is unavailable".to_string())?;
            std::path::PathBuf::from(home).join(
                value
                    .trim_start_matches('~')
                    .trim_start_matches(['/', '\\']),
            )
        } else {
            let path = std::path::PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                let base = (!self.workspace_cwd.is_empty())
                    .then(|| std::path::PathBuf::from(&self.workspace_cwd))
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                base.join(path)
            }
        };
        let canonical = expanded
            .canonicalize()
            .map_err(|error| format!("workspace unavailable: {error}"))?;
        if !canonical.is_dir() {
            return Err("workspace must be a directory".into());
        }
        self.set_workspace_cwd(&canonical);
        self.status = format!("workspace: {}", self.cwd);
        Ok(())
    }

    pub fn dashboard_actions(&self) -> DashboardActions {
        let Some(session) = self.available_sessions.get(self.dashboard_selected) else {
            return DashboardActions::default();
        };
        let runtime = self.runtime_sessions.get(&session.path);
        DashboardActions {
            delete: !session.current && runtime.is_none(),
            stop: runtime.is_some_and(|runtime| runtime.status != "idle"),
            close: runtime.is_some(),
        }
    }

    pub fn begin_dashboard_rename(&mut self) {
        let Some(session) = self.available_sessions.get(self.dashboard_selected) else {
            return;
        };
        self.pending_session_path = Some(session.path.clone());
        self.overlay_query = session.name.clone().unwrap_or_default();
        self.overlay_selected = 0;
        self.overlay = OverlayKind::SessionRename;
    }

    pub fn begin_dashboard_delete(&mut self) {
        let Some(session) = self.available_sessions.get(self.dashboard_selected) else {
            return;
        };
        if self.runtime_sessions.contains_key(&session.path) {
            self.status = "cannot delete a resident session; close it first".into();
            return;
        }
        if session.current {
            self.status = "cannot delete the active session; close or switch it first".into();
            return;
        }
        self.pending_session_path = Some(session.path.clone());
        self.overlay_query.clear();
        self.overlay_selected = 0;
        self.overlay = OverlayKind::SessionDeleteConfirm;
    }
    pub fn dashboard_session_at_row(&self, row: usize, visible_rows: usize) -> Option<usize> {
        let selected = self
            .dashboard_selected
            .min(self.available_sessions.len().saturating_sub(1));
        let viewport =
            crate::picker::list_viewport(selected, self.available_sessions.len(), visible_rows);
        let index = viewport.start + row;
        (index < self.available_sessions.len()).then_some(index)
    }

    pub fn activate_dashboard_session(&mut self) -> Option<String> {
        let selected = self.dashboard_selected;
        let path = self.available_sessions.get(selected)?.path.clone();
        let already_current = self.available_sessions[selected].current;
        for (index, session) in self.available_sessions.iter_mut().enumerate() {
            session.current = index == selected;
        }
        (!already_current).then_some(path)
    }

    pub fn set_hovered_transcript_target(&mut self, target: Option<(usize, String)>) -> bool {
        let (entry, target_id) = target
            .map(|(entry, id)| (Some(entry), Some(id)))
            .unwrap_or((None, None));
        if self.hovered_entry == entry && self.hovered_target_id == target_id {
            return false;
        }
        self.hovered_entry = entry;
        self.hovered_target_id = target_id;
        true
    }

    fn slash_token_range(&self) -> Option<(usize, usize, String)> {
        let cursor = self.cursor.min(self.prompt.chars().count());
        let before = self.prompt.chars().take(cursor).collect::<String>();
        let start = before
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
            .map_or(0, |(index, character)| index + character.len_utf8());
        let token = before[start..].to_string();
        let start_chars = before[..start].chars().count();
        token
            .starts_with('/')
            .then_some((start_chars, cursor, token))
    }

    pub fn first_slash_match(&self) -> Option<String> {
        let (_, _, token) = self.slash_token_range()?;
        self.slash_command_names()
            .into_iter()
            .find(|command| command.starts_with(&token.to_ascii_lowercase()))
    }

    pub fn complete_slash_command(&mut self) -> bool {
        let Some(command) = self.first_slash_match() else {
            return false;
        };
        let Some((start, end, _)) = self.slash_token_range() else {
            return false;
        };
        let start_byte = char_to_byte(&self.prompt, start);
        let end_byte = char_to_byte(&self.prompt, end);
        self.prompt.replace_range(start_byte..end_byte, &command);
        self.cursor = start + command.chars().count();
        self.history_index = None;
        true
    }

    pub fn move_slash_selection(&mut self, delta: isize) {
        if self.slash_token_range().is_none() {
            return;
        }
        let count = self.slash_suggestions().len();
        if count == 0 {
            self.overlay_selected = 0;
            return;
        }
        self.overlay_selected = if delta < 0 {
            self.overlay_selected
                .checked_sub(delta.unsigned_abs())
                .unwrap_or(count - 1)
        } else {
            self.overlay_selected.saturating_add(delta as usize) % count
        };
    }

    pub fn activate_slash_command(&mut self) -> Option<OverlayAction> {
        if let Some((start, end, token)) = self.slash_token_range() {
            let suggestions = self.slash_suggestions();
            let exact = suggestions
                .iter()
                .any(|(command, _)| command == &token.to_ascii_lowercase());
            if !exact {
                if let Some((command, _)) = suggestions.get(self.overlay_selected) {
                    let start_byte = char_to_byte(&self.prompt, start);
                    let end_byte = char_to_byte(&self.prompt, end);
                    self.prompt.replace_range(start_byte..end_byte, command);
                } else {
                    return None;
                }
            }

            // A slash command is executable wherever it appears in the draft.
            // Drop prose before the command so `something /model` opens the
            // model picker instead of being sent as an ordinary prompt. Keep
            // any text after the command as its command arguments.
            let command_start = char_to_byte(&self.prompt, start);
            let command_input = self.prompt[command_start..].trim().to_string();
            self.prompt = command_input;
            self.cursor = self.prompt.chars().count();
        }
        let input = self.prompt.trim();
        let mut parts = input.splitn(2, char::is_whitespace);
        let command = parts.next().unwrap_or_default().to_ascii_lowercase();
        let argument = parts
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let action = match command.as_str() {
            "/dashboard" | "/home" | "/welcome" => {
                self.view = View::Dashboard;
                self.dashboard_selected = self
                    .available_sessions
                    .iter()
                    .position(|session| session.current)
                    .unwrap_or(0);
                OverlayAction::RefreshSessions
            }
            "/workflows" => {
                self.view = View::Workflows;
                self.workflow_selected = self
                    .workflow_selected
                    .min(self.workflow_runs.len().saturating_sub(1));
                OverlayAction::None
            }
            "/workflow" => {
                if argument.is_none() {
                    self.clear_prompt();
                    return Some(OverlayAction::LoadWorkflowCatalog);
                }
                if let Some(workflow) = argument
                    .and_then(|value| value.strip_prefix("check "))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                {
                    self.clear_prompt();
                    self.status = format!("checking workflow {workflow}…");
                    return Some(OverlayAction::PreviewWorkflow { workflow });
                }
                let parsed = argument.and_then(|value| parse_workflow_start_argument(value).ok());
                if let Some((workflow, input)) = parsed {
                    let expected_definition_hash = self
                        .workflow_preview
                        .as_ref()
                        .filter(|preview| preview.name == workflow)
                        .map(|preview| preview.definition_hash.clone());
                    self.workflow_preview = None;
                    OverlayAction::StartWorkflow {
                        workflow,
                        input,
                        expected_definition_hash,
                    }
                } else {
                    self.push_command_usage("Usage: /workflow <name> <task>");
                    OverlayAction::None
                }
            }
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
            "/name" => {
                self.push_command_usage("Usage: /name <session name>");
                OverlayAction::None
            }
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
            "/subagent-model" => {
                self.open_overlay(OverlayKind::SubagentModelPicker);
                OverlayAction::None
            }
            "/trust" => OverlayAction::SetProjectTrust(!self.runtime_settings.project_trusted),
            "/export" => OverlayAction::ExportSession(argument.map(str::to_string)),
            "/import" if argument.is_some() => {
                OverlayAction::ImportSession(argument.unwrap().to_string())
            }
            "/import" => {
                self.push_command_usage("Usage: /import <session.jsonl>");
                OverlayAction::None
            }
            "/copy" => OverlayAction::CopyLast,
            "/login" if argument.is_some() => {
                let id = argument.unwrap();
                let provider = self
                    .available_auth_providers
                    .iter()
                    .find(|provider| provider.id == id)
                    .cloned();
                if let Some(provider) = provider {
                    self.start_auth(provider)
                } else {
                    self.push_command_usage(&format!("Unknown auth provider: {id}"));
                    OverlayAction::None
                }
            }
            "/login" => {
                self.open_overlay(OverlayKind::LoginProvider);
                OverlayAction::None
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
                self.reset_entry_ids();
                self.subagent_tasks.clear();
                self.subagent_transcripts.clear();
                self.workflow_runs.clear();
                self.workflow_artifact = None;
                self.context_used = 0;
                self.context_known = false;
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

    fn push_command_usage(&mut self, message: &str) {
        self.entries.push(Entry::Assistant {
            lines: vec![message.into()],
            timestamp: String::new(),
        });
    }

    fn slash_command_names(&self) -> Vec<String> {
        SLASH_COMMANDS
            .iter()
            .map(|command| (*command).to_string())
            .chain(
                self.runtime_commands
                    .iter()
                    .map(|command| command.name.clone()),
            )
            .collect()
    }

    pub fn is_valid_slash_command(&self, token: &str) -> bool {
        self.slash_command_names()
            .iter()
            .any(|command| command == token)
    }

    pub fn slash_suggestions(&self) -> Vec<(String, String)> {
        let Some((_, _, token)) = self.slash_token_range() else {
            return Vec::new();
        };
        let query = token.to_ascii_lowercase();
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
            .filter(|(name, _)| name.starts_with(&query))
            .collect()
    }

    pub fn open_overlay(&mut self, overlay: OverlayKind) {
        self.scroll_drag = None;
        self.scrollbar_dragging = false;
        self.pending_transcript_click = None;
        self.overlay = overlay;
        self.overlay_stack.clear();
        self.overlay_query.clear();
        self.overlay_cursor = 0;
        self.overlay_selected = 0;
        self.overlay_hovered = None;
        self.overlay_close_hovered = false;
    }

    pub fn open_child_overlay(&mut self, overlay: OverlayKind) {
        self.overlay_stack.push(OverlaySnapshot {
            kind: self.overlay,
            query: std::mem::take(&mut self.overlay_query),
            cursor: self.overlay_cursor,
            selected: self.overlay_selected,
        });
        self.overlay = overlay;
        self.overlay_cursor = 0;
        self.overlay_selected = 0;
        self.overlay_hovered = None;
        self.overlay_close_hovered = false;
    }

    pub fn close_overlay(&mut self) {
        if self.overlay != OverlayKind::Permission {
            if let Some(parent) = self.overlay_stack.pop() {
                self.overlay = parent.kind;
                self.overlay_query = parent.query;
                self.overlay_cursor = parent.cursor;
                self.overlay_selected = parent.selected;
            } else {
                self.overlay = OverlayKind::None;
                self.overlay_query.clear();
                self.overlay_cursor = 0;
                self.overlay_selected = 0;
            }
            self.pending_paste_id = None;
            self.pending_image_id = None;
            self.overlay_hovered = None;
            self.overlay_close_hovered = false;
        }
    }

    pub fn cancel_oauth(&mut self) -> OverlayAction {
        if matches!(
            self.overlay,
            OverlayKind::SessionRename | OverlayKind::SessionDeleteConfirm
        ) {
            self.pending_session_path = None;
            if self.view == View::Dashboard {
                self.close_overlay();
            } else {
                self.open_overlay(OverlayKind::SessionPicker);
            }
            return OverlayAction::None;
        }
        if matches!(
            self.overlay,
            OverlayKind::TreeSummaryPicker | OverlayKind::TreeSummaryEditor
        ) {
            let selected = self.pending_tree_entry.clone();
            self.overlay = OverlayKind::TreePicker;
            self.overlay_query.clear();
            let entries = self.filtered_tree();
            self.overlay_selected = selected
                .as_deref()
                .and_then(|id| entries.iter().position(|entry| entry.id == id))
                .unwrap_or_else(|| entries.len().saturating_sub(1));
            return OverlayAction::None;
        }
        if !matches!(
            self.overlay,
            OverlayKind::OauthPrompt | OverlayKind::OauthSelect
        ) {
            if self.overlay == OverlayKind::ApiKeyPrompt {
                self.pending_api_key_provider = None;
                self.overlay_query.clear();
            }
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

    fn start_auth(&mut self, provider: AuthProviderInfo) -> OverlayAction {
        match provider.auth_type {
            AuthType::Oauth => {
                self.close_overlay();
                self.status = format!("starting OAuth for {}…", provider.id);
                OverlayAction::BeginOauth(provider.id)
            }
            AuthType::ApiKey => {
                self.pending_api_key_provider = Some(provider.id);
                self.open_overlay(OverlayKind::ApiKeyPrompt);
                OverlayAction::None
            }
        }
    }

    pub fn overlay_items(&self) -> Vec<String> {
        let source = match self.overlay {
            OverlayKind::CommandPalette => self
                .command_palette_entries()
                .into_iter()
                .map(|entry| {
                    if let Some(key) = entry.key {
                        format!("{}  ·  {key}  ·  {}", entry.label, entry.description)
                    } else {
                        format!("{}  ·  {}", entry.label, entry.description)
                    }
                })
                .collect(),
            OverlayKind::ModelPicker => self
                .filtered_models()
                .into_iter()
                .map(|model| model.display_name.clone())
                .collect(),
            OverlayKind::WorkflowPicker => self
                .filtered_workflows()
                .into_iter()
                .map(|workflow| {
                    let description = workflow
                        .description
                        .as_deref()
                        .or(workflow.error.as_deref())
                        .unwrap_or("No description");
                    let invalid = if workflow.valid { "" } else { " [invalid]" };
                    format!(
                        "{}  · {}{}  · {}",
                        workflow.name, workflow.source, invalid, description
                    )
                })
                .collect(),
            OverlayKind::WorkflowPreview => self.workflow_preview_items(),
            OverlayKind::SessionPicker => self
                .filtered_sessions()
                .into_iter()
                .map(|session| self.session_picker_label(session))
                .collect(),
            OverlayKind::SessionRename | OverlayKind::SessionDeleteConfirm => Vec::new(),
            OverlayKind::TreePicker | OverlayKind::ForkPicker => self
                .filtered_tree()
                .into_iter()
                .map(|entry| tree_label(entry, self.tree_show_timestamps))
                .collect(),
            OverlayKind::TreeSummaryPicker => vec![
                "No summary".into(),
                "Summarize".into(),
                "Summarize with custom prompt".into(),
            ],
            OverlayKind::TreeSummaryEditor
            | OverlayKind::PasteEditor
            | OverlayKind::ImageViewer => Vec::new(),
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
                    "Cache miss notices: {}",
                    if self.runtime_settings.show_cache_miss_notices {
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
                format!("Pi extensions: {}", self.runtime_extensions.len()),
                format!(
                    "Scoped models: {}",
                    self.runtime_settings.enabled_models.len()
                ),
                format!(
                    "Subagent model: {}",
                    self.runtime_settings
                        .subagent_model
                        .as_deref()
                        .unwrap_or("inherit parent")
                ),
                format!("Theme: {}", self.theme_mode.label()),
            ],
            OverlayKind::Extensions => self
                .filtered_extensions()
                .into_iter()
                .map(|extension| {
                    format!(
                        "[{}] {}",
                        if extension.enabled { "✓" } else { " " },
                        extension.label
                    )
                })
                .collect(),
            OverlayKind::ScopedModels => self
                .filtered_models()
                .into_iter()
                .map(|model| {
                    let mark = if self.runtime_settings.enabled_models.contains(&model.id) {
                        "✓"
                    } else {
                        " "
                    };
                    format!("[{mark}] {}", model.display_name)
                })
                .collect(),
            OverlayKind::SubagentModelPicker => {
                let query = self.overlay_query.to_ascii_lowercase();
                let inherit = "Inherit parent (default)";
                let mut items = Vec::new();
                if query.is_empty() || inherit.to_ascii_lowercase().contains(&query) {
                    items.push(inherit.into());
                }
                items.extend(
                    self.filtered_models()
                        .into_iter()
                        .map(|model| model.display_name.clone()),
                );
                items
            }
            OverlayKind::ApiKeyPrompt | OverlayKind::OauthPrompt => Vec::new(),
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
            OverlayKind::LoginProvider => self
                .filtered_auth_providers()
                .into_iter()
                .map(auth_provider_label)
                .collect(),
            OverlayKind::ThinkingPicker => self.thinking_levels.clone(),
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
            .filter(|item| {
                matches!(
                    self.overlay,
                    OverlayKind::SessionPicker
                        | OverlayKind::ModelPicker
                        | OverlayKind::WorkflowPicker
                        | OverlayKind::Extensions
                        | OverlayKind::ScopedModels
                        | OverlayKind::SubagentModelPicker
                        | OverlayKind::LoginProvider
                ) || query.is_empty()
                    || item.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    pub(crate) fn filtered_models(&self) -> Vec<&ModelInfo> {
        let query = self.overlay_query.to_ascii_lowercase();
        let mut models = self
            .available_models
            .iter()
            .filter(|model| {
                query.is_empty()
                    || model.display_name.to_ascii_lowercase().contains(&query)
                    || model.id.to_ascii_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        models.sort_by(|left, right| {
            model_provider(left)
                .cmp(model_provider(right))
                .then_with(|| left.display_name.cmp(&right.display_name))
        });
        models
    }

    fn filtered_auth_providers(&self) -> Vec<&AuthProviderInfo> {
        let query = self.overlay_query.to_ascii_lowercase();
        self.available_auth_providers
            .iter()
            .filter(|provider| {
                query.is_empty()
                    || provider.display_name.to_ascii_lowercase().contains(&query)
                    || provider.id.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    pub(crate) fn filtered_extensions(&self) -> Vec<&pi_harness::RuntimeExtension> {
        let query = self.overlay_query.trim().to_ascii_lowercase();
        self.runtime_extensions
            .iter()
            .filter(|extension| {
                query.is_empty()
                    || extension.label.to_ascii_lowercase().contains(&query)
                    || extension.path.to_ascii_lowercase().contains(&query)
                    || extension.source.to_ascii_lowercase().contains(&query)
            })
            .collect()
    }

    pub(crate) fn filtered_workflows(&self) -> Vec<&WorkflowCatalogEntry> {
        let query = self.overlay_query.to_ascii_lowercase();
        self.workflow_catalog
            .iter()
            .filter(|workflow| {
                query.is_empty()
                    || workflow.name.to_ascii_lowercase().contains(&query)
                    || workflow.source.to_ascii_lowercase().contains(&query)
                    || workflow
                        .description
                        .as_deref()
                        .is_some_and(|value| value.to_ascii_lowercase().contains(&query))
                    || workflow
                        .error
                        .as_deref()
                        .is_some_and(|value| value.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    fn workflow_preview_items(&self) -> Vec<String> {
        let Some(preview) = self.workflow_preview.as_ref() else {
            return vec!["No resolved workflow available".into()];
        };
        let mut lines = vec![
            preview
                .description
                .clone()
                .unwrap_or_else(|| "No description".into()),
            format!(
                "Definition {}",
                &preview.definition_hash[..preview.definition_hash.len().min(12)]
            ),
            format!("Readiness: {}", preview.readiness.status),
        ];
        #[cfg(any())]
        if let Some(budget) = preview.budget.as_ref() {
            let limits = [
                budget
                    .max_agent_attempts
                    .map(|value| format!("attempts<={value}")),
                budget
                    .max_prompt_tokens
                    .map(|value| format!("prompt<={value}")),
                budget
                    .max_output_tokens
                    .map(|value| format!("output<={value}")),
                budget
                    .max_cache_write_tokens
                    .map(|value| format!("cache_write<={value}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            lines.push(format!("Workflow budget: {}", limits.join(" Â· ")));
        }
        #[cfg(any())]
        for policy in &preview.provider_policies {
            let limits = [
                policy
                    .max_concurrency
                    .map(|value| format!("concurrency<={value}")),
                policy
                    .max_starts
                    .zip(policy.window_ms)
                    .map(|(starts, window)| format!("rate<={starts}/{window}ms")),
                policy
                    .failure_threshold
                    .zip(policy.cooldown_ms)
                    .map(|(failures, cooldown)| {
                        format!("circuit={failures} failures/{cooldown}ms")
                    }),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            lines.push(format!(
                "Provider {}: {}",
                policy.provider,
                limits.join(" Â· ")
            ));
        }
        #[cfg(any())]
        for component in &preview.components {
            lines.push(format!(
                "Component {}: {}{} Â· definition {}{}",
                component.invocation,
                component.workflow,
                component
                    .version
                    .as_ref()
                    .map(|version| format!("@{version}"))
                    .unwrap_or_default(),
                &component.definition_hash[..component.definition_hash.len().min(12)],
                component
                    .parameter_binding_hash
                    .as_deref()
                    .map(|hash| format!(" Â· bindings {}", &hash[..hash.len().min(12)]))
                    .unwrap_or_default()
            ));
            if !component.parameter_bindings.is_empty() {
                let mut bindings = component
                    .parameter_bindings
                    .iter()
                    .take(8)
                    .map(|(name, source)| {
                        let source = if source.chars().count() > 120 {
                            format!("{}...", source.chars().take(117).collect::<String>())
                        } else {
                            source.clone()
                        };
                        format!("{name}={source}")
                    })
                    .collect::<Vec<_>>();
                if component.parameter_bindings.len() > bindings.len() {
                    bindings.push(format!(
                        "+{} more",
                        component.parameter_bindings.len() - bindings.len()
                    ));
                }
                lines.push(format!("  parameter map: {}", bindings.join(", ")));
            }
        }
        #[cfg(any())]
        for contract in &preview.contracts {
            lines.push(format!(
                "Contract {}: max {}B Â· schema {}{}",
                contract.name,
                contract.max_bytes,
                contract.schema_hash,
                contract
                    .description
                    .as_deref()
                    .map(|description| format!(" Â· {description}"))
                    .unwrap_or_default()
            ));
        }
        #[cfg(any())]
        if let Some(parameters) = preview.parameters.as_ref() {
            let defaults = parameters.defaults.to_string();
            let defaults = if defaults.chars().count() > 240 {
                format!("{}...", defaults.chars().take(237).collect::<String>())
            } else {
                defaults
            };
            let required = if parameters.required.is_empty() {
                "none".into()
            } else {
                parameters.required.join(", ")
            };
            lines.push(format!(
                "Parameters: max {}B Â· schema {} Â· required {} Â· defaults {}{}",
                parameters.max_bytes,
                parameters.schema_hash,
                required,
                defaults,
                parameters
                    .description
                    .as_deref()
                    .map(|description| format!(" Â· {description}"))
                    .unwrap_or_default()
            ));
        }
        for issue in &preview.readiness.issues {
            lines.push(format!(
                "{} [{}] {}: {}",
                if issue.severity == "blocker" {
                    "BLOCK"
                } else {
                    "WARN"
                },
                issue.code,
                issue.step_id.as_deref().unwrap_or("workflow"),
                issue.message
            ));
        }
        lines.push(if preview.readiness.status == "blocked" {
            "Resolve blockers before starting  ·  Esc: back".into()
        } else {
            "Enter: use this workflow  ·  Esc: back".into()
        });
        for (index, step) in preview.steps.iter().enumerate() {
            append_workflow_preview_step(&mut lines, step, &format!("{}", index + 1), "");
        }
        lines
    }

    pub fn move_overlay_selection(&mut self, delta: isize) {
        self.overlay_hovered = None;
        // Tree rows are rendered directly from SessionTreeEntry. Building
        // overlay_items() here would format every row merely to learn the
        // count, making each arrow-key repeat noticeably expensive on long
        // sessions.
        let count = if matches!(
            self.overlay,
            OverlayKind::TreePicker | OverlayKind::ForkPicker
        ) {
            self.visible_tree_count()
        } else {
            self.overlay_items().len()
        };
        if count == 0 {
            self.overlay_selected = 0;
            return;
        }
        if matches!(
            self.overlay,
            OverlayKind::TreePicker | OverlayKind::ForkPicker
        ) {
            self.overlay_selected = if delta < 0 {
                self.overlay_selected.checked_sub(1).unwrap_or(count - 1)
            } else if self.overlay_selected + 1 >= count {
                0
            } else {
                self.overlay_selected + 1
            };
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
        self.overlay_hovered = None;
        if matches!(
            self.overlay,
            OverlayKind::CommandPalette
                | OverlayKind::ModelPicker
                | OverlayKind::WorkflowPicker
                | OverlayKind::SessionPicker
                | OverlayKind::SessionRename
                | OverlayKind::TreePicker
                | OverlayKind::TreeSummaryEditor
                | OverlayKind::PasteEditor
                | OverlayKind::ImageViewer
                | OverlayKind::LabelEditor
                | OverlayKind::FilePicker
                | OverlayKind::Extensions
                | OverlayKind::ScopedModels
                | OverlayKind::SubagentModelPicker
                | OverlayKind::ApiKeyPrompt
                | OverlayKind::OauthPrompt
                | OverlayKind::OauthSelect
                | OverlayKind::LoginProvider
                | OverlayKind::RewindPicker
        ) {
            if self.overlay == OverlayKind::PasteEditor {
                let byte = char_to_byte(&self.overlay_query, self.overlay_cursor);
                self.overlay_query.insert(byte, character);
                self.overlay_cursor += 1;
            } else {
                self.overlay_query.push(character);
            }
            self.overlay_selected = 0;
            if self.overlay == OverlayKind::TreePicker {
                self.tree_folded.clear();
            }
        }
    }

    pub fn overlay_backspace(&mut self) {
        self.overlay_hovered = None;
        if self.overlay == OverlayKind::PasteEditor {
            if self.overlay_cursor > 0 {
                let start = char_to_byte(&self.overlay_query, self.overlay_cursor - 1);
                let end = char_to_byte(&self.overlay_query, self.overlay_cursor);
                self.overlay_query.replace_range(start..end, "");
                self.overlay_cursor -= 1;
            }
        } else {
            self.overlay_query.pop();
        }
        self.overlay_selected = 0;
        if self.overlay == OverlayKind::TreePicker {
            self.tree_folded.clear();
        }
    }

    pub fn activate_overlay(&mut self) -> OverlayAction {
        if self.overlay == OverlayKind::PasteEditor {
            let Some(id) = self.pending_paste_id else {
                self.close_overlay();
                return OverlayAction::None;
            };
            let replacement = self.overlay_query.clone();
            self.replace_paste(id, replacement);
            self.close_overlay();
            return OverlayAction::None;
        }
        if self.overlay == OverlayKind::SessionRename {
            let Some(target) = self.pending_session_path.take() else {
                return OverlayAction::None;
            };
            let name = self.overlay_query.trim().to_string();
            if name.is_empty() {
                self.pending_session_path = Some(target);
                self.status = "session name cannot be empty".into();
                return OverlayAction::None;
            }
            if self.view == View::Dashboard {
                self.close_overlay();
            } else {
                self.open_overlay(OverlayKind::SessionPicker);
            }
            return OverlayAction::RenameSession { target, name };
        }
        if self.overlay == OverlayKind::SessionDeleteConfirm {
            let Some(target) = self.pending_session_path.take() else {
                return OverlayAction::None;
            };
            if self.view == View::Dashboard {
                self.close_overlay();
            } else {
                self.open_overlay(OverlayKind::SessionPicker);
            }
            return OverlayAction::DeleteSession { target };
        }
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
        if self.overlay == OverlayKind::ApiKeyPrompt {
            let Some(provider) = self.pending_api_key_provider.take() else {
                return OverlayAction::None;
            };
            if self.overlay_query.is_empty() {
                self.pending_api_key_provider = Some(provider);
                self.status = "API key cannot be empty".into();
                return OverlayAction::None;
            }
            let key = std::mem::take(&mut self.overlay_query);
            self.close_overlay();
            self.status = format!("updating credentials for {provider}…");
            return OverlayAction::SetApiKey { provider, key };
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
        if self.overlay == OverlayKind::TreeSummaryEditor {
            let Some(entry_id) = self.pending_tree_entry.take() else {
                return OverlayAction::None;
            };
            let instructions = self.overlay_query.trim().to_string();
            self.close_overlay();
            return OverlayAction::NavigateTree {
                entry_id,
                summarize: true,
                instructions: (!instructions.is_empty()).then_some(instructions),
            };
        }
        let Some(item) = self.overlay_items().get(self.overlay_selected).cloned() else {
            return OverlayAction::None;
        };
        match self.overlay {
            OverlayKind::CommandPalette => {
                let Some(target) = self
                    .command_palette_entries()
                    .get(self.overlay_selected)
                    .map(|entry| entry.target.clone())
                else {
                    return OverlayAction::None;
                };
                let CommandPaletteTarget::Action(action) = target else {
                    let CommandPaletteTarget::Slash(command) = target else {
                        unreachable!()
                    };
                    self.prompt = format!("{command} ");
                    self.cursor = self.prompt.chars().count();
                    self.focus = Focus::Prompt;
                    self.close_overlay();
                    return OverlayAction::None;
                };
                match action {
                    crate::actions::ActionId::SessionPicker => {
                        self.open_overlay(OverlayKind::SessionPicker)
                    }
                    crate::actions::ActionId::ModelPicker => {
                        self.open_overlay(OverlayKind::ModelPicker)
                    }
                    crate::actions::ActionId::Settings => self.open_overlay(OverlayKind::Settings),
                    crate::actions::ActionId::TogglePerformance => {
                        self.perf_visible = !self.perf_visible;
                        self.status = if self.perf_visible {
                            "render performance meter enabled (F3 to hide)".into()
                        } else {
                            "render performance meter hidden".into()
                        };
                        self.close_overlay();
                    }
                    crate::actions::ActionId::CycleMode => {
                        self.cycle_permission_mode();
                        self.close_overlay();
                        return OverlayAction::SetPermissionMode(
                            self.permission_mode.wire_value().into(),
                        );
                    }
                    crate::actions::ActionId::ToggleTasks => {
                        self.close_overlay();
                        self.view = if self.view == View::Tasks {
                            View::Transcript
                        } else {
                            View::Tasks
                        };
                    }
                    crate::actions::ActionId::CommandPalette => {}
                    crate::actions::ActionId::Quit => return OverlayAction::Quit,
                    _ => {}
                }
            }
            OverlayKind::ModelPicker => {
                let Some(model) = self
                    .filtered_models()
                    .get(self.overlay_selected)
                    .cloned()
                    .cloned()
                else {
                    return OverlayAction::None;
                };
                self.model = model.display_name;
                self.pending_thinking_picker = true;
                self.close_overlay();
                return OverlayAction::SetModel { id: model.id };
            }
            OverlayKind::WorkflowPicker => {
                let Some(workflow) = self
                    .filtered_workflows()
                    .get(self.overlay_selected)
                    .cloned()
                    .cloned()
                else {
                    return OverlayAction::None;
                };
                if !workflow.valid {
                    self.status = format!(
                        "workflow {} is invalid: {}",
                        workflow.name,
                        workflow
                            .error
                            .as_deref()
                            .unwrap_or("unknown definition error")
                    );
                    return OverlayAction::None;
                }
                let workflow = workflow.name;
                self.close_overlay();
                self.status = format!("resolving workflow {workflow}…");
                return OverlayAction::PreviewWorkflow { workflow };
            }
            OverlayKind::WorkflowPreview => {
                let Some(preview) = self.workflow_preview.as_ref() else {
                    return OverlayAction::None;
                };
                if preview.readiness.status == "blocked" {
                    self.status =
                        "workflow readiness is blocked; resolve preflight errors first".into();
                    return OverlayAction::None;
                }
                self.prompt = format!("/workflow {} ", preview.name);
                self.cursor = self.prompt.chars().count();
                self.focus = Focus::Prompt;
                self.close_overlay();
                return OverlayAction::None;
            }
            OverlayKind::SessionPicker => {
                let sessions = self.filtered_sessions();
                let Some(session) = sessions.get(self.overlay_selected) else {
                    return OverlayAction::None;
                };
                let target = session.path.clone();
                for session in &mut self.available_sessions {
                    session.current = session.path == target;
                }
                self.close_overlay();
                self.view = View::Transcript;
                return OverlayAction::ResumeSession { target };
            }
            OverlayKind::TreePicker | OverlayKind::ForkPicker => {
                let entries = self.filtered_tree();
                let Some(entry) = entries.get(self.overlay_selected) else {
                    return OverlayAction::None;
                };
                let entry_id = entry.id.clone();
                let fork = self.overlay == OverlayKind::ForkPicker;
                if fork {
                    self.close_overlay();
                    return OverlayAction::ForkSession { entry_id };
                }
                let active_leaf = self
                    .session_tree
                    .iter()
                    .rfind(|entry| entry.active)
                    .map(|entry| entry.id.as_str());
                if active_leaf == Some(entry_id.as_str()) {
                    self.status = "already at this point".into();
                    self.close_overlay();
                    return OverlayAction::None;
                }
                self.pending_tree_entry = Some(entry_id);
                self.open_overlay(OverlayKind::TreeSummaryPicker);
                return OverlayAction::None;
            }
            OverlayKind::TreeSummaryPicker => {
                let Some(entry_id) = self.pending_tree_entry.clone() else {
                    return OverlayAction::None;
                };
                if item == "Summarize with custom prompt" {
                    self.open_overlay(OverlayKind::TreeSummaryEditor);
                    return OverlayAction::None;
                }
                self.pending_tree_entry = None;
                self.close_overlay();
                return OverlayAction::NavigateTree {
                    entry_id,
                    summarize: item == "Summarize",
                    instructions: None,
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
                    3 => OverlayAction::SetRuntimeSetting {
                        key: "show_cache_miss_notices".into(),
                        value: serde_json::json!(!self.runtime_settings.show_cache_miss_notices),
                    },
                    4 => {
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
                    5 => OverlayAction::SetProjectTrust(!self.runtime_settings.project_trusted),
                    6 => {
                        self.open_child_overlay(OverlayKind::Extensions);
                        return OverlayAction::None;
                    }
                    7 => {
                        self.open_child_overlay(OverlayKind::ScopedModels);
                        return OverlayAction::None;
                    }
                    8 => {
                        self.open_child_overlay(OverlayKind::SubagentModelPicker);
                        return OverlayAction::None;
                    }
                    _ => {
                        self.theme_mode = self.theme_mode.next();
                        self.status = format!("{} theme", self.theme_mode.label());
                        return OverlayAction::SetTheme(self.theme_mode);
                    }
                };
                self.apply_runtime_setting(&action);
                return action;
            }
            OverlayKind::Extensions => {
                let extensions = self.filtered_extensions();
                let Some(extension) = extensions.get(self.overlay_selected) else {
                    return OverlayAction::None;
                };
                if extension.scope == "temporary" {
                    return OverlayAction::None;
                }
                let path = extension.path.clone();
                let enabled = !extension.enabled;
                return OverlayAction::SetExtensionEnabled { path, enabled };
            }
            OverlayKind::ScopedModels => {
                let models = self.runtime_settings.enabled_models.clone();
                self.close_overlay();
                return OverlayAction::SetScopedModels(models);
            }
            OverlayKind::SubagentModelPicker => {
                let inherit_visible = self.overlay_query.is_empty()
                    || "inherit parent (default)"
                        .contains(&self.overlay_query.to_ascii_lowercase());
                let model = if item == "Inherit parent (default)" {
                    None
                } else {
                    self.filtered_models()
                        .get(
                            self.overlay_selected
                                .saturating_sub(usize::from(inherit_visible)),
                        )
                        .map(|model| model.id.clone())
                };
                if item != "Inherit parent (default)" && model.is_none() {
                    return OverlayAction::None;
                }
                let action = OverlayAction::SetRuntimeSetting {
                    key: "subagent_model".into(),
                    value: model
                        .clone()
                        .map_or(serde_json::Value::Null, serde_json::Value::String),
                };
                self.runtime_settings.subagent_model = model;
                self.close_overlay();
                return action;
            }
            OverlayKind::LoginProvider => {
                let provider = self
                    .filtered_auth_providers()
                    .get(self.overlay_selected)
                    .copied()
                    .cloned();
                return provider.map_or(OverlayAction::None, |provider| self.start_auth(provider));
            }
            OverlayKind::ThinkingPicker => {
                self.close_overlay();
                return OverlayAction::SetThinking(item);
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
            OverlayKind::ApiKeyPrompt
            | OverlayKind::OauthPrompt
            | OverlayKind::SessionRename
            | OverlayKind::SessionDeleteConfirm
            | OverlayKind::TreeSummaryEditor
            | OverlayKind::PasteEditor
            | OverlayKind::ImageViewer
            | OverlayKind::LabelEditor
            | OverlayKind::None => {}
            OverlayKind::FilePicker => {
                if is_image_reference(&item) {
                    self.remove_file_reference_trigger();
                    self.close_overlay();
                    self.status = format!("loading {item}…");
                    match crate::image_attachment_from_path(std::path::PathBuf::from(&item)) {
                        Ok(image) => self.attach_image(image),
                        Err(error) => self.status = format!("image reference unavailable: {error}"),
                    }
                } else {
                    self.insert_file_reference(&item);
                    self.close_overlay();
                }
            }
        }
        OverlayAction::None
    }

    pub fn cycle_session_sort(&mut self) {
        if self.overlay == OverlayKind::SessionPicker {
            self.session_sort = self.session_sort.next();
            self.overlay_selected = 0;
        }
    }

    pub fn toggle_named_sessions(&mut self) {
        if self.overlay == OverlayKind::SessionPicker {
            self.session_named_only = !self.session_named_only;
            self.overlay_selected = 0;
        }
    }

    pub fn toggle_session_paths(&mut self) {
        if self.overlay == OverlayKind::SessionPicker {
            self.session_show_path = !self.session_show_path;
        }
    }

    pub fn begin_session_rename(&mut self) {
        if self.overlay != OverlayKind::SessionPicker {
            return;
        }
        let sessions = self.filtered_sessions();
        let Some(session) = sessions.get(self.overlay_selected) else {
            return;
        };
        let target = session.path.clone();
        let name = session.name.clone().unwrap_or_default();
        self.pending_session_path = Some(target);
        self.overlay = OverlayKind::SessionRename;
        self.overlay_query = name;
        self.overlay_selected = 0;
    }

    pub fn begin_session_delete(&mut self) {
        if self.overlay != OverlayKind::SessionPicker {
            return;
        }
        let sessions = self.filtered_sessions();
        let Some(session) = sessions.get(self.overlay_selected) else {
            return;
        };
        if session.current {
            self.status = "cannot delete the active session".into();
            return;
        }
        self.pending_session_path = Some(session.path.clone());
        self.overlay = OverlayKind::SessionDeleteConfirm;
        self.overlay_query.clear();
        self.overlay_selected = 0;
    }

    pub(crate) fn filtered_sessions(&self) -> Vec<&SessionInfo> {
        let query = self.overlay_query.trim().to_ascii_lowercase();
        let mut sessions = self
            .available_sessions
            .iter()
            .filter(|session| {
                !self.session_named_only
                    || session
                        .name
                        .as_deref()
                        .is_some_and(|name| !name.trim().is_empty())
            })
            .filter(|session| session_search_text(session).contains(&query))
            .collect::<Vec<_>>();
        match self.session_sort {
            SessionSort::Recent => {
                sessions.sort_by_key(|session| std::cmp::Reverse(session.modified_at_ms))
            }
            SessionSort::Relevance if !query.is_empty() => sessions.sort_by_key(|session| {
                session_search_text(session)
                    .find(&query)
                    .unwrap_or(usize::MAX)
            }),
            SessionSort::Threaded if query.is_empty() => {
                sessions = threaded_session_order(sessions);
            }
            SessionSort::Relevance | SessionSort::Threaded => {}
        }
        sessions
    }

    fn session_picker_label(&self, session: &SessionInfo) -> String {
        let prefix =
            if self.session_sort == SessionSort::Threaded && self.overlay_query.trim().is_empty() {
                let mut depth = 0usize;
                let mut parent = session.parent_session_path.as_deref();
                while let Some(path) = parent {
                    let Some(ancestor) = self
                        .available_sessions
                        .iter()
                        .find(|candidate| candidate.path == path)
                    else {
                        break;
                    };
                    depth += 1;
                    if depth >= 32 {
                        break;
                    }
                    parent = ancestor.parent_session_path.as_deref();
                }
                if depth == 0 {
                    String::new()
                } else {
                    format!("{}└─ ", "   ".repeat(depth.saturating_sub(1)))
                }
            } else {
                String::new()
            };
        format!("{prefix}{}", session_label(session, self.session_show_path))
    }

    pub fn toggle_scoped_model(&mut self) {
        if self.overlay != OverlayKind::ScopedModels {
            return;
        }
        let Some(model_id) = self
            .filtered_models()
            .get(self.overlay_selected)
            .map(|model| model.id.clone())
        else {
            return;
        };
        if let Some(index) = self
            .runtime_settings
            .enabled_models
            .iter()
            .position(|id| id == &model_id)
        {
            self.runtime_settings.enabled_models.remove(index);
        } else {
            self.runtime_settings.enabled_models.push(model_id);
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
            OverlayAction::SetRuntimeSetting { key, value } if key == "show_cache_miss_notices" => {
                self.runtime_settings.show_cache_miss_notices = value.as_bool().unwrap_or(false)
            }
            OverlayAction::SetRuntimeSetting { key, value } if key == "default_project_trust" => {
                self.runtime_settings.default_project_trust =
                    value.as_str().unwrap_or_default().into()
            }
            OverlayAction::SetRuntimeSetting { key, value } if key == "subagent_model" => {
                self.runtime_settings.subagent_model = value.as_str().map(str::to_owned)
            }
            OverlayAction::SetProjectTrust(trusted) => {
                self.runtime_settings.project_trusted = *trusted
            }
            _ => {}
        }
    }

    pub fn cycle_tree_filter(&mut self) {
        let selected_id = self
            .filtered_tree()
            .get(self.overlay_selected)
            .map(|entry| entry.id.clone());
        self.tree_filter = self.tree_filter.next();
        self.tree_folded.clear();
        let entries = self.filtered_tree();
        self.overlay_selected = selected_id
            .as_deref()
            .and_then(|id| entries.iter().position(|entry| entry.id == id))
            .unwrap_or_else(|| entries.len().saturating_sub(1));
    }

    pub fn toggle_tree_timestamps(&mut self) {
        self.tree_show_timestamps = !self.tree_show_timestamps;
    }

    pub fn move_tree_page(&mut self, direction: isize, page: usize) {
        self.overlay_hovered = None;
        let count = self.filtered_tree().len();
        if count == 0 {
            self.overlay_selected = 0;
        } else if direction < 0 {
            self.overlay_selected = self.overlay_selected.saturating_sub(page);
        } else {
            self.overlay_selected = self.overlay_selected.saturating_add(page).min(count - 1);
        }
    }

    pub fn fold_or_move_tree(&mut self, unfold: bool) {
        if self.overlay != OverlayKind::TreePicker {
            return;
        }
        let entries = self.filtered_tree();
        let Some(selected) = entries.get(self.overlay_selected) else {
            return;
        };
        let selected_id = selected.id.clone();
        let parent_id = selected.parent_id.clone();
        let child_id = entries
            .iter()
            .find(|entry| entry.parent_id.as_deref() == Some(selected_id.as_str()))
            .map(|entry| entry.id.clone());
        let sibling_count = entries
            .iter()
            .filter(|entry| entry.parent_id == parent_id)
            .count();
        let foldable = child_id.is_some() && (parent_id.is_none() || sibling_count > 1);
        drop(entries);

        if unfold {
            if self.tree_folded.remove(&selected_id) {
                return;
            }
            if let Some(child_id) = child_id {
                let entries = self.filtered_tree();
                if let Some(index) = entries.iter().position(|entry| entry.id == child_id) {
                    self.overlay_selected = index;
                }
            }
        } else if foldable {
            self.tree_folded.insert(selected_id);
            let count = self.filtered_tree().len();
            self.overlay_selected = self.overlay_selected.min(count.saturating_sub(1));
        } else if let Some(parent_id) = parent_id {
            let entries = self.filtered_tree();
            if let Some(index) = entries.iter().position(|entry| entry.id == parent_id) {
                self.overlay_selected = index;
            }
        }
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

    fn remove_file_reference_trigger(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let before_cursor = self.prompt.chars().take(self.cursor).collect::<String>();
        if before_cursor.ends_with('@') {
            let byte = char_to_byte(&self.prompt, self.cursor - 1);
            self.prompt.remove(byte);
            self.cursor -= 1;
        }
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
            instructions: None,
        }
    }

    pub(crate) fn filtered_tree(&self) -> Vec<&SessionTreeEntry> {
        let query = if self.overlay == OverlayKind::TreePicker {
            self.overlay_query.trim().to_ascii_lowercase()
        } else {
            String::new()
        };
        let by_id = self
            .session_tree
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect::<std::collections::HashMap<_, _>>();
        // Resolve folded descendants once. Walking from every entry back to
        // its ancestors is quadratic for Pi's common long, linear sessions.
        let hidden_by_fold = if self.tree_folded.is_empty() {
            HashSet::new()
        } else {
            let mut children: std::collections::HashMap<&str, Vec<&str>> =
                std::collections::HashMap::new();
            for entry in &self.session_tree {
                if let Some(parent) = entry.parent_id.as_deref() {
                    children.entry(parent).or_default().push(&entry.id);
                }
            }
            let mut hidden = HashSet::new();
            let mut stack = self
                .tree_folded
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            while let Some(parent) = stack.pop() {
                if let Some(descendants) = children.get(parent) {
                    for descendant in descendants {
                        if hidden.insert(*descendant) {
                            stack.push(*descendant);
                        }
                    }
                }
            }
            hidden
        };
        let filtered = self
            .session_tree
            .iter()
            .filter(|entry| self.tree_entry_matches_filter(entry))
            .filter(|entry| tree_entry_matches_query(entry, &query))
            .filter(|entry| !hidden_by_fold.contains(entry.id.as_str()))
            .collect::<Vec<_>>();
        if self.overlay == OverlayKind::ForkPicker {
            filtered
        } else {
            active_first_tree_order(filtered, &by_id)
        }
    }

    fn visible_tree_count(&self) -> usize {
        if self.overlay == OverlayKind::ForkPicker {
            return self.session_tree.len();
        }
        // Folding requires descendant resolution; reuse the canonical view in
        // that uncommon case. Normal navigation only needs a cheap count and
        // must not rebuild ordering/topology for every repeated key event.
        if !self.tree_folded.is_empty() {
            return self.filtered_tree().len();
        }
        let query = self.overlay_query.trim().to_ascii_lowercase();
        self.session_tree
            .iter()
            .filter(|entry| self.tree_entry_matches_filter(entry))
            .filter(|entry| tree_entry_matches_query(entry, &query))
            .count()
    }

    fn tree_entry_matches_filter(&self, entry: &SessionTreeEntry) -> bool {
        if self.overlay == OverlayKind::ForkPicker {
            return true;
        }
        match self.tree_filter {
            TreeFilter::Default => {
                !matches!(
                    entry.kind.as_str(),
                    "custom" | "label" | "session_info" | "model_change" | "thinking_level_change"
                ) && !(entry.role.as_deref() == Some("assistant")
                    && entry.text.trim().is_empty()
                    && !entry.active)
            }
            TreeFilter::NoTools => {
                entry.role.as_deref() != Some("toolResult")
                    && !matches!(
                        entry.kind.as_str(),
                        "custom"
                            | "label"
                            | "session_info"
                            | "model_change"
                            | "thinking_level_change"
                    )
            }
            TreeFilter::UserOnly => entry.role.as_deref() == Some("user"),
            TreeFilter::LabeledOnly => entry.label.is_some(),
            TreeFilter::All => true,
        }
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
        let continuation = self
            .entries
            .iter()
            .rposition(|entry| matches!(entry, Entry::Reasoning { active: true, .. }))
            .filter(|index| {
                self.entries[index + 1..]
                    .iter()
                    .all(|entry| matches!(entry, Entry::Tool { .. } | Entry::Diff { .. }))
            });
        let target_index = continuation.or_else(|| self.entries.len().checked_sub(1));
        let target = target_index.and_then(|index| self.entries.get_mut(index));
        if let Some(Entry::Reasoning {
            text: current,
            active: true,
            ..
        }) = target
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
        self.scroll_drag = None;
        self.scrollbar_dragging = false;
        self.pending_transcript_click = None;
    }

    pub fn focus_scrollback(&mut self) {
        self.focus = Focus::Scrollback;
        if self.focused_entry.is_none()
            && let Some(index) = self.entries.len().checked_sub(1)
        {
            self.focused_entry = Some(index);
            self.focused_target_id = self.entry_target_id(index);
        }
    }

    pub fn subagent_task_for_entry(&self, entry: &Entry) -> Option<&SubagentTask> {
        let Entry::Tool { id, .. } = entry else {
            return None;
        };
        self.subagent_tasks.get(id.strip_prefix("subagent:")?)
    }

    fn rebuild_active_subagent_entries(&mut self) {
        if self.active_subagent_task_ids.is_empty() {
            return;
        }
        let expanded_ids: HashSet<String> = self
            .expanded_tool_groups
            .iter()
            .filter_map(|index| match self.entries.get(*index) {
                Some(Entry::Tool { id, .. }) => Some(id.clone()),
                _ => None,
            })
            .collect();
        let active_ids: HashSet<&str> = self
            .active_subagent_task_ids
            .iter()
            .map(String::as_str)
            .collect();
        let first = self
            .entries
            .iter()
            .position(|entry| {
                matches!(entry, Entry::Tool { id, .. } if id.strip_prefix("subagent:").is_some_and(|id| active_ids.contains(id)))
            })
            .unwrap_or(self.entries.len());
        let initializing_group = first == self.entries.len();
        self.entries.retain(|entry| {
            !matches!(entry, Entry::Tool { id, .. } if id.strip_prefix("subagent:").is_some_and(|id| active_ids.contains(id)))
        });
        let mut tasks: Vec<_> = self
            .active_subagent_task_ids
            .iter()
            .filter_map(|id| self.subagent_tasks.get(id))
            .cloned()
            .collect();
        tasks.sort_by_key(|task| task.started_at_ms);
        let entries = tasks.into_iter().map(|task| {
            let status = match task.status.as_str() {
                "running" => ToolStatus::Running,
                "completed" => ToolStatus::Success,
                _ => ToolStatus::Error,
            };
            let duration = (task.status != "running")
                .then(|| format_elapsed(std::time::Duration::from_millis(task.duration_ms)));
            let started_at = (task.status == "running").then(|| {
                Instant::now()
                    .checked_sub(std::time::Duration::from_millis(task.duration_ms))
                    .unwrap_or_else(Instant::now)
            });
            Entry::Tool {
                id: format!("subagent:{}", task.task_id),
                label: "Agent".into(),
                detail: task.description,
                status,
                duration,
                started_at,
                result: task.output.or(task.error),
                expanded: false,
            }
        });
        let insertion = first.min(self.entries.len());
        self.entries.splice(insertion..insertion, entries);
        self.expanded_tool_groups.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if matches!(entry, Entry::Tool { id, .. } if expanded_ids.contains(id)) {
                self.expanded_tool_groups.insert(index);
            }
        }
        if initializing_group {
            self.expanded_tool_groups.insert(insertion);
        }
        self.entry_ids.borrow_mut().clear();
    }

    pub fn insert_char(&mut self, character: char) {
        self.escape_armed_at = None;
        self.focused_image = None;
        let position = self.cursor;
        let byte = char_to_byte(&self.prompt, self.cursor);
        self.prompt.insert(byte, character);
        for block in &mut self.paste_blocks {
            if block.start >= position {
                block.start += 1;
                block.end += 1;
            } else if block.end > position {
                block.end += 1;
            }
        }
        self.cursor += 1;
        self.history_index = None;
    }

    pub fn composer_display_cursor(&self) -> usize {
        let mut display = self.cursor;
        for block in &self.paste_blocks {
            if self.cursor >= block.end {
                display = display.saturating_sub(
                    block.end.saturating_sub(block.start).saturating_sub(
                        format!("[paste#{}]", block.end - block.start)
                            .chars()
                            .count(),
                    ),
                );
            } else if self.cursor > block.start {
                display = display.saturating_sub(self.cursor - block.start);
                display += format!("[paste#{}]", block.end - block.start)
                    .chars()
                    .count();
                break;
            }
        }
        display
    }

    pub fn composer_display_len(&self) -> usize {
        self.prompt.chars().count()
            + self
                .paste_blocks
                .iter()
                .map(|block| {
                    format!("[paste#{}]", block.end - block.start)
                        .chars()
                        .count()
                        .saturating_sub(block.end - block.start)
                })
                .sum::<usize>()
    }

    pub fn composer_display_text(&self) -> String {
        if self.paste_blocks.is_empty() {
            return self.prompt.clone();
        }
        let chars = self.prompt.chars().collect::<Vec<_>>();
        let mut output = String::new();
        let mut cursor = 0usize;
        for block in &self.paste_blocks {
            if block.start > cursor {
                output.extend(chars[cursor..block.start.min(chars.len())].iter());
            }
            output.push_str(&format!(
                "[paste#{}]",
                block.end.saturating_sub(block.start)
            ));
            cursor = block.end.min(chars.len());
        }
        if cursor < chars.len() {
            output.extend(chars[cursor..].iter());
        }
        output
    }

    pub fn paste_at_cursor(&self) -> Option<u64> {
        self.paste_blocks
            .iter()
            .find(|block| self.cursor >= block.start && self.cursor <= block.end)
            .map(|block| block.id)
    }

    pub fn focus_paste(&mut self, id: u64) -> bool {
        let Some(block) = self.paste_blocks.iter().find(|block| block.id == id) else {
            return false;
        };
        self.cursor = block.end;
        self.focus = Focus::Prompt;
        self.paste_hover = Some(id);
        true
    }

    pub fn begin_paste_edit(&mut self, id: u64) -> bool {
        let Some(block) = self.paste_blocks.iter().find(|block| block.id == id) else {
            return false;
        };
        let start = char_to_byte(&self.prompt, block.start);
        let end = char_to_byte(&self.prompt, block.end);
        let content = self.prompt[start..end].to_string();
        self.open_overlay(OverlayKind::PasteEditor);
        self.overlay_query = content;
        self.overlay_cursor = self.overlay_query.chars().count();
        self.paste_editor_preferred_column = None;
        self.paste_editor_scroll.set(0);
        self.paste_editor_follow_cursor.set(true);
        self.pending_paste_id = Some(id);
        true
    }

    pub fn move_paste_editor_cursor(&mut self, delta: isize) {
        if self.overlay != OverlayKind::PasteEditor {
            return;
        }
        let length = self.overlay_query.chars().count();
        self.overlay_cursor = if delta < 0 {
            self.overlay_cursor.saturating_sub(delta.unsigned_abs())
        } else {
            self.overlay_cursor
                .saturating_add(delta as usize)
                .min(length)
        };
        self.paste_editor_preferred_column = None;
        self.paste_editor_follow_cursor.set(true);
    }

    pub fn move_paste_editor_vertical(&mut self, delta: isize) {
        if self.overlay != OverlayKind::PasteEditor {
            return;
        }
        let rows = self.paste_editor_rows.borrow();
        let Some(current) = rows.iter().rposition(|row| {
            row.first()
                .is_some_and(|(start, _)| *start <= self.overlay_cursor)
                && row
                    .last()
                    .is_some_and(|(end, _)| self.overlay_cursor <= *end)
        }) else {
            return;
        };
        let column = self.paste_editor_preferred_column.unwrap_or_else(|| {
            let position = rows[current]
                .iter()
                .position(|(offset, _)| *offset == self.overlay_cursor)
                .unwrap_or(0);
            rows[current].get(position).map_or(0, |(_, column)| *column)
        });
        let target = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current
                .saturating_add(delta as usize)
                .min(rows.len().saturating_sub(1))
        };
        if let Some((offset, _)) = rows[target]
            .iter()
            .min_by_key(|(_, target_column)| target_column.abs_diff(column))
        {
            self.overlay_cursor = *offset;
            self.paste_editor_preferred_column = Some(column);
            self.paste_editor_follow_cursor.set(true);
        }
    }

    pub fn move_paste_editor_line_edge(&mut self, end: bool) {
        let rows = self.paste_editor_rows.borrow();
        let Some(row) = rows.iter().rfind(|row| {
            row.first()
                .is_some_and(|(start, _)| *start <= self.overlay_cursor)
                && row
                    .last()
                    .is_some_and(|(row_end, _)| self.overlay_cursor <= *row_end)
        }) else {
            return;
        };
        if let Some((offset, _)) = if end { row.last() } else { row.first() } {
            self.overlay_cursor = *offset;
            self.paste_editor_preferred_column = None;
            self.paste_editor_follow_cursor.set(true);
        }
    }

    pub fn scroll_paste_editor(&self, delta: isize) {
        let current = self.paste_editor_scroll.get();
        self.paste_editor_scroll.set(if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta as usize)
        });
        self.paste_editor_follow_cursor.set(false);
    }

    pub fn click_paste_editor(&mut self, column: u16, row: u16) -> bool {
        let targets = self.paste_editor_targets.borrow();
        let Some((_, _, offset)) = targets
            .iter()
            .filter(|(_, target_row, _)| *target_row == row)
            .min_by_key(|(target_column, _, _)| target_column.abs_diff(column))
        else {
            return false;
        };
        self.overlay_cursor = *offset;
        self.paste_editor_preferred_column = None;
        self.paste_editor_follow_cursor.set(true);
        true
    }

    pub fn paste_editor_action_at(&self, column: u16, row: u16) -> Option<u8> {
        self.paste_editor_actions
            .borrow()
            .iter()
            .find(|(_, start, end, target_row)| {
                row == *target_row && column >= *start && column < *end
            })
            .map(|(action, _, _, _)| *action)
    }

    pub fn insert_paste_editor_text(&mut self, text: String) {
        if self.overlay != OverlayKind::PasteEditor {
            return;
        }
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let byte = char_to_byte(&self.overlay_query, self.overlay_cursor);
        self.overlay_query.insert_str(byte, &normalized);
        self.overlay_cursor += normalized.chars().count();
        self.paste_editor_preferred_column = None;
        self.paste_editor_follow_cursor.set(true);
    }

    pub fn replace_paste(&mut self, id: u64, replacement: String) -> bool {
        let Some(block) = self
            .paste_blocks
            .iter()
            .find(|block| block.id == id)
            .cloned()
        else {
            return false;
        };
        let start_byte = char_to_byte(&self.prompt, block.start);
        let end_byte = char_to_byte(&self.prompt, block.end);
        self.prompt
            .replace_range(start_byte..end_byte, &replacement);
        let old_length = block.end - block.start;
        let new_length = replacement.chars().count();
        if new_length == 0 {
            self.paste_blocks.retain(|candidate| candidate.id != id);
        } else if let Some(candidate) = self
            .paste_blocks
            .iter_mut()
            .find(|candidate| candidate.id == id)
        {
            candidate.end = candidate.start + new_length;
        }
        let delta = new_length as isize - old_length as isize;
        for candidate in &mut self.paste_blocks {
            if candidate.id != id && candidate.start >= block.end {
                candidate.start = candidate.start.saturating_add_signed(delta);
                candidate.end = candidate.end.saturating_add_signed(delta);
            }
        }
        self.cursor = block.start + new_length;
        self.paste_hover = None;
        true
    }

    pub fn remove_paste(&mut self, id: u64) -> bool {
        let Some(block) = self
            .paste_blocks
            .iter()
            .find(|block| block.id == id)
            .cloned()
        else {
            return false;
        };
        let start = char_to_byte(&self.prompt, block.start);
        let end = char_to_byte(&self.prompt, block.end);
        self.prompt.replace_range(start..end, "");
        let length = block.end - block.start;
        self.paste_blocks.retain(|candidate| candidate.id != id);
        for candidate in &mut self.paste_blocks {
            if candidate.start >= block.end {
                candidate.start -= length;
                candidate.end -= length;
            }
        }
        self.cursor = block.start;
        self.paste_hover = None;
        true
    }

    pub fn insert_paste(&mut self, text: String) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() {
            return;
        }
        let start = self.cursor;
        let byte = char_to_byte(&self.prompt, start);
        self.prompt.insert_str(byte, &normalized);
        let length = normalized.chars().count();
        for block in &mut self.paste_blocks {
            if block.start >= start {
                block.start += length;
                block.end += length;
            } else if block.end > start {
                block.end += length;
            }
        }
        self.paste_blocks.push(PasteBlock {
            id: self.next_paste_id,
            start,
            end: start + length,
        });
        self.next_paste_id += 1;
        self.cursor += length;
        self.history_index = None;
    }

    pub fn backspace(&mut self) {
        if self.focused_image.is_some()
            || (self.prompt.is_empty() && !self.image_attachments.is_empty())
        {
            let id = self
                .focused_image
                .or_else(|| self.image_attachments.last().map(|image| image.id));
            if let Some(id) = id {
                self.image_attachments.retain(|image| image.id != id);
                self.focused_image = None;
                self.image_hover = None;
                return;
            }
        }
        if let Some(id) = self.paste_at_cursor()
            && self.remove_paste(id)
        {
            self.history_index = None;
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let start = char_to_byte(&self.prompt, self.cursor - 1);
        let end = char_to_byte(&self.prompt, self.cursor);
        self.prompt.replace_range(start..end, "");
        self.cursor -= 1;
        for block in &mut self.paste_blocks {
            if block.start > self.cursor {
                block.start -= 1;
                block.end -= 1;
            }
        }
        self.history_index = None;
    }

    pub fn delete(&mut self) {
        if let Some(id) = self.paste_at_cursor()
            && self.remove_paste(id)
        {
            self.history_index = None;
            return;
        }
        if self.cursor >= self.prompt.chars().count() {
            return;
        }
        let start = char_to_byte(&self.prompt, self.cursor);
        let end = char_to_byte(&self.prompt, self.cursor + 1);
        self.prompt.replace_range(start..end, "");
        for block in &mut self.paste_blocks {
            if block.start > self.cursor {
                block.start -= 1;
                block.end -= 1;
            }
        }
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
        self.clear_prompt_text();
        self.image_attachments.clear();
        self.focused_image = None;
        self.image_hover = None;
    }

    pub fn clear_prompt_text(&mut self) {
        self.prompt.clear();
        self.cursor = 0;
        self.paste_blocks.clear();
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

    pub fn toggle_all_tools(&mut self) {
        let expand = self.entries.iter().any(|entry| {
            matches!(
                entry,
                Entry::Tool {
                    expanded: false,
                    ..
                }
            )
        });
        for entry in &mut self.entries {
            if let Entry::Tool { expanded, .. } = entry {
                *expanded = expand;
            }
        }

        // A collapsed group would otherwise keep its children hidden even
        // though their individual entries have been expanded.
        self.expanded_tool_groups.clear();
        if expand {
            let mut index = 0;
            while index < self.entries.len() {
                let Entry::Tool { label, .. } = &self.entries[index] else {
                    index += 1;
                    continue;
                };
                let count = self.entries[index..]
                    .iter()
                    .take_while(
                        |entry| matches!(entry, Entry::Tool { label: other, .. } if other == label),
                    )
                    .count();
                if count > 1 {
                    self.expanded_tool_groups.insert(index);
                }
                index += count;
            }
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

    pub fn toggle_all_diffs(&mut self) {
        let expand = self.entries.iter().any(|entry| {
            matches!(
                entry,
                Entry::Diff {
                    expanded: false,
                    ..
                }
            )
        });
        for entry in &mut self.entries {
            if let Entry::Diff { expanded, .. } = entry {
                *expanded = expand;
            }
        }
    }

    pub fn toggle_entry_at(&mut self, index: usize) {
        self.focused_entry = Some(index);
        self.focused_section = None;
        let target_id = self.entry_target_id(index);
        let mut toggled = false;
        match self.entries.get_mut(index) {
            Some(
                Entry::Reasoning { expanded, .. }
                | Entry::Diff { expanded, .. }
                | Entry::Plan { expanded, .. },
            ) => {
                *expanded = !*expanded;
                toggled = true;
            }
            Some(Entry::Tool { .. }) => {
                self.toggle_tool_at(index);
                toggled = true;
            }
            _ => {}
        }
        if toggled && let Some(target_id) = target_id {
            self.pinned_entry_modes.insert(target_id);
        }
    }

    pub fn activate_entry_at(&mut self, index: usize) {
        let subagent = self.entries.get(index).and_then(|entry| match entry {
            Entry::Tool { id, .. } => {
                let task_id = id.strip_prefix("subagent:")?;
                Some((
                    task_id.to_owned(),
                    self.subagent_tasks
                        .get(task_id)
                        .and_then(|task| task.workflow_run_id.clone()),
                ))
            }
            _ => None,
        });
        if let Some((task_id, workflow_run_id)) = subagent {
            self.inspected_subagent = Some(task_id);
            self.subagent_return_view = if let Some(run_id) = workflow_run_id {
                self.select_workflow(&run_id);
                View::Workflows
            } else {
                self.view
            };
            self.view = View::Subagent;
            self.scroll_from_bottom = 0;
        } else {
            let foldable = matches!(
                self.entries.get(index),
                Some(
                    Entry::Reasoning { .. }
                        | Entry::Diff { .. }
                        | Entry::Plan { .. }
                        | Entry::Tool { .. }
                )
            );
            if foldable {
                self.toggle_entry_at(index);
            }
        }
    }

    pub fn selected_block_markdown(&self) -> Option<String> {
        self.focused_entry
            .and_then(|index| self.entry_markdown(index))
            .filter(|text| !text.is_empty())
    }

    pub fn selected_block_text(&self) -> Option<String> {
        let index = self.focused_entry?;
        let entry = self.entries.get(index)?;
        let text = match entry {
            Entry::Assistant { lines, .. } => crate::markdown::render(lines, 120, self.theme())
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string(),
            _ => self.entry_markdown(index)?,
        };
        (!text.is_empty()).then_some(text)
    }

    pub fn selected_code(&self) -> Option<String> {
        let Entry::Assistant { lines, .. } = self.entries.get(self.focused_entry?)? else {
            return None;
        };
        let blocks = crate::markdown::code_blocks(lines);
        (!blocks.is_empty()).then(|| {
            blocks
                .into_iter()
                .map(|block| block.body)
                .collect::<Vec<_>>()
                .join("\n\n")
        })
    }

    pub fn selected_turn_markdown(&self) -> Option<String> {
        let selected = self.focused_entry?;
        let user = (0..=selected)
            .rev()
            .find(|index| matches!(self.entries.get(*index), Some(Entry::User { .. })))?;
        let assistant = (selected.max(user)..self.entries.len())
            .find(|index| matches!(self.entries.get(*index), Some(Entry::Assistant { .. })))?;
        let mut transcript = Vec::new();
        for index in user..=assistant {
            let Some(entry) = self.entries.get(index) else {
                continue;
            };
            let Some(body) = self.entry_markdown(index) else {
                continue;
            };
            let heading = match entry {
                Entry::User { .. } => "## User".to_string(),
                Entry::Assistant { .. } => "## Assistant".to_string(),
                Entry::Reasoning { .. } => "### Reasoning".to_string(),
                Entry::Tool { label, .. } => format!("### Tool: {label}"),
                Entry::Diff { path, .. } => format!("### Change: `{path}`"),
                Entry::Plan { .. } => "### Plan".to_string(),
                Entry::Compaction { .. } => "### Compaction".to_string(),
                Entry::CompactionIndicator { .. } => continue,
            };
            transcript.push(format!("{heading}\n\n{}", body.trim()));
        }
        (!transcript.is_empty()).then(|| transcript.join("\n\n"))
    }

    fn entry_markdown(&self, index: usize) -> Option<String> {
        match self.entries.get(index)? {
            Entry::User { text, .. } => Some(text.clone()),
            Entry::Assistant { lines, .. } => Some(lines.join("\n")),
            Entry::Reasoning { text, .. } => Some(text.clone()),
            Entry::Diff { path, lines, .. } => Some(format!(
                "```diff\n--- a/{path}\n+++ b/{path}\n{}\n```",
                lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            )),
            Entry::Tool { detail, result, .. } => {
                Some(result.as_deref().unwrap_or(detail).to_string())
            }
            Entry::Plan { entries, .. } => Some(
                entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "- [{}] {}",
                            if entry.status == "completed" {
                                "x"
                            } else {
                                " "
                            },
                            entry.step
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Entry::Compaction { summary, .. } => Some(summary.clone()),
            Entry::CompactionIndicator { .. } => None,
        }
    }

    pub fn activate_transcript_target(&mut self, index: usize, target_id: &str) {
        if target_id.starts_with("tool-group:") {
            self.toggle_tool_group(index);
        } else {
            self.activate_entry_at(index);
        }
        self.focused_entry = Some(index);
        self.focused_target_id = Some(target_id.to_string());
    }

    pub fn finish_transcript_click(&mut self, released: Option<(usize, String)>) {
        let pending = self.pending_transcript_click.take();
        self.scroll_drag = None;
        self.scrollbar_dragging = false;
        if let (Some((index, target_id)), Some((_, released_id))) = (pending, released)
            && target_id == released_id
        {
            self.activate_transcript_target(index, &target_id);
        }
    }

    pub fn begin_scrollback_drag(&mut self, row: u16) {
        self.scroll_drag = Some(ScrollDrag {
            start_row: row,
            start_from_bottom: self.scroll_from_bottom,
        });
    }

    pub fn transcript_contains(&self, column: u16, row: u16) -> bool {
        self.transcript_rect
            .get()
            .is_some_and(|(x, y, width, height)| {
                column >= x
                    && column < x.saturating_add(width)
                    && row >= y
                    && row < y.saturating_add(height)
            })
    }

    pub fn transcript_scrollbar_contains(&self, column: u16, row: u16) -> bool {
        self.transcript_scrollbar_rect
            .get()
            .is_some_and(|(x, y, width, height)| {
                column >= x
                    && column < x.saturating_add(width)
                    && row >= y
                    && row < y.saturating_add(height)
            })
    }

    pub fn drag_scrollback_to(&mut self, row: u16, max_scroll: usize) {
        let Some(drag) = self.scroll_drag else {
            return;
        };
        self.scroll_from_bottom = if row >= drag.start_row {
            drag.start_from_bottom
                .saturating_add(usize::from(row - drag.start_row))
                .min(max_scroll)
        } else {
            drag.start_from_bottom
                .saturating_sub(usize::from(drag.start_row - row))
        };
    }

    pub fn drag_scrollbar_to(&mut self, row: u16, max_scroll: usize) {
        let Some((_, y, _, height)) = self.transcript_scrollbar_rect.get() else {
            return;
        };
        if height <= 1 {
            return;
        }
        let position = usize::from(row.saturating_sub(y).min(height - 1));
        let from_top = max_scroll.saturating_mul(position) / usize::from(height - 1);
        self.scroll_from_bottom = max_scroll.saturating_sub(from_top);
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

    pub fn submit_message(&mut self) -> Option<(String, Vec<pi_harness::MessageImage>)> {
        let prompt = self.prompt.trim().to_string();
        if prompt.is_empty() && self.image_attachments.is_empty() {
            return None;
        }
        let images = self
            .image_attachments
            .iter()
            .map(|image| pi_harness::MessageImage {
                path: image.path.clone(),
                mime_type: image.mime_type.clone(),
                temporary: image.temporary,
            })
            .collect::<Vec<_>>();
        let display = if prompt.is_empty() {
            self.image_attachments
                .iter()
                .map(|image| format!("[{}]", image.name))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            prompt.clone()
        };
        if !prompt.is_empty() {
            self.prompt_history.push(prompt.clone());
        }
        self.entries.push(Entry::User {
            text: display,
            timestamp: String::new(),
        });
        self.clear_prompt();
        self.scroll_to_bottom();
        if self.streaming {
            self.status = "queued".into();
        } else {
            self.begin_turn_if_needed();
            self.streaming = true;
            self.submission_pending = true;
            self.status = "sendingâ€¦".into();
        }
        Some((prompt, images))
    }

    pub fn attach_image(&mut self, mut image: ImageAttachment) {
        let status = format!(
            "attached {} ({}×{}, {})",
            image.name, image.width, image.height, image.mime_type
        );
        image.id = self.next_image_id;
        self.next_image_id += 1;
        self.image_attachments.push(image);
        self.focused_image = None;
        self.focus = Focus::Prompt;
        self.status = status;
    }

    pub fn begin_image_view(&mut self, id: u64) -> bool {
        if !self.image_attachments.iter().any(|image| image.id == id) {
            return false;
        }
        self.open_overlay(OverlayKind::ImageViewer);
        self.pending_image_id = Some(id);
        true
    }

    pub fn viewed_image(&self) -> Option<&ImageAttachment> {
        let id = self.pending_image_id?;
        self.image_attachments.iter().find(|image| image.id == id)
    }

    pub fn remove_viewed_image(&mut self) -> bool {
        let Some(id) = self.pending_image_id else {
            return false;
        };
        let before = self.image_attachments.len();
        self.image_attachments.retain(|image| image.id != id);
        self.close_overlay();
        before != self.image_attachments.len()
    }

    pub fn image_view_action_at(&self, column: u16, row: u16) -> Option<u8> {
        self.image_view_actions
            .borrow()
            .iter()
            .find(|(_, start, end, target_row)| {
                row == *target_row && column >= *start && column < *end
            })
            .map(|(action, _, _, _)| *action)
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

    pub fn running_tool_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    Entry::Tool {
                        status: ToolStatus::Running,
                        ..
                    }
                )
            })
            .count()
    }

    pub fn has_background_work(&self) -> bool {
        self.running_tool_count() > 0
            || self
                .subagent_tasks
                .values()
                .any(|task| task.status == "running")
            || self
                .workflow_runs
                .values()
                .any(|workflow| matches!(workflow.status.as_str(), "pending" | "running"))
    }

    /// Whether the currently visible view contains a time-based animation.
    ///
    /// Resident sessions that are running in another workspace only animate
    /// the dashboard. Treating them as globally animated forced a full
    /// transcript redraw every 100 ms and repeatedly reset the terminal cursor.
    pub fn needs_animation_frame(&self) -> bool {
        if self.scroll_drag.is_some()
            || self.scrollbar_dragging
            || self.image_processing_started_at.is_some()
        {
            return true;
        }
        match self.view {
            View::Dashboard => self
                .runtime_sessions
                .values()
                .any(|session| session.status == "running"),
            View::Transcript => {
                self.streaming
                    || self.active_compaction_started_at().is_some()
                    || self.has_background_work()
            }
            View::Workflows | View::WorkflowArtifact => self
                .workflow_runs
                .values()
                .any(|workflow| matches!(workflow.status.as_str(), "pending" | "running")),
            View::Tasks | View::Subagent => self
                .subagent_tasks
                .values()
                .any(|task| task.status == "running"),
            View::BlockViewer => false,
        }
    }

    /// Records the start of a new LLM turn. Called on local submission and
    /// retained when the first runtime or content event arrives. Snapshots
    /// `context_used` as the input-token count and resets the output-char
    /// accumulator so the working banner can show a meaningful ↑/↓ token
    /// tally while the model is generating.
    pub fn begin_turn_if_needed(&mut self) {
        if self.turn_started_at.is_none() {
            self.cache_miss_notice = None;
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
            AgentEvent::RuntimeState {
                idle,
                streaming,
                compacting,
                context_tokens,
                context_window,
                ..
            } => {
                if let Some(tokens) = context_tokens {
                    self.context_used = tokens;
                    self.context_known = true;
                }
                if let Some(window) = context_window {
                    self.context_limit = window;
                }
                if self.submission_pending && idle && !compacting {
                    self.streaming = true;
                } else {
                    if !idle || compacting || streaming {
                        self.submission_pending = false;
                    }
                    self.streaming = streaming || !idle;
                }
                if idle && !compacting && !self.submission_pending {
                    self.turn_started_at = None;
                }
            }
            AgentEvent::AppUpdate { status } => {
                self.app_update = (!matches!(status, AppUpdateStatus::Current)).then_some(status);
            }
            AgentEvent::RuntimeSessions { sessions } => {
                self.runtime_sessions = sessions
                    .into_iter()
                    .map(|session| (session.path.clone(), session))
                    .collect();
            }
            AgentEvent::SessionReset => {
                self.permission_mode = PermissionMode::Normal;
                self.view = View::Transcript;
                self.entries.clear();
                self.reset_entry_ids();
                self.clear_prompt();
                self.queued_steering.clear();
                self.queued_follow_up.clear();
                self.pending_permission = None;
                self.pending_oauth = None;
                self.pending_api_key_provider = None;
                self.overlay = OverlayKind::None;
                self.overlay_stack.clear();
                self.escape_armed_at = None;
                self.context_used = 0;
                self.context_known = false;
                self.scroll_from_bottom = 0;
                self.status = "session resumed".into();
                self.streaming = false;
                self.submission_pending = false;
                self.turn_started_at = None;
                self.turn_input_tokens = 0;
                self.turn_output_chars = 0;
                self.cache_miss_notice = None;
                self.expanded_tool_groups.clear();
                self.focused_tool = None;
                self.focused_entry = None;
                self.focused_section = None;
                self.focused_target_id = None;
                self.hovered_entry = None;
                self.hovered_target_id = None;
                self.workflow_runs.clear();
                self.workflow_selected = 0;
                self.workflow_artifact = None;
                self.subagent_tasks.clear();
                self.subagent_transcripts.clear();
                self.active_subagent_task_ids.clear();
                self.task_selected = 0;
                self.inspected_subagent = None;
                self.subagent_return_view = View::Transcript;
            }
            AgentEvent::PermissionModeChanged { mode } => {
                self.permission_mode = PermissionMode::from_wire(&mode);
            }
            AgentEvent::UserMessage { text } => {
                self.cache_miss_notice = None;
                if self.active_subagent_task_ids.iter().all(|id| {
                    self.subagent_tasks
                        .get(id)
                        .is_none_or(|task| task.status != "running")
                }) {
                    self.active_subagent_task_ids.clear();
                }
                self.entries.push(Entry::User {
                    text,
                    timestamp: String::new(),
                });
            }
            AgentEvent::ModelChanged { id, display_name } => {
                self.model = display_name;
                if let Some(limit) = self
                    .available_models
                    .iter()
                    .find(|model| model.id == id)
                    .and_then(|model| model.context_window)
                {
                    self.context_limit = limit;
                }
            }
            AgentEvent::ModelsChanged { models } => {
                self.available_models = models;
                self.status = format!("{} models available", self.available_models.len());
            }
            AgentEvent::SessionInfo { summary } => {
                self.entries.push(Entry::Assistant {
                    lines: vec![summary],
                    timestamp: String::new(),
                });
                self.status = "session info".into();
            }
            AgentEvent::SessionList { sessions } => {
                self.available_sessions = sessions;
                self.sync_current_session_workspace();
                self.dashboard_selected = self
                    .dashboard_selected
                    .min(self.available_sessions.len().saturating_sub(1));
                self.pending_session_path = None;
                if self.view != View::Dashboard {
                    self.open_overlay(OverlayKind::SessionPicker);
                }
                self.status = "sessions refreshed".into();
            }
            AgentEvent::SessionsChanged { sessions } => {
                self.available_sessions = sessions;
                self.sync_current_session_workspace();
                self.dashboard_selected = self
                    .dashboard_selected
                    .min(self.available_sessions.len().saturating_sub(1));
                self.status = "sessions refreshed".into();
            }
            AgentEvent::PromptPrefill { text } => {
                self.prompt = text;
                self.cursor = self.prompt.chars().count();
                self.paste_blocks.clear();
                self.focus = Focus::Prompt;
            }
            AgentEvent::ThinkingChanged { level } => self.thinking_level = level,
            AgentEvent::ThinkingOptions { levels } => {
                self.thinking_levels = levels;
                if self.pending_thinking_picker {
                    self.pending_thinking_picker = false;
                    self.open_overlay(OverlayKind::ThinkingPicker);
                }
            }
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
                self.runtime_extensions = resources.extensions;
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
                self.entries.push(Entry::Assistant {
                    lines: vec![format!(
                        "Authentication successful for {provider}. Model list refreshed; use /model to switch."
                    )],
                    timestamp: String::new(),
                });
            }
            AgentEvent::AuthChanged {
                provider,
                configured,
            } => {
                if let Some(info) = self
                    .available_auth_providers
                    .iter_mut()
                    .find(|info| info.id == provider)
                {
                    info.configured = configured;
                }
                self.status = if configured {
                    format!("updated credentials for {provider}")
                } else {
                    format!("removed credentials for {provider}")
                };
                self.entries.push(Entry::Assistant {
                    lines: vec![format!(
                        "{} Pi credentials for {provider}.",
                        if configured { "Updated" } else { "Removed" }
                    )],
                    timestamp: String::new(),
                });
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
                self.plan_entries = entries.clone();
                let complete = plan_entries_complete(&entries);
                if let Some(Entry::Plan {
                    entries: current,
                    expanded,
                }) = self.entries.iter_mut().rev().find(|entry| {
                    matches!(entry, Entry::Plan { entries, .. } if !plan_entries_complete(entries))
                }) {
                    *current = entries;
                    *expanded = !complete;
                } else if !entries.is_empty() {
                    self.entries.push(Entry::Plan {
                        entries,
                        expanded: !complete,
                    });
                }
                self.status = "plan updated".into();
            }
            AgentEvent::SessionTree { entries, user_only } => {
                self.session_tree = entries;
                self.tree_folded.clear();
                self.pending_tree_entry = None;
                self.open_overlay(if user_only {
                    OverlayKind::ForkPicker
                } else {
                    OverlayKind::TreePicker
                });
                let visible = self.filtered_tree();
                self.overlay_selected = if user_only {
                    visible.len().saturating_sub(1)
                } else {
                    visible
                        .iter()
                        .rposition(|entry| entry.active)
                        .unwrap_or_else(|| visible.len().saturating_sub(1))
                };
            }
            AgentEvent::TextDelta { text } => {
                self.submission_pending = false;
                self.begin_turn_if_needed();
                let added = text.chars().count() as u64;
                self.apply_text_delta(text);
                self.turn_output_chars = self.turn_output_chars.saturating_add(added);
                self.status = "generating…".into();
                self.streaming = true;
            }
            AgentEvent::ReasoningDelta { text } => {
                self.submission_pending = false;
                self.begin_turn_if_needed();
                let added = text.chars().count() as u64;
                self.append_reasoning_text(&text);
                self.turn_output_chars = self.turn_output_chars.saturating_add(added);
                self.status = "thinking…".into();
                self.streaming = true;
            }
            AgentEvent::CacheMiss {
                missed_tokens,
                missed_cost,
                idle_ms,
                model_changed,
            } => {
                self.cache_miss_notice = Some(CacheMissNotice {
                    missed_tokens,
                    missed_cost,
                    idle_ms,
                    model_changed,
                });
                self.status = "prompt cache miss noticed".into();
            }
            AgentEvent::ToolCallStart { id, name, args } => {
                if name.eq_ignore_ascii_case("update_plan") {
                    return;
                }
                if is_internal_subagent_tool(&name) {
                    self.status = "starting subagent…".into();
                    return;
                }
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
            AgentEvent::SubagentUpdate { task } => {
                if !self.active_subagent_task_ids.contains(&task.task_id) {
                    self.active_subagent_task_ids.push(task.task_id.clone());
                }
                if task.status == "running" {
                    self.status = format!("subagent working: {}", task.description);
                } else if self
                    .subagent_tasks
                    .values()
                    .all(|other| other.task_id == task.task_id || other.status != "running")
                {
                    self.status = if task.status == "completed" {
                        "idle".into()
                    } else {
                        format!("subagent {}", task.status)
                    };
                }
                self.subagent_tasks.insert(task.task_id.clone(), *task);
                self.task_selected = self
                    .task_selected
                    .min(self.subagent_tasks.len().saturating_sub(1));
                self.rebuild_active_subagent_entries();
            }
            AgentEvent::SubagentTranscript { task_id, event } => {
                let transcript = self.subagent_transcripts.entry(task_id).or_default();
                match *event {
                    AgentEvent::ReasoningDelta { text } => {
                        if let Some(AgentEvent::ReasoningDelta { text: current }) =
                            transcript.last_mut()
                        {
                            current.push_str(&text);
                        } else {
                            transcript.push(AgentEvent::ReasoningDelta { text });
                        }
                    }
                    AgentEvent::TextDelta { text } => {
                        if let Some(AgentEvent::TextDelta { text: current }) = transcript.last_mut()
                        {
                            current.push_str(&text);
                        } else {
                            transcript.push(AgentEvent::TextDelta { text });
                        }
                    }
                    AgentEvent::ToolCallResult {
                        id,
                        mut result,
                        is_error,
                        duration_ms,
                    } => {
                        result.content.clear();
                        result.details = None;
                        transcript.push(AgentEvent::ToolCallResult {
                            id,
                            result,
                            is_error,
                            duration_ms,
                        });
                    }
                    event @ (AgentEvent::UserMessage { .. }
                    | AgentEvent::ToolCallStart { .. }
                    | AgentEvent::Error { .. }) => transcript.push(event),
                    _ => {}
                }
            }
            AgentEvent::WorkflowUpdate { workflow } => {
                let workflow = *workflow;
                self.status = match workflow.status.as_str() {
                    "paused" => format!("workflow waiting: {}", workflow.name),
                    "failed" => format!("workflow failed: {}", workflow.name),
                    "running" => format!("workflow running: {}", workflow.name),
                    _ => format!("workflow {}: {}", workflow.status, workflow.name),
                };
                self.workflow_runs.insert(workflow.run_id.clone(), workflow);
                self.workflow_selected = self
                    .workflow_selected
                    .min(self.workflow_runs.len().saturating_sub(1));
            }
            AgentEvent::WorkflowCatalog { workflows } => {
                self.workflow_catalog = workflows;
                self.status = format!("{} workflows available", self.workflow_catalog.len());
                self.open_overlay(OverlayKind::WorkflowPicker);
            }
            AgentEvent::WorkflowPreview { preview } => {
                let preview = *preview;
                self.status = format!("workflow preflight: {}", preview.name);
                self.workflow_preview = Some(preview);
                self.open_overlay(OverlayKind::WorkflowPreview);
            }
            AgentEvent::WorkflowArtifact { artifact } => {
                self.workflow_artifact = Some(*artifact);
                self.view = View::WorkflowArtifact;
                self.scroll_from_bottom = 0;
                self.status = "workflow artifact loaded".into();
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
                    // Live Pi events carry the duration measured by the sidecar.
                    // Historical sessions may not have persisted one; timing the
                    // synchronous replay produces a misleading "0ms" instead of
                    // the original tool duration, so leave it blank.
                    started_at.take();
                    *duration = duration_ms.map(|milliseconds| {
                        format_elapsed(std::time::Duration::from_millis(milliseconds))
                    });
                    *current_result = Some(content);
                    if label == "Search"
                        && !detail.contains(" matches)")
                        && let Some(count) =
                            search_match_count(current_result.as_deref().unwrap_or_default())
                    {
                        detail.push_str(&format!(" ({count} matches)"));
                    }
                    *status = if is_error {
                        ToolStatus::Error
                    } else {
                        ToolStatus::Success
                    };
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
                self.context_known = true;
                self.turn_input_tokens = input_tokens;
                self.turn_output_chars = 0;
                self.turn_started_at = None;
                let running = self.running_tool_count();
                self.status = if running == 0 {
                    "idle".into()
                } else {
                    format!("{running} background task(s) running")
                };
                self.streaming = false;
                self.submission_pending = false;
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
                self.submission_pending = false;
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

fn plan_entries_complete(entries: &[pi_harness::PlanEntry]) -> bool {
    !entries.is_empty() && entries.iter().all(|entry| entry.status == "completed")
}

fn model_provider(model: &ModelInfo) -> &str {
    model
        .id
        .split_once('/')
        .map_or("unknown", |(provider, _)| provider)
}

fn builtin_description(command: &str) -> &'static str {
    match command {
        "/dashboard" => "Open the session dashboard",
        "/home" | "/welcome" => "Return to the welcome screen",
        "/workflow" => "Start a durable workflow: /workflow <name> <task>",
        "/workflows" => "Open durable workflow runs and checkpoint controls",
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
        "/paste" => "Paste clipboard text or attach an image",
        "/scoped-models" => "Choose models for cycling",
        "/subagent-model" => "Choose the persistent model for native subagents",
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
        "/quit" => "Quit Torii",
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

fn is_image_reference(path: &str) -> bool {
    matches!(
        std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg"
    )
}

fn char_to_byte(value: &str, character_index: usize) -> usize {
    value
        .char_indices()
        .nth(character_index)
        .map_or(value.len(), |(index, _)| index)
}

fn auth_provider_label(provider: &AuthProviderInfo) -> String {
    let auth = match provider.auth_type {
        AuthType::ApiKey => "API key",
        AuthType::Oauth => "OAuth",
    };
    format!(
        "{}  · {}{}",
        provider.display_name,
        auth,
        if provider.configured {
            "  · configured"
        } else {
            ""
        }
    )
}

pub(crate) fn session_label(session: &SessionInfo, show_path: bool) -> String {
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
    let modified = format_session_age(session.modified_at_ms, unix_time_ms());
    let path = if show_path {
        format!("  · {}", session.path)
    } else {
        String::new()
    };
    format!(
        "{title}  · {modified}  · {} msg{path}",
        session.message_count
    )
}

pub(crate) fn format_session_age(modified_at_ms: u64, now_ms: u64) -> String {
    if modified_at_ms == 0 {
        return "unknown".into();
    }
    let elapsed_seconds = now_ms.saturating_sub(modified_at_ms) / 1_000;
    match elapsed_seconds {
        0..60 => "now".into(),
        60..3_600 => format!("{}m ago", elapsed_seconds / 60),
        3_600..86_400 => format!("{}h ago", elapsed_seconds / 3_600),
        86_400..604_800 => format!("{}d ago", elapsed_seconds / 86_400),
        604_800..31_536_000 => format!("{}w ago", elapsed_seconds / 604_800),
        _ => format!("{}y ago", elapsed_seconds / 31_536_000),
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn session_search_text(session: &SessionInfo) -> String {
    format!(
        "{} {} {} {} {}",
        session.id,
        session.name.as_deref().unwrap_or_default(),
        session.first_message,
        session.path,
        session.cwd
    )
    .to_ascii_lowercase()
}

fn threaded_session_order(sessions: Vec<&SessionInfo>) -> Vec<&SessionInfo> {
    let paths = sessions
        .iter()
        .map(|session| session.path.as_str())
        .collect::<HashSet<_>>();
    let mut children: std::collections::HashMap<Option<&str>, Vec<&SessionInfo>> =
        std::collections::HashMap::new();
    for session in sessions {
        let parent = session
            .parent_session_path
            .as_deref()
            .filter(|parent| paths.contains(parent));
        children.entry(parent).or_default().push(session);
    }
    for siblings in children.values_mut() {
        siblings.sort_by_key(|session| std::cmp::Reverse(session.modified_at_ms));
    }
    let mut ordered = Vec::new();
    let mut stack = children.get(&None).cloned().unwrap_or_default();
    stack.reverse();
    while let Some(session) = stack.pop() {
        ordered.push(session);
        if let Some(descendants) = children.get(&Some(session.path.as_str())) {
            stack.extend(descendants.iter().rev().copied());
        }
    }
    ordered
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
        format!(
            "  {}",
            entry.label_timestamp.as_deref().unwrap_or(&entry.timestamp)
        )
    } else {
        String::new()
    };
    format!("{branch} {indent}{role}: {}{label}{timestamp}", entry.text)
}

fn tree_entry_matches_query(entry: &SessionTreeEntry, query: &str) -> bool {
    query.is_empty()
        || entry.text.to_ascii_lowercase().contains(query)
        || entry
            .role
            .as_deref()
            .is_some_and(|role| role.to_ascii_lowercase().contains(query))
        || entry
            .label
            .as_deref()
            .is_some_and(|label| label.to_ascii_lowercase().contains(query))
}

fn active_first_tree_order<'a>(
    visible: Vec<&'a SessionTreeEntry>,
    all: &std::collections::HashMap<&str, &'a SessionTreeEntry>,
) -> Vec<&'a SessionTreeEntry> {
    let visible_ids = visible
        .iter()
        .map(|entry| entry.id.as_str())
        .collect::<HashSet<_>>();
    let mut children: std::collections::HashMap<Option<&str>, Vec<&SessionTreeEntry>> =
        std::collections::HashMap::new();
    for entry in visible {
        let mut parent = entry.parent_id.as_deref();
        while parent.is_some_and(|id| !visible_ids.contains(id)) {
            parent = parent.and_then(|id| all.get(id).and_then(|item| item.parent_id.as_deref()));
        }
        children.entry(parent).or_default().push(entry);
    }
    for siblings in children.values_mut() {
        siblings.sort_by_key(|entry| !entry.active);
    }
    let mut ordered = Vec::new();
    let mut stack = children.get(&None).cloned().unwrap_or_default();
    stack.reverse();
    while let Some(entry) = stack.pop() {
        ordered.push(entry);
        if let Some(descendants) = children.get(&Some(entry.id.as_str())) {
            stack.extend(descendants.iter().rev().copied());
        }
    }
    ordered
}

fn tool_display(name: &str, args: &serde_json::Value) -> (String, String) {
    let normalized = name.to_ascii_lowercase();
    let label = match normalized.as_str() {
        "bash" | "shell" | "run" | "run_terminal_command" | "run_terminal_cmd" => "Run".to_string(),
        "read" | "read_file" | "read_files" => "Read".to_string(),
        "ls" | "list" | "list_dir" => "List".to_string(),
        "find" | "find_file" | "find_files" | "glob" => "Find".to_string(),
        "edit" => "Edit".to_string(),
        "write" => "Write".to_string(),
        "search" | "grep" | "grep_file" | "grep_files" | "web_search" => "Search".to_string(),
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
    if normalized == "read_files" {
        detail = format_string_list(args, "paths", "file");
    } else if normalized == "find_files" {
        detail = format_query_list(args, "pattern", "pattern");
    } else if normalized == "grep_files" {
        detail = format_query_list(args, "pattern", "query");
    } else if label == "Search" {
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

fn format_string_list(args: &serde_json::Value, key: &str, noun: &str) -> String {
    let values: Vec<_> = args
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    match values.as_slice() {
        [] => args.to_string(),
        [only] => (*only).to_string(),
        values => {
            let preview = values
                .iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{} {noun}s · {preview}{}",
                values.len(),
                if values.len() > 3 { ", …" } else { "" }
            )
        }
    }
}

fn format_query_list(args: &serde_json::Value, key: &str, noun: &str) -> String {
    let queries: Vec<_> = args
        .get("queries")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_object)
        .collect();
    if queries.is_empty() {
        return args.to_string();
    }
    let render = |query: &&serde_json::Map<String, serde_json::Value>| {
        let value = query
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        let path = query
            .get("path")
            .and_then(serde_json::Value::as_str)
            .filter(|path| !path.is_empty() && *path != ".");
        path.map_or_else(
            || format!("\"{value}\""),
            |path| format!("\"{value}\" in {path}"),
        )
    };
    match queries.as_slice() {
        [only] => render(only),
        queries => {
            let preview = queries
                .iter()
                .take(2)
                .map(render)
                .collect::<Vec<_>>()
                .join("; ");
            let plural = if let Some(stem) = noun.strip_suffix('y') {
                format!("{stem}ies")
            } else {
                format!("{noun}s")
            };
            format!(
                "{} {plural} · {preview}{}",
                queries.len(),
                if queries.len() > 2 { "; …" } else { "" }
            )
        }
    }
}

fn is_internal_subagent_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "spawn_subagent"
            | "get_command_or_subagent_output"
            | "wait_commands_or_subagents"
            | "get_subagent_result"
    )
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
            lines.extend(replacement_diff(old, new));
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
    Some(replacement_diff(old_text, new_text))
}

/// Build a preview diff from an edit tool call's arguments.
///
/// This runs before the tool executes (e.g. while a permission prompt is up),
/// so the authoritative `details.diff` in the result is not available yet. We
/// still want the preview to match the final diff, so we compute a real
/// line-level diff instead of dumping the whole old block as removed and the
/// whole new block as added — a one-line change inside a larger block should
/// read as one `-`/`+` pair, not `-N +N`. Line numbers are left unset because
/// the arguments carry text, not file positions; the result diff fills them in.
fn replacement_diff(old_text: &str, new_text: &str) -> Vec<DiffLine> {
    let old_lines: Vec<&str> = old_text.split_terminator('\n').collect();
    let new_lines: Vec<&str> = new_text.split_terminator('\n').collect();
    let rows = old_lines.len();
    let cols = new_lines.len();

    // Longest-common-subsequence table over lines. Edit payloads are small, so
    // the O(rows * cols) table is cheap and keeps the diff line-accurate.
    let mut lcs = vec![vec![0u32; cols + 1]; rows + 1];
    for i in (0..rows).rev() {
        for j in (0..cols).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut diff_lines = Vec::new();
    let push = |diff_lines: &mut Vec<DiffLine>, text: &str, kind: DiffKind| {
        diff_lines.push(DiffLine {
            number: None,
            text: text.to_string(),
            kind,
        });
    };
    let (mut i, mut j) = (0, 0);
    while i < rows && j < cols {
        if old_lines[i] == new_lines[j] {
            push(&mut diff_lines, old_lines[i], DiffKind::Context);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            push(&mut diff_lines, old_lines[i], DiffKind::Removed);
            i += 1;
        } else {
            push(&mut diff_lines, new_lines[j], DiffKind::Added);
            j += 1;
        }
    }
    while i < rows {
        push(&mut diff_lines, old_lines[i], DiffKind::Removed);
        i += 1;
    }
    while j < cols {
        push(&mut diff_lines, new_lines[j], DiffKind::Added);
        j += 1;
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
    let numbered = diff
        .lines()
        .filter_map(parse_numbered_diff_line)
        .collect::<Vec<_>>();
    if numbered.iter().any(|line| line.number.is_some())
        && !diff.lines().any(|line| line.starts_with("@@ "))
    {
        return numbered;
    }

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
        } else if line.trim() == "..." {
            continue;
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

fn parse_numbered_diff_line(line: &str) -> Option<DiffLine> {
    let marker = line.chars().next()?;
    if !matches!(marker, ' ' | '+' | '-') {
        return None;
    }
    // Pi pads its compact diff columns (`  602`, `- 602`, `+ 606`). The
    // padding is presentation, not another line-number column.
    let remainder = line[marker.len_utf8()..].trim_start();
    let digits = remainder
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        if matches!(marker, '+' | '-') {
            // Compact Pi diffs number context lines but leave changed lines as
            // `-     source` / `+     source`. Keep the source indentation;
            // trimming it makes removed/added code appear malformed.
            return Some(DiffLine {
                number: None,
                text: line[marker.len_utf8()..].to_string(),
                kind: if marker == '+' {
                    DiffKind::Added
                } else {
                    DiffKind::Removed
                },
            });
        }
        let text = remainder.trim();
        return (text == "...").then(|| DiffLine {
            number: None,
            text: text.into(),
            kind: DiffKind::Context,
        });
    }
    let text = remainder[digits.len()..]
        .strip_prefix(' ')
        .unwrap_or(&remainder[digits.len()..]);
    Some(DiffLine {
        number: digits.parse().ok(),
        text: text.into(),
        kind: match marker {
            '+' => DiffKind::Added,
            '-' => DiffKind::Removed,
            _ => DiffKind::Context,
        },
    })
}

fn parse_diff_range(range: &str) -> Option<u32> {
    range
        .trim_start_matches(['-', '+'])
        .split(',')
        .next()?
        .parse()
        .ok()
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
                state.context_known = true;
            }
            state.status = if latest_error.is_some() {
                "compaction failed".into()
            } else {
                "compacted".into()
            };
        }
    }
}
