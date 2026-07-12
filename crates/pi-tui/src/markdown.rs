use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::theme::Theme;

pub fn render(source: &[String], width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut output = Vec::new();
    let mut code_language: Option<String> = None;

    for source_line in source {
        if let Some(language) = source_line.strip_prefix("```") {
            if code_language.is_some() {
                code_language = None;
            } else {
                let language = if language.trim().is_empty() {
                    "code"
                } else {
                    language.trim()
                };
                output.push(code_line(
                    format!(" {language}"),
                    width,
                    Style::default().fg(theme.muted).bg(theme.code_background),
                ));
                code_language = Some(language.to_string());
            }
            continue;
        }

        if code_language.is_some() {
            for line in wrap_code(source_line, width.saturating_sub(2)) {
                output.push(code_line(
                    format!("  {line}"),
                    width,
                    Style::default()
                        .fg(theme.code_foreground)
                        .bg(theme.code_background),
                ));
            }
            continue;
        }

        if source_line.is_empty() {
            output.push(Line::raw(""));
            continue;
        }

        let heading_level = source_line.chars().take_while(|char| *char == '#').count();
        if heading_level > 0 && source_line.chars().nth(heading_level) == Some(' ') {
            let text = source_line[heading_level + 1..].to_string();
            for line in wrap(&text, width) {
                output.push(Line::from(Span::styled(
                    line,
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            continue;
        }

        if let Some(item) = source_line
            .strip_prefix("- [x] ")
            .or_else(|| source_line.strip_prefix("- [X] "))
        {
            let item_width = width.saturating_sub(2);
            for (index, line) in wrap(item, item_width).into_iter().enumerate() {
                let prefix = if index == 0 { "☑ " } else { "  " };
                let mut spans = vec![Span::styled(
                    prefix.to_string(),
                    Style::default().fg(theme.accent),
                )];
                spans.extend(inline_spans(&line, Style::default(), theme));
                output.push(Line::from(spans));
            }
            continue;
        }
        if let Some(item) = source_line.strip_prefix("- [ ] ") {
            let item_width = width.saturating_sub(2);
            for (index, line) in wrap(item, item_width).into_iter().enumerate() {
                let prefix = if index == 0 { "☐ " } else { "  " };
                let mut spans = vec![Span::styled(
                    prefix.to_string(),
                    Style::default().fg(theme.muted),
                )];
                spans.extend(inline_spans(&line, Style::default(), theme));
                output.push(Line::from(spans));
            }
            continue;
        }
        if let Some(item) = source_line
            .strip_prefix("- ")
            .or_else(|| source_line.strip_prefix("* "))
        {
            let item_width = width.saturating_sub(2);
            for (index, line) in wrap(item, item_width).into_iter().enumerate() {
                let prefix = if index == 0 { "• " } else { "  " };
                let mut spans = vec![Span::styled(
                    prefix.to_string(),
                    Style::default().fg(theme.accent),
                )];
                spans.extend(inline_spans(&line, Style::default(), theme));
                output.push(Line::from(spans));
            }
            continue;
        }
        if let Some(numbered) = numbered_list_item(source_line) {
            let (number, item) = numbered;
            let marker = format!("{number}. ");
            let marker_width = marker.chars().count();
            let item_width = width.saturating_sub(marker_width);
            for (index, line) in wrap(item, item_width).into_iter().enumerate() {
                let prefix = if index == 0 {
                    marker.clone()
                } else {
                    " ".repeat(marker_width)
                };
                let mut spans = vec![Span::styled(prefix, Style::default().fg(theme.accent))];
                spans.extend(inline_spans(&line, Style::default(), theme));
                output.push(Line::from(spans));
            }
            continue;
        }

        for line in wrap(source_line, width) {
            output.push(Line::from(inline_spans(&line, Style::default(), theme)));
        }
    }

    output
}

fn wrap_code(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let characters = value.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return vec![String::new()];
    }
    characters
        .chunks(width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn numbered_list_item(source_line: &str) -> Option<(String, &str)> {
    let dot = source_line.find(". ")?;
    let (number, after) = source_line.split_at(dot);
    if number.is_empty() || !number.chars().all(|char| char.is_ascii_digit()) {
        return None;
    }
    Some((number.to_string(), &after[2..]))
}

pub fn wrap(value: &str, width: usize) -> Vec<String> {
    if value.is_empty() || width == 0 {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        let separator = usize::from(!current.is_empty());
        if current.chars().count() + separator + word.chars().count() > width && !current.is_empty()
        {
            lines.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn inline_spans(value: &str, base: Style, theme: Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (bold_index, bold_part) in value.split("**").enumerate() {
        let bold = bold_index % 2 == 1;
        for (code_index, part) in bold_part.split('`').enumerate() {
            if part.is_empty() {
                continue;
            }
            let code = code_index % 2 == 1;
            let mut style = base;
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if code {
                style = style.fg(theme.code_foreground).bg(theme.code_background);
            }
            spans.push(Span::styled(part.to_string(), style));
        }
    }
    spans
}

fn code_line(value: String, width: usize, style: Style) -> Line<'static> {
    let count = value.chars().count();
    Line::from(Span::styled(
        value + &" ".repeat(width.saturating_sub(count)),
        style,
    ))
}
