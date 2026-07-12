use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    markdown,
    state::{AppState, DiffKind, DiffLine, Entry, Focus, ToolStatus},
    theme::Theme,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolHit {
    Group(usize),
    Item(usize),
}

pub fn tool_hit_at(
    state: &AppState,
    terminal_width: u16,
    terminal_height: u16,
    screen_row: u16,
) -> Option<ToolHit> {
    let width = terminal_width.saturating_sub(7) as usize;
    let viewport = terminal_height.saturating_sub(7) as usize;
    let total = max_scroll(state, terminal_width, terminal_height) + viewport;
    let max_scroll = total.saturating_sub(viewport);
    let scroll = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    let logical_row = usize::from(screen_row.saturating_sub(2)).saturating_add(scroll);
    let mut row = 0;
    let mut index = 0;
    while index < state.entries.len() {
        match &state.entries[index] {
            Entry::User { .. } => row += 5,
            Entry::Reasoning {
                text,
                active,
                expanded,
            } => {
                row += reasoning_lines(text, *active, *expanded, width, Theme::GROK_NIGHT).len();
            }
            Entry::Diff {
                path,
                lines,
                expanded,
            } => {
                row += diff_render_lines(path, lines, *expanded, width, Theme::GROK_NIGHT).len();
            }
            Entry::Compaction {
                summary,
                tokens_before,
                tokens_after,
                active,
                error,
                started_at: _,
            } => {
                row += compaction_lines(
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
                row += compaction_indicator_line(*tokens_before, width, Theme::GROK_NIGHT).len();
            }
            Entry::Assistant { lines, .. } => {
                row += markdown::render(lines, width, Theme::GROK_NIGHT).len() + 1;
            }
            Entry::Tool { label, .. } => {
                let count = state.entries[index..]
                    .iter()
                    .take_while(
                        |entry| matches!(entry, Entry::Tool { label: other, .. } if other == label),
                    )
                    .count();
                if logical_row == row {
                    return Some(if count > 1 {
                        ToolHit::Group(index)
                    } else {
                        ToolHit::Item(index)
                    });
                }
                row += 1;
                if count > 1 && state.expanded_tool_groups.contains(&index) {
                    for child in index..index + count {
                        if logical_row == row {
                            return Some(ToolHit::Item(child));
                        }
                        let Entry::Tool {
                            result, expanded, ..
                        } = &state.entries[child]
                        else {
                            continue;
                        };
                        row += 1;
                        if *expanded && let Some(result) = result {
                            row += markdown::wrap(result, width.saturating_sub(6)).len();
                        }
                    }
                }
                index += count.saturating_sub(1);
            }
        }
        index += 1;
    }
    None
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
    let banner_height: u16 = if compaction_active { 1 } else { 0 };
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(banner_height),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(outer);

    render_header(frame, areas[0], state, theme);
    render_transcript(frame, areas[1], state, theme);
    if compaction_active {
        render_compaction_banner(frame, areas[2], state, theme);
    }
    render_composer(frame, areas[3], state, theme);
    render_shortcuts(frame, areas[4], state, theme);
    crate::overlay::render(frame, state);
}

pub fn max_scroll(state: &AppState, width: u16, height: u16) -> usize {
    let content_width = width.saturating_sub(7) as usize;
    let mut line_count = 0;
    let mut index = 0;
    while index < state.entries.len() {
        let entry = &state.entries[index];
        line_count += match entry {
            Entry::User { .. } => 5,
            Entry::Reasoning {
                text,
                active,
                expanded,
            } => reasoning_lines(text, *active, *expanded, content_width, Theme::GROK_NIGHT).len(),
            Entry::Diff {
                path,
                lines,
                expanded,
            } => diff_render_lines(path, lines, *expanded, content_width, Theme::GROK_NIGHT).len(),
            Entry::Tool { .. } => {
                let (lines, consumed) =
                    tool_group_lines(state, index, content_width, Theme::GROK_NIGHT);
                index += consumed.saturating_sub(1);
                lines.len()
            }
            Entry::Compaction {
                summary,
                tokens_before,
                tokens_after,
                active,
                error,
                started_at: _,
            } => compaction_lines(
                summary,
                *tokens_before,
                *tokens_after,
                *active,
                error.as_deref(),
                content_width,
                Theme::GROK_NIGHT,
            )
            .len(),
            Entry::CompactionIndicator { tokens_before, .. } => {
                compaction_indicator_line(*tokens_before, content_width, Theme::GROK_NIGHT).len()
            }
            Entry::Assistant { lines, .. } => {
                markdown::render(lines, content_width, Theme::GROK_NIGHT).len() + 1
            }
        };
        index += 1;
    }
    line_count.saturating_sub(height.saturating_sub(7) as usize)
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
    let width = content.width.saturating_sub(1) as usize;
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
            } => {
                lines.extend(diff_render_lines(path, diff_lines, *expanded, width, theme));
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

    let line_count = lines.len();
    let viewport_height = content.height as usize;
    let max_scroll = line_count.saturating_sub(viewport_height);
    let scroll = max_scroll.saturating_sub(state.scroll_from_bottom.min(max_scroll));
    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(theme.foreground).bg(theme.background))
        .scroll((scroll as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, content);

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
    let glyph = if active { "◌" } else if error.is_some() { "✕" } else { "◆" };
    let title = if active {
        "Compacting context…".to_string()
    } else if error.is_some() {
        "Compaction failed".to_string()
    } else {
        "Compacted context".to_string()
    };
    let mut header = vec![Line::from(vec![
        Span::styled(
            format!("{glyph} {title}"),
            Style::default().fg(if error.is_some() { theme.error } else { theme.accent }),
        ),
    ])];
    if let (Some(before), Some(after)) = (tokens_before, tokens_after) {
        header.push(Line::from(Span::styled(
            format!("   {} → {} tokens", compact_number(before), compact_number(after)),
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
}

const COMPACTION_SPINNER: &[&str] = &[
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];

fn render_compaction_banner(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &AppState,
    theme: Theme,
) {
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
    spans.push(Span::styled(left.clone(), Style::default().fg(theme.accent)));
    if needed <= width {
        // 3-column layout: left | center | right
        let gap_left = (width - left_chars - center_chars - right_chars) / 2;
        let gap_right =
            width - left_chars - gap_left - center_chars - right_chars;
        spans.push(Span::raw(" ".repeat(gap_left)));
        spans.push(Span::styled(center, Style::default().fg(theme.foreground)));
        spans.push(Span::raw(" ".repeat(gap_right)));
        spans.push(Span::styled(right, Style::default().fg(theme.muted)));
    } else if left_chars + min_gap + right_chars <= width {
        // 2-column: left | right (drop center)
        let gap = width - left_chars - right_chars;
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(right, Style::default().fg(theme.muted)));
    } else {
        // Narrow: just truncate the left text
        let budget = width.saturating_sub(right_chars + min_gap);
        let truncated_left: String = if left.chars().count() > budget {
            let take = budget.saturating_sub(1);
            let mut s: String = left.chars().take(take).collect();
            s.push('…');
            s
        } else {
            left.clone()
        };
        spans.clear();
        spans.push(Span::styled(truncated_left, Style::default().fg(theme.accent)));
    }
    let line = Line::from(spans);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(theme.background)),
        area,
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
    let focus = if tool.focused { "▌  " } else { "   " };
    let mut spans = vec![
        Span::styled(
            format!("{focus}{prefix}{marker} "),
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
        let detail_indent = if tool.nested { 9 } else { 5 };
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
        Span::styled("◆ ", Style::default().fg(theme.foreground)),
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
