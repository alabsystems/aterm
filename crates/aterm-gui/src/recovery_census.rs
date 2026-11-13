// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Launch recovery census.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use aterm_update::recovery_ledger::{
    EndClass, KeeperAtLaunch, LaunchKind, Pressure, PrevRun, Restores, Row, Tri, Uptime,
    append_row, ledger_path, read_rows,
};

/// The final exit's log line (lib.rs `main_entry`), which dates a clean quit.
pub(crate) const QUIT_LINE: &str = "final exit: quitting cleanly";

const STALL_LINE: &str = "MAIN-THREAD STALL";
/// The watchdog's line when a reported stall ends (`watchdog::stall_ended_message`).
pub(crate) const STALL_ENDED_LINE: &str = "MAIN-THREAD STALL ENDED";
/// The pressure source's own line (`memory_pressure::level_line`).
pub(crate) const PRESSURE_LINE: &str = "memory pressure: the system reports";
const SHED_LINE_CRITICAL: &str = "memory pressure (critical)";
const SHED_LINE_WARN: &str = "memory pressure (warn)";

const MAX_LOG_BYTES: u64 = 12 * 1024 * 1024;

/// This launch, as its row records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Me {
    pub(crate) pid: u32,
    pub(crate) started: u64,
    /// Microseconds within [`Self::started`] from the kernel's birth record.
    pub(crate) started_us: Option<u32>,
    pub(crate) build: u64,
    pub(crate) version: String,
}

impl Me {
    fn current() -> Self {
        let pid = std::process::id();
        let birth = process_birth(pid);
        Self {
            pid,
            started: birth.map_or_else(now_secs, |birth| birth.seconds),
            started_us: birth.and_then(|birth| birth.micros),
            build: crate::running_build_number(),
            version: crate::build_info::version().to_string(),
        }
    }
}

/// Kernel birth timestamp of a process (Unix seconds and microseconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Birth {
    pub(crate) seconds: u64,
    pub(crate) micros: Option<u32>,
}

/// What the census asks the system, so tests can answer instead.
pub(crate) trait Probe {
    fn birth(&self, pid: u32) -> Option<Birth>;
    fn trial(&self, build: u64) -> u32;
}

struct System;

impl Probe for System {
    fn birth(&self, pid: u32) -> Option<Birth> {
        process_birth(pid)
    }
    fn trial(&self, build: u64) -> u32 {
        aterm_update::trial_launch_count(build)
    }
}

fn process_birth(pid: u32) -> Option<Birth> {
    let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
    Some(Birth {
        seconds: birth.seconds,
        micros: u32::try_from(birth.microseconds)
            .ok()
            .filter(|us| *us < 1_000_000),
    })
}

fn now_secs() -> u64 {
    aterm_time::unix_secs().unwrap_or(0)
}

/// Spawn the background census thread to record this launch and previous-run classification.
pub(crate) fn record(launch: LaunchKind, restores: Option<Restores>, keeper: KeeperAtLaunch) {
    record_into(
        Some(crate::session_log_node::node_log()),
        crate::logging::log_dir,
        launch,
        restores,
        keeper,
    );
}

/// Environment variable naming a scratch folder for dev/headless census row output.
pub(crate) const LEDGER_SEAM: &str = "ATERM_DEBUG_RECOVERY_LEDGER";

/// Where a cold start's census row goes ([`cold_census`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ColdCensus {
    /// Write to the launch's own log directory and amend the node log (`start prev_end=`).
    Own,
    /// The [`LEDGER_SEAM`]'s folder alone, the node log not told.
    Seam(PathBuf),
    /// Write no census row.
    Off,
}

/// Determine the [`ColdCensus`] destination for a cold start armed as `arming`.
pub(crate) fn cold_census(
    arming: crate::crash_signal::Arming,
    seam: Option<&std::ffi::OsStr>,
) -> ColdCensus {
    use crate::crash_signal::Arming;
    match arming {
        Arming::App | Arming::Lab => ColdCensus::Own,
        Arming::Other | Arming::AppAfterCommit => match seam.filter(|dir| !dir.is_empty()) {
            Some(dir) => ColdCensus::Seam(PathBuf::from(dir)),
            None => ColdCensus::Off,
        },
    }
}

/// A cold start's row, as [`cold_census`] gates a launch armed as `arming`.
pub(crate) fn record_cold(
    arming: crate::crash_signal::Arming,
    restores: Option<Restores>,
    keeper: KeeperAtLaunch,
) {
    record_cold_into(
        crate::session_log_node::node_log(),
        crate::logging::log_dir,
        &cold_census(arming, aterm_types::dev_seam!(LEDGER_SEAM).as_deref()),
        restores,
        keeper,
    );
}

fn record_cold_into(
    node: &'static crate::session_log_node::NodeLogHost,
    own_dir: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    census: &ColdCensus,
    restores: Option<Restores>,
    keeper: KeeperAtLaunch,
) {
    match census {
        ColdCensus::Own => record_into(Some(node), own_dir, LaunchKind::Cold, restores, keeper),
        ColdCensus::Seam(dir) => {
            let dir = dir.clone();
            record_into(None, move || Some(dir), LaunchKind::Cold, restores, keeper);
        }
        ColdCensus::Off => {}
    }
}

fn record_into(
    node: Option<&'static crate::session_log_node::NodeLogHost>,
    dir: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    launch: LaunchKind,
    restores: Option<Restores>,
    keeper: KeeperAtLaunch,
) {
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-recovery-census",
        crate::qos::Role::Background,
        move || {
            let Some(dir) = dir() else {
                return;
            };
            let mut row = census_row(&dir, &Me::current(), launch, now_secs(), &System);
            row.keeper = Some(keeper);
            row.restores = restores;
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
            // After the row: the next launch judges this one from the row, so a node log that
            // says the run before ended a given way means the ledger already holds this run.
            if let Some(node) = node {
                node.note_prev_end(row.prev.class);
            }
        },
    );
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
    let is_me = |row: &Row| row.is_process(me.pid, me.started, me.started_us);
    let own_image = rows.last().filter(|row| is_me(row)).cloned();
    let last = rows.into_iter().rev().find(|row| !is_me(row));
    let prev = match (launch, own_image, last) {
        (LaunchKind::Cold, Some(own), _) => handoff_prev(dir, Some(&own), now, probe),
        (LaunchKind::Successor, _, last) => handoff_prev(dir, last.as_ref(), now, probe),
        (LaunchKind::Cold, None, Some(last)) => cold_prev(dir, &last, probe),
        (LaunchKind::Cold, None, None) => first_row_prev(dir),
    };
    Row {
        at: now,
        pid: me.pid,
        started: me.started,
        started_us: me.started_us,
        build: me.build,
        version: me.version.clone(),
        launch,
        prev,
        keeper: None,
        restores: None,
    }
}

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

fn cold_prev(dir: &Path, last: &Row, probe: &dyn Probe) -> PrevRun {
    let facts = scan_logs(dir, last.pid, last.started);
    let evidence = find_evidence(dir, last.pid, last.started);
    let running = probe
        .birth(last.pid)
        .is_some_and(|birth| last.is_process(last.pid, birth.seconds, birth.micros));
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

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn unconsumed(name: &str) -> &str {
    name.strip_suffix(".seen").unwrap_or(name)
}

fn panic_report_pid(name: &str) -> Option<u32> {
    let pid = unconsumed(name)
        .strip_prefix("crash-")?
        .strip_suffix(".log")?;
    pid.bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| pid.parse().ok())
        .flatten()
}

fn marker_pid_birth(name: &str) -> Option<(u32, u64)> {
    let body = unconsumed(name).strip_suffix(".log")?;
    let rest = body
        .strip_prefix(crate::crash_signal::markers::PREFIX)
        .or_else(|| body.strip_prefix("crash-signal-"))?;
    let mut parts = rest.splitn(3, '-');
    let pid = parts
        .next()
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
    let nanos: u128 = parts
        .next()
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?
        .parse()
        .ok()?;
    Some((pid, u64::try_from(nanos / 1_000_000_000).ok()?))
}

fn panic_report_time(path: &Path) -> Option<u64> {
    use std::io::{BufRead as _, BufReader, Read as _};
    let file = crate::logging::open_regular_nonblock(path).ok()?;
    let mut first = String::new();
    BufReader::new(file.take(512)).read_line(&mut first).ok()?;
    crate::logging::panic_report_timestamp(&first).map(|at| at.as_secs())
}

fn marker_signal(path: &Path) -> Option<u8> {
    use std::io::Read as _;
    let (file, meta) = crate::logging::open_regular_nonblock_with_meta(path).ok()?;
    let cap = usize::try_from(meta.len().min(256).saturating_add(1)).unwrap_or(0);
    let mut head = String::with_capacity(cap);
    file.take(256).read_to_string(&mut head).ok()?;
    let after = head.split("fatal signal ").nth(1)?;
    after
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

fn find_evidence(dir: &Path, pid: u32, birth: u64) -> Evidence {
    let mut evidence = Evidence::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return evidence;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if panic_report_pid(name) == Some(pid) {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.file_type().is_file() && mtime_secs(&meta) + 1 >= birth {
                evidence.panic_at = Some(panic_report_time(&entry.path()));
            }
            continue;
        }
        let Some((marker_pid, marker_birth)) = marker_pid_birth(name) else {
            continue;
        };
        if marker_pid != pid || marker_birth + 2 < birth {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.file_type().is_file() {
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
        let Some((pid, birth)) =
            marker_pid_birth(name).or_else(|| panic_report_pid(name).map(|p| (p, 0)))
        else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = mtime_secs(&meta);
        if newest.is_none_or(|(t, _, _)| mtime >= t) {
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
    const fn stall(self) -> Tri {
        match (self.lines, self.stall_open) {
            (0, _) => Tri::Unknown,
            (_, true) => Tri::Yes,
            (_, false) => Tri::No,
        }
    }

    const fn pressure(self) -> Pressure {
        match (self.lines, self.pressure) {
            (0, _) => Pressure::Unknown,
            (_, None) => Pressure::None,
            (_, Some(p)) => p,
        }
    }

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
            // A shed line never lowers a critical report the pressure source made before it.
            if self.pressure != Some(Pressure::Critical) {
                self.pressure = Some(Pressure::Warn);
            }
        }
        if body.contains(QUIT_LINE) {
            self.quit_at = Some(secs);
        }
    }
}

/// Fold `aterm.log` line `bodies` and return whether a stall remained open at the end.
#[cfg(test)]
pub(crate) fn stall_at_end<S: AsRef<str>>(bodies: &[S]) -> Tri {
    let mut facts = LogFacts::default();
    for (secs, body) in (0u64..).zip(bodies) {
        facts.fold(secs, body.as_ref());
    }
    facts.stall()
}

fn scan_logs(dir: &Path, pid: u32, birth: u64) -> LogFacts {
    use std::io::{BufRead as _, BufReader, Read as _};
    let mut facts = LogFacts::default();
    let mut budget = MAX_LOG_BYTES;
    let current = dir.join("aterm.log");
    let older: PathBuf = crate::logging::rotated_path(&current);
    let pid_token = pid.to_string();
    for path in [older, current] {
        if budget == 0 {
            continue;
        }
        let Ok((file, meta)) = aterm_uds::regular_file::open(&path) else {
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
            if secs + 1 < birth {
                continue;
            }
            facts.fold(secs, rest);
        }
    }
    facts
}

#[cfg(test)]
#[path = "recovery_census_tests.rs"]
mod tests;
