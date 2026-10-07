//! Scenario registry shared by the fake provider (state machine) and the fake
//! MCP server (payload generation).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentKind {
    Text,
    Json,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tier {
    Mvp,
    PostMvp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayMode {
    None,
    SameProcess,
    Resume,
}

#[derive(Debug)]
pub struct ScenarioDef {
    pub id: &'static str,
    pub summary: &'static str,
    pub kind: ContentKind,
    pub tier: Tier,
    /// Assistant turns. Each inner slice lists the call IDs issued together in
    /// one provider response.
    pub turns: &'static [&'static [&'static str]],
    /// Answer the first request that carries tool results with a deterministic
    /// HTTP 429, then expect the runtime to retry.
    pub rate_limit_first_result: bool,
    /// Ask the runtime to submit the completed tool-result history again,
    /// either in the same process or after a checkpoint/restart.
    pub replay: ReplayMode,
}

pub const CALL_1: &str = "BC_CALL_000001";
pub const CALL_2: &str = "BC_CALL_000002";

pub const TOOL_NAME: &str = "boundary_test";
pub const FINAL_TEXT: &str = "BOUNDARYCHECK_SCENARIO_COMPLETE";
pub const REPLAY_TEXT: &str = "BOUNDARYCHECK_REPLAY_REQUIRED";
pub const CHECKPOINT_TEXT: &str = "BOUNDARYCHECK_CHECKPOINT_AND_EXIT";

static SCENARIOS: &[ScenarioDef] = &[
    ScenarioDef {
        id: "exact-text",
        summary: "small UTF-8 text with JSON-escaped characters must arrive byte-identical",
        kind: ContentKind::Text,
        tier: Tier::Mvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "large-text-1k",
        summary: "1,024-byte text with five positional sentinels",
        kind: ContentKind::Text,
        tier: Tier::Mvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "large-text-50k",
        summary: "51,200-byte text with five positional sentinels",
        kind: ContentKind::Text,
        tier: Tier::Mvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "large-text-100k",
        summary: "102,400-byte text with five positional sentinels",
        kind: ContentKind::Text,
        tier: Tier::Mvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "concurrent-two-tools",
        summary: "two calls in one turn (Boston / Chicago); association by call ID, any order",
        kind: ContentKind::Text,
        tier: Tier::Mvp,
        turns: &[&[CALL_1, CALL_2]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "sequential-history",
        summary: "two calls in consecutive turns; history must keep each result exactly once",
        kind: ContentKind::Text,
        tier: Tier::PostMvp,
        turns: &[&[CALL_1], &[CALL_2]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "structured-json",
        summary: "nested JSON (big integers, decimals, unicode keys); compared semantically",
        kind: ContentKind::Json,
        tier: Tier::PostMvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "retry-429",
        summary: "provider answers the result request with HTTP 429 once; the retry must be unchanged",
        kind: ContentKind::Text,
        tier: Tier::PostMvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: true,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "mcp-error",
        summary: "MCP result with isError=true; error content must reach the provider unchanged",
        kind: ContentKind::Error,
        tier: Tier::PostMvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "unicode-boundaries",
        summary: "combining marks, NFC/NFD pairs, ZWJ emoji, RTL text; byte-exact",
        kind: ContentKind::Text,
        tier: Tier::PostMvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::None,
    },
    ScenarioDef {
        id: "replay-history",
        summary: "the completed tool-result history must remain unchanged when replayed",
        kind: ContentKind::Text,
        tier: Tier::PostMvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::SameProcess,
    },
    ScenarioDef {
        id: "persistence-resume",
        summary: "persisted tool-result history must survive a process restart unchanged",
        kind: ContentKind::Text,
        tier: Tier::PostMvp,
        turns: &[&[CALL_1]],
        rate_limit_first_result: false,
        replay: ReplayMode::Resume,
    },
];

pub fn all() -> &'static [ScenarioDef] {
    SCENARIOS
}

pub fn find(id: &str) -> Option<&'static ScenarioDef> {
    SCENARIOS.iter().find(|s| s.id == id)
}

impl ScenarioDef {
    pub fn required_capability(&self) -> Option<&'static str> {
        match self.replay {
            ReplayMode::None => None,
            ReplayMode::SameProcess => Some("history-replay"),
            ReplayMode::Resume => Some("persistence-resume"),
        }
    }

    pub fn call_ids(&self) -> Vec<&'static str> {
        self.turns.iter().flat_map(|t| t.iter().copied()).collect()
    }

    pub fn has_call(&self, call_id: &str) -> bool {
        self.call_ids().contains(&call_id)
    }

    pub fn prompt(&self, run_id: &str) -> String {
        format!(
            "boundarycheck run={run_id} scenario={}: call the {TOOL_NAME} tool exactly as the model instructs, then stop.",
            self.id
        )
    }
}

/// The exact tool arguments the fake provider sends for a call.
pub fn tool_arguments(run_id: &str, scenario_id: &str, call_id: &str) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    m.insert("run_id".into(), run_id.into());
    m.insert("scenario".into(), scenario_id.into());
    m.insert("call_id".into(), call_id.into());
    serde_json::Value::Object(m)
}

pub fn format_run_id(n: u32) -> String {
    format!("BC_RUN_{n:06}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_calls_deterministic() {
        let mut ids: Vec<_> = all().iter().map(|s| s.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all().len());
        assert_eq!(find("sequential-history").unwrap().call_ids(), vec![CALL_1, CALL_2]);
        assert_eq!(format_run_id(1), "BC_RUN_000001");
    }
}
