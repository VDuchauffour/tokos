//! The bordered panels, square-box style. Each draws into a [`Rect`] on screen.
//!
//! The [`Painter`] is a thin wrapper over the ratatui [`Buffer`] with edge-safe
//! writes, mirroring the original curses `Painter.text(y, x, s, attr)` API so
//! the panel drawing logic ports one-to-one.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect as TuiRect;
use ratatui::style::Style;

use crate::state::{History, MergedLogEntry, Snapshot, epoch_now};
use crate::ui::layout::Rect;
use crate::ui::theme::{Pair, Theme};
use crate::ui::widgets::{big_number, braille_chart, fmt_duration, fmt_seconds};

// Square box-drawing symbols.
const ROUND_LU: &str = "┌";
const ROUND_RU: &str = "┐";
const ROUND_LD: &str = "└";
const ROUND_RD: &str = "┘";
const H_BAR: &str = "─";
const V_BAR: &str = "│";
const SUPERSCRIPT: [&str; 10] = ["⁰", "¹", "²", "³", "⁴", "⁵", "⁶", "⁷", "⁸", "⁹"];

/// Limit `s` to `max_cols` display columns (counting one column per char — all
/// glyphs used here are single-cell: braille, box-drawing, block elements).
fn truncate_width(s: &str, max_cols: usize) -> String {
    s.chars().take(max_cols).collect()
}

pub struct Painter<'a> {
    buf: &'a mut Buffer,
    width: u16,
    height: u16,
    pub theme: &'a Theme,
    pub focused: bool,
}

impl<'a> Painter<'a> {
    pub fn new(buf: &'a mut Buffer, area: TuiRect, theme: &'a Theme) -> Self {
        Self {
            buf,
            width: area.width,
            height: area.height,
            theme,
            focused: false,
        }
    }

    /// Write `s` at `(y, x)`, clipped to the terminal area (edge-safe).
    pub fn text(&mut self, y: i32, x: i32, s: &str, style: Style) {
        if y < 0 || x < 0 || y >= self.height as i32 || x >= self.width as i32 {
            return;
        }
        let avail = (self.width as i32 - x).max(0) as usize;
        let s = truncate_width(s, avail);
        self.buf.set_string(x as u16, y as u16, s, style);
    }

    /// Draw a square-bordered box and return the inner content [`Rect`].
    ///
    /// `num` (1-9) is drawn as a superscript before the title, marking the key
    /// that toggles the panel. The title is written directly on the top border
    /// line so the outline stays a clean rectangle. `title2` sits on the
    /// bottom edge. `border_pair` colors the outline uniformly.
    #[allow(clippy::too_many_arguments)]
    pub fn box_(
        &mut self,
        rect: Rect,
        title: &str,
        num: i32,
        title_pair: Pair,
        title2: &str,
        border_pair: Pair,
    ) -> Rect {
        let (y, x, h, w) = (rect.y, rect.x, rect.h, rect.w);
        if h < 2 || w < 2 {
            return Rect::new(y, x, 0, 0);
        }
        let border = if self.focused {
            self.theme.attr(Pair::Focus, true, false)
        } else {
            self.theme.attr(border_pair, false, false)
        };

        let line = H_BAR.repeat((w - 2) as usize);
        self.text(y, x, &format!("{ROUND_LU}{line}{ROUND_RU}"), border);
        self.text(y + h - 1, x, &format!("{ROUND_LD}{line}{ROUND_RD}"), border);
        for i in 1..h - 1 {
            self.text(y + i, x, V_BAR, border);
            self.text(y + i, x + w - 1, V_BAR, border);
        }

        self.title_tab(y, x, num, title, title_pair);
        if !title2.is_empty() {
            self.title_tab(y + h - 1, x, 0, title2, title_pair);
        }
        Rect::new(y + 1, x + 1, h - 2, w - 2)
    }

    /// Write the title text directly on the border line at `y`, starting two
    /// cells in from the left corner. The border's `─` chars remain visible
    /// around the title, keeping the outline a clean rectangle.
    fn title_tab(&mut self, y: i32, x: i32, num: i32, title: &str, title_pair: Pair) {
        let mut cx = x + 2;
        if num != 0 {
            let sup = SUPERSCRIPT[num.clamp(0, 9) as usize];
            self.text(y, cx, sup, self.theme.attr(Pair::Hi, true, false));
            cx += 1;
        }
        let label = format!(" {title} ");
        self.text(y, cx, &label, self.theme.attr(title_pair, true, false));
    }
}

fn draw_chart(p: &mut Painter<'_>, inner: Rect, series: &[f64], vmin: f64, vmax: f64, pair: Pair) {
    if inner.h <= 0 || inner.w <= 0 {
        return;
    }
    let rows = braille_chart(
        series,
        inner.w as usize,
        inner.h as usize,
        vmin,
        vmax,
        true,
        false,
    );
    let attr = p.theme.attr(pair, false, false);
    for (i, row) in rows.iter().enumerate() {
        p.text(inner.y + i as i32, inner.x, row, attr);
    }
}

/// A mirrored chart split at a shared centre line.
///
/// Throughput grows up from the centre (positional green->red gradient).
/// Below it, the request count grows down as a stacked two-band chart: running
/// (green) nearest the centre, waiting (magenta) stacked beyond. Corner labels
/// name each half.
fn rule(label: &str, width: i32) -> String {
    if width <= 0 {
        return String::new();
    }
    let w = width as usize;
    let mut s = format!("┄ {label} ");
    let cur = s.chars().count();
    if cur < w {
        s.push_str(&"┄".repeat(w - cur));
    }
    s.chars().take(w).collect()
}

/// A mirrored chart of two single series sharing a centre line.
///
/// `top` grows up from the centre, `bottom` grows down from it; each half scales
/// to its own max. The current value of each is a bold corner label.
fn draw_mirror_chart(
    p: &mut Painter<'_>,
    rect: Rect,
    top: &[f64],
    top_label: &str,
    bottom: &[f64],
    bottom_label: &str,
) {
    if rect.h <= 0 || rect.w <= 0 {
        return;
    }
    let t = p.theme;
    let top_h = (rect.h / 2).max(1);
    let bot_h = rect.h - top_h;

    let top_vmax = top.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
    let top_rows = braille_chart(
        top,
        rect.w as usize,
        top_h as usize,
        0.0,
        top_vmax,
        true,
        false,
    );
    for (i, row) in top_rows.iter().enumerate() {
        let attr = t.attr(Pair::Cyan, false, false);
        p.text(rect.y + i as i32, rect.x, row, attr);
    }

    if bot_h > 0 {
        let bot_vmax = bottom.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
        let bot_rows = braille_chart(
            bottom,
            rect.w as usize,
            bot_h as usize,
            0.0,
            bot_vmax,
            true,
            true,
        );
        for (i, row) in bot_rows.iter().enumerate() {
            let attr = t.attr(Pair::Magenta, false, false);
            p.text(rect.y + top_h + i as i32, rect.x, row, attr);
        }
    }

    if !top_label.is_empty() {
        p.text(
            rect.y,
            rect.x,
            &format!(" {top_label} "),
            t.attr(Pair::Purple, true, false),
        );
    }
    if bot_h > 0 && !bottom_label.is_empty() {
        p.text(
            rect.y + rect.h - 1,
            rect.x,
            &format!(" {bottom_label} "),
            t.attr(Pair::Pink, true, false),
        );
    }
}

/// Render a stats column: each row is a label on the left
/// and a colour-coded value right-aligned within `rw`.
fn draw_stat_column(
    p: &mut Painter<'_>,
    y0: i32,
    h: i32,
    rx: i32,
    rw: i32,
    rows: &[(&str, Pair, String)],
) {
    for (i, (label, vpair, value)) in rows.iter().enumerate() {
        if i as i32 >= h {
            break;
        }
        let y = y0 + i as i32;
        p.text(
            y,
            rx,
            &truncate_width(label, rw as usize),
            p.theme.attr(Pair::Dim, false, false),
        );
        let vs = truncate_width(value, rw as usize);
        let pad = (rw as usize).saturating_sub(vs.chars().count());
        p.text(y, rx + pad as i32, &vs, p.theme.attr(*vpair, true, false));
    }
}

pub fn draw_throughput(
    p: &mut Painter<'_>,
    rect: Rect,
    _snap: &Snapshot,
    hist: &History,
    num: i32,
) {
    let inner = p.box_(rect, "throughput", num, Pair::Title, "tok/s", Pair::BoxThru);
    if inner.h <= 0 {
        return;
    }

    let gen_series = hist.series["gen_tok_s"].values();
    let prompt = hist.series["prompt_tok_s"].values();
    let gen_now = *hist.derived.get("gen_tok_s").unwrap_or(&0.0);
    let prompt_now = *hist.derived.get("prompt_tok_s").unwrap_or(&0.0);

    let right_w = 24.min((inner.w / 3).max(18));
    let chart_w = inner.w - right_w - 1;
    if chart_w < 8 {
        // too narrow to split; chart takes the whole panel
        draw_mirror_chart(
            p,
            inner,
            &gen_series,
            &format!("gen {}", big_number(gen_now, " tok/s")),
            &prompt,
            &format!("prompt {}", big_number(prompt_now, " tok/s")),
        );
        return;
    }

    draw_mirror_chart(
        p,
        Rect::new(inner.y, inner.x, inner.h, chart_w),
        &gen_series,
        "",
        &prompt,
        "",
    );

    let div_x = inner.x + chart_w;
    for i in 0..inner.h {
        p.text(
            inner.y + i,
            div_x,
            V_BAR,
            p.theme.attr(Pair::Div, false, false),
        );
    }
    let rx = div_x + 2;
    let rw = inner.w - chart_w - 2;

    let gen_pk = gen_series.iter().cloned().fold(0.0_f64, f64::max);
    let prompt_pk = prompt.iter().cloned().fold(0.0_f64, f64::max);
    let rows: [(&str, Pair, String); 4] = [
        ("gen now", Pair::Dim, big_number(gen_now, " tok/s")),
        ("gen peak", Pair::Dim, big_number(gen_pk, " tok/s")),
        ("prompt now", Pair::Dim, big_number(prompt_now, " tok/s")),
        ("prompt peak", Pair::Dim, big_number(prompt_pk, " tok/s")),
    ];
    draw_stat_column(p, inner.y, inner.h, rx, rw, &rows);
}

pub fn draw_requests(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot, _hist: &History, num: i32) {
    let inner = p.box_(rect, "requests", num, Pair::Title, "activity", Pair::BoxReq);
    if inner.h <= 0 {
        return;
    }

    p.text(
        inner.y,
        inner.x,
        &H_BAR.repeat(inner.w as usize),
        p.theme.attr(Pair::Div, false, false),
    );
    let feed_rect = Rect::new(inner.y + 1, inner.x, inner.h - 1, inner.w);
    draw_request_feed(p, feed_rect, &snap.merged_log, snap.access_error.as_deref());
}

/// Narrow-terminal fallback: latency rows with inline sparklines, then the
/// KV-cache chart below.
fn draw_perf_stacked(
    p: &mut Painter<'_>,
    inner: Rect,
    hist: &History,
    metrics: &[(&str, &str, Pair)],
    kv: f64,
    hit: f64,
) {
    for (i, (label, key, pair)) in metrics.iter().enumerate() {
        if i as i32 >= inner.h {
            break;
        }
        let val = *hist.derived.get(*key).unwrap_or(&0.0);
        p.text(
            inner.y + i as i32,
            inner.x,
            &format!("{label:<5}"),
            p.theme.attr(Pair::Dim, false, false),
        );
        p.text(
            inner.y + i as i32,
            inner.x + 6,
            &format!("{:>7}", fmt_seconds(val)),
            p.theme.attr(*pair, true, false),
        );
        let spark_x = inner.x + 14;
        let spark_w = inner.w - 14;
        if spark_w > 2 {
            let series = hist.series[*key].values();
            let vmax = series.iter().cloned().fold(0.0_f64, f64::max).max(1e-6);
            let rowsb = braille_chart(&series, spark_w as usize, 1, 0.0, vmax, true, false);
            if let Some(row) = rowsb.first() {
                p.text(
                    inner.y + i as i32,
                    spark_x,
                    row,
                    p.theme.attr(*pair, false, false),
                );
            }
        }
    }

    let dy = inner.y + metrics.len() as i32;
    if dy >= inner.y + inner.h {
        return;
    }
    p.text(
        dy,
        inner.x,
        &rule(&format!("kv {kv:.0}%  prefix {hit:.0}%"), inner.w),
        p.theme.attr(Pair::Div, false, false),
    );
    let chart_y = dy + 1;
    if chart_y < inner.y + inner.h {
        draw_chart(
            p,
            Rect::new(chart_y, inner.x, inner.y + inner.h - chart_y, inner.w),
            &hist.series["kv_cache"].values(),
            0.0,
            100.0,
            Pair::Default,
        );
    }
}

pub fn draw_perf(p: &mut Painter<'_>, rect: Rect, snap: &Snapshot, hist: &History, num: i32) {
    let inner = p.box_(rect, "perf", num, Pair::Title, "recent avg", Pair::BoxLat);
    if inner.h <= 0 {
        return;
    }

    let metrics: [(&str, &str, Pair); 4] = [
        ("TTFT", "ttft", Pair::Cyan),
        ("TPOT", "tpot", Pair::Green),
        ("e2e", "e2e", Pair::Magenta),
        ("queue", "queue_time", Pair::Dim),
    ];

    let right_w = 18.min((inner.w / 4).max(12));
    let chart_w = inner.w - right_w - 1;
    if chart_w < 8 {
        draw_perf_stacked(
            p,
            inner,
            hist,
            &metrics,
            *hist.derived.get("kv_cache").unwrap_or(&0.0),
            snap.backend.prefix_cache_hit_rate() * 100.0,
        );
        return;
    }

    let div_x = inner.x + chart_w;
    for i in 0..inner.h {
        p.text(
            inner.y + i,
            div_x,
            V_BAR,
            p.theme.attr(Pair::Div, false, false),
        );
    }
    let rx = div_x + 2;
    let rw = inner.w - chart_w - 2;

    // Left: one colour-coded sparkline per latency metric (one row each).
    for (i, (_label, key, pair)) in metrics.iter().enumerate() {
        if i as i32 >= inner.h {
            break;
        }
        let series = hist.series[*key].values();
        let vmax = series.iter().cloned().fold(0.0_f64, f64::max).max(1e-6);
        let rowsb = braille_chart(&series, chart_w as usize, 1, 0.0, vmax, true, false);
        if let Some(row) = rowsb.first() {
            p.text(
                inner.y + i as i32,
                inner.x,
                row,
                p.theme.attr(*pair, false, false),
            );
        }
    }

    // Per-request section: separator after latency metrics, then sparklines.
    let pr_y = inner.y + metrics.len() as i32 + 1;
    let has_pr = pr_y < inner.y + inner.h;
    if has_pr {
        p.text(
            pr_y,
            inner.x,
            &rule("per-request", chart_w),
            p.theme.attr(Pair::Div, false, false),
        );
    }

    let pr_metrics: [(&str, &str, Pair); 4] = [
        ("p-tok", "req_prompt_tok", Pair::Cyan),
        ("g-tok", "req_gen_tok", Pair::Green),
        ("prefill", "req_prefill", Pair::Yellow),
        ("decode", "req_decode", Pair::Purple),
    ];
    let mut pr_text_rows: Vec<(&str, Pair, String)> = Vec::new();
    let pr_start = if has_pr { pr_y + 1 } else { inner.y + inner.h };
    for (i, (label, key, pair)) in pr_metrics.iter().enumerate() {
        let row_y = pr_start + i as i32;
        if row_y >= inner.y + inner.h {
            break;
        }
        let series = hist.series[*key].values();
        let vmax = series.iter().cloned().fold(0.0_f64, f64::max).max(1e-6);
        let rowsb = braille_chart(&series, chart_w as usize, 1, 0.0, vmax, true, false);
        if let Some(row) = rowsb.first() {
            p.text(row_y, inner.x, row, p.theme.attr(*pair, false, false));
        }
        let val = *hist.derived.get(*key).unwrap_or(&0.0);
        let v = if key.contains("tok") {
            format!("{val:.0}")
        } else {
            fmt_seconds(val)
        };
        pr_text_rows.push((label, Pair::Dim, v));
    }

    // Right: all text rows, aligned with the lines on the left.
    let mut rows: Vec<(&str, Pair, String)> = metrics
        .iter()
        .map(|(label, key, _)| {
            (
                *label,
                Pair::Dim,
                fmt_seconds(*hist.derived.get(*key).unwrap_or(&0.0)),
            )
        })
        .collect();
    rows.extend(pr_text_rows.iter().cloned());
    draw_stat_column(p, inner.y, inner.h, rx, rw, &rows);
}

// Metrics shown in the 1·5·15 windowed-average panel:
// (label, series key, is_latency).
const LOADAVG_ROWS: &[(&str, &str, bool)] = &[
    ("gen tok/s", "gen_tok_s", false),
    ("prompt tok/s", "prompt_tok_s", false),
    ("running", "running", false),
    ("waiting", "waiting", false),
    ("kv cache", "kv_cache", false),
    ("ttft", "ttft", true),
    ("tpot", "tpot", true),
    ("e2e", "e2e", true),
    ("queue", "queue_time", true),
];

pub fn draw_loadavg(p: &mut Painter<'_>, rect: Rect, _snap: &Snapshot, hist: &History, num: i32) {
    let inner = p.box_(
        rect,
        "1·5·15",
        num,
        Pair::Title,
        "windowed avg",
        Pair::BoxCache,
    );
    if inner.h <= 0 || inner.w <= 0 {
        return;
    }

    // Five columns: a flexible label, then now / 1m / 5m / 15m right-aligned.
    let mut num_w = (inner.w / 6).clamp(6, 9);
    let mut label_w = inner.w - num_w * 4;
    if label_w < 8 {
        num_w = ((inner.w - 8) / 4).max(5);
        label_w = inner.w - num_w * 4;
    }
    if label_w < 6 || num_w < 5 {
        return;
    }

    let cell = |p: &mut Painter<'_>, y: i32, col_i: i32, s: &str, pair: Pair, bold: bool| {
        let s = truncate_width(s, num_w as usize);
        let cx = inner.x + label_w + col_i * num_w;
        p.text(
            y,
            cx + num_w - s.chars().count() as i32,
            &s,
            p.theme.attr(pair, bold, false),
        );
    };

    // Header row.
    p.text(
        inner.y,
        inner.x,
        &truncate_width("metric", label_w as usize),
        p.theme.attr(Pair::Dim, false, true),
    );
    for (i, h) in ["now", "1m", "5m", "15m"].iter().enumerate() {
        cell(p, inner.y, i as i32, h, Pair::Dim, false);
    }

    for (r, (label, key, is_lat)) in LOADAVG_ROWS.iter().enumerate() {
        let y = inner.y + 1 + r as i32;
        if y >= inner.y + inner.h {
            break;
        }
        p.text(
            y,
            inner.x,
            &truncate_width(label, label_w as usize),
            p.theme.attr(Pair::Dim, false, false),
        );
        let emas = *hist.avg.get(*key).unwrap_or(&[0.0; 3]);
        let now = *hist.derived.get(*key).unwrap_or(&0.0);
        let values = [now, emas[0], emas[1], emas[2]];
        for (i, v) in values.iter().enumerate() {
            let s = if *is_lat {
                fmt_seconds(*v)
            } else {
                big_number(*v, "")
            };
            cell(
                p,
                y,
                i as i32,
                &s,
                if i == 0 { Pair::Title } else { Pair::Dim },
                i == 0,
            );
        }
    }
}

/// Truncate a prompt for display, normalizing whitespace and adding `…` when
/// needed. Control characters (ESC, BEL, C1, etc.) are stripped so no escape
/// sequence can reach the terminal.
pub fn truncate_prompt(prompt: Option<&str>, maxlen: usize) -> String {
    let p = match prompt {
        None => return String::new(),
        Some("") => return String::new(),
        Some(s) => s,
    };
    let text: String = p.split_whitespace().collect::<Vec<_>>().join(" ");
    let text: String = text
        .chars()
        .filter(|c| {
            let o = *c as u32;
            o >= 0x20 && !(0x7f..0xa0).contains(&o)
        })
        .collect();
    let count = text.chars().count();
    if count > maxlen {
        let kept: String = text.chars().take(maxlen.saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        text
    }
}

/// Compact character count for the feed's size column: 42, 1.2k, 440k, 1.3M.
fn fmt_chars(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Render the vLLM inference request feed into `rect` — no box.
///
/// Columns (left to right, right columns drop out on narrow panels):
/// `age  req id (flex)  size  max_tok`. Newest first.
fn draw_request_feed(
    p: &mut Painter<'_>,
    rect: Rect,
    entries: &[MergedLogEntry],
    error: Option<&str>,
) {
    if rect.h <= 0 || rect.w <= 0 {
        return;
    }
    if let Some(err) = error {
        let msg = format!("⚠ {err}");
        p.text(
            rect.y,
            rect.x,
            &truncate_width(&msg, rect.w as usize),
            p.theme.attr(Pair::Yellow, false, true),
        );
        return;
    }
    if entries.is_empty() {
        let hint = "run vLLM with --enable-log-requests to show inference requests";
        p.text(
            rect.y,
            rect.x,
            &truncate_width(hint, rect.w as usize),
            p.theme.attr(Pair::Dim, false, true),
        );
        return;
    }

    let now = epoch_now();
    let (age_w, size_w, tok_w) = (4_i32, 6_i32, 6_i32);
    let x_age = rect.x;
    let x_req = x_age + age_w + 1;
    let show_tok = rect.w >= 20;
    let show_size = rect.w >= 28;

    let trailing =
        (if show_size { size_w + 1 } else { 0 }) + (if show_tok { tok_w + 1 } else { 0 });
    let req_w = (rect.x + rect.w - x_req - trailing).max(4);
    let x_size = x_req + req_w + 1;
    let x_tok = if show_size {
        x_size + size_w + 1
    } else {
        x_req + req_w + 1
    };

    let hdr = p.theme.attr(Pair::Dim, false, true);
    p.text(rect.y, x_age, "age", hdr);
    p.text(
        rect.y,
        x_req,
        &truncate_width("req id", req_w as usize),
        hdr,
    );
    if show_size {
        let s = "size";
        p.text(rect.y, x_size + size_w - s.len() as i32, s, hdr);
    }
    if show_tok {
        p.text(
            rect.y,
            x_tok,
            &truncate_width("max_tok", tok_w as usize),
            hdr,
        );
    }

    for (i, e) in entries.iter().enumerate() {
        let y = rect.y + 1 + i as i32;
        if y >= rect.y + rect.h {
            break;
        }
        let age = fmt_duration((now - e.t).max(0.0));
        let age_str = format!("{:>width$}", age, width = age_w as usize);
        p.text(
            y,
            x_age,
            &truncate_width(&age_str, age_w as usize),
            p.theme.attr(Pair::Dim, false, false),
        );
        let rid = e.request_id.as_deref().unwrap_or("—");
        let rid = if e.request_id.is_some() {
            truncate_width(rid, req_w as usize)
        } else {
            "—".to_string()
        };
        let rid_style = if e.request_id.is_some() {
            p.theme.attr(Pair::Cyan, false, false)
        } else {
            p.theme.attr(Pair::Dim, false, true)
        };
        // left-justify rid to req_w
        let rid_padded = format!("{:<width$}", rid, width = req_w as usize);
        p.text(
            y,
            x_req,
            &truncate_width(&rid_padded, req_w as usize),
            rid_style,
        );
        if show_size {
            let (val, has) = match e.prompt_chars {
                Some(n) => (fmt_chars(n), true),
                None => ("—".to_string(), false),
            };
            let style = if has {
                p.theme.attr(Pair::Green, false, false)
            } else {
                p.theme.attr(Pair::Dim, false, true)
            };
            let v = format!("{:>width$}", val, width = size_w as usize);
            p.text(y, x_size, &truncate_width(&v, size_w as usize), style);
        }
        if show_tok {
            let (val, has) = match e.max_tokens {
                Some(n) => (format!("{:>width$}", n, width = tok_w as usize), true),
                None => ("-".to_string(), false),
            };
            let style = if has {
                p.theme.attr(Pair::Cyan, true, false)
            } else {
                p.theme.attr(Pair::Dim, false, true)
            };
            p.text(y, x_tok, &truncate_width(&val, tok_w as usize), style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_prompt_basic() {
        assert_eq!(truncate_prompt(Some("short"), 30), "short");
        assert_eq!(truncate_prompt(Some(&"a".repeat(30)), 30), "a".repeat(30));
        assert_eq!(
            truncate_prompt(Some(&"a".repeat(31)), 30),
            format!("{}…", "a".repeat(29))
        );
        assert_eq!(truncate_prompt(None, 30), "");
        assert_eq!(truncate_prompt(Some(""), 30), "");
    }
}
