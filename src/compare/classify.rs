//! Deterministic classification of a text difference.
//!
//! Only structural facts are used: common prefix/suffix lengths, exact
//! substring containment, exact repetition, and Unicode normalization
//! equality. No similarity scores.

use crate::compare::content::{common_prefix, common_suffix, excerpt};
use crate::mcp::payload::find;
use crate::model::verdict::FailureClass;
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Shape {
    Emptied,
    PrefixRetention,
    SuffixRetention,
    HeadTailRetention,
    InteriorRetention,
    Repetition,
    Normalization,
    Rewrite,
}

#[derive(Debug, Clone, Serialize)]
pub struct TextDifference {
    pub class: FailureClass,
    pub label: String,
    pub shape: Shape,
    pub tool_bytes: usize,
    pub provider_bytes: usize,
    pub first_difference: usize,
    pub retained_head_bytes: usize,
    pub retained_tail_bytes: usize,
    pub removed_bytes: usize,
    pub inserted_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inserted_excerpt: Option<String>,
    pub replacement_character_inserted: bool,
    pub provider_valid_utf8: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unicode_normalization: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repetitions: Option<usize>,
}

const REPLACEMENT: &[u8] = "\u{fffd}".as_bytes();

pub fn classify_text(expected: &[u8], actual: &[u8]) -> Option<TextDifference> {
    if expected == actual {
        return None;
    }
    let (n, m) = (expected.len(), actual.len());
    let pre = common_prefix(expected, actual);
    let suf = common_suffix(expected, actual).min(n.min(m) - pre);
    let inserted = &actual[pre..m - suf];
    let removed = n - pre - suf;
    let valid_utf8 = std::str::from_utf8(actual).is_ok();
    let replacement = find(inserted, REPLACEMENT).is_some() && find(expected, REPLACEMENT).is_none();

    let mut normalization = None;
    let mut repetitions = None;
    let shape = if m == 0 {
        Shape::Emptied
    } else if m > n && n > 0 && m % n == 0 && actual.chunks(n).all(|c| c == expected) {
        repetitions = Some(m / n);
        Shape::Repetition
    } else if m < n && (pre == m || suf == m) {
        // Pure prefix/suffix: decided without the (costlier) normalization passes.
        if pre == m {
            Shape::PrefixRetention
        } else {
            Shape::SuffixRetention
        }
    } else if let Some(form) = normalization_form(expected, actual) {
        normalization = Some(form);
        Shape::Normalization
    } else if m < n {
        if pre == m {
            Shape::PrefixRetention
        } else if suf == m {
            Shape::SuffixRetention
        } else if pre > 0 && suf > 0 {
            Shape::HeadTailRetention
        } else if pre > 0 {
            Shape::PrefixRetention
        } else if suf > 0 {
            Shape::SuffixRetention
        } else if find(expected, actual).is_some() {
            Shape::InteriorRetention
        } else {
            Shape::Rewrite
        }
    } else {
        Shape::Rewrite
    };

    let mut label = match shape {
        Shape::Emptied => "content emptied".to_owned(),
        Shape::PrefixRetention if inserted.is_empty() => "prefix retention".into(),
        Shape::PrefixRetention => "prefix retention with appended text".into(),
        Shape::SuffixRetention if inserted.is_empty() => "suffix retention".into(),
        Shape::SuffixRetention => "suffix retention with prepended text".into(),
        Shape::HeadTailRetention if inserted.is_empty() => "head/tail retention (middle deleted)".into(),
        Shape::HeadTailRetention => "head/tail retention with inserted text".into(),
        Shape::InteriorRetention => "interior retention (head and tail deleted)".into(),
        Shape::Repetition => format!("content repeated {}x", m / n),
        Shape::Normalization => format!("unicode normalization ({})", normalization.unwrap_or("?")),
        Shape::Rewrite if m > n && removed == 0 => "inserted text".into(),
        Shape::Rewrite => "content replaced".into(),
    };
    let mut class = match shape {
        Shape::Emptied
        | Shape::PrefixRetention
        | Shape::SuffixRetention
        | Shape::HeadTailRetention
        | Shape::InteriorRetention => FailureClass::Truncation,
        Shape::Repetition => FailureClass::DuplicateResult,
        Shape::Normalization | Shape::Rewrite => FailureClass::ContentMutation,
    };
    if !valid_utf8 || replacement {
        class = FailureClass::InvalidUtf8;
        label.push_str(if valid_utf8 {
            "; U+FFFD replacement character at the boundary"
        } else {
            "; provider content is not valid UTF-8"
        });
    }

    Some(TextDifference {
        class,
        label,
        shape,
        tool_bytes: n,
        provider_bytes: m,
        first_difference: pre,
        retained_head_bytes: pre,
        retained_tail_bytes: suf,
        removed_bytes: removed,
        inserted_bytes: inserted.len(),
        inserted_excerpt: (!inserted.is_empty()).then(|| excerpt(inserted, 120)),
        replacement_character_inserted: replacement,
        provider_valid_utf8: valid_utf8,
        unicode_normalization: normalization,
        repetitions,
    })
}

fn normalization_form(expected: &[u8], actual: &[u8]) -> Option<&'static str> {
    let (Ok(e), Ok(a)) = (std::str::from_utf8(expected), std::str::from_utf8(actual)) else {
        return None;
    };
    type Normalizer = fn(&str) -> String;
    let forms: [(&str, Normalizer); 4] = [
        ("NFC", |s| s.nfc().collect()),
        ("NFD", |s| s.nfd().collect()),
        ("NFKC", |s| s.nfkc().collect()),
        ("NFKD", |s| s.nfkd().collect()),
    ];
    for (name, f) in forms {
        if f(e) == a {
            return Some(name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(n: usize) -> Vec<u8> {
        (0..n).map(|i| b'a' + (i % 23) as u8).collect()
    }

    #[test]
    fn identical_is_none() {
        assert!(classify_text(b"same", b"same").is_none());
    }

    #[test]
    fn truncation_shapes() {
        let t = text(1000);
        let d = classify_text(&t, &t[..400]).unwrap();
        assert_eq!((d.shape, d.class), (Shape::PrefixRetention, FailureClass::Truncation));
        assert_eq!(d.first_difference, 400);

        let d = classify_text(&t, &t[600..]).unwrap();
        assert_eq!(d.shape, Shape::SuffixRetention);

        let ht = [&t[..300], &t[700..]].concat();
        let d = classify_text(&t, &ht).unwrap();
        assert_eq!((d.shape, d.removed_bytes, d.inserted_bytes), (Shape::HeadTailRetention, 400, 0));

        let marked = [&t[..300], b"...[truncated]...".as_slice(), &t[700..]].concat();
        let d = classify_text(&t, &marked).unwrap();
        assert_eq!(d.shape, Shape::HeadTailRetention);
        assert_eq!(d.inserted_excerpt.as_deref(), Some("...[truncated]..."));
        assert_eq!(d.class, FailureClass::Truncation);

        let d = classify_text(&t, &t[100..200]).unwrap();
        assert_eq!(d.shape, Shape::InteriorRetention);

        let d = classify_text(&t, b"").unwrap();
        assert_eq!(d.shape, Shape::Emptied);

        let appended = [&t[..500], b" [output truncated]".as_slice()].concat();
        let d = classify_text(&t, &appended).unwrap();
        assert_eq!((d.shape, d.class), (Shape::PrefixRetention, FailureClass::Truncation));
    }

    #[test]
    fn invalid_utf8_near_truncation_boundary() {
        let t = "abc\u{1f600}def".as_bytes();
        // cut inside the 4-byte emoji, then lossy-decoded
        let lossy = String::from_utf8_lossy(&t[..5]).into_owned();
        let d = classify_text(t, lossy.as_bytes()).unwrap();
        assert_eq!(d.class, FailureClass::InvalidUtf8);
        assert!(d.replacement_character_inserted);
        // raw invalid bytes
        let d = classify_text(t, &t[..5]).unwrap();
        assert_eq!(d.class, FailureClass::InvalidUtf8);
        assert!(!d.provider_valid_utf8);
        // clean cut on a character boundary is plain truncation
        let d = classify_text(t, &t[..3]).unwrap();
        assert_eq!(d.class, FailureClass::Truncation);
    }

    #[test]
    fn mutation_shapes() {
        let d = classify_text(b"payload", b"payloadpayload").unwrap();
        assert_eq!((d.shape, d.class, d.repetitions), (Shape::Repetition, FailureClass::DuplicateResult, Some(2)));

        let d = classify_text("A\u{30a}".as_bytes(), "\u{c5}".as_bytes()).unwrap();
        assert_eq!((d.shape, d.unicode_normalization), (Shape::Normalization, Some("NFC")));
        assert_eq!(d.class, FailureClass::ContentMutation);

        let d = classify_text(b"city=Boston", b"city=Chicago").unwrap();
        assert_eq!((d.shape, d.class), (Shape::Rewrite, FailureClass::ContentMutation));

        let d = classify_text(b"result", b"WARNING: result").unwrap();
        assert_eq!(d.label, "inserted text");
    }
}
