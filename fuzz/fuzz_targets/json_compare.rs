//! Two JSON documents (split at the first 0xFF byte): comparison is symmetric,
//! agrees with the canonical form, and duplicate-key detection never panics.
#![no_main]
use boundarycheck::compare::json::{canonical_string, duplicate_key, first_diff};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
    let mut parts = data.splitn(2, |b| *b == 0xFF);
    let (a, b) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    for t in [a, b] {
        if let Ok(s) = std::str::from_utf8(t) {
            let _ = duplicate_key(s);
        }
    }
    let (Ok(a), Ok(b)) = (serde_json::from_slice::<Value>(a), serde_json::from_slice::<Value>(b)) else { return };
    assert!(first_diff(&a, &a).is_none());
    let same = first_diff(&a, &b).is_none();
    assert_eq!(same, first_diff(&b, &a).is_none());
    assert_eq!(same, canonical_string(&a) == canonical_string(&b));
});
