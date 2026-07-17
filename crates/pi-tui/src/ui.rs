use std::collections::HashSet;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::{
    actions::{self, ActionContext, ActionId},
    agent_layout::AgentLayout,
    markdown,
    state::{AppState, DiffKind, DiffLine, Entry, Focus, OverlayKind, ToolStatus, View},
    theme::Theme,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionHit {
    pub index: usize,
    pub actionable: bool,
    pub id: String,
}

#[derive(Clone, Debug)]
struct LayoutSection {
    id: String,
    index: usize,
    start: usize,
    end: usize,
    actionable: bool,
}

#[derive(Clone, Debug)]
struct TranscriptLayout {
    total_rows: usize,
    sections: Vec<LayoutSection>,
}

impl TranscriptLayout {
    fn build(state: &AppState, width: usize) -> Self {
        let (total_rows, sections) = build_layout_sections(state, width);
        Self {
            total_rows,
            sections,
        }
    }

    fn max_scroll(&self, viewport: usize) -> usize {
        self.total_rows.saturating_sub(viewport)
    }
}

fn transcript_geometry(state: &AppState, width: u16, height: u16) -> (u16, u16, u16, usize) {
    if let Some((x, y, width, height)) = state.transcript_rect.get() {
        return (x, y, width, usize::from(height));
    }
    let layout = AgentLayout::compute(Rect::new(0, 0, width, height), state);
    let content = layout.scrollback.inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    (
        content.x,
        content.y,
        content.width,
        usize::from(content.height),
    )
}

fn build_layout_sections(state: &AppState, width: usize) -> (usize, Vec<LayoutSection>) {
    let theme = state.theme();
    let mut row = 0;
    let mut sections = Vec::new();
    let mut index = 0;
    while index < state.entries.len() {
        let section_index = index;
        let start = row;
        let mut actionable = false;
        match &state.entries[index] {
            Entry::User { .. } => row += 5,
            Entry::Reasoning {
                text,
                active,
                expanded,
            } => {
                row += reasoning_lines(text, *active, *expanded, false, width, theme).len();
                actionable = true;
            }
            Entry::Diff {
                path,
                lines,
                expanded,
                ..
            } => {
                row += diff_render_lines(path, lines, *expanded, false, width, theme).len();
                actionable = true;
            }
            Entry::Plan { entries, expanded } => {
                row += plan_lines(entries, *expanded, width, theme).len();
                actionable = true;
            }
            Entry::Tool { .. } => {
                let Entry::Tool { id, label, .. } = &state.entries[index] else {
                    unreachable!()
                };
                let count = state.entries[index..]
                    .iter()
                    .take_while(
                        |entry| matches!(entry, Entry::Tool { label: other, .. } if other == label),
                    )
                    .count();
                let orchestration = label == "Agent" && id.starts_with("subagent:");
                if count > 1 || orchestration {
                    sections.push(LayoutSection {
                        id: format!("tool-group:{id}"),
                        index,
                        start: row,
                        end: row + 1,
                        actionable: true,
                    });
                    row += 1;
                    if state.expanded_tool_groups.contains(&index) {
                        for child in index..index + count {
                            let child_start = row;
                            row += if orchestration {
                                orchestration_task_lines(
                                    state,
                                    child,
                                    child + 1 == index + count,
                                    width,
                                    theme,
                                )
                                .len()
                            } else {
                                tool_item_line_count(&state.entries[child], width, true, theme)
                            };
                            let Entry::Tool { id, .. } = &state.entries[child] else {
                                unreachable!()
                            };
                            sections.push(LayoutSection {
                                id: format!("tool:{id}"),
                                index: child,
                                start: child_start,
                                end: row.max(child_start + 1),
                                actionable: true,
                            });
                        }
                    }
                    index += count;
                    continue;
                }
                row += tool_item_line_count(&state.entries[index], width, false, theme);
                actionable = true;
            }
            Entry::Compaction {
                summary,
                tokens_before,
                tokens_after,
                active,
                error,
                ..
            } => {
                row += 1 + compaction_lines(
                    summary,
                    *tokens_before,
                    *tokens_after,
                    *active,
                    error.as_deref(),
                    width,
                    theme,
                )
                .len();
            }
            Entry::CompactionIndicator { tokens_before, .. } => {
                row += 1 + compaction_indicator_line(*tokens_before, width, theme).len();
            }
            Entry::Assistant { lines, .. } => {
                row += 1 + markdown::render(lines, width, theme).len()
            }
        }
        sections.push(LayoutSection {
            id: state
                .entry_target_id(section_index)
                .expect("layout section entry exists"),
            index: section_index,
            start,
            end: row.max(start + 1),
            actionable,
        });
        index += 1;
    }
    (row, sections)
}

fn tool_item_line_count(entry: &Entry, width: usize, nested: bool, theme: Theme) -> usize {
    let Entry::Tool {
        label,
        detail,
        status,
        duration,
        started_at,
        result,
        expanded,
        ..
    } = entry
    else {
        return 0;
    };
    tool_render_lines(
        ToolRender {
            label,
            detail,
            status: *status,
            result: result.as_deref(),
            expanded: *expanded,
            duration: duration.as_deref(),
            started_at: *started_at,
            nested,
            focused: false,
            hovered: false,
            waiting_for_user: false,
        },
        width,
        theme,
    )
    .len()
}

pub fn section_hit_at(
    state: &AppState,
    width: u16,
    height: u16,
    column: u16,
    screen_row: u16,
) -> Option<SectionHit> {
    let (x, y, content_width, viewport) = transcript_geometry(state, width, height);
    if column < x
        || column >= x.saturating_add(content_width)
        || screen_row < y
        || usize::from(screen_row - y) >= viewport
    {
        return None;
    }
    if let Some((id, index, _, _, actionable)) = state
        .transcript_hit_regions
        .borrow()
        .iter()
        .find(|(_, _, start, end, _)| screen_row >= *start && screen_row < *end)
        .cloned()
    {
        return Some(SectionHit {
            id,
            index,
            actionable,
        });
    }
    let render_width = content_width.saturating_sub(1) as usize;
    let layout = TranscriptLayout::build(state, render_width);
    let max_scroll = layout.max_scroll(viewport);
    let scroll = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    let logical = usize::from(screen_row - y).saturating_add(scroll);
    layout
        .sections
        .into_iter()
        .find(|section| logical >= section.start && logical < section.end)
        .map(|section| SectionHit {
            index: section.index,
            actionable: section.actionable,
            id: section.id,
        })
}

pub fn move_section_focus(state: &mut AppState, width: u16, height: u16, direction: i32) {
    let (_, _, content_width, viewport) = transcript_geometry(state, width, height);
    let layout = TranscriptLayout::build(state, content_width.saturating_sub(1) as usize);
    let sections = &layout.sections;
    if sections.is_empty() {
        return;
    }
    let current = state
        .focused_target_id
        .as_ref()
        .and_then(|id| sections.iter().position(|section| &section.id == id))
        .or_else(|| {
            state
                .focused_section
                .filter(|position| *position < sections.len())
        });
    let next = if direction < 0 {
        current.map_or(sections.len() - 1, |position| position.saturating_sub(1))
    } else {
        current.map_or(0, |position| (position + 1).min(sections.len() - 1))
    };
    select_layout_section(state, &layout, next, viewport);
}

/// Give the transcript focus using Grok's activation behavior: preserve a
/// still-visible selection, otherwise select the last visible/selectable
/// block. This is deliberately layout-aware so a child hidden behind a
/// collapsed tool group can never become an invisible focus target.
pub fn focus_scrollback(state: &mut AppState, width: u16, height: u16) {
    state.focus = Focus::Scrollback;
    let (_, _, content_width, viewport) = transcript_geometry(state, width, height);
    let layout = TranscriptLayout::build(state, content_width.saturating_sub(1) as usize);
    if layout.sections.is_empty() {
        state.focused_section = None;
        state.focused_target_id = None;
        state.focused_entry = None;
        state.focused_tool = None;
        return;
    }
    let selected = state
        .focused_target_id
        .as_ref()
        .and_then(|id| layout.sections.iter().position(|section| &section.id == id))
        .unwrap_or(layout.sections.len() - 1);
    select_layout_section(state, &layout, selected, viewport);
}

fn select_layout_section(
    state: &mut AppState,
    layout: &TranscriptLayout,
    position: usize,
    viewport: usize,
) {
    let section = &layout.sections[position];
    let (index, start, end) = (section.index, section.start, section.end);
    state.focused_section = Some(position);
    state.focused_target_id = Some(section.id.clone());
    state.focused_entry = Some(index);
    state.focused_tool =
        matches!(state.entries.get(index), Some(Entry::Tool { .. })).then_some(index);
    let max_scroll = layout.max_scroll(viewport);
    let mut top = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    if end.saturating_sub(start) > viewport || start < top {
        top = start;
    } else if end > top.saturating_add(viewport) {
        top = end.saturating_sub(viewport);
    }
    state.scroll_from_bottom = max_scroll.saturating_sub(top.min(max_scroll));
}

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    let theme = state.theme();
    // Clear symbols as well as styles. This matters when a resume replaces a
    // tall focused/hovered section with shorter content in terminals that keep
    // the previous alternate-screen cells until explicitly overwritten.
    frame.render_widget(Clear, frame.area());
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background).fg(theme.foreground)),
        frame.area(),
    );

    if state.view == View::Dashboard {
        render_dashboard(frame, state, theme);
        crate::overlay::render(frame, state);
        return;
    }
    if state.view == View::BlockViewer {
        render_block_viewer(frame, state, theme);
        return;
    }
    if state.view == View::Workflows {
        render_workflows(frame, state, theme);
        return;
    }
    if state.view == View::WorkflowArtifact {
        render_workflow_artifact(frame, state, theme);
        return;
    }
    if state.view == View::Tasks {
        render_tasks(frame, state, theme);
        return;
    }
    if state.view == View::Subagent {
        render_subagent(frame, state, theme);
        return;
    }

    let layout = AgentLayout::compute(frame.area(), state);
    let compaction_active = state.active_compaction_started_at().is_some();
    let working_active = state.turn_started_at.is_some()
        || state.image_processing_started_at.is_some()
        || state
            .subagent_tasks
            .values()
            .any(|task| task.status == "running");

    render_header(frame, layout.status, state, theme);
    render_transcript(frame, layout.scrollback, state, theme);
    if layout.queue.height > 0 {
        render_queue(frame, layout.queue, state, theme);
    }
    if working_active {
        render_working_banner(frame, layout.turn_status, state, theme);
    }
    if compaction_active {
        render_compaction_banner(frame, layout.compaction, state, theme);
    }
    if state.pending_permission.is_some() {
        render_permission_panel(frame, layout.permission, state, theme);
    }
    render_composer(frame, layout.prompt, state, theme);
    render_shortcuts(frame, layout.shortcuts, state, theme);
    if state.overlay != OverlayKind::Permission {
        crate::overlay::render(frame, state);
    }
}

fn render_permission_panel(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let Some(permission) = &state.pending_permission else {
        return;
    };
    let inner_width = usize::from(area.width.saturating_sub(4)).max(1);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            " Permission required ",
            Style::default().fg(theme.background).bg(theme.warning),
        ),
        Span::styled(
            format!("  {}", permission.tool),
            Style::default().fg(theme.warning),
        ),
    ])];
    lines.push(Line::from(Span::styled(
        format!(
            "  {}",
            truncate(&permission.reason, inner_width.saturating_sub(2))
        ),
        Style::default().fg(theme.text_secondary),
    )));
    if area.height > 4 {
        lines.push(Line::raw(""));
    }
    let options = ["Allow once", "Always allow", "Deny"];
    let mut option_spans = vec![Span::raw("  ")];
    for (index, option) in options.iter().enumerate() {
        let selected = index == state.overlay_selected;
        option_spans.push(Span::styled(
            format!(" {} ", option),
            if selected {
                Style::default().fg(theme.background).bg(theme.foreground)
            } else {
                Style::default().fg(theme.muted)
            },
        ));
        option_spans.push(Span::raw("  "));
    }
    lines.push(Line::from(option_spans));
    lines.push(Line::from(Span::styled(
        "  ←/→ choose  Enter confirm",
        Style::default().fg(theme.subtle),
    )));
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.background)),
        area,
    );
}

fn render_queue(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let total = state.queued_steering.len() + state.queued_follow_up.len();
    let mut lines = vec![Line::from(vec![
        Span::styled("Queued", Style::default().fg(theme.gray_bright)),
        Span::styled(format!("  {total}"), Style::default().fg(theme.subtle)),
    ])];
    let width = usize::from(area.width.saturating_sub(12));
    for (kind, text) in state
        .queued_steering
        .iter()
        .map(|text| ("now", text))
        .chain(state.queued_follow_up.iter().map(|text| ("next", text)))
        .take(usize::from(area.height.saturating_sub(1)))
    {
        lines.push(Line::from(vec![
            Span::styled("  ├ ", Style::default().fg(theme.subtle)),
            Span::styled(
                format!("{kind:<4} "),
                Style::default().fg(if kind == "now" {
                    theme.accent_running
                } else {
                    theme.accent_tool
                }),
            ),
            Span::styled(
                truncate(text, width),
                Style::default().fg(theme.text_secondary),
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.background)),
        area,
    );
}

fn render_workflows(frame: &mut Frame<'_>, state: &AppState, theme: Theme) {
    let area = frame.area().inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(area);
    let workflows = state.sorted_workflows();
    let mut list = Vec::new();
    for (index, workflow) in workflows.iter().enumerate() {
        let selected = index == state.workflow_selected;
        let color = match workflow.status.as_str() {
            "completed" => theme.success,
            "failed" => theme.error,
            "cancelled" => theme.muted,
            "paused" => theme.accent,
            _ => theme.foreground,
        };
        list.push(Line::from(vec![
            Span::styled(
                if selected { "> " } else { "  " },
                Style::default().fg(theme.accent),
            ),
            Span::styled(
                workflow.name.clone(),
                Style::default().fg(if selected { theme.foreground } else { color }),
            ),
            Span::styled(format!("  {}", workflow.status), Style::default().fg(color)),
        ]));
        list.push(Line::from(Span::styled(
            format!(
                "    {}/{} steps  {}",
                workflow.completed_steps, workflow.total_steps, workflow.run_id
            ),
            Style::default().fg(theme.muted),
        )));
    }
    if list.is_empty() {
        list.push(Line::from(Span::styled(
            "No workflows in this session",
            Style::default().fg(theme.muted),
        )));
    }
    frame.render_widget(
        Paragraph::new(list).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Workflows ")
                .border_style(Style::default().fg(theme.accent)),
        ),
        columns[0],
    );

    let mut details = Vec::new();
    if let Some(workflow) = state.selected_workflow() {
        if let Some(description) = &workflow.description {
            details.push(Line::from(Span::styled(
                description.clone(),
                Style::default().fg(theme.muted),
            )));
            details.push(Line::raw(""));
        }
        if let Some(budget) = workflow.budget.as_ref() {
            let mut usage = Vec::new();
            if let Some(limit) = budget.max_agent_attempts {
                usage.push(format!("attempts {}/{}", budget.agent_attempts, limit));
            }
            if let Some(limit) = budget.max_prompt_tokens {
                usage.push(format!(
                    "prompt {}+{} reserved/{}",
                    budget.prompt_tokens, budget.reserved_prompt_tokens, limit
                ));
            }
            if let Some(limit) = budget.max_output_tokens {
                usage.push(format!(
                    "output {}+{} reserved/{}",
                    budget.output_tokens, budget.reserved_output_tokens, limit
                ));
            }
            if let Some(limit) = budget.max_cache_write_tokens {
                usage.push(format!(
                    "cache-write {}+{} reserved/{}",
                    budget.cache_write_tokens, budget.reserved_cache_write_tokens, limit
                ));
            }
            details.push(Line::from(Span::styled(
                format!(" Budget  {}", usage.join(" Â· ")),
                Style::default().fg(theme.accent),
            )));
            if budget.unknown_usage_attempts > 0 {
                details.push(Line::from(Span::styled(
                    format!(
                        "         {} launched attempt(s) have unknown usage",
                        budget.unknown_usage_attempts
                    ),
                    Style::default().fg(theme.error),
                )));
            }
            details.push(Line::raw(""));
        }
        for provider in &workflow.provider_states {
            let mut state = vec![format!(
                "circuit {} Â· {} active Â· {} starts Â· {} failure streak",
                provider.circuit,
                provider.active_attempts,
                provider.starts_in_window,
                provider.consecutive_failures
            )];
            if let Some(limit) = provider.max_concurrency {
                state.push(format!("max {limit} concurrent"));
            }
            details.push(Line::from(Span::styled(
                format!(" Provider {}  {}", provider.provider, state.join(" Â· ")),
                Style::default().fg(if provider.circuit == "closed" {
                    theme.muted
                } else {
                    theme.warning
                }),
            )));
        }
        if !workflow.provider_states.is_empty() {
            details.push(Line::raw(""));
        }
        for step in &workflow.steps {
            let marker = match step.status.as_str() {
                "completed" => "[x]",
                "skipped" => "[~]",
                "running" => "[>]",
                "waiting" => "[!]",
                "failed" => "[x]",
                "cancelled" => "[-]",
                _ => "[ ]",
            };
            let color = match step.status.as_str() {
                "completed" => theme.success,
                "skipped" => theme.muted,
                "failed" => theme.error,
                "waiting" | "running" => theme.accent,
                _ => theme.muted,
            };
            details.push(Line::from(vec![
                Span::styled(format!(" {marker} "), Style::default().fg(color)),
                Span::styled(step.id.clone(), Style::default().fg(theme.foreground)),
                Span::styled(format!("  {}", step.status), Style::default().fg(color)),
            ]));
            let metadata = [step.role.as_deref(), step.model.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            if !metadata.is_empty() {
                details.push(Line::from(Span::styled(
                    format!("       {metadata}"),
                    Style::default().fg(theme.muted),
                )));
            }
            if let Some(error) = &step.error {
                details.push(Line::from(Span::styled(
                    format!("       {error}"),
                    Style::default().fg(theme.error),
                )));
            }
            if !step.artifact_ids.is_empty() {
                details.push(Line::from(Span::styled(
                    format!("       artifacts: {}", step.artifact_ids.join(", ")),
                    Style::default().fg(theme.muted),
                )));
            }
            if step.attempt_count > 0
                || step.timeout_ms.is_some()
                || step.max_attempts.is_some()
                || step.output_contract.is_some()
                || step.condition.is_some()
            {
                let mut policy = Vec::new();
                if step.attempt_count > 0 {
                    policy.push(format!(
                        "{} attempt{}",
                        step.attempt_count,
                        if step.attempt_count == 1 { "" } else { "s" }
                    ));
                }
                if let Some(timeout) = step.timeout_ms {
                    policy.push(format!("{} timeout", format_elapsed(timeout)));
                }
                if let Some(max_attempts) = step.max_attempts {
                    policy.push(format!("max {max_attempts}"));
                }
                if let Some(contract) = &step.output_contract {
                    policy.push(format!("output {contract}"));
                }
                if let Some(condition) = &step.condition {
                    policy.push(format!("when {condition}"));
                }
                details.push(Line::from(Span::styled(
                    format!("       {}", policy.join(" · ")),
                    Style::default().fg(theme.muted),
                )));
            }
            if let Some(observability) = &step.observability {
                append_workflow_observability(&mut details, observability, "       ", theme);
            }
            for child in &step.children {
                let marker = match child.status.as_str() {
                    "completed" => "[x]",
                    "skipped" => "[~]",
                    "running" => "[>]",
                    "waiting" => "[!]",
                    "failed" => "[x]",
                    "cancelled" => "[-]",
                    _ => "[ ]",
                };
                let color = match child.status.as_str() {
                    "completed" => theme.success,
                    "failed" => theme.error,
                    "waiting" | "running" => theme.accent,
                    _ => theme.muted,
                };
                details.push(Line::from(vec![
                    Span::styled(format!("    {marker} "), Style::default().fg(color)),
                    Span::styled(child.id.clone(), Style::default().fg(theme.foreground)),
                    Span::styled(format!("  {}", child.status), Style::default().fg(color)),
                ]));
                let mut metadata = [child.role.clone(), child.model.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                if child.attempt_count > 0 {
                    metadata.push(format!(
                        "{} attempt{}",
                        child.attempt_count,
                        if child.attempt_count == 1 { "" } else { "s" }
                    ));
                }
                if let Some(timeout) = child.timeout_ms {
                    metadata.push(format!("{} timeout", format_elapsed(timeout)));
                }
                if let Some(max_attempts) = child.max_attempts {
                    metadata.push(format!("max {max_attempts}"));
                }
                if let Some(contract) = &child.output_contract {
                    metadata.push(format!("output {contract}"));
                }
                if !metadata.is_empty() {
                    details.push(Line::from(Span::styled(
                        format!("          {}", metadata.join(" · ")),
                        Style::default().fg(theme.muted),
                    )));
                }
                if let Some(error) = &child.error {
                    details.push(Line::from(Span::styled(
                        format!("          {error}"),
                        Style::default().fg(theme.error),
                    )));
                }
                if let Some(observability) = &child.observability {
                    append_workflow_observability(&mut details, observability, "          ", theme);
                }
            }
        }
        if let Some(error) = &workflow.error {
            details.push(Line::raw(""));
            details.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(theme.error),
            )));
        }
    } else {
        details.push(Line::from(Span::styled(
            "Workflow details appear here",
            Style::default().fg(theme.muted),
        )));
    }
    frame.render_widget(
        Paragraph::new(details).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Execution plan ")
                .title_bottom(Line::from(" Up/Down select   v artifact   a approve   d reject   r retry   x cancel   Esc close ").alignment(Alignment::Center))
                .border_style(Style::default().fg(theme.accent)),
        ),
        columns[1],
    );
}

fn block_viewer_content(
    state: &AppState,
    width: usize,
    theme: Theme,
) -> (String, Vec<Line<'static>>) {
    let Some(index) = state.viewed_entry else {
        return (
            "Conversation block".into(),
            vec![Line::raw("No block selected")],
        );
    };
    let Some(entry) = state.entries.get(index) else {
        return (
            "Conversation block".into(),
            vec![Line::raw("Block is no longer available")],
        );
    };
    match entry {
        Entry::User { text, .. } => (
            "User prompt".into(),
            markdown::render(
                &text.lines().map(str::to_owned).collect::<Vec<_>>(),
                width,
                theme,
            ),
        ),
        Entry::Assistant { lines, .. } => (
            "Assistant response".into(),
            markdown::render(lines, width, theme),
        ),
        Entry::Reasoning { text, active, .. } => (
            "Thinking".into(),
            reasoning_lines(text, *active, true, false, width, theme),
        ),
        Entry::Diff { path, lines, .. } => (
            format!("Edit {path}"),
            diff_render_lines(path, lines, true, false, width, theme),
        ),
        Entry::Tool {
            label,
            detail,
            status,
            duration,
            started_at,
            result,
            ..
        } => (
            format!("{label} tool"),
            tool_render_lines(
                ToolRender {
                    label,
                    detail,
                    status: *status,
                    result: result.as_deref(),
                    expanded: true,
                    duration: duration.as_deref(),
                    started_at: *started_at,
                    nested: false,
                    focused: false,
                    hovered: false,
                    waiting_for_user: permission_matches_tool(state, label),
                },
                width,
                theme,
            ),
        ),
        Entry::Plan { entries, .. } => ("Plan".into(), plan_lines(entries, true, width, theme)),
        Entry::Compaction {
            summary,
            tokens_before,
            tokens_after,
            active,
            error,
            ..
        } => (
            "Compaction".into(),
            compaction_lines(
                summary,
                *tokens_before,
                *tokens_after,
                *active,
                error.as_deref(),
                width,
                theme,
            ),
        ),
        Entry::CompactionIndicator { tokens_before, .. } => (
            "Compaction".into(),
            compaction_indicator_line(*tokens_before, width, theme),
        ),
    }
}

fn render_block_viewer(frame: &mut Frame<'_>, state: &AppState, theme: Theme) {
    let area = frame.area().inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let content_width = usize::from(area.width.saturating_sub(2));
    let (title, lines) = block_viewer_content(state, content_width, theme);
    let viewport = usize::from(area.height.saturating_sub(2));
    let max_scroll = lines.len().saturating_sub(viewport);
    let scroll = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .title_bottom(Line::from(" Esc/q close   ↑/↓ scroll ").alignment(Alignment::Center))
        .border_style(Style::default().fg(theme.selection_border));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn append_workflow_observability(
    details: &mut Vec<Line<'static>>,
    observation: &pi_harness::WorkflowAttemptObservability,
    indent: &str,
    theme: Theme,
) {
    details.push(Line::from(Span::styled(
        format!(
            "{indent}route: {} · thinking {} · {} {}",
            observation.model.as_deref().unwrap_or("unknown"),
            observation.thinking.as_deref().unwrap_or("default"),
            observation.capability,
            observation.session
        ),
        Style::default().fg(theme.muted),
    )));
    details.push(Line::from(Span::styled(
        format!(
            "{indent}context: {} prompt · {} artifacts ({}, {} truncated) · {} system",
            format_bytes(observation.prompt_bytes),
            observation.artifact_count,
            format_bytes(observation.artifact_bytes),
            observation.truncated_artifact_count,
            observation
                .system_prompt_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "unknown".into())
        ),
        Style::default().fg(theme.muted),
    )));
    if let Some(fingerprint) = observation.tool_schema_fingerprint.as_deref() {
        let tools = observation.active_tools.as_ref().map_or(0, Vec::len);
        let prefix = observation
            .cache_prefix_fingerprint
            .as_deref()
            .map(short_fingerprint)
            .unwrap_or("unknown");
        let cache_state = match observation.cache_prefix_changed {
            Some(true) => "changed",
            Some(false) => "stable/new",
            None => "unknown",
        };
        details.push(Line::from(Span::styled(
            format!(
                "{indent}tools: {tools} · schema {} · prefix {prefix} ({cache_state})",
                short_fingerprint(fingerprint)
            ),
            Style::default().fg(if observation.cache_prefix_changed == Some(true) {
                theme.warning
            } else {
                theme.muted
            }),
        )));
    }
    if observation.input_tokens.is_some()
        || observation.output_tokens.is_some()
        || observation.cache_read_tokens.is_some()
        || observation.cache_write_tokens.is_some()
    {
        details.push(Line::from(Span::styled(
            format!(
                "{indent}usage: ↑{} ↓{} R{} W{} · cache hit {:.0}%",
                observation.input_tokens.unwrap_or_default(),
                observation.output_tokens.unwrap_or_default(),
                observation.cache_read_tokens.unwrap_or_default(),
                observation.cache_write_tokens.unwrap_or_default(),
                observation.cache_hit_rate.unwrap_or_default() * 100.0
            ),
            Style::default().fg(theme.muted),
        )));
    }
    if !observation.policy_violations.is_empty() {
        details.push(Line::from(Span::styled(
            format!(
                "{indent}guardrail {}: {}",
                observation.policy_action.as_deref().unwrap_or("warn"),
                observation.policy_violations.join("; ")
            ),
            Style::default().fg(theme.warning),
        )));
    }
    if let Some(outcome) = observation.provider_outcome.as_deref() {
        details.push(Line::from(Span::styled(
            format!(
                "{indent}provider: {outcome}{}",
                observation
                    .provider_failure_kind
                    .as_deref()
                    .map(|kind| format!(" ({kind})"))
                    .unwrap_or_default()
            ),
            Style::default().fg(if outcome == "failure" {
                theme.warning
            } else {
                theme.muted
            }),
        )));
    }
}

fn short_fingerprint(value: &str) -> &str {
    &value[..value.len().min(10)]
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

fn render_workflow_artifact(frame: &mut Frame<'_>, state: &AppState, theme: Theme) {
    let area = frame.area().inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let Some(artifact) = state.workflow_artifact.as_ref() else {
        return;
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} · {}", artifact.step_id, artifact.producer_role),
                Style::default().fg(theme.foreground),
            ),
            Span::styled(
                artifact
                    .producer_model
                    .as_ref()
                    .map(|model| format!(" · {model}"))
                    .unwrap_or_default(),
                Style::default().fg(theme.muted),
            ),
        ]),
        Line::from(Span::styled(
            artifact.summary.clone(),
            Style::default().fg(theme.muted),
        )),
        Line::raw(""),
    ];
    lines.extend(
        artifact
            .content
            .lines()
            .map(|line| Line::raw(line.to_string())),
    );
    if artifact.truncated {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "[artifact truncated at 100 KB]",
            Style::default().fg(theme.error),
        )));
    }
    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(viewport);
    let scroll = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" Artifact · {} ", artifact.artifact_id))
                    .title_bottom(
                        Line::from(" Esc/q back   Up/Down scroll ").alignment(Alignment::Center),
                    )
                    .border_style(Style::default().fg(theme.accent)),
            ),
        area,
    );
}

fn render_tasks(frame: &mut Frame<'_>, state: &AppState, theme: Theme) {
    let area = frame.area().inner(Margin {
        horizontal: 3,
        vertical: 2,
    });
    let tasks = state.sorted_subagent_tasks();
    let visible_tasks = usize::from(area.height.saturating_sub(2)) / 2;
    let max_offset = tasks.len().saturating_sub(visible_tasks.max(1));
    let mut offset = state.task_list_offset.get().min(max_offset);
    if state.task_selected < offset {
        offset = state.task_selected;
    } else if state.task_selected >= offset.saturating_add(visible_tasks.max(1)) {
        offset = state
            .task_selected
            .saturating_add(1)
            .saturating_sub(visible_tasks.max(1));
    }
    state.task_list_offset.set(offset);
    state.task_list_rect.set(Some((
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )));
    let mut lines = Vec::new();
    for (index, task) in tasks
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible_tasks.max(1))
    {
        let selected = index == state.task_selected;
        let elapsed = task.duration_ms;
        let icon = match task.status.as_str() {
            "running" => WORKING_SPINNER[(elapsed / 80) as usize % WORKING_SPINNER.len()],
            "completed" => "✓",
            "cancelled" => "■",
            _ => "✗",
        };
        let color = match task.status.as_str() {
            "running" | "completed" => theme.success,
            "cancelled" => theme.muted,
            _ => theme.error,
        };
        let activity = compact_task_activity(&task.activity, &state.cwd);
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "› " } else { "  " },
                Style::default().fg(theme.accent),
            ),
            Span::styled(format!("{icon} Agent "), Style::default().fg(color)),
            Span::styled(
                task.description.clone(),
                Style::default().fg(if selected {
                    theme.foreground
                } else {
                    theme.muted
                }),
            ),
            Span::styled(
                format!("  {}  {}", activity, format_elapsed(elapsed)),
                Style::default().fg(theme.muted),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "      {} · {} · {} · {}",
                task.subagent_type,
                task.model.as_deref().unwrap_or("inherited model"),
                task.thinking_level.as_deref().unwrap_or("default thinking"),
                task.capability_mode,
            ),
            Style::default().fg(theme.muted),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No subagent tasks yet",
            Style::default().fg(theme.muted),
        )));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Tasks · Subagents ")
        .title_bottom(
            Line::from(" ↑/↓ select   Enter inspect   k kill   Ctrl+B close ")
                .alignment(Alignment::Center),
        )
        .border_style(Style::default().fg(theme.accent));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

pub fn subagent_close_at(width: u16, height: u16, column: u16, row: u16) -> bool {
    let area = Rect::new(0, 0, width, height).inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    row == area.y
        && column >= area.right().saturating_sub(6)
        && column < area.right().saturating_sub(1)
}

fn render_subagent(frame: &mut Frame<'_>, state: &AppState, theme: Theme) {
    let area = frame.area().inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    let Some(task_id) = state.inspected_subagent.as_deref() else {
        return;
    };
    let task = state.subagent_tasks.get(task_id);
    let elapsed = task.map_or(0, |task| task.duration_ms);
    let title = task.map_or_else(
        || format!(" Agent {task_id} "),
        |task| {
            format!(
                " Agent · {} · {} · {} ",
                task.description,
                task.status,
                format_elapsed(elapsed)
            )
        },
    );
    let mut lines =
        subagent_transcript_lines(state, task_id, area.width.saturating_sub(4) as usize, theme);
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Waiting for child transcript…",
            Style::default().fg(theme.muted),
        )));
    }
    let viewport = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(viewport);
    let scroll = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title(Line::from(" [×] ").alignment(Alignment::Right))
        .title_bottom(Line::from(" Esc/q close   ↑/↓ scroll ").alignment(Alignment::Center))
        .border_style(
            Style::default().fg(if task.is_some_and(|task| task.status == "running") {
                theme.accent
            } else {
                theme.muted
            }),
        );
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .scroll((scroll as u16, 0)),
        area,
    );
}

fn subagent_transcript_lines(
    state: &AppState,
    task_id: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(task) = state.subagent_tasks.get(task_id) {
        lines.push(Line::from(vec![
            Span::styled("  Activity  ", Style::default().fg(theme.muted)),
            Span::styled(
                compact_task_activity(&task.activity, &state.cwd),
                Style::default().fg(theme.foreground),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!(
                "  {} · {} · {} · {}",
                task.model.as_deref().unwrap_or("inherited model"),
                task.thinking_level.as_deref().unwrap_or("default thinking"),
                task.subagent_type,
                task.capability_mode,
            ),
            Style::default().fg(theme.muted),
        )));
        lines.push(Line::raw(""));
    }
    let mut reasoning = String::new();
    let mut response = String::new();
    let mut visible_tool_calls = HashSet::new();
    let flush = |lines: &mut Vec<Line<'static>>, text: &mut String, is_reasoning: bool| {
        if text.is_empty() {
            return;
        }
        for line in markdown::wrap(text.trim_end(), width.saturating_sub(4).max(1)) {
            lines.push(Line::from(vec![
                Span::styled(
                    if is_reasoning { "  | " } else { "  " },
                    Style::default().fg(theme.subtle),
                ),
                Span::styled(
                    line,
                    Style::default()
                        .fg(if is_reasoning {
                            theme.muted
                        } else {
                            theme.foreground
                        })
                        .add_modifier(if is_reasoning {
                            Modifier::ITALIC
                        } else {
                            Modifier::empty()
                        }),
                ),
            ]));
        }
        text.clear();
    };
    for event in state
        .subagent_transcripts
        .get(task_id)
        .into_iter()
        .flatten()
    {
        match event {
            pi_harness::AgentEvent::ReasoningDelta { text } => {
                flush(&mut lines, &mut response, false);
                reasoning.push_str(text);
            }
            pi_harness::AgentEvent::TextDelta { text } => {
                flush(&mut lines, &mut reasoning, true);
                response.push_str(text);
            }
            pi_harness::AgentEvent::UserMessage { text } => {
                flush(&mut lines, &mut reasoning, true);
                flush(&mut lines, &mut response, false);
                lines.push(Line::raw(""));
                for line in markdown::wrap(text, width.saturating_sub(4).max(1)) {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default()
                            .fg(theme.foreground)
                            .bg(theme.user_background),
                    )));
                }
            }
            pi_harness::AgentEvent::ToolCallStart { id, name, args } => {
                flush(&mut lines, &mut reasoning, true);
                flush(&mut lines, &mut response, false);
                if !is_internal_subagent_ui_tool(name) {
                    visible_tool_calls.insert(id.as_str());
                    let detail = args
                        .get("path")
                        .or_else(|| args.get("command"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    lines.push(Line::from(vec![
                        Span::styled("  + ", Style::default().fg(theme.accent_tool)),
                        Span::styled(name.clone(), Style::default().fg(theme.foreground)),
                        Span::styled(
                            format!("  {}", truncate_text(detail, width.saturating_sub(12))),
                            Style::default().fg(theme.muted),
                        ),
                    ]));
                }
            }
            pi_harness::AgentEvent::ToolCallResult {
                id,
                is_error,
                duration_ms,
                ..
            } if visible_tool_calls.contains(id.as_str()) => {
                flush(&mut lines, &mut reasoning, true);
                flush(&mut lines, &mut response, false);
                lines.push(Line::from(Span::styled(
                    format!(
                        "    {} {}",
                        if *is_error { "failed" } else { "done" },
                        duration_ms.map(format_elapsed).unwrap_or_default()
                    ),
                    Style::default().fg(if *is_error { theme.error } else { theme.muted }),
                )));
            }
            pi_harness::AgentEvent::Error { message, .. } => {
                flush(&mut lines, &mut reasoning, true);
                flush(&mut lines, &mut response, false);
                lines.push(Line::from(Span::styled(
                    format!("  x {message}"),
                    Style::default().fg(theme.error),
                )));
            }
            _ => {}
        }
    }
    flush(&mut lines, &mut reasoning, true);
    flush(&mut lines, &mut response, false);
    lines
}

fn is_internal_subagent_ui_tool(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "spawn_subagent"
            | "get_command_or_subagent_output"
            | "wait_commands_or_subagents"
            | "get_subagent_result"
    )
}

const TORII: &[&str] = &[
    "       _________________________       ",
    "       \\_____, ,__, ,__, ,_____/       ",
    "        _____| |__| |__| |_____        ",
    "        \\____, ,_______, ,____/        ",
    "             | |       | |             ",
    " @           | |       | |           @ ",
    " @@          | |       | |          @@ ",
    " @@@  @  @  @| |       | |@  @  @  @@@ ",
    "             | |       | |             ",
];

fn render_dashboard(frame: &mut Frame<'_>, state: &AppState, theme: Theme) {
    let area = frame.area().inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let width = area.width.min(86);
    let height = area.height.min(30);
    let panel = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let compact = panel.height < 25 || panel.width < 68;
    let logo_height = if compact { 5 } else { TORII.len() as u16 };
    let chunks = Layout::vertical([
        Constraint::Length(logo_height),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .split(panel);

    let logo: Vec<Line<'_>> = if compact {
        TORII.iter().take(5).map(|line| Line::from(*line)).collect()
    } else {
        TORII.iter().map(|line| Line::from(*line)).collect()
    };
    frame.render_widget(
        Paragraph::new(logo)
            .alignment(Alignment::Center)
            .style(Style::default().fg(theme.error)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Torii",
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("agent workspace  ·  v{}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(theme.muted),
            )),
            Line::from(vec![
                Span::styled(state.model.clone(), Style::default().fg(theme.accent_model)),
                Span::styled("  ·  ", Style::default().fg(theme.subtle)),
                Span::styled(
                    truncate_text(&state.cwd, usize::from(panel.width.saturating_sub(24))),
                    Style::default().fg(theme.muted),
                ),
            ]),
        ])
        .alignment(Alignment::Center),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new("Sessions").style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        chunks[2],
    );

    let visible = chunks[3].height as usize;
    let selected = state
        .dashboard_selected
        .min(state.available_sessions.len().saturating_sub(1));
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let mut lines = Vec::new();
    for (index, session) in state
        .available_sessions
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
    {
        let selected_here = index == selected;
        let runtime = state.runtime_sessions.get(&session.path);
        let running = runtime.is_some_and(|runtime| runtime.status == "running")
            || (session.current && (state.streaming || state.has_background_work()));
        let attention = runtime.is_some_and(|runtime| runtime.status == "attention");
        let resident_idle = runtime.is_some_and(|runtime| runtime.status == "idle");
        let (indicator, status, color) = if running {
            let elapsed = runtime
                .and_then(|runtime| runtime.started_at_ms)
                .map_or_else(
                    || {
                        state
                            .turn_started_at
                            .map_or(0, |started| started.elapsed().as_millis())
                    },
                    |started| unix_time_ms().saturating_sub(u128::from(started)),
                );
            let frame = WORKING_SPINNER[(elapsed / 80) as usize % WORKING_SPINNER.len()];
            (
                frame,
                format!("Running {}", format_dashboard_elapsed(elapsed)),
                theme.success,
            )
        } else if attention {
            ("!", "Attention".into(), theme.warning)
        } else if session.current || resident_idle {
            ("●", "Idle".into(), theme.success)
        } else {
            ("○", "Inactive".into(), theme.muted)
        };
        let title = session
            .name
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&session.first_message);
        let marker = if selected_here { "›" } else { " " };
        let available = panel.width.saturating_sub(31) as usize;
        let title = truncate_text(title, available.max(8));
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker} {indicator} {status:<13} "),
                Style::default().fg(color),
            ),
            Span::styled(
                format!("{title:<width$}", width = available.max(8)),
                Style::default().fg(if selected_here {
                    theme.foreground
                } else {
                    theme.muted
                }),
            ),
            Span::styled(
                format!("  {}", session.modified),
                Style::default().fg(theme.muted),
            ),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No saved sessions yet",
            Style::default().fg(theme.muted),
        )));
    }
    state.dashboard_list_rect.set(Some((
        chunks[3].x,
        chunks[3].y,
        chunks[3].width,
        chunks[3].height,
    )));
    frame.render_widget(Paragraph::new(lines), chunks[3]);
    frame.render_widget(
        Paragraph::new(
            "↑/↓ select   Enter open   n new   r rename   d delete   s stop   x close   Esc return",
        )
        .alignment(Alignment::Center)
        .style(Style::default().fg(theme.muted)),
        chunks[4],
    );
}

fn unix_time_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn format_dashboard_elapsed(milliseconds: u128) -> String {
    let seconds = milliseconds / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    }
}

fn truncate_text(value: &str, width: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(width).collect();
    if chars.next().is_some() && width > 1 {
        result.pop();
        result.push('…');
    }
    result
}

pub fn max_scroll(state: &AppState, width: u16, height: u16) -> usize {
    if state.view == View::BlockViewer {
        let area_width = width.saturating_sub(6);
        let (_, lines) = block_viewer_content(state, usize::from(area_width), state.theme());
        return lines
            .len()
            .saturating_sub(usize::from(height.saturating_sub(4)));
    }
    if state.view == View::Subagent {
        let count = state.inspected_subagent.as_deref().map_or(0, |task_id| {
            subagent_transcript_lines(
                state,
                task_id,
                width.saturating_sub(4) as usize,
                state.theme(),
            )
            .len()
        });
        return count.saturating_sub(usize::from(height.saturating_sub(4)));
    }
    if state.view != View::Transcript {
        return 0;
    }
    let (_, _, content_width, viewport) = transcript_geometry(state, width, height);
    TranscriptLayout::build(state, content_width.saturating_sub(1) as usize).max_scroll(viewport)
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let separator = || Span::styled(" │ ", Style::default().fg(theme.subtle));
    let queued = state.queued_steering.len() + state.queued_follow_up.len();
    let mut right: Vec<(Option<u8>, Span<'static>)> = Vec::new();
    if state.tasks_total > 0 {
        if !right.is_empty() {
            right.push((None, separator()));
        }
        right.push((
            Some(2),
            Span::styled(
                format!("Plan {}/{}", state.tasks_complete, state.tasks_total),
                Style::default().fg(theme.accent_plan).add_modifier(
                    if state.header_hover == Some(2) {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    },
                ),
            ),
        ));
    }
    if queued > 0 {
        if !right.is_empty() {
            right.push((None, separator()));
        }
        right.push((
            Some(3),
            Span::styled(
                format!("+{queued}"),
                Style::default().fg(theme.accent_user).add_modifier(
                    if state.header_hover == Some(3) {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    },
                ),
            ),
        ));
    }
    if !right.is_empty() {
        right.push((None, separator()));
    }
    let context_used = state
        .context_used
        .saturating_add(state.turn_output_chars / 4);
    let context_percent = context_used
        .saturating_mul(100)
        .checked_div(state.context_limit.max(1))
        .unwrap_or(0);
    let estimate = if state.turn_started_at.is_some() {
        "~"
    } else {
        ""
    };
    let used_label = compact_number(context_used);
    let limit_label = compact_number(state.context_limit);
    let token_label = format!("{estimate}{used_label} / {limit_label}");
    let context_label = if state.header_hover == Some(4) {
        let width = token_label.chars().count().max(6);
        let bar_width = width.saturating_sub(6);
        let filled = bar_width.saturating_mul(context_percent.min(100) as usize) / 100;
        format!(
            "{}{} {:>4}%",
            "█".repeat(filled),
            "░".repeat(bar_width.saturating_sub(filled)),
            context_percent.min(100)
        )
    } else {
        token_label
    };
    right.push((
        Some(4),
        Span::styled(
            context_label,
            Style::default()
                .fg(if context_percent >= 95 {
                    theme.error
                } else if context_percent >= 85 {
                    theme.warning
                } else {
                    theme.muted
                })
                .add_modifier(if state.header_hover == Some(4) {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ));
    let right_width = right
        .iter()
        .map(|(_, span)| span.content.chars().count())
        .sum::<usize>();
    let left_budget = usize::from(area.width).saturating_sub(right_width + 1);
    let branch = format!("⎇ {}", state.branch);
    let cwd_budget = left_budget.saturating_sub(branch.chars().count() + 2);
    let cwd = truncate(&state.cwd, cwd_budget);
    let mut spans = vec![
        Span::styled(branch, Style::default().fg(theme.text_secondary)),
        Span::styled("  ", Style::default()),
        Span::styled(
            cwd.clone(),
            Style::default()
                .fg(if state.header_hover == Some(0) {
                    theme.foreground
                } else {
                    theme.muted
                })
                .add_modifier(if state.header_hover == Some(0) {
                    Modifier::UNDERLINED
                } else {
                    Modifier::empty()
                }),
        ),
    ];
    let left_width = spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    let gap = usize::from(area.width).saturating_sub(left_width + right_width);
    spans.push(Span::raw(" ".repeat(gap)));
    let mut targets = state.header_targets.borrow_mut();
    targets.clear();
    let cwd_x = area.x + (left_width.saturating_sub(cwd.chars().count())) as u16;
    if !cwd.is_empty() {
        targets.push((0, cwd_x, cwd_x + cwd.chars().count() as u16, area.y));
    }
    let mut right_x = area.x + left_width.saturating_add(gap) as u16;
    for (kind, span) in right {
        let width = span.content.chars().count() as u16;
        if let Some(kind) = kind {
            targets.push((kind, right_x, right_x.saturating_add(width), area.y));
        }
        right_x = right_x.saturating_add(width);
        spans.push(span);
    }
    drop(targets);
    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_transcript(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let content = area.inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    state
        .transcript_rect
        .set(Some((content.x, content.y, content.width, content.height)));
    let width = content.width.saturating_sub(1) as usize;
    let layout = TranscriptLayout::build(state, width);
    let mut lines = Vec::new();
    let mut user_headers = Vec::new();

    let mut entry_index = 0;
    while entry_index < state.entries.len() {
        let entry = &state.entries[entry_index];
        match entry {
            Entry::User { text, timestamp } => {
                lines.push(Line::raw(""));
                let header_start = lines.len();
                lines.extend(user_card_lines(text, timestamp, width, theme));
                user_headers.push((header_start, text.clone(), timestamp.clone()));
                lines.push(Line::raw(""));
            }
            Entry::Reasoning {
                text,
                active,
                expanded,
            } => {
                lines.extend(reasoning_lines(
                    text,
                    *active,
                    *expanded,
                    state.hovered_entry == Some(entry_index),
                    width,
                    theme,
                ));
            }
            Entry::Diff {
                path,
                lines: diff_lines,
                expanded,
                ..
            } => {
                lines.extend(diff_render_lines(
                    path,
                    diff_lines,
                    *expanded,
                    state.hovered_entry == Some(entry_index),
                    width,
                    theme,
                ));
            }
            Entry::Tool { .. } => {
                let (rendered, consumed) = tool_group_lines(state, entry_index, width, theme);
                lines.extend(rendered);
                entry_index += consumed.saturating_sub(1);
            }
            Entry::Plan { entries, expanded } => {
                lines.extend(plan_lines(entries, *expanded, width, theme));
            }
            Entry::Compaction {
                summary,
                tokens_before,
                tokens_after,
                active,
                error,
                started_at: _,
            } => {
                lines.push(Line::raw(""));
                lines.extend(compaction_lines(
                    summary,
                    *tokens_before,
                    *tokens_after,
                    *active,
                    error.as_deref(),
                    width,
                    theme,
                ));
            }
            Entry::CompactionIndicator { tokens_before, .. } => {
                lines.push(Line::raw(""));
                lines.extend(compaction_indicator_line(*tokens_before, width, theme));
            }
            Entry::Assistant {
                lines: message_lines,
                timestamp,
            } => {
                lines.push(Line::raw(""));
                let mut rendered = markdown::render(message_lines, width, theme);
                if let Some(first) = rendered.first_mut()
                    && !timestamp.is_empty()
                {
                    let gap = width.saturating_sub(first.width() + timestamp.chars().count());
                    first.spans.push(Span::raw(" ".repeat(gap)));
                    first.spans.push(Span::styled(
                        timestamp.clone(),
                        Style::default().fg(theme.muted),
                    ));
                }
                lines.extend(rendered);
            }
        }
        entry_index += 1;
    }

    let line_count = layout.total_rows;
    debug_assert_eq!(
        line_count,
        lines.len(),
        "painted transcript diverged from TranscriptLayout"
    );
    let viewport_height = content.height as usize;
    let max_scroll = line_count.saturating_sub(viewport_height);
    let scroll = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    let viewport_end = scroll.saturating_add(viewport_height);
    let hit_regions = layout
        .sections
        .iter()
        .filter_map(|section| {
            let entry = state.entries.get(section.index)?;
            let (start, end) = match entry {
                Entry::User { .. } => (
                    section.start.saturating_add(1),
                    section.end.saturating_sub(1),
                ),
                Entry::Reasoning { .. } | Entry::Diff { .. } | Entry::Plan { .. } => {
                    (section.start, section.end.saturating_sub(1))
                }
                Entry::Assistant { .. }
                | Entry::Compaction { .. }
                | Entry::CompactionIndicator { .. } => {
                    (section.start.saturating_add(1), section.end)
                }
                Entry::Tool { .. } => (section.start, section.end),
            };
            let visible_start = start.max(scroll);
            let visible_end = end.min(viewport_end);
            (visible_start < visible_end).then(|| {
                (
                    section.id.clone(),
                    section.index,
                    content
                        .y
                        .saturating_add(visible_start.saturating_sub(scroll) as u16),
                    content
                        .y
                        .saturating_add(visible_end.saturating_sub(scroll) as u16),
                    section.actionable,
                )
            })
        })
        .collect();
    *state.transcript_hit_regions.borrow_mut() = hit_regions;
    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground).bg(theme.background))
        .scroll((scroll as u16, 0));
    frame.render_widget(paragraph, content);

    if let Some(target_id) = state.hovered_target_id.as_deref()
        && state.focused_target_id.as_deref() != Some(target_id)
        && let Some(section) = layout
            .sections
            .iter()
            .find(|section| section.id == target_id)
        && matches!(state.entries.get(section.index), Some(Entry::Tool { .. }))
    {
        render_tool_row_background(
            frame,
            content,
            section,
            scroll,
            viewport_height,
            theme.bg_hover,
        );
    }

    if state.focus == Focus::Scrollback
        && let Some(target_id) = state.focused_target_id.as_deref()
        && let Some(section) = layout
            .sections
            .iter()
            .find(|section| section.id == target_id)
    {
        render_selected_tool_background(
            frame,
            content,
            section,
            scroll,
            viewport_height,
            state,
            theme,
        );
        render_section_border(
            frame,
            area,
            content,
            section,
            scroll,
            viewport_height,
            theme,
            false,
        );
    }
    if let Some(target_id) = state.hovered_target_id.as_deref()
        && state.focused_target_id.as_deref() != Some(target_id)
        && let Some(section) = layout
            .sections
            .iter()
            .find(|section| section.id == target_id)
        // Grok tool-call hover is a background + chevron affordance. Its
        // selection box is reserved for keyboard/click selection.
        && !matches!(state.entries.get(section.index), Some(Entry::Tool { .. }))
    {
        render_section_border(
            frame,
            area,
            content,
            section,
            scroll,
            viewport_height,
            theme,
            true,
        );
    }

    if line_count > viewport_height {
        state.transcript_scrollbar_rect.set(Some((
            area.right().saturating_sub(1),
            area.y,
            1,
            area.height,
        )));
        render_scrollbar(frame, area, line_count, viewport_height, scroll, theme);
        if state.scroll_from_bottom > 0 {
            let marker = Rect::new(
                area.x + area.width.saturating_sub(1) / 2,
                area.y + area.height.saturating_sub(1),
                1,
                1,
            );
            frame.render_widget(
                Paragraph::new("▼")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(theme.foreground)),
                marker,
            );
        }
    } else {
        state.transcript_scrollbar_rect.set(None);
    }

    if let Some((_, text, timestamp)) = user_headers
        .iter()
        .rev()
        .find(|(position, _, _)| scroll > *position)
    {
        let sticky_height = content.height.min(3);
        let sticky_area = Rect::new(content.x, content.y, content.width, sticky_height);
        frame.render_widget(
            Paragraph::new(user_card_lines(text, timestamp, width, theme)),
            sticky_area,
        );
    }
}

/// Grok gives a selected collapsed tool row a subtle background beneath its
/// selection box. Expanded output keeps its own semantic row styling and gets
/// the border only.
fn render_selected_tool_background(
    frame: &mut Frame<'_>,
    content: Rect,
    section: &LayoutSection,
    scroll: usize,
    viewport_height: usize,
    state: &AppState,
    theme: Theme,
) {
    let is_collapsed_tool = match state.entries.get(section.index) {
        Some(Entry::Tool { expanded, .. }) => section.id.starts_with("tool-group:") || !expanded,
        _ => false,
    };
    if !is_collapsed_tool {
        return;
    }
    render_tool_row_background(
        frame,
        content,
        section,
        scroll,
        viewport_height,
        theme.bg_dark,
    );
}

fn render_tool_row_background(
    frame: &mut Frame<'_>,
    content: Rect,
    section: &LayoutSection,
    scroll: usize,
    viewport_height: usize,
    color: Color,
) {
    let viewport_end = scroll.saturating_add(viewport_height);
    let visible_start = section.start.max(scroll);
    let visible_end = section.end.min(viewport_end);
    if visible_start >= visible_end || content.width <= 2 {
        return;
    }
    let y_start = content
        .y
        .saturating_add(visible_start.saturating_sub(scroll) as u16);
    let y_end = content
        .y
        .saturating_add(visible_end.saturating_sub(scroll) as u16);
    let style = Style::default().bg(color);
    for y in y_start..y_end {
        for x in content.x.saturating_add(1)..content.right().saturating_sub(1) {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_section_border(
    frame: &mut Frame<'_>,
    area: Rect,
    content: Rect,
    section: &LayoutSection,
    scroll: usize,
    viewport_height: usize,
    theme: Theme,
    preview: bool,
) {
    let viewport_end = scroll.saturating_add(viewport_height);
    if section.end <= scroll || section.start >= viewport_end {
        return;
    }
    let style = Style::default().fg(if preview {
        theme.hover_border
    } else {
        theme.selection_border
    });
    let visible_start = section.start.max(scroll);
    let visible_end = section.end.min(viewport_end);
    let top_clipped = section.start < scroll;
    let bottom_clipped = section.end > viewport_end;
    let y_top = content
        .y
        .saturating_add(visible_start.saturating_sub(scroll) as u16);
    let y_bottom = content
        .y
        .saturating_add(visible_end.saturating_sub(scroll) as u16)
        .saturating_sub(1);
    let left_x = area.x;
    let right_x = area.right().saturating_sub(2);
    for y in y_top..=y_bottom {
        let dashed = (y == y_top && top_clipped) || (y == y_bottom && bottom_clipped);
        let symbol = if dashed { "┆" } else { "│" };
        frame.buffer_mut().set_string(left_x, y, symbol, style);
        frame.buffer_mut().set_string(right_x, y, symbol, style);
    }
    if !top_clipped && y_top > 0 {
        let y = y_top - 1;
        frame.buffer_mut().set_string(left_x, y, "┌", style);
        frame.buffer_mut().set_string(right_x, y, "┐", style);
    }
    if !bottom_clipped {
        let y = y_bottom.saturating_add(1);
        if y < area.bottom() {
            frame.buffer_mut().set_string(left_x, y, "└", style);
            frame.buffer_mut().set_string(right_x, y, "┘", style);
        }
    }
}

fn render_scrollbar(
    frame: &mut Frame<'_>,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    scroll: usize,
    theme: Theme,
) {
    let track_length = area.height as usize;
    let Some((thumb_start, thumb_length)) =
        scrollbar_geometry(content_length, viewport_length, track_length, scroll)
    else {
        return;
    };
    let lines = (0..track_length)
        .map(|row| {
            let symbol = if row >= thumb_start && row < thumb_start + thumb_length {
                "█"
            } else {
                " "
            };
            Line::from(Span::styled(symbol, Style::default().fg(theme.foreground)))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines),
        Rect::new(area.right().saturating_sub(1), area.y, 1, area.height),
    );
}

pub(crate) fn scrollbar_geometry(
    content_length: usize,
    viewport_length: usize,
    track_length: usize,
    scroll: usize,
) -> Option<(usize, usize)> {
    if content_length <= viewport_length || viewport_length == 0 || track_length == 0 {
        return None;
    }
    let thumb_length = viewport_length
        .saturating_mul(track_length)
        .div_ceil(content_length)
        .clamp(1, track_length);
    let max_scroll = content_length - viewport_length;
    let max_thumb_start = track_length - thumb_length;
    let thumb_start = scroll
        .min(max_scroll)
        .saturating_mul(max_thumb_start)
        .div_ceil(max_scroll);
    Some((thumb_start, thumb_length))
}

fn compaction_lines(
    summary: &str,
    tokens_before: Option<u64>,
    tokens_after: Option<u64>,
    active: bool,
    error: Option<&str>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let glyph = if active {
        "*"
    } else if error.is_some() {
        "x"
    } else {
        "+"
    };
    let title = if active {
        "Compacting context…".to_string()
    } else if error.is_some() {
        "Compaction failed".to_string()
    } else {
        "Compacted context".to_string()
    };
    let mut header = vec![Line::from(vec![Span::styled(
        format!("{glyph} {title}"),
        Style::default().fg(if error.is_some() {
            theme.error
        } else {
            theme.accent
        }),
    )])];
    if let (Some(before), Some(after)) = (tokens_before, tokens_after) {
        header.push(Line::from(Span::styled(
            format!(
                "   {} → {} tokens",
                compact_number(before),
                compact_number(after)
            ),
            Style::default().fg(theme.muted),
        )));
    } else if let Some(before) = tokens_before {
        header.push(Line::from(Span::styled(
            format!("   {} tokens before", compact_number(before)),
            Style::default().fg(theme.muted),
        )));
    }
    let indent: &str = "   ";
    let indent_width = indent.chars().count();
    let render_width = width.saturating_sub(indent_width);
    let body_lines: Vec<Line<'static>> = if !summary.is_empty() {
        let source: Vec<String> = summary.split('\n').map(str::to_string).collect();
        markdown::render(&source, render_width, theme)
            .into_iter()
            .map(|line| {
                let mut prefixed: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
                prefixed.push(Span::raw(indent.to_string()));
                prefixed.extend(line.spans);
                Line::from(prefixed)
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut lines = header;
    lines.extend(body_lines);
    if !active {
        lines.push(Line::raw(""));
    }
    lines
}

fn plan_lines(
    entries: &[pi_harness::PlanEntry],
    expanded: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let completed = entries
        .iter()
        .filter(|entry| entry.status == "completed")
        .count();
    let mut header = vec![
        Span::styled(
            "+ Plan",
            Style::default()
                .fg(theme.accent_plan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {completed}/{}", entries.len()),
            Style::default().fg(theme.muted),
        ),
    ];
    if !expanded {
        let summary = entries
            .iter()
            .find(|entry| entry.status == "in_progress")
            .map(|entry| entry.step.as_str())
            .or_else(|| (completed == entries.len()).then_some("completed"));
        if let Some(summary) = summary {
            let budget = width.saturating_sub(12);
            header.push(Span::styled(
                format!(" · {}", truncate(summary, budget)),
                Style::default().fg(theme.muted),
            ));
        }
    }

    let mut lines = vec![Line::from(header)];
    if expanded {
        let body_width = width.saturating_sub(4).max(1);
        for (entry_index, entry) in entries.iter().enumerate() {
            let wrapped = markdown::wrap(entry.step.trim(), body_width);
            let wrapped = if wrapped.is_empty() {
                vec![String::new()]
            } else {
                wrapped
            };
            for (line_index, text) in wrapped.iter().enumerate() {
                let last = entry_index + 1 == entries.len() && line_index + 1 == wrapped.len();
                let rail = if last { "└ " } else { "│ " };
                let (marker, color, modifier) = match entry.status.as_str() {
                    "completed" => ("✓ ", theme.success, Modifier::DIM),
                    "in_progress" => ("> ", theme.accent_plan, Modifier::BOLD),
                    "pending" => ("· ", theme.muted, Modifier::empty()),
                    _ => ("? ", theme.warning, Modifier::empty()),
                };
                lines.push(Line::from(vec![
                    Span::styled(rail, Style::default().fg(theme.subtle)),
                    Span::styled(
                        if line_index == 0 { marker } else { "  " },
                        Style::default().fg(color).add_modifier(modifier),
                    ),
                    Span::styled(
                        text.clone(),
                        Style::default().fg(color).add_modifier(modifier),
                    ),
                ]));
            }
        }
    }
    lines.push(Line::raw(""));
    lines
}

fn reasoning_lines(
    text: &str,
    active: bool,
    expanded: bool,
    hovered: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let body_width = width.saturating_sub(2);
    if text.trim().is_empty() {
        return Vec::new();
    }
    let glyph = if hovered {
        ">"
    } else if active {
        "*"
    } else {
        "+"
    };
    let header_color = if hovered {
        theme.foreground
    } else if active {
        theme.accent_running
    } else {
        theme.muted
    };
    let label = if active { "Thinking…" } else { "Thinking" };
    let mut lines = vec![Line::from(Span::styled(
        format!("{glyph} {label}"),
        Style::default()
            .fg(header_color)
            .add_modifier(Modifier::ITALIC),
    ))];

    if expanded {
        let source = markdown::wrap(text.trim(), body_width);
        let mut body = markdown::render(&source, body_width, theme);
        let body_len = body.len();
        for (index, mut line) in body.drain(..).enumerate() {
            let rail = if index + 1 == body_len {
                "└ "
            } else {
                "│ "
            };
            let mut spans = Vec::with_capacity(line.spans.len() + 1);
            spans.push(Span::styled(rail, Style::default().fg(theme.subtle)));
            spans.extend(line.spans.drain(..).map(|mut span| {
                span.style = span.style.fg(theme.muted).add_modifier(Modifier::ITALIC);
                span
            }));
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn user_card_lines(text: &str, timestamp: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let style = Style::default().bg(theme.bg_light).fg(theme.foreground);
    let time_width = timestamp.chars().count();
    let available = width.saturating_sub(time_width + 5);
    let text = truncate(text, available);
    let gap = width.saturating_sub(text.chars().count() + time_width + 2);
    vec![
        Line::from(Span::styled(" ".repeat(width), style)),
        Line::from(Span::styled(
            format!("  {text}{}{}", " ".repeat(gap), timestamp),
            style,
        )),
        Line::from(Span::styled(" ".repeat(width), style)),
    ]
}

struct ToolRender<'a> {
    label: &'a str,
    detail: &'a str,
    status: ToolStatus,
    result: Option<&'a str>,
    expanded: bool,
    duration: Option<&'a str>,
    started_at: Option<std::time::Instant>,
    nested: bool,
    focused: bool,
    hovered: bool,
    waiting_for_user: bool,
}

const COMPACTION_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const WORKING_SPINNER: &[&str] = &["⠋", "⠙", "⠸", "⠴", "⠦", "⠇", "⠏", "⠋"];

fn render_working_banner(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    if let Some(started_at) = state.image_processing_started_at {
        let elapsed_ms = started_at.elapsed().as_millis() as u64;
        let spinner = WORKING_SPINNER[(elapsed_ms / 100) as usize % WORKING_SPINNER.len()];
        render_three_column_banner(
            frame,
            area,
            &format!("{spinner} Processing image…"),
            &format_elapsed(elapsed_ms),
            "clipboard",
            theme,
            theme.warning,
            theme.muted,
        );
        return;
    }
    let active_tasks: Vec<_> = state
        .subagent_tasks
        .values()
        .filter(|task| task.status == "running")
        .collect();
    let elapsed_ms = state.turn_started_at.map_or_else(
        || {
            active_tasks
                .iter()
                .map(|task| task.duration_ms)
                .max()
                .unwrap_or(0)
        },
        |started_at| started_at.elapsed().as_millis() as u64,
    );
    let frame_index = (elapsed_ms / 100) as usize % WORKING_SPINNER.len();
    let spinner = WORKING_SPINNER[frame_index];
    // Estimate output tokens from the accumulated streamed characters. The
    // rule of thumb is roughly 4 characters per token for English text; the
    // banner shows this as a live estimate that gets replaced by the wire's
    // real number once TurnComplete arrives.
    let estimated_output_tokens = state.turn_output_chars / 4;
    let elapsed_label = format_elapsed(elapsed_ms);
    let left = if let Some(task) = active_tasks.iter().max_by_key(|task| task.started_at_ms) {
        let count = active_tasks.len();
        format!(
            "{spinner} {count} agent{} active · {}",
            if count == 1 { "" } else { "s" },
            compact_task_activity(&task.activity, &state.cwd)
        )
    } else {
        format!("{spinner} Working…")
    };
    let tokens_label = format!(
        "↑ {} input  ↓ {} output",
        compact_number(state.turn_input_tokens),
        compact_number(estimated_output_tokens),
    );
    render_three_column_banner(
        frame,
        area,
        &left,
        &elapsed_label,
        &tokens_label,
        theme,
        theme.accent,
        theme.muted,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_three_column_banner(
    frame: &mut Frame<'_>,
    area: Rect,
    left: &str,
    center: &str,
    right: &str,
    theme: Theme,
    left_color: Color,
    right_color: Color,
) {
    let width = area.width as usize;
    let left_chars = left.chars().count();
    let center_chars = center.chars().count();
    let right_chars = right.chars().count();
    let min_gap = 2;
    let needed = left_chars
        .saturating_add(min_gap)
        .saturating_add(center_chars)
        .saturating_add(min_gap)
        .saturating_add(right_chars);
    let mut spans: Vec<Span<'static>> = Vec::new();
    if needed <= width {
        let gap_left = (width - left_chars - center_chars - right_chars) / 2;
        let gap_right = width - left_chars - gap_left - center_chars - right_chars;
        spans.push(Span::styled(
            left.to_string(),
            Style::default().fg(left_color),
        ));
        spans.push(Span::raw(" ".repeat(gap_left)));
        spans.push(Span::styled(
            center.to_string(),
            Style::default().fg(theme.foreground),
        ));
        spans.push(Span::raw(" ".repeat(gap_right)));
        spans.push(Span::styled(
            right.to_string(),
            Style::default().fg(right_color),
        ));
    } else if left_chars + min_gap + right_chars <= width {
        let gap = width - left_chars - right_chars;
        spans.push(Span::styled(
            left.to_string(),
            Style::default().fg(left_color),
        ));
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(
            right.to_string(),
            Style::default().fg(right_color),
        ));
    } else {
        let budget = width.saturating_sub(right_chars + min_gap);
        let truncated: String = if left.chars().count() > budget {
            let take = budget.saturating_sub(1);
            let mut s: String = left.chars().take(take).collect();
            s.push('…');
            s
        } else {
            left.to_string()
        };
        spans.push(Span::styled(truncated, Style::default().fg(left_color)));
    }
    let line = Line::from(spans);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(theme.background)),
        area,
    );
}

fn render_compaction_banner(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let started_at = state.active_compaction_started_at();
    let elapsed_ms = started_at
        .map(|start| start.elapsed().as_millis() as u64)
        .unwrap_or(0);
    let frame_index = (elapsed_ms / 80) as usize % COMPACTION_SPINNER.len();
    let spinner = COMPACTION_SPINNER[frame_index];
    let elapsed_label = format_elapsed(elapsed_ms);
    let left = format!("{spinner} Compacting context…");
    let center = elapsed_label;
    let right = format!("↓ {} tokens", compact_number(state.context_used));
    let error_color = if state.entries.iter().rev().any(|entry| {
        matches!(
            entry,
            Entry::Compaction {
                active: false,
                error: Some(_),
                ..
            }
        )
    }) {
        theme.error
    } else {
        theme.muted
    };
    render_three_column_banner(
        frame,
        area,
        &left,
        &center,
        &right,
        theme,
        theme.accent,
        error_color,
    );
}

fn format_elapsed(milliseconds: u64) -> String {
    if milliseconds < 60_000 {
        format!("{:.1}s", milliseconds as f64 / 1000.0)
    } else {
        let total_seconds = milliseconds / 1000;
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("{minutes}:{seconds:02}")
    }
}

fn compaction_indicator_line(
    tokens_before: Option<u64>,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let label = match tokens_before {
        Some(count) => format!("Previously compacted from {} tokens", compact_number(count)),
        None => "Previously compacted".to_string(),
    };
    let budget = width.saturating_sub(6);
    let truncated: String = if label.chars().count() > budget {
        let take = budget.saturating_sub(1);
        let mut out: String = label.chars().take(take).collect();
        out.push('…');
        out
    } else {
        label
    };
    vec![Line::from(Span::styled(
        format!("   ┊ {truncated}"),
        Style::default().fg(theme.muted),
    ))]
}

fn tool_group_lines(
    state: &AppState,
    start: usize,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, usize) {
    let Entry::Tool { id, label, .. } = &state.entries[start] else {
        return (Vec::new(), 1);
    };
    let count = state.entries[start..]
        .iter()
        .take_while(|entry| matches!(entry, Entry::Tool { label: other, .. } if other == label))
        .count();
    let orchestration = label == "Agent"
        && matches!(&state.entries[start], Entry::Tool { id, .. } if id.starts_with("subagent:"));
    if count == 1 && !orchestration {
        let Entry::Tool {
            detail,
            status,
            duration,
            started_at,
            result,
            expanded,
            ..
        } = &state.entries[start]
        else {
            unreachable!()
        };
        return (
            tool_render_lines(
                ToolRender {
                    label,
                    detail,
                    status: *status,
                    result: result.as_deref(),
                    expanded: *expanded,
                    duration: duration.as_deref(),
                    started_at: *started_at,
                    nested: false,
                    focused: state.focus == Focus::Scrollback
                        && state
                            .focused_target_id
                            .as_deref()
                            .and_then(|target| target.strip_prefix("tool:"))
                            == Some(id.as_str()),
                    hovered: state.hovered_entry == Some(start),
                    waiting_for_user: permission_matches_tool(state, label),
                },
                width,
                theme,
            ),
            1,
        );
    }

    let group = &state.entries[start..start + count];
    let running = group.iter().any(|entry| {
        matches!(
            entry,
            Entry::Tool {
                status: ToolStatus::Running,
                ..
            }
        )
    });
    let failed = group.iter().any(|entry| {
        matches!(
            entry,
            Entry::Tool {
                status: ToolStatus::Error,
                ..
            }
        )
    });
    let status = if running {
        ToolStatus::Running
    } else if failed {
        ToolStatus::Error
    } else {
        ToolStatus::Success
    };
    let started_at = group
        .iter()
        .filter_map(|entry| match entry {
            Entry::Tool { started_at, .. } => *started_at,
            _ => None,
        })
        .min();
    let duration = group
        .iter()
        .filter_map(|entry| match entry {
            Entry::Tool { duration, .. } => duration.as_deref(),
            _ => None,
        })
        .max_by_key(|value| duration_millis(value));
    let expanded = state.expanded_tool_groups.contains(&start);
    if orchestration {
        let mut lines = vec![orchestration_header_line(
            state, start, count, status, expanded, started_at, duration, width, theme,
        )];
        if expanded {
            for offset in 0..count {
                lines.extend(orchestration_task_lines(
                    state,
                    start + offset,
                    offset + 1 == count,
                    width,
                    theme,
                ));
            }
        }
        return (lines, count);
    }
    let summary = grouped_tool_summary(group, label, failed, count);
    let mut lines = tool_render_lines(
        ToolRender {
            label,
            detail: &format!("{summary} {}", if expanded { "v" } else { ">" }),
            status,
            result: None,
            expanded: false,
            duration,
            started_at,
            nested: false,
            focused: state.focus == Focus::Scrollback
                && state
                    .focused_target_id
                    .as_deref()
                    .and_then(|target| target.strip_prefix("tool-group:"))
                    == Some(id.as_str()),
            hovered: state.hovered_entry == Some(start),
            waiting_for_user: permission_matches_tool(state, label),
        },
        width,
        theme,
    );
    if expanded {
        for (offset, entry) in group.iter().enumerate() {
            let Entry::Tool {
                label,
                detail,
                status,
                duration,
                started_at,
                result,
                expanded,
                ..
            } = entry
            else {
                continue;
            };
            lines.extend(tool_render_lines(
                ToolRender {
                    label,
                    detail,
                    status: *status,
                    result: result.as_deref(),
                    expanded: *expanded,
                    duration: duration.as_deref(),
                    started_at: *started_at,
                    nested: true,
                    focused: state.focus == Focus::Scrollback
                        && state.focused_target_id.as_deref()
                            == state.entry_target_id(start + offset).as_deref(),
                    hovered: state.hovered_entry == Some(start + offset),
                    waiting_for_user: permission_matches_tool(state, label),
                },
                width,
                theme,
            ));
        }
    }
    (lines, count)
}

fn grouped_tool_summary(group: &[Entry], label: &str, failed: bool, count: usize) -> String {
    let aggregate = match label {
        "Read" => aggregate_detail_units(group, "file", "files"),
        "Search" => aggregate_detail_units(group, "query", "queries"),
        "Find" => aggregate_detail_units(group, "pattern", "patterns"),
        _ => None,
    };
    let mut summary = format!("{count} calls");
    if let Some((total, noun)) = aggregate {
        summary.push_str(&format!(" · {total} {noun}"));
    }
    if failed {
        let errors = group
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    Entry::Tool {
                        status: ToolStatus::Error,
                        ..
                    }
                )
            })
            .count();
        summary.push_str(&format!(" · {errors} failed"));
    }
    summary
}

fn aggregate_detail_units<'a>(
    group: &'a [Entry],
    singular: &'a str,
    plural: &'a str,
) -> Option<(usize, &'a str)> {
    let mut total = 0usize;
    for entry in group {
        let Entry::Tool { detail, .. } = entry else {
            return None;
        };
        let (count, remainder) = detail.split_once(' ')?;
        let noun = remainder.split_whitespace().next()?;
        let count = count.parse::<usize>().ok()?;
        if noun != singular && noun != plural {
            return None;
        }
        total = total.saturating_add(count);
    }
    Some((total, if total == 1 { singular } else { plural }))
}

#[allow(clippy::too_many_arguments)]
fn orchestration_header_line(
    state: &AppState,
    start: usize,
    count: usize,
    status: ToolStatus,
    expanded: bool,
    started_at: Option<std::time::Instant>,
    duration: Option<&str>,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let completed = state.entries[start..start + count]
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                Entry::Tool {
                    status: ToolStatus::Success,
                    ..
                }
            )
        })
        .count();
    let failed = state.entries[start..start + count]
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                Entry::Tool {
                    status: ToolStatus::Error,
                    ..
                }
            )
        })
        .count();
    let title = state.entries[..start]
        .iter()
        .rev()
        .find_map(|entry| match entry {
            Entry::User { text, .. } => text
                .split_once("Objective:")
                .map(|(_, objective)| objective.trim())
                .or(Some(text.as_str())),
            _ => None,
        })
        .unwrap_or("Delegated work");
    let elapsed = started_at
        .map(|start| format_elapsed(start.elapsed().as_millis() as u64))
        .or_else(|| duration.map(str::to_string))
        .unwrap_or_default();
    let marker = match status {
        ToolStatus::Running => ">",
        ToolStatus::Success => "+",
        ToolStatus::Error => "x",
    };
    let color = match status {
        ToolStatus::Running => theme.accent_running,
        ToolStatus::Success => theme.success,
        ToolStatus::Error => theme.error,
    };
    let summary = if failed > 0 {
        format!("{completed}/{count} complete · {failed} failed")
    } else {
        format!("{completed}/{count} complete")
    };
    let suffix = format!(
        "  {summary}  {elapsed}  {}",
        if expanded { "v" } else { ">" }
    );
    let budget = width.saturating_sub(suffix.chars().count() + 4);
    Line::from(vec![
        Span::styled(format!("{marker} "), Style::default().fg(color)),
        Span::styled(
            truncate(title, budget),
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(suffix, Style::default().fg(theme.muted)),
    ])
}

fn orchestration_task_lines(
    state: &AppState,
    index: usize,
    last: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let Some(task) = state
        .entries
        .get(index)
        .and_then(|entry| state.subagent_task_for_entry(entry))
    else {
        return Vec::new();
    };
    let elapsed = task.duration_ms;
    let (marker, color, state_label) = match task.status.as_str() {
        "running" => (">", theme.accent_running, "Running"),
        "completed" => ("+", theme.success, "Completed"),
        "cancelled" => ("-", theme.muted, "Cancelled"),
        _ => ("x", theme.error, "Failed"),
    };
    let rail = if last { "  `- " } else { "  |- " };
    let activity = compact_task_activity(&task.activity, &state.cwd);
    let right = format!("  {state_label}  {}", format_elapsed(elapsed));
    let description_budget = width.saturating_sub(rail.len() + right.len() + 6);
    let metadata = [
        task.model.as_deref().unwrap_or("inherited model"),
        task.thinking_level.as_deref().unwrap_or("default thinking"),
        task.subagent_type.as_str(),
    ]
    .join(" · ");
    vec![
        Line::from(vec![
            Span::styled(rail, Style::default().fg(theme.subtle)),
            Span::styled(format!("{marker} "), Style::default().fg(color)),
            Span::styled(
                truncate(&task.description, description_budget),
                Style::default().fg(theme.foreground),
            ),
            Span::styled(right, Style::default().fg(theme.muted)),
        ]),
        Line::from(vec![
            Span::styled(
                if last { "     " } else { "  |  " },
                Style::default().fg(theme.subtle),
            ),
            Span::styled(
                truncate(&activity, width.saturating_sub(7)),
                Style::default().fg(theme.text_secondary),
            ),
            Span::styled(
                format!("  · {metadata}"),
                Style::default().fg(theme.muted).add_modifier(Modifier::DIM),
            ),
        ]),
    ]
}

fn compact_task_activity(activity: &str, cwd: &str) -> String {
    let normalized = activity.replace('\\', "/");
    let (verb, value) = normalized
        .split_once(' ')
        .map_or(("", normalized.as_str()), |(verb, value)| (verb, value));
    let cwd = cwd.trim_start_matches("~/").replace('\\', "/");
    let compact = value
        .find(&cwd)
        .map(|index| {
            value[index + cwd.len()..]
                .trim_start_matches('/')
                .to_string()
        })
        .unwrap_or_else(|| value.to_string());
    let compact = match verb.to_ascii_lowercase().as_str() {
        "read" | "reading" => format!("Reading {compact}"),
        "search" | "searching" => format!("Searching {compact}"),
        "write" | "writing" => format!("Writing {compact}"),
        _ if verb.is_empty() => compact,
        _ => format!("{verb} {compact}"),
    };
    if compact.is_empty() {
        "Waiting for activity…".into()
    } else {
        compact
    }
}

fn duration_millis(value: &str) -> u64 {
    value
        .strip_suffix("ms")
        .and_then(|value| value.parse().ok())
        .or_else(|| {
            value
                .strip_suffix('s')
                .and_then(|value| value.parse::<f64>().ok())
                .map(|seconds| (seconds * 1000.0) as u64)
        })
        .unwrap_or(0)
}

fn permission_matches_tool(state: &AppState, label: &str) -> bool {
    state.pending_permission.as_ref().is_some_and(|permission| {
        permission.tool.eq_ignore_ascii_case(label)
            || permission
                .tool
                .to_ascii_lowercase()
                .contains(&label.to_ascii_lowercase())
    })
}

fn tool_render_lines(tool: ToolRender<'_>, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (marker, marker_color) = if tool.waiting_for_user {
        ("!", theme.accent_running)
    } else {
        match tool.status {
            ToolStatus::Running => ("*", theme.accent_running),
            ToolStatus::Success => ("+", theme.accent_tool),
            ToolStatus::Error => ("x", theme.error),
        }
    };
    let marker = if tool.hovered { ">" } else { marker };
    let elapsed = tool
        .started_at
        .map(|started| format_live_elapsed(started.elapsed()));
    let timing = elapsed.as_deref().or(tool.duration).unwrap_or("");
    let timing = if timing.is_empty() {
        String::new()
    } else {
        format!("  {timing}")
    };
    let detail_width =
        width.saturating_sub(tool.label.chars().count() + timing.chars().count() + 4);
    let prefix = if tool.nested { "  |- " } else { "" };
    let mut spans = vec![
        Span::styled(
            format!("{prefix}{marker} "),
            Style::default().fg(if tool.focused {
                theme.selection_border
            } else {
                marker_color
            }),
        ),
        Span::styled(
            format!("{} ", tool.label),
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    spans.extend(tool_detail_spans(
        tool.label,
        &truncate(tool.detail, detail_width),
        theme,
    ));
    spans.push(Span::styled(timing, Style::default().fg(theme.subtle)));
    let mut lines = vec![Line::from(spans)];

    if tool.expanded
        && let Some(result) = tool.result
    {
        let detail_indent = if tool.nested { 6 } else { 2 };
        let result_width = width.saturating_sub(detail_indent + 2);
        for (index, line) in markdown::wrap(result, result_width).into_iter().enumerate() {
            let prefix = if index == 0 {
                format!("{}└ ", " ".repeat(detail_indent))
            } else {
                " ".repeat(detail_indent + 2)
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(theme.subtle)),
                Span::styled(line, Style::default().fg(theme.muted)),
            ]));
        }
    }
    lines
}

fn tool_detail_spans(label: &str, detail: &str, theme: Theme) -> Vec<Span<'static>> {
    if label == "Search"
        && detail.starts_with('"')
        && let Some(query_end) = detail.get(1..).and_then(|rest| rest.find('"'))
    {
        let query_end = query_end + 2;
        let query = detail[..query_end].to_string();
        let remainder = &detail[query_end..];
        if let Some(path) = remainder.strip_prefix(" in ") {
            let (path, count) = path
                .rsplit_once(" (")
                .map_or((path, None), |(path, count)| (path, Some(count)));
            let mut spans = vec![
                Span::styled(query, Style::default().fg(theme.command)),
                Span::styled(" in ", Style::default().fg(theme.muted)),
                Span::styled(path.to_string(), Style::default().fg(theme.path)),
            ];
            if let Some(count) = count {
                spans.push(Span::styled(
                    format!(" ({count}"),
                    Style::default().fg(theme.subtle),
                ));
            }
            return spans;
        }
        return vec![Span::styled(query, Style::default().fg(theme.command))];
    }
    let color = if matches!(label, "Read" | "Edit" | "Write") && !detail.contains(" files") {
        theme.path
    } else {
        theme.muted
    };
    vec![Span::styled(detail.to_string(), Style::default().fg(color))]
}

fn format_live_elapsed(duration: std::time::Duration) -> String {
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

fn diff_render_lines(
    path: &str,
    diff_lines: &[DiffLine],
    expanded: bool,
    hovered: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let added = diff_lines
        .iter()
        .filter(|line| matches!(line.kind, DiffKind::Added))
        .count();
    let removed = diff_lines
        .iter()
        .filter(|line| matches!(line.kind, DiffKind::Removed))
        .count();
    // The change count belongs in the header whether or not the body is
    // expanded — collapsing a diff should not be the only way to see how many
    // lines it touched.
    let summary = format!("  +{added} -{removed}");
    let path_width = width.saturating_sub(7 + summary.chars().count());
    let mut lines = vec![Line::from(vec![
        Span::styled(
            if hovered { "> " } else { "+ " },
            Style::default().fg(theme.foreground),
        ),
        Span::styled(
            "Edit ",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(truncate(path, path_width), Style::default().fg(theme.path)),
        Span::styled("  +", Style::default().fg(theme.muted)),
        Span::styled(added.to_string(), Style::default().fg(theme.success)),
        Span::styled(" -", Style::default().fg(theme.muted)),
        Span::styled(removed.to_string(), Style::default().fg(theme.error)),
    ])];

    if expanded {
        for diff_line in diff_lines {
            let number = diff_line
                .number
                .map(|number| format!("{number:>5} "))
                .unwrap_or_else(|| "      ".into());
            let prefix = match diff_line.kind {
                DiffKind::Context => "  ",
                DiffKind::Added => "+ ",
                DiffKind::Removed => "- ",
            };
            let body_width = width.saturating_sub(number.chars().count());
            let body = pad(
                truncate(&format!("{prefix}{}", diff_line.text), body_width),
                body_width,
            );
            let body_style = match diff_line.kind {
                DiffKind::Context => Style::default().fg(theme.foreground),
                DiffKind::Added => Style::default()
                    .fg(theme.diff_insert_fg)
                    .bg(theme.diff_insert_bg),
                DiffKind::Removed => Style::default()
                    .fg(theme.diff_delete_fg)
                    .bg(theme.diff_delete_bg),
            };
            let number_color = match diff_line.kind {
                DiffKind::Added => theme.diff_insert_fg,
                DiffKind::Removed => theme.diff_delete_fg,
                DiffKind::Context => theme.muted,
            };
            lines.push(Line::from(vec![
                Span::styled(number, Style::default().fg(number_color)),
                Span::styled(body, body_style),
            ]));
        }
    }
    lines.push(Line::raw(""));
    lines
}

fn render_composer(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let image_labels = state
        .image_attachments
        .iter()
        .map(|image| {
            let mut name = image.name.chars().take(22).collect::<String>();
            if image.name.chars().count() > 22 {
                name.pop();
                name.push('…');
            }
            (image.id, format!("[{name}] "))
        })
        .collect::<Vec<_>>();
    let image_width = image_labels
        .iter()
        .map(|(_, label)| label.chars().count())
        .sum::<usize>();
    let available = (area.width.saturating_sub(5) as usize).saturating_sub(image_width);
    let prompt_length = if state.paste_blocks.is_empty() {
        state.prompt.chars().count()
    } else {
        state.composer_display_len()
    };
    let display_cursor = if state.paste_blocks.is_empty() {
        state.cursor
    } else {
        state.composer_display_cursor()
    };
    let start = display_cursor.saturating_sub(available.saturating_sub(1));
    let value = if state.prompt.is_empty() && state.image_attachments.is_empty() {
        state.placeholder.clone()
    } else {
        state.prompt.chars().skip(start).take(available).collect()
    };
    let value_style = if state.prompt.is_empty() && state.image_attachments.is_empty() {
        Style::default().fg(theme.subtle)
    } else {
        Style::default().fg(theme.foreground)
    };
    let border_color = if state.focus == Focus::Prompt {
        theme.prompt_border_active
    } else {
        theme.prompt_border
    };
    let model_label = format!(" {} ", state.model);
    let thinking_label = format!("· thinking {} ", state.thinking_level);
    let mode_label = if state.multiline_mode {
        format!("· {} · multiline ", state.permission_mode.label())
    } else {
        format!("· {} ", state.permission_mode.label())
    };
    let total = (model_label.chars().count()
        + thinking_label.chars().count()
        + mode_label.chars().count()) as u16;
    let mut x = area.right().saturating_sub(total + 1);
    let y = area.bottom().saturating_sub(1);
    let mut targets = state.composer_targets.borrow_mut();
    targets.clear();
    drop(targets);
    update_paste_targets(state, area, start);
    let mut targets = state.composer_targets.borrow_mut();
    for (kind, label) in [(0, &model_label), (1, &thinking_label), (2, &mode_label)] {
        let end = x.saturating_add(label.chars().count() as u16);
        targets.push((kind, x, end, y));
        x = end;
    }
    drop(targets);
    let title_spans = vec![
        Span::styled(
            model_label,
            Style::default()
                .fg(if state.composer_hover == Some(0) {
                    theme.foreground
                } else {
                    theme.accent_model
                })
                .bg(if state.composer_hover == Some(0) {
                    theme.bg_hover
                } else {
                    theme.background
                })
                .add_modifier(if state.composer_hover == Some(0) {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(
            thinking_label,
            Style::default()
                .fg(if state.composer_hover == Some(1) {
                    theme.foreground
                } else {
                    theme.accent_thinking
                })
                .bg(if state.composer_hover == Some(1) {
                    theme.bg_hover
                } else {
                    theme.background
                })
                .add_modifier(if state.composer_hover == Some(1) {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(
            mode_label,
            Style::default()
                .fg(if state.composer_hover == Some(2) {
                    theme.foreground
                } else {
                    theme.accent_plan
                })
                .bg(if state.composer_hover == Some(2) {
                    theme.bg_hover
                } else {
                    theme.background
                })
                .add_modifier(if state.composer_hover == Some(2) {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ];
    let title_line = Line::from(title_spans).alignment(Alignment::Right);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title_bottom(title_line);
    let mut prompt_spans = vec![Span::styled("> ", Style::default().fg(theme.foreground))];
    let mut image_x = area.x + 3;
    let mut image_targets = state.image_targets.borrow_mut();
    image_targets.clear();
    for (id, label) in &image_labels {
        let end = image_x + label.chars().count() as u16;
        image_targets.push((*id, image_x, end, area.y + 1));
        prompt_spans.push(Span::styled(
            label,
            Style::default().fg(
                if state.image_hover == Some(*id) || state.focused_image == Some(*id) {
                    theme.accent
                } else {
                    theme.muted
                },
            ),
        ));
        image_x = end;
    }
    drop(image_targets);
    if state.prompt.is_empty() && state.image_attachments.is_empty() {
        prompt_spans.push(Span::styled(value, value_style));
    } else if !state.prompt.is_empty() {
        prompt_spans.extend(composer_prompt_spans(state, start, available, theme));
    }
    if area.height <= 3 {
        frame.render_widget(Paragraph::new(Line::from(prompt_spans)).block(block), area);

        if state.focus == Focus::Prompt {
            let cursor_offset = if prompt_length == 0 {
                0
            } else {
                display_cursor.saturating_sub(start).min(available)
            };
            frame.set_cursor_position((
                area.x + 3 + image_width as u16 + cursor_offset as u16,
                area.y + 1,
            ));
        }
        return;
    }

    let display = if state.prompt.is_empty() && state.image_attachments.is_empty() {
        state.placeholder.clone()
    } else {
        state.composer_display_text()
    };
    let content_width = usize::from(area.width.saturating_sub(6)).max(1);
    let prompt_layout = crate::prompt::layout(&display, display_cursor, content_width);
    let visible_rows = usize::from(area.height.saturating_sub(2)).max(1);
    let first_row = prompt_layout
        .cursor_row
        .saturating_sub(visible_rows.saturating_sub(1));
    let mut rendered = Vec::new();
    for (row, text) in prompt_layout
        .lines
        .iter()
        .enumerate()
        .skip(first_row)
        .take(visible_rows)
    {
        let prefix = if row == 0 { "❯ " } else { "  " };
        let style = if state.prompt.is_empty() && state.image_attachments.is_empty() {
            Style::default().fg(theme.subtle)
        } else {
            Style::default().fg(theme.foreground)
        };
        let mut spans = vec![Span::styled(prefix, Style::default().fg(theme.foreground))];
        if row == 0 {
            spans.extend(image_labels.iter().map(|(id, label)| {
                Span::styled(
                    label.clone(),
                    Style::default().fg(
                        if state.image_hover == Some(*id) || state.focused_image == Some(*id) {
                            theme.accent_user
                        } else {
                            theme.gray_bright
                        },
                    ),
                )
            }));
        }
        spans.push(Span::styled(text.clone(), style));
        rendered.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(rendered).block(block), area);
    if state.focus == Focus::Prompt {
        frame.set_cursor_position((
            area.x
                .saturating_add(3)
                .saturating_add(if prompt_layout.cursor_row == 0 {
                    image_width as u16
                } else {
                    0
                })
                .saturating_add(prompt_layout.cursor_column as u16),
            area.y
                .saturating_add(1)
                .saturating_add(prompt_layout.cursor_row.saturating_sub(first_row) as u16),
        ));
    }
}

fn update_paste_targets(state: &AppState, area: Rect, start: usize) {
    let mut targets = state.paste_targets.borrow_mut();
    targets.clear();
    let mut display = 0usize;
    for block in &state.paste_blocks {
        display += block.start.saturating_sub(
            state
                .paste_blocks
                .iter()
                .find(|candidate| candidate.end <= block.start)
                .map_or(0, |candidate| candidate.end),
        );
        let label_width = format!("[paste#{}]", block.end - block.start)
            .chars()
            .count();
        let left = area
            .x
            .saturating_add(3)
            .saturating_add(display.saturating_sub(start) as u16);
        targets.push((
            block.id,
            left,
            left.saturating_add(label_width as u16),
            area.y + 1,
        ));
        display += label_width;
    }
}

fn composer_prompt_spans(
    state: &AppState,
    start: usize,
    available: usize,
    theme: Theme,
) -> Vec<Span<'static>> {
    let chars = state.prompt.chars().collect::<Vec<_>>();
    let end = (start + available).min(chars.len());
    let mut spans = Vec::new();
    let mut index = start;
    if !state.paste_blocks.is_empty() {
        return composer_paste_spans(state, &chars, theme);
    }
    while index < end {
        let token_start = index;
        while index < end && !chars[index].is_whitespace() {
            index += 1;
        }
        let token = chars[token_start..index].iter().collect::<String>();
        let valid = token.starts_with('/') && state.is_valid_slash_command(&token);
        spans.push(Span::styled(
            token,
            if valid {
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            },
        ));
        if index < end {
            let whitespace_start = index;
            while index < end && chars[index].is_whitespace() {
                index += 1;
            }
            spans.push(Span::styled(
                chars[whitespace_start..index].iter().collect::<String>(),
                Style::default().fg(theme.foreground),
            ));
        }
    }
    spans
}

fn composer_paste_spans(state: &AppState, chars: &[char], theme: Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut cursor = 0;
    for block in &state.paste_blocks {
        if block.start > cursor {
            spans.push(Span::styled(
                chars[cursor..block.start.min(chars.len())]
                    .iter()
                    .collect::<String>(),
                Style::default().fg(theme.foreground),
            ));
        }
        let label = format!("[paste#{}]", block.end.saturating_sub(block.start));
        let hovered = state.paste_hover == Some(block.id);
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(if hovered {
                    theme.background
                } else {
                    theme.accent_system
                })
                .bg(if hovered {
                    theme.accent_system
                } else {
                    theme.code_background
                })
                .add_modifier(Modifier::BOLD),
        ));
        cursor = block.end.min(chars.len());
    }
    if cursor < chars.len() {
        spans.push(Span::styled(
            chars[cursor..].iter().collect::<String>(),
            Style::default().fg(theme.foreground),
        ));
    }
    spans
}

fn render_shortcuts(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let context = match state.focus {
        Focus::Prompt => ActionContext::Prompt,
        Focus::Scrollback => ActionContext::Scrollback,
    };
    let mut hints = actions::hints(&[context, ActionContext::Agent]);
    if state.streaming {
        hints.retain(|action| action.id != ActionId::CycleMode);
    } else {
        hints.retain(|action| !matches!(action.id, ActionId::SendNow | ActionId::CancelTurn));
    }
    let left = hints
        .into_iter()
        .map(|action| {
            let label = match (action.id, state.streaming) {
                (ActionId::SendPrompt, true) => "Queue",
                (ActionId::CancelTurn, true) => "Cancel",
                _ => action.label,
            };
            format!("{}: {label}", action.primary.display())
        })
        .collect::<Vec<_>>()
        .join("  │  ");
    let status_color = if state.status.contains("unavailable") || state.status.contains("failed") {
        theme.error
    } else if state.status.contains("loading") {
        theme.warning
    } else {
        theme.muted
    };
    let status_budget = usize::from(area.width) / 3;
    let hide_redundant_status = (state.turn_started_at.is_some()
        || state
            .subagent_tasks
            .values()
            .any(|task| task.status == "running"))
        && !state.status.contains("failed")
        && !state.status.contains("unavailable");
    let status = if hide_redundant_status {
        String::new()
    } else {
        truncate(&state.status, status_budget)
    };
    let status_width = status.chars().count();
    let left_budget = usize::from(area.width).saturating_sub(status_width + 2);
    let left = truncate(&left, left_budget.saturating_sub(1));
    let used = 1 + left.chars().count() + status_width;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {left}"), Style::default().fg(theme.foreground)),
            Span::raw(" ".repeat(usize::from(area.width).saturating_sub(used))),
            Span::styled(status, Style::default().fg(status_color)),
        ])),
        area,
    );
}

fn compact_number(value: u64) -> String {
    if value >= 1_000 {
        format!("{}K", value / 1_000)
    } else {
        value.to_string()
    }
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

fn pad(mut value: String, width: usize) -> String {
    value.push_str(&" ".repeat(width.saturating_sub(value.chars().count())));
    value
}
