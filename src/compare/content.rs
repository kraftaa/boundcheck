//! Byte-level content helpers.

use sha2::{Digest, Sha256};

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
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
    fn excerpt_escapes_and_bounds() {
        assert_eq!(excerpt(b"a\nb\x01", 10), "a\\nb\\u{1}");
        assert_eq!(excerpt(b"abcdef", 3), "abc…");
    }
}
