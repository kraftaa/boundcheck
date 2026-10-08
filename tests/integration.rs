//! End-to-end tests: boundarycheck launches the fake provider, the fixture
//! runtime (a real Python process) and, through it, the fake MCP server.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_boundarycheck");

struct Run {
    code: i32,
    report: Value,
    stdout: String,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn boundarycheck(extra: &[&str], agent: &[&str]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let out = Command::new(BIN)
        .current_dir(root())
        .args(["run", "--adapter", "fixture-agent", "--report"])
        .arg(&report)
        .args(extra)
        .arg("--")
        .args(agent)
        .output()
        .expect("run boundarycheck");
    let report = std::fs::read(&report).map(|b| serde_json::from_slice(&b).unwrap()).unwrap_or(Value::Null);
    Run { code: out.status.code().unwrap_or(-1), report, stdout: String::from_utf8_lossy(&out.stdout).into_owned() }
}

fn conforming(extra: &[&str]) -> Run {
    boundarycheck(extra, &["python3", "fixtures/conforming-agent/agent.py"])
}

fn faulty(fault: &str, scenarios: &[&str]) -> Run {
    let mut extra = vec![];
    for s in scenarios {
        extra.extend(["--scenario", s]);
    }
    boundarycheck(&extra, &["python3", "fixtures/faulty-agent/agent.py", "--fault", fault])
}

fn scenario<'a>(r: &'a Run, id: &str) -> &'a Value {
    r.report["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no scenario {id}"))
}

fn classes(s: &Value) -> Vec<String> {
    s["classifications"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_owned()).collect()
}

#[test]
fn conforming_fixture_passes_every_scenario() {
    let r = conforming(&[]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let scenarios = r.report["scenarios"].as_array().unwrap();
    assert_eq!(scenarios.len(), 23);
    for s in scenarios {
        assert_eq!(s["verdict"], "PASS", "{}: {}", s["id"], s);
    }
    assert_eq!(r.report["schema_version"], "boundarycheck.report/v1");
    assert_eq!(r.report["provider_protocol"], "openai-chat-completions");
    assert_eq!(r.report["runtime"]["version"], "0.1.0");
    assert_eq!(r.report["exit"]["code"], 0);
    assert_eq!(r.report["command"]["argv"][1], "fixtures/conforming-agent/agent.py");
    assert!(r.stdout.contains("Summary: 23 passed, 0 failed, 0 unknown"));

    // Correlation IDs survived the round trip and hashes match on both sides.
    let call = &scenario(&r, "exact-text")["calls"][0];
    assert_eq!(call["call_id"], "BC_CALL_000001");
    assert_eq!(call["tool"]["sha256"], call["provider"][0]["sha256"]);
    assert_eq!(call["provider"][0]["extracted_text"], "identical");
    // The retry scenario really saw a 429 followed by the retried request.
    let statuses: Vec<i64> = scenario(&r, "retry-429")["provider_requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["response_status"].as_i64().unwrap())
        .collect();
    assert_eq!(statuses, vec![200, 429, 200]);
    assert_eq!(scenario(&r, "persistence-resume")["process"]["restarts"], 1);
    assert_eq!(scenario(&r, "persistence-resume")["provider_requests"][2]["role"], "replay after resume");
}

#[test]
fn legal_reordering_of_concurrent_results_passes() {
    let r = conforming(&["--scenario", "concurrent-two-tools"]);
    assert_eq!(r.code, 0);
    let notes = scenario(&r, "concurrent-two-tools")["notes"].as_array().unwrap();
    assert!(notes.iter().any(|n| n.as_str().unwrap().contains("MCP calls completed in order")));
    assert!(notes.iter().any(|n| n.as_str().unwrap().contains("legal reordering")));
    let r2 = boundarycheck(
        &["--scenario", "concurrent-two-tools"],
        &["python3", "fixtures/conforming-agent/agent.py", "--reorder"],
    );
    assert_eq!(r2.code, 0, "{}", r2.stdout);
    let s = scenario(&r2, "concurrent-two-tools");
    assert_eq!(s["verdict"], "PASS");
}

#[test]
fn faulty_fixture_faults_are_detected() {
    let cases: &[(&str, &str, &str)] = &[
        ("truncate", "large-text-100k", "Truncation"),
        ("head-tail", "large-text-100k", "Truncation"),
        ("head-tail-marker", "large-text-50k", "Truncation"),
        ("utf8-split", "large-text-100k", "InvalidUtf8"),
        ("swap", "concurrent-two-tools", "WrongToolCallAssociation"),
        ("duplicate", "exact-text", "DuplicateResult"),
        ("missing", "concurrent-two-tools", "MissingResult"),
        ("retry-mutation", "retry-429", "RetryMutation"),
        ("normalize", "unicode-boundaries", "ContentMutation"),
        ("json-float", "structured-json", "StructuralMutation"),
        ("echo-user", "concurrent-two-tools", "DuplicateResult"),
        ("json-dup-key", "structured-json", "StructuralMutation"),
        ("replay-mutation", "replay-history", "ReplayMutation"),
        ("replay-mutation", "persistence-resume", "ReplayMutation"),
    ];
    let handles: Vec<_> = cases
        .iter()
        .map(|&(fault, id, class)| std::thread::spawn(move || (fault, id, class, faulty(fault, &[id, "exact-text"]))))
        .collect();
    for h in handles {
        let (fault, id, class, r) = h.join().unwrap();
        assert_eq!(r.code, 1, "{fault}: {}", r.stdout);
        let s = scenario(&r, id);
        assert_eq!(s["verdict"], "FAIL", "{fault}");
        assert!(classes(s).contains(&class.to_owned()), "{fault}: expected {class}, got {:?}", classes(s));
        if !matches!(fault, "duplicate" | "missing" | "echo-user") {
            assert_eq!(scenario(&r, "exact-text")["verdict"], "PASS", "{fault} must not affect exact-text");
        }
    }
}

#[test]
fn retry_and_replay_findings_keep_their_underlying_cause() {
    let cases = [
        ("retry-mutation", "retry-429", "RetryMutation", "ContentMutation"),
        ("replay-mutation", "replay-history", "ReplayMutation", "ContentMutation"),
        ("replay-drop", "replay-history", "ReplayMutation", "MissingResult"),
        ("replay-drop", "persistence-resume", "ReplayMutation", "MissingResult"),
    ];
    for (fault, id, class, cause) in cases {
        let r = faulty(fault, &[id]);
        let s = scenario(&r, id);
        let f = s["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["class"] == class)
            .unwrap_or_else(|| panic!("{fault}: {s}"));
        assert_eq!(f["underlying_class"], cause, "{fault}");
        assert!(s["underlying_classifications"].as_array().unwrap().iter().any(|c| c == cause), "{fault}");
        assert!(r.stdout.contains(&format!("{class} (underlying: {cause})")), "{fault}: {}", r.stdout);
    }
}

#[test]
fn threshold_sizes_isolate_a_round_number_cutoff() {
    let sizes = ["large-text:65535", "large-text:65536", "large-text:65537", "large-text:1m"];
    let r = faulty("truncate-64k", &sizes);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(scenario(&r, "large-text:65535")["verdict"], "PASS");
    assert_eq!(scenario(&r, "large-text:65536")["verdict"], "PASS");
    for id in ["large-text:65537", "large-text:1048576"] {
        let f = &scenario(&r, id)["findings"][0];
        assert_eq!(f["class"], "Truncation", "{id}");
        assert_eq!(f["text_difference"]["provider_bytes"], 65536, "{id}");
    }
    let bad = conforming(&["--scenario", "large-text:9m"]);
    assert_eq!(bad.code, 2);
}

#[test]
fn truncation_evidence_is_precise() {
    let r = faulty("head-tail", &["large-text-100k"]);
    let f = &scenario(&r, "large-text-100k")["findings"][0];
    let d = &f["text_difference"];
    assert_eq!(d["tool_bytes"], 102400);
    assert_eq!(d["shape"], "head-tail-retention");
    assert_eq!(f["missing_sentinels"], serde_json::json!(["25%", "middle", "75%"]));
    assert!(r.stdout.contains("tool content:       102,400 bytes"), "{}", r.stdout);

    let r = faulty("swap", &["concurrent-two-tools"]);
    let s = scenario(&r, "concurrent-two-tools");
    let f = s["findings"].as_array().unwrap().iter().find(|f| f["call_id"] == "BC_CALL_000001").unwrap();
    assert_eq!(f["content_matches_call"], "BC_CALL_000002");
}

#[test]
fn mcp_content_policy_accepts_legal_and_rejects_illegal_representations() {
    // (fault, scenario, expected verdict, expected class or note fragment)
    let cases: &[(&str, &str, &str, &str)] = &[
        ("join-newline", "multi-text-blocks", "PASS", "text blocks joined with"),
        ("join-space", "multi-text-blocks", "FAIL", "ContentMutation"),
        ("drop-block", "multi-text-blocks", "FAIL", "Truncation"),
        ("send-structured", "structured-content", "PASS", "structuredContent instead of the text"),
        ("structured-mutate", "structured-content", "FAIL", "StructuralMutation"),
        ("error-generic", "mcp-protocol-error", "FAIL", "ContentMutation"),
    ];
    for &(fault, id, verdict, expect) in cases {
        let r = faulty(fault, &[id]);
        let s = scenario(&r, id);
        assert_eq!(s["verdict"], verdict, "{fault}: {s}");
        if verdict == "PASS" {
            assert!(
                s["notes"].as_array().unwrap().iter().any(|n| n.as_str().unwrap().contains(expect)),
                "{fault}: {s}"
            );
        } else {
            assert!(classes(s).contains(&expect.to_owned()), "{fault}: {:?}", classes(s));
        }
    }
    let mutated = faulty("structured-mutate", &["structured-content"]);
    let f = &scenario(&mutated, "structured-content")["findings"][0];
    assert_eq!(f["json_difference"]["path"], "/run_id");
}

#[test]
fn probe_content_is_unknown_with_a_description_of_what_happened() {
    let r = boundarycheck(&["--probes"], &["python3", "fixtures/conforming-agent/agent.py"]);
    assert_eq!(r.code, 3, "{}", r.stdout);
    for id in ["mixed-content", "embedded-resource", "resource-link", "audio-content"] {
        let s = scenario(&r, id);
        assert_eq!(s["verdict"], "UNKNOWN", "{id}");
        assert_eq!(s["unknowns"][0]["reason"], "unrepresentable-content", "{id}");
        assert!(s["unknowns"][0]["detail"].as_str().unwrap().contains("dropped"), "{id}");
    }
    // A text block lost next to an image is still a proven failure.
    let r = faulty("drop-block", &["mixed-content"]);
    assert_eq!(scenario(&r, "mixed-content")["verdict"], "FAIL");
    // Probes are opt-in: the default run has no probe scenarios.
    let r = conforming(&["--scenario", "exact-text"]);
    assert_eq!(r.code, 0);
}

fn run_adapter(adapter: &str, extra: &[&str], agent: &[&str]) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let out = Command::new(BIN)
        .current_dir(root())
        .args(["run", "--adapter", adapter, "--report"])
        .arg(&report)
        .args(extra)
        .arg("--")
        .args(agent)
        .output()
        .expect("run boundarycheck");
    let report = std::fs::read(&report).map(|b| serde_json::from_slice(&b).unwrap()).unwrap_or(Value::Null);
    Run { code: out.status.code().unwrap_or(-1), report, stdout: String::from_utf8_lossy(&out.stdout).into_owned() }
}

#[test]
fn responses_api_conforming_fixture_passes_with_and_without_previous_response_id() {
    for flags in [vec![], vec!["--previous-response-id"]] {
        let mut agent = vec!["python3", "fixtures/conforming-agent/agent.py"];
        agent.extend(flags.iter().copied());
        let r = run_adapter("fixture-agent-responses", &[], &agent);
        assert_eq!(r.code, 0, "{flags:?}: {}", r.stdout);
        assert_eq!(r.report["provider_protocol"], "openai-responses");
        assert_eq!(r.report["summary"]["passed"], 23);
        let paths: Vec<&str> = scenario(&r, "exact-text")["provider_requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|q| q["path"].as_str().unwrap())
            .collect();
        assert!(paths.iter().all(|p| p.ends_with("/v1/responses")), "{paths:?}");
    }
    // Images are representable in Responses: mixed-content is a real PASS/FAIL there.
    let r = run_adapter("fixture-agent-responses", &["--probes"], &["python3", "fixtures/conforming-agent/agent.py"]);
    assert_eq!(scenario(&r, "mixed-content")["verdict"], "PASS");
    assert_eq!(scenario(&r, "audio-content")["verdict"], "UNKNOWN");
    let r = run_adapter(
        "fixture-agent-responses",
        &["--scenario", "mixed-content"],
        &["python3", "fixtures/faulty-agent/agent.py", "--fault", "drop-block"],
    );
    assert_eq!(scenario(&r, "mixed-content")["verdict"], "FAIL");
}

#[test]
fn responses_api_faults_are_detected() {
    let cases: &[(&str, &str, &str)] = &[
        ("truncate", "large-text-100k", "Truncation"),
        ("swap", "concurrent-two-tools", "WrongToolCallAssociation"),
        ("missing", "concurrent-two-tools", "MissingResult"),
        ("duplicate", "exact-text", "DuplicateResult"),
        ("retry-mutation", "retry-429", "RetryMutation"),
        ("replay-drop", "replay-history", "ReplayMutation"),
        ("json-float", "structured-json", "StructuralMutation"),
    ];
    for &(fault, id, class) in cases {
        let r = run_adapter(
            "fixture-agent-responses",
            &["--scenario", id],
            &["python3", "fixtures/faulty-agent/agent.py", "--fault", fault],
        );
        assert_eq!(r.code, 1, "{fault}: {}", r.stdout);
        assert!(classes(scenario(&r, id)).contains(&class.to_owned()), "{fault}: {:?}", classes(scenario(&r, id)));
    }
}

const TRANSPORT_FAULTS: &[&str] = &[
    "retry-500",
    "retry-503",
    "retry-repeated",
    "disconnect-after-request",
    "connection-reset",
    "truncated-response",
    "slow-response",
];

#[test]
fn transport_faults_are_injected_and_changed_retries_are_caught() {
    for adapter in ["fixture-agent", "fixture-agent-responses"] {
        let mut extra = vec![];
        for id in TRANSPORT_FAULTS {
            extra.extend(["--scenario", id]);
        }
        let ok = run_adapter(adapter, &extra, &["python3", "fixtures/conforming-agent/agent.py"]);
        assert_eq!(ok.code, 0, "{adapter}: {}", ok.stdout);
        let kinds = |id: &str| -> Vec<String> {
            scenario(&ok, id)["provider_requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|q| q["response_kind"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(kinds("retry-repeated"), ["tool-calls", "fault-http-429", "fault-http-429", "final-answer"]);
        assert_eq!(kinds("connection-reset"), ["tool-calls", "fault-reset", "final-answer"]);
        assert_eq!(kinds("truncated-response"), ["tool-calls", "fault-truncated", "final-answer"]);
        assert_eq!(kinds("slow-response"), ["tool-calls", "delayed"]);
        let bad =
            run_adapter(adapter, &extra, &["python3", "fixtures/faulty-agent/agent.py", "--fault", "retry-mutation"]);
        for id in TRANSPORT_FAULTS.iter().filter(|id| **id != "slow-response") {
            let s = scenario(&bad, id);
            assert_eq!(s["verdict"], "FAIL", "{adapter} {id}");
            assert!(classes(s).contains(&"RetryMutation".to_owned()), "{adapter} {id}: {:?}", classes(s));
        }
        assert_eq!(scenario(&bad, "slow-response")["verdict"], "PASS", "no retry happened, so nothing to mutate");
    }
}

#[test]
fn legal_json_reserialization_passes() {
    let r = faulty("reserialize-json", &["structured-json"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let p = &scenario(&r, "structured-json")["calls"][0]["provider"][0];
    assert_eq!(p["extracted_text"], "changed");
    assert_eq!(p["json_semantics"], "identical");
}

#[test]
fn runtime_exiting_early_is_unknown() {
    let r = faulty("exit-early", &["exact-text"]);
    assert_eq!(r.code, 3, "{}", r.stdout);
    let s = scenario(&r, "exact-text");
    assert_eq!(s["verdict"], "UNKNOWN");
    assert_eq!(s["unknowns"][0]["reason"], "runtime-exited-early");
    assert!(s["findings"].as_array().unwrap().is_empty());
}

#[test]
fn hanging_runtime_times_out_and_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("agent.pid");
    let r = boundarycheck(
        &["--scenario", "exact-text", "--timeout", "3"],
        &["python3", "fixtures/faulty-agent/agent.py", "--fault", "hang", "--pid-file", pid_file.to_str().unwrap()],
    );
    assert_eq!(r.code, 3, "{}", r.stdout);
    let s = scenario(&r, "exact-text");
    assert_eq!(s["unknowns"][0]["reason"], "scenario-timeout");
    assert_eq!(s["process"]["terminated_by_harness"], true);
    let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let alive = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success();
    assert!(!alive, "runtime process {pid} must be terminated");
}

#[test]
fn harness_and_configuration_errors_exit_2() {
    let r = boundarycheck(&["--scenario", "exact-text"], &["/nonexistent/agent-binary"]);
    assert_eq!(r.code, 2);
    assert!(r.report["harness_error"].as_str().unwrap().contains("cannot launch"));

    let out = Command::new(BIN)
        .current_dir(root())
        .args(["run", "--adapter", "no-such-adapter", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let out = Command::new(BIN)
        .current_dir(root())
        .args(["run", "--adapter", "fixture-agent", "--scenario", "nope", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("no-replay.json");
    let mut adapter: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("adapters/fixture-agent.json")).unwrap()).unwrap();
    adapter.as_object_mut().unwrap().remove("capabilities");
    adapter.as_object_mut().unwrap().remove("resume");
    std::fs::write(&manifest, serde_json::to_vec(&adapter).unwrap()).unwrap();
    let out = Command::new(BIN)
        .current_dir(root())
        .args(["run", "--adapter", manifest.to_str().unwrap(), "--scenario", "persistence-resume", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires adapter capability `persistence-resume`"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let target = dir.path().join("report-target");
        std::fs::write(&target, b"private").unwrap();
        let link = dir.path().join("report-link");
        symlink(&target, &link).unwrap();
        let out = Command::new(BIN)
            .current_dir(root())
            .args([
                "run",
                "--adapter",
                "fixture-agent",
                "--scenario",
                "exact-text",
                "--report",
                link.to_str().unwrap(),
                "--",
                "/nonexistent/agent-binary",
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(std::fs::read(&target).unwrap(), b"private");
    }
}

#[test]
fn runtime_without_the_tool_is_adapter_insufficient() {
    // Point the MCP command at a server that exposes no tools by giving the fixture a bogus MCP command.
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("adapter.json");
    let mut m: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("adapters/fixture-agent.json")).unwrap()).unwrap();
    m["environment"]["BOUNDARYCHECK_MCP_ARGS"] = Value::String(r#"["-c","import sys,json\nfor l in sys.stdin:\n m=json.loads(l)\n if 'id' in m: print(json.dumps({'jsonrpc':'2.0','id':m['id'],'result':{'tools':[]}}),flush=True)"]"#.into());
    m["environment"]["BOUNDARYCHECK_MCP_COMMAND"] = Value::String("python3".into());
    std::fs::write(&manifest, serde_json::to_vec(&m).unwrap()).unwrap();
    let report = dir.path().join("r.json");
    let out = Command::new(BIN)
        .current_dir(root())
        .args([
            "run",
            "--adapter",
            manifest.to_str().unwrap(),
            "--scenario",
            "exact-text",
            "--report",
            report.to_str().unwrap(),
        ])
        .args(["--", "python3", "fixtures/conforming-agent/agent.py"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", String::from_utf8_lossy(&out.stdout));
    let r: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    let reasons: Vec<&str> =
        r["scenarios"][0]["unknowns"].as_array().unwrap().iter().map(|u| u["reason"].as_str().unwrap()).collect();
    assert!(reasons.contains(&"adapter-insufficient"), "{reasons:?}");
}

#[test]
fn artifacts_are_deterministic_private_and_redacted() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let art = dir.path().join("evidence");
    let art_s = art.to_str().unwrap();
    let r = boundarycheck(
        &["--scenario", "large-text-100k", "--scenario", "exact-text", "--artifacts", art_s, "--save-headers"],
        &["python3", "fixtures/faulty-agent/agent.py", "--fault", "truncate"],
    );
    assert_eq!(r.code, 1);
    let run = art.join("BC_RUN_000001");
    let sc = run.join("large-text-100k");
    for f in [
        "comparison.json",
        "tool-response-BC_CALL_000001.raw",
        "provider-request-002.raw",
        "provider-request-002.headers.json",
    ] {
        let p = sc.join(f);
        assert!(p.is_file(), "missing {}", p.display());
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600, "{f}");
    }
    assert!(run.join("run.json").is_file());
    assert!(!run.join("exact-text").exists(), "passing scenarios are not saved without --artifacts-all");
    let headers = std::fs::read_to_string(sc.join("provider-request-002.headers.json")).unwrap();
    assert!(headers.contains("[REDACTED]") && !headers.contains("boundarycheck-test-key"), "{headers}");
    // The raw MCP response is byte-exact JSON-RPC containing the full payload.
    let raw: Value =
        serde_json::from_slice(&std::fs::read(sc.join("tool-response-BC_CALL_000001.raw")).unwrap()).unwrap();
    assert_eq!(raw["result"]["content"][0]["text"].as_str().unwrap().len(), 102400);
    assert_eq!(Path::new(&r.report["artifacts"].as_str().unwrap()), run);

    let r2 = boundarycheck(
        &["--scenario", "exact-text", "--artifacts", art_s],
        &["python3", "fixtures/conforming-agent/agent.py"],
    );
    assert_eq!(r2.report["run_id"], "BC_RUN_000002");
}

#[test]
fn null_tool_content_is_a_proven_failure() {
    // A runtime that sends `content: null` for a tool result: the content was lost.
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("null_agent.py");
    std::fs::write(
        &agent,
        format!(
            "import sys; sys.path.insert(0, {:?})\nimport bc_agent\nclass H(bc_agent.Hooks):\n    def tool_messages(self, ms):\n        return [dict(m, content=None) for m in ms]\nsys.exit(bc_agent.run('null-content', hooks=H()))\n",
            root().join("fixtures").to_str().unwrap()
        ),
    )
    .unwrap();
    let r = boundarycheck(&["--scenario", "exact-text"], &["python3", agent.to_str().unwrap()]);
    assert_eq!(r.code, 1, "{}", r.stdout);
    assert_eq!(classes(scenario(&r, "exact-text")), vec!["Truncation"]);
}

#[test]
fn tool_result_under_unissued_id_is_not_a_silent_pass() {
    let r = faulty("extra-id", &["exact-text"]);
    assert_eq!(r.code, 3, "{}", r.stdout);
    let s = scenario(&r, "exact-text");
    assert_eq!(s["unknowns"][0]["reason"], "ambiguous-correlation");
}

#[test]
fn processes_that_leave_the_process_group_are_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("escapee.pid");
    let r = boundarycheck(
        &["--scenario", "exact-text", "--timeout", "3"],
        &["python3", "fixtures/faulty-agent/agent.py", "--fault", "escape", "--pid-file", pid_file.to_str().unwrap()],
    );
    assert_eq!(r.code, 3, "{}", r.stdout);
    let pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let alive = Command::new("kill").args(["-0", &pid]).stderr(std::process::Stdio::null()).status().unwrap().success();
    assert!(!alive, "setsid() child {pid} must be terminated");
}

#[test]
fn interrupt_kills_children_and_removes_workdir() {
    use std::os::unix::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("agent.pid");
    let mut child = Command::new(BIN)
        .current_dir(root())
        .env("TMPDIR", dir.path())
        .args(["run", "--adapter", "fixture-agent", "--scenario", "exact-text", "--timeout", "30", "--"])
        .args([
            "python3",
            "fixtures/faulty-agent/agent.py",
            "--fault",
            "hang",
            "--pid-file",
            pid_file.to_str().unwrap(),
        ])
        .stdout(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let t = std::time::Instant::now();
    while !pid_file.exists() && t.elapsed() < std::time::Duration::from_secs(15) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let agent_pid = std::fs::read_to_string(&pid_file).unwrap().trim().to_owned();
    Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(2));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let alive =
        Command::new("kill").args(["-0", &agent_pid]).stderr(std::process::Stdio::null()).status().unwrap().success();
    assert!(!alive, "runtime {agent_pid} must be terminated on interrupt");
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("boundarycheck-"))
        .collect();
    assert!(leftovers.is_empty(), "work directory must be removed on interrupt");
}

/// Opt-in (needs a container engine): BOUNDARYCHECK_DOCKER_IMAGE=python:3.13-slim-trixie
/// and, on macOS, BOUNDARYCHECK_DOCKER_BINARY=<Linux boundarycheck>.
#[test]
fn docker_isolation_runs_without_network_or_host_files() {
    let Ok(image) = std::env::var("BOUNDARYCHECK_DOCKER_IMAGE") else {
        eprintln!("skipped: set BOUNDARYCHECK_DOCKER_IMAGE to run");
        return;
    };
    let binary = std::env::var("BOUNDARYCHECK_DOCKER_BINARY").unwrap_or_else(|_| BIN.to_owned());
    let dir = tempfile::tempdir_in(root().join("target")).unwrap();
    let report = dir.path().join("r.json");
    let probe = dir.path().join("probe.py");
    let host_home = std::env::var("HOME").unwrap();
    std::fs::write(
        &probe,
        format!(
            "import os,socket,sys\n\
             try:\n    socket.create_connection(('1.1.1.1', 443), timeout=3); print('PROBE net open', file=sys.stderr)\n\
             except OSError: print('PROBE net blocked', file=sys.stderr)\n\
             print('PROBE home', os.path.exists({host_home:?}), file=sys.stderr)\n\
             print('PROBE leak', 'BC_TEST_SECRET' in os.environ, file=sys.stderr)\n"
        ),
    )
    .unwrap();
    let run = |agent: &[&str], extra: &[&str]| {
        Command::new(BIN)
            .current_dir(root())
            .env("BC_TEST_SECRET", "x")
            .args(["run", "--adapter", "fixture-agent", "--isolation", "docker", "--image", &image])
            .args(["--isolation-binary", &binary, "--report", report.to_str().unwrap()])
            .args(extra)
            .arg("--")
            .args(agent)
            .output()
            .unwrap()
    };
    let out = run(
        &["python3", "fixtures/conforming-agent/agent.py"],
        &["--scenario", "exact-text", "--scenario", "persistence-resume"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let r: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert!(r["isolation"].as_str().unwrap().contains("network=none"));
    assert_eq!(r["summary"]["passed"], 2);

    let art = dir.path().join("ev");
    let rel_probe = probe.strip_prefix(root()).unwrap().to_str().unwrap().to_owned();
    run(
        &["python3", &rel_probe],
        &["--scenario", "exact-text", "--timeout", "15", "--artifacts", art.to_str().unwrap()],
    );
    let stderr = std::fs::read_to_string(art.join("BC_RUN_000001/exact-text/runtime-stderr.log")).unwrap();
    assert!(stderr.contains("PROBE net blocked"), "{stderr}");
    assert!(stderr.contains("PROBE home False"), "{stderr}");
    assert!(stderr.contains("PROBE leak False"), "{stderr}");
}
