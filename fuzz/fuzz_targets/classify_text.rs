//! The classifier must report "no difference" exactly when the bytes are equal,
//! and its reported facts must be internally consistent.
#![no_main]
use boundarycheck::compare::classify::classify_text;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let split = data.first().map_or(0, |b| *b as usize % (data.len().max(1)));
    let (e, a) = data.split_at(split);
    match classify_text(e, a) {
        None => assert_eq!(e, a),
        Some(d) => {
            assert_ne!(e, a);
            assert_eq!((d.tool_bytes, d.provider_bytes), (e.len(), a.len()));
            assert!(d.retained_head_bytes + d.retained_tail_bytes <= e.len().min(a.len()));
        }
    }
    assert!(classify_text(e, e).is_none());
});
