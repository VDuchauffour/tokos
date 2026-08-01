//! Shared Prometheus exposition-text helpers used by every backend collector.
//!
//! [`parse_line`], [`le_to_float`], [`parse_value`] and [`Sample`] are pure
//! functions (no I/O) so they can be unit-tested against fixtures. Both the
//! vLLM and SGLang collectors build their snapshot by iterating the parsed
//! [`Sample`]s and picking the metric names they care about.
//!
//! [`fetch_metrics_text`] does the HTTP GET with a hard size cap so a runaway
//! or hostile server can't exhaust memory.

use std::io::Read;
use std::time::Duration;

/// Cap on a single /metrics body. The endpoint is plain text and normally well
/// under 1 MB; this bounds memory if a compromised/misconfigured server (or a
/// MITM on plain HTTP) streams an unbounded body.
pub const MAX_METRICS_BYTES: usize = 16 * 1024 * 1024;

pub fn le_to_float(le: &str) -> f64 {
    match le.trim() {
        "+Inf" | "Inf" | "inf" => f64::INFINITY,
        s => s.parse::<f64>().unwrap_or(f64::INFINITY),
    }
}

pub fn parse_value(s: &str) -> f64 {
    match s.trim() {
        "+Inf" | "Inf" | "inf" | "Infinity" | "infinity" => f64::INFINITY,
        "-Inf" | "-inf" => f64::NEG_INFINITY,
        "NaN" | "nan" => f64::NAN,
        other => other.parse::<f64>().unwrap_or(0.0),
    }
}

/// One parsed metric sample: name, labels, value.
pub struct Sample<'a> {
    pub name: &'a str,
    pub labels: Vec<(&'a str, String)>,
    pub value: f64,
}

/// Parse a single exposition-text line into a [`Sample`], or `None` for
/// comments / blank lines.
pub fn parse_line(line: &str) -> Option<Sample<'_>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    // Metric name: [a-zA-Z_:][a-zA-Z0-9_:]*
    let name_end = line
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(line.len());
    let (name, rest) = line.split_at(name_end);
    if name.is_empty() {
        return None;
    }

    // Optional label block {k="v",...}
    let mut rest = rest.trim_start();
    let mut labels: Vec<(&str, String)> = Vec::new();
    if rest.starts_with('{') {
        rest = &rest[1..];
        loop {
            rest = rest.trim_start();
            if rest.starts_with('}') {
                rest = &rest[1..];
                break;
            }
            if rest.is_empty() {
                break;
            }
            // key
            let k_end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let (key, after) = rest.split_at(k_end);
            rest = after.trim_start();
            if !rest.starts_with('=') {
                break;
            }
            rest = rest[1..].trim_start();
            if !rest.starts_with('"') {
                break;
            }
            // Read a quoted string with escapes (`\"` `\\` `\n`), stopping at
            // the closing `"`. Operate on bytes but push whole chars so UTF-8
            // prompts survive intact.
            rest = &rest[1..];
            let mut val = String::new();
            let mut i = 0;
            let bytes = rest.as_bytes();
            while i < bytes.len() {
                let b = bytes[i];
                if b == b'\\' {
                    i += 1;
                    if i < bytes.len() {
                        match bytes[i] {
                            b'"' => val.push('"'),
                            b'\\' => val.push('\\'),
                            b'n' => val.push('\n'),
                            other => val.push(other as char),
                        }
                        i += 1;
                    }
                } else if b == b'"' {
                    i += 1;
                    break;
                } else {
                    let ch = rest[i..].chars().next().unwrap();
                    val.push(ch);
                    i += ch.len_utf8();
                }
            }
            rest = &rest[i..];
            labels.push((key, val));
            rest = rest.trim_start();
            if rest.starts_with(',') {
                rest = &rest[1..];
            } else if rest.starts_with('}') {
                rest = &rest[1..];
                break;
            } else {
                break;
            }
        }
    }

    // value (first whitespace-delimited token; ignore optional timestamp)
    let value_token = rest.split_whitespace().next().unwrap_or("");
    if value_token.is_empty() {
        return None;
    }
    let value = parse_value(value_token);
    Some(Sample {
        name,
        labels,
        value,
    })
}

/// Fetch the `/metrics` body with a hard size cap. Returns the text or a short
/// error string suitable for the disconnect banner.
pub fn fetch_metrics_text(url: &str, timeout: Duration) -> Result<String, String> {
    let resp = ureq::get(url)
        .set("Accept", "text/plain")
        .timeout(timeout)
        .call();
    let resp = match resp {
        Ok(r) => r,
        Err(e) => return Err(short_error(&e)),
    };
    let mut reader = resp.into_reader().take((MAX_METRICS_BYTES as u64) + 1);
    let mut buf = Vec::new();
    if let Err(e) = reader.read_to_end(&mut buf) {
        return Err(format!("{e}"));
    }
    if buf.len() > MAX_METRICS_BYTES {
        return Err(format!("/metrics body exceeded {MAX_METRICS_BYTES} bytes"));
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

pub fn short_error(e: &ureq::Error) -> String {
    // ureq splits errors into Status (non-2xx) and Transport; surface a short
    // reason for the disconnect banner.
    match e {
        ureq::Error::Status(code, _resp) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => {
            let msg = t.to_string();
            // "Transport: ..." -> strip the prefix for a terser banner.
            msg.strip_prefix("Transport: ").unwrap_or(&msg).to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comments_and_blank() {
        assert!(parse_line("# HELP foo bar").is_none());
        assert!(parse_line("   ").is_none());
    }

    #[test]
    fn parse_plain_value() {
        let s = parse_line("process_start_time_seconds 1.77995408251e+09").unwrap();
        assert_eq!(s.name, "process_start_time_seconds");
        assert!((s.value - 1.77995408251e9).abs() < 1e-3);
        assert!(s.labels.is_empty());
    }

    #[test]
    fn parse_labels_and_inf() {
        let s = parse_line(r#"vllm:time_to_first_token_seconds_bucket{le="+Inf",engine="0"} 42.0"#)
            .unwrap();
        assert_eq!(s.name, "vllm:time_to_first_token_seconds_bucket");
        assert_eq!(s.value, 42.0);
        let le = s.labels.iter().find(|(k, _)| *k == "le").unwrap();
        assert_eq!(le.1, "+Inf");
    }

    #[test]
    fn parse_escaped_quotes() {
        let s = parse_line(r#"x{prompt="he said \"hi\"\n"} 1.0"#).unwrap();
        let p = s.labels.iter().find(|(k, _)| *k == "prompt").unwrap();
        assert_eq!(p.1, "he said \"hi\"\n");
    }
}
