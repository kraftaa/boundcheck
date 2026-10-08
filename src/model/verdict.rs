//! Verdicts, failure classes and explicit UNKNOWN reasons.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Verdict {
    Pass,
    Fail,
    Unknown,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Unknown => "UNKNOWN",
        }
    }
}

/// Failure classes. Every class must be backed by deterministic evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum FailureClass {
    // Ordered by reporting priority: the first class present is the headline.
    WrongToolCallAssociation,
    MissingResult,
    DuplicateResult,
    RetryMutation,
    InvalidUtf8,
    Truncation,
    StructuralMutation,
    InvalidJson,
    /// Reserved: OpenAI Chat Completions tool messages carry no error flag, so
    /// an error-status change is not observable with the V1 protocol.
    #[allow(dead_code)]
    ErrorStatusMutation,
    ReplayMutation,
    ContentMutation,
}

impl FailureClass {
    pub fn name(self) -> &'static str {
        match self {
            FailureClass::MissingResult => "MissingResult",
            FailureClass::DuplicateResult => "DuplicateResult",
            FailureClass::WrongToolCallAssociation => "WrongToolCallAssociation",
            FailureClass::ContentMutation => "ContentMutation",
            FailureClass::Truncation => "Truncation",
            FailureClass::InvalidUtf8 => "InvalidUtf8",
            FailureClass::InvalidJson => "InvalidJson",
            FailureClass::StructuralMutation => "StructuralMutation",
            FailureClass::RetryMutation => "RetryMutation",
            FailureClass::ReplayMutation => "ReplayMutation",
            FailureClass::ErrorStatusMutation => "ErrorStatusMutation",
        }
    }
}

/// Explicit reasons a scenario could not be decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownReason {
    UnsupportedRequestShape,
    AmbiguousCorrelation,
    RuntimeExitedEarly,
    StartupTimeout,
    ScenarioTimeout,
    ProviderRequestUnparseable,
    AdapterInsufficient,
    ExpectedTransitionMissing,
    UnexpectedTransition,
    ToolArgumentsChanged,
    EvidenceInconsistent,
    UnrepresentableContent,
}

impl UnknownReason {
    pub fn name(self) -> &'static str {
        match self {
            UnknownReason::UnsupportedRequestShape => "unsupported-request-shape",
            UnknownReason::AmbiguousCorrelation => "ambiguous-correlation",
            UnknownReason::RuntimeExitedEarly => "runtime-exited-early",
            UnknownReason::StartupTimeout => "startup-timeout",
            UnknownReason::ScenarioTimeout => "scenario-timeout",
            UnknownReason::ProviderRequestUnparseable => "provider-request-unparseable",
            UnknownReason::AdapterInsufficient => "adapter-insufficient",
            UnknownReason::ExpectedTransitionMissing => "expected-transition-missing",
            UnknownReason::UnexpectedTransition => "unexpected-transition",
            UnknownReason::ToolArgumentsChanged => "tool-arguments-changed",
            UnknownReason::EvidenceInconsistent => "evidence-inconsistent",
            UnknownReason::UnrepresentableContent => "unrepresentable-content",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnknownNote {
    pub reason: UnknownReason,
    pub detail: String,
}

impl UnknownNote {
    pub fn new(reason: UnknownReason, detail: impl Into<String>) -> Self {
        UnknownNote { reason, detail: detail.into() }
    }
}

/// Derive the scenario verdict: proven failures win, then unknowns, else pass.
pub fn decide(has_failures: bool, has_unknowns: bool) -> Verdict {
    if has_failures {
        Verdict::Fail
    } else if has_unknowns {
        Verdict::Unknown
    } else {
        Verdict::Pass
    }
}

/// Process exit codes (see README "Exit codes").
pub const EXIT_PASS: i32 = 0;
pub const EXIT_FAIL: i32 = 1;
pub const EXIT_HARNESS: i32 = 2;
pub const EXIT_UNKNOWN: i32 = 3;

pub fn exit_code(verdicts: &[Verdict], harness_error: bool) -> i32 {
    if harness_error {
        EXIT_HARNESS
    } else if verdicts.contains(&Verdict::Fail) {
        EXIT_FAIL
    } else if verdicts.contains(&Verdict::Unknown) {
        EXIT_UNKNOWN
    } else {
        EXIT_PASS
    }
}

pub fn exit_classification(code: i32) -> &'static str {
    match code {
        EXIT_PASS => "all-passed",
        EXIT_FAIL => "failures",
        EXIT_UNKNOWN => "unknowns",
        _ => "harness-error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_to_exit_code() {
        assert_eq!(exit_code(&[Verdict::Pass, Verdict::Pass], false), 0);
        assert_eq!(exit_code(&[Verdict::Pass, Verdict::Fail, Verdict::Unknown], false), 1);
        assert_eq!(exit_code(&[Verdict::Pass, Verdict::Unknown], false), 3);
        assert_eq!(exit_code(&[Verdict::Fail], true), 2);
        assert_eq!(exit_code(&[], false), 0);
        assert_eq!(exit_classification(3), "unknowns");
    }

    #[test]
    fn failures_take_precedence_over_unknowns() {
        assert_eq!(decide(true, true), Verdict::Fail);
        assert_eq!(decide(false, true), Verdict::Unknown);
        assert_eq!(decide(false, false), Verdict::Pass);
    }
}
