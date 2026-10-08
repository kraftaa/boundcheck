use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "boundarycheck",
    version,
    about = "Verify that an agent runtime transfers MCP tool results to the model provider unchanged"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Run scenarios against an agent runtime: boundarycheck run --adapter <name> -- <agent-command>
    Run(RunArgs),
    /// List the built-in deterministic scenarios.
    ListScenarios,
    /// List the built-in runtime adapters.
    ListAdapters,
    /// Fake MCP stdio server (launched by the runtime under test, not by users).
    #[command(hide = true)]
    McpServer {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        scenario: String,
        #[arg(long)]
        evidence: PathBuf,
    },
}

#[derive(Args)]
pub struct RunArgs {
    /// Runtime adapter: a built-in name, ./adapters/<name>.json, or a manifest path.
    #[arg(long)]
    pub adapter: String,
    /// Write the JSON report to this path.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Save evidence for failed/unknown scenarios under <path>/<run_id>/ (implies keeping the work directory).
    #[arg(long)]
    pub artifacts: Option<PathBuf>,
    /// With --artifacts, also save evidence for passing scenarios.
    #[arg(long, requires = "artifacts")]
    pub artifacts_all: bool,
    /// Per-scenario timeout in seconds.
    #[arg(long, default_value_t = 60.0)]
    pub timeout: f64,
    /// Run only this scenario (repeatable). Default: all scenarios.
    /// Accepts `large-text:<bytes>` (optionally `k`/`m`, 1k..8m) for any payload size.
    #[arg(long = "scenario", value_name = "NAME")]
    pub scenarios: Vec<String>,
    /// Also run the extended sizes: 65,535/65,536/65,537, 250 KiB, 1 MiB-1/1 MiB/1 MiB+1, 5 MiB.
    #[arg(long, conflicts_with = "scenarios")]
    pub extended: bool,
    /// Preserve the temporary work directory.
    #[arg(long)]
    pub keep_workdir: bool,
    /// Save provider request headers (credentials redacted) with the artifacts.
    #[arg(long, requires = "artifacts")]
    pub save_headers: bool,
    /// The agent command, executed directly without a shell.
    #[arg(last = true, required = true, num_args = 1..)]
    pub command: Vec<String>,
}
