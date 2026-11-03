// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Two real Trust builds retain independent covering claims through the managed shim.
//! Scratch admission uses `now`: this checks memory-claim lifetimes, not memory waiting or
//! CPU allocation. The debug lab's own image serves the hop so its scratch-ledger seam is known.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use aterm_capacity::{Ledger, Record};
use aterm_uds::process::{Identity, read_identity, read_process_parent, read_process_path};
use aterm_uds::spawnfd::{Child, Stdio, command};

use crate::{Fail, Res, stop};

#[path = "build_admit_capture.rs"]
mod capture;

const USAGE: &str = "build-admit --targo PATH [--keep] [--sample-after SECONDS (0..59)]";
const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
static HARD_RELEASES: OnceLock<[PathBuf; 2]> = OnceLock::new();

fn release_on_hard_exit() {
    let _capture = capture::lock();
    if let Some(paths) = HARD_RELEASES.get() {
        for path in paths {
            let _ = fs::write(path, "go\n");
        }
    }
}
// A file written by the newly linked script proves it ran before its directory can go.
// The explicit release, rather than a sleep, holds the two builds at the observation point.
const SCRIPT: &str = r#"
fn main() {
    let ready = std::env::var("BC_HOP_READY").unwrap();
    let release = std::env::var("BC_HOP_RELEASE").unwrap();
    std::fs::write(ready, format!("{}\n", std::process::id())).unwrap();
    while !std::path::Path::new(&release).exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
"#;

struct Args {
    targo: PathBuf,
    keep: bool,
    sample_after: Option<Duration>,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let usage = || Fail::usage(format!("usage: {USAGE}"));
    let mut targo = None;
    let mut keep = false;
    let mut sample_after = None;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        match word.as_str() {
            "--targo" if targo.is_none() => {
                let path = words.next().ok_or_else(usage)?;
                targo = Some(PathBuf::from(crate::paths::from_caller(path)));
            }
            "--keep" if !keep => keep = true,
            "--sample-after" if sample_after.is_none() => {
                let seconds = words.next().ok_or_else(usage)?;
                sample_after = Some(sample_delay(seconds).ok_or_else(usage)?);
            }
            _ => return Err(usage()),
        }
    }
    let targo = fs::canonicalize(targo.ok_or_else(usage)?)
        .map_err(|e| Fail::die(format!("build-admit: resolve targo: {e}")))?;
    if targo.file_name().is_none_or(|name| name != "targo") {
        return Err(Fail::usage(
            "build-admit: --targo must name Trust's actual targo image",
        ));
    }
    Ok(Args {
        targo,
        keep,
        sample_after,
    })
}

fn sample_delay(text: &str) -> Option<Duration> {
    let seconds = text.parse::<u64>().ok()?;
    (seconds < HANG.as_secs()).then(|| Duration::from_secs(seconds))
}

struct Build {
    child: Child,
    ready: PathBuf,
    release: PathBuf,
    log: PathBuf,
    witness: Option<Witness>,
    status: Option<ExitStatus>,
    sampled: bool,
}

#[derive(Clone, Copy)]
struct Witness {
    front: Identity,
    script: Identity,
}

struct Fixture {
    root: PathBuf,
    ledger: Ledger,
    builds: Vec<Build>,
    keep: bool,
    sample_after: Option<Duration>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Output can unwind before explicit cleanup. Direct writes release the scripts without
        // recreating scratch which successful cleanup already removed.
        for build in &self.builds {
            let _ = fs::write(&build.release, "go\n");
        }
    }
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::create_dir_all(path.parent().ok_or("file has no parent")?)
        .and_then(|()| fs::write(path, text))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Check a complete ledger snapshot, including the control after the first build exits.
fn held_claims(claims: &[Record], expected: &[u32]) -> Result<(), String> {
    if expected
        .iter()
        .enumerate()
        .any(|(i, pid)| expected[..i].contains(pid))
        || claims.len() != expected.len()
        || expected.iter().any(|pid| {
            !claims.iter().any(|claim| {
                claim.pid == *pid && claim.held.is_some() && claim.need.covers && claim.need.mem > 0
            })
        })
    {
        return Err(format!(
            "expected held covering memory claims for {expected:?}, found {claims:?}"
        ));
    }
    Ok(())
}

/// Physical record files, including unlocked files which `Ledger::list` deliberately omits.
/// The extensions are the ledger's documented `.claim` and `.wait` record forms.
fn record_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path
            .extension()
            .is_some_and(|ext| ext == "claim" || ext == "wait")
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn live_targo(pid: u32, targo: &Path) -> Option<Identity> {
    let raw = i32::try_from(pid).ok()?;
    let identity = read_identity(raw)?;
    if read_process_path(raw)?.canonicalize().ok()?.as_path() != targo {
        return None;
    }
    let locked = atpkg::build_admit::possible_build_lock(
        &|| vec![pid],
        &|pid| aterm_uds::holders::open_files(pid).unwrap_or_default(),
        &|path| aterm_uds::ownerlock::probe(path) == aterm_uds::ownerlock::Owner::Live,
    );
    (locked.is_some() && read_identity(raw) == Some(identity)).then_some(identity)
}

impl Fixture {
    fn new(keep: bool) -> Result<Self, String> {
        // Keep by construction: only the successful, proven-exec path below removes this.
        let parent = aterm_tempfile::Builder::new()
            .prefix("aterm-build-admit-")
            .tempdir()
            .map_err(|e| e.to_string())?
            .keep();
        let root = parent.join("fixture.noindex");
        fs::create_dir_all(root.join("home/Library/Caches")).map_err(|e| e.to_string())?;
        println!("build-admit: scratch {}", root.display());
        Ok(Self {
            ledger: Ledger::at(root.join("ledger")),
            root,
            builds: Vec::new(),
            keep,
            sample_after: None,
        })
    }

    fn environment(&self, cmd: &mut aterm_uds::spawnfd::Command) {
        cmd.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.join("home"))
            .env("ATERM_STATE_HOME", self.root.join("state"))
            .env(atpkg::build_admit::LEDGER_ENV, self.ledger.dir())
            .env(
                aterm_capacity::admit::TRACE_ENV,
                self.root.join("admission.trace"),
            )
            .env("ATERM_ADMIT", "now")
            .stdin(Stdio::null());
    }

    fn capability(&self, helper: &Path) -> Result<(), String> {
        stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
        let out = self.root.join("capability.out");
        let log = self.root.join("capability.err");
        let mut cmd = command(helper);
        self.environment(&mut cmd);
        cmd.args([atpkg::build_admit::VERB, "--capabilities"])
            .stdout(fs::File::create(&out).map_err(|e| e.to_string())?)
            .stderr(fs::File::create(&log).map_err(|e| e.to_string())?);
        stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
        let mut child = cmd.spawn().map_err(|e| e.to_string())?;
        let status = reap(&mut child, HANG, Reap::Child)?;
        let wanted = format!("{}\n", atpkg::build_admit::HOP_CAPABILITY);
        if !status.success() || fs::read_to_string(&out).ok().as_deref() != Some(wanted.as_str()) {
            return Err(format!(
                "lab image did not answer the exact shim-hop capability ({status}); {}",
                log.display()
            ));
        }
        Ok(())
    }

    fn start(&mut self, helper: &Path, targo: &Path, index: usize) -> Result<(), String> {
        stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
        let ws = self.root.join(format!("home/workspace-{index}"));
        write(
            &ws.join("Cargo.toml"),
            &format!(
                "[package]\nname = \"admit_{index}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n"
            ),
        )?;
        write(&ws.join("src/lib.rs"), "pub fn value() -> u32 { 7 }\n")?;
        write(&ws.join("build.rs"), SCRIPT)?;
        let shim = self.root.join("targo-shim");
        if index == 0 {
            let block = atpkg::build_cache::shim_block("targo").ok_or("no targo shim block")?;
            let hop =
                atpkg::build_cache::registered::shim_hop(helper, "targo", &quoted(targo), None);
            write(&shim, &format!("#!/bin/sh\n{block}{hop}"))?;
        }
        let ready = self.root.join(format!("ready-{index}"));
        let release = self.root.join(format!("release-{index}"));
        let log = self.root.join(format!("build-{index}.log"));
        let mut cmd = command("/bin/sh");
        self.environment(&mut cmd);
        cmd.arg(&shim)
            .args(["--unverified", "build", "-q"])
            .env("PWD", &ws)
            .env("CARGO_TARGET_DIR", ws.join("target.noindex"))
            .env("BC_HOP_READY", &ready)
            .env("BC_HOP_RELEASE", &release)
            .current_dir(&ws)
            .process_group(0)
            .setsid(true)
            .stdout(Stdio::null())
            .stderr(fs::File::create(&log).map_err(|e| e.to_string())?);
        stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
        let child = cmd.spawn().map_err(|e| e.to_string())?;
        self.builds.push(Build {
            child,
            ready,
            release,
            log,
            witness: None,
            status: None,
            sampled: false,
        });
        Ok(())
    }

    fn ready(&mut self, targo: &Path) -> Result<(), String> {
        let started = Instant::now();
        let ready_by = started + HANG;
        loop {
            stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
            for (index, build) in self.builds.iter_mut().enumerate() {
                if Instant::now() >= ready_by {
                    return Err("hang detector: two build-script barriers".to_owned());
                }
                if build.witness.is_some() {
                    continue;
                }
                if build.child.exited().map_err(|e| e.to_string())? {
                    return Err(format!(
                        "build exited before its barrier: {}",
                        build.log.display()
                    ));
                }
                let Some(script_pid) = fs::read_to_string(&build.ready)
                    .ok()
                    .and_then(|text| text.trim().parse::<i32>().ok())
                else {
                    if sample_due(build, self.sample_after, started.elapsed()) {
                        build.sampled = true;
                        let path = self.root.join(format!("front-{index}.sample"));
                        let receipt = capture::capture(&mut build.child, &path, ready_by);
                        if let Err(error) =
                            write(&self.root.join(format!("front-{index}.capture")), &receipt)
                        {
                            eprintln!("build-admit: could not retain capture receipt: {error}");
                        }
                        println!("build-admit: {receipt}");
                        stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
                        if Instant::now() >= ready_by {
                            return Err("hang detector: two build-script barriers".to_owned());
                        }
                    }
                    continue;
                };
                let front = live_targo(build.child.id(), targo)
                    .ok_or("barrier parent is not the specified real targo holding a Cargo lock")?;
                let script = read_identity(script_pid).ok_or("barrier script disappeared")?;
                if read_process_parent(script_pid) != Some(build.child.id()) {
                    return Err("barrier script is not the real targo's child".to_owned());
                }
                build.witness = Some(Witness { front, script });
            }
            if Instant::now() >= ready_by {
                return Err("hang detector: two build-script barriers".to_owned());
            }
            if self.builds.iter().all(|build| build.witness.is_some()) {
                return Ok(());
            }
            std::thread::sleep(POLL);
        }
    }

    fn snapshot(&self, expected: &[usize], targo: &Path) -> Result<Vec<Record>, String> {
        for index in expected {
            let build = &self.builds[*index];
            let witness = build.witness.ok_or("no build witness")?;
            if live_targo(build.child.id(), targo) != Some(witness.front)
                || read_identity(witness.script.pid) != Some(witness.script)
                || read_process_parent(witness.script.pid) != Some(build.child.id())
                || build.release.exists()
            {
                return Err(
                    "a supposedly held build changed identity or left its barrier".to_owned(),
                );
            }
        }
        let claims = self.ledger.list().map_err(|e| e.to_string())?;
        let pids: Vec<_> = expected
            .iter()
            .map(|index| self.builds[*index].child.id())
            .collect();
        held_claims(&claims, &pids)?;
        // Close the observation interval: the same processes still hold their locks afterward.
        for index in expected {
            let build = &self.builds[*index];
            let witness = build.witness.ok_or("no build witness")?;
            if live_targo(build.child.id(), targo) != Some(witness.front)
                || read_identity(witness.script.pid) != Some(witness.script)
                || read_process_parent(witness.script.pid) != Some(build.child.id())
                || build.release.exists()
            {
                return Err("a held build changed during its ledger snapshot".to_owned());
            }
        }
        Ok(claims)
    }

    fn exercise(&mut self, helper: &Path, targo: &Path) -> Result<(), String> {
        self.capability(helper)?;
        self.start(helper, targo, 0)?;
        self.start(helper, targo, 1)?;
        self.ready(targo)?;
        let both = self.snapshot(&[0, 1], targo)?;
        write(&self.root.join("both-claims.txt"), &format!("{both:#?}\n"))?;
        let files = record_files(self.ledger.dir())?;
        let paths: Vec<PathBuf> = self
            .builds
            .iter()
            .map(|build| {
                let claim = both
                    .iter()
                    .find(|claim| claim.pid == build.child.id())
                    .ok_or("held front has no recorded claim")?;
                let marker = claim.cover_marker();
                files
                    .iter()
                    .find(|path| {
                        path.extension().is_some_and(|ext| ext == "claim")
                            && path.file_stem().is_some_and(|stem| stem == marker.as_str())
                    })
                    .cloned()
                    .ok_or_else(|| format!("held claim {marker} has no physical record file"))
            })
            .collect::<Result<_, String>>()?;
        write(&self.root.join("claim-paths.txt"), &format!("{paths:#?}\n"))?;
        println!("build-admit: two real targo images hold distinct covering memory claims");
        write(&self.builds[0].release, "go\n")?;
        let status = reap(&mut self.builds[0].child, HANG, Reap::Natural)?;
        self.builds[0].status = Some(status);
        if !status.success() {
            return Err(format!("first build failed ({status})"));
        }
        let started = Instant::now();
        let second = loop {
            let claims = self.ledger.list().map_err(|e| e.to_string())?;
            let first_gone = !paths[0].try_exists().map_err(|e| e.to_string())?;
            if first_gone && held_claims(&claims, &[self.builds[1].child.id()]).is_ok() {
                break self.snapshot(&[1], targo)?;
            }
            stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
            if started.elapsed() >= HANG {
                return Err("hang detector: first claim retirement".to_owned());
            }
            std::thread::sleep(POLL);
        };
        write(
            &self.root.join("second-only.txt"),
            &format!("{second:#?}\n"),
        )?;
        println!("build-admit: first claim retired; second build remains held and claimed");
        write(&self.builds[1].release, "go\n")?;
        let status = reap(&mut self.builds[1].child, HANG, Reap::Natural)?;
        self.builds[1].status = Some(status);
        if !status.success() {
            return Err(format!("second build failed ({status})"));
        }
        let started = Instant::now();
        loop {
            if self.ledger.list().map_err(|e| e.to_string())?.is_empty()
                && record_files(self.ledger.dir())?.is_empty()
            {
                // The original files as well as every physical claim/wait record retired.
                for path in &paths {
                    if path.try_exists().map_err(|e| e.to_string())? {
                        return Err(format!(
                            "retired claim file still exists: {}",
                            path.display()
                        ));
                    }
                }
                break;
            }
            stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
            if started.elapsed() >= HANG {
                return Err("hang detector: final claim retirement".to_owned());
            }
            std::thread::sleep(POLL);
        }
        Ok(())
    }

    fn cleanup(&mut self, passed: bool) -> Result<(), String> {
        // Release every started script even if setup or an observation failed halfway through.
        let mut errors = Vec::new();
        for build in &self.builds {
            if let Err(e) = write(&build.release, "go\n") {
                errors.push(e);
            }
        }
        for build in &mut self.builds {
            if build.status.is_none() {
                match reap(&mut build.child, HANG, Reap::Group) {
                    Ok(status) => build.status = Some(status),
                    Err(e) => errors.push(e),
                }
            }
        }
        // Holders deliberately detached from the builds retire their own claims. Never
        // signal them by an observed pid; report any survivor and preserve its executables.
        let started = Instant::now();
        loop {
            let retirement = self
                .ledger
                .list()
                .map_err(|e| e.to_string())
                .and_then(|claims| record_files(self.ledger.dir()).map(|files| (claims, files)));
            match retirement {
                Ok((claims, files)) if claims.is_empty() && files.is_empty() => break,
                Ok((claims, files)) => {
                    if started.elapsed() >= HANG {
                        errors.push(format!(
                            "hang detector: admission holder or record survived cleanup: {claims:?}; {files:?}"
                        ));
                        break;
                    }
                }
                Err(error) => {
                    errors.push(format!("observe cleanup retirement: {error}"));
                    break;
                }
            }
            std::thread::sleep(POLL);
        }
        if passed && errors.is_empty() && !self.keep {
            // Every generated executable ran and its build exited naturally before removal.
            let parent = self.root.parent().ok_or("scratch has no parent")?;
            let mut proven = aterm_uds::deadline::KeptUntilRan::new(parent);
            proven.ran();
            drop(proven);
        } else {
            eprintln!(
                "build-admit: KEPT {} (logs and executable files)",
                self.root.display()
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

fn sample_due(build: &Build, after: Option<Duration>, elapsed: Duration) -> bool {
    build.witness.is_none() && !build.sampled && after.is_some_and(|after| elapsed >= after)
}

#[derive(Clone, Copy)]
enum Reap {
    /// A normal step respects interrupts and leaves timeout cleanup to `Fixture::cleanup`.
    Natural,
    /// The capability probe owns one child, which has no private process group.
    Child,
    /// Each real build leads a private group; cleanup owns its group until the leader is reaped.
    Group,
}

/// The leader is still unreaped, so the group cannot be reused while this is read.
fn group_clear(group: u32) -> Result<(), String> {
    use aterm_uds::process::{ProcessPresence, group_members, process_presence};
    let members = group_members(group).ok_or("could not list the owned build group")?;
    if !members.complete {
        return Err("owned build group listing is incomplete".to_owned());
    }
    for pid in members.pids {
        if u32::try_from(pid).ok() == Some(group) {
            continue;
        }
        match process_presence(pid) {
            ProcessPresence::Absent | ProcessPresence::Exited => {}
            presence => {
                return Err(format!(
                    "build group {group} retains member {pid}: {presence:?}"
                ));
            }
        }
    }
    Ok(())
}

/// Sweep before any wait reaps the spawned group leader. Zombies need no further signal.
fn sweep_group(group: u32) -> Result<(), String> {
    let raw = i32::try_from(group).map_err(|e| e.to_string())?;
    let signal = aterm_uds::process::kill_group_until_empty(raw);
    let clear = group_clear(group);
    match signal {
        Ok(()) => clear,
        Err(error) if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::EPERM)) => clear,
        Err(error) => Err(format!(
            "signal owned group {group}: {error}; group observation: {clear:?}"
        )),
    }
}

/// Reap naturally; cleanup ends the own, still-unreaped build group after releasing barriers.
fn reap(child: &mut Child, hang: Duration, mode: Reap) -> Result<ExitStatus, String> {
    let started = Instant::now();
    loop {
        match mode {
            Reap::Child => {
                if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                    return Ok(status);
                }
            }
            Reap::Natural | Reap::Group if child.exited().map_err(|e| e.to_string())? => {
                if matches!(mode, Reap::Natural) {
                    // A failed check leaves the leader unreaped for cleanup's group sweep.
                    group_clear(child.id())?;
                    return child.wait().map_err(|e| e.to_string());
                }
                let swept = sweep_group(child.id());
                let status = child.wait().map_err(|e| e.to_string())?;
                return swept.map(|()| status);
            }
            Reap::Natural | Reap::Group => {}
        }
        if matches!(mode, Reap::Natural) {
            stop::check().map_err(|e| format!("interrupted: {}", e.0))?;
        }
        if started.elapsed() >= hang {
            break;
        }
        std::thread::sleep(POLL);
    }
    if !matches!(mode, Reap::Natural) {
        // No wait has reaped this leader, pinning its group id throughout the sweep.
        // The detached admission holder is outside this group and is never signalled here.
        let killed = match mode {
            Reap::Group => sweep_group(child.id()),
            Reap::Child => child.kill().map_err(|e| e.to_string()),
            Reap::Natural => unreachable!(),
        };
        let started = Instant::now();
        loop {
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                return Err(format!(
                    "hang detector: child required cleanup after release; group/child signal: {killed:?}"
                ));
            }
            if started.elapsed() >= HANG {
                break;
            }
            std::thread::sleep(POLL);
        }
    }
    Err(format!("hang detector: child {} did not exit", child.id()))
}

/// Run the scratch-only covering-claim lifetime acceptance.
pub fn cmd(args: &[String]) -> Res {
    let args = parse(args)?;
    let helper = std::env::current_exe().map_err(|e| Fail::die(e.to_string()))?;
    stop::install();
    stop::check()?;
    let mut fixture = Fixture::new(args.keep || args.sample_after.is_some()).map_err(Fail::die)?;
    fixture.sample_after = args.sample_after;
    HARD_RELEASES
        .set([
            fixture.root.join("release-0"),
            fixture.root.join("release-1"),
        ])
        .map_err(|_| Fail::die("build-admit already registered scratch release barriers"))?;
    stop::at_hard_exit(release_on_hard_exit);
    let result = fixture.exercise(&helper, &args.targo);
    let cleanup = fixture.cleanup(result.is_ok());
    let result = match (result, cleanup) {
        (Err(original), Err(cleanup)) => Err(format!("{original}; cleanup: {cleanup}")),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    };
    result.map_err(|e| Fail::die(format!("build-admit: {e}")))?;
    println!(
        "build-admit: PASS; both builds exited successfully and the scratch ledger is empty (memory lifetimes only; no wait or CPU-fairness claim)"
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::{Build, Fixture, HANG, POLL, held_claims, record_files};
    use aterm_capacity::{Held, Need, Record};
    use std::path::Path;

    const UNWIND_STANDIN: &str = "ATERM_LAB_BUILD_ADMIT_UNWIND_STANDIN";

    #[test]
    fn sampling_threshold_is_whole_seconds_below_the_hang_detector() {
        use super::{parse, sample_delay};
        use std::time::Duration;

        assert_eq!(sample_delay("0"), Some(Duration::ZERO));
        assert_eq!(sample_delay("59"), Some(Duration::from_secs(59)));
        for invalid in ["60", "-1", "1.5", "later"] {
            assert_eq!(sample_delay(invalid), None);
        }
        let scratch = aterm_tempfile::tempdir().unwrap();
        let targo = scratch.path().join("targo");
        std::fs::write(&targo, "path validation only; never executed\n").unwrap();
        let mut args = vec!["--targo".to_owned(), targo.to_str().unwrap().to_owned()];
        assert!(parse(&args).unwrap().sample_after.is_none());
        args.extend(["--sample-after", "0"].map(str::to_owned));
        assert_eq!(parse(&args).unwrap().sample_after, Some(Duration::ZERO));
        args.extend(["--sample-after", "1"].map(str::to_owned));
        assert!(parse(&args).is_err());
        assert!(parse(&["--sample-after".to_owned()]).is_err());
    }

    #[test]
    fn unwind_standin() {
        let Some(root) = std::env::var_os(UNWIND_STANDIN) else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        std::fs::write(root.join("ready"), std::process::id().to_string()).unwrap();
        while !root.join("release").exists() {
            std::thread::sleep(POLL);
        }
        std::fs::write(root.join("done"), std::process::id().to_string()).unwrap();
    }

    fn standin(root: &Path) -> aterm_uds::spawnfd::Child {
        aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "build_admit::tests::unwind_standin",
                "--test-threads=1",
            ])
            .env_clear()
            .env(UNWIND_STANDIN, root)
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .unwrap()
    }

    fn fixture(root: &Path, child: aterm_uds::spawnfd::Child) -> Fixture {
        Fixture {
            root: root.to_path_buf(),
            ledger: aterm_capacity::Ledger::at(root.join("ledger")),
            keep: true,
            sample_after: None,
            builds: vec![Build {
                child,
                ready: root.join("ready"),
                release: root.join("release"),
                log: root.join("log"),
                witness: None,
                status: None,
                sampled: false,
            }],
        }
    }

    fn until(mut done: impl FnMut() -> bool) -> bool {
        let started = std::time::Instant::now();
        while !done() {
            if started.elapsed() >= HANG {
                return false;
            }
            std::thread::sleep(POLL);
        }
        true
    }

    #[test]
    fn an_unwind_releases_its_held_child_to_finish_naturally() {
        let mut scratch =
            aterm_uds::deadline::KeptUntilRan::new(aterm_tempfile::tempdir().unwrap().keep());
        let root = scratch.path().to_path_buf();
        // The current test image stays in place; this scratch directory holds only markers.
        let child = standin(&root);
        let pid = i32::try_from(child.id()).unwrap();
        let fixture = fixture(&root, child);
        let ready = until(|| {
            std::fs::read_to_string(root.join("ready"))
                .ok()
                .is_some_and(|text| text == pid.to_string())
        });
        let identity = aterm_uds::process::read_identity(pid);
        let completed = || {
            root.join("done").is_file()
                && (aterm_uds::process::process_presence(pid)
                    == aterm_uds::process::ProcessPresence::Absent
                    || aterm_uds::process::read_identity(pid)
                        .is_some_and(|now| Some(now) != identity))
        };
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _fixture = fixture;
            panic!("stand-in for an output failure while the scripts are held");
        }))
        .is_err();
        let released = root.join("release").is_file();
        let finished_by_drop = released && until(completed);
        // NEGATIVE CONTROL: removing Drop must fail only after the held child is released
        // and finishes; neither an assertion nor the failed observation may strand it.
        let fallback = std::fs::write(root.join("release"), "go\n");
        let finished = until(completed);
        if ready && finished {
            scratch.ran();
        }
        assert!(
            fallback.is_ok() && finished,
            "fallback did not finish the child"
        );
        assert!(ready && identity.is_some(), "child never proved execution");
        assert!(
            unwound && finished_by_drop,
            "unwind left its child waiting for a barrier"
        );
    }

    #[test]
    fn dropping_a_completed_fixture_does_not_recreate_removed_scratch() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path().to_path_buf();
        std::fs::write(root.join("release"), "go\n").unwrap();
        let mut fixture = fixture(&root, standin(&root));
        let status = super::reap(&mut fixture.builds[0].child, HANG, super::Reap::Child).unwrap();
        fixture.builds[0].status = Some(status);
        let ran = root.join("ready").is_file() && root.join("done").is_file();
        std::fs::remove_dir_all(&root).unwrap();
        drop(fixture);
        assert!(status.success() && ran, "child did not finish naturally");
        assert!(
            !root.exists(),
            "dropping the fixture recreated removed scratch"
        );
    }

    fn claim(pid: u32) -> Record {
        Record {
            accounting: aterm_capacity::Accounting::Holder,
            timing: aterm_capacity::TimingEvidence::default(),
            pid,
            start_ms: 1,
            seq: 0,
            checkout: format!("checkout-{pid}"),
            need: Need::new("targo build").mem(1 << 30).covers(),
            held: Some(Held {
                admitted_ms: 1,
                cores: 0,
                alone: false,
            }),
            credit: Vec::new(),
            wrote: None,
            next_ms: None,
        }
    }

    #[test]
    fn snapshots_reject_missing_waiting_duplicate_and_surviving_claims() {
        let both = vec![claim(11), claim(22)];
        assert!(held_claims(&both, &[11, 22]).is_ok());
        assert!(
            held_claims(&both, &[22]).is_err(),
            "first claim never retired"
        );
        assert!(
            held_claims(&both[..1], &[11, 22]).is_err(),
            "second build never claimed"
        );
        assert!(
            held_claims(&both, &[11, 11]).is_err(),
            "one front counted twice"
        );
        assert!(held_claims(&both[1..], &[22]).is_ok());
        let mut bad = both.clone();
        bad[1].held = None;
        assert!(held_claims(&bad, &[11, 22]).is_err(), "waiting is not held");
        bad[1] = claim(22);
        bad[1].need.covers = false;
        assert!(
            held_claims(&bad, &[11, 22]).is_err(),
            "claim does not cover its build"
        );
        bad[1] = claim(22);
        bad[1].need.mem = 0;
        assert!(
            held_claims(&bad, &[11, 22]).is_err(),
            "claim reserves no memory"
        );
        assert!(held_claims(&[], &[]).is_ok());
    }

    #[test]
    fn an_empty_live_ledger_does_not_prove_physical_retirement() {
        let dir = aterm_tempfile::tempdir().expect("scratch ledger");
        let ledger = aterm_capacity::Ledger::at(dir.path());
        let stale = dir.path().join("0000000000001-11-0.claim");
        std::fs::write(&stale, "unlocked orphan\n").expect("orphan record");
        std::fs::write(dir.path().join("ledger.lock"), "").expect("admission lock file");
        assert!(ledger.list().expect("live claims").is_empty());
        let files = record_files(dir.path()).expect("physical records");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0], stale);
        let waiting = dir.path().join("0000000000002-22-0.wait");
        std::fs::write(&waiting, "unlocked waiting orphan\n").expect("waiting record");
        assert_eq!(record_files(dir.path()).expect("physical records").len(), 2);
        std::fs::remove_file(stale).expect("retire held record");
        std::fs::remove_file(waiting).expect("retire waiting record");
        assert!(
            record_files(dir.path())
                .expect("retired records")
                .is_empty()
        );
    }
}
