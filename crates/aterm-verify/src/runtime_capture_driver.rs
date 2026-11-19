// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private runtime-capture binding and guest-native retirement observation. The self record
//! is data: only the exact job PID and native identity checks admit a process for observation.

use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::deadline::{ProcessProgressProbe, ProgressWindow};
use aterm_uds::process::Identity;

const WINDOW: Duration = Duration::from_secs(60);
const RECORD_LIMIT: u64 = 4096;
const JOB_LIMIT: usize = 64 * 1024;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema: String,
    nonce: String,
    directory: String,
    label: String,
}

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    schema: String,
    nonce: String,
    directory: String,
    target: String,
    identity: String,
}

#[derive(Debug, serde::Serialize)]
struct Observation {
    schema: &'static str,
    nonce: String,
    directory: String,
    target: String,
    outcome: &'static str,
    original_identity: Option<String>,
    observations: u64,
    incomparable: u64,
    reason: Option<String>,
    // One final complete job description, not one file or log entry for every poll.
    job: String,
    job_output_complete: bool,
    job_stdout: Vec<u8>,
    job_stderr: Vec<u8>,
}

fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "runtime driver record is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(io::Error::other("runtime driver record exceeds its bound"));
    }
    aterm_json::from_slice(&bytes).map_err(io::Error::other)
}

fn request(directory: &Path) -> io::Result<Request> {
    let request: Request = read_record(&directory.join("driver-request.json"))?;
    if request.schema != "aterm-runtime-driver-request-v1"
        || request.directory != directory.to_string_lossy()
        || request.nonce.len() != 32
        || !request.nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || !request.label.starts_with("systems.alab.aterm.runtime.")
        || request
            .label
            .bytes()
            .any(|b| !b.is_ascii_alphanumeric() && !b".-_".contains(&b))
    {
        return Err(io::Error::other("invalid private runtime driver request"));
    }
    Ok(request)
}

fn target(request: &Request) -> String {
    // SAFETY: getuid has no arguments and reads this process's real user ID.
    format!("gui/{}/{}", unsafe { libc::getuid() }, request.label)
}

pub(super) fn publish_self(directory: &Path) -> io::Result<()> {
    let request = request(directory)?;
    let identity = aterm_uds::process::read_identity(std::process::id() as i32)
        .ok_or_else(|| io::Error::other("runtime driver native identity is unread"))?;
    let binding = Binding {
        schema: "aterm-runtime-driver-binding-v1".into(),
        target: target(&request),
        nonce: request.nonce,
        directory: request.directory,
        identity: identity.token(),
    };
    let bytes = aterm_json::to_string(&binding).map_err(io::Error::other)?;
    let temporary = directory.join("driver-binding.json.tmp");
    super::fresh(&temporary)?.write_all(bytes.as_bytes())?;
    // No reader can mistake a partial write for a published identity, and no invocation may
    // replace an earlier binding. This record still confers no process or signal authority.
    std::fs::hard_link(&temporary, directory.join("driver-binding.json"))?;
    std::fs::remove_file(temporary)
}

fn job(target: &str, text: &str) -> Result<(bool, Option<i32>), String> {
    let ended = aterm_uds::launchd::job_ended_in(target, text)?;
    let mut depth = 1_u32;
    let mut pid = None;
    let mut runs = None;
    // The shared parser above validates the entire exact block and all top-level fields.
    // Track its structural depth so a nested PID cannot bind the direct driver.
    for line in text.lines().skip(1) {
        let trimmed = line.trim();
        if trimmed == "}" {
            depth -= 1;
        } else {
            if depth == 1 {
                pid = line
                    .strip_prefix("\tpid = ")
                    .and_then(|value| value.trim().parse::<i32>().ok())
                    .or(pid);
                runs = line
                    .strip_prefix("\truns = ")
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .or(runs);
            }
            if trimmed
                .split_once(" = ")
                .is_some_and(|(_, value)| value == "{")
            {
                depth += 1;
            }
        }
    }
    // The fresh RunAtLoad-only job is never restarted by this protocol. An ended successor
    // cannot answer for the admitted driver, even when it reuses the exact target or PID.
    if runs.is_none_or(|runs| runs > 1) {
        return Err("runtime driver job has a successor or unread run generation".into());
    }
    Ok((ended, pid))
}

/// The existing private capture protocol's guest-side observation seam. The native arm runs
/// outside the LaunchAgent tree; its own polling, files and launchctl children cannot advance
/// the admitted driver's progress generation.
trait Driver {
    fn job(&mut self) -> Result<String, String>;
    fn binding(&mut self) -> Result<Option<Binding>, String>;
    fn identity(&mut self, pid: i32) -> Option<Identity>;
    fn progress(&mut self, pid: u32) -> Option<u64>;
    fn now(&mut self) -> Instant;
    fn pause(&mut self);
}

fn unknown(
    mut result: Observation,
    driver: &mut impl Driver,
    target: &str,
    reason: String,
) -> Observation {
    // The exact original job may have ended while a native observation failed. A successor
    // run is rejected by job(), including in this final fast-exit check.
    if let Ok(text) = driver.job() {
        result.job = text;
        if job(target, &result.job).is_ok_and(|(ended, _)| ended) {
            result.outcome = "ended";
            return result;
        }
    }
    result.reason = Some(reason);
    result
}

fn observe(request: &Request, target: &str, driver: &mut impl Driver) -> Observation {
    let mut result = Observation {
        schema: "aterm-runtime-driver-observation-v1",
        nonce: request.nonce.clone(),
        directory: request.directory.clone(),
        target: target.into(),
        outcome: "unknown",
        original_identity: None,
        observations: 0,
        incomparable: 0,
        reason: None,
        job: String::new(),
        job_output_complete: false,
        job_stdout: Vec::new(),
        job_stderr: Vec::new(),
    };
    let mut original = None;
    let mut window = ProgressWindow::new(WINDOW, driver.now());
    loop {
        result.observations = result.observations.saturating_add(1);
        let reading = (|| {
            result.job = driver.job()?;
            let (ended, pid) = job(target, &result.job)?;
            if ended {
                return Ok(None);
            }
            if original.is_none() {
                let Some(binding) = driver.binding()? else {
                    // Publication can legitimately lag bootstrap. Missing data is incomparable,
                    // never zero work, and has no invented wall-clock refusal window.
                    return Ok(Some((None, driver.now())));
                };
                if binding.schema != "aterm-runtime-driver-binding-v1"
                    || binding.nonce != request.nonce
                    || binding.directory != request.directory
                    || binding.target != target
                {
                    return Err("runtime driver binding does not match this private request".into());
                }
                let identity = Identity::parse(&binding.identity)
                    .ok_or("runtime driver binding has no exact native identity")?;
                if pid != Some(identity.pid) || driver.identity(identity.pid) != Some(identity) {
                    return Err(
                        "runtime driver binding is not the current exact job process".into(),
                    );
                }
                original = Some(identity);
                result.original_identity = Some(identity.token());
            }
            let identity = original.expect("only a matched native identity is latched");
            if pid != Some(identity.pid) || driver.identity(identity.pid) != Some(identity) {
                return Err("original runtime driver identity is changed or unread".into());
            }
            let progress = driver.progress(identity.pid as u32);
            let after = driver.identity(identity.pid);
            let sampled_at = driver.now();
            result.job = driver.job()?;
            let (ended, pid) = job(target, &result.job)?;
            // A fast exit, including one racing the native sample, is direct job evidence.
            // It does not invent an owning reap or descendant-closure receipt.
            if ended {
                return Ok(None);
            }
            if after != Some(identity) || pid != Some(identity.pid) {
                return Err("original runtime driver changed during its native tree sample".into());
            }
            Ok(Some((progress, sampled_at)))
        })();
        match reading {
            Ok(None) => {
                result.outcome = "ended";
                return result;
            }
            Ok(Some((progress, sampled_at))) => {
                if progress.is_none() {
                    result.incomparable = result.incomparable.saturating_add(1);
                }
                // Timestamp at the native read, excluding any subsequent launchctl cost.
                if window.observe(sampled_at, 0, || progress).is_zero() {
                    let identity = original.expect("an armed idle window has a bound identity");
                    let before = driver.identity(identity.pid);
                    let confirmed = driver.progress(identity.pid as u32);
                    let after = driver.identity(identity.pid);
                    if before != Some(identity) || after != Some(identity) {
                        return unknown(
                            result,
                            driver,
                            target,
                            "original guest driver changed during idle confirmation".into(),
                        );
                    }
                    // launchctl itself may have been delayed while the driver resumed work.
                    // Confirm fresh native work after that command before naming a hang.
                    if !window.observe(driver.now(), 0, || confirmed).is_zero() {
                        driver.pause();
                        continue;
                    }
                    result.job = match driver.job() {
                        Ok(text) => text,
                        Err(reason) => return unknown(result, driver, target, reason),
                    };
                    match job(target, &result.job) {
                        Ok((true, _)) => {
                            result.outcome = "ended";
                            return result;
                        }
                        Ok((false, Some(pid))) if pid == identity.pid => {}
                        _ => {
                            return unknown(
                                result,
                                driver,
                                target,
                                "exact original job changed at idle confirmation".into(),
                            );
                        }
                    }
                    // The final exact-job read can itself be delayed. Recheck native work
                    // across it; a changed/unread identity goes through the ended-first path.
                    // No observer output or host launcher work advances this generation.
                    let before = driver.identity(identity.pid);
                    let final_work = driver.progress(identity.pid as u32);
                    let after = driver.identity(identity.pid);
                    if before != Some(identity) || after != Some(identity) {
                        return unknown(
                            result,
                            driver,
                            target,
                            "original guest driver changed after final job observation".into(),
                        );
                    }
                    if !window.observe(driver.now(), 0, || final_work).is_zero() {
                        driver.pause();
                        continue;
                    }
                    result.outcome = "idle";
                    result.reason = Some("original guest driver made no comparable native CPU/I/O progress for one minute".into());
                    return result;
                }
            }
            Err(reason) => {
                // Ended wins even when disappearance/reuse raced a failed identity read.
                // Otherwise structural/binding/native-read failure is terminal Unknown;
                // a None progress sample alone only disarms the inactivity window.
                return unknown(result, driver, target, reason);
            }
        }
        driver.pause();
    }
}

struct Native<'a> {
    directory: &'a Path,
    target: &'a str,
    probe: ProcessProgressProbe,
    last_job: Option<std::process::Output>,
}

impl Driver for Native<'_> {
    fn job(&mut self) -> Result<String, String> {
        self.last_job = None;
        let output = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
            aterm_uds::spawnfd::command("/bin/launchctl").args(["print", self.target]),
            WINDOW,
            "exact guest runtime driver state",
            JOB_LIMIT,
        )
        .map_err(|error| error.to_string())?;
        self.last_job = Some(output);
        let output = self
            .last_job
            .as_ref()
            .expect("complete owned job output retained");
        if !output.status.success() {
            return Err(format!(
                "launchctl print exited {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        std::str::from_utf8(&output.stdout)
            .map(str::to_owned)
            .map_err(|error| error.to_string())
    }
    fn binding(&mut self) -> Result<Option<Binding>, String> {
        match read_record(&self.directory.join("driver-binding.json")) {
            Ok(binding) => Ok(Some(binding)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
    fn identity(&mut self, pid: i32) -> Option<Identity> {
        aterm_uds::process::read_identity(pid)
    }
    fn progress(&mut self, pid: u32) -> Option<u64> {
        self.probe.read(pid)
    }
    fn now(&mut self) -> Instant {
        Instant::now()
    }
    fn pause(&mut self) {
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub(super) fn retirement(directory: &Path, nonce: &str, admitted_target: &str) -> io::Result<()> {
    let request = request(directory)?;
    if request.nonce != nonce || target(&request) != admitted_target {
        return Err(io::Error::other(
            "retirement request is not this exact admitted target",
        ));
    }
    let mut native = Native {
        directory,
        target: admitted_target,
        probe: ProcessProgressProbe::default(),
        last_job: None,
    };
    let mut result = observe(&request, admitted_target, &mut native);
    if let Some(output) = native.last_job {
        result.job_output_complete = true;
        result.job_stdout = output.stdout;
        result.job_stderr = output.stderr;
    }
    let bytes = aterm_json::to_string(&result).map_err(io::Error::other)?;
    std::io::stdout().write_all(bytes.as_bytes())
}

#[cfg(test)]
#[path = "runtime_capture_driver_tests.rs"]
mod tests;
