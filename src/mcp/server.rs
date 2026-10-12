//! Fake MCP server over stdio (newline-delimited JSON-RPC 2.0).
//!
//! Protocol messages go to stdout, diagnostics to stderr. Every tool response
//! is appended to the evidence log *before* it is written to stdout.

use crate::mcp::payload;
use crate::model::observation::McpEvidence;
use crate::scenario::TOOL_NAME;
use base64::Engine;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// Longest accepted JSON-RPC line. Requests to this server are a few hundred
/// bytes; anything longer is answered with an error and discarded.
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// Tool calls in progress at once. When all slots are busy, the reader stops
/// reading stdin until one frees up.
pub const MAX_CONCURRENT_CALLS: usize = 16;

#[derive(Clone)]
pub struct McpServer {
    run_id: String,
    scenario: String,
    evidence: PathBuf,
    seq: Arc<AtomicU64>,
    in_flight: Arc<AtomicUsize>,
    peak_in_flight: Arc<AtomicUsize>,
}

pub fn serve(run_id: String, scenario: String, evidence: PathBuf) -> i32 {
    let server = McpServer::new(run_id, scenario, evidence);
    match server.run(io::stdin().lock(), io::stdout()) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("boundarycheck mcp-server: {e}");
            1
        }
    }
}

impl McpServer {
    pub fn new(run_id: String, scenario: String, evidence: PathBuf) -> Self {
        McpServer {
            run_id,
            scenario,
            evidence,
            seq: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak_in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The most tool calls that were in progress at the same time.
    #[cfg(test)]
    fn peak_concurrent_calls(&self) -> usize {
        self.peak_in_flight.load(Ordering::Acquire)
    }

    /// Serve newline-delimited JSON-RPC from `input` until EOF.
    pub fn run(&self, input: impl BufRead, mut output: impl Write + Send) -> io::Result<()> {
        let seq = self.next_seq();
        self.record(McpEvidence {
            event: "session-start".into(),
            pid: std::process::id(),
            seq,
            jsonrpc_id: None,
            tool: None,
            arguments: None,
            call_id: None,
            generated: false,
            response_b64: None,
        })?;
        std::thread::scope(|scope| {
            let (tx, rx) = mpsc::channel::<io::Result<Vec<u8>>>();
            let writer = scope.spawn(move || -> io::Result<()> {
                for response in rx {
                    write_line(&mut output, &response?)?;
                }
                Ok(())
            });
            let mut input = input;
            let mut line = Vec::new();
            // A counting semaphore: each tool call takes a slot and returns it when done.
            let (slot_tx, slot_rx) = mpsc::sync_channel::<()>(MAX_CONCURRENT_CALLS);
            for _ in 0..MAX_CONCURRENT_CALLS {
                slot_tx.send(()).expect("slot channel has capacity");
            }
            while let Some(complete) = read_line_capped(&mut input, &mut line, MAX_LINE_BYTES)? {
                if !complete {
                    tx.send(Ok(error_response(
                        Value::Null,
                        -32600,
                        &format!("request line exceeds {MAX_LINE_BYTES} bytes"),
                    )))
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "MCP writer stopped"))?;
                    continue;
                }
                let trimmed = trim(&line);
                if trimmed.is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_slice(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        tx.send(Ok(error_response(Value::Null, -32700, &format!("parse error: {e}"))))
                            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "MCP writer stopped"))?;
                        continue;
                    }
                };
                if msg.get("method").and_then(Value::as_str) == Some("tools/call") {
                    // Blocks while every slot is taken. Cannot fail: `slot_tx` is alive.
                    slot_rx.recv().expect("slot sender is alive");
                    let (server, tx, slot_tx) = (self.clone(), tx.clone(), slot_tx.clone());
                    scope.spawn(move || {
                        let now = server.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
                        server.peak_in_flight.fetch_max(now, Ordering::AcqRel);
                        let handled = server.handle(&msg);
                        server.in_flight.fetch_sub(1, Ordering::AcqRel);
                        let _ = slot_tx.send(());
                        match handled {
                            Ok(Some(response)) => {
                                let _ = tx.send(Ok(response));
                            }
                            Ok(None) => {}
                            Err(error) => {
                                let _ = tx.send(Err(error));
                            }
                        }
                    });
                } else if let Some(response) = self.handle(&msg)? {
                    tx.send(Ok(response))
                        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "MCP writer stopped"))?;
                }
            }
            drop(tx);
            writer.join().map_err(|_| io::Error::other("MCP writer thread panicked"))?
        })
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn handle(&self, msg: &Value) -> io::Result<Option<Vec<u8>>> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let Some(id) = id else {
            return Ok(None); // notification (e.g. notifications/initialized)
        };
        if msg.get("method").is_none() {
            return Ok(None); // a response to something we never sent; ignore
        }
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
                    requested
                } else {
                    SUPPORTED_PROTOCOL_VERSIONS[0]
                };
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "boundarycheck-mcp", "version": env!("CARGO_PKG_VERSION")}
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools": [tool_definition()]}),
            "tools/call" => return self.tools_call(id, &params).map(Some),
            "resources/list" => json!({"resources": []}),
            "prompts/list" => json!({"prompts": []}),
            _ => return Ok(Some(error_response(id, -32601, &format!("method not found: {method}")))),
        };
        Ok(Some(encode(&json!({"jsonrpc": "2.0", "id": id, "result": result}))))
    }

    fn tools_call(&self, id: Value, params: &Value) -> io::Result<Vec<u8>> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        if name != TOOL_NAME {
            return Ok(error_response(id, -32602, &format!("unknown tool: {name}")));
        }
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        let field = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_owned);
        let call_id = field("call_id");
        // Both calls are already in flight when this delay runs. Delaying A
        // deterministically makes B finish first and exercises association
        // independently of request/completion order.
        if self.scenario == "concurrent-two-tools" && call_id.as_deref() == Some(crate::scenario::CALL_1) {
            std::thread::sleep(Duration::from_millis(75));
        }
        let generated = match (field("run_id"), field("scenario"), &call_id) {
            (Some(r), Some(s), Some(c)) if r == self.run_id && s == self.scenario => payload::generate(&r, &s, c),
            _ => None,
        };
        let is_generated = generated.is_some();
        let payload = generated.unwrap_or_else(|| payload::Payload {
            text: format!(
                "boundarycheck: arguments do not match the active run (expected run_id={} scenario={})",
                self.run_id, self.scenario
            ),
            is_error: true,
            ..payload::Payload::default()
        });
        let bytes = response_bytes(id.clone(), &payload);
        let seq = self.next_seq();
        self.record(McpEvidence {
            event: "tool-response".into(),
            pid: std::process::id(),
            seq,
            jsonrpc_id: Some(id),
            tool: Some(name.to_owned()),
            arguments: Some(args),
            call_id,
            generated: is_generated,
            response_b64: Some(base64::engine::general_purpose::STANDARD.encode(&bytes)),
        })?;
        Ok(bytes)
    }

    fn record(&self, ev: McpEvidence) -> io::Result<()> {
        append_locked(&self.evidence, &encode(&serde_json::to_value(ev)?))
    }
}

/// The exact JSON-RPC response line (without `\n`) for a tools/call payload.
/// The comparison uses it to re-derive the full response from the generator.
pub fn response_bytes(id: Value, payload: &payload::Payload) -> Vec<u8> {
    if let Some((code, message)) = &payload.protocol_error {
        return error_response(id, *code, message);
    }
    let mut result = serde_json::Map::new();
    result.insert("content".into(), Value::Array(payload.content_items()));
    if let Some(s) = &payload.structured {
        result.insert("structuredContent".into(), s.clone());
    }
    result.insert("isError".into(), Value::Bool(payload.is_error));
    if let Some(m) = &payload.meta {
        result.insert("_meta".into(), m.clone());
    }
    encode(&json!({"jsonrpc": "2.0", "id": id, "result": Value::Object(result)}))
}

pub fn tool_definition() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "boundarycheck test tool. Returns a deterministic payload for the given identifiers.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "run_id": {"type": "string"},
                "scenario": {"type": "string"},
                "call_id": {"type": "string"}
            },
            "required": ["run_id", "scenario", "call_id"],
            "additionalProperties": false
        }
    })
}

fn encode(v: &Value) -> Vec<u8> {
    serde_json::to_vec(v).expect("serialize JSON-RPC message")
}

fn error_response(id: Value, code: i64, message: &str) -> Vec<u8> {
    encode(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
}

fn write_line(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    out.write_all(bytes)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Read one `\n`-terminated line into `line`, keeping at most `max` bytes.
/// Returns `None` at EOF, `Some(true)` for a complete line and `Some(false)`
/// for a line that was longer than `max` (the rest of it is consumed and
/// discarded, so the next call starts at the following line).
fn read_line_capped(input: &mut impl BufRead, line: &mut Vec<u8>, max: usize) -> io::Result<Option<bool>> {
    line.clear();
    let (mut read_any, mut overlong) = (false, false);
    loop {
        let buf = input.fill_buf()?;
        if buf.is_empty() {
            return Ok(read_any.then_some(!overlong));
        }
        read_any = true;
        let (chunk, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (&buf[..=i], true),
            None => (buf, false),
        };
        if !overlong && line.len() + chunk.len() > max {
            overlong = true;
            line.clear();
        }
        if !overlong {
            line.extend_from_slice(chunk);
        }
        let n = chunk.len();
        input.consume(n);
        if done {
            return Ok(Some(!overlong));
        }
    }
}

fn trim(b: &[u8]) -> &[u8] {
    let start = b.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !c.is_ascii_whitespace()).map_or(start, |i| i + 1);
    &b[start..end]
}

/// Append one line under an exclusive lock so that several MCP server
/// processes (runtimes may spawn more than one) never interleave records.
fn append_locked(path: &Path, line: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = OpenOptions::new().create(true).append(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?;
    let fd = f.as_raw_fd();
    unsafe { libc::flock(fd, libc::LOCK_EX) };
    let mut buf = line.to_vec();
    buf.push(b'\n');
    let res = f.write_all(&buf).and_then(|_| f.flush());
    unsafe { libc::flock(fd, libc::LOCK_UN) };
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_evidence_before_responding() {
        let dir = tempfile::tempdir().unwrap();
        let ev = dir.path().join("ev.jsonl");
        let s = McpServer::new("BC_RUN_000001".into(), "exact-text".into(), ev.clone());
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"boundary_test","arguments":{"run_id":"BC_RUN_000001","scenario":"exact-text","call_id":"BC_CALL_000001"}}}"#,
            "\n",
        );
        let mut out = Vec::new();
        s.run(input.as_bytes(), &mut out).unwrap();
        let lines: Vec<&[u8]> = out.split(|b| *b == b'\n').filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        let recs: Vec<McpEvidence> =
            std::fs::read_to_string(&ev).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(recs[0].event, "session-start");
        let raw = base64::engine::general_purpose::STANDARD.decode(recs[1].response_b64.as_ref().unwrap()).unwrap();
        assert_eq!(raw, lines[1], "evidence holds exactly the bytes written to stdout");
        assert!(recs[1].generated);
    }

    #[test]
    fn concurrent_calls_can_finish_in_reverse_order() {
        let dir = tempfile::tempdir().unwrap();
        let ev = dir.path().join("ev.jsonl");
        let s = McpServer::new("BC_RUN_000001".into(), "concurrent-two-tools".into(), ev);
        let call = |id: u64, call_id: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/call","params":{{"name":"boundary_test","arguments":{{"run_id":"BC_RUN_000001","scenario":"concurrent-two-tools","call_id":"{call_id}"}}}}}}"#
            )
        };
        let input = format!("{}\n{}\n", call(1, crate::scenario::CALL_1), call(2, crate::scenario::CALL_2));
        let mut out = Vec::new();
        s.run(input.as_bytes(), &mut out).unwrap();
        let ids: Vec<u64> = out
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap()["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![2, 1]);
    }

    #[test]
    fn concurrent_tool_calls_never_exceed_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let s = McpServer::new("BC_RUN_000001".into(), "concurrent-two-tools".into(), dir.path().join("ev.jsonl"));
        // CALL_1 sleeps in this scenario, so a flood of them overlaps.
        let calls = 3 * MAX_CONCURRENT_CALLS;
        let input: String = (0..calls)
            .map(|id| {
                format!(
                    "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"boundary_test\",\"arguments\":{{\"run_id\":\"BC_RUN_000001\",\"scenario\":\"concurrent-two-tools\",\"call_id\":\"{}\"}}}}}}\n",
                    crate::scenario::CALL_1
                )
            })
            .collect();
        let mut out = Vec::new();
        s.run(input.as_bytes(), &mut out).unwrap();
        assert_eq!(out.split(|b| *b == b'\n').filter(|l| !l.is_empty()).count(), calls, "every call is answered");
        let peak = s.peak_concurrent_calls();
        assert!(peak <= MAX_CONCURRENT_CALLS, "{peak} calls ran at once");
        assert!(peak > 1, "calls should overlap up to the limit");
    }

    #[test]
    fn overlong_lines_are_rejected_and_the_next_line_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let s = McpServer::new("BC_RUN_000001".into(), "exact-text".into(), dir.path().join("ev.jsonl"));
        let mut input = vec![b'x'; MAX_LINE_BYTES + 1];
        input.extend_from_slice(b"\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n");
        let mut out = Vec::new();
        s.run(input.as_slice(), &mut out).unwrap();
        let lines: Vec<Value> =
            out.split(|b| *b == b'\n').filter(|l| !l.is_empty()).map(|l| serde_json::from_slice(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], -32600);
        assert_eq!(lines[1]["id"], 7);
    }

    #[test]
    fn capped_reader_handles_eof_and_exact_limit() {
        let mut line = Vec::new();
        let mut r: &[u8] = b"abc\nabcd\nlast";
        assert_eq!(read_line_capped(&mut r, &mut line, 4).unwrap(), Some(true));
        assert_eq!(line, b"abc\n");
        assert_eq!(read_line_capped(&mut r, &mut line, 4).unwrap(), Some(false));
        assert_eq!(read_line_capped(&mut r, &mut line, 4).unwrap(), Some(true));
        assert_eq!(line, b"last");
        assert_eq!(read_line_capped(&mut r, &mut line, 4).unwrap(), None);
    }

    #[test]
    fn evidence_append_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, b"private").unwrap();
        let link = dir.path().join("evidence");
        symlink(&target, &link).unwrap();
        assert!(append_locked(&link, b"evidence").is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"private");
    }
}
