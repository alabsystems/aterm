// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Q4: release-product runtime rows in a disposable macOS guest. A committed source snapshot,
//! its submodule, and the resolved compiler are transported as data; the guest builds its own
//! verifier once. The bundle is never executed on the host. Every take retains its complete
//! stdout, stderr, exit status, guest resource usage, and host load, including failed takes.
//! Complete output is bounded at 16 MiB per stream; a capture refusal is never a complete take.

use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Value, json};
use aterm_uds::spawnfd::{Stdio, command};

use crate::mac::after_vm_cleanup as after_cleanup;
use crate::mac::{Vm, VmScratch, not_run};
use crate::paths::paths;
use crate::util::shell_quote as q;
use crate::{Fail, Res, stop};

mod count_compare;
mod measure_costs;
#[path = "mac_runtime_snapshot.rs"]
mod snapshot;

pub(crate) const COUNT_USAGE: &str = "  count-ranks --record DIR [--candidate DIR]\n                Recompute B14's exact count ranks from retained Q4 logs; no product is run.\n                Candidate is five later default-policy takes on the same bundle and guest shape.\n                Relative directories are resolved from this checkout's root.\n  settle-ranks --record DIR\n                Recompute the matched idle-RSS setup experiment, including every row's verdicts.\n  wait-ranks --record DIR\n                Recompute the matched Count-readiness experiments, including setup spans.\n";

pub(crate) fn settle_ranks(args: &[String]) -> Res {
    let [flag, dir] = args else {
        return Err(Fail::usage(COUNT_USAGE));
    };
    if flag != "--record" {
        return Err(Fail::usage(COUNT_USAGE));
    }
    println!(
        "{}",
        count_compare::settle_records(&paths().repo.join(dir))?
    );
    Ok(0)
}

pub(crate) fn wait_ranks(args: &[String]) -> Res {
    let [flag, dir] = args else {
        return Err(Fail::usage(COUNT_USAGE));
    };
    if flag != "--record" {
        return Err(Fail::usage(COUNT_USAGE));
    }
    println!("{}", count_compare::wait_records(&paths().repo.join(dir))?);
    Ok(0)
}

fn count_args(args: &[String], repo: &Path) -> Result<(PathBuf, Option<PathBuf>), Fail> {
    match args {
        [flag, record] if flag == "--record" => Ok((repo.join(record), None)),
        [flag, record, next, candidate] if flag == "--record" && next == "--candidate" => {
            Ok((repo.join(record), Some(repo.join(candidate))))
        }
        _ => Err(Fail::usage(COUNT_USAGE)),
    }
}

pub(crate) fn count_ranks(args: &[String]) -> Res {
    if args == ["--help"] {
        print!("{COUNT_USAGE}");
        return Ok(0);
    }
    let (record, candidate) = count_args(args, &paths().repo)?;
    println!("{}", count_compare::records(&record, candidate.as_deref())?);
    Ok(0)
}

pub(crate) const USAGE: &str = "\
  runtime --app BUNDLE --record DIR [--lane measure|cursor|rewind|rewind-control|cat|compare|fabric|floors] [--image REF]
      [--baseline BUNDLE]
      [--cpus N] [--memory-gib N] [--disk-gb N]
      [--display window|headless]
                Q4: five takes of this committed checkout's verify measure --app BUNDLE, or a
                perf lane, inside one disposable macOS guest. Copies the resolved Trust compiler
                and builds the verifier in the guest; no release product runs on the host.
                Uses the image's admin account without sudo; requires SIP on and performs no
                privileged guest preparation. Inputs and builds stay in that account's home.
                With --baseline, measure/cursor/cat warm both images and alternate their order
                for five paired takes in the same guest, retaining each arm separately.
                Default bounds: 8 vCPUs, 32 GiB RAM, 60 decimal GB disk; --cpus, --memory-gib
                and --disk-gb name larger or smaller fixed bounds, recorded with every run.
                The shipped fabric link needs --memory-gib 48 --disk-gb 80. The clone and builds are
                deleted after checked retirement. Unconfirmed cleanup retains the owned VM,
                reservation, slots and evidence. One of the account's two guest slots is held
                through boot and teardown. DIR keeps JSON rows and raw logs (never a
                cached qualification); a missing image is pulled automatically. --lane compare
                compares the supplied bundle with itself as an instrument/noise control.
                Window is the default: one owned Tart window under the shared display lock.
                Headless is a presentation-clock negative control; cursor runs only the GUI
                cursor-pacing burst, to qualify that clock before the full matrix.
                Qualified count rows run together; timing rows run alone. The sequential
                count experiment was retired after B14 qualification.
                rewind runs only recording throughput; rewind-control runs the identical plan
                with recording off in both arms, an A/A noise control with no recording verdict.
";

// The pinned image's login account owns all runtime inputs and build products.
const ROOT: &str = "/Users//admin/.ubd/runtime.noindex";
const TAKES: u32 = 5;
const MEMORY_MIB: u32 = 32768;
const CPUS: u32 = 8;
const DISK_GB: u32 = 60;
// The first Q4 base pulled on m118, 2026-11-06. Pin the image rather than its mutable tag.
const IMAGE: &str = "ghcr.io/cirruslabs/macos-golden-gate-base@sha256:14f4fc32a778763ccae1a4d543ea0f177720c570dd7f859eda10b9e0020c7d05";
// A build or a complete perf compare may take much longer than a single measurement row.
const BUILD: Duration = Duration::from_secs(7200);
const TAKE: Duration = Duration::from_secs(3600);

#[derive(Debug, PartialEq, Eq)]
struct Ask {
    app: PathBuf,
    baseline: Option<PathBuf>,
    record: PathBuf,
    lane: String,
    image: String,
    cpus: u32,
    memory_mib: u32,
    disk_gb: u32,
    graphics: bool,
}

fn parse(args: &[String]) -> Result<Ask, Fail> {
    let mut app = None;
    let mut baseline = None;
    let mut record = None;
    let mut lane = "measure".to_owned();
    let mut image = std::env::var("LAB_MAC_IMAGE").unwrap_or_else(|_| IMAGE.into());
    let mut cpus = CPUS;
    let mut memory_mib = MEMORY_MIB;
    let mut disk_gb = DISK_GB;
    let mut graphics = true;
    let mut at = 0;
    while at < args.len() {
        let flag = &args[at];
        at += 1;
        let value = args
            .get(at)
            .ok_or_else(|| Fail::usage(format!("{flag} needs a value")))?;
        at += 1;
        match flag.as_str() {
            "--app" if app.is_none() => app = Some(PathBuf::from(value)),
            "--baseline" if baseline.is_none() => baseline = Some(PathBuf::from(value)),
            "--record" if record.is_none() => record = Some(PathBuf::from(value)),
            "--lane"
                if matches!(
                    value.as_str(),
                    "measure"
                        | "cursor"
                        | "rewind"
                        | "rewind-control"
                        | "cat"
                        | "compare"
                        | "fabric"
                        | "floors"
                ) =>
            {
                lane.clone_from(value)
            }
            "--image" => image.clone_from(value),
            "--display" if matches!(value.as_str(), "window" | "headless") => {
                graphics = value == "window"
            }
            "--cpus" | "--memory-gib" | "--disk-gb" => {
                let n = value
                    .parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| Fail::usage(format!("{flag} needs a positive integer")))?;
                match flag.as_str() {
                    "--cpus" => cpus = n,
                    "--disk-gb" => disk_gb = n,
                    _ => {
                        memory_mib = n
                            .checked_mul(1024)
                            .ok_or_else(|| Fail::usage("--memory-gib is too large"))?
                    }
                }
            }
            _ => {
                return Err(Fail::usage(format!(
                    "runtime: unknown/duplicate option or lane {flag} {value}"
                )));
            }
        }
    }
    if baseline.is_some() && !matches!(lane.as_str(), "measure" | "cursor" | "cat") {
        return Err(Fail::usage("--baseline supports measure, cursor and cat"));
    }
    Ok(Ask {
        app: app.ok_or_else(|| Fail::usage("runtime needs --app BUNDLE"))?,
        baseline,
        record: record.ok_or_else(|| Fail::usage("runtime needs --record DIR"))?,
        lane,
        image,
        cpus,
        memory_mib,
        disk_gb,
        graphics,
    })
}

fn checked(program: &str, args: &[String], cwd: &Path) -> Result<String, Fail> {
    let out = command(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(Fail::die(format!(
            "{program}: {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn load() -> String {
    checked(
        "/usr/sbin/sysctl",
        &["-n".into(), "vm.loadavg".into()],
        &paths().repo,
    )
    .unwrap_or_else(|e| e.msg.unwrap_or_else(|| "load unavailable".into()))
}

fn sampled<T>(run: impl FnOnce() -> T) -> (T, Vec<String>) {
    std::thread::scope(|scope| {
        let (end, until) = std::sync::mpsc::channel::<()>();
        let reader = scope.spawn(move || {
            let mut readings = vec![load()];
            while matches!(
                until.recv_timeout(Duration::from_secs(1)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                readings.push(load());
            }
            readings.push(load());
            readings
        });
        let result = run();
        drop(end);
        (result, reader.join().unwrap_or_default())
    })
}

fn max_load(readings: &[String]) -> Option<f64> {
    readings
        .iter()
        .filter_map(|s| {
            s.trim_matches(|c: char| c == '{' || c == '}' || c.is_whitespace())
                .split_whitespace()
                .next()?
                .parse::<f64>()
                .ok()
        })
        .filter(|v| v.is_finite())
        .max_by(f64::total_cmp)
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Fail> {
    crate::util::write_fresh(path, bytes).map_err(|e| format!("{}: {e}", path.display()).into())
}

fn write_json(path: &Path, value: &Value) -> Result<(), Fail> {
    let mut bytes = aterm_json::to_string(value)
        .map_err(|e| format!("JSON: {e}"))?
        .into_bytes();
    bytes.push(b'\n');
    write(path, &bytes)
}

const SIP_STATUS_COMMAND: &str = "csrutil status";

/// References describing this probe, not authenticated image or executable identities.
struct SipProbe<'a> {
    tart: &'a str,
    requested_image: &'a str,
    selected_base: &'a str,
    guest: &'a str,
}

/// Retain the complete bounded probe result before applying the existing stdout-only SIP gate.
/// `capture` uses the runtime's complete-output guard; its error supplies neither complete
/// streams nor a process status. A refusal to write the evidence cannot admit guest setup.
fn require_sip_on(
    record: &Path,
    probe: &SipProbe<'_>,
    capture: impl FnOnce() -> Result<std::process::Output, String>,
) -> Result<(), Fail> {
    let identity = json!({
        "tart_path_reference": probe.tart,
        "requested_image_reference": probe.requested_image,
        "selected_base_reference": probe.selected_base,
        "vm_name_reference": probe.guest,
        "argv": [probe.tart, "exec", probe.guest, "/bin/sh", "-c", SIP_STATUS_COMMAND],
    });
    let out = match capture() {
        Ok(out) => out,
        Err(error) => {
            write_json(
                &record.join("sip.json"),
                &json!({
                    "schema": "aterm-mac-runtime-probe-v1", "probe": identity,
                    "output_complete": false, "success": null, "exit": null, "signal": null,
                    "stdout_file": null, "stderr_file": null,
                    "stdout_bytes": null, "stderr_bytes": null, "error": &error,
                }),
            )?;
            return Err(error.into());
        }
    };
    write(&record.join("sip.stdout"), &out.stdout)?;
    write(&record.join("sip.stderr"), &out.stderr)?;
    write_json(
        &record.join("sip.json"),
        &json!({
            "schema": "aterm-mac-runtime-probe-v1", "probe": identity,
            "output_complete": true, "success": out.status.success(),
            "exit": out.status.code(), "signal": out.status.signal(),
            "stdout_file": "sip.stdout", "stderr_file": "sip.stderr",
            "stdout_bytes": out.stdout.len(), "stderr_bytes": out.stderr.len(), "error": null,
        }),
    )?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        return Err(Fail::die(format!(
            "guest `{SIP_STATUS_COMMAND}`: {}: {text}{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    if !crate::mac::sip_on(&text) {
        return Err(not_run(
            "runtime requires a SIP-on image; it performs no privileged guest preparation",
        ));
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureReceipt {
    schema: String,
    complete: bool,
    exit: Option<i32>,
    signal: Option<i32>,
    stdout_bytes: usize,
    stderr_bytes: usize,
    error: Option<String>,
}

impl CaptureReceipt {
    fn parse(bytes: &[u8]) -> Result<Self, Fail> {
        let receipt: Self = aterm_json::from_slice(bytes)
            .map_err(|error| not_run(format!("guest capture receipt: {error}")))?;
        let status_valid = matches!(
            (receipt.exit, receipt.signal),
            (Some(0..=255), None) | (None, Some(1..=126))
        );
        let unknown = receipt.exit.is_none() && receipt.signal.is_none();
        if receipt.schema != "aterm-runtime-capture-v1"
            || (!status_valid && !unknown)
            || receipt.stdout_bytes > aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES
            || receipt.stderr_bytes > aterm_uds::deadline::MAX_COMPLETE_STDOUT_BYTES
            || (receipt.complete && (!status_valid || receipt.error.is_some()))
            || (!receipt.complete && receipt.error.as_ref().is_none_or(String::is_empty))
        {
            return Err(not_run(
                "guest capture receipt has invalid completion evidence",
            ));
        }
        Ok(receipt)
    }

    fn status(&self) -> Option<std::process::ExitStatus> {
        match (self.exit, self.signal) {
            (Some(code), None) => Some(std::process::ExitStatus::from_raw(code << 8)),
            (None, Some(signal)) => Some(std::process::ExitStatus::from_raw(signal)),
            _ => None,
        }
    }
}

fn preserve_observed(status: Option<std::process::ExitStatus>, error: Fail) -> Fail {
    match status {
        Some(status) => preserve_failure(&[status.code()], error),
        None => error,
    }
}

/// Retain raw transport bytes before applying receipt validation or collecting artifacts. A
/// completed phase requires the guest's direct status, both producer EOFs and exact file lengths.
fn retain_phase<T>(
    record: &Path,
    phase: &str,
    script: &str,
    mut output: std::process::Output,
    receipt: Result<CaptureReceipt, Fail>,
    collect: impl FnOnce(&std::process::Output) -> Result<T, Fail>,
) -> Result<(std::process::Output, T), Fail> {
    write(&record.join(format!("{phase}.stdout")), &output.stdout)?;
    write(&record.join(format!("{phase}.stderr")), &output.stderr)?;
    let status = receipt.as_ref().ok().and_then(CaptureReceipt::status);
    let ready = receipt.and_then(|receipt| {
        if !output.status.success() {
            Err(not_run(format!(
                "guest output transfer exited {}",
                output.status
            )))
        } else if !receipt.complete {
            Err(not_run(receipt.error.unwrap_or_else(|| {
                "producer capture was incomplete".into()
            })))
        } else if output.stdout.len() != receipt.stdout_bytes
            || output.stderr.len() != receipt.stderr_bytes
        {
            Err(not_run(
                "guest output lengths differ from the sealed capture receipt",
            ))
        } else {
            Ok(())
        }
    });
    write_json(
        &record.join(format!("{phase}.execution.json")),
        &json!({"phase": phase, "command": script, "exit": status.and_then(|status| status.code()),
            "signal": status.and_then(|status| status.signal()),
            "success": status.map(|status| status.success()), "output_complete": ready.is_ok(),
            "capture_error": ready.as_ref().err().and_then(|error| error.msg.as_ref()),
            "transport_exit": output.status.code(),
            "stdout_bytes": output.stdout.len(), "stderr_bytes": output.stderr.len(),
            "usage": usage(&String::from_utf8_lossy(&output.stderr))}),
    )?;
    ready.map_err(|error| preserve_observed(status, error))?;
    output.status =
        status.ok_or_else(|| not_run("completed guest capture has no direct status"))?;
    let artifacts = collect(&output).map_err(|error| preserve_observed(status, error))?;
    Ok((output, artifacts))
}

/// An incomplete later capture cannot erase any already observed failed execution.
fn preserve_failure(codes: &[Option<i32>], mut error: Fail) -> Fail {
    if codes.iter().any(|code| matches!(code, None | Some(1))) {
        error.code = 1;
    }
    error
}

/// Transport an artifact as bytes with proven complete stdout and a successful guest read.
fn guest_bytes(vm: &Vm, script: &str) -> Result<Vec<u8>, Fail> {
    let output = vm.exec_output(script, crate::mac::STEP).map_err(not_run)?;
    if !output.status.success() {
        return Err(not_run(format!(
            "guest artifact read exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

/// Timing drivers belong to the console's app band. An Interactive job is still a daemon
/// task: neither its driver nor its subjects can take the UI role the instrument requests.
/// Driver diagnostics are separate from the producer files its pipe capture seals.
fn app_job(root: &Path, phase: &str, arguments: &[String]) -> Value {
    let dir = root.join(phase);
    json!({
        "Label": format!("systems.alab.aterm.runtime.{phase}"),
        "ProgramArguments": arguments,
        "ProcessType": "App", "RunAtLoad": true,
        "StandardOutPath": dir.join("driver.stdout").display().to_string(),
        "StandardErrorPath": dir.join("driver.stderr").display().to_string()
    })
}

fn reserve_phase(root: &Path, phase: &str) -> String {
    format!(
        "mkdir -p {}; mkdir {}",
        q(&root.display().to_string()),
        q(&root.join(phase).display().to_string())
    )
}

fn retain_driver_command(
    record: &Path,
    phase: &str,
    step: &str,
    script: &str,
    result: Result<std::process::Output, Fail>,
) -> Result<std::process::Output, Fail> {
    match result {
        Ok(output) => {
            write(
                &record.join(format!("{phase}.{step}.stdout")),
                &output.stdout,
            )?;
            write(
                &record.join(format!("{phase}.{step}.stderr")),
                &output.stderr,
            )?;
            write_json(
                &record.join(format!("{phase}.{step}.json")),
                &json!({
                "command":script, "output_complete":true, "exit":output.status.code(),
                "signal":output.status.signal(), "success":output.status.success()}),
            )?;
            Ok(output)
        }
        Err(error) => {
            write_json(
                &record.join(format!("{phase}.{step}.json")),
                &json!({
                "command":script, "output_complete":false, "exit":null,
                "signal":null, "success":null, "error":error.msg.as_deref()}),
            )?;
            Err(error)
        }
    }
}

fn created_driver_target(stdout: &[u8], label: &str) -> Result<String, Fail> {
    let text = std::str::from_utf8(stdout)
        .map_err(|error| not_run(format!("capture driver creation is not UTF-8: {error}")))?;
    let mut targets = text
        .lines()
        .filter_map(|line| line.strip_prefix("aterm-runtime-target="));
    let target = targets
        .next()
        .filter(|target| {
            target
                .strip_prefix("gui/")
                .and_then(|rest| rest.split_once('/'))
                .is_some_and(|(uid, name)| {
                    uid.parse::<u32>().is_ok_and(|uid| uid > 0) && name == label
                })
        })
        .ok_or_else(|| not_run("capture driver creation did not name its exact target"))?;
    if targets.next().is_some() {
        return Err(not_run("capture driver creation repeated its target"));
    }
    Ok(target.to_owned())
}

fn retain_driver_diagnostics(
    record: &Path,
    phase: &str,
    dir: &str,
    run: &mut impl FnMut(&str) -> Result<std::process::Output, Fail>,
) -> Result<(), Fail> {
    let mut result = Ok(());
    for stream in ["stdout", "stderr"] {
        let script = format!("cat -- {}", q(&format!("{dir}/driver.{stream}")));
        let diagnostic = retain_driver_command(
            record,
            phase,
            &format!("driver-read-{stream}"),
            &script,
            run(&script),
        )
        .and_then(|output| {
            write(
                &record.join(format!("{phase}.driver.{stream}")),
                &output.stdout,
            )?;
            if output.status.success() {
                Ok(())
            } else {
                Err(not_run(format!(
                    "capture driver {stream} read exited {}",
                    output.status
                )))
            }
        });
        result = after_cleanup(result, || diagnostic);
    }
    result
}

/// Retire only the exact LaunchAgent this phase successfully bootstrapped. Its direct ended
/// state and checked bootout do not assert descendant closure: the native run archive and the
/// checked VM finish carry separate evidence, retained independently.
fn retire_driver_using(
    record: &Path,
    phase: &str,
    dir: &str,
    target: &str,
    nonce: &str,
    mut run: impl FnMut(&str) -> Result<std::process::Output, Fail>,
    mut observe: impl FnMut(&str) -> Result<std::process::Output, Fail>,
) -> Result<(), Fail> {
    let _grace = stop::grace();
    let script = format!(
        "{ROOT}/target.noindex/debug/aterm-verify --runtime-capture observe-driver {} {} {}",
        q(dir),
        q(nonce),
        q(target),
    );
    let ended = retain_driver_command(
        record,
        phase,
        "driver-observation",
        &script,
        observe(&script),
    )
    .and_then(|output| {
        if !output.status.success() {
            return Err(not_run(format!(
                "guest driver observer exited {}",
                output.status
            )));
        }
        let observation: Value = aterm_json::from_slice(&output.stdout).map_err(|error| {
            not_run(format!(
                "guest driver observation is not complete JSON: {error}"
            ))
        })?;
        if observation["schema"].as_str() != Some("aterm-runtime-driver-observation-v1")
            || observation["nonce"].as_str() != Some(nonce)
            || observation["directory"].as_str() != Some(dir)
            || observation["target"].as_str() != Some(target)
        {
            return Err(not_run(
                "guest driver observation does not match its private admitted request",
            ));
        }
        if observation["outcome"].as_str() != Some("ended") {
            return Err(not_run(format!(
                "guest driver retirement {}: {}",
                observation["outcome"], observation["reason"]
            )));
        }
        let description = observation["job"]
            .as_str()
            .ok_or_else(|| not_run("guest driver observation omits its final exact job state"))?;
        let raw: Vec<u8> = aterm_json::from_value(observation["job_stdout"].clone())
            .map_err(|error| not_run(format!("guest driver final raw state: {error}")))?;
        if observation["job_output_complete"].as_bool() != Some(true)
            || raw != description.as_bytes()
        {
            return Err(not_run(
                "guest driver ended state lacks matching complete raw command output",
            ));
        }
        if aterm_uds::launchd::job_ended_in(target, description) != Ok(true)
            || !description
                .lines()
                .filter_map(|line| line.strip_prefix("\truns = "))
                .map(str::trim)
                .eq(std::iter::once("1"))
        {
            return Err(not_run(
                "guest driver observation does not establish its exact ended state",
            ));
        }
        Ok(())
    });
    let bootout = format!("/bin/launchctl bootout {}", q(target));
    let removed = retain_driver_command(record, phase, "driver-bootout", &bootout, run(&bootout))
        .and_then(|output| {
            if output.status.success() {
                Ok(())
            } else {
                Err(not_run(format!(
                    "owned capture driver bootout exited {}",
                    output.status
                )))
            }
        });
    let ended_ok = ended.is_ok();
    let cancelled = ended.as_ref().err().and_then(|error| error.stopped);
    let removed_ok = removed.is_ok();
    let result = after_cleanup(after_cleanup(ended, || removed), || {
        retain_driver_diagnostics(record, phase, dir, &mut run)
    });
    write_json(
        &record.join(format!("{phase}.driver-closure.json")),
        &json!({
        "schema":"aterm-mac-runtime-driver-v1", "target":target,
        "direct_job_ended":ended_ok, "bootout_succeeded":removed_ok,
        "retirement":if cancelled.is_some() { "cancelled-forced-cleanup" } else if ended_ok { "normal-ended" } else { "forced-without-ended-evidence" },
        "observer_cancelled_by_signal":cancelled,
        "complete":result.is_ok(), "descendant_closure":"not asserted by this receipt"}),
    )?;
    result
}

fn app_exec(
    vm: &Vm,
    ask: &Ask,
    phase: &str,
    script: &str,
    limit: Duration,
) -> Result<(std::process::Output, Value), Fail> {
    let deadline = Instant::now() + limit;
    let dir = format!("{ROOT}/driver/{phase}");
    let reservation = reserve_phase(&Path::new(ROOT).join("driver"), phase);
    let job = app_job(
        &Path::new(ROOT).join("driver"),
        phase,
        &[
            format!("{ROOT}/target.noindex/debug/aterm-verify"),
            "--runtime-capture".into(),
            "run".into(),
            dir.clone(),
            limit.as_secs().to_string(),
            script.into(),
        ],
    );
    write_json(&ask.record.join(format!("{phase}.driver.json")), &job)?;
    let encoded = aterm_json::to_string(&job).map_err(|e| format!("driver JSON: {e}"))?;
    let label = job["Label"].as_str().expect("job label");
    let nonce = aterm_uds::rand::hex_token::<16>()
        .map_err(|error| not_run(format!("private driver nonce: {error}")))?;
    let request = json!({"schema":"aterm-runtime-driver-request-v1", "nonce":nonce,
        "directory":dir, "label":label});
    write_json(
        &ask.record.join(format!("{phase}.driver-request.json")),
        &request,
    )?;
    let request = aterm_json::to_string(&request).map_err(|error| not_run(error))?;
    let bootstrap = format!(
        "set -eu; umask 077; {reservation}; printf '%s' {} > {dir}/driver-request.json; \
         printf '%s' {} > {dir}/job.json; \
         /usr/bin/plutil -convert xml1 -o {dir}/job.plist {dir}/job.json; \
         uid=$(id -u); /bin/launchctl bootstrap gui/$uid {dir}/job.plist; \
         printf '%s\\n' \"aterm-runtime-target=gui/$uid/{label}\"",
        q(&request),
        q(&encoded),
    );
    let creation = retain_driver_command(
        &ask.record,
        phase,
        "driver-bootstrap",
        &bootstrap,
        vm.exec_output(&bootstrap, limit).map_err(not_run),
    )
    .and_then(|created| {
        if !created.status.success() {
            return Err(not_run(format!(
                "capture driver bootstrap exited {}",
                created.status
            )));
        }
        created_driver_target(&created.stdout, label)
    });
    // Only positive creation permits control of the exact phase target. If creation evidence
    // is unknown, retain the private diagnostic paths but leave control to checked VM finish.
    let target = match creation {
        Ok(target) => target,
        Err(error) => {
            return after_cleanup(Err(error), || {
                let diagnostics =
                    retain_driver_diagnostics(&ask.record, phase, &dir, &mut |command| {
                        vm.exec_output(command, crate::mac::STEP).map_err(not_run)
                    });
                write_json(
                    &ask.record.join(format!("{phase}.driver-closure.json")),
                    &json!({
                "schema":"aterm-mac-runtime-driver-v1", "target":null,
                "creation_confirmed":false, "complete":false,
                "descendant_closure":"not asserted by this receipt"}),
                )?;
                diagnostics
            });
        }
    };
    let result = (|| {
        let command = format!(
            "{ROOT}/target.noindex/debug/aterm-verify --runtime-capture wait {dir} {seconds} {}",
            q(&target),
            seconds = limit.as_secs()
        );
        let ready = vm
            .exec_output(&command, deadline.saturating_duration_since(Instant::now()))
            .map_err(not_run)?;
        write(
            &ask.record.join(format!("{phase}.driver-wait.stdout")),
            &ready.stdout,
        )?;
        write(
            &ask.record.join(format!("{phase}.driver-wait.stderr")),
            &ready.stderr,
        )?;
        let transfer = format!(
            "result=0; cat {dir}/stdout || result=$?; cat {dir}/stderr >&2 || result=$?; exit \"$result\""
        );
        if !ready.status.success() {
            let reason = not_run(format!(
                "guest capture driver wait exited {}; see {phase}.driver-wait.stderr",
                ready.status
            ));
            return match vm.exec_output(&transfer, crate::mac::STEP) {
                Ok(output) => retain_phase(
                    &ask.record,
                    phase,
                    script,
                    output,
                    Err(reason),
                    |_| -> Result<Value, Fail> {
                        unreachable!("an unavailable capture cannot collect artifacts")
                    },
                ),
                Err(_) => Err(reason),
            };
        }
        write(
            &ask.record.join(format!("{phase}.capture.json")),
            &ready.stdout,
        )?;
        let receipt = CaptureReceipt::parse(&ready.stdout);
        let status = receipt.as_ref().ok().and_then(CaptureReceipt::status);
        let out = vm
            .exec_output(
                &transfer,
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(crate::mac::STEP),
            )
            .map_err(|error| preserve_observed(status, not_run(error)))?;
        retain_phase(&ask.record, phase, script, out, receipt, |out| {
            collect(vm, ask, phase, &String::from_utf8_lossy(&out.stdout))
        })
    })();
    let observed = result.as_ref().ok().map(|(output, _)| output.status);
    after_cleanup(result, || {
        retire_driver_using(
            &ask.record,
            phase,
            &dir,
            &target,
            &nonce,
            |command| vm.exec_output(command, crate::mac::STEP).map_err(not_run),
            |command| vm.retirement_output(command),
        )
    })
    .map_err(|error| preserve_observed(observed, error))
}

/// `time -lp`'s own fields, not the program's numbers. Missing resource readings stay null.
fn usage(stderr: &str) -> Value {
    let mut fields = aterm_json::Map::new();
    for line in stderr.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() == 2 && matches!(words[0], "real" | "user" | "sys") {
            if let Ok(n) = words[1].parse::<f64>() {
                fields.insert(words[0].into(), json!(n));
            }
        } else if words.get(1..) == Some(&["maximum", "resident", "set", "size"][..])
            && let Ok(n) = words[0].parse::<u64>()
        {
            fields.insert("peak_rss_bytes".into(), json!(n));
        }
    }
    Value::Object(fields)
}

fn spread(values: &[f64]) -> Value {
    if values.len() != TAKES as usize || values.iter().any(|v| !v.is_finite()) {
        return Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({"takes": values, "min": sorted[0], "median": sorted[2], "max": sorted[4],
        "range": sorted[4] - sorted[0]})
}

fn experiment_exit(codes: &[Option<i32>], paired: bool) -> i32 {
    if codes.iter().any(|c| matches!(c, None | Some(1))) {
        1
    } else if codes.len() != (TAKES as usize + 1) * if paired { 2 } else { 1 }
        || codes.iter().any(|c| *c != Some(0))
    {
        3
    } else {
        0
    }
}

fn disk_room(df: &str, disk_gb: u32) -> bool {
    // df -g reports GiB; Tart 2.40.1's resizeRawDisk/resizeASIFDisk take decimal GB.
    crate::mac_nux::df_free_gb(df)
        .and_then(|gib| gib.checked_mul(1 << 30))
        .is_some_and(|bytes| bytes >= (u64::from(disk_gb) + 30) * 1_000_000_000)
}

fn verifier_log(stdout: &str) -> Option<&str> {
    stdout.lines().find_map(|line| {
        let path = line.strip_prefix("log: ")?;
        let relative = path.strip_prefix(&format!("{ROOT}/target.noindex/verify/"))?;
        if !relative_member(relative) {
            return None;
        }
        let file = Path::new(relative).file_name()?.to_str()?;
        let (time, pid) = file.strip_suffix(".log")?.split_once('-')?;
        (!time.is_empty()
            && !pid.is_empty()
            && time.bytes().chain(pid.bytes()).all(|b| b.is_ascii_digit()))
        .then_some(path)
    })
}

/// Where the guest's perf lanes write their ledger entries: aterm-verify's `perf::ledger_dir`, in
/// the verify state directory under the guest's `CARGO_TARGET_DIR` ([`environment`]), spelled
/// from the same [`aterm_tempfile::QUIET`] folder name (aterm-lab cannot depend on aterm-verify).
fn perf_ledgers() -> String {
    format!(
        "{ROOT}/target.noindex/verify/{}/perf.noindex",
        aterm_tempfile::QUIET
    )
}

/// `path` relative to [`perf_ledgers`], where it names a file inside it.
fn ledger_member(path: &str) -> Option<&str> {
    let relative = path
        .strip_prefix(perf_ledgers().as_str())?
        .strip_prefix('/')?;
    relative_member(relative).then_some(relative)
}

fn relative_member(path: &str) -> bool {
    !path.is_empty()
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        && Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// stdout contains the verifier's summary. Its row values live in the log; perf also writes
/// structured records into the guest's verify state ([`perf_ledgers`]). Bring those back before
/// the disposable disk goes.
fn collect(vm: &Vm, ask: &Ask, name: &str, stdout: &str) -> Result<Value, Fail> {
    collect_using(ask, name, stdout, |script| guest_bytes(vm, script))
}

fn collect_using(
    ask: &Ask,
    name: &str,
    stdout: &str,
    mut read: impl FnMut(&str) -> Result<Vec<u8>, Fail>,
) -> Result<Value, Fail> {
    let mut artifacts = aterm_json::Map::new();
    if let Some(path) = verifier_log(stdout) {
        let mut run = None;
        let archives = [
            ("", "verifier_log"),
            (".events", "verifier_events"),
            (".run", "verifier_run"),
        ];
        let required = if matches!(ask.lane.as_str(), "measure" | "cursor") {
            archives.len()
        } else {
            1
        };
        for (suffix, key) in archives.into_iter().take(required) {
            let bytes = read(&format!("cat -- {}", q(&format!("{path}{suffix}"))))?;
            let file = format!("{name}.verifier.log{suffix}");
            write(&ask.record.join(&file), &bytes)?;
            artifacts.insert(key.into(), json!(file));
            artifacts.insert(
                format!("{key}_content"),
                json!({"bytes":bytes.len(),
                "sha256":aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes))}),
            );
            if suffix == ".run" {
                run = Some(bytes);
            }
        }
        if let Some(run) = run {
            let run = String::from_utf8(run).map_err(|error| {
                not_run(format!("guest verifier run archive is not UTF-8: {error}"))
            })?;
            // This is the native writer's closed-generation protocol, not a claim based on the
            // producer's status or pipe EOF. Retain the exact raw archive before admitting it.
            let closed = aterm_uds::runs::completed_archive(&run).ok_or_else(|| {
                not_run("guest verifier did not retain a complete closed run archive")
            })?;
            artifacts.insert(
                "verifier_run_id".into(),
                json!(closed.id.map(|id| id.text())),
            );
        }
    } else if matches!(ask.lane.as_str(), "measure" | "cursor") {
        return Err(not_run("guest measure named no row-level log to retain"));
    }
    if !matches!(ask.lane.as_str(), "measure" | "cursor") {
        let ledgers = perf_ledgers();
        let listed = read(&format!(
            "if [ -d {l} ]; then find {l} -type f | LC_ALL=C sort; fi",
            l = q(&ledgers)
        ))?;
        let listed = String::from_utf8(listed)
            .map_err(|error| not_run(format!("guest perf listing is not UTF-8: {error}")))?;
        let files = listed
            .lines()
            .map(|f| {
                ledger_member(f)
                    .ok_or_else(|| not_run("guest perf returned a path outside its ledger"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !files.is_empty() {
            let args = files.iter().map(|f| q(f)).collect::<Vec<_>>().join(" ");
            let archive = read(&format!("tar -C {} -cf - -- {args}", q(&ledgers)))?;
            let file = format!("{name}.records.tar");
            write(&ask.record.join(&file), &archive)?;
            artifacts.insert("perf_records".into(), json!(file));
        }
    }
    Ok(Value::Object(artifacts))
}

fn lane_args(lane: &str) -> Vec<String> {
    let bin = format!("{ROOT}/aterm.app/Contents/MacOS/aterm");
    match lane {
        "measure" => vec![
            "measure".into(),
            "--app".into(),
            format!("{ROOT}/aterm.app"),
        ],
        "cursor" => vec![
            "measure".into(),
            "--app".into(),
            format!("{ROOT}/aterm.app"),
            "--cursor-pacing".into(),
        ],
        "cat" => vec![
            "perf".into(),
            "cat".into(),
            "--binary".into(),
            bin,
            "--dev-smoke".into(),
        ],
        "compare" => vec![
            "perf".into(),
            "compare".into(),
            "--baseline".into(),
            bin.clone(),
            "--candidate".into(),
            bin,
            "--allow-identical".into(),
            "--dev-smoke".into(),
        ],
        "fabric" => vec![
            "perf".into(),
            "fabric".into(),
            "--profile".into(),
            "shipped".into(),
        ],
        "floors" => vec!["perf".into(), "--binary".into(), bin],
        _ => unreachable!("parse admits only known lanes"),
    }
}

/// Cargo names executable artifacts, including under Trust's per-unit build layout. Refuse
/// missing or conflicting paths instead of selecting an arbitrary executable from the cache.
fn rewind_command(artifacts: &str, control: bool) -> Result<String, Fail> {
    let mut executables =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    for line in artifacts.lines() {
        let Ok(v) = aterm_json::from_str::<Value>(line) else {
            continue;
        };
        if v["reason"].as_str() != Some("compiler-artifact") {
            continue;
        }
        if let (Some(name), Some(exe)) = (v["target"]["name"].as_str(), v["executable"].as_str())
            && exe.starts_with(&format!("{ROOT}/target.noindex/"))
            && relative_member(exe.trim_start_matches('/'))
        {
            executables
                .entry(name.into())
                .or_default()
                .insert(exe.into());
        }
    }
    let one = |name: &str| -> Result<&str, Fail> {
        let paths = executables
            .get(name)
            .filter(|p| p.len() == 1)
            .ok_or_else(|| {
                not_run(format!(
                    "focused rewind needs exactly one Cargo executable for {name}"
                ))
            })?;
        Ok(paths.first().expect("one path").as_str())
    };
    let bench = one("rewind")?;
    let producer = one("fake-tui")?;
    let subject = format!("{ROOT}/aterm.app/Contents/MacOS/aterm");
    if control {
        Ok(format!(
            "{} --throughput-control {} {} {ROOT}/tmp",
            q(bench),
            q(&subject),
            q(producer)
        ))
    } else {
        Ok(format!(
            "env ATERM_REWIND_BIN={} CARGO_BIN_EXE_fake-tui={} {} --filter recording_keeps_parse_throughput",
            q(&subject),
            q(producer),
            q(bench)
        ))
    }
}

fn environment(jobs: u32) -> String {
    // This is the guest's compiler, not an override of the host's flags or toolchain.
    format!(
        "export PATH={ROOT}/toolchain/bin:/usr/bin:/bin:/usr/sbin:/sbin; \
        export TRUST_STAGE2_BIN={ROOT}/toolchain/bin; \
        export RUSTC={ROOT}/toolchain/bin/trustc; \
        export CARGO_TARGET_DIR={ROOT}/target.noindex; \
        export CARGO_BUILD_JOBS={jobs}; \
        export TMPDIR={ROOT}/tmp/; cd {ROOT}/source; "
    )
}

fn take_order(paired: bool) -> Vec<(u32, &'static str)> {
    (1..=TAKES)
        .flat_map(|take| {
            let arms = if !paired {
                vec!["candidate"]
            } else if take % 2 == 1 {
                vec!["baseline", "candidate"]
            } else {
                vec!["candidate", "baseline"]
            };
            arms.into_iter().map(move |arm| (take, arm))
        })
        .collect()
}

fn arm_command(command: &str, arm: &str) -> String {
    if arm == "baseline" {
        command.replace(
            &format!("{ROOT}/aterm.app"),
            &format!("{ROOT}/baseline.app"),
        )
    } else {
        command.to_owned()
    }
}

/// Run the lane and retain a refusal as a record too: unavailable input is never a pass.
pub(crate) fn run(args: &[String]) -> Res {
    if args == ["--help"] || args == ["help"] {
        print!("{USAGE}");
        return Ok(0);
    }
    stop::install();
    let ask = parse(args)?;
    std::fs::create_dir(&ask.record)
        .map_err(|e| format!("{} (must be a new directory): {e}", ask.record.display()))?;
    let result = run_in(&ask);
    if let Err(e) = &result {
        write_json(
            &ask.record.join("incomplete.json"),
            &json!({"schema": "aterm-mac-runtime-v1", "complete": false, "exit": e.code, "reason": e.msg}),
        )?;
    }
    result
}

fn resolve_tools(
    root: &Path,
    home: &Path,
    prefix: &Path,
    path: &std::ffi::OsStr,
    stage: Option<&Path>,
    before: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<aterm_toolchain::Toolchain, Fail> {
    let lock = atpkg::trust_lock::found_for(root, Some(prefix));
    let tools = aterm_toolchain::Toolchain::discover_with_store_guarded(
        stage,
        &aterm_toolchain::rustup_home(std::env::var_os("RUSTUP_HOME").as_deref(), home),
        Some(prefix),
        path,
        aterm_toolchain::pinned_channel(root).as_deref(),
        lock.as_ref(),
        before,
    )
    .map_err(not_run)?;
    if let Some(why) = &tools.lock_refusal {
        return Err(not_run(why));
    }
    if !tools.have_targo() {
        return Err(not_run(
            "the source snapshot's Trust compiler is unavailable",
        ));
    }
    Ok(tools)
}

fn retain_host_log(path: &Path, record: &Path, retired: bool) -> Result<(), Fail> {
    match crate::util::read_artifact(path) {
        Ok(bytes) => {
            write(&record.join("tart-run.log"), &bytes)?;
            write_json(
                &record.join("tart-run-log.json"),
                &json!({
                "schema":"aterm-mac-runtime-host-log-v1", "available":true,
                "path":path, "bytes":bytes.len(), "complete":retired,
                "sha256":aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&bytes))}),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_json(
                &record.join("tart-run-log.json"),
                &json!({
                "schema":"aterm-mac-runtime-host-log-v1", "available":false,
                "path":path, "complete":false, "reason":"launcher log was not created"}),
            )?;
            Err(not_run("host Tart log is unavailable"))
        }
        Err(error) => Err(not_run(format!(
            "retain host Tart log {}: {error}",
            path.display()
        ))),
    }
}

fn run_in(ask: &Ask) -> Res {
    let repo = &paths().repo;
    let dirty = snapshot::git(repo, ["status", "--porcelain"])?;
    if !dirty.is_empty() {
        return Err(Fail::usage(
            "runtime measures committed code: commit the checkout first",
        ));
    }
    let app = std::fs::canonicalize(&ask.app).map_err(|e| format!("{}: {e}", ask.app.display()))?;
    let bin = app.join("Contents/MacOS/aterm");
    let app_sha = crate::windows::sha256_file(&bin)?;
    let baseline = ask
        .baseline
        .as_ref()
        .map(|path| -> Result<_, Fail> {
            let app =
                std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let sha = crate::windows::sha256_file(&app.join("Contents/MacOS/aterm"))?;
            Ok((app, sha))
        })
        .transpose()?;
    let source = snapshot::git(repo, ["rev-parse", "HEAD"])?;
    let submodule = snapshot::astream_pin(repo, &source)?;
    let home = Path::new(&paths().real_home);
    let prefix = aterm_toolchain::atpkg_prefix(
        home,
        std::env::var_os("XDG_CONFIG_HOME")
            .as_deref()
            .map(Path::new),
    );
    let mut compiler_leases = Vec::new();
    let tool = resolve_tools(
        repo,
        home,
        &prefix,
        &std::env::var_os("PATH").unwrap_or_default(),
        std::env::var_os("TRUST_STAGE2_BIN")
            .as_deref()
            .map(Path::new),
        &mut |dir| {
            if let Some(lease) =
                aterm_toolchain::lease::take_for_dir(&prefix, dir, "lab mac runtime")
                    .map_err(|e| format!("compiler lease: {e}"))?
            {
                compiler_leases.push(lease);
            }
            Ok(())
        },
    )?;
    let compiler = std::fs::canonicalize(
        tool.stage2_dir
            .parent()
            .ok_or_else(|| not_run("Trust sysroot has no parent"))?,
    )
    .map_err(|e| format!("Trust sysroot: {e}"))?;
    if !compiler.join("bin/trustc").is_file() {
        return Err(not_run("the resolved Trust sysroot has no bin/trustc"));
    }
    let tart = crate::mac::tart_to_run()?;
    write_json(
        &ask.record.join("inputs.json"),
        &json!({
            "schema": "aterm-mac-runtime-v1", "source_commit": source, "astream_commit": submodule,
            "bundle_sha256": app_sha, "baseline_bundle_sha256": baseline.as_ref().map(|(_, sha)| sha), "image": ask.image, "lane": ask.lane,
            "driver": "gui-launch-agent/App", "count_policy": "qualified", "privilege": "guest-user",
            "compiler": compiler.display().to_string(), "compiler_lock_override": tool.lock_override,
            "guest": {"cpus": ask.cpus, "memory_mib": ask.memory_mib, "disk_gb": ask.disk_gb, "graphics": ask.graphics}
        }),
    )?;
    // The disk bound includes the guest's entire build cache, not just the app and image.
    let df = checked(
        "/bin/df",
        &["-g".into(), "/System/Volumes/Data".into()],
        repo,
    )?;
    if !disk_room(&df, ask.disk_gb) {
        return Err(not_run(format!(
            "runtime needs {} GB free ({} GB guest plus 30 GB host margin): {df}",
            u64::from(ask.disk_gb) + 30,
            ask.disk_gb
        )));
    }
    let mut scratch = VmScratch::new(
        aterm_tempfile::Builder::new()
            .prefix("mac-runtime-")
            .suffix(".noindex")
            .tempdir()
            .map_err(|e| format!("runtime scratch: {e}"))?,
        &ask.record,
    )?;
    let share = scratch.path().join("share");
    std::fs::create_dir(&share).map_err(|e| format!("share: {e}"))?;
    snapshot::archive(repo, &source, &submodule, &share)?;
    checked(
        "/usr/bin/tar",
        &[
            "-cf".into(),
            share.join("toolchain.tar").display().to_string(),
            "-C".into(),
            compiler.display().to_string(),
            ".".into(),
        ],
        repo,
    )?;
    checked(
        "/usr/bin/ditto",
        &[
            "-c".into(),
            "-k".into(),
            "--keepParent".into(),
            app.display().to_string(),
            share.join("app.zip").display().to_string(),
        ],
        repo,
    )?;
    if let Some((app, _)) = &baseline {
        checked(
            "/usr/bin/ditto",
            &[
                "-c".into(),
                "-k".into(),
                "--keepParent".into(),
                app.display().to_string(),
                share.join("baseline.zip").display().to_string(),
            ],
            repo,
        )?;
    }
    let base =
        crate::mac::existing_prepared_base(&tart, &ask.image)?.unwrap_or_else(|| ask.image.clone());
    let df = checked(
        "/bin/df",
        &["-g".into(), "/System/Volumes/Data".into()],
        repo,
    )?;
    if !disk_room(&df, ask.disk_gb) {
        return Err(not_run(format!(
            "after image preparation, runtime needs {} GB free: {df}",
            u64::from(ask.disk_gb) + 30
        )));
    }
    let guest = crate::vm_admit::Guest {
        lane: "mac-runtime",
        vm: "mac-runtime",
        memory: u64::from(ask.memory_mib) << 20,
        cpus: ask.cpus,
    };
    let mut admitted = crate::vm_admit::admit(&guest, true)?;
    let mut vm = Vm::clone_from_recorded(&tart, &base, scratch.path(), &share, false, &ask.record)?;
    let result = (|| -> Result<(i32, Value), Fail> {
        vm.hold_admission(&mut admitted)?;
        vm.graphics = ask.graphics;
        checked(
            &tart,
            &[
                "set".into(),
                vm.name.clone(),
                "--cpu".into(),
                ask.cpus.to_string(),
                "--memory".into(),
                ask.memory_mib.to_string(),
                "--disk-size".into(),
                ask.disk_gb.to_string(),
            ],
            repo,
        )?;
        vm.boot()?;
        require_sip_on(
            &ask.record,
            &SipProbe {
                tart: &tart,
                requested_image: &ask.image,
                selected_base: &base,
                guest: &vm.name,
            },
            || vm.exec_output(SIP_STATUS_COMMAND, crate::mac::STEP),
        )?;
        let mut setup = format!(
            "set -eu; test \"$HOME\" = /Users//admin; mkdir -p {ROOT}/source {ROOT}/toolchain {ROOT}/tmp; \
        tar -xf {share}/source.tar -C {ROOT}/source; \
        tar -xf {share}/toolchain.tar -C {ROOT}/toolchain; \
        ditto -x -k {share}/app.zip {ROOT}/unpack; \
        mv {ROOT}/unpack/{app_name} {ROOT}/aterm.app; \
        {env} test \"$(git rev-parse HEAD)\" = {source}; \
        test \"$(git rev-parse HEAD:vendor/astream)\" = {submodule}; \
        test \"$(git -C vendor/astream rev-parse HEAD)\" = {submodule}; \
        test \"$(shasum -a 256 {ROOT}/aterm.app/Contents/MacOS/aterm | cut -d ' ' -f 1)\" = {app_sha}; \
        {ROOT}/toolchain/bin/trustc -Vv; \
        {ROOT}/toolchain/bin/targo --config {build_dir_config} --unverified build -p aterm-verify; ",
            share = q(crate::mac::GUEST_SHARE),
            app_name = q(app
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Fail::usage("bundle name is not UTF-8"))?),
            env = environment(admitted.jobs),
            source = q(&source),
            submodule = q(&submodule),
            build_dir_config = q(&format!(
                "build.build-dir='{}'",
                aterm_toolchain::build_cache::target_build_path(Path::new(&format!(
                    "{ROOT}/target.noindex"
                )))
                .display()
            ))
        );
        if let Some((app, sha)) = &baseline {
            setup.push_str(&format!(
            "ditto -x -k {share}/baseline.zip {ROOT}/baseline-unpack; \
             mv {ROOT}/baseline-unpack/{name} {ROOT}/baseline.app; \
             test \"$(shasum -a 256 {ROOT}/baseline.app/Contents/MacOS/aterm | cut -d ' ' -f 1)\" = {sha}; ",
            share=q(crate::mac::GUEST_SHARE),
            name=q(app.file_name().and_then(|name| name.to_str()).ok_or_else(|| Fail::usage("baseline bundle name is not UTF-8"))?),
            sha=q(sha),
        ));
        }
        let setup_out = vm.exec_output(&setup, BUILD).map_err(not_run)?;
        write(&ask.record.join("setup.stdout"), &setup_out.stdout)?;
        write(&ask.record.join("setup.stderr"), &setup_out.stderr)?;
        if !setup_out.status.success() {
            return Err(not_run(format!(
                "guest verifier build failed: {}; see setup.stderr",
                setup_out.status
            )));
        }
        // Build measurement benches before taking time: the five takes use the same warm cache.
        let prebuild = format!(
            "{} export CARGO_MANIFEST_DIR={ROOT}/source/crates/aterm-verify; {ROOT}/target.noindex/debug/aterm-verify measure --app {ROOT}/aterm.app --compile-only --admit now",
            environment(admitted.jobs)
        );
        if matches!(ask.lane.as_str(), "measure" | "cursor") {
            let out = vm.exec_output(&prebuild, BUILD).map_err(not_run)?;
            write(&ask.record.join("prebuild.stdout"), &out.stdout)?;
            write(&ask.record.join("prebuild.stderr"), &out.stderr)?;
            if !out.status.success() {
                return Err(not_run(
                    "guest measurement prebuild failed; see prebuild.stderr",
                ));
            }
        }
        let focused = matches!(ask.lane.as_str(), "rewind" | "rewind-control");
        let command = if focused {
            let out = vm.exec_output(&format!("{} {ROOT}/toolchain/bin/targo --unverified bench --profile test -p aterm-conformance --bench rewind --no-run --message-format=json", environment(admitted.jobs)), BUILD).map_err(not_run)?;
            write(&ask.record.join("prebuild.stdout"), &out.stdout)?;
            write(&ask.record.join("prebuild.stderr"), &out.stderr)?;
            if !out.status.success() {
                return Err(not_run("focused rewind build failed; see prebuild.stderr"));
            }
            rewind_command(
                &String::from_utf8_lossy(&out.stdout),
                ask.lane == "rewind-control",
            )?
        } else {
            format!(
                "{ROOT}/target.noindex/debug/aterm-verify {}",
                lane_args(&ask.lane)
                    .iter()
                    .map(|a| q(a))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let untimed = format!(
            "{} export CARGO_MANIFEST_DIR={ROOT}/source/crates/aterm-verify; {command}",
            environment(admitted.jobs)
        );
        // Populate every lane's compiler and corpus caches before measuring. This run is retained,
        // too; a red warm-up is never erased by five later green takes.
        println!(
            "mac runtime: {} warm-up (not one of the five takes)",
            ask.lane
        );
        let paired = baseline.is_some();
        let mut codes = Vec::new();
        let mut warmups = Vec::new();
        for arm in if paired {
            vec!["baseline", "candidate"]
        } else {
            vec!["candidate"]
        } {
            let name = if paired {
                format!("warmup-{arm}")
            } else {
                "warmup".into()
            };
            let (warm, artifacts) = app_exec(&vm, ask, &name, &arm_command(&untimed, arm), BUILD)
                .map_err(|error| preserve_failure(&codes, error))?;
            let row = json!({"arm": arm, "exit": warm.status.code(), "success": warm.status.success(), "artifacts": artifacts});
            write_json(&ask.record.join(format!("{name}.json")), &row)?;
            warmups.push(row);
            codes.push(warm.status.code());
        }
        let invocation = format!(
            "{} export CARGO_MANIFEST_DIR={ROOT}/source/crates/aterm-verify; /usr/bin/time -lp {command}",
            environment(admitted.jobs)
        );
        let mut rows = Vec::new();
        let mut real = Vec::new();
        let mut cpu = Vec::new();
        for (take, arm) in take_order(paired) {
            stop::check()?;
            let sha = if arm == "baseline" {
                &baseline.as_ref().expect("paired baseline").1
            } else {
                &app_sha
            };
            let name = if paired {
                format!("take-{take}-{arm}")
            } else {
                format!("take-{take}")
            };
            let selected = arm_command(&invocation, arm);
            println!(
                "mac runtime: {} take {take}/{TAKES} {arm}, bundle {sha}",
                ask.lane
            );
            write_json(
                &ask.record.join(format!("{name}.context.json")),
                &json!({"take":take, "arm":arm, "bundle_sha256":sha, "command":selected}),
            )?;
            let (out, loads) = sampled(|| app_exec(&vm, ask, &name, &selected, TAKE));
            write_json(
                &ask.record.join(format!("{name}.host-load.json")),
                &json!({"host_load_samples":loads, "host_max_load_1m":max_load(&loads)}),
            )?;
            let (out, artifacts) = out.map_err(|error| preserve_failure(&codes, error))?;
            codes.push(out.status.code());
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let resource = usage(&stderr);
            if let Some(n) = resource["real"].as_f64() {
                real.push(n);
            }
            if let (Some(u), Some(s)) = (resource["user"].as_f64(), resource["sys"].as_f64()) {
                cpu.push(u + s);
            }
            rows.push(
            json!({"take": take, "arm": arm, "bundle_sha256": sha, "command": selected, "exit": out.status.code(), "success": out.status.success(),
            "usage": resource, "artifacts": artifacts, "host_load_samples": loads, "host_max_load_1m": max_load(&loads),
            "stdout": stdout.as_ref(), "stderr": stderr.as_ref()}),
        );
            write_json(
                &ask.record.join(format!("{name}.json")),
                rows.last().expect("just pushed"),
            )?;
        }
        let exit = experiment_exit(&codes, paired);
        Ok((
            exit,
            json!({"schema": if paired { "aterm-mac-runtime-paired-v1" } else { "aterm-mac-runtime-v1" },
        "complete": true, "all_exits_zero": exit == 0, "exit": exit,
        "warmup_exit": if paired { None } else { codes[0] },
        "warmup_artifacts": if paired { Value::Null } else { warmups[0]["artifacts"].clone() },
        "warmups": warmups, "takes_per_arm": TAKES, "source_commit": source, "astream_commit": submodule,
        "bundle_sha256": app_sha, "baseline_bundle_sha256": baseline.as_ref().map(|(_, sha)| sha), "image": ask.image, "lane": ask.lane,
        "driver": "gui-launch-agent/App", "count_policy": "qualified",
        "guest": {"cpus": ask.cpus, "memory_mib": ask.memory_mib, "disk_gb": ask.disk_gb, "graphics": ask.graphics},
        "granted_share": admitted.jobs,
        "command": command, "wall_s": spread(&real), "cpu_s": spread(&cpu), "rows": rows}),
        ))
    })();
    let observed = result.as_ref().ok().map(|(code, _)| *code);
    let result = after_cleanup(result, || {
        let finished = vm.finish().map(|_| ());
        // Archival is attempted even when checked VM retirement failed. The retained original
        // scratch remains authoritative when the log is still mutable or could not be copied.
        let retired = finished.is_ok();
        after_cleanup(finished, || {
            retain_host_log(vm.log_path(), &ask.record, retired)
        })
    })
    .map_err(|error| match observed {
        Some(code) => preserve_failure(&[Some(code)], error),
        None => error,
    });
    let (exit, mut rows) = result?;
    let fields = rows.as_object_mut().expect("runtime rows are an object");
    fields.insert("vm_receipt".into(), json!(vm.receipt_path()));
    fields.insert("vm_retirement".into(), vm.receipt());
    write_json(&ask.record.join("rows.json"), &rows)?;
    scratch.discard_after_finish();
    Ok(exit)
}

#[cfg(test)]
#[path = "mac_runtime_tests.rs"]
mod tests;
