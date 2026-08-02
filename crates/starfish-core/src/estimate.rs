//! Best-effort token estimation.
//!
//! Hyperagent does not expose token counts, so Starfish estimates them and is
//! honest about it (the UI and docs label every count as an estimate; the wire
//! responses stay spec-shaped so strict clients don't break).
//!
//! Heuristic: ~4 characters per token for typical English/code mixes, plus a
//! small per-message overhead, mirroring the widely used rule of thumb.

/// Estimate tokens for a single string.
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    // chars/4, rounded up, minimum 1 for non-empty text.
    let chars = text.chars().count() as u64;
    chars.div_ceil(4).max(1)
}

/// Estimate tokens for a conversation: per-part text plus a fixed overhead per
/// message (role markers, separators) similar to chat-format overheads.
pub fn estimate_conversation_tokens(parts: &[&str]) -> u64 {
    const PER_MESSAGE_OVERHEAD: u64 = 4;
    parts
        .iter()
        .map(|p| estimate_tokens(p) + PER_MESSAGE_OVERHEAD)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn short_is_at_least_one() {
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("hi"), 1);
    }

    #[test]
    fn scales_by_chars() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        let s = "x".repeat(400);
        assert_eq!(estimate_tokens(&s), 100);
    }

    #[test]
    fn conversation_adds_overhead() {
        assert_eq!(estimate_conversation_tokens(&["abcd", "abcd"]), 2 + 8);
    }
}
