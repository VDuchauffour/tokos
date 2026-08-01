//! Tail the vLLM server's log into a feed of inference requests.
//!
//! vLLM's `/metrics` is aggregate-only and its request prompts are not logged
//! unless the server runs with `--enable-log-requests`. With that flag vLLM
//! emits a request-log line per inference call, e.g.::
//!
//! ```text
//! Received request chatcmpl-abc: prompt: 'Hello', params: SamplingParams(... max_tokens=100 ...)
//! ```
//!
//! So this collector parses those lines (request id, max_tokens and, on vLLM
//! >= 0.11.3 via PR #29227, the prompt text truncated by `max_log_len`) into a
//! > rolling [`MergedLogEntry`] buffer. Uvicorn access lines (the HTTP envelope
//! > and status) are ignored — only the actual inference requests are shown.
//!
//! The source is either a file (`--log-file`) or the stdout of a streaming
//! command such as `docker logs -f` (`--docker`). A background thread follows
//! it; the UI reads [`AccessLogTailer::merged_log`] each frame.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use regex::Regex;
use tracing::instrument;

use crate::state::{MergedLogEntry, epoch_now};

// vLLM `--enable-log-requests` log lines.
// On vLLM >= 0.11.3 (PR #29227) the prompt is present at INFO level.
// vLLM formats with `prompt: %r` which adds quotes:
//   "Received request <id>: prompt: '<text>', params: SamplingParams(... max_tokens=<n>, ...)"
// On older vLLM the prompt is omitted (only at DEBUG level):
//   "Received request <id>: params: SamplingParams(... max_tokens=<n>, ...)"
// Two regexes: try the new format first (with prompt), then the old (without).
// `(?s)` is needed because vLLM prompts (especially chat-template'd ones)
// contain actual newline characters that `.` wouldn't match otherwise.
static NEW_REQUEST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)Received request (?P<id>\S+): prompt: '(?P<prompt>.*?)', params: SamplingParams\(.*?max_tokens=(?P<tok>\d+)",
    )
    .unwrap()
});
static OLD_REQUEST_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Received request (?P<id>\S+): params: SamplingParams\(.*?max_tokens=(?P<tok>\d+)")
        .unwrap()
});

/// Max prompt length we display in the feed (truncated with `…`).
/// vLLM itself truncates at `max_log_len` (default 1000); we truncate again to
/// keep the terminal column manageable.
pub const MAX_PROMPT_DISPLAY: usize = 30;

// vLLM request ids are "<prefix>-<hex>", and the prefix identifies the endpoint
// that produced them. We recover the endpoint from the prefix to give
// request-log-driven rows a meaningful endpoint column.
fn endpoint_for_prefix(prefix: &str) -> Option<&'static str> {
    match prefix {
        "chatcmpl" => Some("/v1/chat/completions"),
        "cmpl" => Some("/v1/completions"),
        "embd" => Some("/v1/embeddings"),
        "pool" => Some("/pooling"),
        "score" => Some("/score"),
        "rerank" => Some("/rerank"),
        "classify" => Some("/classify"),
        _ => None,
    }
}

/// Best-effort map a vLLM request id to the endpoint that produced it.
pub fn endpoint_for_request_id(request_id: &str) -> String {
    let prefix = request_id.split('-').next().unwrap_or("");
    if prefix.is_empty() {
        "?".to_string()
    } else {
        endpoint_for_prefix(prefix)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("/{prefix}"))
    }
}

struct TailerState {
    entries: VecDeque<MergedLogEntry>,
    error: Option<String>,
}

/// Follows a log source and parses vLLM request-log lines into a buffer.
///
/// Starts at the *end* of the source (like `tail -f`) so only requests observed
/// while running are shown. Any failure (missing file, no `docker`, container
/// gone) is surfaced via [`AccessLogTailer::error`] rather than crashing the UI.
pub struct AccessLogTailer {
    file: Option<String>,
    docker: Option<String>,
    maxlen: usize,
    state: Arc<Mutex<TailerState>>,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl AccessLogTailer {
    pub fn new(file: Option<String>, docker: Option<String>, maxlen: usize) -> Self {
        Self {
            file,
            docker,
            maxlen,
            state: Arc::new(Mutex::new(TailerState {
                entries: VecDeque::new(),
                error: None,
            })),
            stop: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            handle: Mutex::new(None),
        }
    }

    pub fn source_label(&self) -> String {
        if let Some(d) = &self.docker {
            format!("docker {d}")
        } else if let Some(f) = &self.file {
            Path::new(f)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            "—".to_string()
        }
    }

    pub fn error(&self) -> Option<String> {
        self.state.lock().unwrap().error.clone()
    }

    /// Return merged entries, newest first (at most `n`).
    pub fn merged_log(&self, n: Option<usize>) -> Vec<MergedLogEntry> {
        let mut items: Vec<MergedLogEntry> =
            self.state.lock().unwrap().entries.iter().cloned().collect();
        items.reverse();
        match n {
            Some(n) => items.into_iter().take(n).collect(),
            None => items,
        }
    }

    /// Parse one log line into a feed entry (or ignore it). Public so tests can
    /// drive the parser without starting the follower thread.
    pub fn ingest(&self, line: &str) {
        ingest_line(&self.state, line, self.maxlen);
    }

    /// Spawn the follower thread. Idempotent.
    pub fn start(&self) {
        let mut handle_guard = self.handle.lock().unwrap();
        if handle_guard.is_some() {
            return;
        }
        let file = self.file.clone();
        let docker = self.docker.clone();
        let maxlen = self.maxlen;
        let state = self.state.clone();
        let stop = self.stop.clone();
        let child = self.child.clone();
        *handle_guard = Some(thread::spawn(move || {
            if let Some(container) = docker {
                follow_command(container, state, stop, child, maxlen);
            } else if let Some(path) = file {
                follow_file(path, state, stop, maxlen);
            }
        }));
    }

    /// Signal the follower thread to stop and best-effort kill any spawned
    /// `docker logs` child so it doesn't outlive the process.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut c) = guard.take()
        {
            let _ = c.kill();
            let _ = c.wait();
        }
        if let Ok(mut handle_guard) = self.handle.lock()
            && let Some(h) = handle_guard.take()
        {
            let _ = h.join();
        }
    }
}

fn set_error(state: &Arc<Mutex<TailerState>>, msg: String) {
    tracing::error!(error = %msg, "log tailer error");
    state.lock().unwrap().error = Some(msg);
}

fn clear_error(state: &Arc<Mutex<TailerState>>) {
    state.lock().unwrap().error = None;
}

#[instrument(skip(state, stop), fields(source = %path))]
fn follow_file(path: String, state: Arc<Mutex<TailerState>>, stop: Arc<AtomicBool>, maxlen: usize) {
    while !stop.load(Ordering::Relaxed) {
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                set_error(&state, e.to_string());
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        clear_error(&state);
        let mut reader = BufReader::new(file);
        // Start at the end, like `tail -f`.
        let _ = reader.get_mut().seek(SeekFrom::End(0));
        loop {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    thread::sleep(Duration::from_millis(200));
                    // Detect truncation / rotation -> reopen.
                    let reopen = match (fs::metadata(&path), reader.stream_position()) {
                        (Ok(meta), Ok(pos)) => meta.len() < pos,
                        _ => true,
                    };
                    if reopen {
                        break;
                    }
                }
                Ok(_) => {
                    ingest_line(&state, line.trim_end_matches(['\n', '\r']), maxlen);
                }
                Err(_) => break,
            }
        }
    }
}

#[instrument(skip(state, stop, child_slot), fields(container = %container))]
fn follow_command(
    container: String,
    state: Arc<Mutex<TailerState>>,
    stop: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
    maxlen: usize,
) {
    let mut cmd = Command::new("docker");
    cmd.args(["logs", "-f", "--tail", "0", &container])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = if e.kind() == std::io::ErrorKind::NotFound {
                "command not found: docker".to_string()
            } else {
                format!("`docker logs -f {container}`: {e}")
            };
            set_error(&state, msg);
            return;
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *child_slot.lock().unwrap() = Some(child);

    let mut readers = Vec::new();
    if let Some(out) = stdout {
        let st = state.clone();
        let sp = stop.clone();
        readers.push(thread::spawn(move || {
            read_pipe(BufReader::new(out), st, sp, maxlen)
        }));
    }
    if let Some(err) = stderr {
        let st = state.clone();
        let sp = stop.clone();
        readers.push(thread::spawn(move || {
            read_pipe(BufReader::new(err), st, sp, maxlen)
        }));
    }

    // Poll the child: on stop, kill it; on natural exit, reap and report.
    loop {
        if stop.load(Ordering::Relaxed) {
            if let Ok(mut guard) = child_slot.lock()
                && let Some(mut c) = guard.take()
            {
                let _ = c.kill();
                let _ = c.wait();
            }
            break;
        }
        let status = {
            let mut guard = child_slot.lock().unwrap();
            match guard.as_mut() {
                Some(c) => c.try_wait().ok().flatten(),
                None => break,
            }
        };
        if let Some(status) = status {
            // Reap fully.
            if let Ok(mut guard) = child_slot.lock()
                && let Some(mut c) = guard.take()
            {
                let _ = c.wait();
            }
            if !stop.load(Ordering::Relaxed) && !status.success() {
                set_error(
                    &state,
                    format!("`docker logs -f {container}` exited ({status})"),
                );
            }
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    for r in readers {
        let _ = r.join();
    }
}

fn read_pipe<R: BufRead>(
    mut reader: R,
    state: Arc<Mutex<TailerState>>,
    stop: Arc<AtomicBool>,
    maxlen: usize,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF (child exited / pipe closed)
            Ok(_) => {
                ingest_line(&state, line.trim_end_matches(['\n', '\r']), maxlen);
            }
            Err(_) => break,
        }
    }
}

fn ingest_line(state: &Arc<Mutex<TailerState>>, line: &str, maxlen: usize) {
    let Some(caps) = NEW_REQUEST_RE
        .captures(line)
        .or_else(|| OLD_REQUEST_RE.captures(line))
    else {
        return;
    };
    let id = caps.name("id").map(|m| m.as_str().to_string());
    let tok = caps
        .name("tok")
        .and_then(|m| m.as_str().parse::<i64>().ok());
    let prompt = caps
        .name("prompt")
        .map(|m| m.as_str().to_string())
        .filter(|p| !p.is_empty());
    let prompt_chars = prompt.as_ref().map(|p| p.chars().count() as i64);

    let mut e = MergedLogEntry::new(epoch_now());
    e.path = endpoint_for_request_id(id.as_deref().unwrap_or(""));
    e.request_id = id;
    e.max_tokens = tok;
    e.prompt = prompt;
    e.prompt_chars = prompt_chars;

    let mut st = state.lock().unwrap();
    st.entries.push_back(e);
    while st.entries.len() > maxlen {
        st.entries.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::panels::truncate_prompt;

    fn tailer() -> AccessLogTailer {
        AccessLogTailer::new(Some("/nonexistent".to_string()), None, 200)
    }

    #[test]
    fn access_lines_are_ignored() {
        let t = tailer();
        t.ingest(r#"9.9.9.9:8 - "POST /v1/chat/completions HTTP/1.1" 200 OK"#);
        t.ingest(r#"1.1.1.1:2 - "GET /metrics HTTP/1.1" 200 OK"#);
        assert!(t.merged_log(None).is_empty());
    }

    #[test]
    fn prompt_parse_vllm_new_format() {
        let t = tailer();
        t.ingest(
            "Received request chatcmpl-abc: prompt: 'Hello, how are you?', \
             params: SamplingParams(n=1, max_tokens=100), lora_request: None.",
        );
        let entries = t.merged_log(None);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.request_id.as_deref(), Some("chatcmpl-abc"));
        assert_eq!(e.max_tokens, Some(100));
        assert_eq!(e.path, "/v1/chat/completions");
        assert_eq!(e.status, None);
        assert_eq!(e.prompt.as_deref(), Some("Hello, how are you?"));
    }

    #[test]
    fn prompt_parse_vllm_old_format() {
        let t = tailer();
        t.ingest(
            "Received request chatcmpl-xyz: \
             params: SamplingParams(n=1, max_tokens=50), lora_request: None.",
        );
        let entries = t.merged_log(None);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.request_id.as_deref(), Some("chatcmpl-xyz"));
        assert_eq!(e.max_tokens, Some(50));
        assert_eq!(e.path, "/v1/chat/completions");
        assert_eq!(e.prompt, None);
    }

    #[test]
    fn endpoint_for_request_id_values() {
        assert_eq!(
            endpoint_for_request_id("chatcmpl-abc"),
            "/v1/chat/completions"
        );
        assert_eq!(endpoint_for_request_id("cmpl-abc"), "/v1/completions");
        assert_eq!(endpoint_for_request_id("embd-abc"), "/v1/embeddings");
        assert_eq!(endpoint_for_request_id("weird-abc"), "/weird");
    }

    #[test]
    fn truncate_prompt_values() {
        assert_eq!(truncate_prompt(Some("short"), 30), "short");
        assert_eq!(truncate_prompt(Some(&"a".repeat(30)), 30), "a".repeat(30));
        assert_eq!(
            truncate_prompt(Some(&"a".repeat(31)), 30),
            format!("{}…", "a".repeat(29))
        );
        assert_eq!(truncate_prompt(None, 30), "");
        assert_eq!(truncate_prompt(Some(""), 30), "");
    }

    #[test]
    fn max_prompt_display_constant() {
        assert!((10..=60).contains(&MAX_PROMPT_DISPLAY));
    }
}

#[cfg(test)]
mod follow_tests {
    use std::io::Write;
    use std::time::Instant;

    use super::*;

    #[test]
    fn tails_appended_lines() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("tokos_tail_test_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // pre-existing content (should be skipped — tail starts at EOF)
        std::fs::write(&path, "old line\n").unwrap();

        let t = AccessLogTailer::new(Some(path.to_string_lossy().into_owned()), None, 200);
        t.start();
        std::thread::sleep(Duration::from_millis(300));

        // append a valid request-log line
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(
            f,
            "Received request chatcmpl-tail1: prompt: 'hi', params: SamplingParams(n=1, max_tokens=42), lora_request: None."
        )
        .unwrap();
        f.flush().unwrap();
        drop(f);

        let start = Instant::now();
        let mut entries = Vec::new();
        while start.elapsed() < Duration::from_secs(3) {
            entries = t.merged_log(None);
            if !entries.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        t.stop();
        let _ = std::fs::remove_file(&path);

        assert_eq!(entries.len(), 1, "expected 1 entry, got {:?}", entries);
        assert_eq!(entries[0].request_id.as_deref(), Some("chatcmpl-tail1"));
        assert_eq!(entries[0].max_tokens, Some(42));
    }
}
