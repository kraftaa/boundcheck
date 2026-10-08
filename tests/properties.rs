use boundarycheck::{
    compare::classify::{classify_text, Shape},
    mcp::payload::{find, generate, sentinel, sentinel_count},
    model::verdict::FailureClass,
    scenario::{self, CALL_1},
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn parameterized_payloads_keep_exact_size_and_sentinel_offsets(
        size in scenario::LARGE_TEXT_MIN..=256 * 1024usize,
    ) {
        let id = scenario::large_text_id(size);
        let payload = generate("BC_RUN_PROPERTY", &id, CALL_1).expect("valid parameterized scenario");
        let bytes = payload.text.as_bytes();

        prop_assert_eq!(bytes.len(), size);
        prop_assert_eq!(sentinel_count(CALL_1, bytes), 5);
        prop_assert_eq!(find(bytes, sentinel(CALL_1, 50).as_bytes()), Some(size / 2));
        prop_assert!(payload.text.starts_with(&sentinel(CALL_1, 0)));
        prop_assert!(payload.text.ends_with(&sentinel(CALL_1, 100)));
    }

    #[test]
    fn every_valid_parameterized_size_round_trips(
        size in scenario::LARGE_TEXT_MIN..=scenario::LARGE_TEXT_MAX,
    ) {
        let id = scenario::large_text_id(size);
        prop_assert_eq!(scenario::large_text_size(&id), Some(size));
    }

    #[test]
    fn every_strict_prefix_is_classified_as_truncation(
        expected in prop::collection::vec(0x20u8..0x7fu8, 1..4096),
        raw_keep in any::<usize>(),
    ) {
        let keep = raw_keep % expected.len();
        let difference = classify_text(&expected, &expected[..keep]).expect("strict prefix differs");

        prop_assert_eq!(difference.class, FailureClass::Truncation);
        prop_assert_eq!(difference.shape, if keep == 0 { Shape::Emptied } else { Shape::PrefixRetention });
        prop_assert_eq!(difference.provider_bytes, keep);
        prop_assert_eq!(difference.first_difference, keep);
    }
}
