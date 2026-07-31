//! Hand-rolled drawing primitives for the btop look.
//!
//! The headline widget is [`braille_chart`], which packs a series into a 2x4
//! Unicode braille dot matrix per cell for `2w x 4h`-dot resolution.

// Braille dot bit values, indexed [column][row] with row 0 at the top:
//   1 4        0x01 0x08
//   2 5   ->   0x02 0x10
//   3 6        0x04 0x20
//   7 8        0x40 0x80
const DOT_BITS: [[u32; 4]; 2] = [
    [0x01, 0x02, 0x04, 0x40], // left column, top->bottom
    [0x08, 0x10, 0x20, 0x80], // right column, top->bottom
];
const BRAILLE_BASE: u32 = 0x2800;

fn resample_tail(series: &[f64], n: usize) -> Vec<f64> {
    if series.len() >= n {
        series[series.len() - n..].to_vec()
    } else {
        series.to_vec()
    }
}

/// Render `series` as `height` rows of `width` braille glyphs.
///
/// Each cell is a 2x4 dot matrix, so the effective resolution is `2*width`
/// columns by `4*height` dots. Values are clipped to `[vmin, vmax]`; the most
/// recent samples are shown right-aligned.
///
/// With `baseline` (btop's `no_zero` behavior) every sampled column lights at
/// least its bottom dot, so the graph keeps a continuous floor instead of
/// leaving gaps where the value is zero.
///
/// With `flip` the bars grow downward from the top row instead of upward from
/// the bottom — used for the lower half of a btop-style mirrored chart, so its
/// floor sits on the shared centre line.
pub fn braille_chart(
    series: &[f64],
    width: usize,
    height: usize,
    vmin: f64,
    vmax: f64,
    baseline: bool,
    flip: bool,
) -> Vec<String> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let dot_cols = 2 * width;
    let dot_rows = 4 * height;

    let vals = resample_tail(series, dot_cols);
    let pad = dot_cols - vals.len();

    let mut span = vmax - vmin;
    if span <= 0.0 {
        span = 1.0;
    }

    let mut cells = vec![vec![0u32; width]; height];

    for (i, &value) in vals.iter().enumerate() {
        let gc = pad + i;
        let mut norm = (value - vmin) / span;
        if norm < 0.0 {
            norm = 0.0;
        } else if norm > 1.0 {
            norm = 1.0;
        }
        let mut filled = (norm * dot_rows as f64).round() as i64;
        if baseline {
            filled = filled.max(1);
        } else if filled <= 0 {
            continue;
        }
        let cell_col = gc / 2;
        let sub_col = gc % 2;
        for k in 0..filled {
            let gr = if flip { k } else { dot_rows as i64 - 1 - k };
            let cell_row = (gr / 4) as usize;
            let sub_row = (gr % 4) as usize;
            cells[cell_row][cell_col] |= DOT_BITS[sub_col][sub_row];
        }
    }

    cells
        .into_iter()
        .map(|row| {
            row.iter()
                .map(|&bits| char::from_u32(BRAILLE_BASE + bits).unwrap_or(' '))
                .collect()
        })
        .collect()
}

/// Compress `values` onto a [0, 1] axis that expands the band near `vmax`.
///
/// Plotting near-saturated data (e.g. KV cache that sits at 85-95%) on a linear
/// axis crushes all the variation into a thin strip at the top. This log-scales
/// the *headroom* (`vmax - value`) instead: a value of 0 maps to 0 (a thin
/// floor), `vmax` maps to 1 (the top), and the spacing stretches as values
/// approach `vmax` so the busy band fills the chart. Larger `k` = more stretch
/// near the ceiling. Feed the result to [`braille_chart`] with `vmin=0,
/// vmax=1`.
pub fn log_headroom_scale(values: &[f64], vmax: f64) -> Vec<f64> {
    log_headroom_scale_k(values, vmax, 19.0)
}

fn log_headroom_scale_k(values: &[f64], vmax: f64, k: f64) -> Vec<f64> {
    if vmax <= 0.0 {
        return vec![0.0; values.len()];
    }
    let denom = k.ln_1p();
    values
        .iter()
        .map(|&v| {
            let mut h = vmax - v;
            if h < 0.0 {
                h = 0.0;
            } else if h > vmax {
                h = vmax;
            }
            1.0 - (k * h / vmax).ln_1p() / denom
        })
        .collect()
}

/// Two stacked series growing *downward* from the top row.
///
/// Used for the lower half of a btop-style mirrored chart: `near` grows from
/// the centre line outward (its floor keeps the axis continuous), and `far` is
/// stacked beyond it. Returns a `height x width` grid of `(glyph, band)` where
/// `band` is 0 for `near`, 1 for `far`, or -1 for an empty cell.
///
/// Because a braille cell packs 2x4 dots but can carry only one colour, a
/// cell's band is whichever series owns more of its lit dots (ties -> near).
///
/// With `log` the *cumulative* stack is log-scaled (`ln_1p`): `near` fills
/// `ln_1p(near)` of the column and the total reaches `ln_1p(near + far)`, both
/// normalised by `ln_1p(vmax)`. This expands the low end so small integer
/// counts stay distinct instead of being flattened by an occasional burst.
pub fn stacked_chart_down(
    near: &[f64],
    far: &[f64],
    width: usize,
    height: usize,
    vmax: f64,
    log: bool,
) -> Vec<Vec<(char, i8)>> {
    let mut grid = vec![vec![(' ', -1_i8); width]; height];
    if width == 0 || height == 0 {
        return grid;
    }

    let dot_cols = 2 * width;
    let dot_rows = 4 * height;
    let span = if vmax > 0.0 { vmax } else { 1.0 };
    let log_span = if log { span.ln_1p() } else { 0.0 };
    let near_pad = dot_cols.saturating_sub(near.len());
    let far_pad = dot_cols.saturating_sub(far.len());

    let mut near_cells = vec![vec![0u32; width]; height];
    let mut far_cells = vec![vec![0u32; width]; height];

    for gc in 0..dot_cols {
        let ni = gc as isize - near_pad as isize;
        let fi = gc as isize - far_pad as isize;
        let n_has = ni >= 0 && (ni as usize) < near.len();
        let f_has = fi >= 0 && (fi as usize) < far.len();
        if !(n_has || f_has) {
            continue;
        }
        let nval = if n_has {
            near[ni as usize].max(0.0)
        } else {
            0.0
        };
        let fval = if f_has {
            far[fi as usize].max(0.0)
        } else {
            0.0
        };

        let (n_dots, f_dots) = if log {
            let n_frac = if log_span != 0.0 {
                nval.ln_1p() / log_span
            } else {
                0.0
            };
            let t_frac = if log_span != 0.0 {
                (nval + fval).ln_1p() / log_span
            } else {
                0.0
            };
            let n_dots = ((n_frac.min(1.0) * dot_rows as f64).round() as i64).max(1);
            let t_dots = (t_frac.min(1.0) * dot_rows as f64).round() as i64;
            (n_dots, (t_dots - n_dots).max(0))
        } else {
            let n_dots = (((nval / span).min(1.0) * dot_rows as f64).round() as i64).max(1);
            let f_dots = ((fval / span).min(1.0) * dot_rows as f64).round() as i64;
            (n_dots, f_dots)
        };
        let mut f_dots = f_dots;
        if n_dots + f_dots > dot_rows as i64 {
            f_dots = dot_rows as i64 - n_dots;
        }
        let cell_col = gc / 2;
        let sub_col = gc % 2;
        for k in 0..n_dots {
            near_cells[(k / 4) as usize][cell_col] |= DOT_BITS[sub_col][(k % 4) as usize];
        }
        for k in n_dots..(n_dots + f_dots) {
            far_cells[(k / 4) as usize][cell_col] |= DOT_BITS[sub_col][(k % 4) as usize];
        }
    }

    for row in 0..height {
        for col in 0..width {
            let nb = near_cells[row][col];
            let fb = far_cells[row][col];
            if nb == 0 && fb == 0 {
                continue;
            }
            let band = if nb.count_ones() >= fb.count_ones() {
                0
            } else {
                1
            };
            grid[row][col] = (
                char::from_u32(BRAILLE_BASE + (nb | fb)).unwrap_or(' '),
                band as i8,
            );
        }
    }
    grid
}

/// A horizontal bar of `width` chars filled proportional to value/vmax.
pub fn hbar(value: f64, vmax: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let frac = if vmax <= 0.0 {
        0.0
    } else {
        (value / vmax).clamp(0.0, 1.0)
    };
    let filled = (frac * width as f64).round() as usize;
    let filled = filled.min(width);
    "█".repeat(filled) + &"░".repeat(width - filled)
}

/// Compact human-readable number with a unit suffix.
pub fn big_number(value: f64, unit: &str) -> String {
    let text = if value >= 1_000_000.0 {
        format!("{:.2}M", value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    if unit.is_empty() {
        text
    } else {
        format!("{text}{unit}")
    }
}

/// Render a small inline sparkline using block elements (8 levels).
pub fn histogram_bars(values: &[f64], vmax: f64) -> String {
    let blocks = " ▁▂▃▄▅▆▇█";
    let n = blocks.chars().count();
    let vmax = if vmax <= 0.0 { 1.0 } else { vmax };
    values
        .iter()
        .map(|&v| {
            let frac = (v / vmax).clamp(0.0, 1.0);
            let idx = (frac * (n - 1) as f64).round() as usize;
            blocks.chars().nth(idx.min(n - 1)).unwrap_or(' ')
        })
        .collect()
}

/// Human latency: ms below 1s, otherwise seconds.
pub fn fmt_seconds(value: f64) -> String {
    if value <= 0.0 {
        return "—".to_string();
    }
    if value < 1.0 {
        return format!("{:.0}ms", value * 1000.0);
    }
    format!("{value:.2}s")
}

/// Compact uptime: '45s', '12m', '3h 20m', '2d 4h'.
pub fn fmt_duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "—".to_string();
    }
    let s = seconds as i64;
    let (days, rem) = (s / 86400, s % 86400);
    let (hours, rem) = (rem / 3600, rem % 3600);
    let (mins, secs) = (rem / 60, rem % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        format!("{secs}s")
    }
}

/// Bytes -> human readable (KB/MB/GB/...).
pub fn fmt_bytes(mut value: f64) -> String {
    for unit in ["B", "KB", "MB", "GB", "TB"] {
        if value < 1024.0 {
            return format!("{value:.1}{unit}");
        }
        value /= 1024.0;
    }
    format!("{value:.1}PB")
}
