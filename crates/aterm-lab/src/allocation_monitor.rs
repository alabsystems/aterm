// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Actual fenced Targo work observed through the private window's Allocation view.
//! This functional check establishes no CPU-fairness, latency or proof claim.

use aterm_capacity::{
    Admit, Ledger, Pace,
    cohort_namespace::Namespace,
    host_protocol::{Attempt, Phase as NativePhase, Snapshot},
    leadership::Fence,
};
use aterm_uds::spawnfd::{self, Stdio};
use atpkg::build_admit::{
    self, Admission, JobControl, Seat,
    host::Completion,
    service::{
        Owner,
        monitor::{EventKind, Exchange, Progress, RecordSummary},
    },
};
use atpkg::build_cache::Tool;
use std::{
    collections::BTreeMap,
    fs, io,
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

const HANG: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(25);
const SCRIPT: &str = r#"
fn main() {
    let world = std::path::PathBuf::from(std::env::var_os("ALLOCATION_MONITOR_BUILD").unwrap());
    std::fs::write(world.join("ready"), b"actual compiled build script running").unwrap();
    let began = std::time::Instant::now();
    while !world.join("release").exists() {
        assert!(world.is_dir(), "fixture owner removed its world");
        assert!(began.elapsed() < std::time::Duration::from_secs(60), "fixture owner did not release the barrier");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
"#;

pub const HELP: &str = "allocation-monitor --aterm PATH --targo PATH --out DIR [--look dark|light] [--keep]\n\
Open one private real window, build two actual Targo projects behind readiness barriers,\n\
attach the read-only monitor once through authenticated local control, preserve two-build\n\
and one-build PNG/layout observations with the same attachment and exact completed outcome,\n\
close Allocation while the\n\
peer remains owned, then finish it and run both emitted programs. Functional evidence only.\n";

struct Args {
    aterm: Option<String>,
    targo: PathBuf,
    out: PathBuf,
    light: bool,
    keep: bool,
}

fn parse(args: &[String]) -> Result<Args, Fail> {
    let mut args = args.to_vec();
    let aterm = instance::take_value(&mut args, "--aterm")?;
    let targo = instance::take_value(&mut args, "--targo")?.ok_or_else(|| Fail::usage(HELP))?;
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
    let out =
        trail_gallery::resolve_out(&from_caller(&out), &trail_gallery::checkouts(&paths().repo))
            .map_err(Fail::die)?;
    Ok(Args {
        aterm,
        targo,
        out,
        light: look == "light",
        keep,
    })
}

struct Release(Vec<PathBuf>);
impl Drop for Release {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::write(path.join("release"), b"parent cleanup");
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

fn prepare_project(path: &Path) -> io::Result<()> {
    fs::create_dir(path)?;
    fs::create_dir(path.join("src"))?;
    fs::write(
        path.join("Cargo.toml"),
        "[package]\nname='allocation_probe'\nversion='0.0.0'\nedition='2024'\n[workspace]\n",
    )?;
    fs::write(
        path.join("src/main.rs"),
        "fn main() { assert_eq!((0..10).sum::<u64>(), 45); println!(\"allocation-probe-ok\"); }\n",
    )?;
    fs::write(path.join("build.rs"), SCRIPT)
}

fn submit_projects(
    world: &World,
    targo: &Path,
    namespace: &Namespace,
    engine: &mut Engine,
    release: &mut Release,
) -> Result<Vec<Attempt>, String> {
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
    let mut attempts = Vec::new();
    for index in 0..2 {
        let path = world.dir.join(format!("build-{index}.noindex"));
        prepare_project(&path).map_err(|e| format!("prepare project: {e}"))?;
        release.0.push(path.clone());
        let held = match build_admit::admit_cohort_job(&build, &path, &seat, &mut |line| {
            eprintln!("{line}")
        }) {
            Admission::Claimed(held) => *held,
            other => return Err(format!("private native admission refused: {other:?}")),
        };
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
                fs::File::create(path.join("compiler.log")).map_err(|e| e.to_string())?,
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
        if !shape.is_empty() && previous.as_ref() == Some(&shape) {
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
                fs::write(out.join("allocation.png"), png).map_err(|e| e.to_string())?;
                fs::write(out.join("allocation.layout.txt"), after).map_err(|e| e.to_string())?;
                fs::write(out.join("allocation.image-meta.txt"), format!("{meta}\n"))
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
    fs::write(
        args.out.join("world.txt"),
        format!("{}\n", world.dir.display()),
    )
    .map_err(|e| e.to_string())?;
    let aterm = instance::resolve_aterm(args.aterm)?;
    let mut inst = Instance::start(
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
    )?;
    inst.await_socket(instance::BOOT)
        .map_err(|e| format!("GUI startup: {e:?}"))?;
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
    fs::write(
        two_out.join("owner.snapshot.json"),
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
    fs::write(two_out.join("allocation.text.txt"), text).map_err(|e| e.to_string())?;
    capture(&inst, view, &two_out, &mut engine, &two_scene, 0, queued)?;

    // The existing delegated connection must carry the transition. There is no
    // second attachment or probe: only the first real build is released.
    fs::write(
        release.0[0].join("release"),
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
    fs::write(
        one_out.join("owner.snapshot.json"),
        remaining.encode().map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        one_out.join("owner.completions.txt"),
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
    fs::write(one_out.join("allocation.text.txt"), text).map_err(|e| e.to_string())?;
    capture(&inst, view, &one_out, &mut engine, &one_scene, 1, queued)?;
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
    fs::write(release.0[1].join("release"), b"panel closed; finish peer")
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
        fs::copy(
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
    io.finish()?;
    inst.stop();
    fs::copy(&inst.log, args.out.join("gui.log")).map_err(|e| e.to_string())?;
    fs::write(args.out.join("result.txt"), format!("PASS\nattachment={queued}\nattachments=1\nobserved_generation={}\ninitial_revision={}\nremaining_revision={}\nobserved_transition=2->1\ncompleted_attempt={}\ncompletion_revision={}\ncompletion_visible_before_peer_release=true\nactual_builds=2\nemitted_programs=2\npanel_closed_before_peer_release=true\nledger_drained=true\nlook={}\n", expected.generation.0.iter().map(|b| format!("{b:02x}")).collect::<String>(), expected.revision, remaining.revision, attempts[0].sequence, recent[0].revision, if args.light { "light" } else { "dark" })).map_err(|e| e.to_string())?;
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
        "allocation-monitor: PASS; one attachment painted actual completion beside live peer, panel closed before peer release, both programs succeeded and ledger drained"
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_capacity::host_protocol::{Generation, Job, Phase};

    #[test]
    fn scene_requires_exact_owner_completion_beside_current_snapshot() {
        let snapshot = Snapshot {
            generation: Generation([7; 16]),
            revision: 7,
            closed: false,
            jobs: vec![],
        };
        let recent = [RecordSummary {
            revision: 6,
            attempt: Some(8),
            kind: EventKind::Finished {
                succeeded: true,
                cancelled: false,
            },
        }];
        let scene = Scene {
            snapshot: &snapshot,
            recent: &recent,
        };
        let base = concat!(
            "text key=\"allocation/monitor/scope\" label=\"Delegated native session · read-only observation\"\n",
            "text key=\"allocation/monitor/attachment\" label=\"Monitor attachment 4: Observed\"\n",
            "text key=\"allocation/monitor/source\" label=\"Source generation: 07070707070707070707070707070707 · snapshot revision 7 · attachment 4\"\n",
        );
        let completion = "text key=\"allocation/monitor/completion/6\" label=\"Session attempt 8 · Finished · success established · completion revision 6\"\n";
        let text = format!("{base}{completion}");
        assert!(matches_scene(&text, &scene, 4));
        assert!(!matches_scene(base, &scene, 4), "missing actual outcome");
        assert!(!matches_scene(&format!("{text}{completion}"), &scene, 4));
        for (from, to) in [
            ("Session attempt 8", "Session attempt 80"),
            ("success established", "success not established"),
            (
                " · completion revision",
                " · cancellation requested · completion revision",
            ),
            ("completion revision 6", "completion revision 5"),
            ("completion/6", "completion/5"),
        ] {
            assert!(
                !matches_scene(&text.replace(from, to), &scene, 4),
                "reject {to}"
            );
        }
        assert!(
            !matches_scene(
                &text,
                &Scene {
                    snapshot: &snapshot,
                    recent: &[]
                },
                4
            ),
            "extra outcome"
        );
        let audit = format!(
            "{}{}",
            text.replace("text key=", "paint-node visible=true clipped=false key="),
            text.replace(
                "text key=",
                "paint-text overflow=false clip-truncated=false key="
            )
            .replace("label=", "painted=")
        );
        assert!(matches_scene(&audit, &scene, 4));
        assert!(
            !matches_scene(
                &audit.replace(
                    "painted=\"Session attempt 8",
                    "painted=\"Session attempt 80"
                ),
                &scene,
                4
            ),
            "paint differs from semantic outcome"
        );
    }

    #[test]
    fn view_parser_uses_authoritative_tab_index_and_allocation_leaf() {
        assert_eq!(
            allocation_view("tab id=99 index=2\n leaf view=12 kind=app app=allocation\n"),
            Some((12, 2))
        );
        assert_eq!(
            allocation_view("tab id=99 index=2\n leaf view=12 kind=app app=editor\n"),
            None
        );
    }
    #[test]
    fn rendered_witness_requires_actual_generation_attempts_and_admitted_bytes() {
        let expected = Snapshot {
            generation: Generation([7; 16]),
            revision: 3,
            closed: false,
            jobs: vec![Job {
                attempt: 9,
                phase: Phase::Waiting,
                reserved_bytes: 123456,
                failure: None,
            }],
        };
        let text = concat!(
            "text key=\"allocation/monitor/scope\" label=\"Delegated native session · read-only observation\"\n",
            "text key=\"allocation/monitor/attachment\" label=\"Monitor attachment 4: Observed\"\n",
            "text key=\"allocation/monitor/source\" label=\"Source generation: 07070707070707070707070707070707 · snapshot revision 3 · attachment 4\"\n",
            "text key=\"allocation/monitor/attempt/9\" label=\"Session attempt 9 · Waiting for next child observation · admitted peak 123456 bytes (0.0 GiB)\"\n",
        );
        assert!(matches_snapshot(text, &expected, 4));
        let audit = format!(
            "{}{}",
            text.replace("text key=", "paint-node visible=true clipped=false key="),
            text.replace(
                "text key=",
                "paint-text overflow=false clip-truncated=false key="
            )
            .replace("label=", "painted=")
        );
        assert!(
            matches_snapshot(&audit, &expected, 4),
            "semantic and painted rows are separate matching witnesses"
        );
        for (valid, invalid) in [
            ("visible=true", "visible=false"),
            ("clipped=false", "clipped=true"),
            ("overflow=false", "overflow=true"),
            ("clip-truncated=false", "clip-truncated=true"),
        ] {
            assert!(
                !matches_snapshot(&audit.replace(valid, invalid), &expected, 4),
                "reject {invalid}"
            );
        }
        assert!(!matches_snapshot(&audit.replace("painted=\"Session attempt 9 · Waiting for next child observation · admitted peak 123456", "painted=\"Session attempt 9 · Waiting for next child observation · admitted peak 654321"), &expected, 4));
        assert!(!matches_snapshot(
            &text.replace("snapshot revision 3", "snapshot revision 2"),
            &expected,
            4
        ));
        assert!(!matches_snapshot(
            &text.replace("123456", "123455"),
            &expected,
            4
        ));
        assert!(!matches_snapshot(
            &text.replace("attempt 9", "attempt 8"),
            &expected,
            4
        ));
        assert!(!matches_snapshot(
            &text.replace(
                "07070707070707070707070707070707",
                "00000000000000000000000000000000"
            ),
            &expected,
            4
        ));
        assert!(!matches_snapshot(text, &expected, 5));
        assert!(!matches_snapshot(
            &text.replace("· attachment 4\"", "· attachment 40\""),
            &expected,
            4
        ));
        assert!(!matches_snapshot(
            &text.replace("Waiting for next child observation", "Faulted"),
            &expected,
            4
        ));
        assert!(!matches_snapshot(
            &format!(
                "{text}\ntext key=\"allocation/monitor/attempt/10\" label=\"Session attempt 10 · Waiting for next child observation · admitted peak 999 bytes (0.0 GiB)\""
            ),
            &expected,
            4
        ));
        assert!(!matches_snapshot(
            &text.replace("attempt 9 ·", "attempt 90 ·"),
            &expected,
            4
        ));
        assert!(!matches_snapshot(
            &text.replace("· admitted peak", "\n· admitted peak"),
            &expected,
            4
        ));
        assert!(
            !matches_snapshot(&format!("{text}{text}"), &expected, 4),
            "duplicate semantic keys refuse"
        );
    }
}
