//! Raw request records and header redaction.

use crate::provider::protocol::ParsedRequest;
use serde::Serialize;

/// Largest request body retained in memory; larger bodies are recorded as truncated.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RequestRole {
    Initial,
    Result { turn: usize, attempt: u32 },
    Replay { resumed: bool },
    AfterCompletion,
    Rejected,
    Auxiliary,
}

impl RequestRole {
    pub fn label(&self) -> String {
        match self {
            RequestRole::Initial => "initial".into(),
            RequestRole::Result { turn, attempt } => format!("result turn={} attempt={attempt}", turn + 1),
            RequestRole::Replay { resumed } => {
                if *resumed {
                    "replay after resume".into()
                } else {
                    "replay same process".into()
                }
            }
            RequestRole::AfterCompletion => "after-completion".into(),
            RequestRole::Rejected => "rejected".into(),
            RequestRole::Auxiliary => "auxiliary".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RequestRecord {
    pub seq: u64,
    pub received_unix_ms: u128,
    pub offset_ms: u128,
    pub method: String,
    pub path: String,
    /// Raw HTTP body captured before any parsing.
    pub raw: Vec<u8>,
    pub raw_truncated: bool,
    /// Only populated with --save-headers; always redacted.
    pub headers: Option<Vec<(String, String)>>,
    pub role: RequestRole,
    /// Call IDs issued to the runtime before this request arrived.
    pub issued_before: Vec<String>,
    pub response_status: u16,
    pub response_kind: &'static str,
    pub parsed: Option<ParsedRequest>,
    pub parse_error: Option<String>,
}

const SENSITIVE_EXACT: &[&str] = &["authorization", "proxy-authorization", "cookie", "set-cookie"];
const SENSITIVE_PARTS: &[&str] = &["key", "token", "secret", "auth", "cookie", "session", "password", "credential"];

pub fn is_sensitive_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SENSITIVE_EXACT.contains(&n.as_str()) || SENSITIVE_PARTS.iter().any(|p| n.contains(p))
}

pub fn redact_headers<'a>(headers: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .map(|(k, v)| {
            let value = if is_sensitive_header(k) { "[REDACTED]".to_owned() } else { v.to_owned() };
            (k.to_owned(), value)
        })
        .collect()
}

/// Redact secrets from a command line before it is written to a report:
/// `--api-key X`, `--token=X`, `OPENAI_API_KEY=X`, `Bearer X`.
pub fn redact_argv(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut redact_next = false;
    for a in argv {
        if redact_next {
            out.push("[REDACTED]".to_owned());
            redact_next = false;
            continue;
        }
        if let Some((k, _)) = a.split_once('=') {
            if is_sensitive_header(k.trim_start_matches('-')) {
                out.push(format!("{k}=[REDACTED]"));
                continue;
            }
        }
        if a.starts_with("--") && is_sensitive_header(&a[2..]) {
            redact_next = true;
        }
        if looks_like_secret(a) {
            out.push("[REDACTED]".to_owned());
            continue;
        }
        if a.to_ascii_lowercase().starts_with("bearer ") {
            out.push("Bearer [REDACTED]".to_owned());
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// Well-known credential token shapes passed as bare arguments.
fn looks_like_secret(a: &str) -> bool {
    const PREFIXES: &[&str] =
        &["sk-", "sk_", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "AKIA", "AIza", "glpat-"];
    a.len() >= 16 && !a.contains(char::is_whitespace) && PREFIXES.iter().any(|p| a.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_redaction() {
        let h = redact_headers([
            ("Authorization", "Bearer sk-live-123"),
            ("Content-Type", "application/json"),
            ("X-Api-Key", "abc"),
            ("Cookie", "s=1"),
            ("OpenAI-Organization", "org-1"),
            ("x-stainless-retry-count", "1"),
        ]);
        assert_eq!(h[0].1, "[REDACTED]");
        assert_eq!(h[1].1, "application/json");
        assert_eq!(h[2].1, "[REDACTED]");
        assert_eq!(h[3].1, "[REDACTED]");
        assert_eq!(h[4].1, "org-1");
        assert_eq!(h[5].1, "1");
    }

    #[test]
    fn argv_redaction() {
        let argv: Vec<String> = [
            "python",
            "agent.py",
            "--api-key",
            "sk-1",
            "--token=abc",
            "OPENAI_API_KEY=sk-2",
            "--model",
            "x",
            "Bearer zzz",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            redact_argv(&argv),
            vec![
                "python",
                "agent.py",
                "--api-key",
                "[REDACTED]",
                "--token=[REDACTED]",
                "OPENAI_API_KEY=[REDACTED]",
                "--model",
                "x",
                "Bearer [REDACTED]"
            ]
        );
    }
}
