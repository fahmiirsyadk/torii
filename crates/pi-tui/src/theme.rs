use std::{fs, path::PathBuf};

use ratatui::style::Color;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

fn preference_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".torii").join("theme"))
}

pub fn load_preference() -> ThemeMode {
    preference_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .as_deref()
        .and_then(ThemeMode::parse)
        .unwrap_or_default()
}

pub fn save_preference(mode: ThemeMode) -> std::io::Result<()> {
    let Some(path) = preference_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, mode.label())
}

/// Semantic GrokNight palette used throughout the agent UI.
///
/// Values track Grok Build's Apache-2.0 GrokNight theme. Compatibility aliases
/// remain while Torii's older renderers move to semantic roles.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,
    pub muted: Color,
    pub subtle: Color,
    pub user_background: Color,
    pub user_foreground: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub border: Color,
    pub code_background: Color,
    pub code_foreground: Color,

    pub bg_light: Color,
    pub bg_dark: Color,
    pub bg_highlight: Color,
    pub bg_hover: Color,
    pub text_secondary: Color,
    pub gray_bright: Color,
    pub accent_user: Color,
    pub accent_assistant: Color,
    pub accent_thinking: Color,
    pub accent_tool: Color,
    pub accent_system: Color,
    pub accent_running: Color,
    pub accent_model: Color,
    pub accent_plan: Color,
    pub prompt_border: Color,
    pub prompt_border_active: Color,
    pub selection_border: Color,
    pub hover_border: Color,
    pub path: Color,
    pub command: Color,
    pub running: Color,
    pub diff_delete_bg: Color,
    pub diff_delete_fg: Color,
    pub diff_insert_bg: Color,
    pub diff_insert_fg: Color,
}

impl Theme {
    pub const GROK_NIGHT: Self = Self {
        background: Color::Rgb(20, 20, 20),
        foreground: Color::Rgb(225, 225, 225),
        muted: Color::Rgb(108, 108, 108),
        subtle: Color::Rgb(88, 88, 88),
        user_background: Color::Rgb(36, 36, 36),
        user_foreground: Color::Rgb(225, 225, 225),
        accent: Color::Rgb(187, 154, 247),
        success: Color::Rgb(158, 206, 106),
        warning: Color::Rgb(224, 175, 104),
        error: Color::Rgb(247, 118, 142),
        border: Color::Rgb(80, 80, 88),
        code_background: Color::Rgb(28, 28, 28),
        code_foreground: Color::Rgb(200, 200, 200),

        bg_light: Color::Rgb(36, 36, 36),
        bg_dark: Color::Rgb(28, 28, 28),
        bg_highlight: Color::Rgb(36, 36, 36),
        bg_hover: Color::Rgb(44, 44, 44),
        text_secondary: Color::Rgb(200, 200, 200),
        gray_bright: Color::Rgb(120, 120, 120),
        accent_user: Color::Rgb(200, 200, 200),
        accent_assistant: Color::Rgb(187, 154, 247),
        accent_thinking: Color::Rgb(187, 154, 247),
        accent_tool: Color::Rgb(120, 120, 120),
        accent_system: Color::Rgb(122, 162, 247),
        accent_running: Color::Rgb(187, 154, 247),
        accent_model: Color::Rgb(26, 188, 156),
        accent_plan: Color::Rgb(255, 219, 141),
        prompt_border: Color::Rgb(50, 50, 55),
        prompt_border_active: Color::Rgb(80, 80, 88),
        selection_border: Color::Rgb(60, 60, 65),
        hover_border: Color::Rgb(30, 30, 34),
        path: Color::Rgb(255, 158, 100),
        command: Color::Rgb(224, 175, 104),
        running: Color::Rgb(125, 207, 255),
        diff_delete_bg: Color::Rgb(66, 14, 20),
        diff_delete_fg: Color::Rgb(247, 118, 142),
        diff_insert_bg: Color::Rgb(6, 56, 6),
        diff_insert_fg: Color::Rgb(158, 206, 106),
    };

    pub const GROK_LIGHT: Self = Self {
        background: Color::Rgb(250, 250, 250),
        foreground: Color::Rgb(30, 30, 32),
        muted: Color::Rgb(105, 105, 112),
        subtle: Color::Rgb(155, 155, 162),
        user_background: Color::Rgb(238, 238, 240),
        user_foreground: Color::Rgb(30, 30, 32),
        accent: Color::Rgb(105, 70, 190),
        success: Color::Rgb(45, 125, 65),
        warning: Color::Rgb(165, 105, 20),
        error: Color::Rgb(190, 45, 70),
        border: Color::Rgb(185, 185, 192),
        code_background: Color::Rgb(238, 238, 240),
        code_foreground: Color::Rgb(45, 45, 48),
        bg_light: Color::Rgb(244, 244, 246),
        bg_dark: Color::Rgb(232, 232, 235),
        bg_highlight: Color::Rgb(226, 226, 230),
        bg_hover: Color::Rgb(218, 218, 224),
        text_secondary: Color::Rgb(65, 65, 70),
        gray_bright: Color::Rgb(90, 90, 98),
        accent_user: Color::Rgb(55, 95, 155),
        accent_assistant: Color::Rgb(105, 70, 190),
        accent_thinking: Color::Rgb(105, 70, 190),
        accent_tool: Color::Rgb(95, 95, 105),
        accent_system: Color::Rgb(45, 95, 185),
        accent_running: Color::Rgb(105, 70, 190),
        accent_model: Color::Rgb(0, 125, 105),
        accent_plan: Color::Rgb(145, 95, 10),
        prompt_border: Color::Rgb(198, 198, 204),
        prompt_border_active: Color::Rgb(125, 125, 135),
        selection_border: Color::Rgb(160, 160, 168),
        hover_border: Color::Rgb(140, 140, 148),
        path: Color::Rgb(175, 75, 20),
        command: Color::Rgb(155, 90, 15),
        running: Color::Rgb(20, 115, 160),
        diff_delete_bg: Color::Rgb(255, 225, 230),
        diff_delete_fg: Color::Rgb(165, 35, 55),
        diff_insert_bg: Color::Rgb(222, 245, 225),
        diff_insert_fg: Color::Rgb(35, 115, 55),
    };

    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::GROK_NIGHT,
            ThemeMode::Light => Self::GROK_LIGHT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_mode_parser_rejects_unknown_preferences() {
        assert_eq!(ThemeMode::parse(" light\n"), Some(ThemeMode::Light));
        assert_eq!(ThemeMode::parse("dark"), Some(ThemeMode::Dark));
        assert_eq!(ThemeMode::parse("system"), None);
    }
}
