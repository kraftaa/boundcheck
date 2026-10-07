//! JSON report schema and concise human-readable output.

use crate::compare::{CallReport, Finding};
use crate::model::verdict::{FailureClass, UnknownNote, Verdict};
use crate::process::ExitInfo;
use serde::Serialize;
use serde_json::Value;
use std::fmt::Write as _;

pub const SCHEMA_VERSION: &str = "boundarycheck.report/v1";

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub schema_version: &'static str,
    pub boundarycheck_version: &'static str,
    pub run_id: String,
    pub started_at: String,
    pub duration_ms: u128,
    pub adapter: AdapterInfo,
    pub runtime: RuntimeInfo,
    pub provider_protocol: &'static str,
    pub command: CommandInfo,
    pub platform: Platform,
    pub scenarios: Vec<ScenarioReport>,
    pub summary: Summary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness_error: Option<String>,
    pub exit: ExitSummary,
}

#[derive(Debug, Serialize)]
pub struct AdapterInfo {
    pub name: String,
    pub source: String,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct RuntimeInfo {
    /// `null` means the version could not be determined (not "no version").
    pub name: Option<String>,
    pub version: Option<String>,
    pub version_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct CommandInfo {
    /// Argument vector exactly as executed (no shell), with secret-looking values redacted.
    pub argv: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Platform {
    pub os: &'static str,
    pub arch: &'static str,
}

#[derive(Debug, Serialize, Default)]
pub struct Summary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub unknown: usize,
}

#[derive(Debug, Serialize)]
pub struct ExitSummary {
    pub code: i32,
    pub classification: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ScenarioReport {
    pub id: String,
    pub summary: String,
    pub verdict: Verdict,
    pub classifications: Vec<FailureClass>,
    pub started_at: String,
    pub duration_ms: u128,
    pub findings: Vec<Finding>,
    pub unknowns: Vec<UnknownNote>,
    pub notes: Vec<String>,
    pub calls: Vec<CallReport>,
    pub provider_requests: Vec<RequestSummary>,
    pub state_machine: StateMachineInfo,
    pub process: ProcessReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StateMachineInfo {
    pub final_state: String,
    pub calls_issued: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RequestSummary {
    pub seq: u64,
    pub offset_ms: u128,
    pub received_unix_ms: u128,
    pub method: String,
    pub path: String,
    pub role: String,
    pub response_status: u16,
    pub response_kind: &'static str,
    pub bytes: usize,
    pub sha256: String,
    pub stream: Option<bool>,
    pub tool_messages: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProcessReport {
    #[serde(flatten)]
    pub exit: ExitInfo,
    /// Completed runtime processes before the final one (for resume scenarios).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub prior_exits: Vec<ExitInfo>,
    pub restarts: usize,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
}

pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Concise human-readable rendering of one scenario.
pub fn render_scenario(s: &ScenarioReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{} {}", s.verdict.label(), s.id);
    match s.verdict {
        Verdict::Pass => {
            for n in s.notes.iter().filter(|n| n.contains("JSON semantics identical") || n.contains("legal reordering"))
            {
                let _ = writeln!(out, "  note: {n}");
            }
        }
        Verdict::Fail => render_failure(&mut out, s),
        Verdict::Unknown => {
            for u in s.unknowns.iter().take(3) {
                let _ = writeln!(out, "  reason: {} — {}", u.reason.name(), u.detail);
            }
            if let Some(t) = &s.process.stderr_tail {
                let last: Vec<&str> = t.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect();
                for l in last {
                    let _ = writeln!(out, "  stderr: {}", crate::compare::content::excerpt(l.as_bytes(), 160));
                }
            }
        }
    }
    out
}

fn render_failure(out: &mut String, s: &ScenarioReport) {
    let Some(head) = s.findings.iter().min_by_key(|f| (f.class, f.request_seq)) else { return };
    let call = head.call_id.as_deref().unwrap_or("?");
    let _ = writeln!(out, "  call:               {call} (provider request #{})", head.request_seq.unwrap_or(0));
    if let Some(d) = &head.text_difference {
        let w = thousands(d.tool_bytes.max(d.provider_bytes)).len();
        let _ = writeln!(out, "  tool content:       {:>w$} bytes", thousands(d.tool_bytes));
        let _ = writeln!(out, "  provider content:   {:>w$} bytes", thousands(d.provider_bytes));
        let _ = writeln!(out, "  first difference:   byte {}", thousands(d.first_difference));
        if !head.missing_sentinels.is_empty() {
            let _ = writeln!(out, "  missing sentinels:  {}", head.missing_sentinels.join(", "));
        }
        if let (Some(x), None) = (&d.inserted_excerpt, &head.json_difference) {
            let _ = writeln!(out, "  inserted text:      \"{x}\"");
        }
    }
    if let Some(j) = &head.json_difference {
        let _ = writeln!(out, "  first JSON change:  {} ({})", j.path, j.kind);
        let _ = writeln!(out, "  expected / actual:  {} / {}", j.expected, j.actual);
    }
    if head.text_difference.is_none() && head.json_difference.is_none() {
        let _ = writeln!(out, "  evidence:           {}", head.summary);
    }
    if let Some(side) = s
        .calls
        .iter()
        .find(|c| c.call_id == call)
        .and_then(|c| c.provider.iter().find(|p| Some(p.request_seq) == head.request_seq))
    {
        if side.occurrences > 0 {
            let _ = writeln!(out, "  transport representation: {}", side.transport_representation);
            let _ = writeln!(out, "  extracted text bytes:     {}", side.extracted_text);
            if side.json_semantics != "not-applicable" {
                let _ = writeln!(out, "  JSON semantics:           {}", side.json_semantics);
            }
        }
    }
    let label = match (&head.text_difference, &head.json_difference) {
        (Some(d), None) => format!(" — {}", d.label),
        _ => String::new(),
    };
    let _ = writeln!(out, "  classification:     {}{label}", head.class.name());
    let mut seen = vec![(head.class, head.call_id.clone())];
    for f in &s.findings {
        let key = (f.class, f.call_id.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        if seen.len() > 6 {
            let _ = writeln!(out, "  ...");
            break;
        }
        let _ = writeln!(
            out,
            "  also: {} {} (request #{}): {}",
            f.class.name(),
            f.call_id.as_deref().unwrap_or("?"),
            f.request_seq.unwrap_or(0),
            f.summary
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separator() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(102400), "102,400");
        assert_eq!(thousands(1234567), "1,234,567");
    }
}
