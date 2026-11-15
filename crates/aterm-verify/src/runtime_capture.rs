// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The disposable macOS runtime's App-band producer driver. Only pipe EOF seals output;
//! the direct producer's exit alone never publishes a completed phase.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus};
use std::time::{Duration, Instant};

use aterm_uds::deadline::{
    ChildOutput, MAX_COMPLETE_STDOUT_BYTES, stream_until_stopped_observing_exit,
};
use aterm_uds::spawnfd::{Command, command};

const FLAG: &str = "--runtime-capture";

struct Phase {
    directory: PathBuf,
    deadline: Instant,
}

enum Invocation {
    Run(Phase, OsString),
    Wait(Phase, String),
}

impl Invocation {
    fn parse(args: &[OsString]) -> Result<Self, &'static str> {
        let [mode, directory, seconds, argument] = args else {
            return Err(
                "expected run/wait, an absolute phase directory, positive seconds and its script/launchd target",
            );
        };
        let directory = PathBuf::from(directory);
        let seconds = seconds
            .to_str()
            .and_then(|text| text.parse::<u64>().ok())
            .filter(|seconds| *seconds > 0)
            .ok_or("capture seconds must be positive")?;
        if !directory.is_absolute() || !directory.is_dir() {
            return Err("capture directory must be an existing absolute directory");
        }
        let limit = Duration::from_secs(seconds);
        let deadline = Instant::now()
            .checked_add(limit)
            .ok_or("capture deadline is out of range")?;
        let phase = Phase {
            directory,
            deadline,
        };
        match mode.to_str() {
            Some("run") => Ok(Self::Run(phase, argument.clone())),
            Some("wait") => argument
                .to_str()
                .filter(|target| {
                    target.starts_with("gui/") && target.contains("/systems.alab.aterm.runtime.")
                })
                .map(|target| Self::Wait(phase, target.to_owned()))
                .ok_or("expected the runtime's GUI launchd target"),
            _ => Err("expected runtime capture run or wait"),
        }
    }
}

#[derive(Debug, serde::Serialize)]
struct Receipt {
    schema: &'static str,
    complete: bool,
    exit: Option<i32>,
    signal: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    error: Option<String>,
}

impl Receipt {
    fn driver_exit(&self) -> u8 {
        if !self.complete {
            return crate::exit::COULD_NOT_RUN as u8;
        }
        self.exit
            .or_else(|| self.signal.map(|signal| 128 + signal))
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(crate::exit::COULD_NOT_RUN as u8)
    }
}

fn fresh(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

fn publish(directory: &Path, receipt: &Receipt) -> io::Result<()> {
    let mut bytes = aterm_json::to_string(receipt)
        .map_err(io::Error::other)?
        .into_bytes();
    bytes.push(b'\n');
    let temporary = directory.join("capture.json.tmp");
    fresh(&temporary)?.write_all(&bytes)?;
    // The same-directory link publishes complete bytes atomically and refuses an existing
    // completion name. A phase can never replace evidence another invocation already left.
    std::fs::hard_link(&temporary, directory.join("capture.json"))?;
    std::fs::remove_file(temporary)
}

fn capture(
    command: &mut Command,
    directory: &Path,
    deadline: Instant,
    maximum: usize,
    stopped: impl FnMut() -> bool,
    mut observe_exit: impl FnMut(ExitStatus),
) -> io::Result<Receipt> {
    for name in [
        "capture.json",
        "capture.json.tmp",
        "started",
        "stdout",
        "stderr",
    ] {
        match std::fs::symlink_metadata(directory.join(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("capture artifact {name} already exists"),
                ));
            }
        }
    }
    let mut stdout = fresh(&directory.join("stdout"))?;
    let mut stderr = fresh(&directory.join("stderr"))?;
    fresh(&directory.join("started"))?.write_all(b"started\n")?;
    let mut counts = [0_usize; 2];
    let mut observed_exit = None;
    let result = stream_until_stopped_observing_exit(
        command,
        Some(deadline),
        |_| {},
        |stream, bytes| {
            let (file, index, name) = match stream {
                ChildOutput::Stdout => (&mut stdout, 0, "stdout"),
                ChildOutput::Stderr => (&mut stderr, 1, "stderr"),
            };
            if bytes.len() > maximum.saturating_sub(counts[index]) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} exceeds its complete-output bound of {maximum} bytes"),
                ));
            }
            file.write_all(bytes)?;
            counts[index] += bytes.len();
            Ok(())
        },
        stopped,
        |status| {
            observed_exit = Some(status);
            observe_exit(status);
        },
    );
    // The producer inherited only pipes. These files have this one writer; closing it before
    // publishing makes both complete output and an incomplete bounded prefix immutable.
    drop(stdout);
    drop(stderr);
    let (status, error) = match result {
        Ok(Some(status)) => (Some(status), None),
        Ok(None) => (
            observed_exit,
            Some("producer capture stopped before output EOF".into()),
        ),
        Err(error) => (observed_exit, Some(format!("producer capture: {error}"))),
    };
    let receipt = Receipt {
        schema: "aterm-runtime-capture-v1",
        complete: error.is_none() && status.is_some(),
        exit: status.and_then(|status| status.code()),
        signal: status.and_then(|status| status.signal()),
        stdout_bytes: counts[0],
        stderr_bytes: counts[1],
        error,
    };
    publish(directory, &receipt)?;
    Ok(receipt)
}

struct DriverState {
    ended: bool,
    description: Vec<u8>,
}

fn driver_ended(target: &str, text: &str) -> bool {
    if text.lines().next().map(str::trim) != Some(format!("{target} = {{").as_str()) {
        return false;
    }
    let mut state = None;
    let mut runs = None;
    let mut has_pid = false;
    for line in text.lines() {
        let Some(top) = line
            .strip_prefix('\t')
            .filter(|line| !line.starts_with('\t'))
        else {
            continue;
        };
        let Some((key, value)) = top.split_once(" = ") else {
            continue;
        };
        match key {
            "state" => state = Some(value.trim()),
            "runs" => runs = value.trim().parse::<u64>().ok(),
            "pid" => has_pid = true,
            _ => {}
        }
    }
    state == Some("not running") && runs.is_some_and(|runs| runs > 0) && !has_pid
}

fn read_receipt(directory: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(directory.join("capture.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(MAX_COMPLETE_STDOUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_COMPLETE_STDOUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture receipt exceeds its complete-output bound",
        ));
    }
    Ok(Some(bytes))
}

fn await_capture(
    directory: &Path,
    deadline: Instant,
    mut observe: impl FnMut() -> io::Result<DriverState>,
    mut pause: impl FnMut(),
    diagnostics: &mut dyn io::Write,
) -> io::Result<Vec<u8>> {
    let startup_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(bytes) = read_receipt(directory)? {
            return Ok(bytes);
        }
        let driver = observe();
        // The driver may have published between the first read and the ended observation.
        // Its own receipt wins; never synthesize a refusal over a real completion.
        if let Some(bytes) = read_receipt(directory)? {
            return Ok(bytes);
        }
        let driver = driver?;
        if driver.ended {
            diagnostics.write_all(&driver.description)?;
            for name in ["driver.stdout", "driver.stderr"] {
                match File::open(directory.join(name)) {
                    Ok(file) => {
                        let count = io::copy(
                            &mut file.take(MAX_COMPLETE_STDOUT_BYTES as u64 + 1),
                            diagnostics,
                        )?;
                        if count > MAX_COMPLETE_STDOUT_BYTES as u64 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "driver diagnostics exceed their complete-output bound",
                            ));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            let length = |name| -> io::Result<usize> {
                match std::fs::metadata(directory.join(name)) {
                    Ok(metadata)
                        if metadata.is_file()
                            && metadata.len() <= MAX_COMPLETE_STDOUT_BYTES as u64 =>
                    {
                        Ok(metadata.len() as usize)
                    }
                    Ok(_) => Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "driver left invalid producer output",
                    )),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
                    Err(error) => Err(error),
                }
            };
            let refusal = Receipt { schema:"aterm-runtime-capture-v1", complete:false,
                exit:None, signal:None, stdout_bytes:length("stdout")?, stderr_bytes:length("stderr")?,
                error:Some("capture driver ended without publishing a completion receipt; see driver-wait.stderr".into()) };
            // This is a host-bound refusal, not a replacement guest completion artifact.
            return aterm_json::to_string(&refusal)
                .map(String::into_bytes)
                .map_err(io::Error::other);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "capture driver did not complete before its deadline",
            ));
        }
        if !directory.join("started").exists() && Instant::now() >= startup_deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "capture driver never published its start marker",
            ));
        }
        pause();
    }
}

fn wait(phase: Phase, target: &str) -> io::Result<()> {
    let bytes = await_capture(
        &phase.directory,
        phase.deadline,
        || {
            let output = aterm_uds::deadline::output_with_input_until(
                command("/bin/launchctl").args(["print", target]),
                aterm_uds::secret::Secret::new(&[]),
                phase.deadline,
                MAX_COMPLETE_STDOUT_BYTES,
            )?;
            if !output.status.success() {
                return Err(io::Error::other(format!(
                    "cannot observe owned capture driver: {}: {}{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
            let ended = driver_ended(target, &String::from_utf8_lossy(&output.stdout));
            Ok(DriverState {
                ended,
                description: output.stdout,
            })
        },
        || std::thread::sleep(Duration::from_secs(1)),
        &mut std::io::stderr(),
    )?;
    std::io::stdout().write_all(&bytes)
}

/// Serve the runtime's private producer driver before ordinary verifier dispatch.
///
/// An App LaunchAgent runs this mode in the guest. The direct status and both output EOFs
/// precede its atomic completion receipt. Capture refusals retain bounded prefixes and publish
/// an explicitly incomplete receipt; they never claim output completion.
#[must_use]
pub fn entry(args: &[OsString]) -> Option<ExitCode> {
    if args.first().is_none_or(|arg| arg != FLAG) {
        return None;
    }
    let result = Invocation::parse(&args[1..])
        .map_err(str::to_owned)
        .and_then(|invocation| match invocation {
            Invocation::Run(phase, script) => capture(
                command("/bin/sh").args([OsString::from("-c"), script]),
                &phase.directory,
                phase.deadline,
                MAX_COMPLETE_STDOUT_BYTES,
                || false,
                |_| {},
            )
            .map(|receipt| receipt.driver_exit())
            .map_err(|error| error.to_string()),
            Invocation::Wait(phase, target) => wait(phase, &target)
                .map(|()| 0)
                .map_err(|error| error.to_string()),
        });
    Some(ExitCode::from(match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("runtime capture: {error}");
            crate::exit::COULD_NOT_RUN as u8
        }
    }))
}

#[cfg(test)]
#[path = "runtime_capture_tests.rs"]
mod tests;
