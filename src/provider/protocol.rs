//! OpenAI Chat Completions (`POST {base}/chat/completions`) — the single
//! provider protocol supported in V1. See README "Provider protocol" for the
//! exact supported request and response shapes.

use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};

/// Name of the Chat Completions protocol (the default).
pub const PROTOCOL: &str = "openai-chat-completions";

/// The provider protocols boundarycheck implements. Each one has its own
/// endpoint, request parser and response builders; the comparison engine
/// only sees the protocol-neutral [`ParsedRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Protocol {
    /// `POST {base}/chat/completions`
    ChatCompletions,
    /// `POST {base}/responses`
    Responses,
}

impl Protocol {
    pub const ALL: [Protocol; 2] = [Protocol::ChatCompletions, Protocol::Responses];

    pub fn name(self) -> &'static str {
        match self {
            Protocol::ChatCompletions => PROTOCOL,
            Protocol::Responses => "openai-responses",
        }
    }

    pub fn from_name(name: &str) -> Option<Protocol> {
        Protocol::ALL.into_iter().find(|p| p.name() == name)
    }

    pub fn endpoint(self) -> &'static str {
        match self {
            Protocol::ChatCompletions => "/chat/completions",
            Protocol::Responses => "/responses",
        }
    }

    /// Which MCP block types a tool result can carry in this protocol.
    pub fn representable(self) -> crate::model::content::Representable {
        use crate::model::content::Representable;
        match self {
            Protocol::ChatCompletions => Representable::CHAT_COMPLETIONS,
            Protocol::Responses => Representable::RESPONSES,
        }
    }
}

/// One element of a message's content, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    /// An image, identified by the SHA-256 of its decoded `data:` URL bytes when inline.
    Image {
        url: String,
        sha256: Option<String>,
    },
    Other(String),
}

impl Part {
    pub fn image(url: &str) -> Part {
        let sha256 = url.strip_prefix("data:").and_then(|rest| rest.split_once(";base64,")).and_then(|(_, data)| {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(data).ok().map(|b| crate::compare::content::sha256_hex(&b))
        });
        Part::Image { url: url.to_owned(), sha256 }
    }
}

/// Deterministic `created` timestamp used in every response.
const CREATED: u64 = 1_700_000_000;

#[derive(Debug, Clone)]
pub struct ParsedRequest {
    pub model: Option<String>,
    pub stream: bool,
    pub include_usage: bool,
    pub parallel_tool_calls: Option<bool>,
    pub tool_names: Vec<String>,
    pub messages: Vec<ParsedMessage>,
    /// False when the body was not valid UTF-8 and was parsed lossily.
    pub body_valid_utf8: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedMessage {
    pub index: usize,
    pub role: String,
    pub tool_call_id: Option<String>,
    pub content: MessageContent,
    /// Raw JSON token of `content` exactly as it appeared in the body.
    pub raw_content: Option<String>,
    /// IDs from an assistant message's `tool_calls`.
    pub tool_call_ids: Vec<String>,
    /// Content parts in order (text and non-text); empty for string content.
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MessageContent {
    /// String content, or the in-order concatenation of an array of text parts.
    Text {
        text: String,
        parts: usize,
    },
    Absent,
    Unsupported(String),
}

impl MessageContent {
    pub fn text(&self) -> Option<&str> {
        match self {
            MessageContent::Text { text, .. } => Some(text),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct RawRequest<'a> {
    #[serde(borrow, default)]
    messages: Vec<RawMessage<'a>>,
}

#[derive(Deserialize)]
struct RawMessage<'a> {
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
}

pub fn parse_request(raw: &[u8]) -> Result<ParsedRequest, String> {
    let (text, valid) = match std::str::from_utf8(raw) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), true),
        Err(_) => (String::from_utf8_lossy(raw), false),
    };
    let body: Value = serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))?;
    let raw_tokens: Vec<Option<String>> = serde_json::from_str::<RawRequest>(&text)
        .map(|r| r.messages.into_iter().map(|m| m.content.map(|c| c.get().to_owned())).collect())
        .unwrap_or_default();
    let obj = body.as_object().ok_or("request body is not a JSON object")?;
    let messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("request has no `messages` array (not a Chat Completions request)")?;
    let mut parsed = Vec::with_capacity(messages.len());
    for (index, m) in messages.iter().enumerate() {
        let role = m.get("role").and_then(Value::as_str).ok_or(format!("message {index} has no role"))?;
        let content = match m.get("content") {
            None | Some(Value::Null) => MessageContent::Absent,
            Some(Value::String(s)) => MessageContent::Text { text: s.clone(), parts: 0 },
            Some(Value::Array(parts)) => text_parts(parts),
            Some(other) => MessageContent::Unsupported(format!("content of type {}", json_type(other))),
        };
        let tool_call_ids = m
            .get("tool_calls")
            .and_then(Value::as_array)
            .map(|calls| calls.iter().filter_map(|c| c.get("id").and_then(Value::as_str)).map(str::to_owned).collect())
            .unwrap_or_default();
        let parts = match m.get("content") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|p| match (p.get("type").and_then(Value::as_str), p.get("text").and_then(Value::as_str)) {
                    (Some("text"), Some(t)) => Part::Text(t.to_owned()),
                    (Some("image_url"), _) => {
                        Part::image(p.pointer("/image_url/url").and_then(Value::as_str).unwrap_or(""))
                    }
                    (ty, _) => Part::Other(ty.unwrap_or("?").to_owned()),
                })
                .collect(),
            _ => vec![],
        };
        parsed.push(ParsedMessage {
            index,
            role: role.to_owned(),
            tool_call_id: m.get("tool_call_id").and_then(Value::as_str).map(str::to_owned),
            content,
            raw_content: raw_tokens.get(index).cloned().flatten(),
            tool_call_ids,
            parts,
        });
    }
    let tool_names = obj
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|t| t.get("type").and_then(Value::as_str).unwrap_or("function") == "function")
                .filter_map(|t| t.pointer("/function/name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(ParsedRequest {
        model: obj.get("model").and_then(Value::as_str).map(str::to_owned),
        stream: obj.get("stream").and_then(Value::as_bool).unwrap_or(false),
        include_usage: body.pointer("/stream_options/include_usage").and_then(Value::as_bool).unwrap_or(false),
        parallel_tool_calls: obj.get("parallel_tool_calls").and_then(Value::as_bool),
        tool_names,
        messages: parsed,
        body_valid_utf8: valid,
    })
}

fn text_parts(parts: &[Value]) -> MessageContent {
    let mut text = String::new();
    for p in parts {
        match (p.get("type").and_then(Value::as_str), p.get("text").and_then(Value::as_str)) {
            (Some("text"), Some(t)) => text.push_str(t),
            (ty, _) => return MessageContent::Unsupported(format!("content part of type {}", ty.unwrap_or("?"))),
        }
    }
    MessageContent::Text { text, parts: parts.len() }
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::Object(_) => "object",
        _ => "other",
    }
}

pub struct PlannedCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

fn completion_id(seq: u64) -> String {
    format!("chatcmpl-boundarycheck-{seq:06}")
}

fn usage() -> Value {
    json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0})
}

pub fn tool_calls_response(seq: u64, model: &str, calls: &[PlannedCall]) -> Value {
    let tool_calls: Vec<Value> = calls
        .iter()
        .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.arguments}}))
        .collect();
    json!({
        "id": completion_id(seq), "object": "chat.completion", "created": CREATED, "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": null, "tool_calls": tool_calls},
                     "finish_reason": "tool_calls", "logprobs": null}],
        "usage": usage()
    })
}

pub fn final_response(seq: u64, model: &str, text: &str) -> Value {
    json!({
        "id": completion_id(seq), "object": "chat.completion", "created": CREATED, "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                     "finish_reason": "stop", "logprobs": null}],
        "usage": usage()
    })
}

fn chunk(seq: u64, model: &str, delta: Value, finish: Option<&str>) -> String {
    let c = json!({
        "id": completion_id(seq), "object": "chat.completion.chunk", "created": CREATED, "model": model,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish, "logprobs": null}]
    });
    format!("data: {c}\n\n")
}

fn finish_stream(out: &mut String, seq: u64, model: &str, include_usage: bool) {
    if include_usage {
        let c = json!({"id": completion_id(seq), "object": "chat.completion.chunk", "created": CREATED,
                       "model": model, "choices": [], "usage": usage()});
        out.push_str(&format!("data: {c}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
}

pub fn tool_calls_stream(seq: u64, model: &str, calls: &[PlannedCall], include_usage: bool) -> String {
    let mut out = chunk(seq, model, json!({"role": "assistant", "content": null}), None);
    for (i, c) in calls.iter().enumerate() {
        out.push_str(&chunk(
            seq,
            model,
            json!({"tool_calls": [{"index": i, "id": c.id, "type": "function", "function": {"name": c.name, "arguments": ""}}]}),
            None,
        ));
        out.push_str(&chunk(
            seq,
            model,
            json!({"tool_calls": [{"index": i, "function": {"arguments": c.arguments}}]}),
            None,
        ));
    }
    out.push_str(&chunk(seq, model, json!({}), Some("tool_calls")));
    finish_stream(&mut out, seq, model, include_usage);
    out
}

pub fn final_stream(seq: u64, model: &str, text: &str, include_usage: bool) -> String {
    let mut out = chunk(seq, model, json!({"role": "assistant", "content": ""}), None);
    out.push_str(&chunk(seq, model, json!({"content": text}), None));
    out.push_str(&chunk(seq, model, json!({}), Some("stop")));
    finish_stream(&mut out, seq, model, include_usage);
    out
}

pub fn rate_limit_body() -> Value {
    json!({"error": {"message": "boundarycheck deterministic rate limit (retry expected)",
                     "type": "rate_limit_error", "param": null, "code": "rate_limit_exceeded"}})
}

pub fn error_body(message: &str, kind: &str) -> Value {
    json!({"error": {"message": message, "type": kind, "param": null, "code": null}})
}

pub fn models_body() -> Value {
    json!({"object": "list", "data": [{"id": "boundarycheck-model", "object": "model", "created": CREATED, "owned_by": "boundarycheck"}]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_messages_and_raw_tokens() {
        // A JSON escape for U+00E9, assembled at runtime: backslash + "u00e9".
        let esc = format!("{}u00e9", '\\');
        let body = r#"{"model":"m","messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":null,"tool_calls":[{"id":"BC_CALL_000001","type":"function","function":{"name":"boundary_test","arguments":"{}"}}]},
            {"role":"tool","tool_call_id":"BC_CALL_000001","content":"cafESC"},
            {"role":"tool","tool_call_id":"BC_CALL_000002","content":[{"type":"text","text":"a"},{"type":"text","text":"b"}]}
        ],"tools":[{"type":"function","function":{"name":"boundary_test","parameters":{}}}],"stream":true}"#
            .replace("ESC", &esc);
        let r = parse_request(body.as_bytes()).unwrap();
        assert!(r.stream && r.body_valid_utf8);
        assert_eq!(r.tool_names, vec!["boundary_test"]);
        assert_eq!(r.messages[1].tool_call_ids, vec!["BC_CALL_000001"]);
        assert_eq!(r.messages[2].content.text(), Some("caf\u{e9}"));
        assert_eq!(r.messages[2].raw_content.as_deref(), Some(format!("\"caf{esc}\"").as_str()));
        assert_eq!(r.messages[3].content, MessageContent::Text { text: "ab".into(), parts: 2 });
    }

    #[test]
    fn rejects_unsupported_shapes() {
        assert!(parse_request(br#"{"input":"responses api"}"#).is_err());
        assert!(parse_request(b"not json").is_err());
        let r = parse_request(br#"{"messages":[{"role":"tool","tool_call_id":"x","content":[{"type":"image_url"}]}]}"#)
            .unwrap();
        assert!(matches!(r.messages[0].content, MessageContent::Unsupported(_)));
    }

    #[test]
    fn lossy_parse_flags_invalid_utf8() {
        let mut body = br#"{"messages":[{"role":"tool","tool_call_id":"x","content":"ab"#.to_vec();
        body.push(0xF0);
        body.extend_from_slice(br#""}]}"#);
        let r = parse_request(&body).unwrap();
        assert!(!r.body_valid_utf8);
        assert_eq!(r.messages[0].content.text(), Some("ab\u{fffd}"));
    }
}
