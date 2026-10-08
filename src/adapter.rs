//! Runtime adapter manifests: how to configure, start, drive and stop a
//! runtime. Nothing in here is visible to the comparison engine.

use crate::provider::protocol::Protocol;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const BUILTIN: &[(&str, &str)] = &[
    ("fixture-agent", include_str!("../adapters/fixture-agent.json")),
    ("openai-agents-python", include_str!("../adapters/openai-agents-python.json")),
    ("fixture-agent-responses", include_str!("../adapters/fixture-agent-responses.json")),
    ("openai-agents-python-responses", include_str!("../adapters/openai-agents-python-responses.json")),
    ("pydantic-ai-python", include_str!("../adapters/pydantic-ai-python.json")),
    ("pydantic-ai-python-responses", include_str!("../adapters/pydantic-ai-python-responses.json")),
    ("langgraph-python", include_str!("../adapters/langgraph-python.json")),
    ("langgraph-python-responses", include_str!("../adapters/langgraph-python-responses.json")),
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adapter {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub provider_protocol: String,
    /// Optional lifecycle features implemented by this runtime adapter.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Environment variables set for the runtime (values are templates).
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Which parent environment variables the runtime inherits.
    #[serde(default)]
    pub inherit_environment: InheritEnvironment,
    /// Extra parent variables to pass through in `minimal` mode (exact names).
    #[serde(default)]
    pub environment_passthrough: Vec<String>,
    /// Extra arguments appended to the agent command (templates).
    #[serde(default)]
    pub args: Vec<String>,
    /// Files written into the scenario work directory before launch (templates).
    #[serde(default)]
    pub files: Vec<AdapterFile>,
    /// Text written to the runtime's stdin (template); stdin is closed otherwise.
    #[serde(default)]
    pub stdin: Option<String>,
    /// Optional second launch used by persistence/resume scenarios.
    #[serde(default)]
    pub resume: Option<Resume>,
    #[serde(default)]
    pub readiness: Readiness,
    #[serde(default)]
    pub completion: Completion,
    #[serde(default)]
    pub shutdown: Shutdown,
    #[serde(default)]
    pub runtime_version: RuntimeVersion,
    #[serde(skip)]
    pub source: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InheritEnvironment {
    /// Only basic process variables (PATH, HOME, locale, TMPDIR, ...) plus `environment_passthrough`.
    #[default]
    Minimal,
    /// Everything except provider/credential-looking variables.
    All,
}

/// Parent variables inherited in `minimal` mode.
const MINIMAL_ENV: &[&str] =
    &["PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR", "TZ", "TERM"];

impl Adapter {
    /// The provider protocol this adapter's runtime speaks (validated in `parse`).
    pub fn protocol(&self) -> Protocol {
        Protocol::from_name(&self.provider_protocol).unwrap_or(Protocol::ChatCompletions)
    }

    /// The runtime's complete environment: inherited variables per policy,
    /// loopback proxy bypass, then the adapter's own (rendered) variables.
    pub fn child_environment(&self, rendered: &[(String, String)]) -> Vec<(String, String)> {
        let parent: Vec<(String, String)> =
            std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect();
        let mut env = inherited(&parent, self.inherit_environment, &self.environment_passthrough);
        let no_proxy = match parent.iter().find(|(k, _)| k == "NO_PROXY" || k == "no_proxy") {
            Some((_, v)) if !v.is_empty() => format!("{v},127.0.0.1,localhost"),
            _ => "127.0.0.1,localhost".to_owned(),
        };
        env.retain(|(k, _)| k != "NO_PROXY" && k != "no_proxy");
        env.push(("NO_PROXY".into(), no_proxy.clone()));
        env.push(("no_proxy".into(), no_proxy));
        env.retain(|(k, _)| !rendered.iter().any(|(r, _)| r == k));
        env.extend(rendered.iter().cloned());
        env
    }
}

fn inherited(parent: &[(String, String)], mode: InheritEnvironment, passthrough: &[String]) -> Vec<(String, String)> {
    parent
        .iter()
        .filter(|(k, _)| match mode {
            InheritEnvironment::Minimal => MINIMAL_ENV.contains(&k.as_str()) || passthrough.contains(k),
            InheritEnvironment::All => {
                // Never leak provider endpoints or credentials into the runtime under test.
                !(k.starts_with("OPENAI_")
                    || k.starts_with("BOUNDARYCHECK_")
                    || crate::provider::recorder::is_sensitive_header(k))
                    || passthrough.contains(k)
            }
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Resume {
    /// Environment overrides applied to the second runtime process.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Arguments appended only to the second runtime process.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional stdin for the second process; defaults to the adapter's stdin.
    #[serde(default)]
    pub stdin: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Readiness {
    /// The runtime is ready once its first provider request arrives.
    ProviderRequest {
        #[serde(default = "default_startup")]
        timeout_seconds: f64,
    },
}

impl Default for Readiness {
    fn default() -> Self {
        Readiness::ProviderRequest { timeout_seconds: default_startup() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Completion {
    /// Done when the provider state machine sent its final answer; the
    /// runtime then gets `exit_grace_seconds` to exit before it is terminated.
    ProviderScenarioComplete {
        #[serde(default = "default_exit_grace")]
        exit_grace_seconds: f64,
    },
    /// Done when the state machine completed *and* the runtime exited by itself.
    ProcessExit,
}

impl Default for Completion {
    fn default() -> Self {
        Completion::ProviderScenarioComplete { exit_grace_seconds: default_exit_grace() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shutdown {
    /// Time between SIGTERM and SIGKILL (sent to the whole process group).
    #[serde(default = "default_kill_grace")]
    pub grace_seconds: f64,
}

impl Default for Shutdown {
    fn default() -> Self {
        Shutdown { grace_seconds: default_kill_grace() }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RuntimeVersion {
    #[default]
    Unknown,
    Static {
        value: String,
    },
    /// Run argv (templates allowed) once; trimmed stdout is the version.
    Command {
        argv: Vec<String>,
    },
    /// The runtime writes `{"name": ..., "version": ...}` to `{{runtime_info_path}}`.
    ReportFile,
}

fn default_startup() -> f64 {
    30.0
}
fn default_exit_grace() -> f64 {
    5.0
}
fn default_kill_grace() -> f64 {
    3.0
}

pub fn builtin_names() -> Vec<&'static str> {
    BUILTIN.iter().map(|(n, _)| *n).collect()
}

/// Resolve `spec` as a manifest path, `./adapters/<spec>.json`, or a built-in name.
pub fn load(spec: &str) -> Result<Adapter, String> {
    let as_path = Path::new(spec);
    let local = Path::new("adapters").join(format!("{spec}.json"));
    let (text, source) = if as_path.is_file() {
        (std::fs::read_to_string(as_path).map_err(|e| format!("cannot read adapter {spec}: {e}"))?, spec.to_owned())
    } else if local.is_file() {
        (
            std::fs::read_to_string(&local).map_err(|e| format!("cannot read {}: {e}", local.display()))?,
            local.display().to_string(),
        )
    } else if let Some((_, text)) = BUILTIN.iter().find(|(n, _)| *n == spec) {
        ((*text).to_owned(), format!("builtin:{spec}"))
    } else {
        return Err(format!("unknown adapter `{spec}` (built-in adapters: {})", builtin_names().join(", ")));
    };
    parse(&text, &source)
}

pub fn parse(text: &str, source: &str) -> Result<Adapter, String> {
    let mut a: Adapter = serde_json::from_str(text).map_err(|e| format!("invalid adapter manifest {source}: {e}"))?;
    a.source = source.to_owned();
    if Protocol::from_name(&a.provider_protocol).is_none() {
        let known: Vec<&str> = Protocol::ALL.iter().map(|p| p.name()).collect();
        return Err(format!(
            "adapter {} uses provider protocol `{}`; boundarycheck supports only {}",
            a.name,
            a.provider_protocol,
            known.join(", ")
        ));
    }
    for capability in &a.capabilities {
        if !["history-replay", "persistence-resume"].contains(&capability.as_str()) {
            return Err(format!("adapter {} declares unknown capability `{capability}`", a.name));
        }
    }
    if a.capabilities.iter().any(|c| c == "persistence-resume") && a.resume.is_none() {
        return Err(format!("adapter {} declares `persistence-resume` but has no `resume` configuration", a.name));
    }
    // Dry-run every template so configuration errors surface before any process starts.
    let vars = TemplateVars::placeholder();
    let mut templates: Vec<&String> = a.environment.values().chain(a.args.iter()).collect();
    templates.extend(a.files.iter().map(|f| &f.content));
    templates.extend(a.files.iter().map(|f| &f.path));
    templates.extend(a.stdin.iter());
    if let Some(resume) = &a.resume {
        templates.extend(resume.environment.values());
        templates.extend(resume.args.iter());
        templates.extend(resume.stdin.iter());
    }
    if let RuntimeVersion::Command { argv } = &a.runtime_version {
        if argv.is_empty() {
            return Err(format!("adapter {}: runtime_version.argv is empty", a.name));
        }
        templates.extend(argv.iter());
    }
    for t in templates {
        vars.render(t).map_err(|e| format!("adapter {}: {e}", a.name))?;
    }
    for f in &a.files {
        safe_relative_path(&vars.render(&f.path)?).map_err(|e| format!("adapter {}: {e}", a.name))?;
    }
    let Readiness::ProviderRequest { timeout_seconds } = a.readiness;
    let mut durations =
        vec![("readiness.timeout_seconds", timeout_seconds), ("shutdown.grace_seconds", a.shutdown.grace_seconds)];
    if let Completion::ProviderScenarioComplete { exit_grace_seconds } = a.completion {
        durations.push(("completion.exit_grace_seconds", exit_grace_seconds));
    }
    for (name, v) in durations {
        validate_seconds(v).map_err(|e| format!("adapter {}: {name} {e}", a.name))?;
    }
    Ok(a)
}

/// Durations must be finite, non-negative and at most one day.
pub fn validate_seconds(v: f64) -> Result<std::time::Duration, String> {
    if !(0.0..=86_400.0).contains(&v) {
        return Err(format!("must be between 0 and 86400 seconds (got {v})"));
    }
    std::time::Duration::try_from_secs_f64(v).map_err(|e| e.to_string())
}

/// Adapter files are written inside the scenario work directory only. Checked
/// after rendering, so template values cannot introduce `..` or absolute paths.
pub fn safe_relative_path(path: &str) -> Result<&Path, String> {
    let p = Path::new(path);
    let ok = !path.is_empty() && p.components().all(|c| matches!(c, std::path::Component::Normal(_)));
    if ok {
        Ok(p)
    } else {
        Err(format!("file path `{path}` must be relative and stay inside the work directory"))
    }
}

/// Template variables: `{{name}}` inserts the raw value, `{{name_json}}`
/// inserts it as a JSON string literal. `{{mcp_args_json}}` is a JSON array.
pub struct TemplateVars {
    pub vars: BTreeMap<&'static str, String>,
}

impl TemplateVars {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_base_url: &str,
        mcp_command: &str,
        mcp_args: &[String],
        prompt: &str,
        scenario_id: &str,
        run_id: &str,
        workdir: &str,
        runtime_info_path: &str,
    ) -> Self {
        let mut vars = BTreeMap::new();
        vars.insert("provider_base_url", provider_base_url.to_owned());
        vars.insert("mcp_command", mcp_command.to_owned());
        vars.insert("mcp_args_json", serde_json::to_string(mcp_args).unwrap());
        vars.insert("scenario_prompt", prompt.to_owned());
        vars.insert("scenario_id", scenario_id.to_owned());
        vars.insert("run_id", run_id.to_owned());
        vars.insert("workdir", workdir.to_owned());
        vars.insert("runtime_info_path", runtime_info_path.to_owned());
        TemplateVars { vars }
    }

    fn placeholder() -> Self {
        Self::new("http://127.0.0.1:1/x/v1", "mcp", &["a".into()], "p", "s", "r", "/w", "/w/i")
    }

    pub fn render(&self, template: &str) -> Result<String, String> {
        let mut out = String::new();
        let mut rest = template;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let end = after.find("}}").ok_or_else(|| format!("unterminated template in `{template}`"))?;
            let name = after[..end].trim();
            let value = if let Some(v) = self.vars.get(name) {
                v.clone()
            } else if let Some(v) = name.strip_suffix("_json").and_then(|n| self.vars.get(n)) {
                serde_json::to_string(v).unwrap()
            } else {
                return Err(format!(
                    "unknown template variable `{{{{{name}}}}}` (known: {})",
                    self.vars.keys().copied().collect::<Vec<_>>().join(", ")
                ));
            };
            out.push_str(&value);
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::protocol::PROTOCOL;

    #[test]
    fn builtins_parse() {
        for name in builtin_names() {
            load(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn templates() {
        let v = TemplateVars::new(
            "http://h/v1",
            "/bin/bc",
            &["mcp-server".into(), "--x".into()],
            "say \"hi\"",
            "s",
            "r",
            "/w",
            "/w/i",
        );
        assert_eq!(v.render("{{provider_base_url}}").unwrap(), "http://h/v1");
        assert_eq!(v.render("{{mcp_args_json}}").unwrap(), r#"["mcp-server","--x"]"#);
        assert_eq!(v.render("{\"p\": {{scenario_prompt_json}}}").unwrap(), r#"{"p": "say \"hi\""}"#);
        assert!(v.render("{{nope}}").is_err());
        assert!(v.render("{{open").is_err());
    }

    #[test]
    fn environment_policy() {
        let parent: Vec<(String, String)> = [
            ("PATH", "/bin"),
            ("HOME", "/h"),
            ("AWS_SECRET_ACCESS_KEY", "x"),
            ("GITHUB_TOKEN", "x"),
            ("OPENAI_BASE_URL", "https://real"),
            ("VIRTUAL_ENV", "/v"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let names = |m, pass: &[String]| inherited(&parent, m, pass).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        assert_eq!(names(InheritEnvironment::Minimal, &[]), vec!["PATH", "HOME"]);
        assert_eq!(names(InheritEnvironment::Minimal, &["VIRTUAL_ENV".into()]), vec!["PATH", "HOME", "VIRTUAL_ENV"]);
        assert_eq!(names(InheritEnvironment::All, &[]), vec!["PATH", "HOME", "VIRTUAL_ENV"]);
    }

    #[test]
    fn paths_and_durations() {
        assert!(safe_relative_path("conf/mcp.json").is_ok());
        for bad in ["/etc/x", "../x", "a/../../x", "", "./x"] {
            assert!(safe_relative_path(bad).is_err(), "{bad}");
        }
        assert!(validate_seconds(f64::INFINITY).is_err());
        assert!(validate_seconds(f64::NAN).is_err());
        assert!(validate_seconds(-1.0).is_err());
        assert!(validate_seconds(2.5).is_ok());
        let m = r#"{"name":"x","provider_protocol":"openai-chat-completions","shutdown":{"grace_seconds":1e999}}"#;
        assert!(parse(m, "t").is_err());
    }

    #[test]
    fn rejects_other_protocols_and_bad_templates() {
        let m = |proto: &str, env: &str| {
            format!(r#"{{"name":"x","provider_protocol":"{proto}","environment":{{"A":"{env}"}}}}"#)
        };
        assert!(parse(&m("unsupported-messages", "x"), "t").unwrap_err().contains("supports only"));
        assert!(parse(&m("openai-responses", "x"), "t").is_ok());
        assert!(parse(&m(PROTOCOL, "{{bogus}}"), "t").is_err());
        assert!(parse(&m(PROTOCOL, "{{provider_base_url}}"), "t").is_ok());
    }
}
