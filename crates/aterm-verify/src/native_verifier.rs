// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Compile a checkout's verifier as a leaf compiler call, then run that exact image with the
//! caller's original Cargo selectors. Compiler placement is never runtime provenance.

use crate::{EnvSnapshot, NativeBuildPlan};
use aterm_uds::spawnfd::{Command, Tether as _};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn scrub_cargo_run(command: &mut Command) {
    let names: BTreeSet<_> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .filter(|name| {
            name.to_str()
                .is_some_and(aterm_toolchain::build_cache::said_by_cargo_run)
        })
        .collect();
    for name in names {
        command.env_remove(name);
    }
}

fn owned_command(program: &Path, root: &Path) -> Result<Command, String> {
    #[cfg(target_vendor = "apple")]
    if aterm_uds::tether::lost() {
        return Err(
            "verifier bootstrap refused: its parent-death helper image is unavailable".into(),
        );
    }
    let mut command = aterm_uds::spawnfd::command(program);
    command.tether();
    #[cfg(unix)]
    command.process_group(0);
    command.current_dir(root);
    scrub_cargo_run(&mut command);
    Ok(command)
}

/// Prepare and configure a build-only native verifier bootstrap. Callers may apply their tool
/// policy before executing it, then pass successful stdout to [`built`].
///
/// # Errors
/// The selected ownership cannot be prepared, the captured source/layout differs, or the
/// recorded parent-death helper image is unavailable.
pub fn build_command(root: &Path, targo: &Path, plan: &NativeBuildPlan) -> Result<Command, String> {
    if crate::physical(root) != plan.root {
        return Err("verifier bootstrap source differs from its captured build plan".into());
    }
    plan.prepare()
        .map_err(|error| format!("prepare verifier build directory: {error}"))?;
    let mut args: Vec<String> = [
        "--unverified",
        "build",
        "--profile",
        "test",
        "-p",
        "aterm-verify",
        "--bin",
        "aterm-verify",
        "--message-format=json",
        "--config",
        "profile.dev.incremental=false",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some(config) = plan.build_dir() {
        args.splice(0..0, ["--config".to_owned(), config.to_owned()]);
    }
    let args = aterm_toolchain::build_cache::with_test_graph(&args);
    let mut command = owned_command(targo, root)?;
    command
        .args(aterm_toolchain::build_cache::configs_after_subcommand(
            &args,
        ))
        .arg("--target-dir")
        .arg(plan.target())
        .env("CARGO_INCREMENTAL", "0");
    plan.apply_to_command(&mut command)
        .map_err(|error| format!("configure verifier compiler layout: {error}"))?;
    Ok(command)
}

/// Build the checkout's verifier without running it and return Cargo's exact reported image.
/// The build stops once `stopped` reads true (read at least every
/// [`aterm_uds::deadline::STOP_TICK`]): its process group is asked to end (`SIGTERM`), it is
/// not awaited, and nothing it wrote is read.
///
/// # Errors
/// Preparation, launch, compilation or unambiguous executable identification fails, or the
/// build was stopped.
pub fn build(
    root: &Path,
    targo: &Path,
    plan: &NativeBuildPlan,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<PathBuf, String> {
    let mut command = build_command(root, targo, plan)?;
    let started = |error: std::io::Error| {
        format!(
            "{} verifier bootstrap did not start: {error}",
            targo.display()
        )
    };
    let output = output_unless(&mut command, stopped)
        .map_err(started)?
        .ok_or("the landing was interrupted while it built the checkout's verifier")?;
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        eprint!("{}", String::from_utf8_lossy(&output.stdout));
        return Err(format!("verifier bootstrap exited {}", output.status));
    }
    built(root, &output.stdout)
}

/// Run `command`, which leads a process group of its own, to its end, its stdout and stderr
/// captured whole; `None` where `stopped` read true first (read at least every
/// [`aterm_uds::deadline::STOP_TICK`]): the group was then asked to end (`SIGTERM`) and nothing
/// was awaited.
///
/// # Errors
/// It did not start, or could not be waited on.
pub(crate) fn output_unless(
    command: &mut Command,
    stopped: &mut dyn FnMut() -> bool,
) -> std::io::Result<Option<std::process::Output>> {
    use std::io::Read as _;
    command
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::piped());
    let mut child = command.spawn()?;
    let drain = |stream: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|out| Box::new(out) as Box<dyn std::io::Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|err| Box::new(err) as Box<dyn std::io::Read + Send>),
    );
    let mut exit = aterm_uds::deadline::ExitWait::new(&child);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if stopped() {
            #[cfg(unix)]
            if child.owns_unreaped()
                && let Ok(leader) = i32::try_from(child.id())
                && leader > 1
            {
                // SAFETY: killpg takes two integers and touches no memory of this process. The
                // group's leader is this process's unreaped child, so its id still names that
                // group alone.
                unsafe {
                    libc::killpg(leader, libc::SIGTERM);
                }
            }
            return Ok(None);
        }
        exit.wait(aterm_uds::deadline::STOP_TICK);
    };
    let joined = |reader: std::thread::JoinHandle<Vec<u8>>| reader.join().unwrap_or_default();
    Ok(Some(std::process::Output {
        status,
        stdout: joined(stdout),
        stderr: joined(stderr),
    }))
}

/// Read the exact verifier executable from a successful bootstrap's Cargo JSON stdout.
///
/// # Errors
/// Artifact fields are ambiguous, no unique binary from this checkout is reported, or that
/// reported absolute path does not name an executable regular file.
pub fn built(root: &Path, stdout: &[u8]) -> Result<PathBuf, String> {
    let manifest = crate::physical(&root.join("crates/aterm-verify/Cargo.toml"));
    let mut executables = BTreeSet::new();
    for line in stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let row = match aterm_json::from_slice::<crate::compiler_message::UnitMessage>(line) {
            Ok(crate::compiler_message::UnitMessage::Unit(row)) => row,
            Ok(crate::compiler_message::UnitMessage::Ambiguous {
                artifact_reason_seen: true,
                ..
            }) => {
                return Err(
                    "verifier bootstrap reported ambiguous compiler artifact fields".into(),
                );
            }
            _ => continue,
        };
        if row["reason"].as_str() != Some("compiler-artifact")
            || row["target"]["name"].as_str() != Some("aterm-verify")
            || !crate::compiler_message::artifact_target_kinds(&row)
                .is_ok_and(|kinds| kinds == ["bin"])
            || row["profile"]["test"].as_bool() != Some(false)
        {
            continue;
        }
        let Some(reported_manifest) = row["manifest_path"]
            .as_str()
            .map(Path::new)
            .filter(|path| path.is_absolute())
        else {
            continue;
        };
        if crate::physical(reported_manifest) != manifest {
            continue;
        }
        let executable = row["executable"]
            .as_str()
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or("verifier bootstrap artifact has no absolute executable")?;
        executables.insert(executable);
    }
    if executables.len() != 1 {
        return Err(
            "verifier bootstrap did not identify exactly one verifier from this checkout".into(),
        );
    }
    let executable = executables.pop_first().expect("one reported verifier");
    let metadata = std::fs::metadata(&executable)
        .map_err(|error| format!("read reported verifier {}: {error}", executable.display()))?;
    #[cfg(unix)]
    let runnable = {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let runnable = true;
    if !metadata.is_file() || !runnable {
        return Err(format!(
            "reported verifier is not an executable regular file: {}",
            executable.display()
        ));
    }
    Ok(executable)
}

/// Configure an owned direct run of a freshly built verifier. The runtime receives the original
/// selector choices, including removals, and its own manifest rather than the bootstrapping image's.
///
/// # Errors
/// The recorded parent-death helper image is unavailable.
pub fn command(executable: &Path, root: &Path, env: &EnvSnapshot) -> Result<Command, String> {
    let mut command = owned_command(executable, root)?;
    configure_runtime(&mut command, root, env);
    Ok(command)
}

/// Restore runtime selector provenance and the current manifest after a caller has applied its
/// publication/tool policy. Unrelated package and tool variables remain inherited.
pub fn configure_runtime(command: &mut Command, root: &Path, env: &EnvSnapshot) {
    scrub_cargo_run(command);
    env.apply_cargo_selectors(command);
    command.env("CARGO_MANIFEST_DIR", root.join("crates/aterm-verify"));
}
