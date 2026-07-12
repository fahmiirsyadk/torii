use std::collections::HashMap;

use pi_harness::SessionTreeEntry;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::{
    state::{AppState, OverlayKind},
    theme::Theme,
};

pub fn render(frame: &mut Frame<'_>, state: &AppState) {
    if state.overlay == OverlayKind::None {
        render_slash_suggestions(frame, state);
        return;
    }
    if state.overlay == OverlayKind::TreePicker {
        render_tree_picker(frame, state);
        return;
    }
    if state.overlay == OverlayKind::ForkPicker {
        render_fork_picker(frame, state);
        return;
    }

    let theme = Theme::GROK_NIGHT;
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));

    let items = state.overlay_items();
    let detail_rows = usize::from(state.overlay == OverlayKind::Permission) * 2
        + usize::from(matches!(
            state.overlay,
            OverlayKind::TreePicker | OverlayKind::ForkPicker
        )) * 2;
    let height = (items.len() + detail_rows + 4).clamp(6, 16) as u16;
    let width = frame_area.width.saturating_sub(6).clamp(30, 72);
    let area = centered(frame_area, width, height);
    frame.render_widget(Clear, area);

    let title = match state.overlay {
        OverlayKind::CommandPalette => " Commands ",
        OverlayKind::ModelPicker => " Select model ",
        OverlayKind::SessionPicker => " Resume session ",
        OverlayKind::TreePicker => " Session tree ",
        OverlayKind::ForkPicker => " Fork from prompt ",
        OverlayKind::TreeSummaryPicker => " Summarize branch? ",
        OverlayKind::TreeSummaryEditor => " Custom summarization instructions ",
        OverlayKind::LabelEditor => " Entry label ",
        OverlayKind::FilePicker => " Reference file ",
        OverlayKind::ScopedModels => " Scoped models ",
        OverlayKind::OauthPrompt => " OAuth input ",
        OverlayKind::OauthSelect => " OAuth selection ",
        OverlayKind::RewindPicker => " Rewind file edit ",
        OverlayKind::Settings => " Settings ",
        OverlayKind::Permission => " Permission required ",
        OverlayKind::None => "",
    };
    let mut lines = Vec::new();
    if matches!(
        state.overlay,
        OverlayKind::CommandPalette
            | OverlayKind::ModelPicker
            | OverlayKind::SessionPicker
            | OverlayKind::TreePicker
            | OverlayKind::ForkPicker
            | OverlayKind::TreeSummaryEditor
            | OverlayKind::LabelEditor
            | OverlayKind::FilePicker
            | OverlayKind::ScopedModels
            | OverlayKind::OauthPrompt
            | OverlayKind::OauthSelect
            | OverlayKind::RewindPicker
    ) {
        lines.push(Line::from(vec![
            Span::styled(
                if state.overlay == OverlayKind::LabelEditor {
                    "  Label: "
                } else if state.overlay == OverlayKind::TreeSummaryEditor {
                    "  Instructions: "
                } else if state.overlay == OverlayKind::OauthPrompt {
                    "  Reply: "
                } else {
                    "  Filter: "
                },
                Style::default().fg(theme.muted),
            ),
            Span::styled(
                if state.overlay_query.is_empty() {
                    if state.overlay == OverlayKind::LabelEditor {
                        "empty clears label…".to_string()
                    } else if state.overlay == OverlayKind::TreeSummaryEditor {
                        "what should the summary preserve?…".to_string()
                    } else if state.overlay == OverlayKind::OauthPrompt {
                        "enter OAuth value…".to_string()
                    } else {
                        "type to search…".to_string()
                    }
                } else if state.overlay == OverlayKind::OauthPrompt {
                    "•".repeat(state.overlay_query.chars().count())
                } else {
                    state.overlay_query.clone()
                },
                Style::default().fg(theme.foreground),
            ),
        ]));
        lines.push(Line::raw(""));
    }
    if matches!(
        state.overlay,
        OverlayKind::TreePicker | OverlayKind::ForkPicker
    ) {
        lines.push(Line::from(Span::styled(
            format!(
                "  Ctrl+O: {}  ·  Shift+L label  ·  Shift+T time  ·  Shift+Enter summarize",
                state.tree_filter.label()
            ),
            Style::default().fg(theme.muted),
        )));
        lines.push(Line::raw(""));
    }
    if let Some(permission) = &state.pending_permission
        && state.overlay == OverlayKind::Permission
    {
        lines.push(Line::from(vec![
            Span::styled("  Tool: ", Style::default().fg(theme.muted)),
            Span::styled(&permission.tool, Style::default().fg(theme.warning)),
        ]));
        lines.push(Line::from(Span::styled(
            format!("  {}", permission.reason),
            Style::default().fg(theme.muted),
        )));
    }
    if let Some(oauth) = &state.pending_oauth
        && matches!(
            state.overlay,
            OverlayKind::OauthPrompt | OverlayKind::OauthSelect
        )
    {
        lines.push(Line::from(Span::styled(
            format!("  {}", oauth.message),
            Style::default().fg(theme.muted),
        )));
    }
    for (index, item) in items.iter().enumerate() {
        let selected = index == state.overlay_selected;
        let marker = if selected { "› " } else { "  " };
        let model_current = state.overlay == OverlayKind::ModelPicker && item == &state.model;
        let session_current = state.overlay == OverlayKind::SessionPicker
            && state
                .available_sessions
                .iter()
                .any(|session| session.current && crate::state::session_label(session) == *item);
        let current = if model_current || session_current {
            "  ✓ current"
        } else {
            ""
        };
        let style = if selected {
            Style::default().fg(theme.background).bg(theme.foreground)
        } else {
            Style::default().fg(theme.foreground)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{item}{current}"),
            style,
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_alignment(Alignment::Left)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.background));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_tree_picker(frame: &mut Frame<'_>, state: &AppState) {
    let theme = Theme::GROK_NIGHT;
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));

    let entries = state.filtered_tree();
    let max_rows = usize::from(frame_area.height.saturating_sub(10)).max(1);
    let start = centered_window(state.overlay_selected, entries.len(), max_rows);
    let end = (start + max_rows).min(entries.len());
    let height = (end.saturating_sub(start) + 8) as u16;
    let area = centered(
        frame_area,
        frame_area.width.saturating_sub(2).max(1),
        height.min(frame_area.height.saturating_sub(1).max(1)),
    );
    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(Span::styled(
            " Session Tree",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            " ↑/↓ navigate  ·  ←/→ page  ·  Ctrl+←/→ fold/jump  ·  Ctrl+O filter  ·  Shift+L label  ·  Shift+T time",
            Style::default().fg(theme.muted),
        )),
        Line::from(vec![
            Span::styled(" Search: ", Style::default().fg(theme.muted)),
            Span::styled(
                if state.overlay_query.is_empty() {
                    "type to search…"
                } else {
                    &state.overlay_query
                },
                Style::default().fg(theme.foreground),
            ),
        ]),
        Line::raw(""),
    ];

    let topology = TreeTopology::new(&entries, &state.session_tree);
    if entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No entries found",
            Style::default().fg(theme.muted),
        )));
    } else {
        for (index, entry) in entries.iter().enumerate().take(end).skip(start) {
            lines.push(tree_row(
                entry,
                index == state.overlay_selected,
                state,
                &topology,
                theme,
            ));
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!(
            "  ({}/{}){}{}",
            state.overlay_selected.saturating_add(1).min(entries.len()),
            entries.len(),
            if state.tree_filter == crate::state::TreeFilter::Default {
                String::new()
            } else {
                format!(" [{}]", state.tree_filter.label())
            },
            if state.tree_show_timestamps {
                " [+label time]"
            } else {
                ""
            }
        ),
        Style::default().fg(theme.muted),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.background));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_fork_picker(frame: &mut Frame<'_>, state: &AppState) {
    let theme = Theme::GROK_NIGHT;
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));
    let entries = state.filtered_tree();
    let max_messages = 10usize
        .min(usize::from(frame_area.height.saturating_sub(10)) / 3)
        .max(1);
    let start = centered_window(state.overlay_selected, entries.len(), max_messages);
    let end = (start + max_messages).min(entries.len());
    let height = (end.saturating_sub(start) * 3 + 7) as u16;
    let area = centered(
        frame_area,
        frame_area.width.saturating_sub(2).max(1),
        height.min(frame_area.height.saturating_sub(1).max(1)),
    );
    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(Span::styled(
            " Fork from Message",
            Style::default()
                .fg(theme.foreground)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            " Select a user message to copy the active path up to that point into a new session",
            Style::default().fg(theme.muted),
        )),
        Line::raw(""),
    ];
    for (index, entry) in entries.iter().enumerate().take(end).skip(start) {
        let selected = index == state.overlay_selected;
        let style = Style::default()
            .fg(theme.foreground)
            .add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "› " } else { "  " },
                Style::default().fg(theme.accent),
            ),
            Span::styled(entry.text.trim(), style),
        ]));
        lines.push(Line::from(Span::styled(
            format!("  Message {} of {}", index + 1, entries.len()),
            Style::default().fg(theme.muted),
        )));
        lines.push(Line::raw(""));
    }
    if start > 0 || end < entries.len() {
        lines.push(Line::from(Span::styled(
            format!("  ({}/{})", state.overlay_selected + 1, entries.len()),
            Style::default().fg(theme.muted),
        )));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border))
        .style(Style::default().bg(theme.background));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn centered_window(selected: usize, count: usize, capacity: usize) -> usize {
    selected
        .saturating_sub(capacity / 2)
        .min(count.saturating_sub(capacity))
}

struct TreeTopology<'a> {
    visible_parent: HashMap<&'a str, Option<&'a str>>,
    visible_children: HashMap<Option<&'a str>, Vec<&'a str>>,
    visuals: HashMap<&'a str, TreeVisual>,
    multiple_roots: bool,
}

struct TreeVisual {
    indent: usize,
    show_connector: bool,
    is_last: bool,
    gutters: Vec<(usize, bool)>,
    virtual_root_child: bool,
}

impl<'a> TreeTopology<'a> {
    fn new(visible: &[&'a SessionTreeEntry], all: &'a [SessionTreeEntry]) -> Self {
        let entries = visible
            .iter()
            .map(|entry| (entry.id.as_str(), *entry))
            .collect::<HashMap<_, _>>();
        let visible_ids = entries
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let all_entries = all
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect::<HashMap<_, _>>();
        let mut visible_parent = HashMap::new();
        let mut visible_children: HashMap<Option<&str>, Vec<&str>> = HashMap::new();
        for entry in visible {
            let mut parent = entry.parent_id.as_deref();
            while parent.is_some_and(|id| !visible_ids.contains(id)) {
                parent = all_entries
                    .get(parent.unwrap())
                    .and_then(|item| item.parent_id.as_deref());
            }
            visible_parent.insert(entry.id.as_str(), parent);
            visible_children
                .entry(parent)
                .or_default()
                .push(entry.id.as_str());
        }
        let roots = visible_children.get(&None).cloned().unwrap_or_default();
        let multiple_roots = roots.len() > 1;
        let mut visuals = HashMap::new();
        let mut stack = Vec::new();
        for (index, root) in roots.iter().enumerate().rev() {
            stack.push((
                *root,
                usize::from(multiple_roots),
                multiple_roots,
                multiple_roots,
                index + 1 == roots.len(),
                Vec::new(),
                multiple_roots,
            ));
        }
        while let Some((
            id,
            indent,
            just_branched,
            show_connector,
            is_last,
            gutters,
            virtual_root_child,
        )) = stack.pop()
        {
            let children = visible_children.get(&Some(id)).cloned().unwrap_or_default();
            let multiple_children = children.len() > 1;
            let child_indent = if multiple_children || (just_branched && indent > 0) {
                indent + 1
            } else {
                indent
            };
            let mut child_gutters = gutters.clone();
            if show_connector && !virtual_root_child {
                let display_indent = if multiple_roots {
                    indent.saturating_sub(1)
                } else {
                    indent
                };
                child_gutters.push((display_indent.saturating_sub(1), !is_last));
            }
            visuals.insert(
                id,
                TreeVisual {
                    indent,
                    show_connector,
                    is_last,
                    gutters,
                    virtual_root_child,
                },
            );
            for (index, child) in children.iter().enumerate().rev() {
                stack.push((
                    *child,
                    child_indent,
                    multiple_children,
                    multiple_children,
                    index + 1 == children.len(),
                    child_gutters.clone(),
                    false,
                ));
            }
        }
        Self {
            visible_parent,
            visible_children,
            visuals,
            multiple_roots,
        }
    }

    fn visual(&self, id: &str) -> Option<&TreeVisual> {
        self.visuals.get(id)
    }

    fn is_foldable(&self, id: &str) -> bool {
        let has_children = self
            .visible_children
            .get(&Some(id))
            .is_some_and(|children| !children.is_empty());
        if !has_children {
            return false;
        }
        let parent = self.visible_parent.get(id).copied().flatten();
        parent.is_none()
            || self
                .visible_children
                .get(&parent)
                .is_some_and(|siblings| siblings.len() > 1)
    }
}

fn tree_row(
    entry: &SessionTreeEntry,
    selected: bool,
    state: &AppState,
    topology: &TreeTopology<'_>,
    theme: Theme,
) -> Line<'static> {
    let mut spans = Vec::new();
    spans.push(Span::styled(
        if selected { "› " } else { "  " },
        Style::default().fg(theme.accent),
    ));
    let foldable = topology.is_foldable(&entry.id);
    let visual = topology.visual(&entry.id);
    let display_indent = visual.map_or(0, |visual| {
        if topology.multiple_roots {
            visual.indent.saturating_sub(1)
        } else {
            visual.indent
        }
    });
    let connector =
        visual.is_some_and(|visual| visual.show_connector && !visual.virtual_root_child);
    let connector_position = connector.then(|| display_indent.saturating_sub(1));
    for level in 0..display_indent {
        let gutter = visual.and_then(|visual| {
            visual
                .gutters
                .iter()
                .find(|(position, _)| *position == level)
        });
        let prefix = if let Some((_, show)) = gutter {
            if *show {
                "│  ".to_string()
            } else {
                "   ".to_string()
            }
        } else if connector_position == Some(level) {
            let branch = if visual.is_some_and(|visual| visual.is_last) {
                '└'
            } else {
                '├'
            };
            let edge = if state.tree_folded.contains(&entry.id) {
                '⊞'
            } else if foldable {
                '⊟'
            } else {
                '─'
            };
            format!("{branch}{edge} ")
        } else {
            "   ".to_string()
        };
        spans.push(Span::styled(prefix, Style::default().fg(theme.subtle)));
    }
    if state.tree_folded.contains(&entry.id) && !connector {
        spans.push(Span::styled("⊞ ", Style::default().fg(theme.accent)));
    } else if foldable && !connector {
        spans.push(Span::styled("⊟ ", Style::default().fg(theme.subtle)));
    }
    if entry.active {
        spans.push(Span::styled("• ", Style::default().fg(theme.accent)));
    }
    if let Some(label) = &entry.label {
        spans.push(Span::styled(
            format!("[{label}] "),
            Style::default().fg(theme.warning),
        ));
        if state.tree_show_timestamps
            && let Some(timestamp) = &entry.label_timestamp
        {
            spans.push(Span::styled(
                format!("{} ", short_timestamp(timestamp)),
                Style::default().fg(theme.muted),
            ));
        }
    }
    let (label, color) = match (entry.kind.as_str(), entry.role.as_deref()) {
        ("message", Some("user")) => ("user: ".to_string(), Color::Cyan),
        ("message", Some("assistant")) => ("assistant: ".to_string(), theme.success),
        ("message", Some("toolResult")) => (String::new(), theme.muted),
        ("compaction", _) => ("[compaction]: ".to_string(), theme.accent),
        ("branch_summary", _) => ("[branch summary]: ".to_string(), theme.warning),
        (_, Some(role)) => (format!("{role}: "), theme.muted),
        (kind, None) => (format!("[{kind}]: "), theme.muted),
    };
    let selected_style = if selected {
        Modifier::BOLD
    } else {
        Modifier::empty()
    };
    spans.push(Span::styled(
        label,
        Style::default().fg(color).add_modifier(selected_style),
    ));
    let text = if entry.role.as_deref() == Some("assistant") && entry.text.trim().is_empty() {
        "(no content)".to_string()
    } else {
        entry.text.trim().to_string()
    };
    spans.push(Span::styled(
        text,
        Style::default()
            .fg(theme.foreground)
            .add_modifier(selected_style),
    ));
    let mut line = Line::from(spans);
    if selected {
        line = line.style(Style::default().bg(theme.code_background));
    }
    line
}

fn short_timestamp(timestamp: &str) -> String {
    timestamp.get(5..16).unwrap_or(timestamp).replace('T', " ")
}

fn render_slash_suggestions(frame: &mut Frame<'_>, state: &AppState) {
    if !state.prompt.starts_with('/') || state.prompt.contains(' ') {
        return;
    }
    let matches = state.slash_suggestions();
    if matches.is_empty() {
        return;
    }

    let theme = Theme::GROK_NIGHT;
    let height = matches.len() as u16 + 2;
    let width = frame.area().width.saturating_sub(6).min(56);
    let composer_y = frame.area().bottom().saturating_sub(5);
    let area = Rect::new(3, composer_y.saturating_sub(height), width, height);
    let lines = matches
        .into_iter()
        .enumerate()
        .map(|(index, (command, description))| {
            Line::from(vec![
                Span::styled(
                    format!("{} {command:<12}", if index == 0 { "›" } else { " " }),
                    Style::default().fg(theme.foreground),
                ),
                Span::styled(description, Style::default().fg(theme.muted)),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.subtle))
                .style(Style::default().bg(theme.background)),
        ),
        area,
    );
}

pub fn item_at(state: &AppState, width: u16, height: u16, row: u16) -> Option<usize> {
    let frame_area = Rect::new(0, 0, width, height);
    let entries = state.filtered_tree();
    match state.overlay {
        OverlayKind::TreePicker => {
            let max_rows = usize::from(height.saturating_sub(10)).max(1);
            let start = centered_window(state.overlay_selected, entries.len(), max_rows);
            let end = (start + max_rows).min(entries.len());
            let area_height = (end.saturating_sub(start) + 8) as u16;
            let area = centered(
                frame_area,
                width.saturating_sub(2).max(1),
                area_height.min(height.saturating_sub(1).max(1)),
            );
            let first_row = area.y.saturating_add(5);
            let offset = usize::from(row.saturating_sub(first_row));
            (row >= first_row && offset < end.saturating_sub(start)).then_some(start + offset)
        }
        OverlayKind::ForkPicker => {
            let max_messages = 10usize
                .min(usize::from(height.saturating_sub(10)) / 3)
                .max(1);
            let start = centered_window(state.overlay_selected, entries.len(), max_messages);
            let end = (start + max_messages).min(entries.len());
            let area_height = (end.saturating_sub(start) * 3 + 7) as u16;
            let area = centered(
                frame_area,
                width.saturating_sub(2).max(1),
                area_height.min(height.saturating_sub(1).max(1)),
            );
            let first_row = area.y.saturating_add(4);
            let offset = usize::from(row.saturating_sub(first_row));
            let item = offset / 3;
            (row >= first_row && item < end.saturating_sub(start)).then_some(start + item)
        }
        _ => None,
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
