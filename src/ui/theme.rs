use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub border: Color,
    pub brand: Color,
    pub title: Color,
    pub text: Color,
    pub muted: Color,
    pub selected: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

impl Theme {
    pub fn new(color: bool) -> Self {
        if color {
            Self {
                border: Color::Rgb(92, 104, 128),
                brand: Color::Blue,
                title: Color::Cyan,
                text: Color::White,
                muted: Color::Gray,
                selected: Color::Blue,
                success: Color::Green,
                warning: Color::Yellow,
                error: Color::Red,
            }
        } else {
            Self {
                border: Color::Reset,
                brand: Color::Reset,
                title: Color::Reset,
                text: Color::Reset,
                muted: Color::Reset,
                selected: Color::Reset,
                success: Color::Reset,
                warning: Color::Reset,
                error: Color::Reset,
            }
        }
    }

    pub fn border_style(self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn title_style(self) -> Style {
        Style::default().fg(self.title).add_modifier(Modifier::BOLD)
    }

    pub fn brand_style(self) -> Style {
        Style::default().fg(self.brand).add_modifier(Modifier::BOLD)
    }

    pub fn text_style(self) -> Style {
        Style::default().fg(self.text)
    }

    pub fn muted_style(self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn selected_style(self) -> Style {
        Style::default()
            .fg(self.text)
            .bg(self.selected)
            .add_modifier(Modifier::BOLD)
    }

    pub fn success_style(self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn warning_style(self) -> Style {
        Style::default().fg(self.warning)
    }

    pub fn error_style(self) -> Style {
        Style::default().fg(self.error)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new(true)
    }
}
