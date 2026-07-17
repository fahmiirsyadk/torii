use std::collections::HashMap;

use pi_harness::SessionTreeEntry;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
    agent_layout::AgentLayout,
    picker::{self, PickerRow, PickerSpec},
    state::{AppState, ImageAttachment, OverlayKind},
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
    if state.overlay == OverlayKind::PasteEditor {
        render_paste_editor(frame, state);
        return;
    }
    if state.overlay == OverlayKind::ImageViewer {
        render_image_viewer(frame, state);
        return;
    }
    render_grok_picker(frame, state);
}

struct GenericPickerData {
    title: &'static str,
    query_label: Option<&'static str>,
    notes: Vec<String>,
    rows: Vec<PickerRow>,
    footer: &'static str,
    max_width: u16,
    max_height: u16,
}

fn render_grok_picker(frame: &mut Frame<'_>, state: &AppState) {
    let data = generic_picker_data(state);
    let display_query = if state.overlay == OverlayKind::OauthPrompt {
        "•".repeat(state.overlay_query.chars().count())
    } else {
        state.overlay_query.clone()
    };
    let spec = PickerSpec {
        title: data.title,
        query: data
            .query_label
            .map(|label| (label, display_query.as_str())),
        notes: &data.notes,
        rows: &data.rows,
        footer: data.footer,
        max_width: data.max_width,
        max_height: data.max_height,
    };
    picker::render(
        frame,
        &spec,
        state.overlay_selected,
        state.overlay_hovered,
        state.overlay_close_hovered,
        state.theme(),
    );
}

fn generic_picker_data(state: &AppState) -> GenericPickerData {
    let title = match state.overlay {
        OverlayKind::CommandPalette => "Commands",
        OverlayKind::ModelPicker => "Select model",
        OverlayKind::WorkflowPicker => "Workflow catalog",
        OverlayKind::WorkflowPreview => "Workflow preflight",
        OverlayKind::SessionPicker => "Resume session",
        OverlayKind::SessionRename => "Rename session",
        OverlayKind::SessionDeleteConfirm => "Delete session?",
        OverlayKind::TreePicker => "Session tree",
        OverlayKind::ForkPicker => "Fork from prompt",
        OverlayKind::TreeSummaryPicker => "Summarize branch?",
        OverlayKind::TreeSummaryEditor => "Custom summary instructions",
        OverlayKind::PasteEditor => "Edit pasted text",
        OverlayKind::ImageViewer => "Image attachment",
        OverlayKind::LabelEditor => "Entry label",
        OverlayKind::FilePicker => "Reference file",
        OverlayKind::ScopedModels => "Scoped models",
        OverlayKind::SubagentModelPicker => "Subagent model",
        OverlayKind::OauthPrompt => "OAuth input",
        OverlayKind::OauthSelect => "OAuth selection",
        OverlayKind::LoginProvider => "Login provider",
        OverlayKind::ThinkingPicker => "Thinking effort",
        OverlayKind::RewindPicker => "Rewind file edit",
        OverlayKind::Settings => "Settings",
        OverlayKind::Permission => "Permission required",
        OverlayKind::None => "",
    };
    let query_label = overlay_has_query(state.overlay).then_some(match state.overlay {
        OverlayKind::LabelEditor => "Label",
        OverlayKind::SessionRename => "Name",
        OverlayKind::TreeSummaryEditor => "Instructions",
        OverlayKind::OauthPrompt => "Reply",
        _ => "Search",
    });
    let mut notes = Vec::new();
    if state.overlay == OverlayKind::SessionPicker {
        notes.push(format!(
            "Sort {}  ·  {} sessions  ·  paths {}",
            state.session_sort.label(),
            if state.session_named_only {
                "named"
            } else {
                "all"
            },
            if state.session_show_path {
                "shown"
            } else {
                "hidden"
            }
        ));
    }
    if state.overlay == OverlayKind::SessionDeleteConfirm {
        notes.push(format!(
            "Delete {}? This cannot be undone.",
            state.pending_session_path.as_deref().unwrap_or_default()
        ));
    }
    if let Some(permission) = &state.pending_permission
        && state.overlay == OverlayKind::Permission
    {
        notes.push(format!("Tool: {}", permission.tool));
        notes.push(permission.reason.clone());
    }
    if let Some(oauth) = &state.pending_oauth
        && matches!(
            state.overlay,
            OverlayKind::OauthPrompt | OverlayKind::OauthSelect
        )
    {
        notes.push(oauth.message.clone());
    }
    let items = state.overlay_items();
    let filtered_models = state.filtered_models();
    let subagent_inherit_visible = state.overlay == OverlayKind::SubagentModelPicker
        && (state.overlay_query.is_empty()
            || "inherit parent (default)".contains(&state.overlay_query.to_ascii_lowercase()));
    let mut rows = if state.overlay == OverlayKind::Settings {
        settings_rows(state)
    } else {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let mut row = PickerRow::item(index, item.clone());
                let row_model = match state.overlay {
                    OverlayKind::ModelPicker | OverlayKind::ScopedModels => {
                        filtered_models.get(index).copied()
                    }
                    OverlayKind::SubagentModelPicker => index
                        .checked_sub(usize::from(subagent_inherit_visible))
                        .and_then(|model_index| filtered_models.get(model_index).copied()),
                    _ => None,
                };
                if let Some(model) = row_model {
                    row.description = model
                        .id
                        .split_once('/')
                        .map_or("unknown", |(provider, _)| provider)
                        .to_string();
                }
                row.current = match state.overlay {
                    OverlayKind::ModelPicker => {
                        row_model.is_some_and(|model| model.display_name == state.model)
                    }
                    OverlayKind::SubagentModelPicker => {
                        match state.runtime_settings.subagent_model.as_deref() {
                            None => row_model.is_none() && item == "Inherit parent (default)",
                            Some(id) => row_model.is_some_and(|model| model.id == id),
                        }
                    }
                    OverlayKind::SessionPicker => state
                        .filtered_sessions()
                        .get(index)
                        .is_some_and(|session| session.current),
                    _ => false,
                };
                if state.overlay == OverlayKind::ScopedModels {
                    row.checked = Some(item.starts_with("[✓]"));
                    row.label = item
                        .strip_prefix("[✓] ")
                        .or_else(|| item.strip_prefix("[ ] "))
                        .unwrap_or(item)
                        .to_string();
                }
                if state.overlay == OverlayKind::CommandPalette {
                    let parts = item.split("  ·  ").collect::<Vec<_>>();
                    row.label = parts.first().copied().unwrap_or(item).to_string();
                    if parts.len() == 3 {
                        row.right = parts[1].to_string();
                        row.description = parts[2].to_string();
                    } else if parts.len() == 2 {
                        row.description = parts[1].to_string();
                    }
                }
                row
            })
            .collect::<Vec<_>>()
    };
    if state.overlay == OverlayKind::SessionPicker && rows.is_empty() {
        notes.push(if state.session_named_only {
            "No named sessions. Ctrl+N shows all sessions.".into()
        } else {
            "No sessions found.".into()
        });
    }
    if state.overlay == OverlayKind::SessionDeleteConfirm {
        rows.push(PickerRow::item(0, "Delete session"));
    }
    let footer = match state.overlay {
        OverlayKind::SessionPicker => {
            "↑/↓ navigate · Enter open · Ctrl+S sort · Ctrl+N named · Ctrl+P paths · Ctrl+R rename · Ctrl+D delete · Esc close"
        }
        OverlayKind::ScopedModels => "↑/↓ navigate · Space toggle · Enter apply · Esc back",
        OverlayKind::Settings => "↑/↓ navigate · Space/Enter change · Esc close",
        OverlayKind::WorkflowPreview => "↑/↓ scroll · Enter use workflow · Esc back",
        OverlayKind::SessionDeleteConfirm => "Enter delete · Esc cancel",
        _ => "↑/↓ navigate · Enter confirm · Esc close",
    };
    GenericPickerData {
        title,
        query_label,
        notes,
        rows,
        footer,
        max_width: if state.overlay == OverlayKind::WorkflowPreview {
            110
        } else {
            92
        },
        max_height: if state.overlay == OverlayKind::WorkflowPreview {
            30
        } else {
            24
        },
    }
}

fn settings_rows(state: &AppState) -> Vec<PickerRow> {
    let mut rows = vec![PickerRow::header("Agent behavior")];
    let values = [
        (
            "Steering delivery",
            state.runtime_settings.steering_mode.as_str(),
            "How immediate steering messages are drained",
        ),
        (
            "Follow-up delivery",
            state.runtime_settings.follow_up_mode.as_str(),
            "How queued next-turn prompts are drained",
        ),
        (
            "Auto compaction",
            if state.runtime_settings.auto_compaction {
                "on"
            } else {
                "off"
            },
            "Compact context automatically near the model limit",
        ),
        (
            "Default project trust",
            state.runtime_settings.default_project_trust.as_str(),
            "Default policy for project-local resources",
        ),
        (
            "Current project trusted",
            if state.runtime_settings.project_trusted {
                "yes"
            } else {
                "no"
            },
            "Allow this project's agents and workflows",
        ),
    ];
    rows.extend(
        values
            .into_iter()
            .enumerate()
            .map(|(index, (label, value, description))| {
                let mut row = PickerRow::item(index, label);
                row.right = value.into();
                row.description = description.into();
                row
            }),
    );
    rows.push(PickerRow::header("Models"));
    let mut scoped = PickerRow::item(5, "Scoped models");
    scoped.right = state.runtime_settings.enabled_models.len().to_string();
    scoped.description = "Limit model cycling and selection".into();
    rows.push(scoped);
    let mut subagent = PickerRow::item(6, "Subagent model");
    subagent.right = state
        .runtime_settings
        .subagent_model
        .as_deref()
        .unwrap_or("inherit parent")
        .into();
    subagent.description = "Default model for native delegated tasks".into();
    rows.push(subagent);
    rows.push(PickerRow::header("Appearance"));
    let mut theme = PickerRow::item(7, "Theme");
    theme.right = state.theme_mode.label().into();
    theme.description = "Switch between Grok dark and light palettes".into();
    rows.push(theme);
    rows
}

fn render_image_viewer(frame: &mut Frame<'_>, state: &AppState) {
    let theme = state.theme();
    let Some(image) = state.viewed_image() else {
        return;
    };
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));
    let (area, display_width, display_height) = image_viewer_geometry(image, frame_area);
    let preview_rows = usize::from(display_height).div_ceil(2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        picker::modal_block("Image attachment", state.overlay_close_hovered, theme),
        area,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                &image.name,
                Style::default().fg(theme.foreground),
            )),
            Line::from(Span::styled(
                format!("{}×{} · {}", image.width, image.height, image.mime_type),
                Style::default().fg(theme.muted),
            )),
        ])
        .style(Style::default().bg(theme.background)),
        Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 2),
    );
    let preview_x = area.x + area.width.saturating_sub(display_width) / 2;
    let preview_y = area.y + 3;
    for row in 0..preview_rows {
        for column in 0..usize::from(display_width) {
            let source_x = column * usize::from(image.preview_width) / usize::from(display_width);
            let source_top_y =
                row * 2 * usize::from(image.preview_height) / usize::from(display_height);
            let source_bottom_y = ((row * 2 + 1).min(usize::from(display_height) - 1)
                * usize::from(image.preview_height)
                / usize::from(display_height))
            .min(usize::from(image.preview_height) - 1);
            let top = (source_top_y * usize::from(image.preview_width) + source_x) * 4;
            let bottom = (source_bottom_y * usize::from(image.preview_width) + source_x) * 4;
            if bottom + 2 >= image.preview_rgba.len() || top + 2 >= image.preview_rgba.len() {
                continue;
            }
            let foreground = Color::Rgb(
                image.preview_rgba[top],
                image.preview_rgba[top + 1],
                image.preview_rgba[top + 2],
            );
            let background = Color::Rgb(
                image.preview_rgba[bottom],
                image.preview_rgba[bottom + 1],
                image.preview_rgba[bottom + 2],
            );
            frame.buffer_mut().set_string(
                preview_x + column as u16,
                preview_y + row as u16,
                "▀",
                Style::default().fg(foreground).bg(background),
            );
        }
    }
    let footer_y = area.bottom().saturating_sub(2);
    let remove = "[ Remove ]";
    let close = "[ Close ]";
    let footer_x = area.x + 2;
    *state.image_view_actions.borrow_mut() = vec![
        (0, footer_x, footer_x + remove.len() as u16, footer_y),
        (
            1,
            footer_x + remove.len() as u16 + 1,
            footer_x + remove.len() as u16 + close.len() as u16 + 1,
            footer_y,
        ),
    ];
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                remove,
                Style::default()
                    .fg(if state.image_view_action_hover == Some(0) {
                        theme.error
                    } else {
                        theme.muted
                    })
                    .bg(theme.background),
            ),
            Span::raw(" "),
            Span::styled(
                close,
                Style::default()
                    .fg(if state.image_view_action_hover == Some(1) {
                        theme.accent
                    } else {
                        theme.muted
                    })
                    .bg(theme.background),
            ),
            Span::styled("  Esc close", Style::default().fg(theme.muted)),
        ]))
        .style(Style::default().bg(theme.background)),
        Rect::new(footer_x, footer_y, area.width.saturating_sub(4), 1),
    );
}

fn image_viewer_geometry(image: &ImageAttachment, frame_area: Rect) -> (Rect, u16, u16) {
    let max_width = frame_area.width.saturating_sub(6).max(1);
    let max_height = frame_area.height.saturating_sub(9).saturating_mul(2).max(1);
    let (display_width, display_height) =
        if image.preview_width <= max_width && image.preview_height <= max_height {
            (image.preview_width, image.preview_height)
        } else if u32::from(image.preview_width) * u32::from(max_height)
            > u32::from(image.preview_height) * u32::from(max_width)
        {
            (
                max_width,
                ((u32::from(image.preview_height) * u32::from(max_width)
                    / u32::from(image.preview_width)) as u16)
                    .max(1),
            )
        } else {
            (
                ((u32::from(image.preview_width) * u32::from(max_height)
                    / u32::from(image.preview_height)) as u16)
                    .max(1),
                max_height,
            )
        };
    let preview_rows = usize::from(display_height).div_ceil(2);
    let width = (display_width + 4)
        .max(44)
        .min(frame_area.width.saturating_sub(2).max(1));
    let height = (preview_rows as u16 + 7).min(frame_area.height.saturating_sub(2).max(1));
    (
        centered(frame_area, width, height),
        display_width,
        display_height,
    )
}

fn render_paste_editor(frame: &mut Frame<'_>, state: &AppState) {
    let theme = state.theme();
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));
    let area = paste_editor_geometry(frame_area);
    frame.render_widget(Clear, area);
    frame.render_widget(
        picker::modal_block("Edit pasted text", state.overlay_close_hovered, theme),
        area,
    );

    let content_width = usize::from(area.width.saturating_sub(8)).max(1);
    let rows = paste_editor_rows(&state.overlay_query, content_width);
    *state.paste_editor_rows.borrow_mut() = rows
        .iter()
        .map(|row| {
            row.offsets
                .iter()
                .copied()
                .zip(row.columns.iter().copied())
                .collect()
        })
        .collect();
    let viewport_height = usize::from(area.height.saturating_sub(5)).max(1);
    let cursor_row = rows
        .iter()
        .rposition(|row| {
            row.offsets
                .first()
                .is_some_and(|start| *start <= state.overlay_cursor)
                && row
                    .offsets
                    .last()
                    .is_some_and(|end| state.overlay_cursor <= *end)
        })
        .unwrap_or(0);
    let max_scroll = rows.len().saturating_sub(viewport_height);
    let mut scroll = state.paste_editor_scroll.get().min(max_scroll);
    if state.paste_editor_follow_cursor.get() {
        if cursor_row < scroll {
            scroll = cursor_row;
        } else if cursor_row >= scroll + viewport_height {
            scroll = cursor_row + 1 - viewport_height;
        }
    }
    state.paste_editor_scroll.set(scroll);

    frame.render_widget(
        Paragraph::new(format!(
            "{} lines · {} characters",
            state.overlay_query.split('\n').count(),
            state.overlay_query.chars().count()
        ))
        .style(Style::default().fg(theme.muted).bg(theme.background)),
        Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(4), 1),
    );

    let content_x = area.x + 2;
    let content_y = area.y + 2;
    let mut targets = Vec::new();
    let mut cursor_position = None;
    let lines = rows
        .iter()
        .skip(scroll)
        .take(viewport_height)
        .enumerate()
        .map(|(screen_row, row)| {
            let y = content_y + screen_row as u16;
            let gutter = row
                .line_number
                .map_or_else(|| "     ".to_string(), |line| format!("{line:>4} "));
            for (index, offset) in row.offsets.iter().enumerate() {
                let x = content_x + 5 + row.columns[index] as u16;
                targets.push((x, y, *offset));
                if *offset == state.overlay_cursor {
                    cursor_position = Some((x, y));
                }
            }
            Line::from(vec![
                Span::styled(
                    gutter,
                    Style::default().fg(theme.subtle).bg(theme.background),
                ),
                Span::styled(
                    &row.text,
                    Style::default().fg(theme.foreground).bg(theme.background),
                ),
            ])
        })
        .collect::<Vec<_>>();
    *state.paste_editor_targets.borrow_mut() = targets;
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme.foreground).bg(theme.background)),
        Rect::new(
            content_x,
            content_y,
            area.width.saturating_sub(4),
            viewport_height as u16,
        ),
    );
    picker::render_scrollbar_for(
        frame,
        Rect::new(
            content_x,
            content_y,
            area.width.saturating_sub(4),
            viewport_height as u16,
        ),
        scroll,
        viewport_height.min(rows.len()),
        rows.len(),
        theme,
    );
    let footer_y = area.bottom().saturating_sub(2);
    let footer_x = area.x + 2;
    let save = "[ Save ]";
    let cancel = "[ Cancel ]";
    *state.paste_editor_actions.borrow_mut() = vec![
        (0, footer_x, footer_x + save.len() as u16, footer_y),
        (
            1,
            footer_x + save.len() as u16 + 1,
            footer_x + save.len() as u16 + 1 + cancel.len() as u16,
            footer_y,
        ),
    ];
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                save,
                Style::default()
                    .fg(if state.paste_editor_action_hover == Some(0) {
                        theme.accent
                    } else {
                        theme.muted
                    })
                    .bg(theme.background),
            ),
            Span::raw(" "),
            Span::styled(
                cancel,
                Style::default()
                    .fg(if state.paste_editor_action_hover == Some(1) {
                        theme.error
                    } else {
                        theme.muted
                    })
                    .bg(theme.background),
            ),
            Span::styled(
                "  Enter newline · Ctrl+Enter/Ctrl+S save · Esc cancel",
                Style::default().fg(theme.muted).bg(theme.background),
            ),
        ]))
        .style(Style::default().bg(theme.background)),
        Rect::new(footer_x, footer_y, area.width.saturating_sub(4), 1),
    );
    if let Some(position) = cursor_position {
        frame.set_cursor_position(position);
    }
}

struct PasteEditorRow {
    text: String,
    offsets: Vec<usize>,
    columns: Vec<usize>,
    line_number: Option<usize>,
}

fn paste_editor_rows(text: &str, width: usize) -> Vec<PasteEditorRow> {
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut offsets = vec![0];
    let mut columns = vec![0];
    let mut visual_width = 0;
    let mut line_number = 1;
    let mut first_visual_row = true;
    let chars = text.chars().collect::<Vec<_>>();
    for (offset, character) in chars.iter().copied().enumerate() {
        if character == '\n' {
            rows.push(PasteEditorRow {
                text: std::mem::take(&mut current),
                offsets: std::mem::replace(&mut offsets, vec![offset + 1]),
                columns: std::mem::replace(&mut columns, vec![0]),
                line_number: first_visual_row.then_some(line_number),
            });
            visual_width = 0;
            line_number += 1;
            first_visual_row = true;
            continue;
        }
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if visual_width > 0 && visual_width + character_width > width {
            rows.push(PasteEditorRow {
                text: std::mem::take(&mut current),
                offsets: std::mem::replace(&mut offsets, vec![offset]),
                columns: std::mem::replace(&mut columns, vec![0]),
                line_number: first_visual_row.then_some(line_number),
            });
            visual_width = 0;
            first_visual_row = false;
        }
        current.push(character);
        visual_width += character_width;
        offsets.push(offset + 1);
        columns.push(visual_width);
    }
    rows.push(PasteEditorRow {
        text: current,
        offsets,
        columns,
        line_number: first_visual_row.then_some(line_number),
    });
    rows
}

fn render_tree_picker(frame: &mut Frame<'_>, state: &AppState) {
    let theme = state.theme();
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));

    let entries = state.filtered_tree();
    let (area, start, end) =
        tree_picker_geometry(frame_area, state.overlay_selected, entries.len());
    frame.render_widget(Clear, area);

    let mut lines = vec![
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
                state.overlay_hovered == Some(index),
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

    let block = picker::modal_block("Session Tree", state.overlay_close_hovered, theme);
    frame.render_widget(Paragraph::new(lines).block(block), area);
    picker::render_scrollbar_for(
        frame,
        Rect::new(
            area.x + 1,
            area.y + 4,
            area.width.saturating_sub(2),
            end.saturating_sub(start) as u16,
        ),
        start,
        end.saturating_sub(start),
        entries.len(),
        theme,
    );
}

fn render_fork_picker(frame: &mut Frame<'_>, state: &AppState) {
    let theme = state.theme();
    let frame_area = frame.area();
    frame
        .buffer_mut()
        .set_style(frame_area, Style::default().add_modifier(Modifier::DIM));
    let entries = state.filtered_tree();
    let (area, start, end) =
        fork_picker_geometry(frame_area, state.overlay_selected, entries.len());
    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(Span::styled(
            " Select a user message to copy the active path up to that point into a new session",
            Style::default().fg(theme.muted),
        )),
        Line::raw(""),
    ];
    for (index, entry) in entries.iter().enumerate().take(end).skip(start) {
        let selected = index == state.overlay_selected;
        let hovered = state.overlay_hovered == Some(index);
        let style = Style::default()
            .fg(theme.foreground)
            .bg(if selected || hovered {
                theme.bg_highlight
            } else {
                theme.background
            })
            .add_modifier(if selected || hovered {
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
    let block = picker::modal_block("Fork from Message", state.overlay_close_hovered, theme);
    frame.render_widget(Paragraph::new(lines).block(block), area);
    picker::render_scrollbar_for(
        frame,
        Rect::new(
            area.x + 1,
            area.y + 3,
            area.width.saturating_sub(2),
            end.saturating_sub(start).saturating_mul(3) as u16,
        ),
        start,
        end.saturating_sub(start),
        entries.len(),
        theme,
    );
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
    hovered: bool,
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
        ("message", Some("user")) => ("user: ".to_string(), theme.accent_user),
        ("message", Some("assistant")) => ("assistant: ".to_string(), theme.success),
        ("message", Some("toolResult")) => (String::new(), theme.muted),
        ("compaction", _) => ("[compaction]: ".to_string(), theme.accent),
        ("branch_summary", _) => ("[branch summary]: ".to_string(), theme.warning),
        (_, Some(role)) => (format!("{role}: "), theme.muted),
        (kind, None) => (format!("[{kind}]: "), theme.muted),
    };
    let selected_style = if selected || hovered {
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
    if selected || hovered {
        line = line.style(Style::default().bg(theme.code_background));
    }
    line
}

fn truncate_overlay_text(value: &str, width: usize) -> String {
    let mut chars = value.chars();
    let mut result: String = chars.by_ref().take(width).collect();
    if chars.next().is_some() && width > 1 {
        result.pop();
        result.push('…');
    }
    result
}

fn short_timestamp(timestamp: &str) -> String {
    timestamp.get(5..16).unwrap_or(timestamp).replace('T', " ")
}

fn slash_suggestion_area(state: &AppState, frame_area: Rect) -> Option<(Rect, usize, usize)> {
    let matches = state.slash_suggestions();
    if matches.is_empty() {
        return None;
    }
    let agent = AgentLayout::compute(frame_area, state);
    let width = agent.prompt.width;
    let available_above = agent.prompt.y.saturating_sub(frame_area.y);
    let capacity = usize::from(available_above.saturating_sub(2)).clamp(1, 10);
    let start = centered_window(state.overlay_selected, matches.len(), capacity);
    let end = (start + capacity).min(matches.len());
    let height = (end.saturating_sub(start) + 2) as u16;
    Some((
        Rect::new(
            agent.prompt.x,
            agent.prompt.y.saturating_sub(height),
            width,
            height,
        ),
        start,
        end,
    ))
}

pub fn slash_item_at(
    state: &AppState,
    width: u16,
    height: u16,
    column: u16,
    row: u16,
) -> Option<usize> {
    let (area, start, end) = slash_suggestion_area(state, Rect::new(0, 0, width, height))?;
    if column < area.x || column >= area.right() {
        return None;
    }
    let item_row = row.checked_sub(area.y + 1)? as usize;
    if row < area.y + 1 || row >= area.bottom().saturating_sub(1) {
        return None;
    }
    (start + item_row < end).then_some(start + item_row)
}

#[cfg(test)]
pub fn item_at(state: &AppState, width: u16, height: u16, row: u16) -> Option<usize> {
    item_at_position(state, width, height, width / 2, row)
}

pub fn item_at_position(
    state: &AppState,
    width: u16,
    height: u16,
    column: u16,
    row: u16,
) -> Option<usize> {
    let frame_area = Rect::new(0, 0, width, height);
    let entries = state.filtered_tree();
    match state.overlay {
        OverlayKind::TreePicker => {
            let (area, start, end) =
                tree_picker_geometry(frame_area, state.overlay_selected, entries.len());
            if !area.contains((column, row).into()) {
                return None;
            }
            let first_row = area.y.saturating_add(4);
            let offset = usize::from(row.saturating_sub(first_row));
            (row >= first_row && offset < end.saturating_sub(start)).then_some(start + offset)
        }
        OverlayKind::ForkPicker => {
            let (area, start, end) =
                fork_picker_geometry(frame_area, state.overlay_selected, entries.len());
            if !area.contains((column, row).into()) {
                return None;
            }
            let first_row = area.y.saturating_add(3);
            let offset = usize::from(row.saturating_sub(first_row));
            let item = offset / 3;
            (row >= first_row && item < end.saturating_sub(start)).then_some(start + item)
        }
        _ => {
            let data = generic_picker_data(state);
            let display_query = if state.overlay == OverlayKind::OauthPrompt {
                "•".repeat(state.overlay_query.chars().count())
            } else {
                state.overlay_query.clone()
            };
            let spec = PickerSpec {
                title: data.title,
                query: data
                    .query_label
                    .map(|label| (label, display_query.as_str())),
                notes: &data.notes,
                rows: &data.rows,
                footer: data.footer,
                max_width: data.max_width,
                max_height: data.max_height,
            };
            let layout = picker::layout(frame_area, &spec, state.overlay_selected);
            picker::item_at(layout, &data.rows, column, row)
        }
    }
}

pub fn close_at_position(state: &AppState, width: u16, height: u16, column: u16, row: u16) -> bool {
    let frame_area = Rect::new(0, 0, width, height);
    let modal = match state.overlay {
        OverlayKind::TreePicker => {
            let count = state.filtered_tree().len();
            tree_picker_geometry(frame_area, state.overlay_selected, count).0
        }
        OverlayKind::ForkPicker => {
            let count = state.filtered_tree().len();
            fork_picker_geometry(frame_area, state.overlay_selected, count).0
        }
        OverlayKind::PasteEditor => paste_editor_geometry(frame_area),
        OverlayKind::ImageViewer => {
            let Some(image) = state.viewed_image() else {
                return false;
            };
            image_viewer_geometry(image, frame_area).0
        }
        OverlayKind::None => return false,
        _ => {
            let data = generic_picker_data(state);
            let display_query = if state.overlay == OverlayKind::OauthPrompt {
                "•".repeat(state.overlay_query.chars().count())
            } else {
                state.overlay_query.clone()
            };
            let spec = PickerSpec {
                title: data.title,
                query: data
                    .query_label
                    .map(|label| (label, display_query.as_str())),
                notes: &data.notes,
                rows: &data.rows,
                footer: data.footer,
                max_width: data.max_width,
                max_height: data.max_height,
            };
            picker::layout(frame_area, &spec, state.overlay_selected).modal
        }
    };
    row == modal.y
        && column >= modal.right().saturating_sub(6)
        && column < modal.right().saturating_sub(1)
}

fn render_slash_suggestions(frame: &mut Frame<'_>, state: &AppState) {
    let Some((area, start, end)) = slash_suggestion_area(state, frame.area()) else {
        return;
    };
    let matches = state.slash_suggestions();
    let theme = state.theme();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.bg_light)),
        area,
    );
    let count_label = matches.len().to_string();
    let divider_fill = usize::from(area.width).saturating_sub(count_label.width() + 1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("─".repeat(divider_fill), Style::default().fg(theme.subtle)),
            Span::styled(format!(" {count_label}"), Style::default().fg(theme.muted)),
        ])),
        Rect::new(area.x, area.y, area.width, 1),
    );
    let visible = &matches[start..end];
    let label_width = visible
        .iter()
        .map(|(command, _)| command.width())
        .max()
        .unwrap_or(0)
        .min(usize::from(area.width) * 3 / 5)
        .min(40);
    let mut lines = Vec::with_capacity(end.saturating_sub(start));
    for (relative, (command, description)) in visible.iter().enumerate() {
        let index = start + relative;
        let selected = index == state.overlay_selected;
        let hovered = state.overlay_hovered == Some(index);
        let marker = if selected { "❯ " } else { "  " };
        let bg = if selected || hovered {
            theme.bg_highlight
        } else {
            theme.bg_light
        };
        let command_display = truncate_overlay_text(command, label_width);
        let padding = label_width.saturating_sub(command_display.width());
        let desc_width = usize::from(area.width).saturating_sub(label_width + 5);
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{command_display}{}  ", " ".repeat(padding)),
                Style::default()
                    .fg(theme.foreground)
                    .bg(bg)
                    .add_modifier(if selected || hovered {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                truncate_overlay_text(description, desc_width),
                Style::default().fg(theme.muted).bg(bg),
            ),
        ]));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        ),
    );
    picker::render_scrollbar_for(
        frame,
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(2),
        ),
        start,
        end.saturating_sub(start),
        matches.len(),
        theme,
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(usize::from(area.width)),
            Style::default().fg(theme.subtle),
        )),
        Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
    );
}

fn overlay_has_query(overlay: OverlayKind) -> bool {
    matches!(
        overlay,
        OverlayKind::CommandPalette
            | OverlayKind::ModelPicker
            | OverlayKind::SessionPicker
            | OverlayKind::SessionRename
            | OverlayKind::TreeSummaryEditor
            | OverlayKind::LabelEditor
            | OverlayKind::FilePicker
            | OverlayKind::ScopedModels
            | OverlayKind::SubagentModelPicker
            | OverlayKind::OauthPrompt
            | OverlayKind::OauthSelect
            | OverlayKind::LoginProvider
            | OverlayKind::RewindPicker
    )
}

fn tree_picker_geometry(area: Rect, selected: usize, count: usize) -> (Rect, usize, usize) {
    let capacity = usize::from(area.height.saturating_sub(10)).max(1);
    let start = centered_window(selected, count, capacity);
    let end = (start + capacity).min(count);
    let height = (end.saturating_sub(start) as u16 + 7).min(area.height.saturating_sub(1).max(1));
    (
        centered(area, area.width.saturating_sub(2).max(1), height),
        start,
        end,
    )
}

fn paste_editor_geometry(area: Rect) -> Rect {
    centered(
        area,
        area.width.saturating_sub(2).clamp(1, 120),
        area.height.saturating_sub(2).clamp(1, 40),
    )
}

fn fork_picker_geometry(area: Rect, selected: usize, count: usize) -> (Rect, usize, usize) {
    let capacity = 10usize
        .min(usize::from(area.height.saturating_sub(10)) / 3)
        .max(1);
    let start = centered_window(selected, count, capacity);
    let end = (start + capacity).min(count);
    let height =
        (end.saturating_sub(start) as u16 * 3 + 6).min(area.height.saturating_sub(1).max(1));
    (
        centered(area, area.width.saturating_sub(2).max(1), height),
        start,
        end,
    )
}

fn centered(parent: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        parent.x + parent.width.saturating_sub(width) / 2,
        parent.y + parent.height.saturating_sub(height) / 2,
        width.min(parent.width),
        height.min(parent.height),
    )
}
