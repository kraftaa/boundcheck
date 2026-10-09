//! `--isolation docker`: re-run boundarycheck itself inside a locked-down
//! container. The fake provider, the MCP server and the runtime under test all
//! run inside, talking over the container's own loopback, so the container
//! needs no network at all.
//!
//! This protects the host (home directory, credentials, network). It does not
//! make the test tamper-proof: the runtime still runs next to the harness.

use crate::cli::RunArgs;
use crate::model::verdict::EXIT_HARNESS;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where things live inside the container.
const BIN: &str = "/opt/boundarycheck/boundarycheck";
const WORK: &str = "/work";
const OUT_REPORT: &str = "/out/report";
const OUT_ARTIFACTS: &str = "/out/artifacts";

pub struct DockerPlan {
    /// The full container-engine command line.
    pub argv: Vec<String>,
    pub name: String,
    pub image: String,
    /// Host locations of the report and artifacts, for the final message.
    pub report: Option<PathBuf>,
    pub artifacts: Option<PathBuf>,
}

/// Build the container command line for `args`. Pure (no side effects other
/// than creating the report/artifacts directories), so it can be unit-tested.
pub fn plan(args: &RunArgs, engine: &str, image: &str, binary: &Path, cwd: &Path) -> Result<DockerPlan, String> {
    if args.keep_workdir {
        return Err("--keep-workdir is not available with --isolation docker (the work directory is in the container's /tmp); use --artifacts".into());
    }
    let binary = binary.canonicalize().map_err(|e| format!("--isolation-binary {}: {e}", binary.display()))?;
    if !is_linux_executable(&binary) {
        return Err(format!(
            "--isolation-binary {} is not a Linux (ELF) executable; build one with scripts/build-linux-binary.sh",
            binary.display()
        ));
    }
    let name = format!("boundarycheck-{}", std::process::id());
    let mut argv: Vec<String> = [
        engine,
        "run",
        "--rm",
        "--init",
        "--name",
        &name,
        "--network",
        "none",
        "--read-only",
        "--tmpfs",
        "/tmp:rw,exec,size=2g",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "512",
        "-e",
        "HOME=/tmp",
        "-e",
        "PYTHONDONTWRITEBYTECODE=1",
        "-w",
        WORK,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    argv.extend(["-e".into(), format!("BOUNDARYCHECK_ISOLATION=docker image={image} network=none")]);
    #[cfg(unix)]
    {
        // Files written to the mounted output directories belong to the caller.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        argv.extend(["--user".into(), format!("{uid}:{gid}")]);
    }
    let mount = |argv: &mut Vec<String>, host: &Path, inside: &str, ro: bool| {
        argv.push("-v".into());
        argv.push(format!("{}:{inside}{}", host.display(), if ro { ":ro" } else { "" }));
    };
    mount(&mut argv, &binary, BIN, true);
    mount(&mut argv, cwd, WORK, true);

    let mut inner: Vec<String> = vec!["run".into(), "--adapter".into(), container_adapter(&args.adapter, cwd)?];
    for s in &args.scenarios {
        inner.extend(["--scenario".into(), s.clone()]);
    }
    inner.extend(["--timeout".into(), args.timeout.to_string()]);
    for (on, flag) in [
        (args.extended, "--extended"),
        (args.probes, "--probes"),
        (args.artifacts_all, "--artifacts-all"),
        (args.save_headers, "--save-headers"),
        (args.include_runtime_logs, "--include-runtime-logs"),
    ] {
        if on {
            inner.push(flag.into());
        }
    }
    if let Some(report) = &args.report {
        let report = absolute(cwd, report);
        let dir = report.parent().ok_or("--report has no parent directory")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let file = report.file_name().ok_or("--report has no file name")?.to_string_lossy().into_owned();
        mount(&mut argv, &dir.canonicalize().map_err(|e| e.to_string())?, OUT_REPORT, false);
        inner.extend(["--report".into(), format!("{OUT_REPORT}/{file}")]);
    }
    if let Some(dir) = &args.artifacts {
        let dir = absolute(cwd, dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        mount(&mut argv, &dir.canonicalize().map_err(|e| e.to_string())?, OUT_ARTIFACTS, false);
        inner.extend(["--artifacts".into(), OUT_ARTIFACTS.into()]);
    }
    inner.push("--".into());
    inner.extend(args.command.iter().cloned());
    argv.push(image.into());
    argv.push(BIN.into());
    argv.extend(inner);
    Ok(DockerPlan {
        argv,
        name,
        image: image.to_owned(),
        report: args.report.as_ref().map(|r| absolute(cwd, r)),
        artifacts: args.artifacts.as_ref().map(|a| absolute(cwd, a)),
    })
}

/// A manifest path must be inside the mounted working directory; built-in
/// names and `./adapters/<name>.json` resolve the same way inside.
fn container_adapter(spec: &str, cwd: &Path) -> Result<String, String> {
    let p = Path::new(spec);
    if !p.is_file() {
        return Ok(spec.to_owned());
    }
    let abs = absolute(cwd, p).canonicalize().map_err(|e| format!("adapter {spec}: {e}"))?;
    let cwd = cwd.canonicalize().map_err(|e| e.to_string())?;
    match abs.strip_prefix(&cwd) {
        Ok(rel) => Ok(format!("{WORK}/{}", rel.display())),
        Err(_) => Err(format!(
            "adapter manifest {spec} must be inside the current directory with --isolation docker (only it is mounted)"
        )),
    }
}

fn absolute(cwd: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

fn is_linux_executable(p: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == *b"\x7fELF"
}

/// Run the plan; the container's exit code is boundarycheck's exit code.
pub fn run(plan: DockerPlan) -> i32 {
    let engine = plan.argv[0].clone();
    let name = plan.name.clone();
    let _ = ctrlc::set_handler(move || {
        let _ = Command::new(&engine).args(["kill", &name]).output();
        eprintln!("boundarycheck: interrupted; container stopped");
        std::process::exit(EXIT_HARNESS);
    });
    eprintln!("boundarycheck: running inside container image {} (network: none, read-only root)", plan.image);
    match Command::new(&plan.argv[0]).args(&plan.argv[1..]).status() {
        Ok(s) => match s.code() {
            Some(code @ 0..=3) => {
                if let Some(r) = &plan.report {
                    println!("report: {}", r.display());
                }
                if let Some(a) = &plan.artifacts {
                    println!("artifacts: {}", a.display());
                }
                code
            }
            Some(code) => {
                eprintln!("boundarycheck: container engine failed (exit {code}); is the image available and the engine running?");
                EXIT_HARNESS
            }
            None => EXIT_HARNESS,
        },
        Err(e) => {
            eprintln!("boundarycheck: cannot run `{}`: {e}", plan.argv[0]);
            EXIT_HARNESS
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn args(extra: &[&str]) -> RunArgs {
        let mut v = vec!["boundarycheck", "run"];
        v.extend_from_slice(extra);
        match crate::cli::Cli::parse_from(v).command {
            crate::cli::Cmd::Run(a) => a,
            _ => unreachable!(),
        }
    }

    #[test]
    fn plan_locks_down_the_container_and_maps_paths() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bc-linux");
        std::fs::write(&bin, b"\x7fELF....").unwrap();
        std::fs::write(dir.path().join("my-adapter.json"), "{}").unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let a = args(&[
            "--adapter",
            cwd.join("my-adapter.json").to_str().unwrap(),
            "--report",
            "out/r.json",
            "--artifacts",
            "ev",
            "--scenario",
            "exact-text",
            "--",
            "python3",
            "agent.py",
        ]);
        let p = plan(&a, "docker", "python:3.13-slim", &bin, &cwd).unwrap();
        let joined = p.argv.join(" ");
        for must in ["--network none", "--read-only", "--cap-drop ALL", "no-new-privileges", "--rm", "--init"] {
            assert!(joined.contains(must), "{must}: {joined}");
        }
        assert!(joined.contains(&format!("{}:/work:ro", cwd.display())));
        assert!(joined.contains("--adapter /work/my-adapter.json"));
        assert!(joined.contains("--report /out/report/r.json"));
        assert!(joined.contains("--artifacts /out/artifacts"));
        assert!(joined.ends_with("-- python3 agent.py"));
        assert!(!joined.contains("HOME=/Users"), "the host home is not passed in");
        assert!(cwd.join("out").is_dir() && cwd.join("ev").is_dir());
    }

    #[test]
    fn plan_rejects_non_linux_binaries_and_outside_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let mac = cwd.join("bc-mac");
        std::fs::write(&mac, b"\xcf\xfa\xed\xfe").unwrap();
        let err =
            plan(&args(&["--adapter", "fixture-agent", "--", "true"]), "docker", "img", &mac, &cwd).err().unwrap();
        assert!(err.contains("not a Linux"), "{err}");
        let bin = cwd.join("bc");
        std::fs::write(&bin, b"\x7fELF").unwrap();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().join("a.json");
        std::fs::write(&outside, "{}").unwrap();
        let err = plan(&args(&["--adapter", outside.to_str().unwrap(), "--", "true"]), "docker", "img", &bin, &cwd)
            .err()
            .unwrap();
        assert!(err.contains("inside the current directory"), "{err}");
        let err =
            plan(&args(&["--adapter", "fixture-agent", "--keep-workdir", "--", "true"]), "docker", "img", &bin, &cwd)
                .err()
                .unwrap();
        assert!(err.contains("--keep-workdir"));
    }
}
