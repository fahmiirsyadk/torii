//! Typed display behavior for semantic conversation blocks.

use crate::state::{Entry, ToolStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayMode {
    Collapsed,
    Expanded,
}

impl Entry {
    pub fn display_mode(&self) -> DisplayMode {
        match self {
            Self::Reasoning { expanded, .. }
            | Self::Diff { expanded, .. }
            | Self::Tool { expanded, .. }
            | Self::Plan { expanded, .. } => {
                if *expanded {
                    DisplayMode::Expanded
                } else {
                    DisplayMode::Collapsed
                }
            }
            _ => DisplayMode::Expanded,
        }
    }

    pub fn set_display_mode(&mut self, mode: DisplayMode) -> bool {
        let expanded = mode == DisplayMode::Expanded;
        match self {
            Self::Reasoning {
                expanded: current, ..
            }
            | Self::Diff {
                expanded: current, ..
            }
            | Self::Tool {
                expanded: current, ..
            }
            | Self::Plan {
                expanded: current, ..
            } => {
                let changed = *current != expanded;
                *current = expanded;
                changed
            }
            _ => false,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(
            self,
            Self::Reasoning { active: true, .. }
                | Self::Tool {
                    status: ToolStatus::Running,
                    ..
                }
                | Self::Compaction { active: true, .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_mode_is_typed_for_foldable_blocks() {
        let mut entry = Entry::Reasoning {
            text: "thinking".into(),
            active: true,
            expanded: false,
        };
        assert_eq!(entry.display_mode(), DisplayMode::Collapsed);
        assert!(entry.set_display_mode(DisplayMode::Expanded));
        assert_eq!(entry.display_mode(), DisplayMode::Expanded);
        assert!(entry.is_running());
    }
}
