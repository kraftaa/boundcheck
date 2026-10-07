mod adapter;
mod cli;
mod compare;
mod mcp;
mod model;
mod process;
mod provider;
mod report;
mod runner;
mod scenario;

use clap::Parser;
use cli::{Cli, Cmd, RunArgs};
use model::verdict::EXIT_HARNESS;

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Cmd::McpServer { run_id, scenario, evidence } => mcp::server::serve(run_id, scenario, evidence),
        Cmd::ListScenarios => {
            for s in scenario::all() {
                let tier = match s.tier {
                    scenario::Tier::Mvp => "mvp",
                    scenario::Tier::PostMvp => "post-mvp",
                };
                println!("{:<22} {:<9} {}", s.id, tier, s.summary);
            }
            0
        }
        Cmd::ListAdapters => {
            for name in adapter::builtin_names() {
                let a = adapter::load(name);
                println!("{:<22} {}", name, a.map(|a| a.description).unwrap_or_default());
            }
            0
        }
        Cmd::Run(args) => run(args),
    };
    std::process::exit(code);
}

fn run(args: RunArgs) -> i32 {
    let adapter = match adapter::load(&args.adapter) {
        Ok(a) => a,
        Err(e) => return config_error(&e),
    };
    let supports = |s: &&scenario::ScenarioDef| match s.required_capability() {
        None => true,
        Some(c) => adapter.capabilities.iter().any(|a| a == c),
    };
    let scenarios = if args.scenarios.is_empty() {
        scenario::all().iter().filter(supports).collect()
    } else {
        let mut v = vec![];
        for name in &args.scenarios {
            match scenario::find(name) {
                Some(s) if s.required_capability().is_some_and(|c| !adapter.capabilities.iter().any(|a| a == c)) => {
                    return config_error(&format!(
                        "scenario `{name}` requires adapter capability `{}`",
                        s.required_capability().unwrap()
                    ));
                }
                Some(s) if !v.iter().any(|x: &&scenario::ScenarioDef| x.id == s.id) => v.push(s),
                Some(_) => {}
                None => {
                    let known: Vec<&str> = scenario::all().iter().map(|s| s.id).collect();
                    return config_error(&format!("unknown scenario `{name}` (known: {})", known.join(", ")));
                }
            }
        }
        v
    };
    let timeout = match adapter::validate_seconds(args.timeout) {
        Ok(t) if !t.is_zero() => t,
        Ok(_) => return config_error("--timeout must be positive"),
        Err(e) => return config_error(&format!("--timeout {e}")),
    };
    process::install_signal_handler();

    println!("boundarycheck {}", env!("CARGO_PKG_VERSION"));
    println!("adapter {} ({}), protocol {}\n", adapter.name, adapter.source, provider::protocol::PROTOCOL);
    let report = runner::run(runner::RunConfig {
        adapter,
        command: args.command,
        scenarios,
        timeout,
        artifacts: args.artifacts,
        artifacts_all: args.artifacts_all,
        keep_workdir: args.keep_workdir,
        save_headers: args.save_headers,
        on_scenario: Box::new(|s| println!("{}", report::render_scenario(s))),
    });
    let rt = &report.runtime;
    println!(
        "runtime: {} {} (version source: {})",
        rt.name.as_deref().unwrap_or("(name unknown)"),
        rt.version.as_deref().unwrap_or("(version unknown)"),
        rt.version_source
    );
    if let Some(e) = &report.harness_error {
        eprintln!("boundarycheck: harness error: {e}");
    }
    if let Some(dir) = &report.workdir {
        println!("work directory kept: {dir}");
    }
    if let Some(dir) = &report.artifacts {
        println!("artifacts: {dir}");
    }
    let s = &report.summary;
    println!("Summary: {} passed, {} failed, {} unknown", s.passed, s.failed, s.unknown);
    if let Some(path) = &args.report {
        let json = serde_json::to_vec_pretty(&report).expect("serialize report");
        if let Err(e) = write_report(path, &json) {
            eprintln!("boundarycheck: cannot write report {}: {e}", path.display());
            return EXIT_HARNESS;
        }
    }
    report.exit.code
}

/// Reports are created owner-only (0600), like the evidence artifacts.
fn write_report(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(bytes)
}

fn config_error(msg: &str) -> i32 {
    eprintln!("boundarycheck: configuration error: {msg}");
    EXIT_HARNESS
}
