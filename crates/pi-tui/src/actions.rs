//! Typed action registry shared by input routing, the command palette, and hints.
//!
//! The structure is adapted from the action-registry design in Grok Build's
//! Apache-2.0 licensed pager. Torii's actions and dispatch remain project-specific.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ActionId {
    SendPrompt,
    SendNow,
    ToggleMultiline,
    CancelTurn,
    ClearPrompt,
    FocusPrompt,
    FocusScrollback,
    ScrollUp,
    ScrollDown,
    ToggleFold,
    OpenBlockViewer,
    ToggleTasks,
    ToggleQueue,
    CycleMode,
    CommandPalette,
    ModelPicker,
    SessionPicker,
    Settings,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionContext {
    Prompt,
    Scrollback,
    Agent,
    Global,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub fn matches(self, key: &KeyEvent) -> bool {
        self.code == key.code && self.modifiers == key.modifiers
    }

    pub fn display(self) -> String {
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("Ctrl".to_string());
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("Alt".to_string());
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("Shift".to_string());
        }
        let key = match self.code {
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "Tab".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::F(number) => format!("F{number}"),
            KeyCode::Char(character) => character.to_ascii_uppercase().to_string(),
            _ => format!("{:?}", self.code),
        };
        parts.push(key);
        parts.join("+")
    }
}

#[derive(Clone, Debug)]
pub struct ActionDefinition {
    pub id: ActionId,
    pub label: &'static str,
    pub description: &'static str,
    pub context: ActionContext,
    pub primary: KeyBinding,
    pub alternates: &'static [KeyBinding],
    pub hint_priority: Option<u8>,
    pub palette: bool,
}

const NONE: &[KeyBinding] = &[];
const QUESTION: &[KeyBinding] = &[KeyBinding::new(KeyCode::Char('?'), KeyModifiers::NONE)];
const QUEUE_ALT: &[KeyBinding] = &[KeyBinding::new(KeyCode::Char('\''), KeyModifiers::CONTROL)];

pub fn definitions() -> Vec<ActionDefinition> {
    vec![
        ActionDefinition {
            id: ActionId::SendPrompt,
            label: "Send",
            description: "Send the current prompt",
            context: ActionContext::Prompt,
            primary: KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE),
            alternates: NONE,
            hint_priority: Some(0),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::ClearPrompt,
            label: "Clear",
            description: "Clear the current draft",
            context: ActionContext::Prompt,
            primary: KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: None,
            palette: false,
        },
        ActionDefinition {
            id: ActionId::SendNow,
            label: "Send now",
            description: "Deliver the current prompt to the running turn immediately",
            context: ActionContext::Prompt,
            primary: KeyBinding::new(KeyCode::Enter, KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: Some(1),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::ToggleMultiline,
            label: "Multiline",
            description: "Toggle whether Enter inserts a newline",
            context: ActionContext::Prompt,
            primary: KeyBinding::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: None,
            palette: true,
        },
        ActionDefinition {
            id: ActionId::FocusScrollback,
            label: "Scrollback",
            description: "Focus the conversation scrollback",
            context: ActionContext::Prompt,
            primary: KeyBinding::new(KeyCode::Tab, KeyModifiers::NONE),
            alternates: NONE,
            hint_priority: Some(1),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::ScrollUp,
            label: "Navigate",
            description: "Move to the previous conversation block",
            context: ActionContext::Scrollback,
            primary: KeyBinding::new(KeyCode::Up, KeyModifiers::NONE),
            alternates: NONE,
            hint_priority: Some(0),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::ScrollDown,
            label: "Navigate",
            description: "Move to the next conversation block",
            context: ActionContext::Scrollback,
            primary: KeyBinding::new(KeyCode::Down, KeyModifiers::NONE),
            alternates: NONE,
            hint_priority: None,
            palette: false,
        },
        ActionDefinition {
            id: ActionId::ToggleFold,
            label: "Fold",
            description: "Expand or collapse the selected block",
            context: ActionContext::Scrollback,
            primary: KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE),
            alternates: NONE,
            hint_priority: Some(1),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::OpenBlockViewer,
            label: "Open block",
            description: "Open the selected conversation block in a full-screen viewer",
            context: ActionContext::Scrollback,
            primary: KeyBinding::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: Some(3),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::FocusPrompt,
            label: "Prompt",
            description: "Focus the prompt editor",
            context: ActionContext::Scrollback,
            primary: KeyBinding::new(KeyCode::Tab, KeyModifiers::NONE),
            alternates: NONE,
            hint_priority: Some(2),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::ToggleTasks,
            label: "Tasks",
            description: "Open or close the task pane",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: Some(4),
            palette: true,
        },
        ActionDefinition {
            id: ActionId::ToggleQueue,
            label: "Queue",
            description: "Show or hide queued prompts",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::Char(';'), KeyModifiers::CONTROL),
            alternates: QUEUE_ALT,
            hint_priority: None,
            palette: true,
        },
        ActionDefinition {
            id: ActionId::CycleMode,
            label: "Mode",
            description: "Cycle normal, plan, and always-approve modes",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            alternates: NONE,
            hint_priority: Some(3),
            palette: true,
        },
        ActionDefinition {
            id: ActionId::CommandPalette,
            label: "Command palette",
            description: "Search available actions",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
            alternates: QUESTION,
            hint_priority: None,
            palette: true,
        },
        ActionDefinition {
            id: ActionId::ModelPicker,
            label: "Model picker",
            description: "Select the model for this session",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: None,
            palette: true,
        },
        ActionDefinition {
            id: ActionId::SessionPicker,
            label: "Resume session",
            description: "Open the session picker",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: None,
            palette: true,
        },
        ActionDefinition {
            id: ActionId::Settings,
            label: "Settings",
            description: "Open Torii settings",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::F(2), KeyModifiers::NONE),
            alternates: NONE,
            hint_priority: None,
            palette: true,
        },
        ActionDefinition {
            id: ActionId::CancelTurn,
            label: "Cancel turn",
            description: "Cancel the running turn",
            context: ActionContext::Agent,
            primary: KeyBinding::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: Some(0),
            palette: false,
        },
        ActionDefinition {
            id: ActionId::Quit,
            label: "Quit",
            description: "Quit Torii",
            context: ActionContext::Global,
            primary: KeyBinding::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
            alternates: NONE,
            hint_priority: None,
            palette: true,
        },
    ]
}

pub fn lookup(key: &KeyEvent, context: ActionContext) -> Option<ActionId> {
    definitions()
        .into_iter()
        .filter(|definition| definition.context == context)
        .find(|definition| {
            definition.primary.matches(key)
                || definition
                    .alternates
                    .iter()
                    .any(|binding| binding.matches(key))
        })
        .map(|definition| definition.id)
}

pub fn palette(query: &str) -> Vec<ActionDefinition> {
    let query = query.trim().to_ascii_lowercase();
    definitions()
        .into_iter()
        .filter(|definition| definition.palette)
        .filter(|definition| {
            query.is_empty()
                || definition.label.to_ascii_lowercase().contains(&query)
                || definition.description.to_ascii_lowercase().contains(&query)
        })
        .collect()
}

pub fn hints(contexts: &[ActionContext]) -> Vec<ActionDefinition> {
    let mut hints = definitions()
        .into_iter()
        .filter(|definition| contexts.contains(&definition.context))
        .filter(|definition| definition.hint_priority.is_some())
        .collect::<Vec<_>>();
    hints.sort_by_key(|definition| definition.hint_priority.unwrap_or(u8::MAX));
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_exactly_contextual() {
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(
            lookup(&tab, ActionContext::Prompt),
            Some(ActionId::FocusScrollback)
        );
        assert_eq!(
            lookup(&tab, ActionContext::Scrollback),
            Some(ActionId::FocusPrompt)
        );
        assert_eq!(lookup(&tab, ActionContext::Agent), None);
    }

    #[test]
    fn palette_and_hints_share_definitions() {
        let model = palette("model");
        assert_eq!(model.len(), 1);
        assert_eq!(model[0].id, ActionId::ModelPicker);
        let hints = hints(&[ActionContext::Prompt, ActionContext::Agent]);
        assert!(hints.iter().any(|action| action.id == ActionId::SendPrompt));
        assert!(hints.iter().any(|action| action.id == ActionId::CycleMode));
    }

    #[test]
    fn key_display_is_human_readable() {
        assert_eq!(
            KeyBinding::new(KeyCode::Char('m'), KeyModifiers::CONTROL).display(),
            "Ctrl+M"
        );
        assert_eq!(
            KeyBinding::new(KeyCode::BackTab, KeyModifiers::SHIFT).display(),
            "Shift+Tab"
        );
    }
}
