use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::{
    markdown,
    state::{AppState, DiffKind, DiffLine, Entry, Focus, ToolStatus},
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
    let banners = u16::from(state.turn_started_at.is_some())
        + u16::from(state.active_compaction_started_at().is_some());
    let viewport = height.saturating_sub(7).saturating_sub(banners);
    (3, 2, width.saturating_sub(6), viewport as usize)
}

fn build_layout_sections(state: &AppState, width: usize) -> (usize, Vec<LayoutSection>) {
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
                row += reasoning_lines(text, *active, *expanded, width, Theme::GROK_NIGHT).len();
                actionable = true;
            }
            Entry::Diff {
                path,
                lines,
                expanded,
                ..
            } => {
                row += diff_render_lines(path, lines, *expanded, false, width, Theme::GROK_NIGHT)
                    .len();
                actionable = true;
            }
            Entry::Tool { .. } => {
                let Entry::Tool { label, .. } = &state.entries[index] else {
                    unreachable!()
                };
                let count = state.entries[index..]
                    .iter()
                    .take_while(
                        |entry| matches!(entry, Entry::Tool { label: other, .. } if other == label),
                    )
                    .count();
                if count > 1 {
                    let Entry::Tool { id, .. } = &state.entries[index] else {
                        unreachable!()
                    };
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
                            row += tool_item_line_count(&state.entries[child], width, true);
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
                row += tool_item_line_count(&state.entries[index], width, false);
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
                    Theme::GROK_NIGHT,
                )
                .len();
            }
            Entry::CompactionIndicator { tokens_before, .. } => {
                row +=
                    1 + compaction_indicator_line(*tokens_before, width, Theme::GROK_NIGHT).len();
            }
            Entry::Assistant { lines, .. } => {
                row += 1 + markdown::render(lines, width, Theme::GROK_NIGHT).len()
            }
        }
        sections.push(LayoutSection {
            id: section_target_id(&state.entries[section_index], section_index),
            index: section_index,
            start,
            end: row.max(start + 1),
            actionable,
        });
        index += 1;
    }
    (row, sections)
}

fn section_target_id(entry: &Entry, index: usize) -> String {
    match entry {
        Entry::Tool { id, .. } => format!("tool:{id}"),
        Entry::Diff { id, .. } => format!("diff:{id}"),
        Entry::User { .. } => format!("user:{index}"),
        Entry::Reasoning { .. } => format!("reasoning:{index}"),
        Entry::Assistant { .. } => format!("assistant:{index}"),
        Entry::Compaction { .. } => format!("compaction:{index}"),
        Entry::CompactionIndicator { .. } => format!("compaction-indicator:{index}"),
    }
}

fn tool_item_line_count(entry: &Entry, width: usize, nested: bool) -> usize {
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
        },
        width,
        Theme::GROK_NIGHT,
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
    let section = &sections[next];
    let (index, start, end) = (section.index, section.start, section.end);
    state.focused_section = Some(next);
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
    let theme = Theme::GROK_NIGHT;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background).fg(theme.foreground)),
        frame.area(),
    );

    let outer = frame.area().inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    let compaction_active = state.active_compaction_started_at().is_some();
    let working_active = state.turn_started_at.is_some();
    let working_height: u16 = if working_active { 1 } else { 0 };
    let compaction_height: u16 = if compaction_active { 1 } else { 0 };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(working_height),
            Constraint::Length(compaction_height),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(outer);

    render_header(frame, areas[0], state, theme);
    render_transcript(frame, areas[1], state, theme);
    if working_active {
        render_working_banner(frame, areas[2], state, theme);
    }
    if compaction_active {
        render_compaction_banner(frame, areas[3], state, theme);
    }
    render_composer(frame, areas[4], state, theme);
    render_shortcuts(frame, areas[5], state, theme);
    crate::overlay::render(frame, state);
}

pub fn max_scroll(state: &AppState, width: u16, height: u16) -> usize {
    let (_, _, content_width, viewport) = transcript_geometry(state, width, height);
    TranscriptLayout::build(state, content_width.saturating_sub(1) as usize).max_scroll(viewport)
}

fn render_header(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let task_status = if state.tasks_total == 0 {
        String::new()
    } else {
        format!(" │ {}/{} ✓", state.tasks_complete, state.tasks_total)
    };
    let right = format!(
        "{} / {}{}",
        compact_number(state.context_used),
        compact_number(state.context_limit),
        task_status
    );
    let left = format!("⎇ {}  {}", state.branch, state.cwd);
    let gap = area
        .width
        .saturating_sub((left.chars().count() + right.chars().count()) as u16);
    let line = Line::from(vec![
        Span::styled(left, Style::default().fg(theme.muted)),
        Span::raw(" ".repeat(gap as usize)),
        Span::styled(right, Style::default().fg(theme.foreground)),
    ]);
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
                lines.extend(reasoning_lines(text, *active, *expanded, width, theme));
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
                Entry::Reasoning { .. } | Entry::Diff { .. } => {
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

    if let Some(target_id) = state.focused_target_id.as_deref()
        && let Some(section) = layout
            .sections
            .iter()
            .find(|section| section.id == target_id)
    {
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
    let style = Style::default()
        .fg(if preview { theme.muted } else { theme.accent })
        .add_modifier(if preview {
            Modifier::empty()
        } else {
            Modifier::BOLD
        });
    let visible_start = section.start.max(scroll);
    let visible_end = section.end.min(viewport_end);
    let clipped_below = visible_end < section.end;
    for row in visible_start..visible_end {
        let only = section.end.saturating_sub(section.start) == 1;
        let continuation = clipped_below && row.saturating_add(3) >= visible_end;
        let (left, right) = if preview {
            if only { ("‹", "›") } else { ("┆", "┆") }
        } else if continuation {
            ("┊", "┊")
        } else if only {
            ("[", "]")
        } else if row == section.start {
            ("┌", "┐")
        } else if row + 1 == section.end {
            ("└", "┘")
        } else {
            ("│", "│")
        };
        let y = content.y.saturating_add(row.saturating_sub(scroll) as u16);
        frame.render_widget(
            Paragraph::new(left).style(style),
            Rect::new(area.x, y, 1, 1),
        );
        frame.render_widget(
            Paragraph::new(right).style(style),
            Rect::new(area.right().saturating_sub(2), y, 1, 1),
        );
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
        "◌"
    } else if error.is_some() {
        "✕"
    } else {
        "◆"
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

fn reasoning_lines(
    text: &str,
    active: bool,
    expanded: bool,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    // Account for the `┊   ` prefix so reasoning never clips at the right edge.
    let wrapped = markdown::wrap(text, width.saturating_sub(4));
    let visible = if expanded {
        wrapped.len()
    } else if active {
        wrapped.len().min(3)
    } else {
        wrapped.len().min(1)
    };
    let glyph = if expanded { "⌄" } else { "›" };
    let indicator = if active { " ◌" } else { "" };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!("┊ {glyph} Thinking…"),
            Style::default().fg(theme.muted),
        ),
        Span::styled(indicator, Style::default().fg(theme.accent)),
    ])];
    for line in wrapped.into_iter().take(visible) {
        lines.push(Line::from(Span::styled(
            format!("┊   {line}"),
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        )));
    }
    lines.push(Line::raw(""));
    lines
}

fn user_card_lines(text: &str, timestamp: &str, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let style = Style::default()
        .bg(theme.user_background)
        .fg(theme.user_foreground);
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
}

const COMPACTION_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const WORKING_SPINNER: &[&str] = &["⠋", "⠙", "⠸", "⠴", "⠦", "⠇", "⠏", "⠋"];

fn render_working_banner(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let started_at = state
        .turn_started_at
        .unwrap_or_else(std::time::Instant::now);
    let elapsed_ms = started_at.elapsed().as_millis() as u64;
    let frame_index = (elapsed_ms / 100) as usize % WORKING_SPINNER.len();
    let spinner = WORKING_SPINNER[frame_index];
    // Estimate output tokens from the accumulated streamed characters. The
    // rule of thumb is roughly 4 characters per token for English text; the
    // banner shows this as a live estimate that gets replaced by the wire's
    // real number once TurnComplete arrives.
    let estimated_output_tokens = state.turn_output_chars / 4;
    let elapsed_label = format_elapsed(elapsed_ms);
    let left = format!("{spinner} Working…");
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
    let Entry::Tool { label, .. } = &state.entries[start] else {
        return (Vec::new(), 1);
    };
    let count = state.entries[start..]
        .iter()
        .take_while(|entry| matches!(entry, Entry::Tool { label: other, .. } if other == label))
        .count();
    if count == 1 {
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
                    focused: state.focused_tool == Some(start),
                    hovered: state.hovered_entry == Some(start),
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
    let noun = match label.as_str() {
        "Read" => "files",
        "Search" => "searches",
        "Agent" => "agents",
        _ => "calls",
    };
    let expanded = state.expanded_tool_groups.contains(&start);
    let mut lines = tool_render_lines(
        ToolRender {
            label,
            detail: &format!("{count} {noun} {}", if expanded { "⌄" } else { "›" }),
            status,
            result: None,
            expanded: false,
            duration,
            started_at,
            nested: false,
            focused: state
                .focused_tool
                .is_some_and(|focused| focused >= start && focused < start + count),
            hovered: state.hovered_entry == Some(start),
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
                    focused: state.focused_tool == Some(start + offset),
                    hovered: state.hovered_entry == Some(start + offset),
                },
                width,
                theme,
            ));
        }
    }
    (lines, count)
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

fn tool_render_lines(tool: ToolRender<'_>, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (marker, marker_color) = match tool.status {
        ToolStatus::Running => {
            const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];
            let frame = tool
                .started_at
                .map_or(0, |started| (started.elapsed().as_millis() / 120) as usize);
            (SPINNER[frame % SPINNER.len()], theme.warning)
        }
        ToolStatus::Success => ("◆", theme.foreground),
        ToolStatus::Error => ("◆", theme.error),
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
    let prefix = if tool.nested { "  ├ " } else { "" };
    let mut spans = vec![
        Span::styled(
            format!("{prefix}{marker} "),
            Style::default().fg(if tool.focused {
                theme.success
            } else {
                marker_color
            }),
        ),
        Span::styled(
            format!("{} ", tool.label),
            Style::default()
                .fg(theme.foreground)
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
                Span::styled(query, Style::default().fg(theme.success)),
                Span::styled(" in ", Style::default().fg(theme.muted)),
                Span::styled(path.to_string(), Style::default().fg(theme.warning)),
            ];
            if let Some(count) = count {
                spans.push(Span::styled(
                    format!(" ({count}"),
                    Style::default().fg(theme.subtle),
                ));
            }
            return spans;
        }
        return vec![Span::styled(query, Style::default().fg(theme.success))];
    }
    let color = if matches!(label, "Read" | "Edit" | "Write") && !detail.contains(" files") {
        theme.warning
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
    let summary = if expanded {
        String::new()
    } else {
        format!("  +{added} -{removed}")
    };
    let path_width = width.saturating_sub(7 + summary.chars().count());
    let mut lines = vec![Line::from(vec![
        Span::styled(
            if hovered { "> " } else { "◆ " },
            Style::default().fg(theme.foreground),
        ),
        Span::styled(
            "Edit ",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate(path, path_width),
            Style::default().fg(theme.warning),
        ),
        Span::styled(summary, Style::default().fg(theme.muted)),
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
                DiffKind::Context => Style::default().bg(theme.background).fg(theme.foreground),
                DiffKind::Added => Style::default().bg(theme.success).fg(theme.background),
                DiffKind::Removed => Style::default().bg(theme.error).fg(theme.background),
            };
            let number_color = match diff_line.kind {
                DiffKind::Added => theme.success,
                DiffKind::Removed => theme.error,
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
    let available = area.width.saturating_sub(5) as usize;
    let prompt_length = state.prompt.chars().count();
    let start = state.cursor.saturating_sub(available.saturating_sub(1));
    let value = if state.prompt.is_empty() {
        state.placeholder.clone()
    } else {
        state.prompt.chars().skip(start).take(available).collect()
    };
    let value_style = if state.prompt.is_empty() {
        Style::default().fg(theme.subtle)
    } else {
        Style::default().fg(theme.foreground)
    };
    let border_color = if state.focus == Focus::Prompt {
        theme.border
    } else {
        theme.subtle
    };
    let title_line = Line::from(vec![
        Span::styled(
            format!(" {} ", state.model),
            Style::default().fg(theme.muted).bg(theme.background),
        ),
        Span::styled(
            format!(" thinking {} ", state.thinking_level),
            Style::default().fg(theme.subtle),
        ),
        Span::styled(
            format!("{} ", state.permission_mode.label()),
            Style::default().fg(theme.subtle),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title_bottom(title_line)
        .title_alignment(Alignment::Right);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("▯ ", Style::default().fg(theme.foreground)),
            Span::styled(value, value_style),
        ]))
        .block(block),
        area,
    );

    if state.focus == Focus::Prompt {
        let cursor_offset = if prompt_length == 0 {
            0
        } else {
            state.cursor.saturating_sub(start).min(available)
        };
        frame.set_cursor_position((area.x + 3 + cursor_offset as u16, area.y + 1));
    }
}

fn render_shortcuts(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: Theme) {
    let left = match (state.focus, state.streaming) {
        (Focus::Prompt, true) => {
            " Enter: steer  │  Alt+Enter: follow-up  │  Esc: abort/restore queue"
        }
        (Focus::Prompt, false) => " Enter: send  │  Tab: scrollback  │  Shift+Tab: mode",
        (Focus::Scrollback, _) => {
            " ↑/↓: scroll  │  e: reasoning  │  t: tool  │  d: diff  │  Tab: prompt"
        }
    };
    let line = Line::from(Span::styled(left, Style::default().fg(theme.foreground)));
    frame.render_widget(Paragraph::new(line), area);
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
