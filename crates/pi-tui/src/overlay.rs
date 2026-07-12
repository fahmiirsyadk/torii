use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
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

fn centered(parent: Rect, width: u16, height: u16) -> Rect {
    Rect::new(
        parent.x + parent.width.saturating_sub(width) / 2,
        parent.y + parent.height.saturating_sub(height) / 2,
        width.min(parent.width),
        height.min(parent.height),
    )
}
