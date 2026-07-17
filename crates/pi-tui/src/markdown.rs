use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;
#[cfg(test)]
use crate::theme::ThemeMode;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CodeBlockSpan {
    pub info: String,
    pub body: String,
    pub output_line_range: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct MarkdownRenderOutput {
    pub lines: Vec<Line<'static>>,
    pub code_blocks: Vec<CodeBlockSpan>,
}

pub fn render(source: &[String], width: usize, theme: Theme) -> Vec<Line<'static>> {
    render_output(source, width, theme).lines
}

pub fn render_output(source: &[String], width: usize, theme: Theme) -> MarkdownRenderOutput {
    let source = source.join("\n");
    Renderer::new(width, theme).render(&source)
}

pub fn code_blocks(source: &[String]) -> Vec<CodeBlockSpan> {
    let source = source.join("\n");
    let mut current: Option<(String, String)> = None;
    let mut blocks = Vec::new();
    for event in Parser::new_ext(
        &source,
        Options::ENABLE_GFM | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                current = Some((info, String::new()));
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                current.as_mut().expect("code state").1.push_str(&text);
            }
            Event::SoftBreak | Event::HardBreak if current.is_some() => {
                current.as_mut().expect("code state").1.push('\n');
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((info, body)) = current.take() {
                    blocks.push(CodeBlockSpan {
                        info,
                        body: body.trim_end_matches('\n').to_string(),
                        output_line_range: 0..0,
                    });
                }
            }
            _ => {}
        }
    }
    blocks
}

struct CodeState {
    info: String,
    body: String,
}

#[derive(Default)]
struct TableState {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
}

struct Renderer {
    width: usize,
    theme: Theme,
    output: MarkdownRenderOutput,
    current: Vec<Span<'static>>,
    style: Style,
    style_stack: Vec<Style>,
    list_stack: Vec<Option<u64>>,
    pending_prefix: Option<String>,
    blockquote_depth: usize,
    link_stack: Vec<String>,
    code: Option<CodeState>,
    table: Option<TableState>,
}

impl Renderer {
    fn new(width: usize, theme: Theme) -> Self {
        Self {
            width: width.max(1),
            theme,
            output: MarkdownRenderOutput::default(),
            current: Vec::new(),
            style: Style::default().fg(theme.foreground),
            style_stack: Vec::new(),
            list_stack: Vec::new(),
            pending_prefix: None,
            blockquote_depth: 0,
            link_stack: Vec::new(),
            code: None,
            table: None,
        }
    }

    fn render(mut self, source: &str) -> MarkdownRenderOutput {
        let options = Options::ENABLE_GFM
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_TABLES
            | Options::ENABLE_MATH;
        for event in Parser::new_ext(source, options) {
            self.event(event);
        }
        self.flush();
        while self
            .output
            .lines
            .last()
            .is_some_and(|line| line.spans.is_empty())
        {
            self.output.lines.pop();
        }
        self.output
    }

    fn event(&mut self, event: Event<'_>) {
        if self.code.is_some() {
            match event {
                Event::End(TagEnd::CodeBlock) => self.finish_code(),
                Event::Text(text) | Event::Code(text) => {
                    self.code.as_mut().expect("code state").body.push_str(&text)
                }
                Event::SoftBreak | Event::HardBreak => {
                    self.code.as_mut().expect("code state").body.push('\n')
                }
                _ => {}
            }
            return;
        }
        if self.table.is_some() {
            self.table_event(event);
            return;
        }

        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => self.push_styled(
                text.into_string(),
                self.style
                    .fg(self.theme.code_foreground)
                    .bg(self.theme.code_background),
            ),
            Event::InlineMath(text) => {
                self.push_styled(format!("${text}$"), self.style.fg(self.theme.accent))
            }
            Event::DisplayMath(text) => {
                self.flush();
                for line in wrap(&format!("  {text}"), self.width) {
                    self.output.lines.push(Line::from(Span::styled(
                        line,
                        Style::default().fg(self.theme.accent),
                    )));
                }
                self.blank();
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.output.lines.push(Line::from(Span::styled(
                    "─".repeat(self.width),
                    Style::default().fg(self.theme.subtle),
                )));
            }
            Event::TaskListMarker(checked) => {
                self.pending_prefix = Some(if checked { "☑ " } else { "☐ " }.into())
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html),
            Event::FootnoteReference(label) => {
                self.push_styled(format!("[^{label}]"), self.style.fg(self.theme.accent))
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {}
            Tag::Heading { .. } => {
                self.flush();
                self.push_style(self.style.add_modifier(Modifier::BOLD));
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.blockquote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some(CodeState {
                    info,
                    body: String::new(),
                });
            }
            Tag::List(start) => self.list_stack.push(start),
            Tag::Item => {
                let marker = match self.list_stack.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}. ");
                        *number += 1;
                        marker
                    }
                    _ => "• ".into(),
                };
                self.pending_prefix = Some(format!(
                    "{}{}",
                    "  ".repeat(self.list_stack.len().saturating_sub(1)),
                    marker
                ));
            }
            Tag::Emphasis => self.push_style(self.style.add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.push_style(self.style.add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.push_style(self.style.add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.link_stack.push(dest_url.into_string());
                self.push_style(
                    self.style
                        .fg(self.theme.accent)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Tag::Image { dest_url, .. } => {
                self.link_stack.push(dest_url.into_string());
                self.push_text("[image: ");
            }
            Tag::Table(alignments) => {
                self.flush();
                self.table = Some(TableState {
                    alignments,
                    ..TableState::default()
                });
            }
            Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript
            | Tag::MetadataBlock(_) => {}
            Tag::TableHead | Tag::TableRow | Tag::TableCell => unreachable!("handled in table"),
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.blank();
            }
            TagEnd::Heading(_) => {
                self.pop_style();
                self.flush();
                self.blank();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.blank();
            }
            TagEnd::List(_) => {
                self.flush();
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.blank();
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => self.pop_style(),
            TagEnd::Link => {
                self.pop_style();
                if let Some(url) = self.link_stack.pop() {
                    self.push_styled(format!(" ({url})"), Style::default().fg(self.theme.muted));
                }
            }
            TagEnd::Image => {
                if let Some(url) = self.link_stack.pop() {
                    self.push_text(&format!("] ({url})"));
                }
            }
            TagEnd::CodeBlock | TagEnd::Table => unreachable!("handled by specialized state"),
            TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::MetadataBlock(_) => {}
            TagEnd::TableHead | TagEnd::TableRow | TagEnd::TableCell => {}
        }
    }

    fn table_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow) => {
                self.table.as_mut().expect("table state").row.clear()
            }
            Event::Start(Tag::TableCell) => self.table.as_mut().expect("table state").cell.clear(),
            Event::Text(text)
            | Event::Code(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => self
                .table
                .as_mut()
                .expect("table state")
                .cell
                .push_str(&text),
            Event::SoftBreak | Event::HardBreak => {
                self.table.as_mut().expect("table state").cell.push(' ')
            }
            Event::TaskListMarker(checked) => self
                .table
                .as_mut()
                .expect("table state")
                .cell
                .push_str(if checked { "☑ " } else { "☐ " }),
            Event::End(TagEnd::TableCell) => {
                let table = self.table.as_mut().expect("table state");
                table.row.push(std::mem::take(&mut table.cell));
            }
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                let table = self.table.as_mut().expect("table state");
                if !table.row.is_empty() {
                    table.rows.push(std::mem::take(&mut table.row));
                }
            }
            Event::End(TagEnd::Table) => self.finish_table(),
            Event::Html(html) | Event::InlineHtml(html) => self
                .table
                .as_mut()
                .expect("table state")
                .cell
                .push_str(&html),
            _ => {}
        }
    }

    fn finish_code(&mut self) {
        let code = self.code.take().expect("code state");
        let label = if code.info.trim().is_empty() {
            "code"
        } else {
            code.info.trim()
        };
        self.output.lines.push(code_line(
            format!(" {label}"),
            self.width,
            Style::default()
                .fg(self.theme.muted)
                .bg(self.theme.code_background),
        ));
        let start = self.output.lines.len();
        for source_line in code.body.trim_end_matches('\n').split('\n') {
            for line in wrap_code(source_line, self.width.saturating_sub(2)) {
                self.output.lines.push(code_line(
                    format!("  {line}"),
                    self.width,
                    Style::default()
                        .fg(self.theme.code_foreground)
                        .bg(self.theme.code_background),
                ));
            }
        }
        let end = self.output.lines.len();
        self.output.code_blocks.push(CodeBlockSpan {
            info: code.info,
            body: code.body.trim_end_matches('\n').to_string(),
            output_line_range: start..end,
        });
        self.blank();
    }

    fn finish_table(&mut self) {
        let table = self.table.take().expect("table state");
        if table.rows.is_empty() {
            return;
        }
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        if self.width < columns.saturating_mul(4).saturating_add(1) {
            self.render_stacked_table(&table, columns);
            self.blank();
            return;
        }
        let border_width = columns.saturating_mul(3).saturating_add(1);
        let available = self.width.saturating_sub(border_width).max(columns);
        let max_cell = (available / columns).max(3);
        let mut widths = vec![1; columns];
        for row in &table.rows {
            for (column, cell) in row.iter().enumerate() {
                widths[column] = widths[column]
                    .max(cell.lines().map(UnicodeWidthStr::width).max().unwrap_or(0))
                    .min(max_cell);
            }
        }
        while widths.iter().sum::<usize>() + border_width > self.width {
            let Some((column, _)) = widths.iter().enumerate().max_by_key(|(_, width)| *width)
            else {
                break;
            };
            if widths[column] <= 1 {
                break;
            }
            widths[column] -= 1;
        }

        self.table_border('┌', '┬', '┐', &widths);
        for (row_index, row) in table.rows.iter().enumerate() {
            let wrapped = (0..columns)
                .map(|column| {
                    wrap(
                        row.get(column).map(String::as_str).unwrap_or(""),
                        widths[column],
                    )
                })
                .collect::<Vec<_>>();
            let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
            for line_index in 0..height {
                let mut text = String::from("│");
                for column in 0..columns {
                    let cell = wrapped[column]
                        .get(line_index)
                        .map(String::as_str)
                        .unwrap_or("");
                    let padding = widths[column].saturating_sub(UnicodeWidthStr::width(cell));
                    let (left, right) = match table.alignments.get(column) {
                        Some(Alignment::Right) => (padding, 0),
                        Some(Alignment::Center) => (padding / 2, padding - padding / 2),
                        _ => (0, padding),
                    };
                    text.push(' ');
                    text.push_str(&" ".repeat(left));
                    text.push_str(cell);
                    text.push_str(&" ".repeat(right));
                    text.push(' ');
                    text.push('│');
                }
                self.output.lines.push(Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(if row_index == 0 {
                            self.theme.foreground
                        } else {
                            self.theme.muted
                        })
                        .add_modifier(if row_index == 0 {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                )));
            }
            if row_index == 0 && table.rows.len() > 1 {
                self.table_border('├', '┼', '┤', &widths);
            }
        }
        self.table_border('└', '┴', '┘', &widths);
        self.blank();
    }

    fn render_stacked_table(&mut self, table: &TableState, columns: usize) {
        let headers = &table.rows[0];
        for (row_index, row) in table.rows.iter().enumerate().skip(1) {
            if row_index > 1 {
                self.output.lines.push(Line::raw(""));
            }
            for column in 0..columns {
                let header = headers.get(column).map(String::as_str).unwrap_or("value");
                let value = row.get(column).map(String::as_str).unwrap_or("");
                for line in wrap(&format!("{header}: {value}"), self.width) {
                    self.output.lines.push(Line::from(Span::styled(
                        line,
                        Style::default().fg(self.theme.foreground),
                    )));
                }
            }
        }
        if table.rows.len() == 1 {
            for header in headers {
                for line in wrap(header, self.width) {
                    self.output.lines.push(Line::from(Span::styled(
                        line,
                        Style::default()
                            .fg(self.theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
            }
        }
    }

    fn table_border(&mut self, left: char, middle: char, right: char, widths: &[usize]) {
        let mut text = String::new();
        text.push(left);
        for (index, width) in widths.iter().enumerate() {
            text.push_str(&"─".repeat(width.saturating_add(2)));
            text.push(if index + 1 == widths.len() {
                right
            } else {
                middle
            });
        }
        self.output.lines.push(Line::from(Span::styled(
            text,
            Style::default().fg(self.theme.subtle),
        )));
    }

    fn push_style(&mut self, style: Style) {
        self.style_stack.push(self.style);
        self.style = style;
    }

    fn pop_style(&mut self) {
        self.style = self
            .style_stack
            .pop()
            .unwrap_or_else(|| Style::default().fg(self.theme.foreground));
    }

    fn push_text(&mut self, text: &str) {
        self.push_styled(text.to_string(), self.style);
    }

    fn push_styled(&mut self, text: String, style: Style) {
        if !text.is_empty() {
            self.current.push(Span::styled(text, style));
        }
    }

    fn flush(&mut self) {
        if self.current.is_empty() && self.pending_prefix.is_none() {
            return;
        }
        let mut plain = String::new();
        for span in &self.current {
            plain.push_str(span.content.as_ref());
        }
        let prefix = self.pending_prefix.take().unwrap_or_default();
        let quote = if self.blockquote_depth == 0 {
            String::new()
        } else {
            "│ ".repeat(self.blockquote_depth)
        };
        let prefix_width =
            UnicodeWidthStr::width(prefix.as_str()) + UnicodeWidthStr::width(quote.as_str());
        let content_width = self.width.saturating_sub(prefix_width).max(1);
        let wrapped = wrap(&plain, content_width);
        for (index, text) in wrapped.into_iter().enumerate() {
            let mut spans = Vec::new();
            if !quote.is_empty() {
                spans.push(Span::styled(
                    quote.clone(),
                    Style::default().fg(self.theme.subtle),
                ));
            }
            if !prefix.is_empty() {
                spans.push(Span::styled(
                    if index == 0 {
                        prefix.clone()
                    } else {
                        " ".repeat(
                            prefix_width.saturating_sub(UnicodeWidthStr::width(quote.as_str())),
                        )
                    },
                    Style::default().fg(self.theme.accent),
                ));
            }
            // Preserve inline styling when no wrapping occurred. Wrapped prose keeps
            // semantic structure and readable text without pretending cell offsets
            // still match the original span boundaries.
            if index == 0 && text == plain {
                spans.append(&mut self.current);
            } else {
                spans.push(Span::styled(text, self.style));
            }
            self.output.lines.push(Line::from(spans));
        }
        self.current.clear();
    }

    fn blank(&mut self) {
        if self
            .output
            .lines
            .last()
            .is_some_and(|line| !line.spans.is_empty())
        {
            self.output.lines.push(Line::raw(""));
        }
    }
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

pub fn wrap(value: &str, width: usize) -> Vec<String> {
    if value.is_empty() || width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        if UnicodeWidthStr::width(word) > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for character in word.chars() {
                if UnicodeWidthStr::width(chunk.as_str()) + character.width().unwrap_or(0) > width
                    && !chunk.is_empty()
                {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(character);
            }
            current = chunk;
            continue;
        }
        let separator = usize::from(!current.is_empty());
        if UnicodeWidthStr::width(current.as_str()) + separator + UnicodeWidthStr::width(word)
            > width
            && !current.is_empty()
        {
            lines.push(std::mem::take(&mut current));
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

fn code_line(value: String, width: usize, style: Style) -> Line<'static> {
    let count = UnicodeWidthStr::width(value.as_str());
    Line::from(Span::styled(
        value + &" ".repeat(width.saturating_sub(count)),
        style,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_gfm_table_with_borders() {
        let source = vec![
            "| Name | State |".into(),
            "| --- | ---: |".into(),
            "| worker | running |".into(),
        ];
        let rendered = render_output(&source, 60, Theme::for_mode(ThemeMode::Dark));
        let output = text(&rendered.lines);
        assert!(output.contains("┌"));
        assert!(output.contains("worker"));
        assert!(!output.contains("| ---"));
    }

    #[test]
    fn narrow_tables_fall_back_without_exceeding_the_viewport() {
        let source = vec![
            "| A | B | C |".into(),
            "| - | - | - |".into(),
            "| 1 | 2 | 3 |".into(),
        ];
        let rendered = render_output(&source, 5, Theme::for_mode(ThemeMode::Dark));
        assert!(rendered.lines.iter().all(|line| {
            UnicodeWidthStr::width(
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .as_str(),
            ) <= 5
        }));
    }

    #[test]
    fn preserves_code_block_body_for_copy() {
        let source = vec!["```rust".into(), "fn main() {}".into(), "```".into()];
        let rendered = render_output(&source, 60, Theme::for_mode(ThemeMode::Dark));
        assert_eq!(rendered.code_blocks.len(), 1);
        assert_eq!(rendered.code_blocks[0].info, "rust");
        assert_eq!(rendered.code_blocks[0].body, "fn main() {}");
    }

    #[test]
    fn renders_quotes_links_strike_and_rules_semantically() {
        let source = vec![
            "> [docs](https://example.com) and ~~old~~".into(),
            "".into(),
            "---".into(),
        ];
        let output = text(&render(&source, 60, Theme::for_mode(ThemeMode::Dark)));
        assert!(output.contains("│ "));
        assert!(output.contains("https://example.com"));
        assert!(output.contains("─"));
    }
}
