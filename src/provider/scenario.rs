//! Provider-side scenario state machine.
//!
//! ```text
//! AwaitInitial --(initial request)--> issue turn 1 calls --> AwaitResults{0}
//! AwaitResults{t} --(result request)--> [429 once if retry scenario]
//!                                   --> issue turn t+1 calls | final answer --> Complete
//! anything unexpected --> Aborted (scenario becomes UNKNOWN)
//! ```

use crate::model::verdict::{UnknownNote, UnknownReason};
use crate::provider::protocol::{self, ParsedRequest, PlannedCall, Protocol};
use crate::provider::recorder::{RequestRecord, RequestRole};
use crate::provider::responses;
use crate::scenario::{self, Fault, ReplayMode, ScenarioDef, CHECKPOINT_TEXT, FINAL_TEXT, REPLAY_TEXT, TOOL_NAME};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    AwaitInitial,
    AwaitResults { turn: usize },
    AwaitReplay { resumed: bool },
    Complete,
    Aborted,
}

impl Phase {
    pub fn label(&self) -> String {
        match self {
            Phase::AwaitInitial => "WAIT_INITIAL_REQUEST".into(),
            Phase::AwaitResults { turn } => format!("WAIT_TOOL_RESULT(turn={})", turn + 1),
            Phase::AwaitReplay { resumed } => {
                if *resumed {
                    "WAIT_RESUMED_HISTORY".into()
                } else {
                    "WAIT_REPLAYED_HISTORY".into()
                }
            }
            Phase::Complete => "COMPLETE".into(),
            Phase::Aborted => "ABORTED".into(),
        }
    }
}

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
    /// How the HTTP layer delivers the reply (transport-fault scenarios).
    pub delivery: Delivery,
}

/// Transport-level behavior for one reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Normal,
    /// Close the connection without sending anything.
    Close,
    /// Reset the TCP connection without sending anything.
    Reset,
    /// Send the headers (with the full Content-Length) and half the body, then close.
    Truncate,
    /// Send normally after a delay.
    Delay(std::time::Duration),
}

impl Reply {
    fn json(status: u16, v: &Value) -> Reply {
        Reply {
            status,
            content_type: "application/json",
            headers: vec![],
            body: serde_json::to_vec(v).unwrap(),
            delivery: Delivery::Normal,
        }
    }
}

pub struct ScenarioMachine {
    pub run_id: String,
    pub def: &'static ScenarioDef,
    pub phase: Phase,
    pub tool_name: Option<String>,
    pub issued: Vec<String>,
    pub requests: Vec<RequestRecord>,
    pub unknowns: Vec<UnknownNote>,
    pub rejected_paths: Vec<String>,
    pub protocol: Protocol,
    /// Responses API: the conversation of every response sent, by response id,
    /// so that a later `previous_response_id` can be resolved.
    response_store: HashMap<String, Vec<Value>>,
    /// Responses API: output items of the reply being built.
    last_output: Vec<Value>,
    /// Fault replies already sent for this scenario.
    faults_sent: u32,
    attempts: u32,
    started: Instant,
    seq: u64,
}

impl ScenarioMachine {
    /// A machine speaking OpenAI Chat Completions.
    pub fn new(run_id: &str, def: &'static ScenarioDef) -> Self {
        Self::with_protocol(run_id, def, Protocol::ChatCompletions)
    }

    pub fn with_protocol(run_id: &str, def: &'static ScenarioDef, protocol: Protocol) -> Self {
        ScenarioMachine {
            protocol,
            response_store: HashMap::new(),
            last_output: vec![],
            run_id: run_id.to_owned(),
            def,
            phase: Phase::AwaitInitial,
            tool_name: None,
            issued: vec![],
            requests: vec![],
            unknowns: vec![],
            rejected_paths: vec![],
            faults_sent: 0,
            attempts: 0,
            started: Instant::now(),
            seq: 0,
        }
    }

    pub fn base_path(&self) -> String {
        format!("/{}/{}/v1", self.run_id, self.def.id)
    }

    pub fn first_request_seen(&self) -> bool {
        self.requests.iter().any(|r| r.role != RequestRole::Rejected)
    }

    pub fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Complete | Phase::Aborted)
    }

    fn unknown(&mut self, reason: UnknownReason, detail: String) {
        self.unknowns.push(UnknownNote::new(reason, detail));
    }

    /// Requests retained per scenario; later ones are refused (runaway runtime).
    pub const MAX_REQUESTS: usize = 256;

    pub fn handle(
        &mut self,
        method: &str,
        path: &str,
        raw: Vec<u8>,
        raw_truncated: bool,
        headers: Option<Vec<(String, String)>>,
    ) -> Reply {
        if self.requests.len() >= Self::MAX_REQUESTS {
            if self.phase != Phase::Aborted {
                self.unknown(
                    UnknownReason::UnexpectedTransition,
                    format!("more than {} provider requests; further requests refused", Self::MAX_REQUESTS),
                );
                self.phase = Phase::Aborted;
            }
            return Reply::json(
                429,
                &protocol::error_body("boundarycheck request limit reached", "invalid_request_error"),
            );
        }
        self.seq += 1;
        let seq = self.seq;
        let mut rec = RequestRecord {
            seq,
            received_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0),
            offset_ms: self.started.elapsed().as_millis(),
            method: method.to_owned(),
            path: path.to_owned(),
            raw,
            raw_truncated,
            headers,
            role: RequestRole::Auxiliary,
            issued_before: self.issued.clone(),
            response_status: 200,
            response_kind: "",
            parsed: None,
            parse_error: None,
        };
        let reply = self.route(&mut rec);
        rec.response_status = reply.status;
        self.requests.push(rec);
        reply
    }

    fn route(&mut self, rec: &mut RequestRecord) -> Reply {
        let path = rec.path.split('?').next().unwrap_or("").trim_end_matches('/').to_owned();
        let base = self.base_path();
        let Some(rest) = path.strip_prefix(&base) else {
            rec.role = RequestRole::Rejected;
            rec.raw.clear(); // never keep bodies of requests outside the active run
            rec.response_kind = "rejected-foreign-path";
            self.rejected_paths.push(format!("{} {}", rec.method, rec.path));
            return Reply::json(
                403,
                &protocol::error_body(
                    "request does not belong to the active boundarycheck run",
                    "invalid_request_error",
                ),
            );
        };
        match (rec.method.as_str(), rest) {
            ("GET", "/models") => {
                rec.response_kind = "models";
                Reply::json(200, &protocol::models_body())
            }
            ("POST", endpoint) if endpoint == self.protocol.endpoint() => self.model_request(rec),
            _ => {
                rec.role = RequestRole::Rejected;
                rec.response_kind = "rejected-unsupported-endpoint";
                self.rejected_paths.push(format!("{} {}", rec.method, rec.path));
                self.unknown(
                    UnknownReason::UnsupportedRequestShape,
                    format!(
                        "request #{} to unsupported endpoint {} {} (the adapter's protocol {} uses POST {base}{})",
                        rec.seq,
                        rec.method,
                        rec.path,
                        self.protocol.name(),
                        self.protocol.endpoint()
                    ),
                );
                Reply::json(404, &protocol::error_body("unsupported endpoint", "invalid_request_error"))
            }
        }
    }

    fn model_request(&mut self, rec: &mut RequestRecord) -> Reply {
        if rec.raw_truncated {
            rec.response_kind = "body-too-large";
            self.unknown(
                UnknownReason::ProviderRequestUnparseable,
                format!("request #{} exceeded the body size limit", rec.seq),
            );
            self.phase = Phase::Aborted;
            return Reply::json(413, &protocol::error_body("body too large", "invalid_request_error"));
        }
        let parsed = match self.protocol {
            Protocol::ChatCompletions => protocol::parse_request(&rec.raw).map(|p| (p, vec![])),
            Protocol::Responses => responses::parse_request(&rec.raw, |id| self.response_store.get(id).cloned())
                .map(|r| (r.parsed, r.items)),
        };
        let (parsed, items) = match parsed {
            Ok(p) => p,
            Err(e) => {
                rec.response_kind = "unparseable";
                rec.parse_error = Some(e.clone());
                self.unknown(UnknownReason::ProviderRequestUnparseable, format!("request #{}: {e}", rec.seq));
                self.phase = Phase::Aborted;
                return Reply::json(400, &protocol::error_body(&e, "invalid_request_error"));
            }
        };
        self.last_output.clear();
        let reply = self.transition(rec, &parsed);
        if self.protocol == Protocol::Responses
            && reply.status == 200
            && matches!(reply.delivery, Delivery::Normal | Delivery::Delay(_))
        {
            let mut conversation = items;
            conversation.append(&mut self.last_output);
            self.response_store.insert(responses::response_id(rec.seq), conversation);
        }
        rec.parsed = Some(parsed);
        reply
    }

    fn transition(&mut self, rec: &mut RequestRecord, req: &ParsedRequest) -> Reply {
        let seq = rec.seq;
        match self.phase {
            Phase::AwaitInitial => {
                rec.role = RequestRole::Initial;
                if req.messages.iter().any(|m| m.role == "tool") {
                    self.unknown(
                        UnknownReason::UnexpectedTransition,
                        format!("initial request #{seq} already contains tool messages"),
                    );
                    return self.abort(rec, req);
                }
                match discover_tool(&req.tool_names) {
                    Ok(name) => self.tool_name = Some(name),
                    Err(detail) => {
                        let reason = if req.tool_names.iter().filter(|n| matches_tool(n)).count() > 1 {
                            UnknownReason::AmbiguousCorrelation
                        } else {
                            UnknownReason::AdapterInsufficient
                        };
                        self.unknown(reason, format!("request #{seq}: {detail}"));
                        return self.abort(rec, req);
                    }
                }
                self.issue_turn(rec, req, 0)
            }
            Phase::AwaitResults { turn } => {
                self.attempts += 1;
                rec.role = RequestRole::Result { turn, attempt: self.attempts };
                if self.faults_sent < self.def.fault.failures() {
                    self.faults_sent += 1;
                    return self.fault_reply(rec, req);
                }
                let mut reply = self.answer_results(rec, req, turn);
                if let Fault::Delay { ms } = self.def.fault {
                    if self.attempts == 1 {
                        rec.response_kind = "delayed";
                        reply.delivery = Delivery::Delay(std::time::Duration::from_millis(ms));
                    }
                }
                reply
            }
            Phase::AwaitReplay { resumed } => {
                rec.role = RequestRole::Replay { resumed };
                self.phase = Phase::Complete;
                self.final_answer(rec, req)
            }
            Phase::Complete | Phase::Aborted => {
                rec.role = RequestRole::AfterCompletion;
                if self.phase == Phase::Complete {
                    self.unknown(
                        UnknownReason::UnexpectedTransition,
                        format!("request #{seq} arrived after the scenario completed"),
                    );
                }
                self.final_answer(rec, req)
            }
        }
    }

    /// The deterministic failure for this scenario. The phase does not
    /// advance, so the runtime's retry is handled as the next attempt.
    fn fault_reply(&mut self, rec: &mut RequestRecord, req: &ParsedRequest) -> Reply {
        match self.def.fault {
            Fault::Status { code, .. } => {
                rec.response_kind = match code {
                    429 => "fault-http-429",
                    500 => "fault-http-500",
                    503 => "fault-http-503",
                    _ => "fault-http-status",
                };
                let body = if code == 429 {
                    protocol::rate_limit_body()
                } else {
                    protocol::error_body("boundarycheck deterministic server error (retry expected)", "server_error")
                };
                let mut r = Reply::json(code, &body);
                r.headers.push(("retry-after-ms", "50".into()));
                r.headers.push(("retry-after", "1".into()));
                r
            }
            Fault::Disconnect => {
                rec.response_kind = "fault-disconnect";
                Reply { delivery: Delivery::Close, ..Reply::json(200, &Value::Null) }
            }
            Fault::Reset => {
                rec.response_kind = "fault-reset";
                Reply { delivery: Delivery::Reset, ..Reply::json(200, &Value::Null) }
            }
            Fault::Truncate => {
                // A well-formed final answer, cut off halfway by the HTTP layer.
                let model = req.model.as_deref().unwrap_or("boundarycheck-model");
                let mut r = match (self.protocol, req.stream) {
                    (Protocol::ChatCompletions, true) => {
                        sse(protocol::final_stream(rec.seq, model, FINAL_TEXT, req.include_usage))
                    }
                    (Protocol::ChatCompletions, false) => {
                        Reply::json(200, &protocol::final_response(rec.seq, model, FINAL_TEXT))
                    }
                    (Protocol::Responses, true) => {
                        sse(responses::stream(rec.seq, model, &responses::message_items(rec.seq, FINAL_TEXT)))
                    }
                    (Protocol::Responses, false) => Reply::json(
                        200,
                        &responses::response(
                            rec.seq,
                            model,
                            &responses::message_items(rec.seq, FINAL_TEXT),
                            "completed",
                        ),
                    ),
                };
                rec.response_kind = "fault-truncated";
                r.delivery = Delivery::Truncate;
                r
            }
            Fault::None | Fault::Delay { .. } => unreachable!("not a failing fault"),
        }
    }

    /// The normal answer to a result request: the next turn, a replay marker, or the final answer.
    fn answer_results(&mut self, rec: &mut RequestRecord, req: &ParsedRequest, turn: usize) -> Reply {
        if turn + 1 < self.def.turns.len() {
            self.issue_turn(rec, req, turn + 1)
        } else if self.def.replay != ReplayMode::None {
            let resumed = self.def.replay == ReplayMode::Resume;
            self.phase = Phase::AwaitReplay { resumed };
            let marker = if resumed { CHECKPOINT_TEXT } else { REPLAY_TEXT };
            self.marker_answer(rec, req, marker)
        } else {
            self.phase = Phase::Complete;
            self.final_answer(rec, req)
        }
    }

    fn abort(&mut self, rec: &mut RequestRecord, req: &ParsedRequest) -> Reply {
        self.phase = Phase::Aborted;
        self.final_answer(rec, req)
    }

    fn issue_turn(&mut self, rec: &mut RequestRecord, req: &ParsedRequest, turn: usize) -> Reply {
        let name = self.tool_name.clone().unwrap_or_else(|| TOOL_NAME.to_owned());
        let calls: Vec<PlannedCall> = self.def.turns[turn]
            .iter()
            .map(|id| PlannedCall {
                id: (*id).to_owned(),
                name: name.clone(),
                arguments: scenario::tool_arguments(&self.run_id, self.def.id, id).to_string(),
            })
            .collect();
        self.issued.extend(calls.iter().map(|c| c.id.clone()));
        self.phase = Phase::AwaitResults { turn };
        self.attempts = 0;
        rec.response_kind = "tool-calls";
        let model = req.model.as_deref().unwrap_or("boundarycheck-model");
        match (self.protocol, req.stream) {
            (Protocol::ChatCompletions, true) => {
                sse(protocol::tool_calls_stream(rec.seq, model, &calls, req.include_usage))
            }
            (Protocol::ChatCompletions, false) => {
                Reply::json(200, &protocol::tool_calls_response(rec.seq, model, &calls))
            }
            (Protocol::Responses, stream) => {
                self.last_output = responses::tool_call_items(rec.seq, &calls);
                self.responses_reply(rec.seq, model, stream)
            }
        }
    }

    fn final_answer(&mut self, rec: &mut RequestRecord, req: &ParsedRequest) -> Reply {
        self.marker_answer(rec, req, FINAL_TEXT)
    }

    fn marker_answer(&mut self, rec: &mut RequestRecord, req: &ParsedRequest, text: &str) -> Reply {
        rec.response_kind = "final-answer";
        let model = req.model.as_deref().unwrap_or("boundarycheck-model");
        match (self.protocol, req.stream) {
            (Protocol::ChatCompletions, true) => sse(protocol::final_stream(rec.seq, model, text, req.include_usage)),
            (Protocol::ChatCompletions, false) => Reply::json(200, &protocol::final_response(rec.seq, model, text)),
            (Protocol::Responses, stream) => {
                self.last_output = responses::message_items(rec.seq, text);
                self.responses_reply(rec.seq, model, stream)
            }
        }
    }

    fn responses_reply(&self, seq: u64, model: &str, stream: bool) -> Reply {
        if stream {
            sse(responses::stream(seq, model, &self.last_output))
        } else {
            Reply::json(200, &responses::response(seq, model, &self.last_output, "completed"))
        }
    }
}

fn sse(body: String) -> Reply {
    Reply {
        status: 200,
        content_type: "text/event-stream",
        headers: vec![("cache-control", "no-cache".into())],
        delivery: Delivery::Normal,
        body: body.into_bytes(),
    }
}

/// Runtimes may namespace MCP tools (e.g. `server__boundary_test`). Accept the
/// exact name or a suffix after a non-alphanumeric separator; anything else,
/// or more than one candidate, is not guessed.
fn matches_tool(name: &str) -> bool {
    name == TOOL_NAME
        || name.strip_suffix(TOOL_NAME).and_then(|p| p.chars().last()).is_some_and(|c| !c.is_ascii_alphanumeric())
}

pub fn discover_tool(names: &[String]) -> Result<String, String> {
    if names.iter().any(|n| n == TOOL_NAME) {
        return Ok(TOOL_NAME.to_owned());
    }
    let candidates: Vec<&String> = names.iter().filter(|n| matches_tool(n)).collect();
    match candidates.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("the runtime did not offer the `{TOOL_NAME}` tool to the provider (offered: {names:?})")),
        many => Err(format!("several tools could be `{TOOL_NAME}`: {many:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(msgs: &str) -> Vec<u8> {
        format!(r#"{{"model":"m","messages":[{msgs}],"tools":[{{"type":"function","function":{{"name":"boundary_test"}}}}]}}"#).into_bytes()
    }

    #[test]
    fn exact_text_flow() {
        let mut m = ScenarioMachine::new("BC_RUN_000001", scenario::find("exact-text").unwrap());
        let p = "/BC_RUN_000001/exact-text/v1/chat/completions";
        let r = m.handle("POST", p, body(r#"{"role":"user","content":"go"}"#), false, None);
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v.pointer("/choices/0/message/tool_calls/0/id").unwrap(), "BC_CALL_000001");
        assert_eq!(m.phase, Phase::AwaitResults { turn: 0 });
        m.handle("POST", p, body(r#"{"role":"tool","tool_call_id":"BC_CALL_000001","content":"x"}"#), false, None);
        assert_eq!(m.phase, Phase::Complete);
        assert_eq!(m.requests[1].issued_before, vec!["BC_CALL_000001"]);
        m.handle("POST", p, body(r#"{"role":"user","content":"again"}"#), false, None);
        assert_eq!(m.unknowns[0].reason, UnknownReason::UnexpectedTransition);
    }

    #[test]
    fn retry_scenario_rate_limits_once() {
        let mut m = ScenarioMachine::new("BC_RUN_000001", scenario::find("retry-429").unwrap());
        let p = "/BC_RUN_000001/retry-429/v1/chat/completions";
        m.handle("POST", p, body(r#"{"role":"user","content":"go"}"#), false, None);
        assert_eq!(
            m.handle("POST", p, body(r#"{"role":"tool","tool_call_id":"BC_CALL_000001","content":"x"}"#), false, None)
                .status,
            429
        );
        assert_eq!(
            m.handle("POST", p, body(r#"{"role":"tool","tool_call_id":"BC_CALL_000001","content":"x"}"#), false, None)
                .status,
            200
        );
        assert_eq!(m.requests[2].role, RequestRole::Result { turn: 0, attempt: 2 });
        assert_eq!(m.phase, Phase::Complete);
    }

    #[test]
    fn replay_and_resume_require_a_second_history_submission() {
        for (id, resumed) in [("replay-history", false), ("persistence-resume", true)] {
            let mut m = ScenarioMachine::new("BC_RUN_000001", scenario::find(id).unwrap());
            let p = format!("/BC_RUN_000001/{id}/v1/chat/completions");
            m.handle("POST", &p, body(r#"{"role":"user","content":"go"}"#), false, None);
            let result = body(r#"{"role":"tool","tool_call_id":"BC_CALL_000001","content":"x"}"#);
            m.handle("POST", &p, result.clone(), false, None);
            assert_eq!(m.phase, Phase::AwaitReplay { resumed });
            m.handle("POST", &p, result, false, None);
            assert_eq!(m.requests[2].role, RequestRole::Replay { resumed });
            assert_eq!(m.phase, Phase::Complete);
        }
    }

    #[test]
    fn rejects_foreign_runs_and_missing_tool() {
        let mut m = ScenarioMachine::new("BC_RUN_000001", scenario::find("exact-text").unwrap());
        assert_eq!(
            m.handle("POST", "/BC_RUN_000002/exact-text/v1/chat/completions", b"{}".to_vec(), false, None).status,
            403
        );
        assert!(m.requests[0].raw.is_empty());
        assert!(!m.first_request_seen());
        let r = m.handle(
            "POST",
            "/BC_RUN_000001/exact-text/v1/chat/completions",
            br#"{"messages":[{"role":"user","content":"x"}]}"#.to_vec(),
            false,
            None,
        );
        assert_eq!(r.status, 200);
        assert_eq!(m.phase, Phase::Aborted);
        assert_eq!(m.unknowns[0].reason, UnknownReason::AdapterInsufficient);
    }

    #[test]
    fn tool_discovery() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(discover_tool(&s(&["boundary_test"])).unwrap(), "boundary_test");
        assert_eq!(discover_tool(&s(&["mcp__bc__boundary_test"])).unwrap(), "mcp__bc__boundary_test");
        assert!(discover_tool(&s(&["myboundary_test"])).is_err());
        assert!(discover_tool(&s(&["a_boundary_test", "b_boundary_test"])).is_err());
    }
}
