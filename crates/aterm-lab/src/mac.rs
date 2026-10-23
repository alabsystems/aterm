// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disposable macOS guest VM lane via `tart` ([`IMAGE`], [`ROW`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};

use crate::instance::{take_flag, take_value};
use crate::paths::paths;
use crate::{Fail, LAB, Res, stop};

/// Exit status of a lane that could not run: never a pass.
pub const NOT_RUN: i32 = 3;

/// The image a clone is made from: macOS 27 with the Tart Guest Agent (`tart exec`) and no Xcode.
pub const IMAGE: &str = "ghcr.io/cirruslabs/macos-golden-gate-base:latest";

/// Where tart lives when `$LAB_TART` names none (Homebrew's prefixes).
const TART_PATHS: [&str; 2] = ["/opt/homebrew/bin/tart", "/usr/local/bin/tart"];

/// Each bench: its package, and the name it is shared and run by in the guest.
pub const BENCHES: [(&str, &str); 4] = [
    ("atpkg", "atpkg-dmg"),
    ("aterm-containment", "containment-dmg"),
    ("aterm-uds", "uds-dmg"),
    ("aterm-update", "update-dmg"),
];

/// A boot until the guest agent answers.
const BOOT: Duration = Duration::from_secs(300);
/// One bench in the guest: a hang detector.
const ROW: Duration = Duration::from_secs(300);
/// One short guest command (a copy, a status read).
const STEP: Duration = Duration::from_secs(120);
/// Timeout before a `~/Desktop` control read counts as parked on a consent prompt.
const CONTROL: Duration = Duration::from_secs(15);

/// The guest's view of the share.
const GUEST_SHARE: &str = "/Volumes/My Shared Files/lane";
const GUEST_DIR: &str = "/tmp/aterm-mac-lane";

/// The guest agent's executable, the subject tccd names for what `tart exec` runs.
const GUEST_AGENT: &str = "/opt/homebrew/bin/tart-guest-agent";

pub const USAGE: &str = "\
targo --unverified lab mac <verb> — the macOS guest lane: a disposable tart VM (Apple's Virtualization framework)
  dmg [--image REF] [--fda] [--keep]
                the disk-image acceptance in a fresh guest: atpkg's, aterm-containment's, aterm-uds' and
                aterm-update's dmg_acceptance benches, built here with --no-run (nothing is attached on this
                Mac); a clone of REF ($LAB_MAC_IMAGE, else the macOS 27 base image; tart pulls it first, ~35 GB)
                with SIP turned on (csrutil clear, the guest's own administrator) and, --fda, Full Disk Access
                granted to the guest agent; booted with no window, each bench run in the guest with tccd's log
                streamed; prints every row, the removable-volume requests tccd saw and a ~/Desktop control; the
                clone deleted (--keep: stopped and kept as aterm-mac-dmg-kept-<pid>; a killed run's clone is
                reported by the next and retained); ~5 min with the image here. Exit 0 every row passed, 1 a row failed, 3 NOT
                RUN (a row the guest refused, a guest that did not boot, or no tart: https://tart.run, or
                $LAB_TART=/path/to/tart)
";

/// `mac <verb>`.
///
/// # Errors
/// Usage, or the lane could not run.
pub fn cmd_mac(args: &[String]) -> Res {
    match args.first().map(String::as_str) {
        Some("dmg") => cmd_dmg(&args[1..]),
        None | Some("help" | "-h" | "--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some(other) => Err(Fail::usage(format!(
            "mac: unknown verb '{other}' ({LAB} mac help)"
        ))),
    }
}

/// A lane that cannot run here: `aterm-lab: NOT RUN: <why>`, exit 3.
fn not_run(why: impl std::fmt::Display) -> Fail {
    Fail::exit(NOT_RUN, format!("NOT RUN: {why}"))
}

/// The `tart` to run.
///
/// # Errors
/// A relative `named` (usage), or none that runs (NOT RUN).
pub fn pick_tart(named: Option<&str>, executable: impl Fn(&str) -> bool) -> Result<String, Fail> {
    match named {
        Some(p) if !p.starts_with('/') => Err(Fail::usage(format!(
            "LAB_TART={p} must be an absolute path"
        ))),
        Some(p) if executable(p) => Ok(p.to_owned()),
        Some(p) => Err(not_run(format!("LAB_TART={p} is not an executable tart"))),
        None => TART_PATHS
            .into_iter()
            .find(|p| executable(p))
            .map(str::to_owned)
            .ok_or_else(|| not_run("no tart (https://tart.run, or LAB_TART=/path/to/tart)")),
    }
}

/// The benches' executables, by package, from cargo's `--message-format=json` lines.
#[must_use]
pub fn bench_executables(json_lines: &str) -> BTreeMap<String, PathBuf> {
    json_lines
        .lines()
        .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
        .filter(|v| {
            v.get("reason").and_then(aterm_json::Value::as_str) == Some("compiler-artifact")
        })
        .filter(|v| {
            v.get("target")
                .and_then(|t| t.get("name"))
                .and_then(aterm_json::Value::as_str)
                == Some("dmg_acceptance")
        })
        .filter_map(|v| {
            let exe = v.get("executable").and_then(aterm_json::Value::as_str)?;
            let id = v.get("package_id").and_then(aterm_json::Value::as_str)?;
            let (path, fragment) = id.split_once('#')?;
            let package = match fragment.split_once('@') {
                Some((name, _)) => name,
                None => path.rsplit('/').next()?,
            };
            Some((package.to_owned(), PathBuf::from(exe)))
        })
        .collect()
}

/// A bench's verdict in the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every row it planned passed.
    Pass,
    /// A row failed or the bench ended without its `measure result:` line.
    Fail,
    /// No row failed, but the guest refused one (`COULD NOT RUN`).
    NotRun,
}

/// The words a row's panic opens with when the host refused it (`aterm_uds::host::could_not_run`).
const REFUSED_PANIC: &str = "measurement panicked: aterm-gate: COULD NOT RUN";

/// A bench's report read ([`Verdict`]).
#[must_use]
pub fn read_report(out: &str) -> (Vec<&str>, Verdict) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Row {
        Pass,
        Fail,
        Refused,
    }
    let mut shown = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut ended = false;
    let (mut detail, mut first) = (false, false);
    for line in out.lines() {
        if let Some(row) = line.strip_prefix("measure row: ") {
            let status = if row.ends_with(" PASS") {
                Row::Pass
            } else if row.ends_with(" COULD NOT RUN") {
                Row::Refused
            } else {
                Row::Fail
            };
            rows.push(status);
            shown.push(line);
            (detail, first) = (status != Row::Pass, status == Row::Fail);
        } else if line.starts_with("measure result: ") {
            ended = true;
            shown.push(line);
            detail = false;
        } else if line.starts_with("measure ") {
            shown.push(line);
        } else if detail {
            if first
                && line.starts_with(REFUSED_PANIC)
                && let Some(last) = rows.last_mut()
            {
                *last = Row::Refused;
            }
            first = false;
            shown.push(line);
        }
    }
    let verdict = if !ended || rows.contains(&Row::Fail) {
        Verdict::Fail
    } else if rows.is_empty() || rows.contains(&Row::Refused) {
        Verdict::NotRun
    } else {
        Verdict::Pass
    };
    (shown, verdict)
}

/// Names that may belong to an ended lane.
#[must_use]
pub fn ended_lane_candidates(names: &str, me: i32, alive: impl Fn(i32) -> bool) -> Vec<String> {
    names
        .lines()
        .map(str::trim)
        .filter(|name| {
            name.strip_prefix(LANE_VM)
                .filter(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|pid| pid.parse::<i32>().ok())
                .is_some_and(|pid| pid != me && !alive(pid))
        })
        .map(str::to_owned)
        .collect()
}

/// The name every lane clone is made under, before its lane's pid.
const LANE_VM: &str = "aterm-mac-dmg-";

/// What tccd's log says, from its `Handling access request to <service>, from Sub:{…}` lines.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TccSeen {
    pub removable: Vec<String>,
    pub desktop: usize,
}

/// [`TccSeen`] over the log's lines (ndjson, one event per line).
#[must_use]
pub fn tcc_seen(log: &str) -> TccSeen {
    const HANDLING: &str = "Handling access request to kTCCService";
    let mut seen = TccSeen::default();
    for line in log.lines() {
        let Some(at) = line.find(HANDLING) else {
            continue;
        };
        let rest = &line[at + HANDLING.len()..];
        if rest.starts_with("SystemPolicyRemovableVolumes") {
            let subject = rest
                .split_once("from Sub:{")
                .and_then(|(_, s)| s.split_once('}'))
                .map_or("?", |(s, _)| s);
            seen.removable.push(subject.replace("\\/", "/"));
        } else if rest.starts_with("SystemPolicyDesktopFolder") {
            seen.desktop += 1;
        }
    }
    seen
}

/// The control's verdict.
pub const fn control_verdict(
    fda: bool,
    desktop_read_passed: bool,
    seen: &TccSeen,
) -> Result<&'static str, &'static str> {
    match (fda, desktop_read_passed, seen.desktop > 0) {
        (false, false, true) => Ok(
            "a ~/Desktop read was asked of tccd and waits on its question: file consent is enforced",
        ),
        (true, true, _) => Ok("a ~/Desktop read passed on the Full Disk Access grant"),
        (false, _, false) => Err(
            "tccd saw no request for a ~/Desktop read: file consent is not enforced here, so no count of requests means anything",
        ),
        (false, true, true) => Err("a ~/Desktop read passed without a grant"),
        (true, false, _) => Err(
            "a ~/Desktop read did not pass under the Full Disk Access grant: the grant did not take",
        ),
    }
}

/// One guest, from clone to delete.
struct Vm {
    tart: String,
    name: String,
    share: PathBuf,
    log: PathBuf,
    run: Option<Child>,
    keep: bool,
}

impl Vm {
    /// `tart clone <image> <name>` (tart pulls the image first when this Mac has none).
    fn clone_from(tart: &str, image: &str, share: &Path, keep: bool) -> Result<Vm, Fail> {
        let name = format!("{LANE_VM}{}", std::process::id());
        let status = aterm_uds::spawnfd::command(tart)
            .args(["clone", image, &name])
            .stdin(Stdio::null())
            .status()
            .map_err(|e| not_run(format!("tart clone: {e}")))?;
        if !status.success() {
            return Err(not_run(format!("tart clone {image} {name}: {status}")));
        }
        Ok(Vm {
            tart: tart.to_owned(),
            log: share.join("tart-run.log"),
            share: share.to_path_buf(),
            name,
            run: None,
            keep,
        })
    }

    /// Boot it with no window, the share read-only, and wait for the guest agent.
    fn boot(&mut self) -> Result<(), Fail> {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .map_err(|e| format!("{}: {e}", self.log.display()))?;
        let err = log.try_clone().map_err(|e| format!("tart run log: {e}"))?;
        let child = aterm_uds::spawnfd::command(&self.tart)
            .args(["run", "--no-graphics", "--no-audio", "--no-clipboard"])
            .arg(format!("--dir=lane:{}:ro", self.share.display()))
            .arg(&self.name)
            .setsid(true)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| not_run(format!("tart run: {e}")))?;
        self.run = Some(child);
        let start = Instant::now();
        while start.elapsed() < BOOT {
            stop::check()?;
            if self
                .exec("true", Duration::from_secs(15))
                .is_ok_and(|o| o.status.success())
            {
                return Ok(());
            }
            stop::sleep(Duration::from_secs(3))?;
        }
        Err(not_run(format!(
            "the guest agent did not answer within {} s of boot (an image without the Tart Guest Agent cannot run the lane; tart run's log: {})",
            BOOT.as_secs(),
            self.log.display()
        )))
    }

    /// Run `script` with `/bin/sh -c` in the guest, as its console user, until `limit`.
    fn exec(&self, script: &str, limit: Duration) -> Result<std::process::Output, String> {
        aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(&self.tart)
                .args(["exec", &self.name, "/bin/sh", "-c", script]),
            Instant::now() + limit,
        )
        .map_err(|e| format!("tart exec: {e}"))
    }

    /// [`Vm::exec`], its stdout on success, else the failure with what it said.
    fn sh(&self, script: &str, limit: Duration) -> Result<String, Fail> {
        let out = self.exec(script, limit)?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() {
            Ok(text)
        } else {
            Err(Fail::die(format!(
                "guest `{script}`: {}: {text}{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }

    /// `tart stop`, and the `tart run` it ends reaped.
    fn stop(&mut self) {
        if let Some(mut child) = self.run.take() {
            let _ = aterm_uds::spawnfd::command(&self.tart)
                .args(["stop", &self.name])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.wait();
        }
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        self.stop();
        if self.keep {
            let kept = format!("{LANE_VM}kept-{}", std::process::id());
            let renamed = aterm_uds::spawnfd::command(&self.tart)
                .args(["rename", &self.name, &kept])
                .stdin(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            let name = if renamed { &kept } else { &self.name };
            println!("mac dmg: kept the stopped VM {name} (tart delete {name})");
        } else {
            let _ = aterm_uds::spawnfd::command(&self.tart)
                .args(["delete", &self.name])
                .stdin(Stdio::null())
                .status();
        }
    }
}

/// Report possible leftovers without mutating VMs this run did not create.
fn report_ended_lanes(tart: &str) {
    let Ok(out) = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command(tart)
            .args(["list", "--source", "local", "--quiet"])
            .stdin(Stdio::null()),
        Instant::now() + STEP,
    ) else {
        return;
    };
    let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    for name in ended_lane_candidates(&String::from_utf8_lossy(&out.stdout), me, crate::sys::alive)
    {
        println!(
            "mac dmg: retained {name}; its lane pid is absent, but its ownership is unconfirmed"
        );
    }
}

/// Whether `csrutil status` reports SIP enabled.
fn sip_on(status: &str) -> bool {
    status.contains("status: enabled") && !status.contains("disabled")
}

/// Reap `child` within `limit`, killing it when it has not ended by then.
fn reap_within(child: &mut Child, limit: Duration) {
    let start = Instant::now();
    while start.elapsed() < limit {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Enable SIP via `csrutil clear` in the guest before rebooting.
const SIP_ON: &str = "/usr/bin/expect -c 'set timeout 30; spawn sudo -n csrutil clear; \
                      expect \"user:\"; send \"admin\\r\"; expect \"assword\"; send \"admin\\r\"; expect eof'";

/// Grant Full Disk Access to the guest agent in the system TCC database while SIP is off.
fn fda_grant() -> String {
    format!(
        "set -e; a=$(readlink -f {GUEST_AGENT}); \
         codesign -d -r- \"$a\" 2>&1 | sed -n 's/^designated => //p' | csreq -r- -b /tmp/agent.csreq; \
         hex=$(xxd -p /tmp/agent.csreq | tr -d '\\n'); \
         sudo -n sqlite3 '/Library/Application Support/com.apple.TCC/TCC.db' \
         \"INSERT OR REPLACE INTO access (service,client,client_type,auth_value,auth_reason,auth_version,csreq,flags) \
         VALUES ('kTCCServiceSystemPolicyAllFiles','$a',1,2,4,1,X'$hex',0);\""
    )
}

/// `mac dmg`: the `dmg_acceptance` benches, built here and run in a fresh guest with SIP on and
/// tccd's log streamed, then a `~/Desktop` read as the control.
///
/// # Errors
/// Usage, or the lane could not run.
pub fn cmd_dmg(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let image = take_value(&mut a, "--image")?
        .or_else(|| {
            std::env::var("LAB_MAC_IMAGE")
                .ok()
                .filter(|i| !i.is_empty())
        })
        .unwrap_or_else(|| IMAGE.to_owned());
    let fda = take_flag(&mut a, "--fda");
    let keep = take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "mac dmg: unexpected '{extra}' ({LAB} mac help)"
        )));
    }
    let named = std::env::var("LAB_TART").ok().filter(|p| !p.is_empty());
    let tart = pick_tart(named.as_deref(), crate::sys::executable)?;
    report_ended_lanes(&tart);

    let repo = &paths().repo;
    let targo = std::env::var_os("CARGO").unwrap_or_else(|| "targo".into());
    let mut build = aterm_uds::spawnfd::command(&targo);
    build
        .args(["--unverified", "bench", "--profile", "test"])
        .args(BENCHES.iter().flat_map(|(package, _)| ["-p", *package]))
        .args([
            "--bench",
            "dmg_acceptance",
            "--no-run",
            "--message-format=json",
        ])
        .current_dir(repo)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    let built = build
        .output()
        .map_err(|e| Fail::die(format!("targo: {e}")))?;
    if !built.status.success() {
        return Err(Fail::die(format!(
            "the benches did not build: {}",
            built.status
        )));
    }
    let exes = bench_executables(&String::from_utf8_lossy(&built.stdout));
    let runs = paths().state.join("mac-dmg");
    std::fs::create_dir_all(&runs).map_err(|e| format!("{}: {e}", runs.display()))?;
    let share_guard = aterm_tempfile::Builder::new()
        .prefix("run-")
        .tempdir_in(&runs)
        .map_err(|e| format!("{}: {e}", runs.display()))?;
    let share = share_guard.path();
    for (package, name) in BENCHES {
        let exe = exes.get(package).ok_or_else(|| {
            Fail::die(format!(
                "the build named no dmg_acceptance bench of {package}"
            ))
        })?;
        std::fs::copy(exe, share.join(name)).map_err(|e| format!("{}: {e}", exe.display()))?;
    }

    println!("mac dmg: cloning {image} (tart pulls it first when this Mac has no copy)");
    let mut vm = Vm::clone_from(&tart, &image, share, keep)?;
    vm.boot()?;
    if vm.sh("csrutil status", STEP)?.contains("disabled") {
        if fda {
            vm.sh(&fda_grant(), STEP)?;
            println!("mac dmg: Full Disk Access granted to the guest agent");
        }
        vm.sh(SIP_ON, STEP)?;
        vm.stop();
        vm.boot()?;
    } else if fda {
        return Err(not_run(
            "--fda needs an image with SIP off: the grant is written to the TCC database before SIP is turned on",
        ));
    }
    let sip = vm.sh("csrutil status", STEP)?;
    println!("mac dmg: {} {}", vm.name, sip.trim());
    if !sip_on(&sip) {
        return Err(not_run(format!(
            "SIP is not on in the guest: {}",
            sip.trim()
        )));
    }

    vm.sh(
        &format!(
            "rm -rf {GUEST_DIR} && mkdir -p {GUEST_DIR} && cp '{GUEST_SHARE}'/*-dmg {GUEST_DIR}/"
        ),
        STEP,
    )?;
    let mut stream = aterm_uds::spawnfd::command(&tart)
        .args(["exec", &vm.name, "sudo", "-n", "/bin/sh", "-c"])
        .arg(format!(
            "exec /usr/bin/log stream --info --style ndjson --predicate 'process == \"tccd\"' > {GUEST_DIR}/tccd.ndjson 2>&1"
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Fail::die(format!("tccd's log: {e}")))?;
    stop::sleep(Duration::from_secs(2))?;
    let (mut failed, mut refused) = (0, 0);
    for (_, name) in BENCHES {
        stop::check()?;
        let out = vm.exec(&format!("cd {GUEST_DIR} && ./{name}"), ROW);
        let text = match &out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
            Err(why) => why.clone(),
        };
        let (shown, verdict) = read_report(&text);
        for line in shown {
            println!("mac dmg: {line}");
        }
        match verdict {
            Verdict::Pass => {}
            Verdict::NotRun => refused += 1,
            Verdict::Fail => {
                failed += 1;
                if !text.lines().any(|l| l.starts_with("measure result: ")) {
                    println!("mac dmg: {name} did not run to its end: {}", tail(&text));
                }
            }
        }
    }

    let control = vm.exec("ls ~/Desktop >/dev/null", CONTROL);
    let desktop_read_passed = control.is_ok_and(|o| o.status.success());
    let _ = vm.exec("sudo -n pkill -INT -f '^/usr/bin/log stream'", STEP);
    reap_within(&mut stream, STEP);
    let log = vm.sh(
        &format!("grep -h 'Handling access request to kTCCServiceSystemPolicy' {GUEST_DIR}/tccd.ndjson || true"),
        STEP,
    )?;
    let seen = tcc_seen(&log);
    println!(
        "mac dmg: tccd: {} SystemPolicyRemovableVolumes request(s){}",
        seen.removable.len(),
        seen.removable
            .iter()
            .map(|s| format!("\n  from {s}"))
            .collect::<String>()
    );
    let counted = match control_verdict(fda, desktop_read_passed, &seen) {
        Ok(why) => {
            println!("mac dmg: control: {why}");
            ""
        }
        Err(why) => {
            println!("mac dmg: control FAILED: {why}");
            "; the tccd count proves nothing (control FAILED)"
        }
    };
    drop(vm);
    let benches = BENCHES.len();
    if failed > 0 {
        println!("mac dmg: FAIL ({failed} of {benches} benches){counted}");
        Ok(1)
    } else if refused > 0 {
        println!("mac dmg: NOT RUN ({refused} of {benches} benches refused by the guest){counted}");
        Ok(NOT_RUN)
    } else {
        println!("mac dmg: PASS ({benches} benches){counted}");
        Ok(0)
    }
}

/// The last lines of `text`, for a row that ended without its verdict.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(8)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tart_is_the_named_one_alone_else_homebrews() {
        let none = |_: &str| false;
        let all = |_: &str| true;
        assert_eq!(pick_tart(Some("/x/tart"), all).unwrap(), "/x/tart");
        assert_eq!(pick_tart(Some("rel/tart"), all).unwrap_err().code, 2);
        assert_eq!(pick_tart(Some("/x/tart"), none).unwrap_err().code, NOT_RUN);
        assert_eq!(pick_tart(None, all).unwrap(), TART_PATHS[0]);
        assert_eq!(
            pick_tart(None, |p: &str| p == TART_PATHS[1]).unwrap(),
            TART_PATHS[1]
        );
        assert_eq!(pick_tart(None, none).unwrap_err().code, NOT_RUN);
    }

    #[test]
    fn only_the_dmg_acceptance_executables_are_taken_by_package() {
        let lines = [
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/atpkg#0.107.0","target":{"name":"dmg_acceptance"},"executable":"/b/deps/dmg_acceptance-1"}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/atpkg#0.107.0","target":{"name":"atpkg"},"executable":"/b/deps/atpkg-9"}"#,
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/aterm-uds#0.107.0","target":{"name":"dmg_acceptance"},"executable":null}"#,
            r#"{"reason":"build-finished","success":true}"#,
            "not json",
            r#"{"reason":"compiler-artifact","package_id":"path+file:///w/crates/aterm-update#0.107.0","target":{"name":"dmg_acceptance"},"executable":"/b/deps/dmg_acceptance-4"}"#,
        ]
        .join("\n");
        let exes = bench_executables(&lines);
        assert_eq!(
            exes.into_iter().collect::<Vec<_>>(),
            [
                (
                    "aterm-update".to_owned(),
                    PathBuf::from("/b/deps/dmg_acceptance-4")
                ),
                (
                    "atpkg".to_owned(),
                    PathBuf::from("/b/deps/dmg_acceptance-1")
                ),
            ]
        );
    }

    #[test]
    fn a_package_is_named_by_its_id_whatever_its_folder() {
        let line = |id: &str| {
            format!(
                r#"{{"reason":"compiler-artifact","package_id":"{id}","target":{{"name":"dmg_acceptance"}},"executable":"/b/x"}}"#
            )
        };
        let exes = bench_executables(&line("path+file:///w/crates/uds#aterm-uds@0.107.0"));
        assert_eq!(exes.keys().collect::<Vec<_>>(), ["aterm-uds"]);
    }

    #[test]
    fn a_bench_passes_only_on_its_own_complete_report_and_says_why_a_row_did_not() {
        let pass = "measure row: x::a PASS\nall ran\nmeasure row: x::b PASS\nok\n\
                    measure result: x planned=2 passed=2 failed=0 refused=0\n";
        let (shown, verdict) = read_report(pass);
        assert_eq!(verdict, Verdict::Pass);
        assert_eq!(
            shown,
            [
                "measure row: x::a PASS",
                "measure row: x::b PASS",
                "measure result: x planned=2 passed=2 failed=0 refused=0"
            ]
        );

        let fail = "measure row: x::a FAIL\nmeasurement panicked: left 1 right 2\nat x.rs\n\
                    measure row: x::b PASS\nok\nmeasure result: x planned=2 passed=1 failed=1 refused=0\n";
        let (shown, verdict) = read_report(fail);
        assert_eq!(verdict, Verdict::Fail);
        assert_eq!(
            shown[..3],
            [
                "measure row: x::a FAIL",
                "measurement panicked: left 1 right 2",
                "at x.rs"
            ]
        );

        let typed = "measure row: x::a COULD NOT RUN\nno hdiutil\n\
                     measure result: x planned=1 passed=0 failed=0 refused=1\n";
        assert_eq!(read_report(typed).1, Verdict::NotRun);
        assert_eq!(read_report(typed).0[1], "no hdiutil");
        let panicked = "measure row: x::a FAIL\n\
                        measurement panicked: aterm-gate: COULD NOT RUN — trustd did not answer\n\
                        measure result: x planned=1 passed=0 failed=1 refused=0\n";
        assert_eq!(read_report(panicked).1, Verdict::NotRun);
        let both = format!(
            "{}{fail}",
            typed.replace(
                "measure result: x planned=1 passed=0 failed=0 refused=1\n",
                ""
            )
        );
        assert_eq!(read_report(&both).1, Verdict::Fail);
        let later = "measure row: x::a FAIL\nleft != right\n\
                     measurement panicked: aterm-gate: COULD NOT RUN — quoted\n\
                     measure result: x planned=1 passed=0 failed=1 refused=0\n";
        assert_eq!(read_report(later).1, Verdict::Fail);

        assert_eq!(read_report("measure row: x::a PASS\n").1, Verdict::Fail);
        assert_eq!(read_report("tart exec: timed out").1, Verdict::Fail);
        let empty = "measure result: x planned=0 passed=0 failed=0 refused=0\n";
        assert_eq!(read_report(empty).1, Verdict::NotRun);
    }

    #[test]
    fn ended_lane_candidates_exclude_live_and_kept_names() {
        let names = "aterm-mac-dmg-100\naterm-mac-dmg-200\naterm-mac-dmg-300\n\
                     aterm-mac-dmg-kept-100\naterm-mac-dmg-\naterm-mac-dmg-1x\nother-vm\n";
        let alive = |pid: i32| pid == 200;
        assert_eq!(
            ended_lane_candidates(names, 300, alive),
            ["aterm-mac-dmg-100"]
        );
        assert_eq!(ended_lane_candidates("", 300, alive), Vec::<String>::new());
    }

    #[test]
    fn sip_is_on_only_when_wholly_enabled() {
        assert!(sip_on("System Integrity Protection status: enabled.\n"));
        assert!(!sip_on("System Integrity Protection status: disabled.\n"));
        assert!(!sip_on(
            "System Integrity Protection status: enabled (Custom Configuration).\n\n\
             Configuration:\n\tFilesystem Protections: disabled\n"
        ));
    }

    #[test]
    fn tccd_lines_count_removable_requests_by_subject_and_desktop_requests() {
        let log = [
            r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyRemovableVolumes, from Sub:{com.aterm.aterm}Resp:{TCCDProcess: …}"}"#,
            r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyDesktopFolder, from Sub:{\/opt\/homebrew\/bin\/tart-guest-agent}Resp:{…}"}"#,
            r#"{"eventMessage":"Handling access request to kTCCServiceSystemPolicyAllFiles, from Sub:{com.aterm.aterm}Resp:{…}"}"#,
            r#"{"eventMessage":"AUTHREQ_CTX: service=kTCCServiceSystemPolicyRemovableVolumes"}"#,
        ]
        .join("\n");
        assert_eq!(
            tcc_seen(&log),
            TccSeen {
                removable: vec!["com.aterm.aterm".to_owned()],
                desktop: 1,
            }
        );
        assert_eq!(tcc_seen(""), TccSeen::default());
    }

    #[test]
    fn a_count_means_something_only_where_the_control_shows_consent_enforced() {
        let asked = TccSeen {
            removable: Vec::new(),
            desktop: 1,
        };
        let silent = TccSeen::default();
        assert!(control_verdict(false, false, &asked).is_ok());
        assert!(control_verdict(true, true, &silent).is_ok());
        assert!(control_verdict(false, true, &silent).is_err());
        assert!(control_verdict(false, false, &silent).is_err());
        assert!(control_verdict(false, true, &asked).is_err());
        assert!(control_verdict(true, false, &asked).is_err());
    }
}
