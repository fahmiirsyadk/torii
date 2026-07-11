use ratatui::style::Color;

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
}

impl Theme {
    pub const GROK_NIGHT: Self = Self {
        background: Color::Rgb(0, 0, 0),
        foreground: Color::Rgb(229, 229, 229),
        muted: Color::Rgb(139, 139, 139),
        subtle: Color::Rgb(88, 88, 88),
        user_background: Color::Rgb(143, 143, 143),
        user_foreground: Color::White,
        accent: Color::Rgb(211, 114, 255),
        success: Color::Rgb(0, 232, 0),
        warning: Color::Rgb(255, 204, 0),
        error: Color::Rgb(255, 82, 82),
        border: Color::Rgb(218, 218, 218),
        code_background: Color::Rgb(24, 24, 24),
        code_foreground: Color::Rgb(214, 214, 214),
    };
}
