use std::{io, path::Path};

use anyhow::Result;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::supports_keyboard_enhancement,
};
use ratatui::DefaultTerminal;

pub(super) struct TerminalGuard {
    keyboard_enhancement_enabled: bool,
}

impl TerminalGuard {
    pub(super) fn enter() -> Result<(Self, DefaultTerminal)> {
        let terminal = match ratatui::try_init() {
            Ok(terminal) => terminal,
            Err(error) => {
                ratatui::restore();
                return Err(error.into());
            }
        };
        if let Err(error) = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste) {
            ratatui::restore();
            return Err(error.into());
        }
        let keyboard_enhancement_enabled = supports_keyboard_enhancement().unwrap_or(false);
        if keyboard_enhancement_enabled
            && let Err(error) = execute!(
                io::stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
        {
            let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
            ratatui::restore();
            return Err(error.into());
        }
        Ok((
            Self {
                keyboard_enhancement_enabled,
            },
            terminal,
        ))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.keyboard_enhancement_enabled {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = execute!(io::stdout(), DisableMouseCapture, DisableBracketedPaste);
        ratatui::restore();
    }
}

pub(super) fn display_path(path: &Path) -> String {
    let rendered = path.display().to_string();
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return rendered;
    };
    let home = std::path::PathBuf::from(home);
    path.strip_prefix(&home)
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or(rendered)
}

pub(super) fn git_branch(cwd: &Path) -> Option<String> {
    let branch = std::process::Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty());
    branch.or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|commit| format!("detached:{}", commit.trim()))
            .filter(|commit| !commit.ends_with(':'))
    })
}
