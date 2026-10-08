//! Deterministic payload generation. Payloads depend only on
//! (run_id, scenario_id, call_id): no clocks, no randomness.

use crate::scenario::{self, CALL_1};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Payload {
    pub text: String,
    pub structured: Option<Value>,
    pub is_error: bool,
}

/// Positional sentinels embedded in large-text payloads.
pub const SENTINEL_POINTS: [(u32, &str); 5] = [(0, "start"), (25, "25%"), (50, "middle"), (75, "75%"), (100, "end")];

pub fn sentinel(call_id: &str, pct: u32) -> String {
    format!("[[BC_SENTINEL:{pct:03}:{call_id}]]")
}

/// Labels of sentinels present in `expected` but absent from `actual` (exact substring search).
pub fn missing_sentinels(call_id: &str, expected: &[u8], actual: &[u8]) -> Vec<&'static str> {
    SENTINEL_POINTS
        .iter()
        .filter(|(pct, _)| {
            let s = sentinel(call_id, *pct);
            contains(expected, s.as_bytes()) && !contains(actual, s.as_bytes())
        })
        .map(|(_, label)| *label)
        .collect()
}

/// Number of sentinels the payload carries (0 for non-large-text payloads).
pub fn sentinel_count(call_id: &str, expected: &[u8]) -> usize {
    SENTINEL_POINTS.iter().filter(|(pct, _)| contains(expected, sentinel(call_id, *pct).as_bytes())).count()
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub fn generate(run_id: &str, scenario_id: &str, call_id: &str) -> Option<Payload> {
    let def = scenario::find(scenario_id)?;
    if !def.has_call(call_id) {
        return None;
    }
    let head = format!("{run_id}|{scenario_id}|{call_id}");
    let text = |text: String| Some(Payload { text, structured: None, is_error: false });
    match scenario_id {
        "exact-text" => {
            text(format!("{head}|exact text: \"quoted\" back\\slash\ttab\u{1}ctl\u{2028}ls caf\u{e9} \u{2713}\n"))
        }
        "large-text-1k" => text(large_text(&head, call_id, 1024)),
        "large-text-50k" => text(large_text(&head, call_id, 50 * 1024)),
        "large-text-100k" => text(large_text(&head, call_id, 100 * 1024)),
        id if scenario::large_text_size(id).is_some() => {
            text(large_text(&head, call_id, scenario::large_text_size(id).unwrap()))
        }
        "concurrent-two-tools" => {
            let city = if call_id == CALL_1 { "Boston" } else { "Chicago" };
            text(format!("{head}|city={city}"))
        }
        "sequential-history" => {
            let step = if call_id == CALL_1 { 1 } else { 2 };
            text(format!("{head}|step={step}|history must keep this result exactly once"))
        }
        "retry-429" => text(format!("{head}|retry payload: unchanged across HTTP retries \u{2713}")),
        "replay-history" => text(format!("{head}|replay payload: unchanged across history replay \u{2713}")),
        "persistence-resume" => {
            text(format!("{head}|persisted payload: unchanged across process restart and resume \u{2713}"))
        }
        "unicode-boundaries" => text(format!("{head}|{}", unicode_text())),
        "mcp-error" => Some(Payload {
            text: format!("{head}|error: deterministic tool failure (code BC_E_042)"),
            structured: None,
            is_error: true,
        }),
        "structured-json" => {
            let value = structured_json(run_id, scenario_id, call_id);
            Some(Payload {
                text: serde_json::to_string(&value).expect("serialize"),
                structured: Some(value),
                is_error: false,
            })
        }
        _ => None,
    }
}

fn unicode_text() -> String {
    [
        "nfc=\u{c5}",          // Å precomposed
        "nfd=A\u{30a}",        // A + combining ring
        "e\u{301}\u{301}",     // stacked combining acute
        "\u{301}leading-mark", // combining mark with no base
        "zwj=\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
        "flag=\u{1f1fa}\u{1f1f8}",
        "vs16=\u{2764}\u{fe0f}",
        "rtl=\u{5e9}\u{5dc}\u{5d5}\u{5dd}",
        "deva=\u{915}\u{94d}\u{937}\u{93f}",
        "hangul-jamo=\u{1100}\u{1161}\u{11a8}",
        "astral=\u{1d11e}",
        "bom-mid=\u{feff}",
        "zwnj=a\u{200c}b",
        "fullwidth=\u{ff21}\u{ff22}",
        "ligature=\u{fb01}",
    ]
    .join("|")
}

fn structured_json(run_id: &str, scenario_id: &str, call_id: &str) -> Value {
    // Written as text so that number tokens are preserved exactly
    // (serde_json's arbitrary_precision keeps them verbatim).
    let doc = format!(
        r#"{{"run_id":"{run_id}","scenario":"{scenario_id}","call_id":"{call_id}","kind":"structured",
"order":{{"id":9007199254740993,"big":12345678901234567890,"price":19.990,"ratio":0.1,"exp":1e-7,"neg":-0.0,"count":0}},
"flags":{{"active":true,"deleted":false,"note":null}},
"items":[{{"sku":"A-1","qty":1,"tags":["x","y"]}},{{"sku":"B-2","qty":2,"tags":[]}},
{{"sku":"C-3","qty":3,"tags":["z"],"meta":{{"depth":{{"level":{{"leaf":"deep {e_acute}{ls}"}}}}}}}}],
"empty_object":{{}},"empty_array":[],"unicode_key_{e_acute}":"value","escape":"tab\tquote\"slash\\"}}"#,
        e_acute = '\u{e9}',
        ls = '\u{2028}'
    );
    serde_json::from_str(&doc).expect("static structured payload is valid JSON")
}

/// Build a text of exactly `total` bytes with sentinels starting at 0%, 25%,
/// 50% and 75% offsets and ending exactly at 100%.
fn large_text(head: &str, call_id: &str, total: usize) -> String {
    let mut out = String::with_capacity(total);
    let mut line = 0u32;
    out.push_str(&sentinel(call_id, 0));
    out.push('\n');
    out.push_str(head);
    out.push_str(&format!("|bytes={total}\n"));
    for pct in [25u32, 50, 75] {
        let target = total * pct as usize / 100;
        fill(&mut out, target, call_id, &mut line);
        out.push_str(&sentinel(call_id, pct));
        out.push('\n');
    }
    let end = sentinel(call_id, 100);
    fill(&mut out, total - end.len(), call_id, &mut line);
    out.push_str(&end);
    debug_assert_eq!(out.len(), total);
    out
}

/// Append filler lines (mixed 1-4 byte UTF-8) until `out` is exactly `target` bytes.
fn fill(out: &mut String, target: usize, call_id: &str, line: &mut u32) {
    loop {
        let next = format!("{:06} {call_id} quick brown fox \u{b7} caf\u{e9} \u{2192} \u{1f600} jumps\n", *line);
        if out.len() + next.len() > target {
            break;
        }
        out.push_str(&next);
        *line += 1;
    }
    while out.len() < target {
        out.push('-');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::CALL_2;

    #[test]
    fn parameterized_large_text() {
        assert_eq!(scenario::large_text_size("large-text:65537"), Some(65_537));
        assert_eq!(scenario::large_text_size("large-text:64k"), Some(65_536));
        assert_eq!(scenario::large_text_size("large-text:5M"), Some(5 * 1024 * 1024));
        for bad in
            ["large-text:", "large-text:1023", "large-text:9m", "large-text:-5", "large-text:1.5k", "large-text:k"]
        {
            assert_eq!(scenario::large_text_size(bad), None, "{bad}");
        }
        let def = scenario::find("large-text:1m").unwrap();
        assert_eq!(def.id, "large-text:1048576");
        assert!(std::ptr::eq(def, scenario::find("large-text:1048576").unwrap()), "defs are cached");
        for n in [1024, 65_535, 65_537, 1_048_577] {
            let p = generate("BC_RUN_000001", &scenario::large_text_id(n), CALL_1).unwrap();
            assert_eq!(p.text.len(), n);
            assert_eq!(sentinel_count(CALL_1, p.text.as_bytes()), 5);
            assert_eq!(find(p.text.as_bytes(), sentinel(CALL_1, 50).as_bytes()), Some(n / 2));
        }
    }

    #[test]
    fn large_text_sizes_and_sentinels() {
        for (id, n) in [("large-text-1k", 1024), ("large-text-50k", 51200), ("large-text-100k", 102400)] {
            let p = generate("BC_RUN_000001", id, CALL_1).unwrap();
            assert_eq!(p.text.len(), n, "{id}");
            assert_eq!(sentinel_count(CALL_1, p.text.as_bytes()), 5);
            assert!(p.text.starts_with(&sentinel(CALL_1, 0)));
            assert!(p.text.ends_with(&sentinel(CALL_1, 100)));
            let mid = find(p.text.as_bytes(), sentinel(CALL_1, 50).as_bytes()).unwrap();
            assert_eq!(mid, n / 2, "{id}: middle sentinel offset");
        }
    }

    #[test]
    fn deterministic_and_call_specific() {
        let a = generate("BC_RUN_000001", "concurrent-two-tools", CALL_1).unwrap();
        let b = generate("BC_RUN_000001", "concurrent-two-tools", CALL_2).unwrap();
        assert_eq!(a, generate("BC_RUN_000001", "concurrent-two-tools", CALL_1).unwrap());
        assert!(a.text.ends_with("city=Boston"));
        assert!(b.text.ends_with("city=Chicago"));
        assert!(generate("BC_RUN_000001", "exact-text", CALL_2).is_none());
    }

    #[test]
    fn missing_sentinel_detection() {
        let p = generate("BC_RUN_000001", "large-text-100k", CALL_1).unwrap();
        let t = p.text.as_bytes();
        let head_tail = [&t[..25_000], &t[t.len() - 25_000..]].concat();
        assert_eq!(missing_sentinels(CALL_1, t, &head_tail), vec!["25%", "middle", "75%"]);
        assert!(missing_sentinels(CALL_1, t, t).is_empty());
        assert_eq!(missing_sentinels(CALL_1, t, &t[..50_000]), vec!["middle", "75%", "end"]);
    }

    #[test]
    fn structured_payload_keeps_number_tokens() {
        let p = generate("BC_RUN_000001", "structured-json", CALL_1).unwrap();
        assert!(p.text.contains("9007199254740993"));
        assert!(p.text.contains("12345678901234567890"));
        assert!(p.text.contains("19.990"));
    }
}
