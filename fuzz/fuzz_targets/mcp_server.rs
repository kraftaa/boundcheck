//! Arbitrary stdin for the fake MCP server: never panics, and every line it
//! writes is a JSON-RPC 2.0 message.
#![no_main]
use boundarycheck::mcp::server::McpServer;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::new("BC_RUN_000001".into(), "exact-text".into(), dir.path().join("ev.jsonl"));
    let mut out = Vec::new();
    server.run(data, &mut out).unwrap();
    for line in out.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let v: serde_json::Value = serde_json::from_slice(line).expect("output line is JSON");
        assert_eq!(v["jsonrpc"], "2.0");
    }
});
