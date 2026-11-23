//! Small shared helpers: ids, time, tokens, text trimming.

use rand::Rng;
use sha2::{Digest, Sha256};

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A short, sortable id: millisecond time + randomness, e.g. "th_0190f3a2b1c4_k3x9p2".
pub fn id(prefix: &str) -> String {
    format!("{prefix}_{:x}{}", now_ms(), rand_str(6))
}

pub fn rand_str(n: usize) -> String {
    const A: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut r = rand::thread_rng();
    (0..n).map(|_| A[r.gen_range(0..A.len())] as char).collect()
}

/// A long random secret for tokens.
pub fn secret() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill(&mut b);
    hex(&b)
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

/// Compare secrets without leaking their length of common prefix through timing.
pub fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Cut text to at most `max` characters, on a char boundary, marking the cut.
pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(" …[trimmed]");
    out
}

/// "Fix the Safari export button!" -> "fix-the-safari-export-button"
pub fn slug(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
        if out.len() >= max {
            break;
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() { "task".into() } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs() {
        assert_eq!(slug("Fix the Safari export button!", 40), "fix-the-safari-export-button");
        assert_eq!(slug("!!!", 10), "task");
    }
    #[test]
    fn clipping() {
        assert_eq!(clip("hello", 10), "hello");
        assert!(clip("hello world", 5).starts_with("hello"));
    }
}
