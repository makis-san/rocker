//! Small human-readable formatters for the container screen. Byte counts and
//! rates are data, so callers set them in mono.

/// `912 B`, `4.0 KiB`, `338 MiB`, `1.6 GiB` — three significant figures, binary
/// units.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    let precision = if v >= 100.0 {
        0
    } else if v >= 10.0 {
        1
    } else {
        2
    };
    format!("{v:.*} {}", precision, UNITS[u])
}

/// A throughput, e.g. `1.6 MiB/s`. Sub-1 B/s reads as `0 B/s`.
pub fn rate(bytes_per_s: f64) -> String {
    let n = bytes_per_s.max(0.0).round() as u64;
    format!("{}/s", bytes(n))
}

/// `2026-09-08T14:03:11.482Z` → `2026-09-08 14:03`. Anything unrecognised is
/// returned trimmed but otherwise untouched.
pub fn timestamp(rfc3339: &str) -> String {
    let s = rfc3339.trim();
    match s.split_once('T') {
        Some((date, rest)) if rest.len() >= 5 => format!("{date} {}", &rest[..5]),
        _ => s.to_string(),
    }
}

/// `2026-09-10T14:03:11.482331Z` → `14:03:11`, the clock time a log line
/// carries. Anything unrecognised falls back to its first 19 characters.
pub fn log_time(rfc3339: &str) -> String {
    match rfc3339.split_once('T') {
        Some((_, rest)) if rest.len() >= 8 => rest[..8].to_string(),
        _ => rfc3339.chars().take(19).collect(),
    }
}

/// A coarse "how long ago" for a Unix-millis timestamp: `just now`, `5m ago`,
/// `3h ago`, `2d ago`. A `ts_ms` in the future (clock skew) reads as `just now`.
pub fn ago(ts_ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let secs = now.saturating_sub(ts_ms) / 1000;
    if secs < 45 {
        "just now".to_string()
    } else if secs < 5400 {
        format!("{}m ago", (secs + 30) / 60)
    } else if secs < 172_800 {
        format!("{}h ago", (secs + 1800) / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}
