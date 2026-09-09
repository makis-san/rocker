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
