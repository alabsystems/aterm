// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The build slots' lanes (`docs/DESIGN-harness-occupancy-2026-10-20.md`):
//! what `aterm_sysprobe::slots` reads, held to the real
//! `build-slot` and to a real aterm.
//!
//! * `slot-capture`: the record capture (design §10 Q8's unread-format
//!   guard). The real `build-slot` (`~/.local/bin/build-slot`, else
//!   `$HOME/trust/scripts/build-slot`; a named skip with neither) admits `true`
//!   into a slot directory of the lane's own scratch world (`BUILD_SLOT_DIR`,
//!   never `BUILD_SLOTS`, no worktree lock, no memory floor); its `slot-0`
//!   must parse ([`aterm_sysprobe::slots::Record::parse`]) as a record of
//!   the wrapper's pid, started between the spawn and the exit, spelled as
//!   Python's `json.dumps` spells it, `cmd` `true` and `cwd` the scratch
//!   directory (which the lab checks and the reader drops). A mismatch is a
//!   FAIL naming the field: `build-slot` changed its record.
//! * `slot-capture --watch <dur>`: the slice's exit, run for a day. Every
//!   [`aterm_sysprobe::slots::PERIOD`] the real slot directory is read with
//!   the reader (read only, no lock) and with `lsof` (read only); a holder the
//!   reader proves that `lsof` does not show, a record's own pid `lsof` shows
//!   holding its slot on two reads running that the reader does not prove, and
//!   a slot file that appeared, empty and unheld, across the reader's own read
//!   each FAIL. An hourly line says how it stands.
//! * `slot-lane`: the `lab instance` test of `await sys slots>=1`. A headless
//!   instance whose scratch HOME holds one slot, `slot-0`, taken by a stand-in
//!   (this program again, `slot-hold`: it opens, locks, truncates and writes a
//!   Python-spelled record as `build-slot` does, says `held` and parks): the
//!   wait times out while the slot is held, and answers `OK sys slots=1/1`
//!   once the stand-in is `SIGKILL`ed, which happens only after the
//!   instance's log says that wait armed the slots' cadence
//!   ([`aterm_sysprobe::slots::ARMED_LINE`]). The stand-in starts no build, so no
//!   orphan's witness can discount the slot whatever the machine's load.
//!
//! Every wait for what must happen is a one-minute hang detector (the
//! stand-in's first exec, [`aterm_uds::deadline::FIRST_EXEC_HANG`]). Only
//! the processes the lane started are signalled. Run by hand, never by a
//! default test run.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_sysprobe::slots::{Machine, Record, RecordRead, SlotReader, SlotsRead, Unread};
use aterm_uds::spawnfd::{Child, Command, Stdio};

use crate::instance::{
    BOOT, CONFIG_OFF, Instance, Launch, Reply, World, resolve_aterm, take_value,
};
use crate::paths::paths;
use crate::pyjson::J;
use crate::{Fail, LAB, Res, stop};

/// A one-minute hang detector for what must happen.
const MUST: Duration = Duration::from_secs(60);

/// An early return or unwind keeps the scratch while a child's outcome may
/// still be unknown. Only the ordinary, fully collected path releases it.
struct LaneWorld {
    world: World,
    settled: bool,
}

impl LaneWorld {
    fn new() -> Result<Self, String> {
        Ok(Self {
            world: World::new(CONFIG_OFF)?,
            settled: false,
        })
    }
}

impl std::ops::Deref for LaneWorld {
    type Target = World;

    fn deref(&self) -> &World {
        &self.world
    }
}

impl Drop for LaneWorld {
    fn drop(&mut self) {
        if !self.settled {
            self.world.keep();
        }
    }
}

/// A pipe drained on the calling thread, so there is no detached reader to
/// outlive the world. Each turn reads at most one chunk and keeps a bounded
/// tail; a busy writer cannot keep the deadline from being checked.
struct Pipe {
    file: std::fs::File,
    bytes: Vec<u8>,
    eof: bool,
}

impl Pipe {
    fn new(fd: OwnedFd) -> Result<Self, String> {
        let file = std::fs::File::from(fd);
        crate::sys::set_nonblocking(file.as_raw_fd())
            .map_err(|e| format!("make child pipe nonblocking: {e}"))?;
        Ok(Self {
            file,
            bytes: Vec::new(),
            eof: false,
        })
    }

    fn read(&mut self) -> Result<(), String> {
        if self.eof {
            return Ok(());
        }
        let mut chunk = [0; 8192];
        match self.file.read(&mut chunk) {
            Ok(0) => self.eof = true,
            Ok(n) => {
                self.bytes.extend_from_slice(&chunk[..n]);
                let cut = self
                    .bytes
                    .len()
                    .saturating_sub(aterm_uds::deadline::OUTPUT_KEEP);
                self.bytes.drain(..cut);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(format!("read child pipe: {e}")),
        }
        Ok(())
    }
}

/// Ownership is armed immediately after spawn, before pipe setup or any
/// other fallible operation. Drop signals only the direct, unreaped child
/// and uses the existing exit watcher for a bounded reap reserve. This is
/// no claim that arbitrary descendants have ended.
struct Running {
    child: Child,
    out: Option<Pipe>,
    err: Option<Pipe>,
    status: Option<ExitStatus>,
}

impl Running {
    fn start(cmd: &mut Command) -> Result<Self, String> {
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("start child: {e}"))?;
        let mut run = Self {
            child,
            out: None,
            err: None,
            status: None,
        };
        let out = run.child.stdout.take().ok_or("child has no stdout")?;
        run.out = Some(Pipe::new(out.into())?);
        let err = run.child.stderr.take().ok_or("child has no stderr")?;
        run.err = Some(Pipe::new(err.into())?);
        Ok(run)
    }

    fn step(&mut self) -> Result<(), String> {
        for pipe in [&mut self.out, &mut self.err].into_iter().flatten() {
            pipe.read()?;
        }
        if self.status.is_none() {
            self.status = self.child.try_wait().map_err(|e| format!("wait: {e}"))?;
        }
        Ok(())
    }

    fn collected(&self) -> bool {
        self.status.is_some()
            && [&self.out, &self.err]
                .iter()
                .all(|p| p.as_ref().is_some_and(|p| p.eof))
    }

    fn finish(&mut self, hang: Duration) -> Result<Output, String> {
        let end = Instant::now() + hang;
        loop {
            self.step()?;
            if self.collected() {
                return Ok(Output {
                    status: self.status.expect("collected child"),
                    stdout: std::mem::take(&mut self.out.as_mut().expect("stdout").bytes),
                    stderr: std::mem::take(&mut self.err.as_mut().expect("stderr").bytes),
                });
            }
            if Instant::now() >= end {
                return Err(format!(
                    "hang detector: child or pipes not done within {hang:?}"
                ));
            }
            stop::sleep(Duration::from_millis(20)).map_err(|_| "interrupted".to_owned())?;
        }
    }

    fn end(&mut self) -> Result<(), String> {
        if self.status.is_some() {
            return Ok(());
        }
        let killed = self.child.kill();
        let mut exit = aterm_uds::deadline::ExitWait::new(&self.child);
        let end = Instant::now() + MUST;
        loop {
            self.status = self
                .child
                .try_wait()
                .map_err(|e| format!("reap child: {e}"))?;
            if self.status.is_some() {
                return Ok(());
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("child not reaped after SIGKILL ({killed:?})"));
            }
            exit.wait(left);
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Err(why) = self.end() {
            eprintln!("slot lane: cleanup unknown: {why}");
        }
    }
}

/// The stand-in's line once it holds its slot.
const HELD: &str = "held";

/// The hidden verb the stand-in runs as.
pub const HOLD_VERB: &str = "slot-hold";

/// The PATH `build-slot` runs under: its `#!/usr/bin/env python3` finds the
/// python a person's shell would before the system's.
const CAPTURE_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

/// Microseconds since the epoch now.
fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_micros()).ok())
        .unwrap_or(0)
}

/// The lane's verdict: each check named, each a pass or why not.
#[derive(Default)]
struct Verdict {
    failed: usize,
    checks: usize,
}

impl Verdict {
    fn check(&mut self, name: &str, ok: bool, why: impl FnOnce() -> String) {
        self.checks += 1;
        if ok {
            println!("  PASS {name}");
        } else {
            self.failed += 1;
            println!("  FAIL {name}: {}", why());
        }
    }

    /// The verb's exit: 0 when every check passed.
    fn end(&self, verb: &str) -> i32 {
        if self.failed == 0 {
            println!("{verb}: PASS ({} checks)", self.checks);
            0
        } else {
            println!("{verb}: FAIL ({} of {} checks)", self.failed, self.checks);
            1
        }
    }
}

/// The real `build-slot`, where this host has one.
fn find_build_slot() -> Option<PathBuf> {
    let p = paths();
    let homes = [p.caller_home.as_str(), p.real_home.as_str()];
    homes
        .iter()
        .filter(|h| !h.is_empty())
        .flat_map(|h| {
            [
                Path::new(h).join(".local/bin/build-slot"),
                Path::new(h).join("trust/scripts/build-slot"),
            ]
        })
        .find(|c| crate::sys::executable(&c.to_string_lossy()))
}

/// `<dur>` as `<n>s`, `<n>m` or `<n>h`.
fn duration_of(v: &str) -> Option<Duration> {
    let (n, unit) = v.split_at_checked(v.len().checked_sub(1)?)?;
    let n: u64 = n.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n.checked_mul(60)?,
        "h" => n.checked_mul(3600)?,
        _ => return None,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

/// `slot-capture [--watch <dur>] [--dir DIR]` (the module docs).
///
/// # Errors
/// Usage, or the world could not be made.
pub fn cmd_slot_capture(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let watch = take_value(&mut a, "--watch")?;
    let dir = take_value(&mut a, "--dir")?;
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "slot-capture: unexpected '{extra}' ({LAB} help)"
        )));
    }
    if let Some(w) = watch {
        let Some(span) = duration_of(&w) else {
            return Err(Fail::usage(format!(
                "slot-capture: --watch takes <n>s, <n>m or <n>h, not '{w}'"
            )));
        };
        let dir = dir.map(PathBuf::from).or_else(|| {
            let home = [&paths().caller_home, &paths().real_home]
                .into_iter()
                .find(|h| !h.is_empty())?;
            Some(aterm_sysprobe::slots::default_dir(Path::new(home)))
        });
        let Some(dir) = dir else {
            println!("slot-capture: NOT RUN (skip: no home to find the slots under)");
            return Ok(3);
        };
        return watch_slots(&dir, span);
    }
    if dir.is_some() {
        return Err(Fail::usage("slot-capture: --dir goes with --watch"));
    }
    let Some(build_slot) = find_build_slot() else {
        println!("slot-capture: NOT RUN (skip: no build-slot in ~/.local/bin or $HOME/trust/scripts)");
        return Ok(3);
    };
    let mut world = LaneWorld::new()?;
    let slots = world.dir.join("slots");
    let cwd = world.dir.join("work");
    std::fs::create_dir_all(&cwd).map_err(|e| format!("cannot make {}: {e}", cwd.display()))?;
    let cwd = std::fs::canonicalize(&cwd).map_err(|e| format!("{}: {e}", cwd.display()))?;
    println!(
        "slot-capture: {} -- true, BUILD_SLOT_DIR={}",
        build_slot.display(),
        slots.display()
    );
    let spawned_us = now_us();
    let mut cmd = aterm_uds::spawnfd::command(&build_slot);
    cmd.env_clear()
        .env("HOME", &world.home)
        .env("PATH", CAPTURE_PATH)
        .env("BUILD_SLOT_DIR", &slots)
        .args(["--no-worktree-lock", "--mem-gb", "0", "--", "true"])
        .current_dir(&cwd);
    let mut child = Running::start(&mut cmd)?;
    let wrapper = child.child.id();
    let status = child.finish(MUST)?.status.code();
    let exited_us = now_us();
    let mut v = Verdict::default();
    v.check(
        "build-slot ran `true` to its end (exit 0)",
        status == Some(0),
        || format!("exit {status:?}"),
    );
    let raw = std::fs::read(slots.join("slot-0")).unwrap_or_default();
    let text = String::from_utf8_lossy(&raw).into_owned();
    println!("  slot-0: {text}");
    match Record::parse(&raw) {
        RecordRead::Rec(r) => {
            v.check(
                "the record's pid is the wrapper's",
                r.pid == wrapper,
                || format!("pid {} (the wrapper was {wrapper})", r.pid),
            );
            v.check(
                "the record's started falls between the spawn and the exit",
                r.started_us + 1_000_000 >= spawned_us && r.started_us <= exited_us + 1_000_000,
                || {
                    format!(
                        "started {} us, spawned {spawned_us}, exited {exited_us}",
                        r.started_us
                    )
                },
            );
        }
        other => v.check("slot-0 parses as a record", false, || {
            format!("{other:?} (build-slot changed its record: the reader reads it unread_format)")
        }),
    }
    let doc = aterm_json::from_slice::<aterm_json::Value>(&raw).ok();
    let field = |k: &str| doc.as_ref().and_then(|d| d[k].as_str().map(str::to_owned));
    v.check(
        "the record's cmd is `true`",
        field("cmd").as_deref() == Some("true"),
        || format!("cmd {:?}", field("cmd")),
    );
    v.check(
        "the record's cwd is the scratch directory",
        field("cwd").as_deref() == cwd.to_str(),
        || format!("cwd {:?}, ran in {}", field("cwd"), cwd.display()),
    );
    v.check(
        "the record is spelled as Python's json.dumps spells it",
        text.starts_with("{\"pid\": ") && text.contains(", \"started\": "),
        || format!("{text:?}"),
    );
    world.settled = true;
    Ok(v.end("slot-capture"))
}

/// One slot file's identity and record, read without a lock.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SlotFile {
    path: PathBuf,
    /// `(size, mtime ns, inode)`.
    stamp: (u64, i128, u64),
    /// The record's pid, where it parses.
    pid: Option<u32>,
}

/// The slot files of `dir` (`slot-<digits>`, regular files), by name.
fn slot_files(dir: &Path) -> Vec<SlotFile> {
    use std::os::unix::fs::MetadataExt as _;
    let mut out: Vec<SlotFile> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name().to_str().is_some_and(|n| {
                n.strip_prefix("slot-")
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .filter_map(|e| {
            let path = e.path();
            let m = std::fs::symlink_metadata(&path).ok()?;
            m.file_type().is_file().then(|| {
                let pid = match Record::parse(&std::fs::read(&path).unwrap_or_default()) {
                    RecordRead::Rec(r) => Some(r.pid),
                    RecordRead::Empty | RecordRead::Bad => None,
                };
                SlotFile {
                    stamp: (
                        m.size(),
                        i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()),
                        m.ino(),
                    ),
                    path,
                    pid,
                }
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Which pids `lsof` shows holding each of `files` open (read only).
fn lsof_holders(files: &[SlotFile]) -> Result<Vec<(PathBuf, u32)>, String> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let mut cmd = aterm_uds::spawnfd::command("/usr/sbin/lsof");
    cmd.args(["-n", "-F", "pn", "--"])
        .args(files.iter().map(|f| f.path.as_os_str()));
    let out = Running::start(&mut cmd)?.finish(MUST)?;
    // lsof exits 1 when none of the files is open.
    if !matches!(out.status.code(), Some(0 | 1)) {
        return Err(format!(
            "lsof failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let mut held = Vec::new();
    let mut pid = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if let (Some(name), Some(p)) = (line.strip_prefix('n'), pid) {
            held.push((PathBuf::from(name), p));
        }
    }
    Ok(held)
}

/// `--watch`: the reader beside `lsof` over the real slots for `span`.
fn watch_slots(dir: &Path, span: Duration) -> Res {
    println!(
        "slot-capture --watch: {} for {} s, a read every {} s",
        dir.display(),
        span.as_secs(),
        aterm_sysprobe::slots::PERIOD.as_secs()
    );
    let mut reader = SlotReader::new(dir.to_path_buf());
    let start = Instant::now();
    let mut next_summary = start + Duration::from_secs(3600);
    let (mut reads, mut fails) = (0u64, 0u64);
    // The slots, each with its record's pid, `lsof` showed that pid holding
    // last read (a new holder of the same slot starts its count over).
    let mut seen_once: Vec<(PathBuf, u32)> = Vec::new();
    let mut last = SlotsRead::Unread(Unread::NotSampled);
    while start.elapsed() < span {
        let before = slot_files(dir);
        let load = aterm_sysprobe::load_average();
        let machine = Machine {
            load5: load.map(|l| l[1]),
            ncpu: std::thread::available_parallelism().ok().map(usize::from),
        };
        // Armed, as a wait on the slots arms the host's reader.
        let read = reader.read(Instant::now(), now_us(), &machine, true);
        let after = slot_files(dir);
        let held = lsof_holders(&after)?;
        reads += 1;
        let mut fail = |why: String| {
            fails += 1;
            println!("  FAIL read {reads}: {why}");
        };
        let proven: Vec<u32> = match &read {
            SlotsRead::Read { holders, .. } => holders.iter().map(|h| h.pid).collect(),
            SlotsRead::Unread(_) => Vec::new(),
        };
        // A holder that ended between the reader's read and lsof's let its
        // slot go: only one still alive after lsof is one lsof missed.
        for pid in &proven {
            if !held.iter().any(|(_, p)| p == pid) && aterm_uds::process::pid_alive(*pid) {
                fail(format!(
                    "the reader proves pid {pid} a holder; lsof shows it holding no slot"
                ));
            }
        }
        let mut seen_now = Vec::new();
        for f in &after {
            // Its record stood before the reader's read (a holder that came
            // between the read and lsof is the next read's first sighting).
            let own = f.pid.is_some_and(|pid| {
                before.iter().any(|b| b.path == f.path && b.pid == f.pid)
                    && held.iter().any(|(path, p)| *p == pid && path == &f.path)
            });
            if own && let Some(pid) = f.pid {
                if seen_once.contains(&(f.path.clone(), pid)) && !proven.contains(&pid) {
                    fail(format!(
                        "lsof shows {} held by its record's pid {pid} on two reads; the reader does not prove it",
                        f.path.display()
                    ));
                }
                seen_now.push((f.path.clone(), pid));
            }
            if !before.iter().any(|b| b.path == f.path)
                && f.stamp.0 == 0
                && !held.iter().any(|(path, _)| path == &f.path)
            {
                fail(format!(
                    "{} appeared, empty and unheld, across the reader's read",
                    f.path.display()
                ));
            }
        }
        seen_once = seen_now;
        if read != last {
            println!("  read {reads}: {read:?}");
            last = read;
        }
        if Instant::now() >= next_summary {
            println!(
                "slot-capture --watch: {} h, {reads} reads, {fails} failed",
                start.elapsed().as_secs() / 3600
            );
            next_summary += Duration::from_secs(3600);
        }
        stop::sleep(aterm_sysprobe::slots::PERIOD)?;
    }
    println!("slot-capture --watch: {reads} reads, {fails} failed");
    Ok(i32::from(fails > 0))
}

/// `slot-hold <file>` (hidden): the stand-in holder. It takes `<file>` as
/// `build-slot` does (open, `LOCK_EX | LOCK_NB`, truncate, one write of a
/// Python-spelled record of its pid and now), says [`HELD`], and parks until
/// it is `SIGKILL`ed, when the kernel drops its lock.
pub fn hold_main(args: &[String]) -> i32 {
    use std::os::unix::fs::OpenOptionsExt as _;
    let [slot] = args else {
        eprintln!("aterm-lab: {HOLD_VERB} <file>");
        return 2;
    };
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o644)
        .open(slot);
    let Ok(mut file) = file else {
        eprintln!("aterm-lab: {HOLD_VERB}: cannot open {slot}");
        return 1;
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the stand-in plays build-slot's holder: it takes the slot's flock as build-slot \
                  does and keeps it until it is SIGKILLed, when the kernel drops it"
    )]
    let locked = file.try_lock();
    if locked.is_err() {
        eprintln!("aterm-lab: {HOLD_VERB}: {slot} is held");
        return 1;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let record = J::obj([
        ("pid", J::Int(i64::from(std::process::id()))),
        ("cmd", J::s(HOLD_VERB)),
        ("cwd", J::s(cwd.to_string_lossy())),
        ("started", J::Float(now_us() as f64 / 1e6)),
    ])
    .py();
    if file.set_len(0).is_err() || file.write_all(record.as_bytes()).is_err() {
        eprintln!("aterm-lab: {HOLD_VERB}: cannot write {slot}");
        return 1;
    }
    println!("{HELD}");
    let _ = std::io::stdout().flush();
    loop {
        std::thread::park();
    }
}

/// The stand-in on `slot`, working in `dir`; back once it holds the slot
/// (its first exec waited under [`aterm_uds::deadline::FIRST_EXEC_HANG`]).
fn start_holder(world: &World, slot: &Path, dir: &Path) -> Result<Running, String> {
    let env = world.env();
    let mut cmd = aterm_uds::spawnfd::command(&paths().exe);
    cmd.env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .arg(HOLD_VERB)
        .arg(slot)
        .current_dir(dir);
    let mut child = Running::start(&mut cmd)?;
    let end = Instant::now() + aterm_uds::deadline::FIRST_EXEC_HANG;
    loop {
        child.step()?;
        let out = child.out.as_ref().expect("stdout");
        if out
            .bytes
            .split_inclusive(|b| *b == b'\n')
            .any(|line| line == b"held\n")
        {
            return Ok(child);
        }
        if out.eof || Instant::now() >= end {
            return Err("the stand-in never said it held its slot".into());
        }
        stop::sleep(Duration::from_millis(20)).map_err(|_| "interrupted".to_owned())?;
    }
}

/// `slot-lane [--aterm PATH] [--keep]` (the module docs).
///
/// # Errors
/// Usage, or the world, the stand-in or the instance could not be made.
pub fn cmd_slot_lane(args: &[String]) -> Res {
    stop::install();
    let mut a = args.to_vec();
    let aterm = take_value(&mut a, "--aterm")?;
    let keep = crate::instance::take_flag(&mut a, "--keep");
    if let Some(extra) = a.first() {
        return Err(Fail::usage(format!(
            "slot-lane: unexpected '{extra}' ({LAB} help)"
        )));
    }
    let aterm = resolve_aterm(aterm).map_err(|e| Fail::exit(3, e))?;
    let mut world = LaneWorld::new()?;
    if keep {
        world.world.keep();
    }
    // The instance's reader looks in `<HOME>/.cache/build-slot`.
    let slots = aterm_sysprobe::slots::default_dir(&world.home);
    let holder_dir = world.dir.join("holder");
    for d in [&slots, &holder_dir] {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot make {}: {e}", d.display()))?;
    }
    let slot = slots.join("slot-0");
    let mut holder = start_holder(&world, &slot, &holder_dir)?;
    println!(
        "slot-lane: stand-in pid {} holds {}",
        holder.child.id(),
        slot.display()
    );
    let mut inst = Instance::start(
        &world,
        Launch {
            aterm,
            headless: true,
            args: vec![
                "--columns".into(),
                "100".into(),
                "--lines".into(),
                "30".into(),
            ],
            env: world.env(),
            cwd: world.home.clone(),
            sandbox: None,
        },
    )?;
    let ran = await_socket(&mut inst, &world).and_then(|()| lane(&inst, &world, &mut holder));
    inst.stop();
    let ended = holder.end().and_then(|()| holder.finish(MUST).map(|_| ()));
    let instance_ended = !inst.running();
    if ran.is_ok() && ended.is_ok() && instance_ended {
        world.settled = true;
    }
    ended?;
    if !instance_ended {
        return Err("the instance's exit could not be confirmed; its world was kept".into());
    }
    match ran {
        Ok(Some(v)) => Ok(v.end("slot-lane")),
        Ok(None) => Ok(3),
        Err(why) => {
            println!("slot-lane: FAIL (the lane did not run to its end: {why})");
            Ok(1)
        }
    }
}

/// The waits against the running `inst` (the module docs); `None`: a named
/// skip.
fn lane(inst: &Instance, world: &World, holder: &mut Running) -> Result<Option<Verdict>, String> {
    let ls = ctl(inst, world, &["ls"])?;
    let sid = ls
        .stdout
        .split_whitespace()
        .find(|w| w.starts_with("s-"))
        .map(str::to_owned)
        .ok_or_else(|| format!("no session in `ls`: {}", ls.stdout))?;
    let at = format!("@{sid}");
    let sys = ctl(inst, world, &[&at, "sys"])?;
    if sys.code != 0 {
        println!(
            "slot-lane: NOT RUN (skip: `sys` does not answer on a headless instance: {})",
            sys.first()
        );
        return Ok(None);
    }
    let mut v = Verdict::default();
    // (a) Held: the wait times out. Fifteen seconds outlast the reader's
    // first reads (one sighting is pending, the second, a period later,
    // proves the holder).
    let held = ctl(
        inst,
        world,
        &[&at, "await", "sys", "slots>=1", "timeout=15000"],
    )?;
    println!(
        "  while held: await sys slots>=1 timeout=15000 -> {}",
        held.first()
    );
    v.check(
        "while the stand-in holds the one slot, the wait times out (exit 124)",
        held.code == 124 && held.stdout.trim() == "OK timeout",
        || {
            format!(
                "exit {}: {} {}",
                held.code,
                held.stdout.trim(),
                held.stderr.trim()
            )
        },
    );
    // (b) Freed: the wait armed first, then the stand-in SIGKILLed. The
    // armed lines (a)'s wait left, counted before this one is sent.
    let armed_before = crate::util::scratch_text(&world.state().join("logs/aterm.log"))
        .matches(aterm_sysprobe::slots::ARMED_LINE)
        .count();
    let mut control = ctl_start(
        inst,
        world,
        &[&at, "await", "sys", "slots>=1", "timeout=60000"],
    )?;
    // The wait is armed before the slot frees: the instance logs the slots'
    // armed cadence when this wait, the only one, arms it. Once the stand-in
    // dies the reader needs two reads a period apart to call the slot free,
    // so the wait's first turn reads it held.
    let log = world.state().join("logs/aterm.log");
    let armed_lines = |text: &str| text.matches(aterm_sysprobe::slots::ARMED_LINE).count();
    let deadline = Instant::now() + MUST;
    while armed_lines(&crate::util::scratch_text(&log)) < armed_before + 1 {
        control.step()?;
        if control.collected() {
            return Err("the ctl client ended before the slots wait armed".into());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "hang detector: the wait never armed the slots (no `{}` in {})",
                aterm_sysprobe::slots::ARMED_LINE,
                log.display()
            ));
        }
        stop::sleep(Duration::from_millis(50)).map_err(|_| "interrupted".to_owned())?;
    }
    holder.end()?;
    let reply = control.finish(MUST + Duration::from_secs(30))?;
    let code = reply.status.code().unwrap_or(-1);
    let answer = String::from_utf8_lossy(&reply.stdout);
    println!(
        "  freed: await sys slots>=1 timeout=60000 -> {}",
        answer.trim()
    );
    v.check(
        "once the stand-in is SIGKILLed, the wait answers OK sys slots=1/1",
        code == 0 && answer.starts_with("OK sys slots=1/1 "),
        || format!("exit {code}: {}", answer.trim()),
    );
    Ok(Some(v))
}

/// A private instance client, owned before any wait for its answer or the
/// instance's armed witness. All pipe reads stay on this thread.
fn ctl_start(inst: &Instance, world: &World, args: &[&str]) -> Result<Running, String> {
    let env = world.env();
    let mut cmd = aterm_uds::spawnfd::command(&inst.client);
    cmd.env_clear()
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .current_dir(&world.home)
        .args([
            "ctl",
            "--sock",
            &inst.sock.to_string_lossy(),
            "--timeout",
            "90",
        ])
        .args(args);
    Running::start(&mut cmd)
}

/// Readiness uses the same owned client as the lane, including when the
/// socket exists but its version reply never arrives. The child is held to
/// the remaining boot deadline, followed only by its cleanup reserve.
fn await_socket(inst: &mut Instance, world: &World) -> Result<(), String> {
    let end = Instant::now() + BOOT;
    loop {
        if !inst.running() {
            return Err(format!(
                "the instance exited before its socket answered; its log:\n{}",
                inst.log_tail(20)
            ));
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!(
                "the instance never answered on {} within {BOOT:?}; its log:\n{}",
                inst.sock.display(),
                inst.log_tail(20)
            ));
        }
        if inst.sock.exists()
            && ctl_start(inst, world, &["version"])?
                .finish(left)?
                .status
                .success()
        {
            return Ok(());
        }
        stop::sleep(Duration::from_millis(100)).map_err(|_| "interrupted".to_owned())?;
    }
}

fn ctl(inst: &Instance, world: &World, args: &[&str]) -> Result<Reply, String> {
    let out = ctl_start(inst, world, args)?.finish(MUST + Duration::from_secs(30))?;
    Ok(Reply {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::duration_of;
    use std::time::Duration;

    #[test]
    fn watch_duration_requires_a_positive_bounded_number_and_an_ascii_unit() {
        for (text, seconds) in [("1s", 1), ("30s", 30), ("20m", 1200), ("24h", 86400)] {
            assert_eq!(
                duration_of(text),
                Some(Duration::from_secs(seconds)),
                "{text}"
            );
        }
        for text in [
            "",
            "s",
            "0s",
            "1",
            "-1s",
            "1é",
            "é",
            "1😀",
            "1秒",
            "18446744073709551616s",
            "18446744073709551615m",
            "18446744073709551615h",
        ] {
            assert_eq!(duration_of(text), None, "{text}");
        }
        assert_eq!(
            duration_of("18446744073709551615s"),
            Some(Duration::from_secs(u64::MAX))
        );
    }
}
