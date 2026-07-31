//! Panel registry: metadata + draw fn for every panel a view can reference.
//!
//! Decouples panel identity from any particular view. A [`Panel`] names its draw
//! function and what it `requires` — a capability that must be present for the
//! panel to be shown (e.g. `"log"` for the requests panel). The app computes the
//! available capability set each frame and the layout prunes panels whose
//! requirement is unmet, so a view referencing such a panel still works without
//! the capability (that panel just drops out and its siblings reflow).

use crate::state::{History, Snapshot};
use crate::ui::layout::Rect;
use crate::ui::panels::Painter;

/// A panel draw fn: `(Painter, Rect, Snapshot, History, num)`.
pub type DrawFn = fn(&mut Painter<'_>, Rect, &Snapshot, &History, i32);

pub struct Panel {
    pub id: &'static str,
    pub title: &'static str,
    pub draw: DrawFn,
    pub requires: &'static [&'static str],
}

pub const REGISTRY: &[Panel] = &[
    Panel {
        id: "throughput",
        title: "throughput",
        draw: crate::ui::panels::draw_throughput,
        requires: &[],
    },
    Panel {
        id: "requests",
        title: "requests",
        draw: crate::ui::panels::draw_requests,
        requires: &[],
    },
    Panel {
        id: "perf",
        title: "perf",
        draw: crate::ui::panels::draw_perf,
        requires: &[],
    },
    Panel {
        id: "loadavg",
        title: "1·5·15",
        draw: crate::ui::panels::draw_loadavg,
        requires: &[],
    },
];

pub fn find_panel(id: &str) -> Option<&'static Panel> {
    REGISTRY.iter().find(|p| p.id == id)
}
