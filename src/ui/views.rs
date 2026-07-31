//! Built-in views: named layout trees over the panel registry.
//!
//! Each view is a fixed arrangement the user cycles through with the number
//! keys (`1`-`N`) or `Tab`.

use std::sync::LazyLock;

use crate::ui::layout::{col, leaf, row, Node};

pub struct View {
    pub name: &'static str,
    pub root: Node,
}

// The default arrangement: throughput over the 1·5·15 averages, beside the
// request feed (weight 3 of 5).
fn overview() -> Node {
    col(
        vec![row(
            vec![
                col(vec![leaf("throughput", 3), leaf("loadavg", 2)], 1),
                leaf("requests", 1),
            ],
            3,
        )],
        1,
    )
}

// Load-average style 1/5/15-minute windows over the key metrics, with the live
// throughput and latency charts beneath for context.
fn rates() -> Node {
    col(
        vec![
            leaf("loadavg", 3),
            row(vec![leaf("throughput", 1), leaf("perf", 1)], 2),
        ],
        1,
    )
}

// Request-feed focus: the live feed large, with a compact perf strip.
fn requests() -> Node {
    col(vec![leaf("requests", 3), leaf("perf", 2)], 1)
}

pub static VIEWS: LazyLock<Vec<View>> = LazyLock::new(|| {
    vec![
        View {
            name: "overview",
            root: overview(),
        },
        View {
            name: "1·5·15",
            root: rates(),
        },
        View {
            name: "requests",
            root: requests(),
        },
    ]
});
