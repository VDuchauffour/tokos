//! Colors matching btop's default theme.
//!
//! ratatui writes 24-bit color directly, so each role maps to an exact
//! [`Color::Rgb`] taken from btop's built-in `Default` theme, and the
//! green->yellow->red value gradient (plus the network download/upload
//! gradients) are interpolated continuously by [`Theme::grad_attr`] /
//! [`Theme::net_attr`] rather than pre-allocated into discrete curses pairs.

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
}

type Rgb = (u8, u8, u8);

const GRAD_CPU_STOPS: [Rgb; 3] = [(0x77, 0xCA, 0x9B), (0xCB, 0xC0, 0x6C), (0xDC, 0x4C, 0x4C)];
const NET_DOWN_STOPS: [Rgb; 3] = [(0x1E, 0x13, 0x40), (0x9A, 0x5F, 0xEB), (0xCD, 0xB6, 0xFF)];
const NET_UP_STOPS: [Rgb; 3] = [(0x3A, 0x0A, 0x2A), (0xFF, 0x5F, 0xB0), (0xFF, 0xC0, 0xE0)];

fn palette(pair: Pair) -> Option<Color> {
    let rgb = match pair {
        Pair::Default => return None,
        Pair::Title => (0xff, 0xff, 0xff),
        Pair::Green => (0x77, 0xca, 0x9b),
        Pair::Yellow => (0xcb, 0xc0, 0x6c),
        Pair::Red => (0xdc, 0x4c, 0x4c),
        Pair::Cyan => (0x74, 0xe6, 0xfc),
        Pair::Dim => (0xaa, 0xaa, 0xaa),
        Pair::Magenta => (0xd9, 0x62, 0x6d),
        Pair::Hi => (0xb5, 0x40, 0x40),
        Pair::Div => (0x30, 0x30, 0x30),
        Pair::Inactive => (0x40, 0x40, 0x40),
        Pair::BoxThru => (0x6c, 0x6c, 0x4b),
        Pair::BoxReq => (0x5c, 0x58, 0x8d),
        Pair::BoxLat => (0x80, 0x52, 0x52),
        Pair::BoxCache => (0x55, 0x6d, 0x59),
        Pair::Purple => (0x9a, 0x5f, 0xeb),
        Pair::Pink => (0xff, 0x5f, 0xb0),
        Pair::Prompt => (0xe8, 0xa7, 0x6a),
    };
    Some(Color::Rgb(rgb.0, rgb.1, rgb.2))
}

/// Parse a btop theme hex string: `#rrggbb`, or the grayscale shorthand
/// `#nn` -> `#nnnnnn`.
fn parse_hex(h: &str) -> Rgb {
    let h = h.trim_start_matches('#');
    if h.len() == 2 {
        let v = u8::from_str_radix(h, 16).unwrap_or(0);
        return (v, v, v);
    }
    (
        u8::from_str_radix(h.get(0..2).unwrap_or("0"), 16).unwrap_or(0),
        u8::from_str_radix(h.get(2..4).unwrap_or("0"), 16).unwrap_or(0),
        u8::from_str_radix(h.get(4..6).unwrap_or("0"), 16).unwrap_or(0),
    )
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
}

/// Interpolate `frac` (0..1) across three evenly-spaced RGB stops.
fn grad_color(frac: f32, stops: [Rgb; 3]) -> Rgb {
    let f = frac.clamp(0.0, 1.0);
    if f <= 0.5 {
        lerp(stops[0], stops[1], f / 0.5)
    } else {
        lerp(stops[1], stops[2], (f - 0.5) / 0.5)
    }
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

    /// Style for a point `frac` (0..1) along the green->red value gradient.
    /// Used for btop-style positional coloring: a graph's vertical position or
    /// a meter cell's spot along its length.
    pub fn grad_attr(&self, frac: f32, bold: bool) -> Style {
        let (r, g, b) = grad_color(frac, GRAD_CPU_STOPS);
        style_of(Some(Color::Rgb(r, g, b)), bold, false)
    }

    /// Style for a point `frac` (0..1) along a btop network gradient. `up`
    /// selects the upload gradient (magenta->pink) instead of download
    /// (indigo->lavender). Both fade dark->bright with `frac` so a graph is
    /// dim at its baseline and bright at its peak, like btop's net tab.
    pub fn net_attr(&self, frac: f32, up: bool, bold: bool) -> Style {
        let stops = if up { NET_UP_STOPS } else { NET_DOWN_STOPS };
        let (r, g, b) = grad_color(frac, stops);
        style_of(Some(Color::Rgb(r, g, b)), bold, false)
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

// Re-export the `hex` helper used by the palette for any future truecolor
// extensions; kept private to this module otherwise.
#[allow(dead_code)]
fn _hex_color(h: &str) -> Color {
    let (r, g, b) = parse_hex(h);
    Color::Rgb(r, g, b)
}
