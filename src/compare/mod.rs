//! Evaluation: correlate what the MCP server emitted with what the runtime
//! sent to the provider, by call ID only, and derive a verdict.

pub mod classify;
pub mod content;
pub mod json;

use crate::compare::classify::{classify_text, TextDifference};
use crate::compare::content::sha256_hex;
use crate::compare::json::JsonDiff;
use crate::mcp::payload;
use crate::model::observation::{McpEvidence, ProviderObservation, ToolObservation};
use crate::model::verdict::{decide, FailureClass, UnknownNote, UnknownReason, Verdict};
use crate::provider::protocol::{MessageContent, ParsedMessage};
use crate::provider::recorder::RequestRole;
use crate::provider::scenario::{Phase, ScenarioMachine};
use crate::scenario::{self, ContentKind, ScenarioDef, TOOL_NAME};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub class: FailureClass,
    /// For `RetryMutation` / `ReplayMutation`: the proven fault underneath
    /// (e.g. `MissingResult`, `WrongToolCallAssociation`, `Truncation`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underlying_class: Option<FailureClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_difference: Option<TextDifference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_difference: Option<JsonDiff>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing_sentinels: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrences: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_matches_call: Option<String>,
}

impl Finding {
    /// Re-label a finding as a retry/replay mutation, keeping the original
    /// class (the first one, if it was already re-labelled) as the cause.
    fn relabel(&mut self, class: FailureClass) {
        self.underlying_class.get_or_insert(self.class);
        self.class = class;
    }

    fn new(class: FailureClass, call_id: &str, seq: u64, attempt: Option<u32>, summary: String) -> Self {
        Finding {
            class,
            underlying_class: None,
            call_id: Some(call_id.to_owned()),
            request_seq: Some(seq),
            attempt,
            summary,
            text_difference: None,
            json_difference: None,
            missing_sentinels: vec![],
            occurrences: None,
            provider_tool_call_id: None,
            content_matches_call: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSide {
    pub emissions: usize,
    /// Size and hash of the raw JSON-RPC response line (transport evidence).
    pub transport_bytes: usize,
    pub transport_sha256: String,
    pub bytes: usize,
    pub sha256: String,
    pub is_error: bool,
    pub generated: bool,
    pub arguments_unchanged: bool,
    pub sentinels: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_canonical_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderSide {
    pub request_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    pub occurrences: usize,
    pub message_indexes: Vec<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_parts: Option<usize>,
    pub transport_representation: &'static str,
    pub extracted_text: &'static str,
    pub json_semantics: &'static str,
    pub error_status: &'static str,
    pub outcome: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallReport {
    pub call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolSide>,
    pub provider: Vec<ProviderSide>,
}

#[derive(Debug, Clone)]
pub struct Evaluation {
    pub verdict: Verdict,
    pub classifications: Vec<FailureClass>,
    pub underlying_classifications: Vec<FailureClass>,
    pub findings: Vec<Finding>,
    pub unknowns: Vec<UnknownNote>,
    pub notes: Vec<String>,
    pub calls: Vec<CallReport>,
    pub final_phase: String,
}

#[derive(Deserialize)]
struct RawMcpResponse<'a> {
    #[serde(borrow)]
    result: Option<RawMcpResult<'a>>,
}

#[derive(Deserialize)]
struct RawMcpResult<'a> {
    #[serde(borrow, default)]
    content: Vec<RawMcpItem<'a>>,
}

#[derive(Deserialize)]
struct RawMcpItem<'a> {
    #[serde(borrow, default)]
    text: Option<&'a RawValue>,
}

/// Extract the logical tool result from one raw MCP response line.
pub fn tool_observation(def: &ScenarioDef, rec: &McpEvidence) -> Option<ToolObservation> {
    let raw = base64::engine::general_purpose::STANDARD.decode(rec.response_b64.as_ref()?).ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let result = v.get("result")?;
    let items = result.get("content")?.as_array()?;
    let mut text = String::new();
    for item in items {
        text.push_str(item.get("text")?.as_str()?);
    }
    let raw_text_token = std::str::from_utf8(&raw)
        .ok()
        .and_then(|s| serde_json::from_str::<RawMcpResponse>(s).ok())
        .and_then(|r| r.result)
        .filter(|r| r.content.len() == 1)
        .and_then(|r| r.content[0].text.map(|t| t.get().to_owned()));
    let json = match def.kind {
        ContentKind::Json => serde_json::from_str(&text).ok().or_else(|| result.get("structuredContent").cloned()),
        _ => None,
    };
    Some(ToolObservation {
        call_id: rec.call_id.clone()?,
        content_sha256: sha256_hex(text.as_bytes()),
        extracted_content: text.into_bytes(),
        raw_transport: raw,
        raw_text_token,
        json,
        is_error: result.get("isError").and_then(Value::as_bool).unwrap_or(false),
        arguments: rec.arguments.clone(),
        generated: rec.generated,
    })
}

fn provider_observation(m: &ParsedMessage) -> Option<ProviderObservation> {
    // `content: null` or a missing `content` is proven-empty content, not an unsupported shape.
    let (text, parts) = match &m.content {
        MessageContent::Text { text, parts } => (text.as_str(), *parts),
        MessageContent::Absent => ("", 0),
        MessageContent::Unsupported(_) => return None,
    };
    Some(ProviderObservation {
        raw_content_token: m.raw_content.clone(),
        content_sha256: sha256_hex(text.as_bytes()),
        extracted_content: text.as_bytes().to_vec(),
        content_parts: parts,
    })
}

pub fn evaluate(machine: &ScenarioMachine, mcp: &[McpEvidence], runner_unknowns: Vec<UnknownNote>) -> Evaluation {
    let def = machine.def;
    let mut unknowns = machine.unknowns.clone();
    unknowns.extend(runner_unknowns);
    let mut findings: Vec<Finding> = vec![];
    let mut notes: Vec<String> = vec![];

    // ---- MCP boundary -------------------------------------------------
    let sessions = mcp.iter().filter(|r| r.event == "session-start").count();
    let mut emitted: BTreeMap<String, Vec<ToolObservation>> = BTreeMap::new();
    for rec in mcp.iter().filter(|r| r.event == "tool-response" && r.tool.as_deref() == Some(TOOL_NAME)) {
        match tool_observation(def, rec) {
            Some(obs) => emitted.entry(obs.call_id.clone()).or_default().push(obs),
            None => notes
                .push(format!("MCP response seq {} (pid {}) carried no call_id or no text content", rec.seq, rec.pid)),
        }
    }
    for turn in def.turns.iter().filter(|turn| turn.len() > 1) {
        let completion_order: Vec<&str> = mcp
            .iter()
            .filter(|r| r.event == "tool-response")
            .filter_map(|r| r.call_id.as_deref())
            .filter(|id| turn.contains(id))
            .collect();
        if completion_order.len() == turn.len() && completion_order != *turn {
            notes.push(format!(
                "MCP calls completed in order {completion_order:?}, different from issue order {:?}",
                turn
            ));
        }
    }
    if sessions == 0 && !machine.issued.is_empty() {
        unknowns.push(UnknownNote::new(
            UnknownReason::AdapterInsufficient,
            "the runtime never started the boundarycheck MCP server",
        ));
    }
    let mut expected: BTreeMap<String, ToolObservation> = BTreeMap::new();
    let mut tool_sides: HashMap<String, ToolSide> = HashMap::new();
    for call in &machine.issued {
        let Some(list) = emitted.get(call) else {
            unknowns.push(UnknownNote::new(
                UnknownReason::ExpectedTransitionMissing,
                format!("the runtime never invoked {TOOL_NAME} through MCP for {call}"),
            ));
            continue;
        };
        let first = list[0].clone();
        if list.iter().any(|o| o.extracted_content != first.extracted_content) {
            notes.push(format!(
                "MCP server emitted different contents for {call} across invocations; comparing against the first"
            ));
        }
        if list.len() > 1 {
            notes.push(format!("{TOOL_NAME} was invoked {} times for {call}", list.len()));
        }
        let planned = scenario::tool_arguments(&machine.run_id, def.id, call);
        let args_ok = first.arguments.as_ref().is_some_and(|a| json::first_diff(&planned, a).is_none());
        if !args_ok || !first.generated {
            unknowns.push(UnknownNote::new(
                UnknownReason::ToolArgumentsChanged,
                format!(
                    "tool arguments for {call} did not reach the MCP server unchanged: sent {planned}, received {}",
                    first.arguments.as_ref().map(|a| a.to_string()).unwrap_or_else(|| "(none)".into())
                ),
            ));
        }
        // Independent integrity check: the evidence log lives in a directory the
        // runtime can write to, so cross-check it against the generator.
        if first.generated {
            match payload::generate(&machine.run_id, def.id, call) {
                Some(p) if p.text.as_bytes() == first.extracted_content.as_slice() && p.is_error == first.is_error => {}
                _ => unknowns.push(UnknownNote::new(
                    UnknownReason::EvidenceInconsistent,
                    format!("MCP evidence for {call} does not match the deterministic payload generator"),
                )),
            }
        }
        tool_sides.insert(
            call.clone(),
            ToolSide {
                emissions: list.len(),
                transport_bytes: first.raw_transport.len(),
                transport_sha256: sha256_hex(&first.raw_transport),
                bytes: first.extracted_content.len(),
                sha256: first.content_sha256.clone(),
                is_error: first.is_error,
                generated: first.generated,
                arguments_unchanged: args_ok,
                sentinels: payload::sentinel_count(call, &first.extracted_content),
                json_canonical_sha256: first.json.as_ref().map(|j| sha256_hex(json::canonical_string(j).as_bytes())),
            },
        );
        expected.insert(call.clone(), first);
    }

    // ---- Provider boundary --------------------------------------------
    let mut provider_sides: HashMap<String, Vec<ProviderSide>> = HashMap::new();
    let mut first_attempt_ok: HashMap<(usize, String), bool> = HashMap::new();
    for req in &machine.requests {
        let Some(parsed) = &req.parsed else { continue };
        if req.issued_before.is_empty() {
            continue;
        }
        let (turn, attempt) = match req.role {
            RequestRole::Result { turn, attempt } => (Some(turn), Some(attempt)),
            _ => (None, None),
        };
        let replayed = matches!(req.role, RequestRole::Replay { .. });
        let tool_msgs: Vec<&ParsedMessage> = parsed.messages.iter().filter(|m| m.role == "tool").collect();
        let echoed = parsed.messages.iter().any(|m| m.tool_call_ids.iter().any(|id| req.issued_before.contains(id)));
        if tool_msgs.is_empty() && !echoed {
            unknowns.push(UnknownNote::new(
                UnknownReason::UnexpectedTransition,
                format!(
                    "request #{} carries neither the issued tool calls nor any tool result (conversation restarted?)",
                    req.seq
                ),
            ));
            continue;
        }
        if parsed.parallel_tool_calls == Some(false) && def.turns.iter().any(|t| t.len() > 1) {
            notes.push(format!(
                "request #{} set parallel_tool_calls=false; the scenario still issues concurrent calls",
                req.seq
            ));
        }

        for call in &req.issued_before {
            let Some(exp) = expected.get(call) else { continue };
            let occ: Vec<&&ParsedMessage> =
                tool_msgs.iter().filter(|m| m.tool_call_id.as_deref() == Some(call)).collect();
            let mut side = ProviderSide {
                request_seq: req.seq,
                attempt,
                occurrences: occ.len(),
                message_indexes: occ.iter().map(|m| m.index).collect(),
                bytes: None,
                sha256: None,
                content_parts: None,
                transport_representation: "not-comparable",
                extracted_text: "absent",
                json_semantics: "not-applicable",
                error_status: if def.kind == ContentKind::Error { "not-representable" } else { "not-applicable" },
                outcome: "match",
            };
            let mut call_findings: Vec<Finding> = vec![];

            if occ.is_empty() {
                side.outcome = "missing";
                call_findings.push(missing_finding(
                    call,
                    exp,
                    req.seq,
                    attempt,
                    &tool_msgs,
                    &parsed.messages,
                    &expected,
                ));
            } else {
                // The exact payload (which embeds its own unique identifiers) copied into
                // a non-tool message means the provider sees the result more than once.
                if let Some(m) = parsed.messages.iter().find(|m| {
                    m.role != "tool"
                        && m.content.text().is_some_and(|t| payload::contains(t.as_bytes(), &exp.extracted_content))
                }) {
                    let mut f = Finding::new(
                        FailureClass::DuplicateResult,
                        call,
                        req.seq,
                        attempt,
                        format!("result of {call} also appears inside a `{}` message (index {})", m.role, m.index),
                    );
                    f.occurrences = Some(occ.len() + 1);
                    call_findings.push(f);
                }
                if occ.len() > 1 {
                    side.outcome = "duplicate";
                    let mut f = Finding::new(
                        FailureClass::DuplicateResult,
                        call,
                        req.seq,
                        attempt,
                        format!("{call} appears in {} tool messages (indexes {:?})", occ.len(), side.message_indexes),
                    );
                    f.occurrences = Some(occ.len());
                    call_findings.push(f);
                }
                for (i, m) in occ.iter().enumerate() {
                    let Some(obs) = provider_observation(m) else {
                        side.outcome = "unsupported";
                        unknowns.push(UnknownNote::new(
                            UnknownReason::UnsupportedRequestShape,
                            format!(
                                "request #{} message {}: unsupported tool content shape ({:?})",
                                req.seq, m.index, m.content
                            ),
                        ));
                        continue;
                    };
                    if i == 0 {
                        side.bytes = Some(obs.extracted_content.len());
                        side.sha256 = Some(obs.content_sha256.clone());
                        side.content_parts = Some(obs.content_parts);
                        side.transport_representation = match (&exp.raw_text_token, &obs.raw_content_token) {
                            (Some(a), Some(b)) if obs.content_parts == 0 => {
                                if a == b {
                                    "identical"
                                } else {
                                    "changed"
                                }
                            }
                            (Some(_), Some(_)) => "changed",
                            _ => "not-comparable",
                        };
                    }
                    let (text_state, json_state, finding) =
                        compare_occurrence(def, exp, &obs, parsed.body_valid_utf8, &expected, req.seq, attempt);
                    if i == 0 {
                        side.extracted_text = text_state;
                        side.json_semantics = json_state;
                    }
                    if json_state == "identical" && text_state == "changed" {
                        notes.push(format!(
                            "{call} (request #{}): text re-serialized; JSON semantics identical",
                            req.seq
                        ));
                    }
                    if let Some(f) = finding {
                        if side.outcome == "match" {
                            side.outcome = "mismatch";
                        }
                        call_findings.push(f);
                    }
                }
            }

            if let (Some(t), Some(a)) = (turn, attempt) {
                let key = (t, call.clone());
                if a == 1 {
                    first_attempt_ok.insert(key, call_findings.is_empty());
                } else if first_attempt_ok.get(&key) == Some(&true) {
                    for f in &mut call_findings {
                        f.summary = format!(
                            "retry attempt {a} changed a result that attempt 1 delivered intact: {}",
                            f.summary
                        );
                        f.relabel(FailureClass::RetryMutation);
                    }
                }
            }
            if replayed {
                for f in &mut call_findings {
                    f.summary = format!("replayed history changed a previously delivered result: {}", f.summary);
                    f.relabel(FailureClass::ReplayMutation);
                }
            }
            findings.extend(call_findings);
            provider_sides.entry(call.clone()).or_default().push(side);
        }

        // Ordering is informational: concurrent results may arrive in any order.
        let order: Vec<&str> = tool_msgs
            .iter()
            .filter_map(|m| m.tool_call_id.as_deref())
            .filter(|id| req.issued_before.iter().any(|c| c == id))
            .collect();
        let mut sorted = order.clone();
        sorted.sort_by_key(|id| req.issued_before.iter().position(|c| c == id));
        if order != sorted {
            notes.push(format!("request #{}: tool results arrived in order {order:?} (legal reordering)", req.seq));
        }
        // Tool results under identifiers the provider never issued.
        for m in &tool_msgs {
            if m.tool_call_id.as_ref().is_some_and(|id| req.issued_before.contains(id)) {
                continue;
            }
            let content = m.content.text().map(str::as_bytes);
            let owner = expected.iter().find(|(_, o)| Some(o.extracted_content.as_slice()) == content).map(|(c, _)| c);
            let label = m
                .tool_call_id
                .as_deref()
                .map(|id| format!("unissued tool_call_id {id}"))
                .unwrap_or_else(|| "no tool_call_id".into());
            match owner {
                // Owner has no message of its own: already reported as WrongToolCallAssociation.
                Some(c) if !tool_msgs.iter().any(|t| t.tool_call_id.as_deref() == Some(c.as_str())) => {}
                Some(c) => {
                    let mut f = Finding::new(
                        FailureClass::DuplicateResult,
                        c,
                        req.seq,
                        attempt,
                        format!("result of {c} is sent again in message {} with {label}", m.index),
                    );
                    f.provider_tool_call_id = m.tool_call_id.clone();
                    findings.push(f);
                }
                None => unknowns.push(UnknownNote::new(
                    UnknownReason::AmbiguousCorrelation,
                    format!(
                        "request #{} message {}: tool result with {label} matches no MCP emission",
                        req.seq, m.index
                    ),
                )),
            }
        }
    }
    if !machine.rejected_paths.is_empty() {
        notes.push(format!("rejected requests outside the active run/endpoint: {:?}", machine.rejected_paths));
    }

    let completed = machine.phase == Phase::Complete;
    if !completed && unknowns.is_empty() {
        unknowns.push(UnknownNote::new(
            UnknownReason::ExpectedTransitionMissing,
            format!("scenario state machine stopped in {}", machine.phase.label()),
        ));
    }
    dedup_notes(&mut notes);
    let mut classifications: Vec<FailureClass> = findings.iter().map(|f| f.class).collect();
    classifications.sort();
    classifications.dedup();
    let mut underlying_classifications: Vec<FailureClass> =
        findings.iter().filter_map(|f| f.underlying_class).collect();
    underlying_classifications.sort();
    underlying_classifications.dedup();
    let calls = def
        .call_ids()
        .into_iter()
        .map(|c| CallReport {
            call_id: c.to_owned(),
            tool: tool_sides.remove(c),
            provider: provider_sides.remove(c).unwrap_or_default(),
        })
        .collect();
    Evaluation {
        verdict: decide(!findings.is_empty(), !unknowns.is_empty()),
        classifications,
        underlying_classifications,
        findings,
        unknowns,
        notes,
        calls,
        final_phase: machine.phase.label(),
    }
}

fn dedup_notes(notes: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    notes.retain(|n| seen.insert(n.clone()));
}

fn missing_finding(
    call: &str,
    exp: &ToolObservation,
    seq: u64,
    attempt: Option<u32>,
    tool_msgs: &[&ParsedMessage],
    all: &[ParsedMessage],
    expected: &BTreeMap<String, ToolObservation>,
) -> Finding {
    let bytes = exp.extracted_content.as_slice();
    // Exact content under a different (or absent) identifier proves an association error.
    if let Some(m) = tool_msgs
        .iter()
        .find(|m| m.tool_call_id.as_deref() != Some(call) && m.content.text().map(str::as_bytes) == Some(bytes))
    {
        let mut f = Finding::new(
            FailureClass::WrongToolCallAssociation,
            call,
            seq,
            attempt,
            match &m.tool_call_id {
                Some(id) => format!("result of {call} was delivered under tool_call_id {id}"),
                None => format!("result of {call} was delivered without a tool_call_id"),
            },
        );
        f.provider_tool_call_id = m.tool_call_id.clone();
        f.content_matches_call = Some(call.to_owned());
        return f;
    }
    let elsewhere = all
        .iter()
        .find(|m| m.role != "tool" && m.content.text().is_some_and(|t| payload::contains(t.as_bytes(), bytes)));
    let summary = match elsewhere {
        Some(m) => format!(
            "no tool message for {call}; its exact content appears inside a `{}` message (index {})",
            m.role, m.index
        ),
        None => {
            let other_ids: Vec<&str> = tool_msgs
                .iter()
                .filter_map(|m| m.tool_call_id.as_deref())
                .filter(|id| !expected.contains_key(*id))
                .collect();
            if other_ids.is_empty() {
                format!("no tool message for {call} reached the provider")
            } else {
                format!("no tool message for {call}; unrecognised tool_call_ids present: {other_ids:?}")
            }
        }
    };
    Finding::new(FailureClass::MissingResult, call, seq, attempt, summary)
}

/// Compare one provider-visible occurrence with the MCP emission.
/// Returns (extracted_text state, json_semantics state, finding).
fn compare_occurrence(
    def: &ScenarioDef,
    exp: &ToolObservation,
    obs: &ProviderObservation,
    body_valid_utf8: bool,
    expected: &BTreeMap<String, ToolObservation>,
    seq: u64,
    attempt: Option<u32>,
) -> (&'static str, &'static str, Option<Finding>) {
    let call = exp.call_id.as_str();
    let (e, a) = (exp.extracted_content.as_slice(), obs.extracted_content.as_slice());
    let is_json = def.kind == ContentKind::Json;
    if e == a {
        return ("identical", if is_json { "identical" } else { "not-applicable" }, None);
    }
    // Exact content of another call under this ID: association error, not a mutation.
    if let Some((other, _)) = expected.iter().find(|(id, o)| id.as_str() != call && o.extracted_content == a) {
        let mut f = Finding::new(
            FailureClass::WrongToolCallAssociation,
            call,
            seq,
            attempt,
            format!("tool_call_id {call} carries the exact result of {other}"),
        );
        f.provider_tool_call_id = Some(call.to_owned());
        f.content_matches_call = Some(other.clone());
        return ("changed", if is_json { "changed" } else { "not-applicable" }, Some(f));
    }
    let text_diff = classify_text(e, a);
    let missing = payload::missing_sentinels(call, e, a);
    if is_json {
        let exp_json = exp.json.as_ref();
        let actual_text = std::str::from_utf8(a).ok();
        let duplicate = actual_text
            .and_then(json::duplicate_key)
            .filter(|_| std::str::from_utf8(e).ok().and_then(json::duplicate_key).is_none());
        if let Some(path) = duplicate {
            let mut f = Finding::new(
                FailureClass::StructuralMutation,
                call,
                seq,
                attempt,
                format!("JSON object key {path} appears twice; its value depends on the parser"),
            );
            f.json_difference = Some(JsonDiff {
                path,
                kind: "duplicate-key",
                expected: "(key appears once)".into(),
                actual: "(key repeated)".into(),
            });
            f.text_difference = text_diff;
            return ("changed", "changed", Some(f));
        }
        match actual_text.and_then(|s| serde_json::from_str::<Value>(s).ok()) {
            Some(actual) => match exp_json.and_then(|j| json::first_diff(j, &actual)) {
                None if exp_json.is_some() => return ("changed", "identical", None),
                None => {}
                Some(d) => {
                    let mut f = Finding::new(
                        FailureClass::StructuralMutation,
                        call,
                        seq,
                        attempt,
                        format!(
                            "JSON value changed at {} ({}): expected {}, got {}",
                            d.path, d.kind, d.expected, d.actual
                        ),
                    );
                    f.json_difference = Some(d);
                    f.text_difference = text_diff;
                    return ("changed", "changed", Some(f));
                }
            },
            None => {
                let mut f = Finding::new(
                    FailureClass::InvalidJson,
                    call,
                    seq,
                    attempt,
                    format!(
                        "provider-visible content is not valid JSON ({})",
                        text_diff.as_ref().map(|d| d.label.as_str()).unwrap_or("changed")
                    ),
                );
                f.text_difference = text_diff;
                return ("changed", "invalid-json", Some(f));
            }
        }
    }
    let Some(mut d) = text_diff else { return ("identical", "not-applicable", None) };
    if !body_valid_utf8 {
        d.class = FailureClass::InvalidUtf8;
        d.label.push_str("; provider request body was not valid UTF-8");
    }
    let mut f = Finding::new(
        d.class,
        call,
        seq,
        attempt,
        format!(
            "{}: tool {} bytes, provider {} bytes, first difference at byte {}",
            d.label, d.tool_bytes, d.provider_bytes, d.first_difference
        ),
    );
    f.missing_sentinels = missing;
    f.text_difference = Some(d);
    ("changed", if is_json { "changed" } else { "not-applicable" }, Some(f))
}
