//! Child process management: own process group, bounded output capture,
//! and termination on every exit path (normal, error, panic, Ctrl-C).

use serde::Serialize;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Bytes of stdout/stderr kept per stream; the rest is drained and counted.
pub const CAPTURE_LIMIT: usize = 1024 * 1024;

static ACTIVE_GROUPS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Kill every live child process group on Ctrl-C / SIGTERM and exit with the
/// harness-error code.
/// Temporary work directories to delete if the run is interrupted.
static CLEANUP_DIRS: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

pub fn register_cleanup_dir(dir: &Path) {
    if let Ok(mut d) = CLEANUP_DIRS.lock() {
        d.push(dir.to_path_buf());
    }
}

pub fn unregister_cleanup_dir(dir: &Path) {
    if let Ok(mut d) = CLEANUP_DIRS.lock() {
        d.retain(|p| p != dir);
    }
}

pub fn install_signal_handler() {
    become_subreaper();
    let _ = ctrlc::set_handler(|| {
        kill_all_groups();
        for pid in descendants() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        for dir in CLEANUP_DIRS.lock().map(|d| d.clone()).unwrap_or_default() {
            let _ = std::fs::remove_dir_all(dir);
        }
        eprintln!("boundarycheck: interrupted; child processes terminated");
        std::process::exit(crate::model::verdict::EXIT_HARNESS);
    });
}

pub fn kill_all_groups() {
    let groups = ACTIVE_GROUPS.lock().map(|g| g.clone()).unwrap_or_default();
    for pgid in groups {
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
    }
}

/// On Linux, orphaned descendants (e.g. a process that called `setsid()` and
/// whose parent exited) are reparented to boundarycheck instead of init, so
/// they remain visible to the descendant sweep below.
fn become_subreaper() {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    }
}

/// (pid, ppid) for every process on the system.
fn process_table() -> Vec<(i32, i32)> {
    #[cfg(target_os = "linux")]
    {
        let mut out = vec![];
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else { continue };
            // Fields after the parenthesised command: state ppid ...
            if let Some(ppid) = stat.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse().ok())
            {
                out.push((pid, ppid));
            }
        }
        out
    }
    #[cfg(not(target_os = "linux"))]
    {
        let Ok(mut ps) = Command::new("ps")
            .args(["-A", "-o", "pid=", "-o", "ppid="])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return vec![];
        };
        let ps_pid = ps.id() as i32;
        let mut text = String::new();
        if let Some(mut o) = ps.stdout.take() {
            let _ = o.read_to_string(&mut text);
        }
        let _ = ps.wait();
        text.lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace().map(|x| x.parse::<i32>().ok());
                Some((it.next()??, it.next()??))
            })
            .filter(|(pid, _)| *pid != ps_pid)
            .collect()
    }
}

/// Every live descendant of boundarycheck itself. Only one runtime runs at a
/// time, so this is exactly the runtime's process tree, including members
/// that left its process group with `setsid()`/`setpgid()`.
pub fn descendants() -> Vec<i32> {
    let table = process_table();
    let mut found = vec![std::process::id() as i32];
    let mut i = 0;
    while i < found.len() {
        let parent = found[i];
        found.extend(table.iter().filter(|(_, pp)| *pp == parent).map(|(p, _)| *p));
        i += 1;
    }
    found.remove(0);
    found
}

#[derive(Debug, Clone, Serialize)]
pub struct ExitInfo {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub terminated_by_harness: bool,
}

impl ExitInfo {
    fn from_status(s: ExitStatus, by_harness: bool) -> Self {
        ExitInfo { exit_code: s.code(), signal: s.signal(), terminated_by_harness: by_harness }
    }

    pub fn describe(&self) -> String {
        match (self.exit_code, self.signal) {
            (Some(c), _) => format!("exit code {c}"),
            (None, Some(s)) => format!("signal {s}"),
            _ => "unknown status".into(),
        }
    }
}

pub struct Spawned {
    child: Child,
    pgid: i32,
    readers: Vec<JoinHandle<u64>>,
    exit: Option<ExitInfo>,
    reaped: Option<()>,
    pub stream_bytes: (u64, u64),
}

pub struct SpawnSpec<'a> {
    pub argv: &'a [String],
    /// The complete environment of the child (nothing else is inherited).
    pub env: &'a [(String, String)],
    pub cwd: &'a Path,
    pub stdout_path: &'a Path,
    pub stderr_path: &'a Path,
    pub stdin: Option<String>,
}

pub fn spawn(spec: SpawnSpec) -> io::Result<Spawned> {
    let (program, args) =
        spec.argv.split_first().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty agent command"))?;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(spec.cwd).process_group(0).env_clear();
    for (k, v) in spec.env {
        cmd.env(k, v);
    }
    cmd.stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let pgid = child.id() as i32;
    if let Ok(mut g) = ACTIVE_GROUPS.lock() {
        g.push(pgid);
    }
    if let (Some(text), Some(mut stdin)) = (spec.stdin, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let readers = vec![
        capture(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), spec.stdout_path)?,
        capture(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>), spec.stderr_path)?,
    ];
    Ok(Spawned { child, pgid, readers, exit: None, reaped: None, stream_bytes: (0, 0) })
}

fn capture(src: Option<Box<dyn Read + Send>>, path: &Path) -> io::Result<JoinHandle<u64>> {
    let mut file = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    Ok(std::thread::spawn(move || {
        let Some(mut src) = src else { return 0 };
        let mut buf = [0u8; 16 * 1024];
        let (mut total, mut kept) = (0u64, 0usize);
        while let Ok(n) = src.read(&mut buf) {
            if n == 0 {
                break;
            }
            total += n as u64;
            let take = n.min(CAPTURE_LIMIT - kept);
            if take > 0 {
                let _ = file.write_all(&buf[..take]);
                kept += take;
            }
        }
        if total as usize > kept {
            let _ = writeln!(file, "\n[boundarycheck: {} further bytes not captured]", total as usize - kept);
        }
        total
    }))
}

impl Spawned {
    /// Has the runtime exited? Uses `WNOWAIT` so the child stays a zombie:
    /// its PID (= our process-group ID) cannot be reused until `finish` has
    /// killed the rest of the group and reaped it.
    fn exited(&self) -> bool {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(libc::P_PID, self.pgid as libc::id_t, &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT)
        };
        rc == 0 && unsafe { info.si_pid() } != 0
    }

    pub fn try_wait(&mut self) -> Option<ExitInfo> {
        if self.exit.is_none() && self.reaped.is_none() && self.exited() {
            self.exit = Some(ExitInfo { exit_code: None, signal: None, terminated_by_harness: false });
        }
        self.exit.clone()
    }

    /// Stop the runtime (if still running) and everything in its process
    /// group, then reap it and collect output capture threads.
    pub fn finish(&mut self, grace: Duration) -> ExitInfo {
        // Descendants that escaped the process group are signalled individually.
        // Each is remembered with its process-group ID so that a PID reused by an
        // unrelated process (different group) is never signalled later.
        let escaped = |pgid: i32| -> Vec<(i32, i32)> {
            descendants()
                .into_iter()
                .map(|p| (p, unsafe { libc::getpgid(p) }))
                .filter(|(_, g)| *g > 0 && *g != pgid)
                .collect()
        };
        // Snapshot before signalling anything: once a parent dies, its children
        // are reparented (to init on macOS) and leave our process tree.
        let mut stray = escaped(self.pgid);
        let by_harness = if self.exit.is_some() || self.exited() {
            false
        } else {
            for (p, _) in &stray {
                unsafe { libc::kill(*p, libc::SIGTERM) };
            }
            unsafe { libc::killpg(self.pgid, libc::SIGTERM) };
            let deadline = Instant::now() + grace;
            while Instant::now() < deadline && !self.exited() {
                std::thread::sleep(Duration::from_millis(20));
            }
            true
        };
        for e in escaped(self.pgid) {
            if !stray.contains(&e) {
                stray.push(e);
            }
        }
        for (p, g) in &stray {
            if unsafe { libc::getpgid(*p) } == *g {
                unsafe { libc::kill(*p, libc::SIGKILL) };
            }
        }
        // The leader is alive or an unreaped zombie here, so the group ID is
        // still ours: kill the rest of the group (e.g. a leaked MCP server).
        unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
        let stray: Vec<i32> = stray.into_iter().map(|(p, _)| p).collect();
        let status = self.child.wait().unwrap_or_else(|_| ExitStatus::from_raw(9));
        reap(&stray);
        let info = ExitInfo::from_status(status, by_harness);
        self.reaped = Some(());
        self.exit = Some(info.clone());
        // A process that escaped the sweep (e.g. reparented to launchd on macOS)
        // may still hold the output pipes open: never wait for it forever.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut bytes = [0u64; 2];
        for (i, r) in self.readers.drain(..).enumerate() {
            while !r.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if r.is_finished() {
                bytes[i] = r.join().unwrap_or(0);
            }
        }
        self.stream_bytes = (bytes[0], bytes[1]);
        self.unregister();
        info
    }

    fn unregister(&self) {
        if let Ok(mut g) = ACTIVE_GROUPS.lock() {
            g.retain(|p| *p != self.pgid);
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if self.reaped.is_none() {
            unsafe { libc::killpg(self.pgid, libc::SIGKILL) };
            let _ = self.child.wait();
            self.unregister();
        }
    }
}

/// Reap killed processes that were reparented to us (Linux subreaper); a
/// no-op for processes that are not our children.
fn reap(pids: &[i32]) {
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut pending: Vec<i32> = pids.to_vec();
    while !pending.is_empty() && Instant::now() < deadline {
        pending.retain(|p| unsafe { libc::waitpid(*p, std::ptr::null_mut(), libc::WNOHANG) } == 0);
        if !pending.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Last `max` bytes of a capture file, lossily decoded.
pub fn tail(path: &Path, max: usize) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }
    let start = buf.len().saturating_sub(max);
    Some(String::from_utf8_lossy(&buf[start..]).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminates_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let argv: Vec<String> =
            ["sh", "-c", "sleep 30 & echo $! > child.pid; wait"].iter().map(|s| s.to_string()).collect();
        let mut p = spawn(SpawnSpec {
            argv: &argv,
            env: &[("PATH".to_string(), std::env::var("PATH").unwrap_or_default())],
            cwd: dir.path(),
            stdout_path: &dir.path().join("o"),
            stderr_path: &dir.path().join("e"),
            stdin: None,
        })
        .unwrap();
        let pidfile = dir.path().join("child.pid");
        let t = Instant::now();
        while !pidfile.exists() && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(50));
        let grandchild: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        let info = p.finish(Duration::from_millis(500));
        assert!(info.terminated_by_harness);
        std::thread::sleep(Duration::from_millis(50));
        assert_ne!(unsafe { libc::kill(grandchild, 0) }, 0, "grandchild must be gone");
    }
}
