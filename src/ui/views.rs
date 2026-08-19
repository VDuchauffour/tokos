//! The single persistent layout tree: all panels always visible.
//!
//! Arrangement: loadavg sidebar (left) beside a stacked main column
//! (throughput over perf) on top; requests feed at the bottom.

use std::sync::LazyLock;

use crate::ui::layout::{Node, col, leaf, row};

/// The persistent layout: sidebar + stacked main on top, requests feed below.
pub static LAYOUT: LazyLock<Node> = LazyLock::new(|| {
    col(
        vec![
            row(
                vec![
                    leaf("loadavg", 1),
                    col(vec![leaf("throughput", 1), leaf("perf", 1)], 2),
                ],
                4,
            ),
            leaf("requests", 2),
        ],
        1,
    )
});
