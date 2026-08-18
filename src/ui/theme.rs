//! Colors for the TUI, using the terminal's 16-color palette (lazygit-style).
//!
//! Each logical [`Pair`] role maps to a named [`Color`] variant so the UI
//! inherits the user's terminal theme rather than forcing 24-bit RGB.

use ratatui::style::{Color, Modifier, Style};

/// A logical color role (the curses "pair" id), decoupled from the actual RGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pair {
    Default,
    Title,
    Green,
    Yellow,
    Red,
    Cyan,
    Dim,
    Magenta,
    Hi,
    Div,
    Inactive,
    BoxThru,
    BoxReq,
    BoxLat,
    BoxCache,
    Purple,
    Pink,
    Prompt,
    Focus,
}

fn palette(pair: Pair) -> Option<Color> {
    let color = match pair {
        Pair::Default => return None,
        Pair::Title => Color::Reset,
        Pair::Green => Color::Green,
        Pair::Yellow => Color::Yellow,
        Pair::Red => Color::Red,
        Pair::Cyan => Color::Cyan,
        Pair::Dim => Color::DarkGray,
        Pair::Magenta => Color::Magenta,
        Pair::Hi => Color::Yellow,
        Pair::Div => Color::DarkGray,
        Pair::Inactive => Color::DarkGray,
        Pair::BoxThru => Color::DarkGray,
        Pair::BoxReq => Color::DarkGray,
        Pair::BoxLat => Color::DarkGray,
        Pair::BoxCache => Color::DarkGray,
        Pair::Purple => Color::Blue,
        Pair::Pink => Color::Magenta,
        Pair::Prompt => Color::Yellow,
        Pair::Focus => Color::White,
    };
    Some(color)
}

fn style_of(color: Option<Color>, bold: bool, dim: bool) -> Style {
    let mut s = Style::default();
    if let Some(c) = color {
        s = s.fg(c);
    }
    if bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    if dim {
        s = s.add_modifier(Modifier::DIM);
    }
    s
}

pub struct Theme;

impl Theme {
    pub fn new() -> Self {
        Self
    }

    /// Style for a logical [`Pair`], optionally bold/dim.
    pub fn attr(&self, pair: Pair, bold: bool, dim: bool) -> Style {
        style_of(palette(pair), bold, dim)
    }

    /// Pick the btop cpu-gradient green/yellow/red by a low-good threshold.
    pub fn threshold(&self, value: f64, warn: f64, crit: f64) -> Pair {
        if value >= crit {
            Pair::Red
        } else if value >= warn {
            Pair::Yellow
        } else {
            Pair::Green
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::new()
    }
}
