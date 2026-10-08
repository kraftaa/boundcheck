//! Byte-level content helpers.

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Characters that attach to the preceding character: combining marks,
/// ZERO WIDTH JOINER, variation selectors, emoji modifiers and tag characters.
pub fn extends_previous(c: char) -> bool {
    unicode_normalization::char::is_combining_mark(c)
        || c == '\u{200d}'
        || ('\u{fe00}'..='\u{fe0f}').contains(&c)
        || ('\u{e0100}'..='\u{e01ef}').contains(&c)
        || ('\u{1f3fb}'..='\u{1f3ff}').contains(&c)
        || ('\u{e0020}'..='\u{e007f}').contains(&c)
}

/// `needle` occurs in `haystack` and the occurrence is not visually modified
/// by the character right after it (e.g. a combining accent that would merge
/// with its last character). Exact bytes, no normalization.
pub fn contains_intact(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut from = 0;
    while let Some(i) = crate::mcp::payload::find(&haystack[from..], needle) {
        let end = from + i + needle.len();
        let next = std::str::from_utf8(&haystack[end..]).map(|s| s.chars().next()).unwrap_or_else(|e| {
            std::str::from_utf8(&haystack[end..end + e.valid_up_to()]).ok().and_then(|s| s.chars().next())
        });
        if !next.is_some_and(extends_previous) {
            return true;
        }
        from += i + 1;
    }
    false
}

pub fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

pub fn common_suffix(a: &[u8], b: &[u8]) -> usize {
    a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count()
}

/// Printable, bounded excerpt of possibly-invalid UTF-8 bytes.
pub fn excerpt(bytes: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i == max_chars {
            out.push('…');
            break;
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn exact_comparison() {
        assert_eq!(common_prefix(b"hello", b"hello"), 5);
        assert_eq!(common_prefix(b"hello", b"help"), 3);
        assert_eq!(common_prefix(b"hello", b"hello!"), 5);
        // NFC vs NFD e-acute differ at the byte level
        assert_eq!(common_prefix("caf\u{e9}".as_bytes(), "cafe\u{301}".as_bytes()), 3);
        assert_eq!(common_suffix(b"abcxyz", b"xyz"), 3);
    }

    #[test]
    fn containment_rejects_a_modified_last_character() {
        assert!(contains_intact(b"xx error) yy", b"error)"));
        assert!(!contains_intact("error)\u{301}".as_bytes(), b"error)"), "combining accent attaches");
        assert!(!contains_intact("hi\u{200d}\u{1f600}".as_bytes(), b"hi"), "ZWJ continues the sequence");
        assert!(contains_intact("a)\u{301} then a) ok".as_bytes(), b"a)"), "a later intact occurrence counts");
        assert!(!contains_intact(b"abc", b"abd"));
    }

    #[test]
    fn excerpt_escapes_and_bounds() {
        assert_eq!(excerpt(b"a\nb\x01", 10), "a\\nb\\u{1}");
        assert_eq!(excerpt(b"abcdef", 3), "abc…");
    }
}
