//! Loopback HTTP server hosting one scenario state machine.

use crate::provider::recorder::{redact_headers, MAX_BODY_BYTES};
use crate::provider::scenario::ScenarioMachine;
use crate::scenario::ScenarioDef;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Shared {
    pub machine: Mutex<ScenarioMachine>,
    pub changed: Condvar,
}

pub struct Provider {
    pub base_url: String,
    pub shared: Arc<Shared>,
    server: Arc<tiny_http::Server>,
    thread: Option<JoinHandle<()>>,
}

impl Provider {
    /// Bind a dynamically allocated loopback port and start serving.
    pub fn start(run_id: &str, def: &'static ScenarioDef, save_headers: bool) -> Result<Provider, String> {
        let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| format!("cannot bind fake provider: {e}"))?;
        let port = server.server_addr().to_ip().map(|a| a.port()).ok_or("fake provider has no IP address")?;
        let machine = ScenarioMachine::new(run_id, def);
        let base_url = format!("http://127.0.0.1:{port}{}", machine.base_path());
        let shared = Arc::new(Shared { machine: Mutex::new(machine), changed: Condvar::new() });
        let server = Arc::new(server);
        let (srv, sh) = (server.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("boundarycheck-provider".into())
            .spawn(move || serve(srv, sh, save_headers))
            .map_err(|e| format!("cannot start provider thread: {e}"))?;
        Ok(Provider { base_url, shared, server, thread: Some(thread) })
    }

    pub fn lock(&self) -> MutexGuard<'_, ScenarioMachine> {
        self.shared.machine.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Block until the machine changes or `timeout` elapses.
    pub fn wait(&self, timeout: Duration) {
        let guard = self.lock();
        let _ = self.shared.changed.wait_timeout(guard, timeout);
    }

    /// Stop serving and hand back the recorded state. Does not wait more than
    /// two seconds for the server thread (e.g. one blocked reading a stalled
    /// request body); the recorded state is taken out under the lock either way.
    pub fn stop(mut self) -> ScenarioMachine {
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !t.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if t.is_finished() {
                let _ = t.join();
            }
        }
        let mut m = self.lock();
        let fresh = ScenarioMachine::new(&m.run_id.clone(), m.def);
        std::mem::replace(&mut *m, fresh)
    }
}

fn serve(server: Arc<tiny_http::Server>, shared: Arc<Shared>, save_headers: bool) {
    for mut rq in server.incoming_requests() {
        let mut raw = Vec::new();
        let read = rq.as_reader().take(MAX_BODY_BYTES as u64 + 1).read_to_end(&mut raw);
        let truncated = raw.len() > MAX_BODY_BYTES;
        raw.truncate(MAX_BODY_BYTES);
        let headers = save_headers.then(|| {
            let pairs: Vec<(String, String)> = rq
                .headers()
                .iter()
                .map(|h| (h.field.as_str().as_str().to_owned(), h.value.as_str().to_owned()))
                .collect();
            redact_headers(pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        });
        let method = rq.method().as_str().to_uppercase();
        let url = rq.url().to_owned();
        let reply = {
            let mut m = shared.machine.lock().unwrap_or_else(|e| e.into_inner());
            if read.is_err() {
                m.handle(&method, &url, Vec::new(), true, headers)
            } else {
                m.handle(&method, &url, raw, truncated, headers)
            }
        };
        shared.changed.notify_all();
        let mut resp = tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
        let mut add = |k: &str, v: &str| {
            if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
                resp.add_header(h);
            }
        };
        add("content-type", reply.content_type);
        for (k, v) in &reply.headers {
            add(k, v);
        }
        let _ = rq.respond(resp);
    }
}
