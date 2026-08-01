//! The ratatui application: background poller thread + render loop.
//!
//! [`Poller`] runs in a daemon thread, scraping vLLM `/metrics` at the
//! configured interval, storing the latest snapshot under a lock. The main
//! thread loops at a faster tick (250 ms), reading the latest snapshot,
//! deriving rates from the [`History`], and redrawing the active view's
//! panels. Views are switched with `1`-`N` (or `Tab`); each is a fixed layout
//! tree defined in [`crate::ui::views`].

use std::collections::HashSet;
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend, style::Style};

use crate::collectors::access_log::AccessLogTailer;
use crate::collectors::{self, Backend, BackendKind};
use crate::config::AppConfig;
use crate::state::{BackendSnapshot, History, Snapshot, monotonic_now};
use crate::ui::layout::compute_layout;
use crate::ui::panels::Painter;
use crate::ui::registry::{REGISTRY, find_panel};
use crate::ui::theme::{Pair, Theme};
use crate::ui::views::VIEWS;

/// Render tick: how often the UI wakes to handle input / redraw (seconds).
const RENDER_TICK_MS: u64 = 250;
const MIN_INTERVAL: f64 = 0.2;
const MAX_INTERVAL: f64 = 10.0;
/// How long (seconds) the view-name toast stays up after switching views.
const TOAST_SECONDS: f64 = 1.5;

struct PollerState {
    latest: Mutex<Option<Snapshot>>,
    stop: AtomicBool,
    paused: AtomicBool,
    interval: Mutex<f64>,
    pending_backend: Mutex<Option<BackendKind>>,
    kind: Mutex<BackendKind>,
    kind_epoch: AtomicUsize,
}

/// Background thread that takes snapshots without blocking the UI.
pub struct Poller {
    state: Arc<PollerState>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Poller {
    pub fn start(config: &AppConfig, tailer: Option<Arc<AccessLogTailer>>) -> Self {
        let state = Arc::new(PollerState {
            latest: Mutex::new(None),
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            interval: Mutex::new(config.interval),
            pending_backend: Mutex::new(None),
            kind: Mutex::new(config.backend),
            kind_epoch: AtomicUsize::new(0),
        });
        let st = state.clone();
        let metrics_url = config.metrics_url();
        let timeout = config.http_timeout;
        let backend = config.backend;
        let handle = thread::spawn(move || {
            let _span = tracing::info_span!("poller", url = %metrics_url).entered();
            let mut collector: Box<dyn Backend> =
                collectors::make_collector(backend, metrics_url.clone(), timeout);
            loop {
                if st.stop.load(Ordering::Relaxed) {
                    break;
                }
                tracing::trace!("polling metrics");

                if let Some(new_kind) = st.pending_backend.lock().unwrap().take() {
                    collector = collectors::make_collector(new_kind, metrics_url.clone(), timeout);
                }

                if !st.paused.load(Ordering::Relaxed) {
                    let (merged, err) = match &tailer {
                        Some(t) => (t.merged_log(None), t.error()),
                        None => (Vec::new(), None),
                    };
                    let snap = Snapshot {
                        monotonic: monotonic_now(),
                        backend: collector.poll(),
                        merged_log: merged,
                        access_error: err,
                    };
                    *st.latest.lock().unwrap() = Some(snap);

                    let effective = collector.effective_kind();
                    let mut kind_guard = st.kind.lock().unwrap();
                    if effective != *kind_guard {
                        *kind_guard = effective;
                        drop(kind_guard);
                        st.kind_epoch.fetch_add(1, Ordering::Relaxed);
                    }
                }
                let interval = *st.interval.lock().unwrap();
                let mut slept = 0.0_f64;
                while slept < interval && !st.stop.load(Ordering::Relaxed) {
                    let step = 0.1_f64.min(interval - slept);
                    thread::sleep(Duration::from_secs_f64(step));
                    slept += step;
                }
            }
        });
        Poller {
            state,
            handle: Mutex::new(Some(handle)),
        }
    }

    /// Pop the latest snapshot if a new one is available.
    pub fn take(&self) -> Option<Snapshot> {
        self.state.latest.lock().unwrap().take()
    }

    pub fn stop(&self) {
        self.state.stop.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.handle.lock()
            && let Some(h) = guard.take()
        {
            let _ = h.join();
        }
    }

    pub fn toggle_pause(&self) -> bool {
        let cur = self.state.paused.load(Ordering::Relaxed);
        self.state.paused.store(!cur, Ordering::Relaxed);
        !cur
    }

    pub fn paused(&self) -> bool {
        self.state.paused.load(Ordering::Relaxed)
    }

    pub fn interval(&self) -> f64 {
        *self.state.interval.lock().unwrap()
    }

    pub fn set_interval(&self, v: f64) {
        *self.state.interval.lock().unwrap() = v;
    }

    /// Request a backend swap to `kind`. The poller thread applies it at the
    /// top of the next loop iteration — never mid-fetch.
    pub fn request_backend(&self, kind: BackendKind) {
        *self.state.pending_backend.lock().unwrap() = Some(kind);
    }

    /// Monotonically increasing counter that ticks every time the effective
    /// backend kind changes. The render loop compares this to decide when to
    /// clear `History`.
    pub fn kind_epoch(&self) -> usize {
        self.state.kind_epoch.load(Ordering::Relaxed)
    }
}

pub struct App {
    config: AppConfig,
    theme: Theme,
    history: History,
    tailer: Option<Arc<AccessLogTailer>>,
    poller: Poller,
    show_help: bool,
    last: Option<Snapshot>,
    active_view: usize,
    toast_until: f64,
    last_kind_epoch: usize,
    backend_toast_until: f64,
}

impl App {
    pub fn new(config: AppConfig) -> Self {
        let tailer = if config.has_log_source() {
            Some(Arc::new(AccessLogTailer::new(
                config.log_file.clone(),
                config.docker_container.clone(),
                200,
            )))
        } else {
            None
        };
        let poller = Poller::start(&config, tailer.clone());
        Self {
            config,
            theme: Theme::new(),
            history: History::new(crate::config::HISTORY_LEN),
            tailer,
            poller,
            show_help: false,
            last: None,
            active_view: 0,
            toast_until: 0.0,
            last_kind_epoch: 0,
            backend_toast_until: 0.0,
        }
    }

    fn available_caps(&self) -> HashSet<&'static str> {
        let mut caps: HashSet<&'static str> = HashSet::new();
        if self.tailer.is_some() {
            caps.insert("log");
        }
        REGISTRY
            .iter()
            .filter(|p| p.requires.iter().all(|r| caps.contains(r)))
            .map(|p| p.id)
            .collect()
    }

    pub fn run(&mut self) -> io::Result<i32> {
        if let Some(t) = &self.tailer {
            t.start();
        }
        // Restore the terminal even if we panic mid-loop.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
            prev_hook(info);
        }));

        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.run_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
        result?;
        Ok(0)
    }

    fn run_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
        loop {
            if let Some(snap) = self.poller.take() {
                let epoch = self.poller.kind_epoch();
                if epoch != self.last_kind_epoch {
                    self.history.clear();
                    self.last_kind_epoch = epoch;
                }
                self.history.update(snap.clone());
                self.last = Some(snap);
            }

            terminal.draw(|frame| self.draw(frame))?;

            if !event::poll(Duration::from_millis(RENDER_TICK_MS))? {
                continue;
            }
            let ev = event::read()?;
            let Event::Key(key) = ev else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if self.show_help {
                self.show_help = false;
                continue;
            }

            match key.code {
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(());
                }
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('h') | KeyCode::Char('?') => self.show_help = true,
                KeyCode::Char('p') => {
                    self.poller.toggle_pause();
                }
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.poller
                        .set_interval(MIN_INTERVAL.max(self.poller.interval() / 2.0));
                }
                KeyCode::Char('-') => {
                    self.poller
                        .set_interval(MAX_INTERVAL.min(self.poller.interval() * 2.0));
                }
                KeyCode::Tab => {
                    self.select_view((self.active_view + 1) % VIEWS.len());
                }
                KeyCode::Char(c) if ('1'..='9').contains(&c) => {
                    self.select_view((c as u8 - b'1') as usize);
                }
                KeyCode::Char('b') => {
                    let next = match self.config.backend {
                        BackendKind::Auto => BackendKind::Vllm,
                        BackendKind::Vllm => BackendKind::Sgl,
                        BackendKind::Sgl => BackendKind::Auto,
                    };
                    self.config.backend = next;
                    self.poller.request_backend(next);
                    self.backend_toast_until = monotonic_now() + TOAST_SECONDS;
                }
                _ => {}
            }
        }
    }

    fn select_view(&mut self, idx: usize) {
        if idx >= VIEWS.len() || idx == self.active_view {
            return;
        }
        self.active_view = idx;
        self.toast_until = monotonic_now() + TOAST_SECONDS;
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let lines = area.height as i32;
        let cols = area.width as i32;
        let view = &VIEWS[self.active_view];
        let caps = self.available_caps();
        let layout = compute_layout(lines, cols, &view.root, &caps);

        let mut painter = Painter::new(frame.buffer_mut(), area, &self.theme);

        if layout.too_small {
            let msg = "terminal too small — resize to at least 62x20";
            let x = ((cols - msg.chars().count() as i32) / 2).max(0);
            painter.text(
                lines / 2,
                x,
                msg,
                self.theme.attr(Pair::Yellow, true, false),
            );
            return;
        }

        let default_snap = Snapshot::new(monotonic_now(), BackendSnapshot::default());
        let snap = self.last.as_ref().unwrap_or(&default_snap);

        for (pid, rect) in &layout.panels {
            if let Some(panel) = find_panel(pid) {
                (panel.draw)(&mut painter, *rect, snap, &self.history, 0);
            }
        }

        if layout.panels.is_empty() {
            let msg = "no panels available in this view";
            let x = ((cols - msg.chars().count() as i32) / 2).max(0);
            painter.text(lines / 2, x, msg, self.theme.attr(Pair::Dim, false, true));
        }

        if monotonic_now() < self.toast_until {
            self.draw_toast(&mut painter, cols, view.name);
        }
        if monotonic_now() < self.backend_toast_until {
            let label = format!(" backend: {} ", self.config.backend.as_str());
            let x = ((cols - label.chars().count() as i32) / 2).max(0);
            painter.text(1, x, &label, self.theme.attr(Pair::Hi, true, false));
        }
        if self.show_help {
            self.draw_help(&mut painter, lines, cols);
        }
    }

    fn draw_toast(&self, p: &mut Painter<'_>, cols: i32, name: &str) {
        let label = format!(" {}/{}  {} ", self.active_view + 1, VIEWS.len(), name);
        let x = ((cols - label.chars().count() as i32) / 2).max(0);
        p.text(0, x, &label, self.theme.attr(Pair::Hi, true, false));
    }

    fn draw_help(&self, p: &mut Painter<'_>, lines: i32, cols: i32) {
        let views: Vec<String> = VIEWS
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{} {}", i + 1, v.name))
            .collect();
        let views_joined = views.join("  ");
        let body: Vec<String> = vec![
            "tokos — keybindings".to_string(),
            String::new(),
            "  q / Esc    quit".to_string(),
            "  + / -      faster / slower refresh".to_string(),
            "  p          pause / resume polling".to_string(),
            "  b          cycle backend (auto → vllm → sglang)".to_string(),
            "  Tab        cycle to the next view".to_string(),
            format!("  1 - {}      switch view", VIEWS.len()),
            "  h / ?      toggle this help".to_string(),
            String::new(),
            format!("Views: {views_joined}"),
            "  (panels unavailable on this host drop out automatically)".to_string(),
            String::new(),
            "press any key to close".to_string(),
        ];
        let bw = body.iter().map(|s| s.chars().count()).max().unwrap_or(0) + 4;
        let bh = body.len() + 2;
        let y0 = ((lines - bh as i32) / 2).max(0);
        let x0 = ((cols - bw as i32) / 2).max(0);

        // Clear the area under the overlay so panel content doesn't bleed through.
        let blank = " ".repeat(bw);
        for row in 0..bh {
            p.text(y0 + row as i32, x0, &blank, Style::default());
        }
        let inner = p.box_(
            crate::ui::layout::Rect::new(y0, x0, bh as i32, bw as i32),
            "help",
            0,
            Pair::Title,
            "",
            Pair::Div,
        );
        for (i, s) in body.iter().enumerate() {
            if i as i32 >= inner.h {
                break;
            }
            let pair = if i == 0 { Pair::Title } else { Pair::Dim };
            p.text(
                inner.y + i as i32,
                inner.x + 1,
                s,
                p.theme.attr(pair, false, false),
            );
        }
    }
}
