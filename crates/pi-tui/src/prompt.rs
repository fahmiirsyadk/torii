//! Prompt measurement and wrapping independent from terminal painting.

use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptLayout {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_column: usize,
}

pub fn layout(text: &str, cursor: usize, width: usize) -> PromptLayout {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut row = 0usize;
    let mut column = 0usize;
    let mut cursor_row = 0usize;
    let mut cursor_column = 0usize;

    for (index, character) in text.chars().enumerate() {
        if index == cursor {
            cursor_row = row;
            cursor_column = column;
        }
        if character == '\n' {
            lines.push(String::new());
            row += 1;
            column = 0;
            continue;
        }
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0).max(1);
        if column > 0 && column + character_width > width {
            lines.push(String::new());
            row += 1;
            column = 0;
        }
        lines[row].push(character);
        column += character_width;
    }
    if cursor >= text.chars().count() {
        cursor_row = row;
        cursor_column = column;
    }

    PromptLayout {
        lines,
        cursor_row,
        cursor_column,
    }
}

pub fn desired_height(text: &str, cursor: usize, width: usize, max_rows: usize) -> u16 {
    let rows = layout(text, cursor, width)
        .lines
        .len()
        .clamp(1, max_rows.max(1));
    (rows as u16).saturating_add(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_tracks_cursor_across_explicit_lines() {
        let prompt = layout("abcd\nef", 6, 3);
        assert_eq!(prompt.lines, vec!["abc", "d", "ef"]);
        assert_eq!((prompt.cursor_row, prompt.cursor_column), (2, 1));
    }

    #[test]
    fn desired_height_is_bounded() {
        assert_eq!(desired_height("one", 3, 20, 8), 3);
        assert_eq!(desired_height("a\nb\nc\nd", 7, 20, 2), 4);
    }
}
