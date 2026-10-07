//! Orchestrates one run: per scenario, start a fake provider, launch the
//! runtime through its adapter, drive the state machine, then evaluate.

use crate::adapter::{Adapter, Completion, Readiness, RuntimeVersion, TemplateVars};
use crate::compare::{self, content::sha256_hex};
use crate::model::observation::McpEvidence;
use crate::model::verdict::{exit_classification, exit_code, UnknownNote, UnknownReason, Verdict};
use crate::process::{self, SpawnSpec};
use crate::provider::recorder::redact_argv;
use crate::provider::scenario::{Phase, ScenarioMachine};
use crate::provider::{protocol::PROTOCOL, server::Provider};
use crate::report::*;
use crate::scenario::{self, ScenarioDef};
use base64::Engine;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Largest raw artifact file written; larger evidence is truncated and marked.
pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

pub struct RunConfig {
    pub adapter: Adapter,
    pub command: Vec<String>,
    pub scenarios: Vec<&'static ScenarioDef>,
    pub timeout: Duration,
    pub artifacts: Option<PathBuf>,
    pub artifacts_all: bool,
    pub keep_workdir: bool,
    pub save_headers: bool,
    pub on_scenario: Box<dyn Fn(&ScenarioReport)>,
}

pub fn run(cfg: RunConfig) -> RunReport {
    let started = Instant::now();
    let started_at = humantime::format_rfc3339_millis(SystemTime::now()).to_string();
    let run_id = allocate_run_id(cfg.artifacts.as_deref());
    let mut report = RunReport {
        schema_version: SCHEMA_VERSION,
        boundarycheck_version: env!("CARGO_PKG_VERSION"),
        run_id: run_id.clone(),
        started_at,
        duration_ms: 0,
        adapter: AdapterInfo { name: cfg.adapter.name.clone(), source: cfg.adapter.source.clone() },
        runtime: RuntimeInfo { version_source: version_source(&cfg.adapter.runtime_version), ..Default::default() },
        provider_protocol: PROTOCOL,
        command: CommandInfo { argv: redact_argv(&cfg.command) },
        platform: Platform { os: std::env::consts::OS, arch: std::env::consts::ARCH },
        scenarios: vec![],
        summary: Summary::default(),
        artifacts: None,
        workdir: None,
        harness_error: None,
        exit: ExitSummary { code: 0, classification: "" },
    };
    let outcome = run_inner(&cfg, &run_id, &mut report);
    if let Err(e) = outcome {
        report.harness_error = Some(e);
    }
    let verdicts: Vec<Verdict> = report.scenarios.iter().map(|s| s.verdict).collect();
    report.summary = Summary {
        total: verdicts.len(),
        passed: verdicts.iter().filter(|v| **v == Verdict::Pass).count(),
        failed: verdicts.iter().filter(|v| **v == Verdict::Fail).count(),
        unknown: verdicts.iter().filter(|v| **v == Verdict::Unknown).count(),
    };
    let code = exit_code(&verdicts, report.harness_error.is_some());
    report.exit = ExitSummary { code, classification: exit_classification(code) };
    report.duration_ms = started.elapsed().as_millis();
    if let Some(dir) = &report.artifacts {
        let _ =
            write_private(&Path::new(dir).join("run.json"), &serde_json::to_vec_pretty(&report).unwrap_or_default());
    }
    report
}

fn run_inner(cfg: &RunConfig, run_id: &str, report: &mut RunReport) -> Result<(), String> {
    let tmp = tempfile::Builder::new()
        .prefix("boundarycheck-")
        .tempdir()
        .map_err(|e| format!("cannot create work directory: {e}"))?;
    let keep = cfg.keep_workdir || cfg.artifacts.is_some();
    let workdir = tmp.path().to_path_buf();
    if keep {
        report.workdir = Some(workdir.display().to_string());
    } else {
        process::register_cleanup_dir(&workdir);
    }
    let artifacts_root = match &cfg.artifacts {
        Some(dir) => {
            let root = dir.join(run_id);
            if !root.is_dir() {
                return Err(format!("cannot create artifacts directory {}", root.display()));
            }
            report.artifacts = Some(root.display().to_string());
            Some(root)
        }
        None => None,
    };
    let mcp_command = std::env::current_exe().map_err(|e| format!("cannot locate boundarycheck executable: {e}"))?;
    let result = (|| {
        for def in &cfg.scenarios {
            let s =
                run_scenario(cfg, def, run_id, &workdir, &mcp_command, artifacts_root.as_deref(), &mut report.runtime)?;
            (cfg.on_scenario)(&s);
            report.scenarios.push(s);
        }
        Ok(())
    })();
    process::unregister_cleanup_dir(&workdir);
    if keep {
        let _ = tmp.keep();
    }
    result
}

fn version_source(v: &RuntimeVersion) -> String {
    match v {
        RuntimeVersion::Unknown => "none (adapter declares no version source)".into(),
        RuntimeVersion::Static { .. } => "adapter-static".into(),
        RuntimeVersion::Command { .. } => "adapter-command".into(),
        RuntimeVersion::ReportFile => "runtime-report-file".into(),
    }
}

/// Pick the next unused run number. With artifacts, the run directory is
/// created atomically (`mkdir` fails if it exists), so concurrent runs never share one.
fn allocate_run_id(artifacts: Option<&Path>) -> String {
    let Some(dir) = artifacts else { return scenario::format_run_id(1) };
    let mut n = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.file_name().to_str()?.strip_prefix("BC_RUN_")?.parse::<u32>().ok())
                .max()
                .map_or(1, |k| k + 1)
        })
        .unwrap_or(1);
    if mkdir_private(dir).is_err() {
        return scenario::format_run_id(n);
    }
    loop {
        let id = scenario::format_run_id(n);
        match fs::DirBuilder::new().mode(0o700).create(dir.join(&id)) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < u32::MAX => n += 1,
            _ => return id,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_scenario(
    cfg: &RunConfig,
    def: &'static ScenarioDef,
    run_id: &str,
    workdir: &Path,
    mcp_command: &Path,
    artifacts_root: Option<&Path>,
    runtime: &mut RuntimeInfo,
) -> Result<ScenarioReport, String> {
    let dir = workdir.join(def.id);
    mkdir_private(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let evidence = dir.join("mcp-evidence.jsonl");
    write_private(&evidence, b"").map_err(|e| format!("cannot create MCP evidence file: {e}"))?;
    let runtime_info = dir.join("runtime-info.json");
    let (stdout_path, stderr_path) = (dir.join("runtime-stdout.log"), dir.join("runtime-stderr.log"));
    let provider = Provider::start(run_id, def, cfg.save_headers)?;

    let mcp_args: Vec<String> = vec![
        "mcp-server".into(),
        "--run-id".into(),
        run_id.into(),
        "--scenario".into(),
        def.id.into(),
        "--evidence".into(),
        evidence.display().to_string(),
    ];
    let vars = TemplateVars::new(
        &provider.base_url,
        &mcp_command.display().to_string(),
        &mcp_args,
        &def.prompt(run_id),
        def.id,
        run_id,
        &dir.display().to_string(),
        &runtime_info.display().to_string(),
    );
    let a = &cfg.adapter;
    for f in &a.files {
        let path = dir.join(crate::adapter::safe_relative_path(&vars.render(&f.path)?)?);
        write_private(&path, vars.render(&f.content)?.as_bytes())
            .map_err(|e| format!("cannot write adapter file {}: {e}", path.display()))?;
    }
    let env: Vec<(String, String)> =
        a.environment.iter().map(|(k, v)| Ok((k.clone(), vars.render(v)?))).collect::<Result<_, String>>()?;
    let base_env = a.child_environment(&env);
    let mut argv = cfg.command.clone();
    for x in &a.args {
        argv.push(vars.render(x)?);
    }
    let stdin = a.stdin.as_ref().map(|s| vars.render(s)).transpose()?;

    let started = Instant::now();
    let started_at = humantime::format_rfc3339_millis(SystemTime::now()).to_string();
    let mut active_stdout_path = stdout_path.clone();
    let mut active_stderr_path = stderr_path.clone();
    let mut child = process::spawn(SpawnSpec {
        argv: &argv,
        env: &base_env,
        cwd: &std::env::current_dir().map_err(|e| e.to_string())?,
        stdout_path: &active_stdout_path,
        stderr_path: &active_stderr_path,
        stdin,
    })
    .map_err(|e| format!("cannot launch runtime command {:?}: {e}", argv.first().map(String::as_str).unwrap_or("")))?;

    let Readiness::ProviderRequest { timeout_seconds } = a.readiness;
    let startup_deadline = started + Duration::from_secs_f64(timeout_seconds); // validated in adapter::parse
    let scenario_deadline = started + cfg.timeout;
    let mut unknowns: Vec<UnknownNote> = vec![];
    let mut done_at: Option<Instant> = None;
    let mut exited_early_in: Option<String> = None;
    let mut resumed = false;
    let mut prior_exits = vec![];
    let mut prior_stream_bytes = (0u64, 0u64);
    loop {
        provider.wait(Duration::from_millis(25));
        let now = Instant::now();
        let exited = child.try_wait();
        let (seen, phase) = {
            let m = provider.lock();
            (m.first_request_seen(), m.phase)
        };
        if matches!(phase, Phase::Complete | Phase::Aborted) {
            let t = *done_at.get_or_insert(now);
            let grace = match (&a.completion, phase) {
                (_, Phase::Aborted) => Duration::from_secs(2),
                (Completion::ProviderScenarioComplete { exit_grace_seconds }, _) => {
                    Duration::from_secs_f64(*exit_grace_seconds)
                }
                (Completion::ProcessExit, _) => scenario_deadline.saturating_duration_since(t),
            };
            if exited.is_some() {
                break;
            }
            if now.duration_since(t) >= grace {
                if matches!(a.completion, Completion::ProcessExit) && phase == Phase::Complete {
                    unknowns.push(UnknownNote::new(
                        UnknownReason::ScenarioTimeout,
                        "runtime did not exit after the scenario completed (completion type process-exit)",
                    ));
                }
                break;
            }
            continue;
        }
        if exited.is_some() && matches!(phase, Phase::AwaitReplay { resumed: true }) && !resumed {
            let Some(resume) = &a.resume else {
                unknowns.push(UnknownNote::new(
                    UnknownReason::AdapterInsufficient,
                    "persistence-resume requires an adapter `resume` configuration",
                ));
                break;
            };
            let first_exit = child.finish(Duration::from_secs_f64(a.shutdown.grace_seconds));
            prior_stream_bytes.0 += child.stream_bytes.0;
            prior_stream_bytes.1 += child.stream_bytes.1;
            if first_exit.exit_code != Some(0) {
                unknowns.push(UnknownNote::new(
                    UnknownReason::RuntimeExitedEarly,
                    format!("checkpoint process ended with {}", first_exit.describe()),
                ));
                prior_exits.push(first_exit);
                break;
            }
            prior_exits.push(first_exit);
            let mut resume_env = base_env.clone();
            let overrides: Vec<(String, String)> = resume
                .environment
                .iter()
                .map(|(k, v)| Ok((k.clone(), vars.render(v)?)))
                .collect::<Result<_, String>>()?;
            resume_env.retain(|(k, _)| !overrides.iter().any(|(r, _)| r == k));
            resume_env.extend(overrides);
            let mut resume_argv = argv.clone();
            for arg in &resume.args {
                resume_argv.push(vars.render(arg)?);
            }
            let resume_stdin = resume.stdin.as_ref().or(a.stdin.as_ref()).map(|s| vars.render(s)).transpose()?;
            active_stdout_path = dir.join("runtime-resume-stdout.log");
            active_stderr_path = dir.join("runtime-resume-stderr.log");
            child = process::spawn(SpawnSpec {
                argv: &resume_argv,
                env: &resume_env,
                cwd: &std::env::current_dir().map_err(|e| e.to_string())?,
                stdout_path: &active_stdout_path,
                stderr_path: &active_stderr_path,
                stdin: resume_stdin,
            })
            .map_err(|e| format!("cannot relaunch runtime for resume: {e}"))?;
            resumed = true;
            continue;
        }
        if exited.is_some() {
            // Let any request already on the wire be recorded before concluding.
            provider.wait(Duration::from_millis(200));
            let m = provider.lock();
            if !m.is_done() {
                exited_early_in = Some(m.phase.label());
            }
            break;
        }
        if !seen && now >= startup_deadline.min(scenario_deadline) {
            unknowns.push(UnknownNote::new(
                UnknownReason::StartupTimeout,
                format!(
                    "no provider request within {}s (readiness: provider-request)",
                    timeout_seconds.min(cfg.timeout.as_secs_f64())
                ),
            ));
            break;
        }
        if now >= scenario_deadline {
            let phase = provider.lock().phase.label();
            unknowns.push(UnknownNote::new(
                UnknownReason::ScenarioTimeout,
                format!("scenario did not complete within {}s (state {phase})", cfg.timeout.as_secs_f64()),
            ));
            break;
        }
    }
    let exit = child.finish(Duration::from_secs_f64(a.shutdown.grace_seconds));
    let machine = provider.stop();
    if let Some(state) = exited_early_in {
        unknowns.push(UnknownNote::new(
            UnknownReason::RuntimeExitedEarly,
            format!("runtime exited ({}) before the scenario completed (state {state})", exit.describe()),
        ));
    }
    if let (Completion::ProcessExit, Some(code)) = (&a.completion, exit.exit_code) {
        if code != 0 && machine.phase == Phase::Complete {
            unknowns.push(UnknownNote::new(
                UnknownReason::RuntimeExitedEarly,
                format!("runtime exited with code {code} after completion"),
            ));
        }
    }
    let mcp = read_evidence(&evidence);
    let eval = compare::evaluate(&machine, &mcp, unknowns);
    if runtime.version.is_none() {
        resolve_runtime(&a.runtime_version, &vars, &runtime_info, runtime);
    }

    let stderr_tail = (eval.verdict == Verdict::Unknown).then(|| process::tail(&active_stderr_path, 600)).flatten();
    let mut s = ScenarioReport {
        id: def.id.to_owned(),
        summary: def.summary.to_owned(),
        verdict: eval.verdict,
        classifications: eval.classifications.clone(),
        started_at,
        duration_ms: started.elapsed().as_millis(),
        findings: eval.findings.clone(),
        unknowns: eval.unknowns.clone(),
        notes: eval.notes.clone(),
        calls: eval.calls.clone(),
        provider_requests: request_summaries(&machine),
        state_machine: StateMachineInfo { final_state: eval.final_phase.clone(), calls_issued: machine.issued.clone() },
        process: ProcessReport {
            exit,
            restarts: prior_exits.len(),
            prior_exits,
            stdout_bytes: prior_stream_bytes.0 + child.stream_bytes.0,
            stderr_bytes: prior_stream_bytes.1 + child.stream_bytes.1,
            stderr_tail,
        },
        artifacts: None,
    };
    if let Some(root) = artifacts_root {
        if cfg.artifacts_all || s.verdict != Verdict::Pass {
            let out = root.join(def.id);
            write_artifacts(&out, &machine, &mcp, &dir, &mut s)
                .map_err(|e| format!("cannot write artifacts to {}: {e}", out.display()))?;
            s.artifacts = Some(out.display().to_string());
        }
    }
    Ok(s)
}

fn read_evidence(path: &Path) -> Vec<McpEvidence> {
    fs::read_to_string(path)
        .map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}

fn resolve_runtime(src: &RuntimeVersion, vars: &TemplateVars, info_path: &Path, rt: &mut RuntimeInfo) {
    match src {
        RuntimeVersion::Unknown => {}
        RuntimeVersion::Static { value } => rt.version = Some(value.clone()),
        RuntimeVersion::Command { argv } => {
            let argv: Vec<String> = argv.iter().filter_map(|a| vars.render(a).ok()).collect();
            if let Ok(out) =
                std::process::Command::new(&argv[0]).args(&argv[1..]).stdin(std::process::Stdio::null()).output()
            {
                if out.status.success() {
                    rt.version = Some(String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|v| !v.is_empty());
                }
            }
        }
        RuntimeVersion::ReportFile => {
            if let Ok(text) = fs::read_to_string(info_path) {
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(v) => {
                        rt.name = v.get("name").and_then(|x| x.as_str()).map(str::to_owned);
                        rt.version = v.get("version").and_then(|x| x.as_str()).map(str::to_owned);
                        rt.details = Some(v);
                    }
                    Err(_) => rt.version = Some(text.trim().to_owned()).filter(|v| !v.is_empty()),
                }
            }
        }
    }
}

fn request_summaries(m: &ScenarioMachine) -> Vec<RequestSummary> {
    m.requests
        .iter()
        .map(|r| RequestSummary {
            seq: r.seq,
            offset_ms: r.offset_ms,
            received_unix_ms: r.received_unix_ms,
            method: r.method.clone(),
            path: r.path.clone(),
            role: r.role.label(),
            response_status: r.response_status,
            response_kind: r.response_kind,
            bytes: r.raw.len(),
            sha256: sha256_hex(&r.raw),
            stream: r.parsed.as_ref().map(|p| p.stream),
            tool_messages: r
                .parsed
                .as_ref()
                .map(|p| p.messages.iter().filter(|m| m.role == "tool").count())
                .unwrap_or(0),
            parse_error: r.parse_error.clone(),
            artifact: None,
        })
        .collect()
}

fn write_artifacts(
    out: &Path,
    machine: &ScenarioMachine,
    mcp: &[McpEvidence],
    workdir: &Path,
    s: &mut ScenarioReport,
) -> std::io::Result<()> {
    mkdir_private(out)?;
    // Raw MCP responses exactly as written to stdout.
    let mut counts = std::collections::HashMap::new();
    for rec in mcp.iter().filter(|r| r.event == "tool-response") {
        let Some(b64) = &rec.response_b64 else { continue };
        let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
        let call = rec.call_id.clone().unwrap_or_else(|| "unknown".into());
        let n = counts.entry(call.clone()).or_insert(0);
        *n += 1;
        let name = if *n == 1 { format!("tool-response-{call}.raw") } else { format!("tool-response-{call}-{n}.raw") };
        write_bounded(&out.join(name), &raw)?;
    }
    for (r, summary) in machine.requests.iter().zip(s.provider_requests.iter_mut()) {
        if r.raw.is_empty() {
            continue;
        }
        let name = format!("provider-request-{:03}.raw", r.seq);
        write_bounded(&out.join(&name), &r.raw)?;
        summary.artifact = Some(name);
        if let Some(h) = &r.headers {
            write_private(
                &out.join(format!("provider-request-{:03}.headers.json", r.seq)),
                &serde_json::to_vec_pretty(h)?,
            )?;
        }
    }
    for log in ["runtime-stdout.log", "runtime-stderr.log", "runtime-resume-stdout.log", "runtime-resume-stderr.log"] {
        if let Ok(bytes) = fs::read(workdir.join(log)) {
            write_bounded(&out.join(log), &bytes)?;
        }
    }
    write_private(&out.join("comparison.json"), &serde_json::to_vec_pretty(&*s)?)
}

fn mkdir_private(path: &Path) -> std::io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        mkdir_private(parent)?;
    }
    // O_NOFOLLOW: never write through a symlink planted in the artifacts tree.
    let mut f = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(bytes)
}

fn write_bounded(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.len() <= MAX_ARTIFACT_BYTES {
        return write_private(path, bytes);
    }
    let mut data = bytes[..MAX_ARTIFACT_BYTES].to_vec();
    data.extend_from_slice(
        format!("\n[boundarycheck: artifact truncated; {} of {} bytes kept]\n", MAX_ARTIFACT_BYTES, bytes.len())
            .as_bytes(),
    );
    write_private(path, &data)
}
