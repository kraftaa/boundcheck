//! Loopback HTTP/1.1 server hosting one scenario state machine.
//!
//! A small purpose-built server (instead of an HTTP library) so that
//! transport faults can be injected exactly: closing the connection after the
//! request was read, resetting it with a TCP RST, or cutting a response off
//! halfway. It supports what model-provider clients send: keep-alive,
//! `Content-Length` and `chunked` request bodies, and `Expect: 100-continue`.

use crate::provider::protocol::Protocol;
use crate::provider::recorder::{redact_headers, MAX_BODY_BYTES};
use crate::provider::scenario::{Delivery, Reply, ScenarioMachine};
use crate::scenario::ScenarioDef;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

/// Idle time after which a keep-alive connection is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HEADER_BYTES: usize = 64 * 1024;

pub struct Shared {
    pub machine: Mutex<ScenarioMachine>,
    pub changed: Condvar,
}

pub struct Provider {
    pub base_url: String,
    pub shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Provider {
    /// Bind a dynamically allocated loopback port and start serving.
    pub fn start(
        run_id: &str,
        def: &'static ScenarioDef,
        protocol: Protocol,
        save_headers: bool,
    ) -> Result<Provider, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot bind fake provider: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| format!("cannot configure fake provider: {e}"))?;
        let port = listener.local_addr().map_err(|e| format!("fake provider has no address: {e}"))?.port();
        let machine = ScenarioMachine::with_protocol(run_id, def, protocol);
        let base_url = format!("http://127.0.0.1:{port}{}", machine.base_path());
        let shared = Arc::new(Shared { machine: Mutex::new(machine), changed: Condvar::new() });
        let stop = Arc::new(AtomicBool::new(false));
        let (sh, st) = (shared.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("boundarycheck-provider".into())
            .spawn(move || accept_loop(listener, sh, st, save_headers))
            .map_err(|e| format!("cannot start provider thread: {e}"))?;
        Ok(Provider { base_url, shared, stop, thread: Some(thread) })
    }

    pub fn lock(&self) -> MutexGuard<'_, ScenarioMachine> {
        self.shared.machine.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Block until the machine changes or `timeout` elapses.
    pub fn wait(&self, timeout: Duration) {
        let guard = self.lock();
        let _ = self.shared.changed.wait_timeout(guard, timeout);
    }

    /// Stop accepting connections and hand back the recorded state. Open
    /// connections may still finish on their own threads; they no longer
    /// affect the returned state.
    pub fn stop(mut self) -> ScenarioMachine {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let mut m = self.lock();
        let fresh = ScenarioMachine::with_protocol(&m.run_id.clone(), m.def, m.protocol);
        std::mem::replace(&mut *m, fresh)
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, stop: Arc<AtomicBool>, save_headers: bool) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let (sh, st) = (shared.clone(), stop.clone());
                let _ = std::thread::Builder::new()
                    .name("boundarycheck-provider-conn".into())
                    .spawn(move || serve_connection(stream, &sh, &st, save_headers));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    body_truncated: bool,
    close: bool,
}

fn serve_connection(stream: TcpStream, shared: &Shared, stop: &AtomicBool, save_headers: bool) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    let _ = stream.set_nodelay(true);
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    loop {
        let req = match read_request(&mut reader, &mut writer) {
            Ok(Some(r)) => r,
            Ok(None) | Err(_) => return, // client closed, timed out, or sent garbage
        };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let headers = save_headers.then(|| redact_headers(req.headers.iter().map(|(k, v)| (k.as_str(), v.as_str()))));
        let reply = {
            let mut m = shared.machine.lock().unwrap_or_else(|e| e.into_inner());
            m.handle(&req.method, &req.target, req.body, req.body_truncated, headers)
        };
        shared.changed.notify_all();
        let keep_open = deliver(&mut writer, &reply) && !req.close;
        if !keep_open {
            return;
        }
    }
}

/// Write the reply as instructed. Returns whether the connection stays usable.
fn deliver(stream: &mut TcpStream, reply: &Reply) -> bool {
    match reply.delivery {
        Delivery::Normal => write_reply(stream, reply, reply.body.len()).is_ok(),
        Delivery::Delay(d) => {
            std::thread::sleep(d);
            write_reply(stream, reply, reply.body.len()).is_ok()
        }
        Delivery::Close => {
            let _ = stream.shutdown(Shutdown::Both);
            false
        }
        Delivery::Reset => {
            // SO_LINGER with a zero timeout makes close() send a TCP RST.
            let linger = libc::linger { l_onoff: 1, l_linger: 0 };
            unsafe {
                libc::setsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    &linger as *const libc::linger as *const libc::c_void,
                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                );
            }
            false
        }
        Delivery::Truncate => {
            let _ = write_reply(stream, reply, reply.body.len() / 2);
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
            false
        }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Write status line, headers (Content-Length is always the full body) and
/// the first `send` bytes of the body.
fn write_reply(stream: &mut TcpStream, reply: &Reply, send: usize) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: keep-alive\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body[..send.min(reply.body.len())])?;
    stream.flush()
}

fn read_line(reader: &mut impl BufRead, budget: &mut usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = reader.by_ref().take(*budget as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if n > *budget {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "header too large"));
    }
    *budget -= n;
    Ok(Some(String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_owned()))
}

/// Read one request. `Ok(None)` means the client closed the connection cleanly.
fn read_request(reader: &mut impl BufRead, writer: &mut impl Write) -> io::Result<Option<Request>> {
    let mut budget = MAX_HEADER_BYTES;
    let request_line = loop {
        match read_line(reader, &mut budget)? {
            None => return Ok(None),
            Some(l) if l.is_empty() => continue, // tolerate stray CRLF between requests
            Some(l) => break l,
        }
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(target), version) = (parts.next(), parts.next(), parts.next()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad request line"));
    };
    let mut headers = vec![];
    loop {
        match read_line(reader, &mut budget)? {
            None => return Ok(None),
            Some(l) if l.is_empty() => break,
            Some(l) => {
                if let Some((k, v)) = l.split_once(':') {
                    headers.push((k.trim().to_owned(), v.trim().to_owned()));
                }
            }
        }
    }
    let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
    let close = header("connection").is_some_and(|v| v.eq_ignore_ascii_case("close")) || version == Some("HTTP/1.0");
    if header("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue")) {
        writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    }
    let mut body = Vec::new();
    let mut truncated = false;
    let mut keep = |chunk: &[u8], body: &mut Vec<u8>| {
        let room = (MAX_BODY_BYTES + 1).saturating_sub(body.len());
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if body.len() > MAX_BODY_BYTES {
            truncated = true;
            body.truncate(MAX_BODY_BYTES);
        }
    };
    if header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        loop {
            let mut size_budget = 1024;
            let size_line = read_line(reader, &mut size_budget)?.ok_or(io::ErrorKind::UnexpectedEof)?;
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
            if size == 0 {
                // Trailers until an empty line.
                let mut trailer_budget = MAX_HEADER_BYTES;
                while let Some(l) = read_line(reader, &mut trailer_budget)? {
                    if l.is_empty() {
                        break;
                    }
                }
                break;
            }
            copy_exact(reader, size, |c| keep(c, &mut body))?;
            let mut crlf = [0u8; 2];
            reader.read_exact(&mut crlf)?;
        }
    } else if let Some(len) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        copy_exact(reader, len, |c| keep(c, &mut body))?;
    }
    Ok(Some(Request {
        method: method.to_uppercase(),
        target: target.to_owned(),
        headers,
        body,
        body_truncated: truncated,
        close,
    }))
}

/// Parse every request in `bytes` as one keep-alive connection would; returns
/// `(method, target, body length, body truncated)` per request. For fuzzing
/// and tests: never panics, and memory stays bounded by the body limit.
#[doc(hidden)]
pub fn parse_requests(bytes: &[u8]) -> Vec<(String, String, usize, bool)> {
    let mut reader = BufReader::new(bytes);
    let mut sink = io::sink();
    let mut out = vec![];
    while let Ok(Some(r)) = read_request(&mut reader, &mut sink) {
        out.push((r.method, r.target, r.body.len(), r.body_truncated));
        if out.len() > 1000 {
            break;
        }
    }
    out
}

/// Read exactly `n` bytes, handing them over in chunks (bounded memory).
fn copy_exact(reader: &mut impl Read, mut n: usize, mut sink: impl FnMut(&[u8])) -> io::Result<()> {
    let mut buf = [0u8; 64 * 1024];
    while n > 0 {
        let want = n.min(buf.len());
        reader.read_exact(&mut buf[..want])?;
        sink(&buf[..want]);
        n -= want;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;

    const BODY: &str = r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"tools":[{"type":"function","function":{"name":"boundary_test"}}]}"#;

    /// Send raw bytes and read until the peer closes or a full response arrived.
    fn send(port: u16, raw: &[u8]) -> io::Result<Vec<u8>> {
        let mut s = TcpStream::connect(("127.0.0.1", port))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.write_all(raw)?;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = s.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            // Skip an interim `100 Continue` response.
            let full = String::from_utf8_lossy(&out).into_owned();
            let text = full.strip_prefix("HTTP/1.1 100 Continue\r\n\r\n").unwrap_or(&full);
            let prefix = full.len() - text.len();
            if let Some(end) = text.find("\r\n\r\n") {
                let len: usize = text
                    .split("content-length: ")
                    .nth(1)
                    .and_then(|r| r.split("\r\n").next())
                    .and_then(|l| l.parse().ok())
                    .unwrap_or(0);
                if out.len() >= prefix + end + 4 + len {
                    break;
                }
            }
        }
        Ok(out)
    }

    fn port(p: &Provider) -> u16 {
        p.base_url.trim_start_matches("http://127.0.0.1:").split('/').next().unwrap().parse().unwrap()
    }

    fn post(path: &str) -> String {
        format!("POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{BODY}", BODY.len())
    }

    #[test]
    fn content_length_and_chunked_bodies_reach_the_machine() {
        let def = scenario::find("exact-text").unwrap();
        let p = Provider::start("BC_RUN_000001", def, Protocol::ChatCompletions, false).unwrap();
        let path = "/BC_RUN_000001/exact-text/v1/chat/completions";
        let out = send(port(&p), post(path).as_bytes()).unwrap();
        assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 200 OK"));
        let chunked = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n\r\n{:x}\r\n{BODY}\r\n0\r\n\r\n",
            BODY.len()
        );
        let out = send(port(&p), chunked.as_bytes()).unwrap();
        assert!(String::from_utf8_lossy(&out).contains("HTTP/1.1 200 OK"));
        let m = p.stop();
        assert_eq!(m.requests.len(), 2);
        assert_eq!(m.requests[1].raw, BODY.as_bytes(), "chunked body reassembled exactly");
    }

    #[test]
    fn parser_handles_pipelined_chunked_and_malformed_input() {
        let ok = format!("{}{}", post("/a"), post("/b"));
        let got = parse_requests(ok.as_bytes());
        assert_eq!(got.iter().map(|r| r.1.as_str()).collect::<Vec<_>>(), ["/a", "/b"]);
        assert_eq!(got[0].2, BODY.len());
        let chunked =
            b"POST /c HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;ext=1\r\nde\r\n0\r\nX-T: 1\r\n\r\n";
        assert_eq!(parse_requests(chunked), [("POST".into(), "/c".into(), 5, false)]);
        for bad in [
            &b"\x00\x01garbage"[..],
            b"POST\r\n\r\n",
            b"POST /x HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"POST /x HTTP/1.1\r\nContent-Length: 99\r\n\r\nshort",
        ] {
            assert!(parse_requests(bad).is_empty(), "{:?}", String::from_utf8_lossy(bad));
        }
        let huge_header = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(70_000));
        assert!(parse_requests(huge_header.as_bytes()).is_empty(), "header budget enforced");
    }

    #[test]
    fn faults_are_delivered_on_the_wire() {
        for id in ["disconnect-after-request", "connection-reset", "truncated-response"] {
            let def = scenario::find(id).unwrap();
            let p = Provider::start("BC_RUN_000001", def, Protocol::ChatCompletions, false).unwrap();
            let path = format!("/BC_RUN_000001/{id}/v1/chat/completions");
            send(port(&p), post(&path).as_bytes()).unwrap(); // initial request: a normal tool call
            let out = send(port(&p), post(&path).as_bytes());
            match id {
                "disconnect-after-request" => assert_eq!(out.unwrap(), b""),
                "connection-reset" => assert!(out.map(|o| o.is_empty()).unwrap_or(true)),
                _ => {
                    let out = out.unwrap();
                    let text = String::from_utf8_lossy(&out);
                    let len: usize =
                        text.split("content-length: ").nth(1).unwrap().split("\r\n").next().unwrap().parse().unwrap();
                    let body_len = out.len() - text.find("\r\n\r\n").unwrap() - 4;
                    assert!(body_len < len, "body cut off: {body_len} of {len}");
                }
            }
            let m = p.stop();
            assert_eq!(m.requests.len(), 2, "{id}: the faulted request was still recorded");
        }
    }
}
