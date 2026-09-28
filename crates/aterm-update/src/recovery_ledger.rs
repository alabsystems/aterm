// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RECOVERY LEDGER — how each run of the installed app ended. The product half of
//! the PTY keeper's P0 census (`docs/DESIGN-pty-keeper-2026-09-26.md` §7).
//!
//! WHY IT EXISTS. The keeper design rests on a frequency it could not measure: how
//! often the daily driver dies without an exit path, and whether it was wedged or
//! starved of memory when it did (§2 F19: the kill census began only on 2026-09-25,
//! and nothing recorded a CLEAN end at all, so there was no denominator). Owner
//! decision 4's alternative — relaunch only when the census shows a stall or critical
//! memory pressure at death — needs exactly these rows.
//!
//! WHAT A ROW IS. One per windowed launch of the installed app, and one per update
//! successor at its Commit. A row records the launch itself (pid, birth, build,
//! version) and classifies the run before it: how it ended ([`EndClass`]), its build
//! and version, how long it ran ([`Uptime`]), whether the main-thread watchdog's last
//! word for it was a stall that never ended ([`Tri`]), the last memory-pressure level
//! the OS reported to it ([`Pressure`]), and how many launches the updater's boot trial
//! had counted against its build when the row was written.
//!
//! READ-ONLY IN EVERY SENSE BUT THE ONE FILE. Nothing about how aterm runs changes.
//! The evidence is what already exists when the next launch starts — the crash marker
//! and the panic report (`aterm-gui`'s `crash_signal` and `logging`), the watchdog's
//! and the pressure source's lines in `aterm.log`, the boot sentinel — and the only
//! thing written is this ledger. Nothing reads it to decide anything: `aterm doctor`
//! summarizes it ([`doctor_lines`]).
//!
//! THE FILE: [`LEDGER_FILE_NAME`] in the log directory
//! ([`aterm_types::dirs::logs_dir`], beside the crash markers it reads), `0600`, one
//! space-separated `key=value` line per row, newest last, trimmed to the newest
//! [`KEEP_ROWS`] once it passes [`TRIM_AT_BYTES`]. A line this build cannot parse is
//! skipped, never fatal: the ledger is observability, not state.

use std::path::{Path, PathBuf};

/// The ledger's file name in the log directory. Deliberately NOT `crash-*.log`: the
/// crash scan reads every non-empty file of that shape as a crash report.
pub const LEDGER_FILE_NAME: &str = "recovery-ledger.log";

/// How many rows a trim keeps (the newest).
pub const KEEP_ROWS: usize = 256;

/// A ledger larger than this is trimmed to [`KEEP_ROWS`] after the append that
/// crossed it. A row is about 250 bytes, so this is roughly twice [`KEEP_ROWS`].
pub const TRIM_AT_BYTES: u64 = 128 * 1024;

/// The most bytes [`read_rows`] reads — the TAIL of an oversized file. A file in the
/// log directory is not necessarily one aterm wrote (the urandom incident
/// `aterm-gui`'s `logging::CRASH_HEAD_BYTES` records), so no read is unbounded.
pub const MAX_READ_BYTES: u64 = 512 * 1024;

/// The format version every row this build writes carries (`v=1`).
pub const ROW_VERSION: u32 = 1;

/// The ledger's path in `logs_dir`.
#[must_use]
pub fn ledger_path(logs_dir: &Path) -> PathBuf {
    logs_dir.join(LEDGER_FILE_NAME)
}

/// The ledger's path in this user's log directory, or `None` when none resolves.
#[must_use]
pub fn default_ledger_path() -> Option<PathBuf> {
    aterm_types::dirs::logs_dir().map(|dir| ledger_path(&dir))
}

/// How a run ended, as the next launch could tell from what it left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndClass {
    /// It removed its crash marker on an exit path: a quit, an update's exit, a
    /// refused start. The marker is removed by every `exit(3)` and every clean `_exit`.
    Clean,
    /// It handed its sessions to an update successor: the successor's own row says so.
    Handoff,
    /// No signal handler ran and no exit path ran: SIGKILL (Force Quit, jetsam, a
    /// harness), a kernel kill, power loss. Its empty marker outlived it.
    Killed,
    /// A fatal signal the handler caught (SIGSEGV 11, SIGABRT 6, SIGBUS 10, SIGILL 4,
    /// SIGFPE 8); the number is what the handler wrote into the marker.
    Signal(u8),
    /// It filed a Rust panic report. (A panic on a background thread that the run
    /// survived files one too — the same reading the crash banner makes.)
    Panic,
    /// It was still alive when this launch started (a second instance).
    Running,
    /// Its row is there, but none of its evidence is.
    Unknown,
    /// There is no earlier run to classify: the ledger's first row.
    None,
}

impl EndClass {
    /// The row token (`clean`, `signal-11`, …).
    #[must_use]
    pub fn token(self) -> String {
        match self {
            Self::Clean => "clean".into(),
            Self::Handoff => "handoff".into(),
            Self::Killed => "killed".into(),
            Self::Signal(n) => format!("signal-{n}"),
            Self::Panic => "panic".into(),
            Self::Running => "running".into(),
            Self::Unknown => "unknown".into(),
            Self::None => "none".into(),
        }
    }

    /// The inverse of [`Self::token`].
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "clean" => Self::Clean,
            "handoff" => Self::Handoff,
            "killed" => Self::Killed,
            "panic" => Self::Panic,
            "running" => Self::Running,
            "unknown" => Self::Unknown,
            "none" => Self::None,
            other => Self::Signal(other.strip_prefix("signal-")?.parse().ok()?),
        })
    }

    /// Whether this end is one the person did not ask for — the class the keeper
    /// exists for. `Unknown` is not counted: it is missing evidence, not a death.
    #[must_use]
    pub fn is_unexpected(self) -> bool {
        matches!(self, Self::Killed | Self::Signal(_) | Self::Panic)
    }

    /// The words for a person.
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Clean => "clean".into(),
            Self::Handoff => "handed off to an update".into(),
            Self::Killed => "killed (no signal handler, no exit path)".into(),
            Self::Signal(n) => format!("fatal signal {n} ({})", signal_name(n)),
            Self::Panic => "panic".into(),
            Self::Running => "still running when the launch after it began".into(),
            Self::Unknown => "no evidence left".into(),
            Self::None => "nothing earlier recorded".into(),
        }
    }
}

/// The conventional name of the fatal signals the handler traps (Darwin numbering).
fn signal_name(n: u8) -> &'static str {
    match n {
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        _ => "signal",
    }
}

/// Yes, no, or not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tri {
    /// The evidence says yes.
    Yes,
    /// The evidence says no.
    No,
    /// There is no evidence either way (no log lines for the run).
    Unknown,
}

impl Tri {
    fn token(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown => "?",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "yes" => Self::Yes,
            "no" => Self::No,
            "?" => Self::Unknown,
            _ => return None,
        })
    }
}

/// The last memory-pressure level the OS reported to a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    /// The last report was critical.
    Critical,
    /// The last report was a warning.
    Warn,
    /// The last report was a return to normal.
    Normal,
    /// The run's lines carry no pressure report at all.
    None,
    /// No log lines for the run were found.
    Unknown,
}

impl Pressure {
    fn token(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Warn => "warn",
            Self::Normal => "normal",
            Self::None => "none",
            Self::Unknown => "?",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "critical" => Self::Critical,
            "warn" => Self::Warn,
            "normal" => Self::Normal,
            "none" => Self::None,
            "?" => Self::Unknown,
            _ => return None,
        })
    }
}

/// How long a run lasted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Uptime {
    /// Seconds from its birth to its end.
    pub secs: u64,
    /// `true` when the end is only a lower bound — the run's last log line, for a run
    /// whose death left no time of its own (a SIGKILL). Written `<secs>+`.
    pub at_least: bool,
}

/// What a row says about the run before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrevRun {
    /// Its pid, when a row or an artifact named one.
    pub pid: Option<u32>,
    /// Its build number, when its own row recorded one.
    pub build: Option<u64>,
    /// Its version, when its own row recorded one.
    pub version: Option<String>,
    /// How it ended.
    pub class: EndClass,
    /// How long it ran, when that can be told.
    pub uptime: Option<Uptime>,
    /// Whether the watchdog's last word for it was a stall that never ended.
    pub stall: Tri,
    /// The last memory-pressure level reported to it.
    pub pressure: Pressure,
    /// Launches the boot sentinel had counted against its build when this row was
    /// written (0: no boot trial pending for that build).
    pub trial: u32,
}

impl PrevRun {
    /// The "nothing earlier" value of a ledger's first row.
    #[must_use]
    pub fn none() -> Self {
        Self {
            pid: None,
            build: None,
            version: None,
            class: EndClass::None,
            uptime: None,
            stall: Tri::Unknown,
            pressure: Pressure::Unknown,
            trial: 0,
        }
    }
}

/// How the launch that wrote a row began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    /// A windowed start of the installed app.
    Cold,
    /// An update successor, written at its Commit.
    Successor,
}

impl LaunchKind {
    fn token(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Successor => "successor",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "cold" => Self::Cold,
            "successor" => Self::Successor,
            _ => return None,
        })
    }
}

/// One ledger row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// When the row was written (Unix seconds).
    pub at: u64,
    /// The launch's pid.
    pub pid: u32,
    /// The launch's birth (Unix seconds, the kernel's record where there is one).
    pub started: u64,
    /// The launch's build number.
    pub build: u64,
    /// The launch's version.
    pub version: String,
    /// How the launch began.
    pub launch: LaunchKind,
    /// The run before it.
    pub prev: PrevRun,
}

/// A value token with no space, `=`, or control character in it — the one shape the
/// line format can carry. Anything else is written `?`.
fn clean_token(value: &str) -> &str {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && c != '=')
    {
        value
    } else {
        "?"
    }
}

fn opt_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "?".into(), |v| v.to_string())
}

fn parse_opt_u64(token: &str) -> Option<Option<u64>> {
    if token == "?" {
        Some(None)
    } else {
        token.parse().ok().map(Some)
    }
}

impl Row {
    /// The row's line, without its newline.
    #[must_use]
    pub fn to_line(&self) -> String {
        let prev = &self.prev;
        let uptime = match prev.uptime {
            None => "?".to_string(),
            Some(Uptime { secs, at_least }) => {
                format!("{secs}{}", if at_least { "+" } else { "" })
            }
        };
        format!(
            "v={ROW_VERSION} at={} pid={} started={} build={} version={} launch={} \
             prev={} prev_pid={} prev_build={} prev_version={} prev_uptime={uptime} \
             prev_stall={} prev_pressure={} prev_trial={}",
            self.at,
            self.pid,
            self.started,
            self.build,
            clean_token(&self.version),
            self.launch.token(),
            prev.class.token(),
            prev.pid.map_or_else(|| "?".into(), |p| p.to_string()),
            opt_u64(prev.build),
            clean_token(prev.version.as_deref().unwrap_or("?")),
            prev.stall.token(),
            prev.pressure.token(),
            prev.trial,
        )
    }

    /// Parse one line; `None` for anything that is not a complete version-1 row.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let mut fields = std::collections::BTreeMap::new();
        for pair in line.split_whitespace() {
            let (key, value) = pair.split_once('=')?;
            fields.insert(key, value);
        }
        if fields.get("v")?.parse::<u32>().ok()? != ROW_VERSION {
            return None;
        }
        let get = |key: &str| fields.get(key).copied();
        let uptime = match get("prev_uptime")? {
            "?" => None,
            token => {
                let (digits, at_least) = match token.strip_suffix('+') {
                    Some(digits) => (digits, true),
                    None => (token, false),
                };
                Some(Uptime {
                    secs: digits.parse().ok()?,
                    at_least,
                })
            }
        };
        let text = |token: &str| (token != "?").then(|| token.to_string());
        Some(Self {
            at: get("at")?.parse().ok()?,
            pid: get("pid")?.parse().ok()?,
            started: get("started")?.parse().ok()?,
            build: get("build")?.parse().ok()?,
            version: get("version")?.to_string(),
            launch: LaunchKind::parse(get("launch")?)?,
            prev: PrevRun {
                pid: match get("prev_pid")? {
                    "?" => None,
                    token => Some(token.parse().ok()?),
                },
                build: parse_opt_u64(get("prev_build")?)?,
                version: text(get("prev_version")?),
                class: EndClass::parse(get("prev")?)?,
                uptime,
                stall: Tri::parse(get("prev_stall")?)?,
                pressure: Pressure::parse(get("prev_pressure")?)?,
                trial: get("prev_trial")?.parse().ok()?,
            },
        })
    }
}

/// Every row [`Row::parse`] accepts in `text`, oldest first.
#[must_use]
pub fn parse_rows(text: &str) -> Vec<Row> {
    text.lines().filter_map(Row::parse).collect()
}

/// The ledger's rows, oldest first; empty when there is no ledger or it cannot be
/// read. Reads at most [`MAX_READ_BYTES`] — the TAIL of a larger file, from its first
/// whole line — and only a regular file (never a FIFO, a device or a symlink's target).
#[must_use]
pub fn read_rows(path: &Path) -> Vec<Row> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Vec::new();
    };
    if !meta.file_type().is_file() {
        return Vec::new();
    }
    let Ok(mut file) = open_for_read(path) else {
        return Vec::new();
    };
    let skip = meta.len().saturating_sub(MAX_READ_BYTES);
    if skip > 0 && file.seek(SeekFrom::Start(skip)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.take(MAX_READ_BYTES).read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = if skip > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    parse_rows(text)
}

/// Open `path` for reading without following a final symlink or blocking on a FIFO.
#[cfg(unix)]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Append `row` to the ledger at `path` (created `0600`), then trim it to the newest
/// [`KEEP_ROWS`] when it has passed [`TRIM_AT_BYTES`].
///
/// Two launches can race here (an update successor and a manual start), so the append
/// and the trim run under an `flock` on the ledger itself, taken without waiting and
/// retried for about a second; an append that cannot get it still writes its one line
/// with a single `O_APPEND` write (whole on a local volume) and skips the trim. A
/// trim replaces the file by rename, so a writer that locked the old inode re-opens
/// the path before it writes.
///
/// # Errors
/// The open or the write failed.
#[cfg(unix)]
pub fn append_row(path: &Path, row: &Row) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let mut line = row.to_line();
    line.push('\n');
    for _ in 0..4 {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let locked = lock_briefly(&file);
        // A trim that renamed a new file over the path between our open and our lock
        // left us holding the retired inode: go round and open the path again.
        let same_inode = match (file.metadata(), std::fs::symlink_metadata(path)) {
            (Ok(ours), Ok(now)) => ours.ino() == now.ino() && ours.dev() == now.dev(),
            _ => false,
        };
        if locked && !same_inode {
            continue;
        }
        file.write_all(line.as_bytes())?;
        if locked && file.metadata().is_ok_and(|m| m.len() > TRIM_AT_BYTES) {
            trim_locked(path);
        }
        return Ok(());
    }
    Err(std::io::Error::other(
        "the ledger was replaced under every attempt",
    ))
}

/// `flock(LOCK_EX)` without waiting, retried every 20 ms for about a second.
#[cfg(unix)]
fn lock_briefly(file: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd as _;
    for _ in 0..50 {
        // SAFETY: `file` owns a live descriptor for the call; LOCK_NB never waits,
        // and the lock is released when the descriptor closes.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

/// Rewrite the ledger at `path` with its newest [`KEEP_ROWS`] rows, by a `0600`
/// temporary beside it renamed over it. The caller holds the ledger's lock.
#[cfg(unix)]
fn trim_locked(path: &Path) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let rows = read_rows(path);
    let keep = &rows[rows.len().saturating_sub(KEEP_ROWS)..];
    let mut text = String::new();
    for row in keep {
        text.push_str(&row.to_line());
        text.push('\n');
    }
    let tmp = path.with_file_name(format!(".{LEDGER_FILE_NAME}.trim"));
    let _ = std::fs::remove_file(&tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()));
    if written.is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `secs` as a short span for a person: `42 s`, `17 min`, `3 h 5 min`, `2 d 4 h`.
#[must_use]
pub fn span_words(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
        _ => format!("{} d {} h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// What the ledger says, for `aterm doctor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Rows read.
    pub rows: usize,
    /// When the oldest row read was written.
    pub since: Option<u64>,
    /// How many of the classified runs ended each way, in [`EndClass`] order.
    pub ends: Vec<(EndClass, usize)>,
    /// Of the unexpected ends, how many had an unended stall, and how many critical
    /// memory pressure, as their last word.
    pub unexpected_stalled: usize,
    /// See [`Self::unexpected_stalled`].
    pub unexpected_critical: usize,
    /// The newest row that recorded an unexpected end.
    pub last_unexpected: Option<Row>,
}

impl Summary {
    /// Whether any recorded run ended unexpectedly.
    #[must_use]
    pub fn any_unexpected(&self) -> bool {
        self.last_unexpected.is_some()
    }
}

/// Fold `rows` into a [`Summary`].
#[must_use]
pub fn summarize(rows: &[Row]) -> Summary {
    let mut counts: Vec<(EndClass, usize)> = Vec::new();
    let mut summary = Summary {
        rows: rows.len(),
        since: rows.first().map(|row| row.at),
        ends: Vec::new(),
        unexpected_stalled: 0,
        unexpected_critical: 0,
        last_unexpected: None,
    };
    for row in rows {
        let class = row.prev.class;
        if class == EndClass::None {
            continue;
        }
        match counts.iter_mut().find(|(c, _)| *c == class) {
            Some((_, n)) => *n += 1,
            None => counts.push((class, 1)),
        }
        if class.is_unexpected() {
            summary.unexpected_stalled += usize::from(row.prev.stall == Tri::Yes);
            summary.unexpected_critical += usize::from(row.prev.pressure == Pressure::Critical);
            summary.last_unexpected = Some(row.clone());
        }
    }
    counts.sort_by_key(|(class, n)| (std::cmp::Reverse(*n), class.token()));
    summary.ends = counts;
    summary
}

/// The ledger as the fact of `aterm doctor`'s `recovery:` row — its lines, without
/// the label, which the row prints once — and whether any recorded run ended
/// unexpectedly (a NOTE there, never a failure: a past crash is not a reason to
/// refuse to launch). `rows` is [`read_rows`]'s answer for `path`.
#[must_use]
pub fn doctor_lines(rows: &[Row], path: &Path) -> (Vec<String>, bool) {
    let summary = summarize(rows);
    if summary.rows == 0 {
        return (
            vec![format!(
                "no windowed launch of the installed app is recorded yet; the \
                 census starts with the next one ({})",
                path.display()
            )],
            false,
        );
    }
    let since = summary
        .since
        .map(aterm_types::rfc3339::format_rfc3339)
        .unwrap_or_default();
    let launches = if summary.rows == 1 {
        "1 windowed launch".to_string()
    } else {
        format!("{} windowed launches", summary.rows)
    };
    let ends = if summary.ends.is_empty() {
        "no earlier run to classify yet".to_string()
    } else {
        let parts: Vec<String> = summary
            .ends
            .iter()
            .map(|(class, n)| format!("{n} {}", class.describe()))
            .collect();
        format!("the runs before them ended: {}", parts.join(", "))
    };
    let mut lines = vec![format!("{launches} since {since}; {ends}")];
    if let Some(row) = &summary.last_unexpected {
        let prev = &row.prev;
        let mut words = vec![prev.class.describe()];
        words.push(match (prev.build, prev.version.as_deref()) {
            (Some(build), Some(version)) => format!("build {build} ({version})"),
            (Some(build), None) => format!("build {build}"),
            (None, _) => "build not recorded".to_string(),
        });
        if let Some(uptime) = prev.uptime {
            words.push(format!(
                "up {}{}",
                span_words(uptime.secs),
                if uptime.at_least { " or more" } else { "" }
            ));
        }
        words.push(match prev.stall {
            Tri::Yes => "the main thread was stalled at its end".to_string(),
            Tri::No => "no stall at its end".to_string(),
            Tri::Unknown => "stall not known".to_string(),
        });
        words.push(match prev.pressure {
            Pressure::Critical => "memory pressure critical".to_string(),
            Pressure::Warn => "memory pressure at warning".to_string(),
            Pressure::Normal | Pressure::None => "memory pressure normal".to_string(),
            Pressure::Unknown => "memory pressure not known".to_string(),
        });
        lines.push(format!(
            "the last unexpected end, found at the {} launch: {}",
            aterm_types::rfc3339::format_rfc3339(row.at),
            words.join("; ")
        ));
        if summary.unexpected_stalled + summary.unexpected_critical > 0 {
            lines.push(format!(
                "of the unexpected ends, {} had a stalled main thread and {} \
                 critical memory pressure at the end",
                summary.unexpected_stalled, summary.unexpected_critical
            ));
        }
    }
    lines.push(format!("ledger {}", path.display()));
    (lines, summary.any_unexpected())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: u64, pid: u32, prev: PrevRun) -> Row {
        Row {
            at,
            pid,
            started: at - 1,
            build: 1_790_305_290,
            version: "0.93.0".into(),
            launch: LaunchKind::Cold,
            prev,
        }
    }

    fn prev(class: EndClass) -> PrevRun {
        PrevRun {
            pid: Some(7641),
            build: Some(1_790_000_000),
            version: Some("0.92.0".into()),
            class,
            uptime: Some(Uptime {
                secs: 172_800,
                at_least: true,
            }),
            stall: Tri::Yes,
            pressure: Pressure::Critical,
            trial: 2,
        }
    }

    #[test]
    fn a_row_round_trips_through_its_line_for_every_class() {
        for class in [
            EndClass::Clean,
            EndClass::Handoff,
            EndClass::Killed,
            EndClass::Signal(11),
            EndClass::Panic,
            EndClass::Running,
            EndClass::Unknown,
            EndClass::None,
        ] {
            let r = row(1_790_500_000, 123, prev(class));
            let line = r.to_line();
            assert!(!line.contains('\n'));
            assert_eq!(Row::parse(&line), Some(r), "{line}");
        }
        let mut exact = row(1_790_500_000, 9, PrevRun::none());
        exact.prev.uptime = Some(Uptime {
            secs: 5,
            at_least: false,
        });
        exact.launch = LaunchKind::Successor;
        assert_eq!(Row::parse(&exact.to_line()), Some(exact));
    }

    #[test]
    fn a_line_is_one_line_whatever_the_version_string_holds() {
        let mut r = row(10, 1, PrevRun::none());
        r.version = "0.93.0 injected=1\nv=1".into();
        r.prev.version = Some("a=b".into());
        let line = r.to_line();
        assert!(!line.contains('\n'));
        let back = Row::parse(&line).expect("a sanitized row parses");
        assert_eq!(back.version, "?");
        assert_eq!(back.prev.version, None);
    }

    #[test]
    fn malformed_and_foreign_lines_are_skipped_not_fatal() {
        let good = row(20, 2, prev(EndClass::Killed)).to_line();
        let text = format!(
            "garbage\nv=2 at=1\n{}\nv=1 at=x pid=1\n\n{good}\n",
            row(10, 1, PrevRun::none()).to_line()
        );
        let rows = parse_rows(&text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].prev.class, EndClass::Killed);
    }

    #[test]
    fn the_summary_counts_ends_and_names_the_last_unexpected_one() {
        let rows = vec![
            row(1_790_000_000, 1, PrevRun::none()),
            row(1_790_000_100, 2, prev(EndClass::Clean)),
            row(1_790_000_200, 3, prev(EndClass::Killed)),
            row(1_790_000_300, 4, prev(EndClass::Clean)),
            row(1_790_000_400, 5, prev(EndClass::Signal(6))),
            row(1_790_000_500, 6, prev(EndClass::Handoff)),
        ];
        let s = summarize(&rows);
        assert_eq!(s.rows, 6);
        assert_eq!(
            s.ends,
            vec![
                (EndClass::Clean, 2),
                (EndClass::Handoff, 1),
                (EndClass::Killed, 1),
                (EndClass::Signal(6), 1),
            ]
        );
        assert_eq!(s.unexpected_stalled, 2);
        assert_eq!(s.unexpected_critical, 2);
        assert_eq!(s.last_unexpected.as_ref().map(|r| r.pid), Some(5));
        let (lines, unexpected) = doctor_lines(&rows, Path::new("/x/recovery-ledger.log"));
        assert!(unexpected);
        assert!(lines[0].starts_with("6 windowed launches since 2026-09-21"));
        assert!(lines[0].contains("2 clean"), "{}", lines[0]);
        assert!(
            lines[1].contains("fatal signal 6 (SIGABRT)"),
            "{}",
            lines[1]
        );
        assert!(lines[1].contains("up 2 d 0 h or more"), "{}", lines[1]);
        assert!(lines[1].contains("stalled at its end"), "{}", lines[1]);
    }

    #[test]
    fn an_empty_or_all_clean_ledger_is_no_note() {
        let (lines, unexpected) = doctor_lines(&[], Path::new("/x/l"));
        assert!(!unexpected);
        assert!(lines[0].contains("no windowed launch"));
        let rows = vec![row(1, 1, PrevRun::none()), row(2, 2, prev(EndClass::Clean))];
        let (lines, unexpected) = doctor_lines(&rows, Path::new("/x/l"));
        assert!(!unexpected);
        assert!(lines[0].contains("1 clean"));
    }

    #[test]
    fn spans_read_as_a_person_says_them() {
        assert_eq!(span_words(42), "42 s");
        assert_eq!(span_words(17 * 60 + 5), "17 min");
        assert_eq!(span_words(3 * 3600 + 5 * 60), "3 h 5 min");
        assert_eq!(span_words(2 * 86_400 + 4 * 3600), "2 d 4 h");
    }

    #[cfg(unix)]
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-recovery-ledger-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[cfg(unix)]
    #[test]
    fn append_creates_a_private_file_and_keeps_rows_in_order() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("append");
        let path = ledger_path(&dir);
        append_row(&path, &row(1, 1, PrevRun::none())).expect("append 1");
        append_row(&path, &row(2, 2, prev(EndClass::Clean))).expect("append 2");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let rows = read_rows(&path);
        assert_eq!(rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![1, 2]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_ledger_past_the_trim_mark_keeps_the_newest_rows() {
        let dir = scratch("trim");
        let path = ledger_path(&dir);
        let mut n: u32 = 0;
        // Append until the file crosses TRIM_AT_BYTES and the append trims it.
        loop {
            n += 1;
            append_row(&path, &row(u64::from(n) + 10, n, prev(EndClass::Clean))).expect("append");
            let rows = read_rows(&path);
            if rows.len() < n as usize {
                assert_eq!(
                    rows.len(),
                    KEEP_ROWS,
                    "a trim keeps exactly the newest rows"
                );
                assert_eq!(
                    rows.last().map(|r| r.pid),
                    Some(n),
                    "the newest row survives"
                );
                assert_eq!(rows.first().map(|r| r.pid), Some(n + 1 - KEEP_ROWS as u32));
                break;
            }
            assert!(n < 10_000, "the ledger never trimmed");
        }
        assert!(std::fs::metadata(&path).expect("meta").len() <= TRIM_AT_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_in_its_place_is_never_read_or_followed() {
        let dir = scratch("symlink");
        let path = ledger_path(&dir);
        let target = dir.join("elsewhere");
        std::fs::write(
            &target,
            format!("{}\n", row(1, 1, PrevRun::none()).to_line()),
        )
        .expect("target");
        std::os::unix::fs::symlink(&target, &path).expect("symlink");
        assert!(read_rows(&path).is_empty(), "a symlink is not the ledger");
        assert!(
            append_row(&path, &row(2, 2, PrevRun::none())).is_err(),
            "an append never writes through a symlink"
        );
        assert_eq!(
            std::fs::read_to_string(&target)
                .expect("target")
                .lines()
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
