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
            if let Some(node) = node {
                node.note_prev_end(row.prev.class);
            }
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
    unconsumed(name)
        .strip_prefix("crash-")?
        .strip_suffix(".log")?
        .parse()
        .ok()
}

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

fn panic_report_time(path: &Path) -> Option<u64> {
    use std::io::{BufRead as _, BufReader, Read as _};
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    BufReader::new(file.take(512)).read_line(&mut first).ok()?;
    let at = first.split("crashed at unix ").nth(1)?;
    at.split(['.', ' ', '\n']).next()?.parse().ok()
}

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

fn find_evidence(dir: &Path, pid: u32, birth: u64) -> Evidence {
    let mut evidence = Evidence::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return evidence;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
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
mod tests {
    use super::*;
    use aterm_update::recovery_ledger::{Row, append_row};

    struct Fake {
        alive: Option<(u32, u64)>,
        trial: u32,
    }

    impl Probe for Fake {
        fn birth(&self, pid: u32) -> Option<Birth> {
            self.alive
                .filter(|(p, _)| *p == pid)
                .map(|(_, seconds)| Birth {
                    seconds,
                    micros: None,
                })
        }
        fn trial(&self, _build: u64) -> u32 {
            self.trial
        }
    }

    const NOBODY: Fake = Fake {
        alive: None,
        trial: 0,
    };

    struct Scratch(aterm_tempfile::TempDir);

    impl Scratch {
        fn new(tag: &str) -> Self {
            Self(
                aterm_tempfile::Builder::new()
                    .prefix(&format!("aterm-recovery-census-{tag}-"))
                    .tempdir()
                    .expect("scratch dir"),
            )
        }
        fn file(&self, name: &str, body: &str) -> PathBuf {
            let path = self.0.path().join(name);
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

    const BORN: u64 = 1_790_000_000;

    fn me(pid: u32) -> Me {
        Me {
            pid,
            started: BORN + 5000,
            started_us: None,
            build: 1_790_400_000,
            version: "0.94.0".into(),
        }
    }

    fn with_previous(s: &Scratch) {
        let previous = Row {
            at: BORN,
            pid: 4242,
            started: BORN,
            started_us: None,
            build: 1_790_305_290,
            version: "0.93.0".into(),
            launch: LaunchKind::Cold,
            prev: PrevRun::none(),
            keeper: None,
            restores: None,
        };
        append_row(&ledger_path(s.0.path()), &previous).expect("seed the ledger");
    }

    fn marker(pid: u32, birth: u64) -> String {
        format!(
            "crash-marker-{pid}-{}-app.log",
            u128::from(birth) * 1_000_000_000 + 7
        )
    }

    #[test]
    fn the_census_thread_amends_the_node_logs_start() {
        let s = Scratch::new("node-log");
        let clock =
            crate::session_log::Clock::reading(|| aterm_slog::Stamp::new(1_760_000_000_000, 5));
        let node: &'static crate::session_log_node::NodeLogHost = Box::leak(Box::new(
            crate::session_log_node::NodeLogHost::started(7, clock),
        ));
        let dir = s.0.path().to_path_buf();
        record_into(
            Some(node),
            move || Some(dir),
            LaunchKind::Cold,
            None,
            KeeperAtLaunch::On,
        );
        let amended = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the census's prev_end in the node log",
            || {
                node.lines()
                    .into_iter()
                    .find(|l| l.contains(" amend n start "))
            },
        );
        assert!(
            amended.starts_with("2 amend n start prev_end=none by=aterm src=aterm conf=exact "),
            "{amended}"
        );
        let rows = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the census's row",
            || Some(read_rows(&ledger_path(s.0.path()))).filter(|rows| rows.len() == 1),
        );
        assert_eq!(rows[0].keeper, Some(KeeperAtLaunch::On));
    }

    fn swept_then_counted(
        owner: crate::crash_signal::MarkerOwner,
        scan: crate::crash_signal::MarkerOwner,
    ) -> (bool, Row) {
        use crate::crash_signal::{MarkerOwner, markers};
        let s = Scratch::new("lab-desk");
        with_previous(&s);
        s.log(&[
            (BORN + 1, 4242, "watchdog armed"),
            (BORN + 70, 4242, "idle"),
        ]);
        let marker = s.file(
            &markers::file_name(4242, u128::from(BORN) * 1_000_000_000 + 7, owner),
            "",
        );
        let swept = markers::sweep(s.0.path(), 5000, scan);
        let kept = marker.exists();
        assert_eq!(swept.is_empty(), kept, "the sweep answers what it removed");
        assert!(
            markers::dead_app_markers(s.0.path(), 5000).is_empty(),
            "no windowed launch's kill scan ever reports it"
        );
        if scan == MarkerOwner::Lab {
            let consumed =
                crate::logging::consume_dead_markers(s.0.path(), 5000, None, MarkerOwner::Lab);
            assert_eq!(consumed.is_some(), kept, "the lab desk's kill scan");
        }
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        (kept, row)
    }

    #[test]
    fn a_lab_desks_census_reads_its_killed_run_and_no_other_start_fools_it() {
        use crate::crash_signal::MarkerOwner::{App, Lab, Other};
        for (owner, scan, kept, class) in [
            (Lab, Lab, true, EndClass::Killed),
            (Other, Lab, false, EndClass::Clean),
            (Lab, App, false, EndClass::Clean),
            (Lab, Other, false, EndClass::Clean),
        ] {
            let (left, row) = swept_then_counted(owner, scan);
            assert_eq!(left, kept, "{owner:?} under a {scan:?} sweep");
            assert_eq!(
                row.prev.class, class,
                "{owner:?} under a {scan:?} sweep: {row:?}"
            );
            assert_eq!(row.prev.pid, Some(4242));
        }
    }

    #[test]
    fn the_census_gate_is_on_for_a_lab_desk_and_off_for_a_plain_headless_one() {
        use crate::crash_signal::{Arming, MarkerOwner, markers};
        let lab = Arming::for_launch(false, true, false, true);
        let plain = Arming::for_launch(false, true, false, false);
        assert_eq!(cold_census(lab, None), ColdCensus::Own, "a lab desk");
        assert_eq!(cold_census(plain, None), ColdCensus::Off, "plain headless");
        assert_eq!((lab, plain), (Arming::Lab, Arming::Other));
        assert_eq!(cold_census(Arming::App, None), ColdCensus::Own);
        assert_eq!(cold_census(Arming::AppAfterCommit, None), ColdCensus::Off);
        assert_eq!(
            cold_census(plain, Some(std::ffi::OsStr::new("/private/tmp/x"))),
            ColdCensus::Seam(PathBuf::from("/private/tmp/x"))
        );
        assert_eq!(
            cold_census(plain, Some(std::ffi::OsStr::new(""))),
            ColdCensus::Off
        );

        let s = Scratch::new("gate");
        with_previous(&s);
        s.log(&[(BORN + 1, 4242, "watchdog armed")]);
        s.file(
            &markers::file_name(4242, u128::from(BORN) * 1_000_000_000 + 7, MarkerOwner::Lab),
            "",
        );
        let started = |ep| -> &'static crate::session_log_node::NodeLogHost {
            let clock =
                crate::session_log::Clock::reading(|| aterm_slog::Stamp::new(1_760_000_000_000, 5));
            Box::leak(Box::new(crate::session_log_node::NodeLogHost::started(
                ep, clock,
            )))
        };
        let plain_node = started(8);
        let dir = s.0.path().to_path_buf();
        record_cold_into(
            plain_node,
            move || Some(dir),
            &cold_census(plain, None),
            None,
            KeeperAtLaunch::Off,
        );
        let lines = plain_node.lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("1 open n start prev_end=- "),
            "{lines:?}"
        );
        assert_eq!(read_rows(&ledger_path(s.0.path())).len(), 1);

        assert!(
            crate::logging::consume_dead_markers(s.0.path(), 5000, None, MarkerOwner::Lab)
                .is_some()
        );
        let lab_node = started(9);
        let dir = s.0.path().to_path_buf();
        record_cold_into(
            lab_node,
            move || Some(dir),
            &cold_census(lab, None),
            None,
            KeeperAtLaunch::Off,
        );
        let amended = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the lab census's prev_end in the node log",
            || {
                lab_node
                    .lines()
                    .into_iter()
                    .find(|l| l.contains(" amend n start "))
            },
        );
        assert!(
            amended.starts_with("2 amend n start prev_end=killed by=aterm "),
            "{amended}"
        );
        let rows = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the lab census's row",
            || Some(read_rows(&ledger_path(s.0.path()))).filter(|rows| rows.len() == 2),
        );
        assert_eq!(rows[1].prev.class, EndClass::Killed);
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
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
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

    #[test]
    fn a_run_with_its_own_lines_and_no_marker_ended_clean_bounded_by_its_last_line() {
        let s = Scratch::new("clean-bound");
        with_previous(&s);
        s.log(&[
            (BORN + 1, 4242, "watchdog armed"),
            (BORN + 70, 4242, "idle"),
        ]);
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
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
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
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
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
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
            "aterm: fatal signal 11\n",
        );
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        assert_eq!(row.prev.class, EndClass::Signal(11));
        assert_eq!(row.prev.uptime.map(|u| u.at_least), Some(false));
        assert_eq!(row.prev.stall, Tri::Unknown, "no log lines: not known");
    }

    #[test]
    fn a_marker_in_the_older_banner_form_still_names_its_signal() {
        let s = Scratch::new("signal-old");
        with_previous(&s);
        s.file(
            &format!("{}.seen", marker(4242, BORN)),
            "aterm: fatal signal 6 \u{2014} crash marker written\n",
        );
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        assert_eq!(row.prev.class, EndClass::Signal(6));
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
            "aterm: fatal signal 6\n",
        );
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
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
        s.file(&format!("{}.seen", marker(4242, BORN - 86_400)), "");
        s.log(&[(BORN - 86_000, 4242, "MAIN-THREAD STALL: an old one")]);
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
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
        let row = census_row(s.0.path(), &me(5000), LaunchKind::Cold, BORN + 5000, &alive);
        assert_eq!(row.prev.class, EndClass::Running);
        let reused = Fake {
            alive: Some((4242, BORN + 4000)),
            trial: 0,
        };
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &reused,
        );
        assert_ne!(
            row.prev.class,
            EndClass::Running,
            "a recycled pid is not the run"
        );
    }

    #[test]
    fn a_pid_reused_within_the_same_second_is_not_the_previous_run() {
        struct Exact(u32, Birth);
        impl Probe for Exact {
            fn birth(&self, pid: u32) -> Option<Birth> {
                (pid == self.0).then_some(self.1)
            }
            fn trial(&self, _build: u64) -> u32 {
                0
            }
        }
        let s = Scratch::new("reused-second");
        let first = Me {
            pid: 4242,
            started: BORN,
            started_us: Some(250_000),
            ..me(4242)
        };
        let row = census_row(s.0.path(), &first, LaunchKind::Cold, BORN + 1, &NOBODY);
        assert_eq!(row.started_us, Some(250_000), "the row carries the birth");
        append_row(&ledger_path(s.0.path()), &row).expect("append");
        let reused = Exact(
            4242,
            Birth {
                seconds: BORN,
                micros: Some(750_000),
            },
        );
        let next = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &reused,
        );
        assert_ne!(
            next.prev.class,
            EndClass::Running,
            "a pid reused within the second is not the run"
        );
        let same = Exact(
            4242,
            Birth {
                seconds: BORN,
                micros: Some(250_000),
            },
        );
        let next = census_row(s.0.path(), &me(5000), LaunchKind::Cold, BORN + 5000, &same);
        assert_eq!(next.prev.class, EndClass::Running);
    }

    #[test]
    fn a_successor_records_the_handoff_and_the_boot_trial() {
        let s = Scratch::new("successor");
        with_previous(&s);
        let trial = Fake {
            alive: Some((4242, BORN)),
            trial: 1,
        };
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Successor,
            BORN + 700,
            &trial,
        );
        assert_eq!(row.launch, LaunchKind::Successor);
        assert_eq!(row.prev.class, EndClass::Handoff);
        assert_eq!(row.prev.pid, Some(4242));
        assert_eq!(row.prev.trial, 1);
        assert_eq!(row.prev.uptime.map(|u| u.secs), Some(700));
    }

    #[test]
    fn the_first_row_never_takes_a_marker_owned_by_another_kind_of_launch() {
        let s = Scratch::new("first-other");
        s.file(
            &format!(
                "crash-marker-888-{}-other.log.seen",
                u128::from(BORN) * 1_000_000_000 + 7
            ),
            "aterm: fatal signal 11\n",
        );
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        assert_eq!(
            row.prev.class,
            EndClass::None,
            "a dev or test launch's crash is not the run before"
        );
        assert_eq!(row.prev.pid, None);
        s.file(&format!("{}.seen", marker(777, BORN)), "");
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        assert_eq!(row.prev.pid, Some(777));
        assert_eq!(row.prev.class, EndClass::Killed);
    }

    #[test]
    fn the_first_row_classifies_the_newest_artifact_or_says_none() {
        let s = Scratch::new("first");
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        assert_eq!(row.prev.class, EndClass::None);
        s.file(&format!("{}.seen", marker(777, BORN)), "");
        let row = census_row(
            s.0.path(),
            &me(5000),
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        assert_eq!(row.prev.class, EndClass::Killed);
        assert_eq!(row.prev.pid, Some(777));
        assert_eq!(row.prev.build, None);
    }

    #[test]
    fn a_cold_lane_update_exec_is_a_handoff_from_the_image_it_replaced() {
        let s = Scratch::new("self");
        with_previous(&s);
        let old_image = Me {
            build: 1_790_300_000,
            version: "0.93.0".into(),
            ..me(5000)
        };
        let first = census_row(
            s.0.path(),
            &old_image,
            LaunchKind::Cold,
            BORN + 5000,
            &NOBODY,
        );
        assert_eq!(first.prev.pid, Some(4242));
        append_row(&ledger_path(s.0.path()), &first).expect("append");
        s.log(&[
            (BORN + 5001, 5000, "watchdog armed"),
            (BORN + 9000, 5000, "update apply: cold lane"),
        ]);
        let alive = Fake {
            alive: Some((5000, BORN + 5000)),
            trial: 0,
        };
        let again = census_row(s.0.path(), &me(5000), LaunchKind::Cold, BORN + 9002, &alive);
        assert_eq!(again.prev.pid, Some(5000), "the image it replaced");
        assert_eq!(again.prev.class, EndClass::Handoff);
        assert_eq!(again.prev.build, Some(1_790_300_000));
        assert_eq!(again.prev.version.as_deref(), Some("0.93.0"));
        assert_eq!(again.prev.uptime.map(|u| u.secs), Some(4002));
        assert_eq!(again.prev.stall, Tri::No);
        append_row(&ledger_path(s.0.path()), &again).expect("append");
        let rows = read_rows(&ledger_path(s.0.path()));
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
