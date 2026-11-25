//! Text helpers: title validation, display-width truncation, slugs.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const MAX_TITLE_CHARS: usize = 80;

/// Validate a thread title (FR-10, FR-15): trimmed, 1..=80 characters.
pub fn validate_title(raw: &str) -> Result<String, &'static str> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("Title is required");
    }
    if t.chars().count() > MAX_TITLE_CHARS {
        return Err("Title is longer than 80 characters");
    }
    Ok(t.to_string())
}

/// Display width of a string in terminal columns.
pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to at most `max` display columns, appending `…` when cut.
/// Works on grapheme clusters so emoji and CJK are never split.
pub fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let budget = max - 1; // room for the ellipsis
    let mut out = String::new();
    let mut used = 0;
    for g in s.graphemes(true) {
        let w = width(g);
        if used + w > budget {
            break;
        }
        out.push_str(g);
        used += w;
    }
    out.push('…');
    out
}

/// Pad `s` with spaces on the right up to `cols` display columns.
pub fn pad(s: &str, cols: usize) -> String {
    let w = width(s);
    if w >= cols {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(cols - w))
    }
}

/// A filesystem- and branch-safe slug: lowercase ASCII alphanumerics and `-`.
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let mut s: String = out.trim_end_matches('-').chars().take(40).collect();
    s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "thread".to_string() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_validation() {
        assert!(validate_title("").is_err());
        assert!(validate_title("   \t ").is_err());
        assert_eq!(validate_title("  hello ").unwrap(), "hello");
        assert!(validate_title(&"a".repeat(80)).is_ok());
        assert!(validate_title(&"a".repeat(81)).is_err());
        // 80 multibyte chars is fine: the limit is characters, not bytes
        assert!(validate_title(&"日".repeat(80)).is_ok());
    }

    #[test]
    fn truncate_ascii() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(width(&truncate("hello world", 8)), 8);
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abc", 1), "…");
    }

    #[test]
    fn truncate_cjk() {
        // each CJK char is 2 columns wide
        let t = truncate("日本語のタイトル", 7);
        assert_eq!(t, "日本語…");
        assert!(width(&t) <= 7);
    }

    #[test]
    fn truncate_emoji() {
        let s = "🚀🚀🚀 launch plan";
        let t = truncate(s, 6);
        assert!(width(&t) <= 6);
        assert!(t.ends_with('…'));
        // family emoji (ZWJ sequence) is never split
        let fam = "👨‍👩‍👧 family";
        let t = truncate(fam, 4);
        assert!(t.starts_with("👨‍👩‍👧") || t == "…");
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("Stripe vs Paddle pricing!"), "stripe-vs-paddle-pricing");
        assert_eq!(slugify("  --DB schema--  "), "db-schema");
        assert_eq!(slugify("日本語"), "thread");
        assert!(slugify(&"x".repeat(100)).len() <= 40);
    }
}
