// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `disk-full`: a volume filled to zero recovers unattended (DC-06a). On a sparse APFS image
//! filled under its emergency line and then, once the instance runs, to zero (`ENOSPC`), a
//! private instance's disk watch releases the reserve before any other row, removes what a build
//! that died left (an unfinished incremental session and a metadata folder) and keeps every
//! finished unit and every source byte; on a second image whose ballast holds the volume under
//! the line with nothing to take, it releases its reserve, writes exactly one `cannot-meet` row
//! across two passes and removes nothing. A headless instance supervises no agent, so its watch
//! reaches a checkout only for a session that asks for room: each pass here is one a shell tab's
//! `await sys` runs, from the folder the scenario judges.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::disk_lane::{
    HOST_FREE_MIN, Image, Verdict, aterm_control_pct, fill, free_on, lay_source, lay_target,
};
use crate::instance::{
    BOOT, CONFIG_OFF, Instance, Launch, World, config_after, resolve_aterm, take_flag, take_value,
};
use crate::{Fail, LAB, Res, stop};

/// A wait for what must happen: a hang detector, not a budget.
const MUST: Duration = Duration::from_secs(60);

/// How long the ballast scenario waits before it asks for room again: past the minute in which
/// the window answers an ask the last pass spent with no pass of its own (`SPENT_RECENT`).
const ASK_AGAIN: Duration = Duration::from_secs(65);

/// The image's size (`hdiutil`'s spelling).
const IMAGE: &str = "8g";

/// The free bytes the fill leaves for the instance to start in: under any volume's emergency
/// line. Once it runs, [`fill_to_zero`] takes the rest.
const LEFT: u64 = 256 << 20;

/// The reserve aterm keeps, at its least.
const RESERVE: u64 = 256 << 20;

const DISK: &[&str] = &["[disk]", "warn_free_gib = 6", "auto_free_gib = 5"];

/// Which image a run fills.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// A checkout whose build died, and the reserve.
    Died,
    /// Ballast alone: no build, no cache.
    Ballast,
}

impl Scenario {
    const fn name(self) -> &'static str {
        match self {
            Scenario::Died => "died",
            Scenario::Ballast => "ballast",
        }
    }
}

/// Run the `disk-full` lane.
///
/// # Errors
/// Returns an error on invalid usage or if the aterm to run cannot be found.
pub fn cmd_disk_full(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let keep = take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "disk-full: unexpected '{extra}' ({LAB} help)"
        )));
    }
    if !Path::new("/usr/bin/hdiutil").exists() {
        println!("disk-full: NOT RUN (skip: no hdiutil on this host)");
        return Ok(3);
    }
    match free_on(Path::new("/tmp")) {
        Some(f) if f >= HOST_FREE_MIN => {}
        free => {
            println!(
                "disk-full: NOT RUN (skip: {} free here, the lane needs {} GiB so it cannot be what fills the disk)",
                free.map_or_else(|| "unknown".to_owned(), |f| format!("{} GiB", f >> 30)),
                HOST_FREE_MIN >> 30
            );
            return Ok(3);
        }
    }
    let aterm = resolve_aterm(aterm).map_err(|e| Fail::exit(3, e))?;
    let mut v = Verdict::default();
    for scenario in [Scenario::Died, Scenario::Ballast] {
        if let Err(why) = run(&aterm, keep, scenario, &mut v) {
            v.check(
                &format!("{}: the lane ran to its end", scenario.name()),
                false,
                || why,
            );
        }
    }
    if v.passed() {
        println!("disk-full: PASS ({} checks)", v.rows.len());
        Ok(0)
    } else {
        println!("disk-full: FAIL");
        Ok(1)
    }
}

/// The folder or file at `path` dated `ago` before now.
fn backdate(path: &Path, ago: Duration) -> Result<(), String> {
    let when = std::time::SystemTime::now() - ago;
    std::fs::File::open(path)
        .and_then(|f| f.set_modified(when))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The rows of the ledger under `home`.
fn ledger(home: &Path) -> Vec<String> {
    let path = home
        .join("Library/Application Support/aterm/harness")
        .join(aterm_agent::harness::cli::HARNESS)
        .join(aterm_agent::harness::cli::DISK_LEDGER);
    crate::util::read_regular_text(&path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The rows of `rows` of `kind`.
fn of_kind<'a>(rows: &'a [String], kind: &str) -> impl Iterator<Item = (usize, &'a String)> {
    let want = format!("\"kind\":\"{kind}\"");
    rows.iter()
        .enumerate()
        .filter(move |(_, r)| r.contains(&want))
}

/// Fill the volume under `path` to zero: write `path` until the volume refuses a byte more
/// (`ENOSPC`), as a build that filled the disk leaves it. The bytes written.
fn fill_to_zero(path: &Path) -> Result<u64, String> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f =
        crate::util::create_regular_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut written = 0u64;
    for size in [1usize << 20, 4 << 10] {
        let chunk = vec![0x5au8; size];
        loop {
            match f.write(&chunk) {
                Ok(0) => break,
                Ok(n) => written += n as u64,
                Err(e) if e.kind() == std::io::ErrorKind::StorageFull => break,
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
    }
    match aterm_durable::ordered(&f) {
        Err(e) if e.kind() != std::io::ErrorKind::StorageFull => {
            Err(format!("{}: {e}", path.display()))
        }
        _ => Ok(written),
    }
}

/// Wait up to [`MUST`] for `done` to hold.
fn wait(mut done: impl FnMut() -> bool) -> Result<bool, String> {
    let deadline = Instant::now() + MUST;
    while Instant::now() < deadline {
        if done() {
            return Ok(true);
        }
        stop::sleep(Duration::from_millis(500)).map_err(|_| "interrupted".to_owned())?;
    }
    Ok(done())
}

fn run(aterm: &Path, keep: bool, scenario: Scenario, v: &mut Verdict) -> Result<(), String> {
    let name = scenario.name();
    let disk: Vec<String> = DISK.iter().map(|l| (*l).to_owned()).collect();
    let mut world = World::new(&config_after(CONFIG_OFF, true, &disk))?;
    if keep {
        world.keep();
    }
    let image = Image::attach_at(
        &world.dir.join("full.sparseimage"),
        &world.dir.join("mnt"),
        IMAGE,
        "atermdiskfull",
    )?;
    let home = image_home(&world.dir.join("mnt"));
    world.rehome(&home)?;
    let work = home.join("work");
    let session = work.join("target/debug/incremental/lay-1/s-old1-died1-working");
    let meta = work.join("target/debug/deps/rmetaAb12Cd");
    let mut finished: Vec<PathBuf> = Vec::new();
    let mut source: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    if scenario == Scenario::Died {
        lay_source(&work)?;
        lay_target(&work.join("target"))?;
        fill(&session.join("query-cache.bin"), 8 << 20)?;
        crate::util::write_fresh(&session.with_file_name("s-old1-died1.lock"), b"")
            .map_err(|e| format!("lock: {e}"))?;
        fill(&meta.join("lib.rmeta"), 8 << 20)?;
        backdate(&session, Duration::from_secs(3_600))?;
        backdate(&meta, Duration::from_secs(3_600))?;
        for entry in std::fs::read_dir(work.join("target/debug/deps"))
            .map_err(|e| format!("deps: {e}"))?
            .flatten()
        {
            if entry.path() != meta {
                finished.push(entry.path());
            }
        }
        for rel in ["Cargo.toml", "src/main.rs", ".git/index"] {
            let p = work.join(rel);
            let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            source.push((p, bytes));
        }
    } else {
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    }
    fill(
        &home.join("Library/Caches/aterm/disk-reserve.noindex/reserve"),
        RESERVE,
    )?;
    let free = free_on(&home).ok_or("the image's free space could not be read")?;
    if free > LEFT {
        fill(&home.join("ballast/one"), free - LEFT)?;
    }
    println!(
        "disk-full: {name}: fixtures laid; {} MiB free",
        free_on(&home).unwrap_or(0) >> 20
    );
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm: aterm.to_path_buf(),
            headless: true,
            args: vec![
                "--columns".into(),
                "110".into(),
                "--lines".into(),
                "32".into(),
            ],
            env: world
                .env()
                .into_iter()
                .chain([(
                    aterm_agent::harness::disk::WATCH_SEAM.to_owned(),
                    "1".to_owned(),
                )])
                .collect(),
            cwd: home.clone(),
            sandbox: None,
            reroute: false,
        },
    )?;
    let judged = (|| -> Result<(), String> {
        inst.await_socket(BOOT).map_err(|e| format!("{e:?}"))?;
        if inst.ctl(&["ready"]).code != 0 {
            return Err("the instance is not ready".to_owned());
        }
        let at = enter(&inst, &work)?;
        let filled = fill_to_zero(&home.join("ballast/zero"))?;
        println!(
            "disk-full: {name}: filled to zero ({} MiB more); {} MiB free",
            filled >> 20,
            free_on(&home).unwrap_or(0) >> 20
        );
        let asked = ask_for_room(&inst, &at, &home);
        println!("disk-full: {name}: await sys free>=5GiB -> {asked}");
        match scenario {
            Scenario::Died => judge_died(&home, &session, &meta, &finished, &source, v),
            Scenario::Ballast => judge_ballast(&inst, &at, &home, v),
        }
    })();
    let cleanup = inst.stop();
    drop(image);
    cleanup?;
    judged
}

/// The home on the image mounted at `mnt`.
fn image_home(mnt: &Path) -> PathBuf {
    mnt.join("home")
}

/// The instance's one session, as `@<sid>`, its shell moved into `cwd` and its working
/// directory told to the window (OSC 7), so an ask for room from it reaches that folder.
fn enter(inst: &Instance, cwd: &Path) -> Result<String, String> {
    let cwd = std::fs::canonicalize(cwd).map_err(|e| format!("{}: {e}", cwd.display()))?;
    let ls = inst.ctl(&["ls"]);
    let sid = ls
        .stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .map(str::to_owned)
        .ok_or_else(|| format!("no session in `ls`: {}", ls.stdout))?;
    let at = format!("@{sid}");
    let line = format!(
        "cd '{}' && printf '\\033]7;file://localhost%s\\007' \"$PWD\" && echo CWD-$((6*7))\n",
        cwd.display()
    );
    let _ = inst.ctl_stdin(&[&at, "send", "--stdin"], line.as_bytes());
    if !wait(|| inst.ctl(&[&at, "text"]).stdout.contains("CWD-42"))? {
        return Err(format!("the session did not enter {}", cwd.display()));
    }
    Ok(at)
}

/// The session `at` waits for 5 GiB free on the image under `home` (`await sys`), which pokes
/// the watch to look and run a pass at once: what it answered, once that pass ended or the
/// minute ran out.
fn ask_for_room(inst: &Instance, at: &str, home: &Path) -> String {
    let vol = format!("vol={}", aterm_control_pct(&home.to_string_lossy()));
    let reply = inst.ctl(&[at, "await", "sys", "free>=5GiB", &vol, "timeout=60000"]);
    format!(
        "exit {}: {}{}",
        reply.code,
        reply.stdout.trim(),
        reply.stderr.trim()
    )
}

fn judge_died(
    home: &Path,
    session: &Path,
    meta: &Path,
    finished: &[PathBuf],
    source: &[(PathBuf, Vec<u8>)],
    v: &mut Verdict,
) -> Result<(), String> {
    let gone = wait(|| !session.exists() && !meta.exists())?;
    v.check(
        "died: the died build's partial output is gone",
        gone,
        || {
            format!(
                "session there: {}, metadata folder there: {}",
                session.exists(),
                meta.exists()
            )
        },
    );
    let rows = ledger(home);
    let released = rows
        .iter()
        .position(|r| r.contains("\"kind\":\"reserve\"") && r.contains("released"));
    let first = rows
        .iter()
        .position(|r| !(r.contains("\"kind\":\"reserve\"") || r.contains("\"kind\":\"poke\"")));
    v.check(
        "died: the reserve's released row precedes every other row of the pass",
        released.is_some_and(|at| first.is_none_or(|first| at < first)),
        || format!("released at {released:?}, the first other row at {first:?}"),
    );
    let died: Vec<usize> = of_kind(&rows, "removing")
        .filter(|(_, r)| r.contains("\"witness\":\"died-build\""))
        .map(|(at, _)| at)
        .collect();
    let others = of_kind(&rows, "removing")
        .filter(|(_, r)| !r.contains("\"witness\":\"died-build\""))
        .map(|(at, _)| at)
        .min();
    v.check(
        "died: each piece was journalled before it went, ahead of any finished unit",
        died.len() == 2 && others.is_none_or(|o| died.iter().all(|d| *d < o)),
        || format!("died rows at {died:?}, the first other at {others:?}"),
    );
    let lost: Vec<&PathBuf> = finished.iter().filter(|p| !p.exists()).collect();
    v.check("died: every finished unit stays", lost.is_empty(), || {
        format!("gone: {lost:?}")
    });
    let changed: Vec<&PathBuf> = source
        .iter()
        .filter(|(p, bytes)| std::fs::read(p).ok().as_ref() != Some(bytes))
        .map(|(p, _)| p)
        .collect();
    v.check(
        "died: every source fixture is byte-identical",
        changed.is_empty(),
        || format!("changed: {changed:?}"),
    );
    Ok(())
}

fn judge_ballast(inst: &Instance, at: &str, home: &Path, v: &mut Verdict) -> Result<(), String> {
    let said = wait(|| of_kind(&ledger(home), "cannot-meet").count() > 0)?;
    v.check("ballast: a cannot-meet row is written", said, || {
        "none within a minute".to_owned()
    });
    let released = of_kind(&ledger(home), "reserve").any(|(_, r)| r.contains("released"));
    v.check("ballast: the reserve was released", released, || {
        "no released row".to_owned()
    });
    // An ask moves the emergency's wait, so a second one runs a pass of its own; the memo keeps
    // that pass's row out.
    stop::sleep(ASK_AGAIN).map_err(|_| "interrupted".to_owned())?;
    let asked = ask_for_room(inst, at, home);
    println!("disk-full: ballast: await sys free>=5GiB again -> {asked}");
    let passed = wait(|| of_kind(&ledger(home), "report").count() >= 2)?;
    let rows = ledger(home);
    let reports = of_kind(&rows, "report").count();
    v.check(
        "ballast: the second ask ran a pass of its own",
        passed && reports == 2,
        || format!("{reports} report rows"),
    );
    let unmet = of_kind(&rows, "cannot-meet").count();
    v.check(
        "ballast: exactly one cannot-meet row across both passes",
        unmet == 1,
        || format!("{unmet} rows"),
    );
    let removing = of_kind(&rows, "removing").count();
    v.check("ballast: nothing is removed", removing == 0, || {
        format!("{removing} removing rows")
    });
    if let Some((_, row)) = of_kind(&rows, "cannot-meet").next() {
        v.check(
            "ballast: the row is the emergency's",
            row.contains("\"by\":\"emergency\""),
            || row.clone(),
        );
    }
    Ok(())
}
