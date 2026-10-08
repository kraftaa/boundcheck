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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Clone)]
pub struct McpServer {
    run_id: String,
    scenario: String,
    evidence: PathBuf,
    seq: Arc<AtomicU64>,
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
        McpServer { run_id, scenario, evidence, seq: Arc::new(AtomicU64::new(0)) }
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
            loop {
                line.clear();
                if input.read_until(b'\n', &mut line)? == 0 {
                    break;
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
                    let (server, tx) = (self.clone(), tx.clone());
                    scope.spawn(move || match server.handle(&msg) {
                        Ok(Some(response)) => {
                            let _ = tx.send(Ok(response));
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let _ = tx.send(Err(error));
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
        let bytes = if let Some((code, message)) = &payload.protocol_error {
            error_response(id.clone(), *code, message)
        } else {
            let mut result = serde_json::Map::new();
            result.insert("content".into(), Value::Array(payload.content_items()));
            if let Some(s) = payload.structured {
                result.insert("structuredContent".into(), s);
            }
            result.insert("isError".into(), Value::Bool(payload.is_error));
            if let Some(m) = payload.meta {
                result.insert("_meta".into(), m);
            }
            encode(&json!({"jsonrpc": "2.0", "id": id, "result": Value::Object(result)}))
        };
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
        let s = McpServer {
            run_id: "BC_RUN_000001".into(),
            scenario: "exact-text".into(),
            evidence: ev.clone(),
            seq: Arc::new(AtomicU64::new(0)),
        };
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
        let s = McpServer {
            run_id: "BC_RUN_000001".into(),
            scenario: "concurrent-two-tools".into(),
            evidence: ev,
            seq: Arc::new(AtomicU64::new(0)),
        };
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
