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
        // Legacy conhost creates an alternate screen buffer with the parent
        // buffer's larger scrollback dimensions. That exposes native horizontal
        // and vertical scrollbars over a full-screen TUI and can cover the last
        // status row. The alternate buffer is disposable, so keep it matched to
        // the visible window while Torii owns it.
        Self::fit_windows_alternate_buffer();
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

    pub(super) fn fit_windows_alternate_buffer() {
        #[cfg(windows)]
        {
            let _ = fit_windows_alternate_buffer();
        }
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

#[cfg(windows)]
fn fit_windows_alternate_buffer() -> io::Result<()> {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, COORD, GetConsoleScreenBufferInfo, GetStdHandle,
        STD_OUTPUT_HANDLE, SetConsoleScreenBufferSize,
    };

    // SAFETY: all pointers refer to stack-owned Win32 structs for the duration
    // of each call, and the output handle is only queried and resized.
    unsafe {
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(output, &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }

        // A scrolled window cannot be shrunk directly without first moving it.
        // Fresh alternate buffers start at the origin; leave unusual hosts
        // untouched rather than changing their viewport position.
        if info.srWindow.Left != 0 || info.srWindow.Top != 0 {
            return Ok(());
        }
        let visible = COORD {
            X: info.srWindow.Right.saturating_add(1),
            Y: info.srWindow.Bottom.saturating_add(1),
        };
        if info.dwSize.X == visible.X && info.dwSize.Y == visible.Y {
            return Ok(());
        }
        if SetConsoleScreenBufferSize(output, visible) == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
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
