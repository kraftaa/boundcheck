//! False-PASS / false-FAIL property tests.
//!
//! The central property is `no_false_pass_and_no_false_fail`: a simulated
//! runtime drives the real provider state machine (and the real MCP server
//! for the tool results) while applying random legal and illegal
//! transformations. An independent oracle, written separately from
//! `compare/`, rejects any PASS it cannot confirm, and any legal run that does
//! not PASS. The remaining properties pin down the comparison primitives and
//! check that the parsers never panic.

use boundarycheck::compare;
use boundarycheck::compare::classify::{classify_text, Shape};
use boundarycheck::compare::content::{common_prefix, common_suffix};
use boundarycheck::compare::json::{canonical_string, duplicate_key, first_diff, normalize_number};
use boundarycheck::mcp::server::McpServer;
use boundarycheck::model::observation::McpEvidence;
use boundarycheck::model::verdict::{FailureClass, Verdict};
use boundarycheck::provider::protocol::parse_request;
use boundarycheck::provider::protocol::Protocol;
use boundarycheck::provider::scenario::{Delivery, Phase, ScenarioMachine};
use boundarycheck::scenario::{self, ContentKind, ScenarioDef, TOOL_NAME};
use proptest::prelude::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use unicode_normalization::UnicodeNormalization;

const RUN: &str = "BC_RUN_000001";

// ---------------------------------------------------------------------------
// Text classification
// ---------------------------------------------------------------------------

/// Text with plenty of multi-byte and combining characters.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z ]{0,40}",
        "\\PC{0,60}",
        prop::collection::vec(
            prop::sample::select(vec!["a", "é", "e\u{301}", "😀", "→", "\n", "\"", "\u{2028}"]),
            0..40
        )
        .prop_map(|v| v.concat()),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The classifier's own false-PASS invariant: "no difference" exactly when equal.
    #[test]
    fn classify_reports_no_difference_iff_equal(e in any::<Vec<u8>>(), a in any::<Vec<u8>>()) {
        prop_assert_eq!(classify_text(&e, &a).is_none(), e == a);
        prop_assert!(classify_text(&e, &e).is_none());
    }

    #[test]
    fn classification_facts_are_consistent(e in text(), cut in any::<prop::sample::Index>(), extra in text()) {
        let e = e.as_bytes();
        let k = cut.index(e.len() + 1);
        for a in [e[..k].to_vec(), e[k..].to_vec(), [&e[..k], extra.as_bytes(), &e[k..]].concat()] {
            match classify_text(e, &a) {
                None => prop_assert_eq!(&a[..], e),
                Some(d) => {
                    prop_assert_ne!(&a[..], e);
                    prop_assert_eq!(d.tool_bytes, e.len());
                    prop_assert_eq!(d.provider_bytes, a.len());
                    prop_assert_eq!(d.first_difference, common_prefix(e, &a));
                    prop_assert!(d.retained_head_bytes + d.retained_tail_bytes <= e.len().min(a.len()));
                    prop_assert!(d.retained_tail_bytes <= common_suffix(e, &a));
                    prop_assert_eq!(d.removed_bytes, e.len() - d.retained_head_bytes - d.retained_tail_bytes);
                    prop_assert_eq!(d.inserted_bytes, a.len() - d.retained_head_bytes - d.retained_tail_bytes);
                    if !a.is_empty() && a.len() < e.len() && e.starts_with(&a) && std::str::from_utf8(&a).is_ok() {
                        prop_assert_eq!(d.shape, Shape::PrefixRetention);
                        prop_assert_eq!(d.class, FailureClass::Truncation);
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical JSON
// ---------------------------------------------------------------------------

/// JSON values with tricky number tokens, unicode strings and nesting.
fn json_value() -> impl Strategy<Value = Value> {
    let number = prop_oneof![
        any::<i64>().prop_map(|n| n.to_string()),
        any::<u64>().prop_map(|n| n.to_string()),
        (any::<i32>(), 0u32..6, -30i32..30).prop_map(|(m, z, e)| format!("{m}.{}1e{e}", "0".repeat(z as usize))),
        prop::sample::select(vec![
            "9007199254740993",
            "12345678901234567890",
            "19.990",
            "1e-7",
            "-0.0",
            "0",
            "1e400",
            "123456789012345678901234567890",
        ])
        .prop_map(str::to_owned),
    ]
    .prop_map(|t| serde_json::from_str::<Value>(&t).unwrap());
    let leaf =
        prop_oneof![Just(Value::Null), any::<bool>().prop_map(Value::Bool), number, text().prop_map(Value::String)];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::btree_map("[a-c/~é]{1,3}", inner, 0..5)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

/// Rewrites of a number token that keep its exact decimal value.
fn respell(token: &str) -> Vec<String> {
    let mut out = vec![format!("{token}e0"), format!("{token}E+0")];
    if token.contains('.') {
        out.push(format!("{token}000"));
    } else {
        out.push(format!("{token}.000"));
        out.push(format!("{token}0e-1"));
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn json_equality_is_reflexive_symmetric_and_matches_canonical_form(a in json_value(), b in json_value()) {
        prop_assert!(first_diff(&a, &a).is_none());
        prop_assert_eq!(first_diff(&a, &b).is_none(), first_diff(&b, &a).is_none());
        prop_assert_eq!(first_diff(&a, &b).is_none(), canonical_string(&a) == canonical_string(&b));
        let pretty: Value = serde_json::from_str(&serde_json::to_string_pretty(&a).unwrap()).unwrap();
        prop_assert!(first_diff(&a, &pretty).is_none());
    }

    #[test]
    fn equal_numbers_compare_equal_in_any_spelling(token in prop_oneof![
        any::<i64>().prop_map(|n| n.to_string()),
        (any::<u32>(), 1u32..8).prop_map(|(m, f)| format!("{m}.{:0>width$}", m % 97, width = f as usize)),
    ]) {
        for other in respell(&token) {
            prop_assert_eq!(normalize_number(&token), normalize_number(&other), "{} vs {}", token, other);
        }
    }

    #[test]
    fn distinct_integers_never_compare_equal(a in any::<i128>(), b in any::<i128>(), e in 0usize..40) {
        // a written as a × 10^e × 10^-e, b written plainly.
        let spelled = format!("{a}{}e-{e}", "0".repeat(e));
        prop_assert_eq!(normalize_number(&spelled) == normalize_number(&b.to_string()), a == b, "{} vs {}", a, b);
    }

    /// Any number in JSON grammar, including exponents far beyond i128, never
    /// panics and is never confused with `1` unless it is exactly 1.
    #[test]
    fn number_normalization_handles_extreme_tokens(t in "-?(0|[1-9][0-9]{0,40})(\\.[0-9]{1,40})?([eE][+-]?[0-9]{1,60})?") {
        let n = normalize_number(&t);
        prop_assert_eq!(&n, &normalize_number(&t));
        if n.1.starts_with("raw:") {
            prop_assert_ne!(n, normalize_number("1"));
        }
    }

    #[test]
    fn duplicate_keys_are_always_found(
        entries in prop::collection::btree_map("[a-d]{1,2}", json_value(), 1..5),
        pick in any::<prop::sample::Index>(),
        other in json_value(),
    ) {
        let text = serde_json::to_string(&Value::Object(entries.clone().into_iter().collect())).unwrap();
        prop_assert!(duplicate_key(&text).is_none(), "{}", text);
        let keys: Vec<&String> = entries.keys().collect();
        let key = keys[pick.index(keys.len())];
        // An earlier duplicate is exactly what last-key-wins parsing hides.
        let hidden = format!("{{{}:{},{}", serde_json::to_string(key).unwrap(), other, &text[1..]);
        prop_assert!(duplicate_key(&hidden).is_some(), "{}", hidden);
        let nested = format!("[1,{{\"z\":{}}}]", hidden);
        prop_assert!(duplicate_key(&nested).is_some(), "{}", nested);
    }
}

// ---------------------------------------------------------------------------
// Parsers never panic
// ---------------------------------------------------------------------------

fn message() -> impl Strategy<Value = Value> {
    let content = prop_oneof![
        Just(Value::Null),
        text().prop_map(Value::String),
        prop::collection::vec(text().prop_map(|t| json!({"type": "text", "text": t})), 0..3).prop_map(Value::Array),
        json_value(),
    ];
    (
        prop::sample::select(vec!["user", "assistant", "tool", "system", ""]),
        content,
        prop::option::of("[A-Z_0-9]{0,16}"),
    )
        .prop_map(|(role, content, id)| {
            let mut m = json!({"role": role, "content": content});
            if let Some(id) = id {
                m["tool_call_id"] = Value::String(id.clone());
                m["tool_calls"] =
                    json!([{"id": id, "type": "function", "function": {"name": "boundary_test", "arguments": "{}"}}]);
            }
            m
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn provider_request_parser_never_panics(
        bytes in any::<Vec<u8>>(),
        msgs in prop::collection::vec(message(), 0..6),
        junk in json_value(),
    ) {
        let _ = parse_request(&bytes);
        let _ = parse_request(junk.to_string().as_bytes());
        let body = json!({"model": "m", "messages": msgs, "tools": [{"type": "function", "function": {"name": TOOL_NAME}}]});
        let mut raw = body.to_string().into_bytes();
        let _ = parse_request(&raw);
        // Invalid UTF-8 inside a string is parsed lossily, never panics.
        if let Some(i) = raw.iter().position(|b| *b == b'"') {
            raw.insert(i + 1, 0xF0);
            let _ = parse_request(&raw);
        }
    }

    #[test]
    fn mcp_server_answers_any_input_with_json_rpc(lines in prop::collection::vec(prop_oneof![
        any::<Vec<u8>>(),
        json_value().prop_map(|v| v.to_string().into_bytes()),
        (any::<i64>(), json_value()).prop_map(|(id, args)| json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": TOOL_NAME, "arguments": args}
        }).to_string().into_bytes()),
        Just(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": TOOL_NAME, "arguments": scenario::tool_arguments(RUN, "exact-text", scenario::CALL_1)}
        }).to_string().into_bytes()),
    ], 0..6)) {
        let dir = tempfile::tempdir().unwrap();
        let server = McpServer::new(RUN.into(), "exact-text".into(), dir.path().join("ev.jsonl"));
        let mut input = Vec::new();
        for l in &lines {
            input.extend(l.iter().copied().filter(|b| *b != b'\n'));
            input.push(b'\n');
        }
        let mut out = Vec::new();
        server.run(&input[..], &mut out).unwrap();
        for line in out.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let v: Value = serde_json::from_slice(line).expect("every output line is JSON");
            prop_assert_eq!(&v["jsonrpc"], "2.0");
        }
    }
}

// ---------------------------------------------------------------------------
// No false PASS / no false FAIL, end to end through the state machine
// ---------------------------------------------------------------------------

const SCENARIOS: &[&str] = &[
    "exact-text",
    "large-text-1k",
    "concurrent-two-tools",
    "sequential-history",
    "structured-json",
    "retry-429",
    "mcp-error",
    "unicode-boundaries",
    "replay-history",
    "persistence-resume",
    "multi-text-blocks",
    "structured-content",
    "error-with-metadata",
    "mcp-protocol-error",
    // Probes: content Chat Completions cannot carry; never PASS, legal runs are UNKNOWN.
    "mixed-content",
    "embedded-resource",
    "resource-link",
    "audio-content",
    "retry-500",
    "retry-503",
    "retry-repeated",
    "disconnect-after-request",
    "connection-reset",
    "truncated-response",
    "slow-response",
];

type McpRun = (Vec<McpEvidence>, HashMap<String, String>);

/// Evidence and result text per call, produced by the real MCP server once per scenario.
fn mcp_results(id: &'static str) -> &'static McpRun {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, &'static McpRun>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some(r) = cache.get(id) {
        return r;
    }
    let def = scenario::find(id).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ev = dir.path().join("ev.jsonl");
    let server = McpServer::new(RUN.into(), id.into(), ev.clone());
    let mut input = String::new();
    for (i, c) in def.call_ids().iter().enumerate() {
        let args = scenario::tool_arguments(RUN, id, c);
        let call = json!({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {"name": TOOL_NAME, "arguments": args}});
        input.push_str(&format!("{call}\n"));
    }
    let mut out = Vec::new();
    server.run(input.as_bytes(), &mut out).unwrap();
    let mut texts = HashMap::new();
    for line in out.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let v: Value = serde_json::from_slice(line).unwrap();
        let i = v["id"].as_u64().unwrap() as usize;
        // A simulated runtime forwards text blocks (concatenated) or a JSON-RPC error's message.
        let t: String = match v["error"]["message"].as_str() {
            Some(message) => message.to_owned(),
            None => v["result"]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["type"] == "text")
                .map(|c| c["text"].as_str().unwrap())
                .collect(),
        };
        texts.insert(def.call_ids()[i].to_owned(), t);
    }
    let evidence = std::fs::read_to_string(&ev).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let run: &'static McpRun = Box::leak(Box::new((evidence, texts)));
    cache.insert(id, run);
    run
}

#[derive(Debug, Clone)]
enum Mutation {
    // Legal: must still PASS.
    None,
    SplitIntoParts(usize),
    AsciiEscapes,
    Reorder,
    ReserializeJson,
    // Illegal: must never PASS.
    Truncate(usize),
    Append(String),
    ReplaceChar(usize),
    Drop,
    Duplicate,
    Swap,
    ChangeId,
    MissingId,
    Normalize,
    Empty,
    Null,
    CopyIntoUser,
    ExtraUnissued,
}

impl Mutation {
    fn is_legal(&self) -> bool {
        matches!(
            self,
            Mutation::None
                | Mutation::SplitIntoParts(_)
                | Mutation::AsciiEscapes
                | Mutation::Reorder
                | Mutation::ReserializeJson
        )
    }
}

/// Where an illegal mutation is applied: the first delivery of a turn's
/// results, the HTTP retry after a 429, or the replayed / resumed history.
#[derive(Debug, Clone, Copy)]
enum Stage {
    Delivery(usize),
    Retry,
    Replay,
}

fn mutation() -> impl Strategy<Value = Mutation> {
    prop_oneof![
        3 => Just(Mutation::None),
        2 => (1usize..6).prop_map(Mutation::SplitIntoParts),
        2 => Just(Mutation::AsciiEscapes),
        1 => Just(Mutation::Reorder),
        1 => Just(Mutation::ReserializeJson),
        2 => any::<usize>().prop_map(Mutation::Truncate),
        1 => text().prop_map(Mutation::Append),
        2 => any::<usize>().prop_map(Mutation::ReplaceChar),
        1 => Just(Mutation::Drop),
        1 => Just(Mutation::Duplicate),
        1 => Just(Mutation::Swap),
        1 => Just(Mutation::ChangeId),
        1 => Just(Mutation::MissingId),
        1 => Just(Mutation::Normalize),
        1 => Just(Mutation::Empty),
        1 => Just(Mutation::Null),
        1 => Just(Mutation::CopyIntoUser),
        1 => Just(Mutation::ExtraUnissued),
    ]
}

fn stage() -> impl Strategy<Value = Stage> {
    prop_oneof![3 => (0usize..2).prop_map(Stage::Delivery), 1 => Just(Stage::Retry), 1 => Just(Stage::Replay)]
}

/// Rewrite one turn's batch of tool messages.
fn apply(m: &Mutation, kind: ContentKind, batch: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = batch.to_vec();
    let text_of = |v: &Value| v["content"].as_str().unwrap_or_default().to_owned();
    let set = |v: &mut Value, t: String| v["content"] = Value::String(t);
    match m {
        Mutation::None | Mutation::AsciiEscapes => {}
        Mutation::SplitIntoParts(n) => {
            for v in &mut out {
                let chars: Vec<char> = text_of(v).chars().collect();
                let size = chars.len().div_ceil(*n).max(1);
                let mut parts: Vec<Value> =
                    chars.chunks(size).map(|c| json!({"type": "text", "text": c.iter().collect::<String>()})).collect();
                if parts.is_empty() {
                    parts.push(json!({"type": "text", "text": ""}));
                }
                v["content"] = Value::Array(parts);
            }
        }
        Mutation::Reorder => out.reverse(),
        Mutation::ReserializeJson => {
            if kind == ContentKind::Json {
                for v in &mut out {
                    let parsed: Value = serde_json::from_str(&text_of(v)).unwrap();
                    set(v, serde_json::to_string_pretty(&parsed).unwrap());
                }
            }
        }
        Mutation::Truncate(k) => {
            let t = text_of(&out[0]);
            let cut = k % t.len().max(1);
            set(&mut out[0], String::from_utf8_lossy(&t.as_bytes()[..cut]).into_owned());
        }
        Mutation::Append(x) => {
            let t = text_of(&out[0]);
            set(&mut out[0], format!("{t}{}", if x.is_empty() { " " } else { x }));
        }
        Mutation::ReplaceChar(k) => {
            let mut chars: Vec<char> = text_of(&out[0]).chars().collect();
            let i = k % chars.len().max(1);
            if let Some(c) = chars.get_mut(i) {
                *c = if *c == 'X' { 'Y' } else { 'X' };
            }
            set(&mut out[0], chars.into_iter().collect());
        }
        Mutation::Drop => {
            out.pop();
        }
        Mutation::Duplicate => out.push(out[0].clone()),
        Mutation::Swap => {
            if out.len() >= 2 {
                let (a, b) = (out[0]["content"].clone(), out[1]["content"].clone());
                out[0]["content"] = b;
                out[1]["content"] = a;
            } else {
                out[0]["tool_call_id"] = Value::String(scenario::CALL_2.into());
            }
        }
        Mutation::ChangeId => out[0]["tool_call_id"] = Value::String("BC_CALL_777777".into()),
        Mutation::MissingId => {
            out[0].as_object_mut().unwrap().remove("tool_call_id");
        }
        Mutation::Normalize => {
            for v in &mut out {
                let t = text_of(v);
                let n: String = t.nfc().collect();
                // Guarantee a real change even for already-normalized text.
                set(v, if n == t { format!("{t}\u{301}") } else { n });
            }
        }
        Mutation::Empty => set(&mut out[0], String::new()),
        Mutation::Null => out[0]["content"] = Value::Null,
        Mutation::CopyIntoUser => {
            let t = text_of(&out[0]);
            out.push(json!({"role": "user", "content": format!("tool said: {t}")}));
        }
        Mutation::ExtraUnissued => {
            out.push(json!({"role": "tool", "tool_call_id": "BC_CALL_999999", "content": "runtime-generated"}));
        }
    }
    out
}

/// Serialize with every non-ASCII character as a JSON UTF-16 escape (a legal transport change).
fn ascii_json(v: &Value) -> String {
    let mut out = String::new();
    for c in v.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("{}u{:04x}", '\\', unit));
            }
        }
    }
    out
}

/// Replace the trailing tool-message batch of `history` using `m`.
fn mutate_tail(m: &Mutation, kind: ContentKind, history: &[Value]) -> Vec<Value> {
    let mut h = history.to_vec();
    let n = h.iter().rev().take_while(|v| v["role"] == "tool").count();
    let batch = h.split_off(h.len() - n);
    h.extend(apply(m, kind, &batch));
    h
}

/// Which wire format the simulated runtime speaks.
#[derive(Debug, Clone, Copy)]
enum Wire {
    Chat,
    /// `previous: true` sends only new items plus `previous_response_id` after the first response.
    Responses {
        previous: bool,
    },
}

fn wire() -> impl Strategy<Value = Wire> {
    prop_oneof![Just(Wire::Chat), Just(Wire::Responses { previous: false }), Just(Wire::Responses { previous: true })]
}

/// Chat-shaped history -> Responses input items (independent of `src/` and of the fixtures).
fn to_items(msgs: &[Value]) -> Vec<Value> {
    let mut items = vec![];
    for m in msgs {
        if let Some(calls) = m["tool_calls"].as_array() {
            for c in calls {
                items.push(json!({"type": "function_call", "call_id": c["id"], "name": c["function"]["name"],
                                  "arguments": c["function"]["arguments"]}));
            }
        } else if m["role"] == "tool" {
            let output = match &m["content"] {
                Value::Array(parts) => {
                    Value::Array(parts.iter().map(|p| json!({"type": "input_text", "text": p["text"]})).collect())
                }
                other => other.clone(),
            };
            let mut item = json!({"type": "function_call_output", "output": output});
            if let Some(id) = m.get("tool_call_id") {
                item["call_id"] = id.clone();
            }
            items.push(item);
        } else {
            items.push(json!({"role": m["role"], "content": m["content"]}));
        }
    }
    items
}

/// The simulated runtime's connection to the fake provider. It records, per
/// request, the conversation the model sees (chat-shaped), rebuilding
/// Responses history from `previous_response_id` on its own.
struct Sim {
    machine: ScenarioMachine,
    path: String,
    wire: Wire,
    escape: bool,
    store: HashMap<String, Vec<Value>>,
    prev: Option<String>,
    sent: usize,
    seq: u64,
    effective: HashMap<u64, Vec<Value>>,
}

impl Sim {
    fn new(def: &'static ScenarioDef, wire: Wire, escape: bool) -> Sim {
        let protocol = match wire {
            Wire::Chat => Protocol::ChatCompletions,
            Wire::Responses { .. } => Protocol::Responses,
        };
        let machine = ScenarioMachine::with_protocol(RUN, def, protocol);
        let path = format!("{}{}", machine.base_path(), protocol.endpoint());
        Sim {
            machine,
            path,
            wire,
            escape,
            store: HashMap::new(),
            prev: None,
            sent: 0,
            seq: 0,
            effective: HashMap::new(),
        }
    }

    /// Send the history; returns the status and the reply as `{content, tool_calls}`.
    fn send(&mut self, msgs: &[Value]) -> (u16, Value) {
        self.seq += 1;
        let (body, eff) = match self.wire {
            Wire::Chat => {
                let tools = json!([{"type": "function", "function": {"name": TOOL_NAME, "parameters": {}}}]);
                (json!({"model": "sim", "messages": msgs, "tools": tools}), msgs.to_vec())
            }
            Wire::Responses { previous } => {
                let tools = json!([{"type": "function", "name": TOOL_NAME, "parameters": {}}]);
                match (&self.prev, previous) {
                    (Some(prev), true) => {
                        let new = &msgs[self.sent.min(msgs.len())..];
                        let mut eff = self.store[prev].clone();
                        eff.extend(new.iter().cloned());
                        let body = json!({"model": "sim", "input": to_items(new), "tools": tools, "previous_response_id": prev});
                        (body, eff)
                    }
                    _ => (json!({"model": "sim", "input": to_items(msgs), "tools": tools}), msgs.to_vec()),
                }
            }
        };
        self.effective.insert(self.seq, eff.clone());
        let raw = if self.escape { ascii_json(&body) } else { body.to_string() };
        let reply = self.machine.handle("POST", &self.path, raw.into_bytes(), false, None);
        // A failed status, or a reply that is never fully delivered, is retried.
        let delivered = matches!(reply.delivery, Delivery::Normal | Delivery::Delay(_));
        if reply.status != 200 || !delivered {
            return (if delivered { reply.status } else { 0 }, Value::Null);
        }
        let v: Value = serde_json::from_slice(&reply.body).unwrap();
        let msg = match self.wire {
            Wire::Chat => v["choices"][0]["message"].clone(),
            Wire::Responses { .. } => {
                let output = v["output"].as_array().unwrap();
                let calls: Vec<Value> = output
                    .iter()
                    .filter(|i| i["type"] == "function_call")
                    .map(|i| {
                        json!({"id": i["call_id"], "type": "function",
                                    "function": {"name": i["name"], "arguments": i["arguments"]}})
                    })
                    .collect();
                let text = output.iter().find(|i| i["type"] == "message").map(|i| i["content"][0]["text"].clone());
                let msg = if calls.is_empty() {
                    json!({"role": "assistant", "content": text.unwrap_or(Value::Null)})
                } else {
                    json!({"role": "assistant", "content": null, "tool_calls": calls})
                };
                let mut conv = eff;
                conv.push(msg.clone());
                let id = v["id"].as_str().unwrap().to_owned();
                self.store.insert(id.clone(), conv);
                self.prev = Some(id);
                msg
            }
        };
        (200, msg)
    }
}

/// Drive the real state machine like a runtime would, applying `m` at `stage`.
/// Returns the simulator (machine + per-request conversations) and whether an
/// illegal mutation was actually applied.
fn simulate(def: &'static ScenarioDef, m: &Mutation, stage: Stage, wire: Wire) -> (Sim, bool) {
    let (_, results) = mcp_results(def.id);
    let mut sim = Sim::new(def, wire, matches!(m, Mutation::AsciiEscapes));
    let illegal_at = |s: fn(Stage) -> bool| !m.is_legal() && s(stage);
    let mut messages = vec![json!({"role": "user", "content": def.prompt(RUN)})];
    let mut pending: Option<Vec<Value>> = None;
    let (mut turn, mut applied) = (0, false);
    for _ in 0..16 {
        let to_send = pending.take().unwrap_or_else(|| messages.clone());
        let (status, msg) = sim.send(&to_send);
        if status != 200 {
            pending = Some(if illegal_at(|s| matches!(s, Stage::Retry)) {
                applied = true;
                mutate_tail(m, def.kind, &messages)
            } else {
                messages.clone()
            });
            continue;
        }
        if let Some(calls) = msg["tool_calls"].as_array() {
            messages.push(json!({"role": "assistant", "content": null, "tool_calls": calls}));
            sim.sent = messages.len();
            let batch: Vec<Value> = calls
                .iter()
                .map(|c| {
                    let id = c["id"].as_str().unwrap();
                    json!({"role": "tool", "tool_call_id": id, "content": results[id]})
                })
                .collect();
            let here = matches!(stage, Stage::Delivery(t) if t == turn);
            let batch = if m.is_legal() || here {
                applied |= !m.is_legal();
                apply(m, def.kind, &batch)
            } else {
                batch
            };
            messages.extend(batch);
            turn += 1;
            continue;
        }
        match msg["content"].as_str() {
            Some(scenario::REPLAY_TEXT) | Some(scenario::CHECKPOINT_TEXT) => {
                // A replay or resume re-submits the whole history as a new conversation.
                sim.prev = None;
                sim.sent = 0;
                pending = Some(if illegal_at(|s| matches!(s, Stage::Replay)) {
                    applied = true;
                    mutate_tail(m, def.kind, &messages)
                } else {
                    messages.clone()
                });
            }
            _ => break,
        }
    }
    (sim, applied)
}

/// Exact decimal value of a number token, implemented independently of `compare::json`.
fn decimal(token: &str) -> String {
    let (sign, t) = token.strip_prefix('-').map_or(("", token), |r| ("-", r));
    let (m, e) = t.split_once(['e', 'E']).map_or((t, 0i128), |(m, e)| (m, e.parse().unwrap_or(i128::MAX)));
    let (i, f) = m.split_once('.').unwrap_or((m, ""));
    let digits = format!("{i}{f}");
    let d = digits.trim_matches('0');
    if d.is_empty() {
        return "0".into();
    }
    let trailing = digits.len() - digits.trim_end_matches('0').len();
    format!("{sign}{d}e{}", e - f.len() as i128 + trailing as i128)
}

fn same_json(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => decimal(&x.to_string()) == decimal(&y.to_string()),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_json(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same_json(v, w)))
        }
        _ => a == b,
    }
}

/// Independent oracle: did every provider request carry exactly what MCP emitted?
fn faithful(def: &ScenarioDef, sim: &Sim) -> Result<(), String> {
    let (_, results) = mcp_results(def.id);
    let machine = &sim.machine;
    if machine.phase != Phase::Complete {
        return Err(format!("state machine ended in {:?}", machine.phase));
    }
    let text = |m: &Value| -> Option<String> {
        match &m["content"] {
            Value::String(s) => Some(s.clone()),
            Value::Array(parts) => parts.iter().map(|p| p["text"].as_str().map(str::to_owned)).collect(),
            _ => None,
        }
    };
    for r in machine.requests.iter().filter(|r| !r.issued_before.is_empty()) {
        let msgs = sim.effective.get(&r.seq).ok_or("request not sent by the simulator")?;
        for m in msgs.iter().filter(|m| m["role"] == "tool") {
            let id = m["tool_call_id"].as_str().ok_or("tool message without id")?;
            if !r.issued_before.iter().any(|c| c == id) {
                return Err(format!("unissued id {id}"));
            }
        }
        for c in &r.issued_before {
            let expected = &results[c];
            let occ: Vec<&Value> =
                msgs.iter().filter(|m| m["role"] == "tool" && m["tool_call_id"] == c.as_str()).collect();
            if occ.len() != 1 {
                return Err(format!("{c}: {} occurrences in request {}", occ.len(), r.seq));
            }
            let got = text(occ[0]).ok_or("non-text content")?;
            let json_equal = def.kind == ContentKind::Json
                && duplicate_key(&got).is_none()
                && serde_json::from_str::<Value>(&got)
                    .ok()
                    .zip(serde_json::from_str::<Value>(expected).ok())
                    .is_some_and(|(g, e)| same_json(&g, &e));
            // Errors (JSON-RPC errors and isError results) only need their exact
            // content, in any wording around it, with the content's last character
            // left intact (no combining mark or joiner right after it).
            #[allow(clippy::unnecessary_map_or)] // `is_none_or` is newer than the MSRV.
            let intact = |hay: &str, needle: &str| {
                hay.match_indices(needle).any(|(i, _)| {
                    hay[i + needle.len()..].chars().next().map_or(true, |c| {
                        unicode_normalization::char::canonical_combining_class(c) == 0
                            && !unicode_normalization::char::is_combining_mark(c)
                            && !matches!(c, '\u{200d}' | '\u{fe00}'..='\u{fe0f}')
                    })
                })
            };
            let error_ok = def.kind == ContentKind::Error && !expected.is_empty() && intact(&got, expected);
            if got != *expected && !json_equal && !error_ok {
                return Err(format!("{c}: content differs in request {}", r.seq));
            }
            if msgs.iter().any(|m| m["role"] != "tool" && text(m).is_some_and(|t| t.contains(expected.as_str()))) {
                return Err(format!("{c}: copied into a non-tool message"));
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn no_false_pass_and_no_false_fail(
        sid in prop::sample::select(SCENARIOS),
        m in mutation(),
        stage in stage(),
        wire in wire(),
    ) {
        let def = scenario::find(sid).unwrap();
        let (mcp, _) = mcp_results(sid);
        let (sim, applied) = simulate(def, &m, stage, wire);
        let eval = compare::evaluate(&sim.machine, mcp, vec![]);
        let oracle = faithful(def, &sim);
        if eval.verdict == Verdict::Pass {
            prop_assert!(oracle.is_ok(), "FALSE PASS: {} {:?} at {:?} over {:?}: {}", sid, m, stage, wire, oracle.unwrap_err());
        }
        if def.tier == scenario::Tier::Probe {
            // The simulator forwards only the text blocks. Never PASS. A legal run is
            // UNKNOWN when the protocol cannot carry the other blocks, and FAIL when it
            // can (dropping an image the protocol could carry is a proven loss).
            prop_assert_ne!(eval.verdict, Verdict::Pass, "FALSE PASS on probe {} {:?}", sid, m);
            if !applied {
                let carried = mcp
                    .iter()
                    .filter_map(|r| compare::tool_observation(def, r))
                    .all(|o| o.blocks.iter().all(|b| sim.machine.protocol.representable().carries(b)));
                let want = if carried { Verdict::Fail } else { Verdict::Unknown };
                prop_assert_eq!(eval.verdict, want, "{} {:?} over {:?}: {:?}", sid, m, wire, eval.findings);
            }
            return Ok(());
        }
        if !applied {
            prop_assert!(oracle.is_ok(), "oracle rejected a legal run: {} {:?}: {:?}", sid, m, oracle);
            prop_assert_eq!(eval.verdict, Verdict::Pass, "FALSE FAIL/UNKNOWN: {} {:?} over {:?}: {:?} {:?}", sid, m, wire, eval.findings, eval.unknowns);
        }
        if eval.verdict == Verdict::Fail {
            prop_assert!(!eval.findings.is_empty());
        }
    }
}

/// Guards the property above: if the oracle were too lenient, it would be vacuous.
#[test]
fn every_illegal_mutation_is_rejected_by_oracle_and_evaluator() {
    let illegal = [
        Mutation::Truncate(5),
        Mutation::Append("x".into()),
        Mutation::ReplaceChar(3),
        Mutation::Drop,
        Mutation::Duplicate,
        Mutation::Swap,
        Mutation::ChangeId,
        Mutation::MissingId,
        Mutation::Normalize,
        Mutation::Empty,
        Mutation::Null,
        Mutation::CopyIntoUser,
        Mutation::ExtraUnissued,
    ];
    for sid in ["concurrent-two-tools", "structured-json"] {
        let def = scenario::find(sid).unwrap();
        let (mcp, _) = mcp_results(sid);
        for m in &illegal {
            for wire in [Wire::Chat, Wire::Responses { previous: false }, Wire::Responses { previous: true }] {
                let (sim, applied) = simulate(def, m, Stage::Delivery(0), wire);
                assert!(applied, "{sid} {m:?} {wire:?}");
                assert!(faithful(def, &sim).is_err(), "oracle accepted {sid} {m:?} {wire:?}");
                let verdict = compare::evaluate(&sim.machine, mcp, vec![]).verdict;
                assert_ne!(verdict, Verdict::Pass, "{sid} {m:?} {wire:?}");
            }
        }
    }
}
