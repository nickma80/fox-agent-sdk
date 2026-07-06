//! Safe string truncation utilities.
//!
//! These functions guarantee that slicing never panics on a non-UTF-8
//! byte boundary — unlike raw `&s[..n]` indexing.  Use them instead of
//! manual char-iteration everywhere you need to limit output length.

/// Return a prefix of `s` containing **at most `max_chars` characters**.
///
/// Finds the byte offset of the `max_chars`-th character using
/// [`str::char_indices`], so slicing `&s[..byte_idx]` is always on a
/// valid UTF-8 boundary.
///
/// # Examples
///
/// ```
/// use fox_agent_core::truncate_to_chars;
///
/// // ASCII — 3 chars = 3 bytes
/// assert_eq!(truncate_to_chars("hello", 3), "hel");
///
/// // CJK — each char is 3 bytes
/// assert_eq!(truncate_to_chars("你好世界", 2), "你好");
///
/// // max_chars >= total chars — returns entire string
/// assert_eq!(truncate_to_chars("hi", 10), "hi");
/// ```
#[inline]
pub fn truncate_to_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s, // fewer than max_chars characters
    }
}

/// Return a prefix of `s` containing **at most `max_bytes` bytes**,
/// ending on a valid UTF-8 character boundary.
///
/// If `max_bytes` falls in the middle of a multi-byte character we
/// walk backward to the nearest boundary, so the returned slice is
/// always valid UTF-8.
///
/// # Examples
///
/// ```
/// use fox_agent_core::truncate_to_bytes;
///
/// // '好' is bytes [0..3), so max_bytes=2 falls inside it
/// let s = "你好";
/// assert_eq!(truncate_to_bytes(s, 2), "");  // walked back to 0
/// assert_eq!(truncate_to_bytes(s, 3), "你");
/// assert_eq!(truncate_to_bytes(s, 6), "你好");
/// ```
#[inline]
pub fn truncate_to_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Format a string, truncating to at most `max_chars` characters.
///
/// Returns `(prefix, suffix)` where `suffix` is non-empty only when
/// truncation occurred.  Useful for producing messages like
/// `"Hello...[truncated 5 chars]"`.
///
/// # Examples
///
/// ```
/// use fox_agent_core::format_truncated;
///
/// let (text, overflow) = format_truncated("Hello, world!", 5);
/// assert_eq!(text, "Hello");
/// assert!(!overflow.is_empty());
///
/// let (text, overflow) = format_truncated("hi", 10);
/// assert_eq!(text, "hi");
/// assert!(overflow.is_empty());
/// ```
#[inline]
pub fn format_truncated(s: &str, max_chars: usize) -> (String, String) {
    if s.chars().count() <= max_chars {
        return (s.to_string(), String::new());
    }
    let truncated = truncate_to_chars(s, max_chars);
    let remaining = s.chars().count() - truncated.chars().count();
    (truncated.to_string(), format!("...[truncated {} chars]", remaining))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_to_chars_ascii() {
        assert_eq!(truncate_to_chars("abc", 2), "ab");
        assert_eq!(truncate_to_chars("abc", 10), "abc");
        assert_eq!(truncate_to_chars("", 5), "");
    }

    #[test]
    fn truncate_to_chars_cjk() {
        // '你' = 3 bytes, '好' = 3 bytes
        assert_eq!(truncate_to_chars("你好世界", 0), "");
        assert_eq!(truncate_to_chars("你好世界", 2), "你好");
        assert_eq!(truncate_to_chars("你好世界", 4), "你好世界");
    }

    #[test]
    fn truncate_to_chars_mixed() {
        // 'a' = 1 byte, '好' = 3 bytes
        assert_eq!(truncate_to_chars("a好b", 2), "a好");
        assert_eq!(truncate_to_chars("a好b", 1), "a");
    }

    #[test]
    fn truncate_to_bytes_boundary() {
        // '状' = bytes [0..3)
        assert_eq!(truncate_to_bytes("状态", 1), "");  // inside '状'
        assert_eq!(truncate_to_bytes("状态", 3), "状");
        assert_eq!(truncate_to_bytes("状态", 6), "状态");
    }

    #[test]
    fn truncate_to_bytes_ascii() {
        assert_eq!(truncate_to_bytes("hello", 3), "hel");
        assert_eq!(truncate_to_bytes("hello", 10), "hello");
    }

    #[test]
    fn format_truncated_no_truncation() {
        let (text, overflow) = format_truncated("hello", 10);
        assert_eq!(text, "hello");
        assert!(overflow.is_empty());
    }

    #[test]
    fn format_truncated_with_truncation() {
        let (text, overflow) = format_truncated("hello world", 5);
        assert_eq!(text, "hello");
        assert!(overflow.contains("truncated"));
    }
}
