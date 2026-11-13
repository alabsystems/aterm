// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual fenced Targo work observed through the private window's Allocation view.
//! This functional check establishes no CPU-fairness, latency or proof claim.

use aterm_capacity::{
    Admit, Ledger, Pace,
    build_cpu::{Owner as CpuOwner, RetainedPool, RetainedSlotState},
    cohort_namespace::Namespace,
    epoch_controller::Config as EpochConfig,
    host_protocol::{Attempt, Epoch, Phase as NativePhase, Snapshot},
    leadership::Fence,
};
use aterm_uds::spawnfd::{self, Stdio};
use atpkg::build_admit::{
    self, Admission, JobControl, Seat,
    host::Completion,
    service::{
        EpochId, Owner,
        monitor::{EventKind, Exchange, Progress, RecordSummary},
    },
};
use atpkg::build_cache::Tool;
use std::{
    collections::BTreeMap,
    fs, io,
    num::NonZeroU32,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::{
    Fail, Res,
    instance::{self, Instance, Launch, World},
    paths::{from_caller, paths},
    stop,
    trail_gallery::{self, ReplyShape, image_bytes, relay_reply},
};

#[cfg(debug_assertions)]
#[path = "allocation_monitor_cache_work.rs"]
mod cache_work;

#[cfg(debug_assertions)]
#[path = "allocation_monitor_paint.rs"]
mod paint;

const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);
/// Conservative GUI/keeper reservation, independently of the actual builds'
/// resource claims. The run records their kernel lifetime peaks separately;
/// this configured budget is not presented as a measured peak.
const WINDOW_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SCRIPT: &str = r#"
fn main() {
    let world = std::path::PathBuf::from(BARRIER_PATH);
    std::fs::write(world.join("ready"), b"actual compiled build script running").unwrap();
    let began = std::time::Instant::now();
    while !world.join("release").exists() {
        assert!(world.is_dir(), "fixture owner removed its world");
        assert!(began.elapsed() < std::time::Duration::from_secs(60), "fixture owner did not release the barrier");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
"#;

pub const HELP: &str = "allocation-monitor --aterm PATH --targo PATH --sdk-bin ABS --out DIR [--look dark|light] [--keep]\n\
Open one private real window, build two actual Targo projects behind readiness barriers,\n\
attach the read-only monitor once through authenticated local control, and preserve two-build\n\
and one-build PNG/layout observations with the exact completed outcome. Then observe a real\n\
private N=2/R=1 epoch live and fully settled beside the still-held peer through that same\n\
attachment. Close Allocation before releasing the peer, then run all three emitted programs\n\
and verify ledger drain. Reopen through the same attachment for actual prepared cold and warm\n\
harness costs, with unchanged script counters and dependency bytes. Requires a debug lab and\n\
an immutable capable SDK whose targo is --targo. Functional window admission reserves\n\
2 GiB and coordinates with other updated lab runs; it supplies no timing evidence and\n\
refuses --admit now. No global CPU-pool activation.\n";

struct Args {
    aterm: Option<String>,
    targo: PathBuf,
    sdk_bin: PathBuf,
    out: PathBuf,
    light: bool,
    keep: bool,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let mut args = args.to_vec();
    let aterm = instance::take_value(&mut args, "--aterm")?;
    let targo = instance::take_value(&mut args, "--targo")?.ok_or_else(|| Fail::usage(HELP))?;
    let sdk_bin = instance::take_value(&mut args, "--sdk-bin")?.ok_or_else(|| Fail::usage(HELP))?;
    let out = instance::take_value(&mut args, "--out")?.ok_or_else(|| Fail::usage(HELP))?;
    let look = instance::take_value(&mut args, "--look")?.unwrap_or_else(|| "dark".into());
    let keep = instance::take_flag(&mut args, "--keep");
    if !args.is_empty() || !matches!(look.as_str(), "dark" | "light") {
        return Err(Fail::usage(HELP));
    }
    let targo = fs::canonicalize(from_caller(&targo))
        .map_err(|e| Fail::die(format!("resolve targo: {e}")))?;
    if targo.file_name().is_none_or(|n| n != "targo") {
        return Err(Fail::usage(
            "--targo must name the actual Trust targo image",
        ));
    }
    if !Path::new(&sdk_bin).is_absolute() {
        return Err(Fail::usage(
            "--sdk-bin must be an absolute immutable SDK bin directory",
        ));
    }
    let sdk_bin =
        fs::canonicalize(&sdk_bin).map_err(|e| Fail::die(format!("resolve SDK bin: {e}")))?;
    if !sdk_bin.is_dir() || fs::canonicalize(sdk_bin.join("targo")).ok().as_ref() != Some(&targo) {
        return Err(Fail::usage(
            "--targo must be the selected --sdk-bin/targo image",
        ));
    }
    let out =
        trail_gallery::resolve_out(&from_caller(&out), &trail_gallery::checkouts(&paths().repo))
            .map_err(Fail::die)?;
    Ok(Args {
        aterm,
        targo,
        sdk_bin,
        out,
        light: look == "light",
        keep,
    })
}

struct Release(Vec<PathBuf>);
impl Drop for Release {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = crate::util::write_fresh(&path.join("release"), b"parent cleanup");
        }
    }
}

struct Engine {
    owner: Owner,
    done: Vec<Completion>,
}
impl Engine {
    fn tick(&mut self) -> Result<(), String> {
        self.done.extend(
            self.owner
                .tick(&mut |_, _| Ok(JobControl::Wait), &mut |line| {
                    eprintln!("{line}")
                })
                .map_err(|e| format!("native owner: {e}"))?,
        );
        Ok(())
    }
    fn live(&mut self, completed: usize) -> Result<(), String> {
        self.tick()?;
        if self.done.len() != completed {
            return Err(format!(
                "unexpected build completion at observation barrier (expected {completed}): {:?}",
                self.done
            ));
        }
        Ok(())
    }
}

struct IoWorker {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
}
impl IoWorker {
    fn start(mut exchange: Exchange) -> io::Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let thread = std::thread::Builder::new()
            .name("allocation-monitor-lab-io".into())
            .spawn(move || {
                loop {
                    if stop.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if exchange.step()? == Progress::Complete {
                        return Ok(());
                    }
                    // Wait on publication or socket readiness in this independent
                    // context. A bounded wait also permits fixture cancellation.
                    match exchange.wait(Instant::now() + Duration::from_millis(100)) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                        Err(e) => return Err(e),
                    }
                }
            })?;
        Ok(Self {
            cancel,
            thread: Some(thread),
        })
    }
    fn finish(mut self) -> Result<(), String> {
        self.cancel.store(true, Ordering::Release);
        self.thread
            .take()
            .expect("monitor I/O worker")
            .join()
            .map_err(|_| "monitor I/O worker panicked".to_owned())?
            .map_err(|e| format!("monitor I/O: {e}"))
    }
}
impl Drop for IoWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn build_script(barrier: Option<&Path>) -> io::Result<String> {
    let expression = match barrier {
        Some(path) => format!(
            "{:?}",
            path.to_str()
                .ok_or_else(|| io::Error::other("build barrier path is not UTF-8"))?
        ),
        None => "std::env::var_os(\"ALLOCATION_MONITOR_BUILD\").unwrap()".into(),
    };
    Ok(SCRIPT.replace("BARRIER_PATH", &expression))
}

fn prepare_project(path: &Path, embedded_barrier: bool) -> io::Result<()> {
    fs::create_dir(path)?;
    fs::create_dir(path.join("src"))?;
    crate::util::write_fresh(
        &path.join("Cargo.toml"),
        "[package]\nname='allocation_probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )?;
    crate::util::write_fresh(
        &path.join("src/main.rs"),
        "fn main() { assert_eq!((0..10).sum::<u64>(), 45); println!(\"allocation-probe-ok\"); }\n",
    )?;
    crate::util::write_fresh(
        &path.join("build.rs"),
        build_script(embedded_barrier.then_some(path))?,
    )
}

fn admit_project(path: &Path, namespace: &Namespace) -> Result<build_admit::Held, String> {
    let machine = aterm_capacity::Live;
    let seat = Seat {
        ledger: Some(Ledger::at(namespace.fence())),
        machine: &machine,
        own: None,
        verify: None,
        mode: Admit::Wait,
        pace: Pace::default(),
        pid: std::process::id(),
        marker: None,
    };
    let build = build_admit::classify(&["--unverified".into(), "build".into()], Tool::Targo)
        .ok_or("Targo build classifier refused")?;
    match build_admit::admit_cohort_job(&build, path, &seat, &mut |line| eprintln!("{line}")) {
        Admission::Claimed(held) => Ok(*held),
        other => Err(format!("private native admission refused: {other:?}")),
    }
}

fn submit_projects(
    world: &World,
    targo: &Path,
    namespace: &Namespace,
    engine: &mut Engine,
    release: &mut Release,
) -> Result<Vec<Attempt>, String> {
    let mut attempts = Vec::new();
    for index in 0..2 {
        let path = world.dir.join(format!("build-{index}.noindex"));
        prepare_project(&path, false).map_err(|e| format!("prepare project: {e}"))?;
        release.0.push(path.clone());
        let held = admit_project(&path, namespace)?;
        let mut command = spawnfd::transparent(targo).into_command();
        command
            .args(["--unverified", "build", "--offline", "--target-dir"])
            .arg(path.join("target.noindex"))
            .args([
                "--config",
                &format!("build.build-dir='{}'", path.join("build.noindex").display()),
            ])
            .env("ALLOCATION_MONITOR_BUILD", &path)
            .env("CARGO_BUILD_JOBS", "1")
            .current_dir(&path)
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                crate::util::create_stdio_file(&path.join("compiler.log"))
                    .map_err(|e| e.to_string())?,
            ));
        let attempt = engine
            .owner
            .submit(held, &mut command, Duration::ZERO)
            .map_err(|e| format!("submit actual Targo: {e}"))?;
        attempts.push(attempt);
    }
    let until = Instant::now() + HANG;
    loop {
        engine.live(0)?;
        if release.0.iter().all(|p| p.join("ready").is_file()) {
            return Ok(attempts);
        }
        if Instant::now() >= until {
            return Err("actual Targo build scripts did not reach both barriers".into());
        }
        stop::sleep(POLL).map_err(|e| format!("interrupted: {e:?}"))?;
    }
}

struct EpochBuild {
    id: EpochId,
    pool: RetainedPool,
    project: PathBuf,
    transport: PathBuf,
    admitted_bytes: u64,
}

impl EpochBuild {
    fn start(
        world: &World,
        targo: &Path,
        namespace: &Namespace,
        engine: &mut Engine,
        release: &mut Release,
    ) -> Result<Self, String> {
        let project = world.dir.join("epoch-build.noindex");
        // Epoch roots have no per-command environment extension. The original
        // compiled fixture embeds its private barrier instead of relying on one.
        prepare_project(&project, true).map_err(|e| format!("prepare epoch project: {e}"))?;
        release.0.push(project.clone());
        let held = admit_project(&project, namespace)?;
        let admitted_bytes = held.claim().record().need.mem;
        let pool = RetainedPool::create_private(
            &world.dir.join("epoch-cpu.noindex"),
            NonZeroU32::new(2).expect("two original CPU units"),
        )
        .map_err(|e| format!("private epoch CPU pool: {e}"))?;
        let mut cpu = Vec::new();
        for _ in 0..2 {
            cpu.push(
                pool.try_acquire(CpuOwner::current(None))
                    .map_err(|e| e.to_string())?
                    .ok_or("fresh private CPU backing unavailable")?,
            );
        }
        let transport = world.dir.join("epoch-transport.noindex");
        let accepted = engine
            .owner
            .submit_epoch(
                held,
                cpu,
                EpochConfig {
                    helper: targo.to_owned(),
                    directory: transport.clone(),
                    epoch: aterm_uds::rand::hex_token::<16>().map_err(|e| e.to_string())?,
                    roots: 1,
                },
            )
            .map_err(|e| format!("submit original epoch custody: {e}"))?;
        accepted
            .result
            .map_err(|e| format!("accepted epoch {} launch: {e}", accepted.id.sequence()))?;
        let until = Instant::now() + HANG;
        loop {
            engine.live(1)?;
            if engine
                .owner
                .epoch_status(&accepted.id)
                .map_err(|e| e.to_string())?
                .opened
            {
                break;
            }
            if Instant::now() >= until {
                return Err("actual Targo epoch helper did not open".into());
            }
            engine.owner.wait(until).map_err(|e| e.to_string())?;
        }
        engine
            .owner
            .epoch_spawn_root(
                &accepted.id,
                0,
                &project.join("Cargo.toml"),
                &project.join("target.noindex"),
            )
            .map_err(|e| format!("spawn original epoch root: {e}"))?;
        engine
            .owner
            .epoch_close(&accepted.id)
            .map_err(|e| format!("close original epoch: {e}"))?;
        let until = Instant::now() + HANG;
        loop {
            engine.live(1)?;
            if project.join("ready").is_file() {
                break;
            }
            if Instant::now() >= until {
                return Err("actual epoch build script did not reach its barrier".into());
            }
            // A held build script creates this file without a native exit or
            // pipe frame. Keep driving queued commands while checking its barrier.
            stop::sleep(POLL).map_err(|e| format!("interrupted: {e:?}"))?;
        }
        Ok(Self {
            id: accepted.id,
            pool,
            project,
            transport,
            admitted_bytes,
        })
    }

    fn snapshot(&self, engine: &Engine, peer: Attempt, settled: bool) -> Result<Snapshot, String> {
        let snapshot = (*engine
            .owner
            .observer()
            .read(None)
            .map_err(|e| format!("epoch snapshot: {e:?}"))?
            .snapshot)
            .clone();
        let expected = Epoch {
            sequence: self.id.sequence(),
            backing: 2,
            roots: 1,
            reserved_bytes: self.admitted_bytes,
            opened: true,
            retired: settled,
            helper_exited: settled,
            control_eof: settled,
            endpoint_cleaned: settled,
            settled_units: if settled { 2 } else { 0 },
            uncertain_units: 0,
            remaining_units: if settled { 0 } else { 2 },
            completed_roots: u32::from(settled),
            failed_roots: 0,
            faulted: false,
            unbound: false,
            not_spawned: false,
            cancellation_started: false,
            failure: None,
            cache_work: Vec::new(),
        };
        if snapshot.generation != peer.generation
            || snapshot.closed
            || snapshot.jobs.len() != 1
            || snapshot.jobs[0].attempt != peer.sequence
            || snapshot.epochs != [expected]
        {
            return Err(format!(
                "actual epoch/held-peer snapshot mismatch: {snapshot:?}"
            ));
        }
        let states = self.pool.snapshot().map_err(|e| e.to_string())?;
        if states.len() != 2
            || !states.iter().all(|state| {
                if settled {
                    matches!(state, RetainedSlotState::Available { .. })
                } else {
                    matches!(state, RetainedSlotState::Held { .. })
                }
            })
        {
            return Err(format!("private epoch CPU inventory mismatch: {states:?}"));
        }
        Ok(snapshot)
    }

    fn finish(&self, engine: &mut Engine) -> Result<(), String> {
        crate::util::write_fresh(
            &self.project.join("release"),
            b"observe original epoch settlement",
        )
        .map_err(|e| e.to_string())?;
        let until = Instant::now() + HANG;
        loop {
            engine.live(1)?;
            let status = engine
                .owner
                .epoch_status(&self.id)
                .map_err(|e| e.to_string())?;
            if status.settled_units == 2 {
                break;
            }
            if status.faulted || status.uncertain_units != 0 || Instant::now() >= until {
                return Err(format!("original epoch failed to settle: {status:?}"));
            }
            engine.owner.wait(until).map_err(|e| e.to_string())?;
        }
        let outcome = engine
            .owner
            .epoch_root_outcome(&self.id, 0)
            .map_err(|e| e.to_string())?
            .ok_or("settled epoch lost its actual root outcome")?;
        let history_only = engine
            .owner
            .epoch_close(&self.id)
            .is_err_and(|error| error.kind() == io::ErrorKind::AlreadyExists);
        if !outcome.success || outcome.code != Some(0) || !history_only {
            return Err(format!(
                "settled epoch did not retain successful read-only history: {outcome:?}"
            ));
        }
        match fs::symlink_metadata(self.transport.join("credits.fifo")) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            other => return Err(format!("settled original FIFO was not removed: {other:?}")),
        }
        let mut restored = Vec::new();
        for _ in 0..2 {
            restored.push(
                self.pool
                    .try_acquire(CpuOwner::current(None))
                    .map_err(|e| e.to_string())?
                    .ok_or("original epoch did not restore both CPU units")?,
            );
        }
        if restored[0].slot() == restored[1].slot()
            || self
                .pool
                .try_acquire(CpuOwner::current(None))
                .map_err(|e| e.to_string())?
                .is_some()
        {
            return Err("restored private pool did not conserve exactly N=2".into());
        }
        // These newly acquired capabilities backed no work. Only the original
        // controller settled the epoch's original capabilities after closure.
        for cap in restored {
            cap.settle().map_err(|e| e.to_string())?;
        }
        crate::util::copy_regular_file(
            self.transport.join("root-0.stderr"),
            self.project.join("compiler.log"),
        )
        .map_err(|e| format!("preserve epoch compiler log: {e}"))?;
        Ok(())
    }
}

fn require(reply: instance::Reply, what: &str) -> Result<String, String> {
    if reply.code == 0 {
        Ok(reply.stdout)
    } else {
        Err(format!("{what}: {}", trail_gallery::reply_text(&reply)))
    }
}

fn allocation_view(tabs: &str) -> Option<(u64, u64)> {
    let mut index = None;
    for line in tabs.lines() {
        let line = line.trim_start();
        let fields = crate::system_state_measure::fields(line);
        let get = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        if line.starts_with("tab ") {
            index = get("index").and_then(|v| v.parse::<u64>().ok());
        }
        if line.starts_with("leaf ") && get("app") == Some("allocation") {
            return Some((get("view")?.parse().ok()?, index?));
        }
    }
    None
}

fn monitor_projection(text: &str, prefix: &str, field: &str) -> Option<BTreeMap<String, String>> {
    let mut rows = BTreeMap::new();
    for line in text.lines().filter(|line| line.starts_with(prefix)) {
        let fields = crate::system_state_measure::fields(line);
        let key = fields.iter().find(|(name, _)| name == "key")?.1.clone();
        if !key.starts_with("allocation/monitor/") {
            continue;
        }
        let has =
            |name: &str, value: &str| fields.iter().any(|pair| pair.0 == name && pair.1 == value);
        if (prefix == "paint-node " && (!has("visible", "true") || !has("clipped", "false")))
            || (prefix == "paint-text "
                && (!has("overflow", "false") || !has("clip-truncated", "false")))
        {
            return None;
        }
        let value = fields.iter().find(|(name, _)| name == field)?.1.clone();
        if rows.insert(key, value).is_some() {
            return None;
        }
    }
    Some(rows)
}

fn epoch_projection(epoch: &Epoch) -> Option<BTreeMap<String, String>> {
    use aterm_capacity::cache_work::{Availability, UnavailableReason};
    let yes = |flag| if flag { "yes" } else { "no" };
    let launch = if epoch.cancellation_started {
        " · launch cancellation started"
    } else if epoch.not_spawned {
        " · no child spawned; cancellation pending"
    } else if epoch.unbound {
        " · child held; durable binding pending"
    } else {
        ""
    };
    let fault = if epoch.faulted {
        " · protocol fault"
    } else {
        ""
    };
    let failure = epoch.failure.map_or_else(String::new, |failure| {
        format!(" · owner error {:?}", failure.kind)
    });
    let cleanup = match (epoch.cancellation_started, epoch.endpoint_cleaned) {
        (true, true) => "unlaunched cleanup confirmed",
        (true, false) => "unlaunched cleanup pending",
        (false, true) => "group drain + endpoint cleanup confirmed",
        (false, false) => "group drain + endpoint cleanup pending",
    };
    let mut rows: BTreeMap<_, _> = [
        ("budget", format!("Epoch {} · CPU backing N={} · root slots R={} · original admitted memory peak {} bytes ({:.1} GiB)", epoch.sequence, epoch.backing, epoch.roots, epoch.reserved_bytes, epoch.reserved_bytes as f64 / (1024.0 * 1024.0 * 1024.0))),
        ("roots", format!("Roots: {} completed of {} slots · {} failed{launch}{fault}{failure}", epoch.completed_roots, epoch.roots, epoch.failed_roots)),
        ("transport", format!("Transport: opened {} · Retired {} · helper exited {} · output ended {}", yes(epoch.opened), yes(epoch.retired), yes(epoch.helper_exited), yes(epoch.control_eof))),
        ("backing", format!("CPU backing: {} returned · {} remaining · {} uncertain · {cleanup}", epoch.settled_units, epoch.remaining_units, epoch.uncertain_units)),
    ].into_iter().map(|(part, label)| (format!("allocation/monitor/epoch/{}/{part}", epoch.sequence), label)).collect();
    for record in &epoch.cache_work {
        let key = |part| {
            format!(
                "allocation/monitor/epoch/{}/root/{}/cache/{part}",
                epoch.sequence, record.root
            )
        };
        let prefix = format!("Root {} · Historical cache validation", record.root);
        let mut put = |part, value| rows.insert(key(part), value).is_none().then_some(());
        match (&record.summary, record.unavailable) {
            (None, Some(reason)) => {
                let reason = match reason {
                    UnavailableReason::NotRecorded => "not recorded",
                    UnavailableReason::UnsupportedDriver => "unsupported driver",
                    UnavailableReason::MissingSession => "session marker missing",
                    UnavailableReason::AmbiguousSession => "session marker ambiguous",
                    UnavailableReason::QueryFailed => "report query failed",
                    UnavailableReason::InvalidReport => "invalid report",
                    UnavailableReason::PersistenceFailed => "report persistence failed",
                };
                put("availability", format!("{prefix}: unavailable · {reason}"))?;
            }
            (Some(summary), None) => {
                summary.validate().ok()?;
                let availability = match summary.availability {
                    Availability::Complete => "complete",
                    Availability::UnavailableLegacyLog => "legacy log unavailable",
                };
                put(
                    "availability",
                    format!(
                        "{prefix} · run {} · scope {} · {availability}",
                        summary.run_id,
                        summary.scope()
                    ),
                )?;
                if let Some(work) = summary.totals {
                    let d = summary.denominators;
                    put(
                        "coverage",
                        format!(
                            "Build-script input work: {} observed of {} script units · {} registered units · {} capture attempts · {} uncacheable",
                            d.observed_units,
                            d.run_script_units,
                            d.registered_units,
                            d.attempts_finished,
                            d.uncacheable_attempts
                        ),
                    )?;
                    put(
                        "work",
                        format!(
                            "Inputs: {} bytes hashed · {} metadata reads · {} revalidations",
                            work.bytes_hashed, work.metadata_reads, work.revalidations
                        ),
                    )?;
                }
            }
            _ => return None,
        }
    }
    let not_recorded = epoch
        .completed_roots
        .checked_sub(u32::try_from(epoch.cache_work.len()).ok()?)?;
    if not_recorded != 0 || epoch.cache_work.is_empty() {
        rows.insert(
            format!(
                "allocation/monitor/epoch/{}/cache/not-recorded",
                epoch.sequence
            ),
            if not_recorded == 0 {
                "Cache validation: no completed-root report recorded".into()
            } else {
                format!("Cache validation: not recorded for {not_recorded} completed roots")
            },
        );
    }
    Some(rows)
}

fn matches_snapshot(text: &str, expected: &Snapshot, attachment: u64) -> bool {
    let audit = text.lines().any(|line| line.starts_with("paint-node "));
    let Some(rows) = monitor_projection(text, if audit { "paint-node " } else { "text " }, "label")
    else {
        return false;
    };
    // An audit contains both semantic nodes and their actual painted text. They
    // are two witnesses of the same rows, never twice as many owned attempts.
    if audit && monitor_projection(text, "paint-text ", "painted").as_ref() != Some(&rows) {
        return false;
    }
    let mut epochs = BTreeMap::new();
    for epoch in &expected.epochs {
        let Some(projected) = epoch_projection(epoch) else {
            return false;
        };
        for (key, label) in projected {
            if epochs.insert(key, label).is_some() {
                return false;
            }
        }
    }
    let generation: String = expected
        .generation
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let source = format!(
        "Source generation: {generation} · snapshot revision {} · attachment {attachment}",
        expected.revision
    );
    rows.get("allocation/monitor/scope")
        .is_some_and(|scope| scope.starts_with("Delegated native session ·"))
        && rows.get("allocation/monitor/attachment")
            == Some(&format!("Monitor attachment {attachment}: Observed"))
        && rows.get("allocation/monitor/source") == Some(&source)
        && rows
            .keys()
            .filter(|key| key.starts_with("allocation/monitor/attempt/"))
            .count()
            == expected.jobs.len()
        && expected.jobs.iter().all(|job| {
            let phase = match job.phase {
                NativePhase::Starting => "Starting",
                NativePhase::Waiting => "Waiting for next child observation",
                NativePhase::Stopped => "Stopped",
                NativePhase::Cancelling => "Cancelling",
                NativePhase::ExitPinned => "Exit observed; leader retained",
                NativePhase::Faulted => "Faulted",
            };
            rows.get(&format!("allocation/monitor/attempt/{}", job.attempt))
                .is_some_and(|row| {
                    row.starts_with(&format!(
                        "Session attempt {} · {phase} · admitted peak {} bytes (",
                        job.attempt, job.reserved_bytes
                    ))
                })
        })
        && rows
            .keys()
            .filter(|key| key.starts_with("allocation/monitor/epoch/"))
            .count()
            == epochs.len()
        && epochs
            .iter()
            .all(|(key, label)| rows.get(key) == Some(label))
}

struct Scene<'a> {
    snapshot: &'a Snapshot,
    recent: &'a [RecordSummary],
}

fn matches_scene(text: &str, expected: &Scene<'_>, attachment: u64) -> bool {
    if !matches_snapshot(text, expected.snapshot, attachment) {
        return false;
    }
    let audit = text.lines().any(|line| line.starts_with("paint-node "));
    let Some(rows) = monitor_projection(text, if audit { "paint-node " } else { "text " }, "label")
    else {
        return false;
    };
    let mut completions = BTreeMap::new();
    for record in expected.recent {
        let Some(attempt) = record.attempt else {
            return false;
        };
        let EventKind::Finished {
            succeeded,
            cancelled,
        } = record.kind
        else {
            return false;
        };
        let key = format!("allocation/monitor/completion/{}", record.revision);
        let label = format!(
            "Session attempt {attempt} · Finished · {}{} · completion revision {}",
            if succeeded {
                "success established"
            } else {
                "success not established"
            },
            if cancelled {
                " · cancellation requested"
            } else {
                ""
            },
            record.revision
        );
        if completions.insert(key, label).is_some() {
            return false;
        }
    }
    rows.iter()
        .filter(|(key, _)| key.starts_with("allocation/monitor/completion/"))
        .eq(completions.iter())
}

fn observe(
    inst: &Instance,
    expected: &Scene<'_>,
    engine: &mut Engine,
    completed: usize,
    attachment: u64,
) -> Result<(u64, u64, String), String> {
    let until = Instant::now() + HANG;
    loop {
        engine.live(completed)?;
        let tabs = require(inst.ctl(&["inspect", "app/v1", "tabs"]), "inspect tabs")?;
        if let Some((view, index)) = allocation_view(&tabs) {
            let text = require(
                inst.ctl(&["inspect", "app/v1", "view", &view.to_string(), "text"]),
                "inspect Allocation text",
            )?;
            if matches_scene(&text, expected, attachment) {
                return Ok((view, index, text));
            }
        }
        if Instant::now() >= until {
            return Err(format!(
                "Allocation never displayed the delegated native rows; tabs:\n{tabs}"
            ));
        }
        stop::sleep(POLL).map_err(|e| format!("interrupted: {e:?}"))?;
    }
}

fn capture(
    inst: &Instance,
    view: u64,
    out: &Path,
    engine: &mut Engine,
    expected: &Scene<'_>,
    completed: usize,
    attachment: u64,
) -> Result<(), String> {
    let mut wire = inst.connect()?;
    let view = view.to_string();
    let audit_words = ["inspect", "app/v1", "view", view.as_str(), "audit"];
    let until = Instant::now() + HANG;
    let mut previous = None;
    let mut last_audit = String::from("No Allocation capture audit received");
    loop {
        engine
            .live(completed)
            .map_err(|error| format!("{error}\nLast Allocation capture audit:\n{last_audit}"))?;
        let _ = relay_reply(&mut wire, &["image", "--bytes"], ReplyShape::Counted)
            .map_err(|e| e.to_string())?;
        let audit = require(
            relay_reply(&mut wire, &audit_words, ReplyShape::Counted).map_err(|e| e.to_string())?,
            "Allocation audit",
        )?;
        last_audit.clone_from(&audit);
        let shape = crate::system_state_gallery::layout_shape(&audit);
        if !shape.is_empty()
            && previous.as_ref() == Some(&shape)
            && matches_scene(&audit, expected, attachment)
        {
            let shot = require(
                relay_reply(
                    &mut wire,
                    &["image", "--meta", "--bytes"],
                    ReplyShape::Counted,
                )
                .map_err(|e| e.to_string())?,
                "Allocation image",
            )?;
            let (png, meta) = image_bytes(&shot)?;
            let after = require(
                relay_reply(&mut wire, &audit_words, ReplyShape::Counted)
                    .map_err(|e| e.to_string())?,
                "Allocation audit after image",
            )?;
            if crate::system_state_gallery::layout_shape(&after) == shape
                && matches_scene(&after, expected, attachment)
            {
                crate::util::write_fresh(&out.join("allocation.png"), png)
                    .map_err(|e| e.to_string())?;
                crate::util::write_fresh(&out.join("allocation.layout.txt"), after)
                    .map_err(|e| e.to_string())?;
                crate::util::write_fresh(
                    &out.join("allocation.image-meta.txt"),
                    format!("{meta}\n"),
                )
                .map_err(|e| e.to_string())?;
                return Ok(());
            }
        }
        if Instant::now() >= until {
            return Err(format!(
                "Allocation frame did not settle\nLast Allocation capture audit:\n{last_audit}"
            ));
        }
        previous = Some(shape);
        stop::sleep(Duration::from_millis(250)).map_err(|e| format!("interrupted: {e:?}"))?;
    }
}

fn run(args: Args) -> Result<(), String> {
    if !cfg!(debug_assertions) {
        return Err(
            "allocation-monitor requires a debug lab for its real prepared-harness fixture".into(),
        );
    }
    if fs::canonicalize(args.sdk_bin.join("targo")).ok().as_ref() != Some(&args.targo) {
        return Err("selected SDK driver changed after argument validation".into());
    }
    fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;
    if fs::read_dir(&args.out)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("qualification output must be new or empty; retain earlier evidence".into());
    }
    let mut version = spawnfd::command(&args.targo);
    version.args(["--unverified", "--version"]);
    let version = aterm_uds::deadline::output_within(&mut version, HANG)
        .map_err(|e| format!("targo version: {e:?}"))?;
    if !version.status.success()
        || !String::from_utf8_lossy(&version.stdout)
            .lines()
            .any(|line| line.starts_with("targo "))
    {
        return Err("--targo did not identify as actual Targo under --unverified --version".into());
    }
    let config = if args.light {
        instance::config_with(&["theme = \"GitHub Light\"".into()])
    } else {
        instance::CONFIG_OFF.into()
    };
    let mut world = World::new(&config)?;
    // Preserve failed inputs/logs. Successful runs release the world only after
    // all children have ended, unless the caller requested --keep.
    world.keep();
    crate::util::write_fresh(
        &args.out.join("world.txt"),
        format!("{}\n", world.dir.display()),
    )
    .map_err(|e| e.to_string())?;
    let aterm = instance::resolve_aterm(args.aterm)?;
    let mut inst = Instance::start_functional(
        &world,
        Launch {
            aterm,
            headless: false,
            args: vec![
                "--columns".into(),
                "120".into(),
                "--lines".into(),
                "45".into(),
            ],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
            reroute: false,
        },
        WINDOW_MEMORY_BYTES,
    )?;
    inst.await_socket(instance::BOOT)
        .map_err(|e| format!("GUI startup: {e:?}"))?;
    let gui_pid = inst.aterm_pid().ok_or("private GUI has no live process")?;
    let gui_start = crate::sys::rusage(gui_pid)
        .filter(|usage| usage.ri_proc_start_abstime != 0)
        .ok_or("private GUI has no kernel usage observation")?
        .ri_proc_start_abstime;
    require(
        inst.ctl(&["appearance", if args.light { "light" } else { "dark" }]),
        "appearance",
    )?;
    require(inst.ctl(&["resize", "px", "1400", "1000"]), "resize")?;
    let namespace = Namespace::create_private(&world.dir, &world.home)
        .map_err(|e| format!("private namespace: {e}"))?;
    let owner = Owner::fenced(
        Fence::acquire(&namespace, Instant::now()).map_err(|e| e.to_string())?,
        2,
        128,
    )
    .map_err(|e| e.to_string())?;
    let mut engine = Engine {
        owner,
        done: Vec::new(),
    };
    let mut release = Release(Vec::new());
    let attempts = submit_projects(&world, &args.targo, &namespace, &mut engine, &mut release)?;
    let expected = (*engine
        .owner
        .observer()
        .read(None)
        .map_err(|e| format!("snapshot: {e:?}"))?
        .snapshot)
        .clone();
    if expected.jobs.len() != 2 {
        return Err("actual native owner did not retain both builds".into());
    }
    let two_out = args.out.join("two-builds");
    fs::create_dir(&two_out).map_err(|e| e.to_string())?;
    crate::util::write_fresh(
        &two_out.join("owner.snapshot.json"),
        expected.encode().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let source = engine.owner.monitor_source().map_err(|e| e.to_string())?;
    let (client, exchange) = source
        .pair(Instant::now() + HANG)
        .map_err(|e| e.to_string())?;
    let io = IoWorker::start(exchange).map_err(|e| e.to_string())?;
    let queued = aterm_ctl::allocation_monitor::attach(
        inst.sock.to_str().ok_or("non-UTF8 socket path")?,
        OwnedFd::from(client.into_stream()),
        expected.generation.0,
        Instant::now() + HANG,
    )
    .map_err(|e| format!("attach monitor: {e}"))?;
    require(inst.ctl(&["open", "app", "allocation"]), "open Allocation")?;
    let two_scene = Scene {
        snapshot: &expected,
        recent: &[],
    };
    let (view, index, text) = observe(&inst, &two_scene, &mut engine, 0, queued)?;
    crate::util::write_fresh(&two_out.join("allocation.text.txt"), text)
        .map_err(|e| e.to_string())?;
    capture(&inst, view, &two_out, &mut engine, &two_scene, 0, queued)?;

    // The existing delegated connection must carry the transition. There is no
    // second attachment or probe: only the first real build is released.
    crate::util::write_fresh(
        &release.0[0].join("release"),
        b"observe first build completion",
    )
    .map_err(|e| e.to_string())?;
    let until = Instant::now() + HANG;
    while engine.done.is_empty() {
        engine.tick()?;
        if !engine.done.is_empty() {
            break;
        }
        if Instant::now() >= until {
            return Err("first actual Targo build did not finish".into());
        }
        engine.owner.wait(until).map_err(|e| e.to_string())?;
    }
    engine.live(1)?;
    let completed = &engine.done[0];
    if completed.id.sequence() != attempts[0].sequence
        || !completed
            .result
            .as_ref()
            .is_ok_and(|outcome| outcome.succeeded())
        || release.0[1].join("release").exists()
    {
        return Err(format!(
            "first build/peer barrier mismatch: {:?}",
            engine.done
        ));
    }
    let remaining_read = engine
        .owner
        .observer()
        .read(None)
        .map_err(|e| format!("remaining snapshot: {e:?}"))?;
    let remaining = (*remaining_read.snapshot).clone();
    let recent: Vec<_> = remaining_read
        .events
        .iter()
        .map(RecordSummary::from)
        .filter(|record| matches!(record.kind, EventKind::Finished { .. } | EventKind::Failed))
        .collect();
    if remaining.generation != expected.generation
        || remaining.revision <= expected.revision
        || remaining.jobs.len() != 1
        || remaining.jobs[0].attempt != attempts[1].sequence
    {
        return Err("native owner did not observe the exact two-to-one transition".into());
    }
    if recent.len() != 1
        || recent[0].attempt != Some(attempts[0].sequence)
        || recent[0].kind
            != (EventKind::Finished {
                succeeded: true,
                cancelled: false,
            })
        || recent[0].revision <= expected.revision
        || recent[0].revision > remaining.revision
    {
        return Err(format!(
            "native owner completion inventory mismatch: {recent:?}"
        ));
    }
    let one_out = args.out.join("one-build");
    fs::create_dir(&one_out).map_err(|e| e.to_string())?;
    crate::util::write_fresh(
        &one_out.join("owner.snapshot.json"),
        remaining.encode().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    crate::util::write_fresh(
        &one_out.join("owner.completions.txt"),
        format!("{recent:#?}\n"),
    )
    .map_err(|e| e.to_string())?;
    let one_scene = Scene {
        snapshot: &remaining,
        recent: &recent,
    };
    let (next_view, next_index, text) = observe(&inst, &one_scene, &mut engine, 1, queued)?;
    if next_view != view || next_index != index {
        return Err("continuous observation replaced the Allocation view".into());
    }
    crate::util::write_fresh(&one_out.join("allocation.text.txt"), text)
        .map_err(|e| e.to_string())?;
    capture(&inst, view, &one_out, &mut engine, &one_scene, 1, queued)?;

    // The same original owner and monitor connection carry real retained epoch
    // work beside the still-held ordinary peer. No second monitor is attached.
    let epoch = EpochBuild::start(&world, &args.targo, &namespace, &mut engine, &mut release)?;
    let epoch_live = epoch.snapshot(&engine, attempts[1], false)?;
    if epoch_live.revision <= remaining.revision
        || epoch
            .pool
            .try_acquire(CpuOwner::current(None))
            .map_err(|e| e.to_string())?
            .is_some()
        || release.0[1].join("release").exists()
    {
        return Err("live epoch did not retain original CPU backing beside the held peer".into());
    }
    let epoch_out = args.out.join("live-epoch");
    fs::create_dir(&epoch_out).map_err(|e| e.to_string())?;
    crate::util::write_fresh(
        &epoch_out.join("owner.snapshot.json"),
        epoch_live.encode().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let epoch_scene = Scene {
        snapshot: &epoch_live,
        recent: &recent,
    };
    let (next_view, next_index, text) = observe(&inst, &epoch_scene, &mut engine, 1, queued)?;
    if next_view != view || next_index != index {
        return Err("live epoch observation replaced the Allocation view".into());
    }
    crate::util::write_fresh(&epoch_out.join("allocation.text.txt"), text)
        .map_err(|e| e.to_string())?;
    capture(
        &inst,
        view,
        &epoch_out,
        &mut engine,
        &epoch_scene,
        1,
        queued,
    )?;

    epoch.finish(&mut engine)?;
    let epoch_settled = epoch.snapshot(&engine, attempts[1], true)?;
    if epoch_settled.revision <= epoch_live.revision || release.0[1].join("release").exists() {
        return Err("original epoch settlement did not precede ordinary peer release".into());
    }
    let settled_out = args.out.join("settled-epoch");
    fs::create_dir(&settled_out).map_err(|e| e.to_string())?;
    crate::util::write_fresh(
        &settled_out.join("owner.snapshot.json"),
        epoch_settled.encode().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    crate::util::write_fresh(
        &settled_out.join("owner.root-outcome.txt"),
        format!(
            "{:?}\n",
            engine
                .owner
                .epoch_root_outcome(&epoch.id, 0)
                .map_err(|e| e.to_string())?
        ),
    )
    .map_err(|e| e.to_string())?;
    let settled_scene = Scene {
        snapshot: &epoch_settled,
        recent: &recent,
    };
    let (next_view, next_index, text) = observe(&inst, &settled_scene, &mut engine, 1, queued)?;
    if next_view != view || next_index != index {
        return Err("settled epoch observation replaced the Allocation view".into());
    }
    crate::util::write_fresh(&settled_out.join("allocation.text.txt"), text)
        .map_err(|e| e.to_string())?;
    capture(
        &inst,
        view,
        &settled_out,
        &mut engine,
        &settled_scene,
        1,
        queued,
    )?;
    require(
        inst.ctl(&["tab", "close", &index.to_string()]),
        "close Allocation",
    )?;
    let until = Instant::now() + HANG;
    loop {
        engine.live(1)?;
        let tabs = require(
            inst.ctl(&["inspect", "app/v1", "tabs"]),
            "inspect after panel close",
        )?;
        if allocation_view(&tabs).is_none() {
            break;
        }
        if Instant::now() >= until {
            return Err("Allocation view did not close".into());
        }
        stop::sleep(POLL).map_err(|e| format!("interrupted: {e:?}"))?;
    }
    if !inst.running() {
        return Err("closing Allocation unexpectedly stopped the private GUI".into());
    }
    crate::util::write_fresh(&release.0[1].join("release"), b"panel closed; finish peer")
        .map_err(|e| e.to_string())?;
    let until = Instant::now() + HANG;
    while !engine.owner.is_empty() {
        engine.tick()?;
        if engine.owner.is_empty() {
            break;
        }
        if Instant::now() >= until {
            return Err(format!("native builds did not finish: {:?}", engine.done));
        }
        engine.owner.wait(until).map_err(|e| e.to_string())?;
    }
    if engine.done.len() != 2
        || engine.done.iter().any(|done| {
            !done
                .result
                .as_ref()
                .is_ok_and(|outcome| outcome.succeeded())
        })
    {
        return Err(format!("build completion failed: {:?}", engine.done));
    }
    for path in &release.0 {
        let mut executable = spawnfd::command(path.join("target.noindex/debug/allocation_probe"));
        let output = aterm_uds::deadline::output_within(&mut executable, HANG)
            .map_err(|e| format!("emitted program: {e:?}"))?;
        if !output.status.success() || output.stdout != b"allocation-probe-ok\n" {
            return Err("emitted program failed".into());
        }
        crate::util::copy_regular_file(
            path.join("compiler.log"),
            args.out.join(format!(
                "{}.compiler.log",
                path.file_name().unwrap().to_string_lossy()
            )),
        )
        .map_err(|e| e.to_string())?;
    }
    if !Ledger::at(namespace.fence())
        .list()
        .map_err(|e| e.to_string())?
        .is_empty()
    {
        return Err("private cohort ledger did not conclusively drain".into());
    }
    // Keep the peer's original sixty-second barrier bounded to the original
    // scenes. Reopening reuses the original Source, Exchange and attachment.
    #[cfg(debug_assertions)]
    cache_work::run(
        &cache_work::Original {
            world: &world,
            out: &args.out,
            targo: &args.targo,
            sdk_bin: &args.sdk_bin,
            namespace: &namespace,
            inst: &inst,
            attachment: queued,
            generation: expected.generation,
        },
        &mut engine,
    )?;
    io.finish()?;
    let gui_usage = crate::sys::rusage(gui_pid)
        .filter(|usage| usage.ri_proc_start_abstime == gui_start)
        .ok_or("private GUI lost its original kernel usage identity")?;
    let keeper_usage =
        crate::sys::rusage(inst.pid).ok_or("private keeper has no kernel usage observation")?;
    let gui_peak = gui_usage.ri_lifetime_max_phys_footprint;
    let keeper_peak = keeper_usage.ri_lifetime_max_phys_footprint;
    if gui_peak == 0
        || keeper_peak == 0
        || gui_peak.saturating_add(keeper_peak) > WINDOW_MEMORY_BYTES
    {
        return Err(format!(
            "native memory observation unavailable or exceeds reservation: GUI {gui_peak}, keeper {keeper_peak}, reserved {WINDOW_MEMORY_BYTES}"
        ));
    }
    crate::util::write_fresh(
        &args.out.join("window-memory.txt"),
        format!(
            "admission=functional\ntiming_qualified=false\nconfigured_reserved_bytes={WINDOW_MEMORY_BYTES}\ngui_pid={gui_pid}\ngui_start_abstime={gui_start}\ngui_lifetime_peak_physical_bytes={gui_peak}\nkeeper_lifetime_peak_physical_bytes={keeper_peak}\npeak_scope=native_gui_and_keeper_before_stop\nbuild_memory=separate_original_claims\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    inst.stop()?;
    crate::util::copy_regular_file(&inst.log, args.out.join("gui.log"))
        .map_err(|e| e.to_string())?;
    crate::util::write_fresh(&args.out.join("result.txt"), format!("PASS\nattachment={queued}\nattachments=1\nobserved_generation={}\ninitial_revision={}\nremaining_revision={}\nobserved_transition=2->1\ncompleted_attempt={}\ncompletion_revision={}\ncompletion_visible_before_peer_release=true\nepoch_sequence={}\nepoch_live_revision={}\nepoch_settled_revision={}\nepoch_backing_n=2\nepoch_roots_r=1\nepoch_original_admitted_bytes={}\nepoch_successful_roots=1\nepoch_settled_units=2\nepoch_uncertain_units=0\nepoch_original_endpoint_absent=true\nepoch_exact_cpu_reacquisition=2\nepoch_settled_before_peer_release=true\nactual_builds=5\nemitted_programs=4\nharness_cold_warm_costs=true\nharness_script_counts=1,1,1\nharness_dependency_unchanged=true\nharness_cold_warm_same_attachment=true\npanel_closed_before_peer_release=true\nledger_drained=true\nlook={}\n", expected.generation.0.iter().map(|b| format!("{b:02x}")).collect::<String>(), expected.revision, remaining.revision, attempts[0].sequence, recent[0].revision, epoch.id.sequence(), epoch_live.revision, epoch_settled.revision, epoch.admitted_bytes, if args.light { "light" } else { "dark" })).map_err(|e| e.to_string())?;
    if !args.keep {
        world.release();
    }
    Ok(())
}

pub fn cmd(args: &[String]) -> Res {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("{HELP}");
        return Ok(0);
    }
    let args = parse(args)?;
    run(args).map_err(|e| Fail::die(format!("allocation-monitor: {e}")))?;
    println!(
        "allocation-monitor: PASS; one attachment painted actual completion, live/settled epoch and real cold/warm harness costs; five builds, unchanged dependency, four programs and drained ledger"
    );
    Ok(0)
}

#[cfg(test)]
#[path = "allocation_monitor_tests.rs"]
mod tests;
