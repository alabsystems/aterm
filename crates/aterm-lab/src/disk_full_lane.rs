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
//!
//! `--scenario custody` (DC-06d): on a third image filled under its emergency line, with an
//! agent's worktree whose session is gone (nothing in it written for eight days) holding an edit
//! and an untracked file, the instance's worktree step journals one `deferred` row and writes no
//! snapshot ref while the volume reads under the line; once the ballast goes (the recovery), the
//! first look above the line pins it, pushes its snapshot to the lab's server (a bare repository
//! on the image, so nothing reads the network) and removes it, and the edit and the file are
//! reachable from the server's ref. No ask for room runs there: an agent's wait moves the
//! emergency's pass ahead of the step, so the instance's own looks drive it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::disk_lane::{
    HOST_FREE_MIN, Image, Verdict, aterm_control_pct, backdate as backdate_tree, fill, free_on,
    lay_source, lay_target,
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

/// The free bytes the custody scenario's fill leaves: under the image's emergency line (4 GiB
/// under [`DISK`]) with the reserve released too, and room for the instance's own writes.
const CUSTODY_LEFT: u64 = 512 << 20;

/// How long after its start the instance's disk watch first looks (`DISK_FIRST_LOOK` in
/// `aterm-gui`'s harness host).
const FIRST_LOOK: Duration = Duration::from_secs(120);

/// How often the instance's disk watch looks after its first
/// (`aterm_agent::supervise::occupancy::reading::LOOK_EVERY_MS`).
const TICK: Duration = Duration::from_millis(60_000);

/// How old the custody worktree's files and git's own files for it are made: past the seven days
/// of quiet the worktree step holds a worktree no build-start line names to.
const QUIET_DAYS: u64 = 8;

/// Which image a run fills.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    /// A checkout whose build died, and the reserve.
    Died,
    /// Ballast alone: no build, no cache.
    Ballast,
    /// An agent's worktree whose session is gone, holding uncommitted work (DC-06d).
    Custody,
}

impl Scenario {
    const ALL: [Scenario; 3] = [Scenario::Died, Scenario::Ballast, Scenario::Custody];

    const fn name(self) -> &'static str {
        match self {
            Scenario::Died => "died",
            Scenario::Ballast => "ballast",
            Scenario::Custody => "custody",
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
    let scenarios: Vec<Scenario> = match take_value(&mut a, "--scenario")? {
        None => Scenario::ALL.to_vec(),
        Some(name) => vec![
            Scenario::ALL
                .into_iter()
                .find(|s| s.name() == name)
                .ok_or_else(|| {
                    Fail::usage(format!(
                        "disk-full: no scenario '{name}' (died, ballast or custody)"
                    ))
                })?,
        ],
    };
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
    for scenario in scenarios {
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
    let mut custody: Option<Custody> = None;
    if scenario == Scenario::Custody {
        custody = Some(lay_custody(&world.dir.join("mnt"), &home)?);
    } else if scenario == Scenario::Died {
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
    // The reserve as aterm lays it: its folder, which holds the pass lock too, private to its
    // owner. A folder open to others fails the pass lock's check, and the watch's step then
    // stops short at its first worktree ("the pass lock was lost").
    let reserve = home.join("Library/Caches/aterm/disk-reserve.noindex/reserve");
    fill(&reserve, RESERVE)?;
    if let Some(dir) = reserve.parent() {
        crate::util::set_dir_mode_nofollow(dir, 0o700)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let free = free_on(&home).ok_or("the image's free space could not be read")?;
    let left = if scenario == Scenario::Custody {
        CUSTODY_LEFT
    } else {
        LEFT
    };
    if free > left {
        fill(&home.join("ballast/one"), free - left)?;
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
        if let Some(custody) = &custody {
            return judge_custody(inst.pid, &home, custody, v);
        }
        let at = enter(&inst, &work)?;
        let filled = fill_to_zero(&home.join("ballast/zero"))?;
        println!(
            "disk-full: {name}: filled to zero ({} MiB more); {} MiB free",
            filled >> 20,
            free_on(&home).unwrap_or(0) >> 20
        );
        let asked = ask_for_room(&inst, &at, &home);
        println!("disk-full: {name}: await sys free>=1GiB -> {asked}");
        match scenario {
            Scenario::Died => judge_died(&home, &session, &meta, &finished, &source, v),
            Scenario::Ballast => judge_ballast(&inst, &at, &home, v),
            Scenario::Custody => Ok(()),
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
    // `ls` names each session in its third column (`<window> <tab> <sid> …`); a sid is no
    // longer spelled `s-…`.
    let sid = ls
        .stdout
        .lines()
        .find_map(|line| line.split_whitespace().nth(2))
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

/// The session `at` waits for 1 GiB free on the image under `home` (`await sys`), which pokes
/// the watch to look and run a pass at once: what it answered, once that pass ended or the
/// minute ran out. A wait is refused (`why=too-large`) above what the volume's cap leaves room
/// for, about 2 GiB on this image, so it asks for less than that.
fn ask_for_room(inst: &Instance, at: &str, home: &Path) -> String {
    let vol = format!("vol={}", aterm_control_pct(&home.to_string_lossy()));
    let reply = inst.ctl(&[at, "await", "sys", "free>=1GiB", &vol, "timeout=60000"]);
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
    println!("disk-full: ballast: await sys free>=1GiB again -> {asked}");
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

/// The custody scenario's fixtures: the lab's server, a checkout of it, and one agent's
/// worktree whose session is gone, with the work only that worktree holds.
struct Custody {
    /// The lab's server: a bare repository on the image, the checkout's `origin`.
    server: PathBuf,
    /// The main checkout, a child of the home.
    checkout: PathBuf,
    /// The agent's worktree, `Developer/.ubd/<name>.noindex` under the home.
    worktree: PathBuf,
    /// The name its pins carry (`refs/snapshots/<name>/…`, the step's own
    /// `worktree::pins::ref_name`).
    name: String,
    /// Each file only the worktree holds, by its path in it, with its bytes.
    work: Vec<(String, String)>,
}

/// `git <args>` in `dir`, as a person set the fixture up, reading none of this host's git
/// settings: its trimmed output.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = aterm_uds::spawnfd::command("/usr/bin/git")
        .args([
            "-c",
            "user.name=lab",
            "-c",
            "user.email=lab@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Lay the custody fixtures on the image mounted at `mnt` under `home`: the server
/// (`mnt/server.git`), a checkout `home/proj` whose `main` it holds, and a worktree of it where
/// an agent makes one, on its own branch, with an edit and an untracked file, everything in it
/// and in the checkout's git directory dated [`QUIET_DAYS`] back. Its name is new each run, so
/// no transcript of a session open on this host names it (one that did would keep it).
fn lay_custody(mnt: &Path, home: &Path) -> Result<Custody, String> {
    let made = |p: &Path| std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()));
    let write = |p: &Path, text: &str| {
        crate::util::write_fresh(p, text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))
    };
    let server = mnt.join("server.git");
    let checkout = home.join("proj");
    made(&server)?;
    git(&server, &["init", "-q", "--bare"])?;
    made(&checkout.join("src"))?;
    git(&checkout, &["init", "-q"])?;
    for (key, value) in [
        ("core.excludesFile", "/dev/null"),
        ("core.attributesFile", "/dev/null"),
        ("core.autocrlf", "false"),
    ] {
        git(&checkout, &["config", key, value])?;
    }
    write(&checkout.join("Cargo.toml"), "[package]\nname = \"proj\"\n")?;
    write(&checkout.join("src/lib.rs"), "// main\n")?;
    git(&checkout, &["add", "Cargo.toml", "src/lib.rs"])?;
    git(&checkout, &["commit", "-qm", "main"])?;
    git(
        &checkout,
        &["remote", "add", "origin", &server.to_string_lossy()],
    )?;
    git(&checkout, &["push", "-q", "origin", "main"])?;
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
        ^ u128::from(std::process::id());
    let name = format!("proj-{:x}.noindex", tag & 0xffff_ffff_ffff);
    let worktree = home.join("Developer/.ubd").join(&name);
    made(worktree.parent().ok_or("the worktree has no parent")?)?;
    let branch = format!("agent/{}", name.trim_end_matches(".noindex"));
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            &worktree.to_string_lossy(),
            "main",
        ],
    )?;
    let work = vec![
        ("src/lib.rs".to_owned(), format!("// edited by {name}\n")),
        ("notes.txt".to_owned(), format!("notes of {name}\n")),
    ];
    for (rel, text) in &work {
        write(&worktree.join(rel), text)?;
    }
    backdate_tree(&worktree, QUIET_DAYS)?;
    backdate_tree(&checkout.join(".git"), QUIET_DAYS)?;
    Ok(Custody {
        server,
        checkout,
        name: aterm_agent::harness::disk::worktree::pins::ref_name(&worktree),
        worktree,
        work,
    })
}

/// The `worktree` rows of the ledger under `home` that name `wt`, parsed.
fn worktree_rows(home: &Path, wt: &Path) -> Vec<aterm_json::Value> {
    let want = wt.to_string_lossy();
    of_kind(&ledger(home), aterm_agent::harness::disk::WORKTREE_ROW_KIND)
        .filter_map(|(_, r)| aterm_json::from_str::<aterm_json::Value>(r).ok())
        .filter(|r| r["path"].as_str() == Some(&*want))
        .collect()
}

/// The refs of the repository `dir` that name the custody worktree's pins
/// (`refs/snapshots/<name>/…`, `refs/orphans/<name>/…`), each `<ref> <commit>`.
fn pins(dir: &Path, name: &str) -> Result<Vec<String>, String> {
    let mut all = Vec::new();
    for kind in ["refs/snapshots", "refs/orphans"] {
        let listed = git(
            dir,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                &format!("{kind}/{name}/"),
            ],
        )?;
        all.extend(listed.lines().map(str::to_owned));
    }
    Ok(all)
}

/// What the worktree step's judgment says of the custody worktree, judged here as the
/// instance's step judges it (its own open Claude Code sessions, discovery and the pinned rule;
/// no session's working directory, roster line or build-start line): the reason a run that
/// failed reads where the instance journals none, since a judgment that keeps a worktree is
/// not on the ledger.
fn why_judged(home: &Path, c: &Custody) -> String {
    use aterm_agent::harness::disk::{discover, worktree};
    use atpkg::build_cache::retired::GitUntil;
    let open = worktree::open_sessions(home);
    let found = discover::linked_worktrees(home);
    let known: Vec<PathBuf> = found
        .iter()
        .flat_map(|(checkout, wt)| [checkout.clone(), wt.clone()])
        .collect();
    let table = aterm_sysprobe::volumes::table();
    let transcripts = home.join(".claude/projects");
    let owed = home.join(worktree::pins::OWED_FILE);
    let facts = worktree::Facts {
        home,
        transcripts_root: Some(&transcripts),
        cwds: &[],
        cwd_unknown: None,
        open: &open,
        table: Some(&table),
        known: &known,
        roster: Some(&[]),
        starts: &[],
        owed: Some(&owed),
        build_dirs: None,
        build_rested: &[],
    };
    let wall = Instant::now() + worktree::STEP_WALL;
    let git = GitUntil(wall + worktree::GIT_GRACE);
    let plan = worktree::plan(
        &git,
        &facts,
        &found,
        None,
        std::time::SystemTime::now(),
        wall,
    );
    let verdict = plan
        .judged
        .iter()
        .find(|(_, wt, _)| *wt == c.worktree)
        .map_or_else(
            || format!("discovery found it not among {} worktree(s)", found.len()),
            |(_, _, verdict)| format!("judged here: {verdict:?}"),
        );
    let mut kinds: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for row in ledger(home) {
        if let Ok(row) = aterm_json::from_str::<aterm_json::Value>(&row) {
            let kind = row["kind"].as_str().unwrap_or("?");
            let action = row["action"].as_str().unwrap_or("");
            *kinds.entry(format!("{kind}/{action}")).or_default() += 1;
        }
    }
    format!(
        "{verdict}; the ledger's rows by kind: {kinds:?}; {} open Claude Code session(s){}",
        open.len(),
        plan.unfinished
            .map_or_else(String::new, |why| format!("; unfinished: {why}"))
    )
}

/// How long [`wait_for`] waits on an instance that keeps working without the answer: a step
/// that works forever, not a slow one.
const WORKING_GUARD: Duration = Duration::from_secs(15 * 60);

/// Wait for `done` to hold: through `schedule`, the instance's own clock (its first look, each
/// tick, a step's wall), and then for as long as the instance (`pid`) and its descendants work,
/// ending unanswered after a [`MUST`] in which they used no CPU and did no I/O, once the instance
/// is gone, or after [`WORKING_GUARD`] of work. The hang detector is the instance's progress, not the wall, which a
/// loaded Mac stretches: a look that runs late or a step whose git runs long keeps the wait open.
fn wait_for(pid: i32, schedule: Duration, mut done: impl FnMut() -> bool) -> Result<bool, String> {
    let root = u32::try_from(pid).map_err(|_| format!("no instance pid ({pid})"))?;
    let began = Instant::now();
    let scheduled = began + schedule;
    let mut tree = aterm_uds::deadline::ProcessProgressProbe::default();
    let mut seen = None;
    let mut worked_at = began;
    loop {
        if done() {
            return Ok(true);
        }
        let at = Instant::now();
        if matches!(
            aterm_uds::process::process_presence(pid),
            aterm_uds::process::ProcessPresence::Absent
                | aterm_uds::process::ProcessPresence::Exited
        ) {
            // What it did as it ended may be the answer.
            return Ok(done());
        }
        // A tree that does not read, or does not compare, is no evidence of a stall.
        match tree.read(root) {
            Some(generation) if seen == Some(generation) => {}
            reading => {
                seen = reading;
                worked_at = at;
            }
        }
        let quiet = at.saturating_duration_since(worked_at.max(scheduled));
        if (at >= scheduled && quiet >= MUST)
            || at.saturating_duration_since(scheduled) >= WORKING_GUARD
        {
            return Ok(done());
        }
        stop::sleep(Duration::from_secs(2)).map_err(|_| "interrupted".to_owned())?;
    }
}

fn judge_custody(pid: i32, home: &Path, c: &Custody, v: &mut Verdict) -> Result<(), String> {
    use aterm_agent::harness::disk::{WORKTREE_DEFERRED, worktree};
    let deferred = |rows: &[aterm_json::Value]| {
        rows.iter()
            .filter(|r| r["action"].as_str() == Some(WORKTREE_DEFERRED))
            .count()
    };
    // The first look runs the emergency's pass; the next, its pass not due, runs the step.
    let seen = wait_for(pid, FIRST_LOOK + TICK * 2, || {
        deferred(&worktree_rows(home, &c.worktree)) > 0
    })?;
    v.check(
        "custody: under the line the step journals its worktree deferred",
        seen,
        || {
            format!(
                "no deferred row; its rows: {:?}; {}",
                worktree_rows(home, &c.worktree),
                why_judged(home, c)
            )
        },
    );
    let free = free_on(home).unwrap_or(0);
    println!("disk-full: custody: deferred at {} MiB free", free >> 20);
    // A look or two more under the line: the wait is journalled once.
    stop::sleep(TICK * 2).map_err(|_| "interrupted".to_owned())?;
    let rows = worktree_rows(home, &c.worktree);
    v.check(
        "custody: one deferred row while the volume stays under the line",
        deferred(&rows) == 1,
        || format!("{} deferred rows: {rows:?}", deferred(&rows)),
    );
    let local = pins(&c.checkout, &c.name)?;
    let served = pins(&c.server, &c.name)?;
    v.check(
        "custody: no snapshot ref under the line, here or at the server",
        local.is_empty() && served.is_empty(),
        || format!("checkout {local:?}, server {served:?}"),
    );
    v.check(
        "custody: the worktree stands under the line",
        c.worktree.is_dir()
            && !rows
                .iter()
                .any(|r| r["action"].as_str() == Some("removing")),
        || format!("rows: {rows:?}"),
    );
    // The recovery: what filled the volume goes, as a build's output or a download would.
    let ballast = home.join("ballast/one");
    std::fs::remove_file(&ballast).map_err(|e| format!("{}: {e}", ballast.display()))?;
    println!(
        "disk-full: custody: ballast removed; {} MiB free",
        free_on(home).unwrap_or(0) >> 20
    );
    let gone = wait_for(
        pid,
        TICK * 2 + worktree::STEP_WALL + worktree::GIT_GRACE,
        || {
            worktree_rows(home, &c.worktree)
                .iter()
                .any(|r| r["action"].as_str() == Some("removed"))
                && !c.worktree.exists()
        },
    )?;
    v.check(
        "custody: above the line the worktree is pinned and gone",
        gone,
        || {
            format!(
                "standing: {}; its rows: {:?}; {}",
                c.worktree.exists(),
                worktree_rows(home, &c.worktree),
                why_judged(home, c)
            )
        },
    );
    let served = pins(&c.server, &c.name)?;
    let snapshot = served
        .iter()
        .filter_map(|line| line.split_once(' ').map(|(name, _)| name.to_owned()))
        .find(|name| name.starts_with("refs/snapshots/"));
    v.check(
        "custody: its snapshot is on the lab's server",
        snapshot.is_some(),
        || {
            let pushes: Vec<String> = of_kind(
                &ledger(home),
                aterm_agent::harness::disk::WORKTREE_PUSH_ROW_KIND,
            )
            .map(|(_, r)| r.clone())
            .collect();
            format!(
                "the server's pins: {served:?}; the checkout's: {:?}; push rows: {pushes:?}",
                pins(&c.checkout, &c.name)
            )
        },
    );
    if let Some(at) = snapshot {
        let lost: Vec<&String> = c
            .work
            .iter()
            .filter(|(rel, text)| {
                git(&c.server, &["show", &format!("{at}:{rel}")])
                    .ok()
                    .as_deref()
                    != Some(text.trim_end())
            })
            .map(|(rel, _)| rel)
            .collect();
        v.check(
            "custody: every edit is reachable from the server's ref",
            lost.is_empty(),
            || format!("not at {at}: {lost:?}"),
        );
    }
    Ok(())
}
