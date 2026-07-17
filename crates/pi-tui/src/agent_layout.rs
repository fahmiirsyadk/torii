//! Pure responsive layout for the primary agent screen.

use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};

use crate::{prompt, state::AppState};

pub const SHORT_TERMINAL_ROWS: u16 = 24;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AgentLayout {
    pub outer: Rect,
    pub status: Rect,
    pub scrollback: Rect,
    pub queue: Rect,
    pub workflow_status: Rect,
    pub turn_status: Rect,
    pub compaction: Rect,
    pub permission: Rect,
    pub prompt: Rect,
    pub shortcuts: Rect,
    pub compact: bool,
}

impl AgentLayout {
    pub fn compute(area: Rect, state: &AppState) -> Self {
        let compact = area.height <= SHORT_TERMINAL_ROWS;
        let outer = if compact {
            area
        } else {
            area.inner(Margin {
                horizontal: 1,
                vertical: 0,
            })
        };
        let content_width = usize::from(outer.width.saturating_sub(6)).max(1);
        let prompt_height = prompt::desired_height(
            &state.composer_display_text(),
            state.composer_display_cursor(),
            content_width,
            if compact { 4 } else { 8 },
        );
        // Keep this row reserved so the composer does not jump when the first
        // streaming delta starts (or when TurnComplete clears the banner).
        let turn_height = 1;
        let compaction_height = u16::from(state.active_compaction_started_at().is_some());
        let queued = state.queued_steering.len() + state.queued_follow_up.len();
        let queue_height = if state.queue_visible && queued > 0 {
            queued.min(3) as u16 + 1
        } else {
            0
        };
        let workflow_height = u16::from(state.workflow_widget().is_some());
        let permission_height = if state.pending_permission.is_some() {
            if compact { 4 } else { 5 }
        } else {
            0
        };
        let status_gap = u16::from(!compact);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(status_gap),
                Constraint::Min(5),
                Constraint::Length(queue_height),
                Constraint::Length(workflow_height),
                Constraint::Length(turn_height),
                Constraint::Length(compaction_height),
                Constraint::Length(permission_height),
                Constraint::Length(prompt_height),
                Constraint::Length(1),
            ])
            .split(outer);
        Self {
            outer,
            status: chunks[0],
            scrollback: chunks[2],
            queue: chunks[3],
            workflow_status: chunks[4],
            turn_status: chunks[5],
            compaction: chunks[6],
            permission: chunks[7],
            prompt: chunks[8],
            shortcuts: chunks[9],
            compact,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_grows_without_starving_scrollback() {
        let mut state = AppState::default();
        state.prompt = "one\ntwo\nthree".into();
        state.cursor = state.prompt.chars().count();
        let layout = AgentLayout::compute(Rect::new(0, 0, 100, 30), &state);
        assert_eq!(layout.prompt.height, 5);
        assert!(layout.scrollback.height >= 5);
    }

    #[test]
    fn short_terminals_use_compact_chrome() {
        let state = AppState::default();
        let layout = AgentLayout::compute(Rect::new(0, 0, 80, 20), &state);
        assert!(layout.compact);
        assert_eq!(layout.outer, Rect::new(0, 0, 80, 20));
        assert_eq!(layout.shortcuts.height, 1);
    }

    #[test]
    fn streaming_does_not_move_the_prompt() {
        let idle = AppState::default();
        let mut streaming = idle.clone();
        streaming.streaming = true;
        streaming.turn_started_at = Some(std::time::Instant::now());
        let area = Rect::new(0, 0, 100, 30);
        assert_eq!(
            AgentLayout::compute(area, &idle).prompt,
            AgentLayout::compute(area, &streaming).prompt
        );
    }
}
