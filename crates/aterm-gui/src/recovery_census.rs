// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RECOVERY CENSUS: at each windowed launch of the installed app, one row in the
//! recovery ledger saying how the run before it ended (`aterm_update::recovery_ledger`,
//! the PTY keeper's P0, `docs/DESIGN-pty-keeper-2026-09-26.md` §7).
//!
//! IT READS WHAT IS ALREADY THERE, off the main thread, after the crash scans:
//!
//! * THE CRASH MARKER ([`crate::crash_signal::markers`]) — one per run, named for its
//!   pid and birth, removed by every clean exit. Non-empty: a fatal signal, whose
//!   number the handler wrote and whose mtime is the moment of death. Empty and
//!   dead: a kill. Gone: a clean exit.
//! * THE PANIC REPORT (`crash-<pid>.log`, [`crate::logging`]) — its first line
//!   carries the moment it was filed.
//! * `aterm.log` — every line carries its time and its writer's pid. The watchdog's
//!   `MAIN-THREAD STALL` lines and the `STALL ENDED` line say whether the run's last
//!   word was an unended stall ([`crate::watchdog`]); the pressure source's lines say
//!   which level the OS last reported ([`crate::memory_pressure`]); the final exit's
//!   one line dates a quit; the run's last line bounds a kill.
//! * THE BOOT SENTINEL (`aterm_update::trial_launch_count`) — whether the previous
//!   build was still inside its boot trial (§2 F17).
//!
//! The crash scans (`logging::take_crash_evidence` / `take_kill_evidence`) consume
//! their artifacts by renaming them `.seen`, so the census reads both names. Every
//! artifact is tied to its run by pid AND by time — pids are recycled — so a report
//! older than the run's birth is never that run's.
//!
//! WHICH LAUNCHES WRITE: the ones whose crash marker is the daily driver's
//! ([`crate::crash_signal::MarkerOwner::App`]) — a cold windowed start of the
//! installed app, and an update successor once its Commit lands. An update's cold
//! lane (no session open) exec's the new build IN PLACE: that image keeps the pid
//! and the birth and boots as a cold start, and its row records the image it
//! replaced as handed off — never the run before the process a second time. Test,
//! dev, headless and translocated starts share the log directory and are SIGKILLed
//! by harnesses routinely; they write nothing, exactly as they report nothing.
//!
//! WHAT IT CANNOT TELL: a run whose marker was never created (no private log dir)
//! reads as clean, and so does a killed run whose consumed marker the crash scans
//! pruned before an installed launch read it (they keep the newest `.seen` artifacts
//! by mtime, and an empty marker's mtime is its run's birth); a run that filed a panic report on a background thread and then
//! quit reads as a panic, as the crash banner does; a second instance still alive at
//! the next launch is recorded as running and its own end is never classified, since
//! each row looks one run back. And the pid-and-time tie excludes only an EARLIER
//! owner of the pid: a process that reuses a dead run's pid before the next launch
//! and writes `aterm.log` lines or a panic report lends them to the run.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aterm_update::recovery_ledger::{
    EndClass, LaunchKind, Pressure, PrevRun, Row, Tri, Uptime, append_row, ledger_path, read_rows,
};

/// The final exit's log line (lib.rs `main_entry`), which dates a clean quit.
pub(crate) const QUIT_LINE: &str = "final exit: quitting cleanly";

/// The watchdog's stall lines, first report and repeats (`watchdog::stall_message`).
const STALL_LINE: &str = "MAIN-THREAD STALL";
/// The watchdog's line when a reported stall ends (`watchdog::stall_ended_message`).
pub(crate) const STALL_ENDED_LINE: &str = "MAIN-THREAD STALL ENDED";
/// The pressure source's own line (`memory_pressure::level_line`).
pub(crate) const PRESSURE_LINE: &str = "memory pressure: the system reports";
/// The main thread's line when it sheds under pressure (older builds' only record).
const SHED_LINE_CRITICAL: &str = "memory pressure (critical)";
const SHED_LINE_WARN: &str = "memory pressure (warn)";

/// The most bytes of the logs one census reads: both rotation halves are about
/// 4 MiB each (`logging::RotationBudget::ATERM_LOG`), with slack.
const MAX_LOG_BYTES: u64 = 12 * 1024 * 1024;

/// This launch, as its row records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Me {
    pub(crate) pid: u32,
    pub(crate) started: u64,
    pub(crate) build: u64,
    pub(crate) version: String,
}

impl Me {
    fn current() -> Self {
        let pid = std::process::id();
        Self {
            pid,
            started: own_birth().unwrap_or_else(now_secs),
            build: crate::running_build_number(),
            version: crate::running_version().to_string(),
        }
    }
}

/// What the census asks the system, so tests can answer instead.
pub(crate) trait Probe {
    /// The birth (Unix seconds) of a live process of ours with this pid.
    fn birth(&self, pid: u32) -> Option<u64>;
    /// Launches the boot sentinel has counted against `build`.
    fn trial(&self, build: u64) -> u32;
}

struct System;

impl Probe for System {
    fn birth(&self, pid: u32) -> Option<u64> {
        process_birth(pid)
    }
    fn trial(&self, build: u64) -> u32 {
        aterm_update::trial_launch_count(build)
    }
}

#[cfg(target_os = "macos")]
fn process_birth(pid: u32) -> Option<u64> {
    crate::seamless::read_process_birth(libc::pid_t::try_from(pid).ok()?).map(|b| b.seconds)
}

#[cfg(not(target_os = "macos"))]
fn process_birth(_pid: u32) -> Option<u64> {
    None
}

fn own_birth() -> Option<u64> {
    process_birth(std::process::id())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Record this launch in the ledger, on a thread of its own: the log scan reads up
/// to [`MAX_LOG_BYTES`], and nothing on the launch path waits for it. Call AFTER the
/// crash scans, whose renames it reads. A no-op without a private log directory.
pub(crate) fn record(launch: LaunchKind) {
    let spawned = std::thread::Builder::new()
        .name("aterm-recovery-census".into())
        .spawn(move || {
            // Nobody waits on the census: it reads files and appends one line.
            crate::qos::set_self(crate::qos::Role::Background);
            let Some(dir) = crate::logging::log_dir() else {
                return;
            };
            let row = census_row(&dir, &Me::current(), launch, now_secs(), &System);
            let path = ledger_path(&dir);
            match append_row(&path, &row) {
                Ok(()) => aterm_log::info!(
                    "recovery census: the run before this launch ({}) ended {}",
                    row.prev
                        .pid
                        .map_or_else(|| "none".into(), |p| format!("pid {p}")),
                    row.prev.class.token()
                ),
                Err(e) => aterm_log::warn!(
                    "recovery census: could not append to {}: {e}",
                    path.display()
                ),
            }
        });
    if let Err(e) = spawned {
        aterm_log::warn!("recovery census: no thread for the census: {e}");
    }
}

/// This launch's row: `me`, and the run before it as `dir` shows it now.
pub(crate) fn census_row(
    dir: &Path,
    me: &Me,
    launch: LaunchKind,
    now: u64,
    probe: &dyn Probe,
) -> Row {
    let rows = read_rows(&ledger_path(dir));
    let is_me = |row: &Row| row.pid == me.pid && row.started == me.started;
    let own_image = rows.last().filter(|row| is_me(row)).cloned();
    let last = rows.into_iter().rev().find(|row| !is_me(row));
    let prev = match (launch, own_image, last) {
        // The newest row is THIS process's own: an update's cold lane exec'd a new
        // image in place (same pid, same kernel birth), and that image is booting
        // now. Its previous run is the image it replaced, which handed off to an
        // update; the run before THAT was classified by the old image's own row, and
        // is never counted twice.
        (LaunchKind::Cold, Some(own), _) => handoff_prev(dir, Some(&own), now, probe),
        (LaunchKind::Successor, _, last) => handoff_prev(dir, last.as_ref(), now, probe),
        (LaunchKind::Cold, None, Some(last)) => cold_prev(dir, &last, probe),
        (LaunchKind::Cold, None, None) => first_row_prev(dir),
    };
    Row {
        at: now,
        pid: me.pid,
        started: me.started,
        build: me.build,
        version: me.version.clone(),
        launch,
        prev,
    }
}

/// The run that handed off to this launch: the parent of a successor at its Commit,
/// or the image a cold-lane update exec replaced. It ends as this launch begins.
fn handoff_prev(dir: &Path, last: Option<&Row>, now: u64, probe: &dyn Probe) -> PrevRun {
    let Some(last) = last else {
        return PrevRun {
            class: EndClass::Handoff,
            ..PrevRun::none()
        };
    };
    let facts = scan_logs(dir, last.pid, last.started);
    PrevRun {
        pid: Some(last.pid),
        build: Some(last.build),
        version: Some(last.version.clone()),
        class: EndClass::Handoff,
        uptime: Some(Uptime {
            secs: now.saturating_sub(last.started),
            at_least: false,
        }),
        stall: facts.stall(),
        pressure: facts.pressure(),
        trial: probe.trial(last.build),
    }
}

/// A cold start: classify the run the ledger's last row describes.
fn cold_prev(dir: &Path, last: &Row, probe: &dyn Probe) -> PrevRun {
    let facts = scan_logs(dir, last.pid, last.started);
    let evidence = find_evidence(dir, last.pid, last.started);
    let running = probe.birth(last.pid) == Some(last.started);
    let (class, end) = if running {
        (EndClass::Running, None)
    } else {
        classify(&evidence, &facts)
    };
    PrevRun {
        pid: Some(last.pid),
        build: Some(last.build),
        version: Some(last.version.clone()),
        class,
        uptime: end.map(|(end, at_least)| Uptime {
            secs: end.saturating_sub(last.started),
            at_least,
        }),
        stall: facts.stall(),
        pressure: facts.pressure(),
        trial: probe.trial(last.build),
    }
}

/// The ledger's first row: no earlier row names a run, but a crash artifact may.
fn first_row_prev(dir: &Path) -> PrevRun {
    let Some((pid, birth)) = newest_artifact_owner(dir) else {
        return PrevRun::none();
    };
    let facts = scan_logs(dir, pid, birth);
    let evidence = find_evidence(dir, pid, birth);
    let (class, end) = classify(&evidence, &facts);
    PrevRun {
        pid: Some(pid),
        class,
        uptime: end.and_then(|(end, at_least)| {
            (birth > 0).then(|| Uptime {
                secs: end.saturating_sub(birth),
                at_least,
            })
        }),
        stall: facts.stall(),
        pressure: facts.pressure(),
        ..PrevRun::none()
    }
}

/// The end class and, when it can be told, the end time (with `true` when it is only
/// a lower bound).
fn classify(evidence: &Evidence, facts: &LogFacts) -> (EndClass, Option<(u64, bool)>) {
    if let Some(at) = evidence.panic_at {
        return (EndClass::Panic, at.map(|at| (at, false)));
    }
    if let Some((signal, at)) = evidence.signal {
        return (EndClass::Signal(signal), Some((at, false)));
    }
    let lower_bound = facts.last_at.map(|at| (at, true));
    if evidence.empty_marker {
        return (EndClass::Killed, lower_bound);
    }
    if let Some(at) = facts.quit_at {
        return (EndClass::Clean, Some((at, false)));
    }
    // A run always logs at its start (the watchdog arms), so a run with no marker
    // AND no line of its own left no evidence at all — its lines rotated away, or
    // the log directory was cleared — and "clean" would be a guess.
    if facts.lines == 0 {
        return (EndClass::Unknown, None);
    }
    (EndClass::Clean, lower_bound)
}

/// What a run left in the log directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Evidence {
    /// A panic report of the run: `Some(Some(t))` when its first line dates it.
    panic_at: Option<Option<u64>>,
    /// A non-empty marker of the run: the signal the handler wrote, and its mtime.
    signal: Option<(u8, u64)>,
    /// An empty marker of the run whose owner is gone.
    empty_marker: bool,
}

/// Seconds of a file's mtime.
fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// `name` with a consumed `.seen` suffix removed.
fn unconsumed(name: &str) -> &str {
    name.strip_suffix(".seen").unwrap_or(name)
}

/// The run's pid from a panic report name, `crash-<pid>.log`.
fn panic_report_pid(name: &str) -> Option<u32> {
    unconsumed(name)
        .strip_prefix("crash-")?
        .strip_suffix(".log")?
        .parse()
        .ok()
}

/// A marker's pid and birth second: `crash-marker-<pid>-<nanos>-<owner>.log`, or the
/// legacy `crash-signal-<pid>-<nanos>.log`.
fn marker_pid_birth(name: &str) -> Option<(u32, u64)> {
    let body = unconsumed(name).strip_suffix(".log")?;
    let rest = body
        .strip_prefix(crate::crash_signal::markers::PREFIX)
        .or_else(|| body.strip_prefix("crash-signal-"))?;
    let mut parts = rest.splitn(3, '-');
    let pid = parts.next()?.parse().ok()?;
    let nanos: u128 = parts.next()?.parse().ok()?;
    Some((pid, u64::try_from(nanos / 1_000_000_000).ok()?))
}

/// The first line's `crashed at unix <secs>.<ms>` of a panic report.
fn panic_report_time(path: &Path) -> Option<u64> {
    use std::io::{BufRead as _, BufReader, Read as _};
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(file.take(512)).read_line(&mut first).ok()?;
    let at = first.split("crashed at unix ").nth(1)?;
    at.split(['.', ' ', '\n']).next()?.parse().ok()
}

/// The signal number a non-empty marker's banner names (`aterm: fatal signal N — …`).
fn marker_signal(path: &Path) -> Option<u8> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut head = String::new();
    file.take(256).read_to_string(&mut head).ok()?;
    let after = head.split("fatal signal ").nth(1)?;
    after
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// What `dir` holds for the run `pid` born at `birth` (artifacts older than the birth
/// belong to an earlier owner of the pid, and are ignored).
fn find_evidence(dir: &Path, pid: u32, birth: u64) -> Evidence {
    let mut evidence = Evidence::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return evidence;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // `DirEntry::metadata` does not follow a symlink on unix.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() {
            continue;
        }
        if panic_report_pid(name) == Some(pid) {
            if mtime_secs(&meta) + 1 >= birth {
                evidence.panic_at = Some(panic_report_time(&entry.path()));
            }
            continue;
        }
        let Some((marker_pid, marker_birth)) = marker_pid_birth(name) else {
            continue;
        };
        // The marker is armed within a second or so of the process's birth.
        if marker_pid != pid || marker_birth + 2 < birth {
            continue;
        }
        if meta.len() > 0 {
            let signal = marker_signal(&entry.path()).unwrap_or(0);
            evidence.signal = Some((signal, mtime_secs(&meta)));
        } else if unconsumed(name) != name
            || crate::crash_signal::markers::probe(&entry.path())
                == crate::crash_signal::markers::Owner::Dead
        {
            evidence.empty_marker = true;
        }
    }
    evidence
}

/// For a ledger's first row: the pid and birth of the newest crash artifact a
/// previous run left (the crash scans have just renamed it `.seen`). Never a marker
/// its name says another kind of launch armed ([`crate::crash_signal::MarkerOwner::Other`]):
/// a test, dev or headless start, or a candidate still under its parent's veto, is
/// not the run before this one.
fn newest_artifact_owner(dir: &Path) -> Option<(u32, u64)> {
    use crate::crash_signal::{MarkerOwner, markers};
    let mut newest: Option<(u64, u32, u64)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".seen") {
            continue;
        }
        if markers::parse(unconsumed(name)).is_some_and(|m| m.owner == MarkerOwner::Other) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = mtime_secs(&meta);
        let owner = marker_pid_birth(name).or_else(|| panic_report_pid(name).map(|p| (p, 0)));
        if let Some((pid, birth)) = owner
            && newest.is_none_or(|(t, _, _)| mtime >= t)
        {
            newest = Some((mtime, pid, birth));
        }
    }
    newest.map(|(_, pid, birth)| (pid, birth))
}

/// What a run's own lines in `aterm.log` say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LogFacts {
    /// Lines of the run found.
    lines: usize,
    /// The time of its last line.
    last_at: Option<u64>,
    /// The time of its final-exit line.
    quit_at: Option<u64>,
    /// Whether a reported stall was still open at its last line.
    stall_open: bool,
    /// The last pressure level its lines report.
    pressure: Option<Pressure>,
}

impl LogFacts {
    fn stall(self) -> Tri {
        match (self.lines, self.stall_open) {
            (0, _) => Tri::Unknown,
            (_, true) => Tri::Yes,
            (_, false) => Tri::No,
        }
    }

    fn pressure(self) -> Pressure {
        match (self.lines, self.pressure) {
            (0, _) => Pressure::Unknown,
            (_, None) => Pressure::None,
            (_, Some(p)) => p,
        }
    }

    /// Fold one line of the run (`secs` its time, `body` everything after the pid).
    fn fold(&mut self, secs: u64, body: &str) {
        self.lines += 1;
        self.last_at = Some(secs);
        if body.contains(STALL_ENDED_LINE) {
            self.stall_open = false;
        } else if body.contains(STALL_LINE) {
            self.stall_open = true;
        }
        if let Some(level) = body.split(PRESSURE_LINE).nth(1) {
            let level = level.trim_start();
            self.pressure = if level.starts_with("critical") {
                Some(Pressure::Critical)
            } else if level.starts_with("warn") {
                Some(Pressure::Warn)
            } else if level.starts_with("normal") {
                Some(Pressure::Normal)
            } else {
                self.pressure
            };
        } else if body.contains(SHED_LINE_CRITICAL) {
            self.pressure = Some(Pressure::Critical);
        } else if body.contains(SHED_LINE_WARN) {
            // A shed line only restates a level: it never lowers a critical report
            // the pressure source made just before it.
            if self.pressure != Some(Pressure::Critical) {
                self.pressure = Some(Pressure::Warn);
            }
        }
        if body.contains(QUIT_LINE) {
            self.quit_at = Some(secs);
        }
    }
}

/// Whether the census reads a stall as open at the end of a run whose `aterm.log`
/// lines carry `bodies`, in order — the fold [`scan_logs`] applies to each line.
/// The watchdog's tests pass it the lines its sampler thread logs.
#[cfg(test)]
pub(crate) fn stall_at_end<S: AsRef<str>>(bodies: &[S]) -> Tri {
    let mut facts = LogFacts::default();
    for (secs, body) in (0u64..).zip(bodies) {
        facts.fold(secs, body.as_ref());
    }
    facts.stall()
}

/// The run `pid` born at `birth`, as its lines in `aterm.log.1` then `aterm.log` tell it.
fn scan_logs(dir: &Path, pid: u32, birth: u64) -> LogFacts {
    use std::io::{BufRead as _, BufReader, Read as _};
    let mut facts = LogFacts::default();
    let mut budget = MAX_LOG_BYTES;
    let current = dir.join("aterm.log");
    let older: PathBuf = crate::logging::rotated_path(&current);
    let pid_token = pid.to_string();
    for path in [older, current] {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.file_type().is_file() || budget == 0 {
            continue;
        }
        let Ok(file) = std::fs::File::open(&path) else {
            continue;
        };
        let take = meta.len().min(budget);
        budget -= take;
        let mut reader = BufReader::new(file.take(take));
        let mut raw = Vec::new();
        loop {
            raw.clear();
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&raw);
            // `<secs>.<ms> <pid> <LEVEL> <target>: <body>` (`logging::format_record`).
            let mut parts = line.splitn(3, ' ');
            let (Some(stamp), Some(line_pid), Some(rest)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            if line_pid != pid_token {
                continue;
            }
            let Some(secs) = stamp.split('.').next().and_then(|s| s.parse::<u64>().ok()) else {
                continue;
            };
            // An earlier owner of the same pid wrote lines before this run was born.
            if secs + 1 < birth {
                continue;
            }
            facts.fold(secs, rest);
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_update::recovery_ledger::{Row, append_row};

    /// Answers for the census, with no live process and no boot trial by default.
    struct Fake {
        alive: Option<(u32, u64)>,
        trial: u32,
    }

    impl Probe for Fake {
        fn birth(&self, pid: u32) -> Option<u64> {
            self.alive.filter(|(p, _)| *p == pid).map(|(_, b)| b)
        }
        fn trial(&self, _build: u64) -> u32 {
            self.trial
        }
    }

    const NOBODY: Fake = Fake {
        alive: None,
        trial: 0,
    };

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aterm-recovery-census-{tag}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
        fn file(&self, name: &str, body: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, body).expect("write fixture");
            path
        }
        fn log(&self, lines: &[(u64, u32, &str)]) {
            let text: String = lines
                .iter()
                .map(|(secs, pid, body)| format!("{secs}.123 {pid} INFO aterm_gui: {body}\n"))
                .collect();
            self.file("aterm.log", &text);
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const BORN: u64 = 1_790_000_000;

    fn me(pid: u32) -> Me {
        Me {
            pid,
            started: BORN + 5000,
            build: 1_790_400_000,
            version: "0.94.0".into(),
        }
    }

    /// The ledger holds one earlier cold launch: pid 4242, born at BORN.
    fn with_previous(s: &Scratch) {
        let previous = Row {
            at: BORN,
            pid: 4242,
            started: BORN,
            build: 1_790_305_290,
            version: "0.93.0".into(),
            launch: LaunchKind::Cold,
            prev: PrevRun::none(),
        };
        append_row(&ledger_path(&s.0), &previous).expect("seed the ledger");
    }

    fn marker(pid: u32, birth: u64) -> String {
        format!(
            "crash-marker-{pid}-{}-app.log",
            u128::from(birth) * 1_000_000_000 + 7
        )
    }

    #[test]
    fn a_clean_quit_is_dated_by_its_final_exit_line() {
        let s = Scratch::new("clean");
        with_previous(&s);
        s.log(&[
            (BORN + 1, 4242, "watchdog armed"),
            (BORN + 3600, 4242, QUIT_LINE),
            (BORN + 3601, 9999, "another process"),
        ]);
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::Clean);
        assert_eq!(row.prev.pid, Some(4242));
        assert_eq!(row.prev.build, Some(1_790_305_290));
        assert_eq!(
            row.prev.uptime,
            Some(Uptime {
                secs: 3600,
                at_least: false
            })
        );
        assert_eq!(row.prev.stall, Tri::No);
        assert_eq!(row.prev.pressure, Pressure::None);
    }

    /// No marker left and no quit line (an exit path of an older build, an update
    /// parent's `_exit`): clean, bounded below by the run's last line.
    #[test]
    fn a_run_with_its_own_lines_and_no_marker_ended_clean_bounded_by_its_last_line() {
        let s = Scratch::new("clean-bound");
        with_previous(&s);
        s.log(&[
            (BORN + 1, 4242, "watchdog armed"),
            (BORN + 70, 4242, "idle"),
        ]);
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::Clean);
        assert_eq!(
            row.prev.uptime,
            Some(Uptime {
                secs: 70,
                at_least: true
            })
        );
    }

    #[test]
    fn an_empty_consumed_marker_is_a_kill_bounded_by_the_last_line() {
        let s = Scratch::new("killed");
        with_previous(&s);
        s.file(&format!("{}.seen", marker(4242, BORN)), "");
        s.log(&[
            (BORN + 10, 4242, "up"),
            (BORN + 90, 4242, &format!("{PRESSURE_LINE} critical")),
            (
                BORN + 100,
                4242,
                "MAIN-THREAD STALL: no heartbeat for 5.1s while inside `UserEvent`",
            ),
        ]);
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::Killed);
        assert_eq!(
            row.prev.uptime,
            Some(Uptime {
                secs: 100,
                at_least: true
            })
        );
        assert_eq!(row.prev.stall, Tri::Yes);
        assert_eq!(row.prev.pressure, Pressure::Critical);
    }

    #[test]
    fn a_stall_that_ended_and_pressure_that_eased_are_not_at_the_end() {
        let s = Scratch::new("ended");
        with_previous(&s);
        s.file(&format!("{}.seen", marker(4242, BORN)), "");
        s.log(&[
            (BORN + 10, 4242, "MAIN-THREAD STALL: no heartbeat for 5.1s"),
            (
                BORN + 11,
                4242,
                &format!("{STALL_ENDED_LINE}: the main thread beat again"),
            ),
            (BORN + 12, 4242, &format!("{PRESSURE_LINE} critical")),
            (BORN + 13, 4242, &format!("{PRESSURE_LINE} normal")),
        ]);
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::Killed);
        assert_eq!(row.prev.stall, Tri::No);
        assert_eq!(row.prev.pressure, Pressure::Normal);
    }

    #[test]
    fn a_written_marker_is_the_signal_it_names_at_its_mtime() {
        let s = Scratch::new("signal");
        with_previous(&s);
        s.file(
            &format!("{}.seen", marker(4242, BORN)),
            "aterm: fatal signal 11 \u{2014} crash marker written\n",
        );
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::Signal(11));
        // The fixture was written now, so its mtime is now: the uptime is exact.
        assert_eq!(row.prev.uptime.map(|u| u.at_least), Some(false));
        assert_eq!(row.prev.stall, Tri::Unknown, "no log lines: not known");
    }

    #[test]
    fn a_panic_report_wins_and_is_dated_by_its_first_line() {
        let s = Scratch::new("panic");
        with_previous(&s);
        s.file(
            "crash-4242.log.seen",
            &format!(
                "aterm-gui 0.93.0 crashed at unix {}.250\npanicked at x\n",
                BORN + 42
            ),
        );
        s.file(
            &format!("{}.seen", marker(4242, BORN)),
            "aterm: fatal signal 6 \u{2014} crash marker written\n",
        );
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::Panic);
        assert_eq!(
            row.prev.uptime,
            Some(Uptime {
                secs: 42,
                at_least: false
            })
        );
    }

    #[test]
    fn another_runs_artifacts_and_lines_under_a_recycled_pid_are_not_this_runs() {
        let s = Scratch::new("recycled");
        with_previous(&s);
        // An EARLIER owner of pid 4242: its marker is named with an older birth, and
        // its lines predate this run's birth.
        s.file(&format!("{}.seen", marker(4242, BORN - 86_400)), "");
        s.log(&[(BORN - 86_000, 4242, "MAIN-THREAD STALL: an old one")]);
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(
            row.prev.class,
            EndClass::Unknown,
            "none of that is this run's, so there is no evidence of it at all"
        );
        assert_eq!(
            row.prev.stall,
            Tri::Unknown,
            "none of those lines are this run's"
        );
    }

    #[test]
    fn a_previous_run_still_alive_is_running_and_its_dead_pid_twin_is_not() {
        let s = Scratch::new("running");
        with_previous(&s);
        let alive = Fake {
            alive: Some((4242, BORN)),
            trial: 0,
        };
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &alive);
        assert_eq!(row.prev.class, EndClass::Running);
        let reused = Fake {
            alive: Some((4242, BORN + 4000)),
            trial: 0,
        };
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &reused);
        assert_ne!(
            row.prev.class,
            EndClass::Running,
            "a recycled pid is not the run"
        );
    }

    #[test]
    fn a_successor_records_the_handoff_and_the_boot_trial() {
        let s = Scratch::new("successor");
        with_previous(&s);
        let trial = Fake {
            alive: Some((4242, BORN)),
            trial: 1,
        };
        let row = census_row(&s.0, &me(5000), LaunchKind::Successor, BORN + 700, &trial);
        assert_eq!(row.launch, LaunchKind::Successor);
        assert_eq!(row.prev.class, EndClass::Handoff);
        assert_eq!(row.prev.pid, Some(4242));
        assert_eq!(row.prev.trial, 1);
        assert_eq!(row.prev.uptime.map(|u| u.secs), Some(700));
    }

    /// The first row's artifact is never one whose marker names ANOTHER kind of
    /// launch (`MarkerOwner::Other`: a test, dev or headless start, or a candidate
    /// its parent could still reject). Those share the log directory, and one's crash
    /// is not the run before an installed launch: read as it, it would be the
    /// ledger's "last unexpected end" in `aterm doctor` until a real one replaced it.
    #[test]
    fn the_first_row_never_takes_a_marker_owned_by_another_kind_of_launch() {
        let s = Scratch::new("first-other");
        s.file(
            &format!(
                "crash-marker-888-{}-other.log.seen",
                u128::from(BORN) * 1_000_000_000 + 7
            ),
            "aterm: fatal signal 11 \u{2014} crash marker written\n",
        );
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(
            row.prev.class,
            EndClass::None,
            "a dev or test launch's crash is not the run before"
        );
        assert_eq!(row.prev.pid, None);
        // The daily driver's own marker beside it is still read.
        s.file(&format!("{}.seen", marker(777, BORN)), "");
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.pid, Some(777));
        assert_eq!(row.prev.class, EndClass::Killed);
    }

    #[test]
    fn the_first_row_classifies_the_newest_artifact_or_says_none() {
        let s = Scratch::new("first");
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::None);
        s.file(&format!("{}.seen", marker(777, BORN)), "");
        let row = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(row.prev.class, EndClass::Killed);
        assert_eq!(row.prev.pid, Some(777));
        assert_eq!(row.prev.build, None);
    }

    /// THE COLD-LANE UPDATE EXEC. An update applied with no session open replaces
    /// the process image IN PLACE (`app_update_handoff`'s `ApplyLane::Cold`,
    /// `command.exec()`): the new image keeps the pid and the kernel birth, boots as
    /// a cold windowed start and runs the census again. Its previous run is the
    /// image it replaced, handed off to an update — never the run the old image's
    /// own row already classified: counting that one twice would double every end
    /// the ledger exists to count.
    #[test]
    fn a_cold_lane_update_exec_is_a_handoff_from_the_image_it_replaced() {
        let s = Scratch::new("self");
        with_previous(&s);
        // The old image's row, written at its own cold start: it classified 4242.
        let old_image = Me {
            build: 1_790_300_000,
            version: "0.93.0".into(),
            ..me(5000)
        };
        let first = census_row(&s.0, &old_image, LaunchKind::Cold, BORN + 5000, &NOBODY);
        assert_eq!(first.prev.pid, Some(4242));
        append_row(&ledger_path(&s.0), &first).expect("append");
        s.log(&[
            (BORN + 5001, 5000, "watchdog armed"),
            (BORN + 9000, 5000, "update apply: cold lane"),
        ]);
        // Same pid, same birth, the new build: the image the exec put in place. The
        // process is alive — it is this one — and that is no "running" peer.
        let alive = Fake {
            alive: Some((5000, BORN + 5000)),
            trial: 0,
        };
        let again = census_row(&s.0, &me(5000), LaunchKind::Cold, BORN + 9002, &alive);
        assert_eq!(again.prev.pid, Some(5000), "the image it replaced");
        assert_eq!(again.prev.class, EndClass::Handoff);
        assert_eq!(again.prev.build, Some(1_790_300_000));
        assert_eq!(again.prev.version.as_deref(), Some("0.93.0"));
        assert_eq!(again.prev.uptime.map(|u| u.secs), Some(4002));
        assert_eq!(again.prev.stall, Tri::No);
        // And the ledger then counts one end of 4242, not two.
        append_row(&ledger_path(&s.0), &again).expect("append");
        let rows = read_rows(&ledger_path(&s.0));
        assert_eq!(
            rows.iter().filter(|r| r.prev.pid == Some(4242)).count(),
            1,
            "the run before the process is classified once"
        );
    }

    #[test]
    fn the_names_the_scans_leave_are_parsed_both_ways() {
        assert_eq!(panic_report_pid("crash-12.log"), Some(12));
        assert_eq!(panic_report_pid("crash-12.log.seen"), Some(12));
        assert_eq!(panic_report_pid("crash-marker-12-3-app.log"), None);
        assert_eq!(
            marker_pid_birth("crash-marker-12-1790000000123456789-app.log.seen"),
            Some((12, 1_790_000_000))
        );
        assert_eq!(
            marker_pid_birth("crash-signal-12-1790000000123456789.log"),
            Some((12, 1_790_000_000))
        );
        assert_eq!(marker_pid_birth("recovery-ledger.log"), None);
    }
}
