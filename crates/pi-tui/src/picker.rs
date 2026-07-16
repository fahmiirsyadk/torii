//! Shared Grok-style modal and picker primitives.
//!
//! Adapted from Grok Build's Apache-2.0 picker/modal component model. Torii
//! keeps the implementation small and state-free so rendering and mouse hit
//! testing always derive from the same geometry.

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PickerRow {
    pub item_index: Option<usize>,
    pub label: String,
    pub description: String,
    pub right: String,
    pub badge: String,
    pub checked: Option<bool>,
    pub current: bool,
    pub disabled: bool,
    pub header: bool,
}

impl PickerRow {
    pub fn item(index: usize, label: impl Into<String>) -> Self {
        Self {
            item_index: Some(index),
            label: label.into(),
            ..Self::default()
        }
    }

    pub fn header(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            header: true,
            ..Self::default()
        }
    }
}

pub struct PickerSpec<'a> {
    pub title: &'a str,
    pub query: Option<(&'a str, &'a str)>,
    pub notes: &'a [String],
    pub rows: &'a [PickerRow],
    pub footer: &'a str,
    pub max_width: u16,
    pub max_height: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PickerLayout {
    pub modal: Rect,
    pub rows: Rect,
    pub visible_start: usize,
    pub visible_end: usize,
    pub selected_visual: Option<usize>,
}

pub fn layout(area: Rect, spec: &PickerSpec<'_>, selected: usize) -> PickerLayout {
    let width = area
        .width
        .saturating_sub(4)
        .min(spec.max_width)
        .max(20)
        .min(area.width);
    let query_rows = usize::from(spec.query.is_some()) * 2;
    let fixed = 2usize + query_rows + spec.notes.len() + 1;
    let desired = (fixed + spec.rows.len()).max(7) as u16;
    let max_height = area.height.saturating_sub(2).min(spec.max_height).max(6);
    let height = desired.min(max_height).min(area.height);
    let modal = centered(area, width, height);
    let rows_y = modal.y + 1 + query_rows as u16 + spec.notes.len() as u16;
    let rows_height = modal.bottom().saturating_sub(rows_y).saturating_sub(2);
    let rows_area = Rect::new(
        modal.x + 2,
        rows_y,
        modal.width.saturating_sub(4),
        rows_height,
    );
    let selected_visual = spec
        .rows
        .iter()
        .position(|row| row.item_index == Some(selected));
    let capacity = usize::from(rows_area.height).max(1);
    let visible_start = selected_visual
        .map(|visual| centered_window(visual, spec.rows.len(), capacity))
        .unwrap_or(0);
    PickerLayout {
        modal,
        rows: rows_area,
        visible_start,
        visible_end: (visible_start + capacity).min(spec.rows.len()),
        selected_visual,
    }
}

pub fn render(
    frame: &mut Frame<'_>,
    spec: &PickerSpec<'_>,
    selected: usize,
    theme: Theme,
) -> PickerLayout {
    let geometry = layout(frame.area(), spec, selected);
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));
    frame.render_widget(Clear, geometry.modal);
    let block = modal_block(spec.title, theme);
    frame.render_widget(block, geometry.modal);

    let mut y = geometry.modal.y + 1;
    if let Some((label, query)) = spec.query {
        let placeholder = if query.is_empty() {
            "type to search…"
        } else {
            query
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {label}: "), Style::default().fg(theme.muted)),
                Span::styled(
                    placeholder.to_string(),
                    Style::default().fg(if query.is_empty() {
                        theme.subtle
                    } else {
                        theme.foreground
                    }),
                ),
                Span::styled("_", Style::default().fg(theme.foreground)),
            ]))
            .style(Style::default().bg(theme.background)),
            Rect::new(
                geometry.modal.x + 1,
                y,
                geometry.modal.width.saturating_sub(2),
                1,
            ),
        );
        y += 1;
        let divider = "─".repeat(usize::from(geometry.modal.width.saturating_sub(4)));
        frame.render_widget(
            Paragraph::new(Line::styled(divider, Style::default().fg(theme.subtle))),
            Rect::new(
                geometry.modal.x + 2,
                y,
                geometry.modal.width.saturating_sub(4),
                1,
            ),
        );
        y += 1;
    }
    for note in spec.notes {
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {note}"),
                Style::default().fg(theme.muted),
            )),
            Rect::new(
                geometry.modal.x + 1,
                y,
                geometry.modal.width.saturating_sub(2),
                1,
            ),
        );
        y += 1;
    }

    let visible = &spec.rows[geometry.visible_start..geometry.visible_end];
    let label_width = visible
        .iter()
        .filter(|row| !row.header)
        .map(|row| row.label.width())
        .max()
        .unwrap_or(0)
        .min(usize::from(geometry.rows.width) * 3 / 5)
        .min(38);
    for (offset, row) in visible.iter().enumerate() {
        let row_y = geometry.rows.y + offset as u16;
        let selected_here = row.item_index == Some(selected);
        if row.header {
            let available = usize::from(geometry.rows.width).saturating_sub(row.label.width() + 5);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("── ", Style::default().fg(theme.subtle)),
                    Span::styled(
                        row.label.clone(),
                        Style::default()
                            .fg(theme.gray_bright)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" {}", "─".repeat(available)),
                        Style::default().fg(theme.subtle),
                    ),
                ])),
                Rect::new(geometry.rows.x, row_y, geometry.rows.width, 1),
            );
            continue;
        }
        let row_bg = if selected_here {
            theme.bg_highlight
        } else {
            theme.background
        };
        let primary = if row.disabled {
            theme.subtle
        } else if selected_here {
            theme.foreground
        } else {
            theme.text_secondary
        };
        let prefix = if selected_here { "❯ " } else { "  " };
        let check = match row.checked {
            Some(true) => "[✓] ",
            Some(false) => "[ ] ",
            None => "",
        };
        let mut left = format!("{prefix}{check}{}", row.label);
        // Keep descriptions in a stable column with enough visual separation
        // from the primary label even when the label is the widest row.
        let target = 4 + usize::from(row.checked.is_some()) * 4 + label_width;
        let left_width = left.width();
        if left_width < target {
            left.push_str(&" ".repeat(target - left_width));
        }
        let suffix = if row.current {
            "✓ current"
        } else {
            &row.right
        };
        let reserved = suffix.width() + usize::from(!suffix.is_empty()) * 2;
        let desc_budget =
            usize::from(geometry.rows.width).saturating_sub(left.width() + reserved + 2);
        let description = truncate_width(&row.description, desc_budget);
        let used = left.width() + description.width() + reserved;
        let gap = usize::from(geometry.rows.width).saturating_sub(used);
        let mut spans = vec![
            Span::styled(
                left,
                Style::default()
                    .fg(primary)
                    .bg(row_bg)
                    .add_modifier(if selected_here {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(description, Style::default().fg(theme.muted).bg(row_bg)),
            Span::styled(" ".repeat(gap), Style::default().bg(row_bg)),
        ];
        if !suffix.is_empty() {
            spans.push(Span::styled(
                suffix.to_string(),
                Style::default()
                    .fg(if row.current {
                        theme.success
                    } else {
                        theme.gray_bright
                    })
                    .bg(row_bg),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(row_bg)),
            Rect::new(geometry.rows.x, row_y, geometry.rows.width, 1),
        );
    }

    render_scrollbar(frame, geometry, spec.rows.len(), theme);
    let footer = if spec.rows.is_empty() {
        spec.footer.to_string()
    } else {
        format!(
            "{}  ·  {}/{}",
            spec.footer,
            selected
                .saturating_add(1)
                .min(spec.rows.iter().filter(|r| r.item_index.is_some()).count()),
            spec.rows.iter().filter(|r| r.item_index.is_some()).count()
        )
    };
    frame.render_widget(
        Paragraph::new(Line::styled(footer, Style::default().fg(theme.muted)))
            .alignment(Alignment::Center),
        Rect::new(
            geometry.modal.x + 1,
            geometry.modal.bottom().saturating_sub(2),
            geometry.modal.width.saturating_sub(2),
            1,
        ),
    );
    geometry
}

pub fn modal_block(title: &str, theme: Theme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.background))
        .title(
            Line::from(Span::styled(
                format!("─ {title} "),
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Left),
        )
        .title(
            Line::from(Span::styled(" [×] ", Style::default().fg(theme.muted)))
                .alignment(Alignment::Right),
        )
}

pub fn item_at(layout: PickerLayout, rows: &[PickerRow], column: u16, row: u16) -> Option<usize> {
    if !layout.rows.contains((column, row).into()) {
        return None;
    }
    let visual = layout.visible_start + usize::from(row - layout.rows.y);
    rows.get(visual)?.item_index
}

fn render_scrollbar(frame: &mut Frame<'_>, geometry: PickerLayout, total: usize, theme: Theme) {
    render_scrollbar_for(
        frame,
        geometry.rows,
        geometry.visible_start,
        geometry.visible_end.saturating_sub(geometry.visible_start),
        total,
        theme,
    );
}

pub fn render_scrollbar_for(
    frame: &mut Frame<'_>,
    area: Rect,
    start: usize,
    visible: usize,
    total: usize,
    theme: Theme,
) {
    if total <= visible || area.height == 0 || visible == 0 {
        return;
    }
    let track = area.height;
    let thumb = ((usize::from(track) * visible) / total).max(1) as u16;
    let max_start = track.saturating_sub(thumb);
    let max_scroll = total.saturating_sub(visible).max(1);
    let thumb_start = ((start * usize::from(max_start)) / max_scroll) as u16;
    let x = area.right().saturating_sub(1);
    for offset in 0..track {
        let glyph = if offset >= thumb_start && offset < thumb_start + thumb {
            "┃"
        } else {
            "│"
        };
        frame.render_widget(
            Paragraph::new(Line::styled(
                glyph,
                Style::default().fg(if glyph == "┃" {
                    theme.gray_bright
                } else {
                    theme.subtle
                }),
            )),
            Rect::new(x, area.y + offset, 1, 1),
        );
    }
}

fn truncate_width(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    if width < 2 {
        return String::new();
    }
    let mut output = String::new();
    for ch in value.chars() {
        if output.width() + ch.width().unwrap_or(0) >= width {
            break;
        }
        output.push(ch);
    }
    output.push('…');
    output
}

fn centered_window(selected: usize, total: usize, capacity: usize) -> usize {
    if total <= capacity {
        0
    } else {
        selected
            .saturating_sub(capacity / 2)
            .min(total.saturating_sub(capacity))
    }
}

fn centered(parent: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        parent.x + parent.width.saturating_sub(width) / 2,
        parent.y + parent.height.saturating_sub(height) / 2,
        width.min(parent.width),
        height.min(parent.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_kept_inside_a_bounded_viewport() {
        let rows = (0..40)
            .map(|i| PickerRow::item(i, format!("row {i}")))
            .collect::<Vec<_>>();
        let spec = PickerSpec {
            title: "Picker",
            query: Some(("Search", "")),
            notes: &[],
            rows: &rows,
            footer: "Enter confirm",
            max_width: 90,
            max_height: 18,
        };
        let layout = layout(Rect::new(0, 0, 100, 24), &spec, 30);
        assert!(layout.visible_start <= 30);
        assert!(layout.visible_end > 30);
        assert!(layout.visible_end < rows.len());
    }

    #[test]
    fn hit_testing_skips_headers() {
        let rows = vec![PickerRow::header("General"), PickerRow::item(0, "First")];
        let spec = PickerSpec {
            title: "Settings",
            query: None,
            notes: &[],
            rows: &rows,
            footer: "Esc close",
            max_width: 80,
            max_height: 20,
        };
        let layout = layout(Rect::new(0, 0, 100, 30), &spec, 0);
        assert_eq!(item_at(layout, &rows, layout.rows.x, layout.rows.y), None);
        assert_eq!(
            item_at(layout, &rows, layout.rows.x, layout.rows.y + 1),
            Some(0)
        );
    }
}
