//! OpenAI Responses API (`POST {base}/responses`).
//!
//! Supported request shape: `input` is a string or an array of items:
//!
//! - messages: `{"role": ..., "content": string | [{"type": "input_text" | "output_text", "text"}, {"type": "input_image", "image_url"}]}`
//!   (with or without `"type": "message"`);
//! - `{"type": "function_call", "call_id", "name", "arguments"}` (the assistant's echo of a call);
//! - `{"type": "function_call_output", "call_id", "output": string | [input_text | input_image | input_file items]}`.
//!
//! Other item types (for example `reasoning`) are kept as opaque entries.
//! `tools[]` entries are `{"type": "function", "name", ...}`. When the request
//! names a `previous_response_id`, the conversation the model sees is the
//! stored conversation of that response followed by this request's `input`;
//! that effective conversation is what gets evaluated.
//!
//! Everything is mapped onto the protocol-neutral [`ParsedRequest`], so the
//! comparison engine does not depend on this module.

use crate::provider::protocol::{MessageContent, ParsedMessage, ParsedRequest, Part, PlannedCall};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};

const CREATED: u64 = 1_700_000_000;

pub fn response_id(seq: u64) -> String {
    format!("resp_boundarycheck_{seq:06}")
}

/// A parsed Responses request plus the conversation items it represents.
pub struct ParsedResponsesRequest {
    pub parsed: ParsedRequest,
    /// The effective conversation (stored history + this request's input), as JSON items.
    pub items: Vec<Value>,
    pub previous_response_id: Option<String>,
}

#[derive(Deserialize)]
struct RawRequest<'a> {
    #[serde(borrow, default)]
    input: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct RawItem<'a> {
    #[serde(borrow, default)]
    content: Option<&'a RawValue>,
    #[serde(borrow, default)]
    output: Option<&'a RawValue>,
}

/// Parse a request body. `history` resolves a `previous_response_id`;
/// `Err` is returned for an unparseable body or an unknown previous response.
pub fn parse_request(
    raw: &[u8],
    history: impl Fn(&str) -> Option<Vec<Value>>,
) -> Result<ParsedResponsesRequest, String> {
    let (text, valid) = match std::str::from_utf8(raw) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), true),
        Err(_) => (String::from_utf8_lossy(raw), false),
    };
    let body: Value = serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = body.as_object().ok_or("request body is not a JSON object")?;
    let input: Vec<Value> = match obj.get("input") {
        Some(Value::String(s)) => vec![json!({"role": "user", "content": s})],
        Some(Value::Array(items)) => items.clone(),
        _ => return Err("request has no `input` string or array (not a Responses request)".into()),
    };
    // Raw JSON tokens of `content` / `output` for this request's own input items.
    let raw_tokens: Vec<Option<String>> = serde_json::from_str::<RawRequest>(&text)
        .ok()
        .and_then(|r| r.input)
        .and_then(|i| serde_json::from_str::<Vec<RawItem>>(i.get()).ok())
        .map(|items| items.into_iter().map(|it| it.output.or(it.content).map(|t| t.get().to_owned())).collect())
        .unwrap_or_default();
    let previous_response_id = obj.get("previous_response_id").and_then(Value::as_str).map(str::to_owned);
    let mut items = match &previous_response_id {
        Some(id) => history(id).ok_or_else(|| format!("previous_response_id {id} is not a response of this run"))?,
        None => vec![],
    };
    let history_len = items.len();
    items.extend(input);
    let mut messages = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let raw_content = index.checked_sub(history_len).and_then(|i| raw_tokens.get(i).cloned().flatten());
        messages.push(message(index, item, raw_content)?);
    }
    let tool_names = obj
        .get("tools")
        .and_then(Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|t| t.get("type").and_then(Value::as_str) == Some("function"))
                .filter_map(|t| t.get("name").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(ParsedResponsesRequest {
        parsed: ParsedRequest {
            model: obj.get("model").and_then(Value::as_str).map(str::to_owned),
            stream: obj.get("stream").and_then(Value::as_bool).unwrap_or(false),
            include_usage: false,
            parallel_tool_calls: obj.get("parallel_tool_calls").and_then(Value::as_bool),
            tool_names,
            messages,
            body_valid_utf8: valid,
        },
        items,
        previous_response_id,
    })
}

fn message(index: usize, item: &Value, raw_content: Option<String>) -> Result<ParsedMessage, String> {
    let s = |k: &str| item.get(k).and_then(Value::as_str).map(str::to_owned);
    let kind = item.get("type").and_then(Value::as_str);
    let mut m = ParsedMessage {
        index,
        role: String::new(),
        tool_call_id: None,
        content: MessageContent::Absent,
        raw_content,
        tool_call_ids: vec![],
        parts: vec![],
    };
    match kind {
        Some("function_call") => {
            m.role = "assistant".into();
            m.tool_call_ids = s("call_id").into_iter().collect();
        }
        Some("function_call_output") => {
            m.role = "tool".into();
            m.tool_call_id = s("call_id");
            (m.content, m.parts) = content(item.get("output"));
        }
        None | Some("message") => {
            m.role = s("role").ok_or(format!("input item {index} has neither a type nor a role"))?;
            (m.content, m.parts) = content(item.get("content"));
        }
        Some(other) => m.role = format!("item:{other}"),
    }
    Ok(m)
}

/// Text content (all text parts, concatenated) plus every part in order.
fn content(v: Option<&Value>) -> (MessageContent, Vec<Part>) {
    match v {
        None | Some(Value::Null) => (MessageContent::Absent, vec![]),
        Some(Value::String(s)) => (MessageContent::Text { text: s.clone(), parts: 0 }, vec![]),
        Some(Value::Array(items)) => {
            let parts: Vec<Part> = items
                .iter()
                .map(|p| {
                    let ty = p.get("type").and_then(Value::as_str).unwrap_or("?");
                    match (ty, p.get("text").and_then(Value::as_str)) {
                        ("input_text" | "output_text" | "text", Some(t)) => Part::Text(t.to_owned()),
                        ("input_image", _) => Part::image(p.get("image_url").and_then(Value::as_str).unwrap_or("")),
                        _ => Part::Other(ty.to_owned()),
                    }
                })
                .collect();
            let text = parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text(t) => Some(t.as_str()),
                    _ => None,
                })
                .collect();
            (MessageContent::Text { text, parts: parts.len() }, parts)
        }
        Some(other) => (MessageContent::Unsupported(format!("content of type {other}")), vec![]),
    }
}

/// Output items for a turn that calls tools.
pub fn tool_call_items(seq: u64, calls: &[PlannedCall]) -> Vec<Value> {
    calls
        .iter()
        .enumerate()
        .map(|(i, c)| {
            json!({"type": "function_call", "id": format!("fc_boundarycheck_{seq:06}_{i}"), "call_id": c.id,
                   "name": c.name, "arguments": c.arguments, "status": "completed"})
        })
        .collect()
}

/// Output items for a final (or marker) text answer.
pub fn message_items(seq: u64, text: &str) -> Vec<Value> {
    vec![json!({
        "type": "message", "id": format!("msg_boundarycheck_{seq:06}"), "status": "completed", "role": "assistant",
        "content": [{"type": "output_text", "text": text, "annotations": [], "logprobs": []}]
    })]
}

/// A complete `response` object.
pub fn response(seq: u64, model: &str, output: &[Value], status: &str) -> Value {
    json!({
        "id": response_id(seq), "object": "response", "created_at": CREATED, "status": status,
        "model": model, "output": output, "parallel_tool_calls": true, "tool_choice": "auto", "tools": [],
        "error": null, "incomplete_details": null, "instructions": null, "metadata": {},
        "temperature": 1.0, "top_p": 1.0, "text": {"format": {"type": "text"}}, "truncation": "disabled",
        "usage": {"input_tokens": 0, "input_tokens_details": {"cached_tokens": 0},
                  "output_tokens": 0, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 0}
    })
}

/// The same response as a server-sent event stream.
pub fn stream(seq: u64, model: &str, output: &[Value]) -> String {
    let mut events: Vec<Value> = vec![];
    let in_progress = response(seq, model, &[], "in_progress");
    events.push(json!({"type": "response.created", "response": in_progress}));
    events.push(json!({"type": "response.in_progress", "response": in_progress}));
    for (i, item) in output.iter().enumerate() {
        let mut added = item.clone();
        added["status"] = json!("in_progress");
        match item["type"].as_str() {
            Some("function_call") => {
                added["arguments"] = json!("");
                events.push(json!({"type": "response.output_item.added", "output_index": i, "item": added}));
                events.push(json!({"type": "response.function_call_arguments.delta", "output_index": i,
                                   "item_id": item["id"], "delta": item["arguments"]}));
                events.push(json!({"type": "response.function_call_arguments.done", "output_index": i,
                                   "item_id": item["id"], "arguments": item["arguments"]}));
            }
            _ => {
                let text = item.pointer("/content/0/text").cloned().unwrap_or(json!(""));
                added["content"] = json!([]);
                events.push(json!({"type": "response.output_item.added", "output_index": i, "item": added}));
                let empty = json!({"type": "output_text", "text": "", "annotations": [], "logprobs": []});
                events.push(json!({"type": "response.content_part.added", "output_index": i, "item_id": item["id"],
                                   "content_index": 0, "part": empty}));
                events.push(json!({"type": "response.output_text.delta", "output_index": i, "item_id": item["id"],
                                   "content_index": 0, "delta": text, "logprobs": []}));
                events.push(json!({"type": "response.output_text.done", "output_index": i, "item_id": item["id"],
                                   "content_index": 0, "text": text, "logprobs": []}));
                events.push(json!({"type": "response.content_part.done", "output_index": i, "item_id": item["id"],
                                   "content_index": 0, "part": item["content"][0]}));
            }
        }
        events.push(json!({"type": "response.output_item.done", "output_index": i, "item": item}));
    }
    events.push(json!({"type": "response.completed", "response": response(seq, model, output, "completed")}));
    let mut out = String::new();
    for (n, mut e) in events.into_iter().enumerate() {
        e["sequence_number"] = json!(n);
        out.push_str(&format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap_or("message")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_history(_: &str) -> Option<Vec<Value>> {
        None
    }

    #[test]
    fn maps_items_onto_the_neutral_model() {
        let body = br#"{"model":"m","input":[
            {"role":"user","content":"go"},
            {"type":"function_call","call_id":"BC_CALL_000001","name":"boundary_test","arguments":"{}"},
            {"type":"function_call_output","call_id":"BC_CALL_000001","output":"result"},
            {"type":"function_call_output","call_id":"BC_CALL_000002","output":[
                {"type":"input_text","text":"a"},{"type":"input_image","image_url":"data:image/png;base64,AAEC"},{"type":"input_text","text":"b"}]},
            {"type":"reasoning","summary":[]}
        ],"tools":[{"type":"function","name":"boundary_test","parameters":{}}],"stream":true}"#;
        let r = parse_request(body, no_history).unwrap();
        let p = &r.parsed;
        assert!(p.stream);
        assert_eq!(p.tool_names, vec!["boundary_test"]);
        assert_eq!(p.messages[1].tool_call_ids, vec!["BC_CALL_000001"]);
        assert_eq!((p.messages[2].role.as_str(), p.messages[2].content.text()), ("tool", Some("result")));
        assert_eq!(p.messages[2].raw_content.as_deref(), Some(r#""result""#));
        assert_eq!(p.messages[3].content.text(), Some("ab"));
        let sha = crate::compare::content::sha256_hex(&[0, 1, 2]);
        assert!(matches!(&p.messages[3].parts[1], Part::Image { sha256: Some(s), .. } if *s == sha));
        assert_eq!(p.messages[4].role, "item:reasoning");
    }

    #[test]
    fn previous_response_id_prepends_the_stored_conversation() {
        let stored = vec![
            json!({"role":"user","content":"go"}),
            json!({"type":"function_call","call_id":"C1","name":"t","arguments":"{}"}),
        ];
        let body = br#"{"input":[{"type":"function_call_output","call_id":"C1","output":"x"}],"previous_response_id":"resp_1"}"#;
        let r = parse_request(body, |id| (id == "resp_1").then(|| stored.clone())).unwrap();
        assert_eq!(r.parsed.messages.len(), 3);
        assert_eq!(r.parsed.messages[2].tool_call_id.as_deref(), Some("C1"));
        assert_eq!(r.parsed.messages[0].raw_content, None, "history items carry no raw token");
        assert!(parse_request(br#"{"input":[],"previous_response_id":"nope"}"#, no_history).is_err());
        assert!(parse_request(br#"{"messages":[]}"#, no_history).is_err());
    }

    #[test]
    fn stream_ends_with_completed_response() {
        let out = stream(3, "m", &message_items(3, "done"));
        assert!(out.starts_with("event: response.created\n"));
        let last = out.trim_end().rsplit("\n\n").next().unwrap();
        assert!(last.starts_with("event: response.completed"));
        assert!(last.contains(r#""text":"done""#));
    }
}
