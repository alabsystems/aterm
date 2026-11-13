// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recovery ledger — records how each run of the installed app ended.

use std::path::{Path, PathBuf};

/// The ledger's file name in the log directory.
pub const LEDGER_FILE_NAME: &str = "recovery-ledger.log";

/// How many rows a trim keeps (the newest).
pub const KEEP_ROWS: usize = 256;

/// A ledger larger than this is trimmed to [`KEEP_ROWS`] after the append that crossed it.
pub const TRIM_AT_BYTES: u64 = 128 * 1024;

/// The most bytes [`read_rows`] reads — the tail of an oversized file, so no read in the log directory is unbounded.
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
    /// It removed its crash marker on an exit path: a quit, an update's exit, a refused start.
    Clean,
    /// It handed its sessions to an update successor: the successor's own row says so.
    Handoff,
    /// No crash signal was caught and no exit path ran: SIGKILL (Force Quit, jetsam, a harness), a kernel kill, power loss, or SIGTERM/SIGINT/SIGHUP, whose handler (aterm-gui's `owned_endpoint`) only removes the control socket, token and alias before re-raising.
    Killed,
    /// A fatal signal the handler caught (SIGSEGV 11, SIGABRT 6, SIGBUS 10, SIGILL 4, SIGFPE 8); the number is what the handler wrote into the marker.
    Signal(u8),
    /// It filed a Rust panic report.
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
            other => Self::Signal(crate::parse_uint(other.strip_prefix("signal-")?)?),
        })
    }

    /// Whether this end is one the person did not ask for — the class the keeper exists for.
    #[must_use]
    pub const fn is_unexpected(self) -> bool {
        matches!(self, Self::Killed | Self::Signal(_) | Self::Panic)
    }

    /// The words for a person.
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Clean => "clean".into(),
            Self::Handoff => "handed off to an update".into(),
            Self::Killed => "killed (no crash signal, no exit path)".into(),
            Self::Signal(n) => format!("fatal signal {n} ({})", signal_name(n)),
            Self::Panic => "panic".into(),
            Self::Running => "still running when the launch after it began".into(),
            Self::Unknown => "no evidence left".into(),
            Self::None => "nothing earlier recorded".into(),
        }
    }
}

/// The conventional name of the fatal signals the handler traps (Darwin numbering).
const fn signal_name(n: u8) -> &'static str {
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
    const fn token(self) -> &'static str {
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
    const fn token(self) -> &'static str {
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
    /// `true` when the end is only a lower bound — the run's last log line, for a run whose death left no time of its own (a SIGKILL).
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
    /// Launches the boot sentinel had counted against its build when this row was written (0: no boot trial pending for that build).
    pub trial: u32,
}

impl PrevRun {
    /// The "nothing earlier" value of a ledger's first row.
    #[must_use]
    pub const fn none() -> Self {
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
    const fn token(self) -> &'static str {
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

/// How a launch's keeper recovery brought its screens back (`aterm-gui`'s `crash_restore`), summing the per-session `status` fields across every session offered by a keeper.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restores {
    /// Sessions whose screen came back exactly.
    pub exact: u32,
    /// Sessions whose screen came back with a loss counted (repainted).
    pub screen: u32,
    /// Sessions that came back blank (no region, or a restore that failed).
    pub meta: u32,
    /// PTY bytes the dead window read and never committed to a tail, over every session.
    pub in_flight_lost: u64,
    /// Sessions whose keyframe's tail had been overwritten.
    pub tail_overflows: u32,
    /// Verified shell-integration marks left in a tail, over every session.
    pub marks_lost: u64,
    /// Host resizes no keyframe or tail reached, over every session.
    pub resizes_unjournaled: u64,
    /// Scrollback lines the restores could not bring back, over every session.
    pub history_lost: u64,
    /// Sessions whose restore lost any scrollback.
    pub history_short: u32,
    /// How many sessions each `restore_why` word named, by word (no `-`).
    pub whys: Vec<(String, u32)>,
    /// Sessions a keeper offered that the launch refused and released: lost, on no rung.
    pub refused: u32,
}

impl Restores {
    /// The sessions the recovery admitted, on any rung (the refused ones are not).
    #[must_use]
    pub const fn recovered(&self) -> u32 {
        self.exact
            .saturating_add(self.screen)
            .saturating_add(self.meta)
    }

    fn token(&self) -> String {
        format!("{}:{}:{}", self.exact, self.screen, self.meta)
    }

    fn parse(token: &str) -> Option<Self> {
        let mut parts = token.split(':');
        let restores = Self {
            exact: crate::parse_uint(parts.next()?)?,
            screen: crate::parse_uint(parts.next()?)?,
            meta: crate::parse_uint(parts.next()?)?,
            ..Self::default()
        };
        parts.next().is_none().then_some(restores)
    }

    fn lost_token(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.in_flight_lost, self.tail_overflows, self.marks_lost, self.resizes_unjournaled
        )
    }

    fn history_token(&self) -> String {
        format!("{}:{}", self.history_lost, self.history_short)
    }

    fn parse_history(&mut self, token: &str) -> Option<()> {
        let mut parts = token.split(':');
        self.history_lost = crate::parse_uint(parts.next()?)?;
        self.history_short = crate::parse_uint(parts.next()?)?;
        parts.next().is_none().then_some(())
    }

    fn parse_lost(&mut self, token: &str) -> Option<()> {
        let mut parts = token.split(':');
        self.in_flight_lost = crate::parse_uint(parts.next()?)?;
        self.tail_overflows = crate::parse_uint(parts.next()?)?;
        self.marks_lost = crate::parse_uint(parts.next()?)?;
        self.resizes_unjournaled = crate::parse_uint(parts.next()?)?;
        parts.next().is_none().then_some(())
    }

    /// `word:n,…`, or `-` for none. A word that could break the token (empty, or holding
    /// a separator the line cannot carry) is written `?`.
    fn why_token(&self) -> String {
        if self.whys.is_empty() {
            return "-".to_string();
        }
        self.whys
            .iter()
            .map(|(word, n)| {
                let word = if word.is_empty() || word.contains([':', ',']) {
                    "?"
                } else {
                    clean_token(word)
                };
                format!("{word}:{n}")
            })
            .collect::<Vec<_>>()
            .join(",")
    }

    fn parse_whys(&mut self, token: &str) -> Option<()> {
        self.whys.clear();
        if token == "-" {
            return Some(());
        }
        for pair in token.split(',') {
            let (word, n) = pair.split_once(':')?;
            if word.is_empty() || word.contains([':', ',']) || clean_token(word) != word {
                return None;
            }
            self.whys.push((word.to_string(), crate::parse_uint(n)?));
        }
        Some(())
    }
}

/// Whether the launch that wrote a row kept its terminals with a PTY keeper (`keeper=`): the state of its keeper link (`aterm-gui`'s `keeper_link`) when the row was written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeeperAtLaunch {
    /// The link is off: `[keeper] enabled = false`, a launch whose config leaves a keeper to a bundled window, or a build with no code identity to check a keeper by (it fails closed).
    #[default]
    Off,
    /// The link is on and no keeper answered the boot HELLO (none running, or one that is not this build's): terminals are registered once one answers.
    Absent,
    /// The link is on and a keeper answered the boot HELLO: every terminal is registered with it.
    On,
    /// The link is on, in an update's successor, which says no boot HELLO and registers what it adopted at its Commit, with whichever keeper answers then.
    Armed,
}

impl KeeperAtLaunch {
    /// The row word (`off`, `absent`, `on`, `armed`).
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Absent => "absent",
            Self::On => "on",
            Self::Armed => "armed",
        }
    }

    /// The inverse of [`Self::token`].
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "off" => Self::Off,
            "absent" => Self::Absent,
            "on" => Self::On,
            "armed" => Self::Armed,
            _ => return None,
        })
    }

    /// Whether the link was on (whatever answered it).
    #[must_use]
    pub const fn link_on(self) -> bool {
        !matches!(self, Self::Off)
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
    /// The microseconds within [`Self::started`] of the kernel's birth record (`started_us=`), when the row's writer had one: with [`Self::pid`] it names the process exactly, where the second alone matches a pid reused within it.
    pub started_us: Option<u32>,
    /// The launch's build number.
    pub build: u64,
    /// The launch's version.
    pub version: String,
    /// How the launch began.
    pub launch: LaunchKind,
    /// The run before it.
    pub prev: PrevRun,
    /// The launch's keeper link ([`KeeperAtLaunch`]); `None` on a row an older build wrote.
    pub keeper: Option<KeeperAtLaunch>,
    /// How this launch's keeper recovery brought its screens back, when it recovered any ([`Restores`]).
    pub restores: Option<Restores>,
}

/// A value token with no space, `=`, or control character in it — the one shape the
/// line format can carry.
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
        crate::parse_uint(token).map(Some)
    }
}

impl Row {
    /// Whether this row is the process born `started` (Unix seconds) and `started_us` (its microseconds, when known) with `pid`: to the microsecond when both sides carry it, else to the second — a pid reused within the second matches only then.
    #[must_use]
    pub const fn is_process(&self, pid: u32, started: u64, started_us: Option<u32>) -> bool {
        self.pid == pid
            && self.started == started
            && match (self.started_us, started_us) {
                (Some(ours), Some(theirs)) => ours == theirs,
                _ => true,
            }
    }

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
        let started_us = self
            .started_us
            .map_or_else(String::new, |us| format!(" started_us={us}"));
        let keeper = self
            .keeper
            .map_or_else(String::new, |k| format!(" keeper={}", k.token()));
        let restored = self.restores.as_ref().map_or_else(String::new, |r| {
            let refused = if r.refused == 0 {
                String::new()
            } else {
                format!(" restore_refused={}", r.refused)
            };
            format!(
                " restored={} restore_lost={} restore_why={} restore_history={}{refused}",
                r.token(),
                r.lost_token(),
                r.why_token(),
                r.history_token()
            )
        });
        format!(
            "v={ROW_VERSION} at={} pid={} started={}{started_us} build={} version={} launch={} \
             prev={} prev_pid={} prev_build={} prev_version={} prev_uptime={uptime} \
             prev_stall={} prev_pressure={} prev_trial={}{keeper}{restored}",
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
        let mut fields: Vec<(&str, &str)> = Vec::with_capacity(18);
        for pair in line.split_whitespace() {
            let (key, value) = pair.split_once('=')?;
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || clean_token(value) != value
            {
                return None;
            }
            if fields.iter().any(|(k, _)| *k == key) {
                return None;
            }
            fields.push((key, value));
        }
        let get = |key: &str| fields.iter().find_map(|(k, v)| (*k == key).then_some(*v));
        if crate::parse_uint::<u32>(get("v")?)? != ROW_VERSION {
            return None;
        }
        let uptime = match get("prev_uptime")? {
            "?" => None,
            token => {
                let (digits, at_least) = match token.strip_suffix('+') {
                    Some(digits) => (digits, true),
                    None => (token, false),
                };
                Some(Uptime {
                    secs: crate::parse_uint(digits)?,
                    at_least,
                })
            }
        };
        let text = |token: &str| (token != "?").then(|| token.to_string());
        Some(Self {
            at: crate::parse_uint(get("at")?)?,
            pid: crate::parse_uint(get("pid")?)?,
            started: crate::parse_uint(get("started")?)?,
            started_us: match get("started_us") {
                None => None,
                Some(token) => Some(crate::parse_uint(token).filter(|us| *us < 1_000_000)?),
            },
            build: crate::parse_uint(get("build")?)?,
            version: get("version")?.to_string(),
            launch: LaunchKind::parse(get("launch")?)?,
            prev: PrevRun {
                pid: match get("prev_pid")? {
                    "?" => None,
                    token => Some(crate::parse_uint(token)?),
                },
                build: parse_opt_u64(get("prev_build")?)?,
                version: text(get("prev_version")?),
                class: EndClass::parse(get("prev")?)?,
                uptime,
                stall: Tri::parse(get("prev_stall")?)?,
                pressure: Pressure::parse(get("prev_pressure")?)?,
                trial: crate::parse_uint(get("prev_trial")?)?,
            },
            keeper: match get("keeper") {
                None => None,
                Some(token) => Some(KeeperAtLaunch::parse(token)?),
            },
            restores: match get("restored") {
                None => None,
                Some(token) => {
                    let mut restores = Restores::parse(token)?;
                    if let Some(lost) = get("restore_lost") {
                        restores.parse_lost(lost)?;
                    }
                    if let Some(whys) = get("restore_why") {
                        restores.parse_whys(whys)?;
                    }
                    if let Some(history) = get("restore_history") {
                        restores.parse_history(history)?;
                    }
                    if let Some(refused) = get("restore_refused") {
                        restores.refused = crate::parse_uint(refused)?;
                    }
                    Some(restores)
                }
            },
        })
    }
}

/// Every row [`Row::parse`] accepts in `text`, oldest first.
#[must_use]
pub fn parse_rows(text: &str) -> Vec<Row> {
    text.lines().filter_map(Row::parse).collect()
}

/// The ledger's rows, oldest first; empty when there is no ledger or it cannot be read.
#[must_use]
pub fn read_rows(path: &Path) -> Vec<Row> {
    read_text(path)
        .map(|text| parse_rows(&text))
        .unwrap_or_default()
}

/// What [`read_rows_checked`] read: the rows this build understands, and how many it skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Checked {
    /// Every row this build parses, in ledger order.
    pub rows: Vec<Row>,
    /// Rows this build cannot interpret (a newer build's end, a torn line), left out.
    pub skipped: usize,
}

/// The doctor's ledger read: a missing ledger is empty history and an unreadable file is
/// missing evidence, not a clean history; a row this build cannot interpret (one a newer build
/// wrote, a torn line) is skipped and counted, so the rest of the history still reads.
/// # Errors
/// The read error.
pub fn read_rows_checked(path: &Path) -> std::io::Result<Checked> {
    let mut checked = Checked::default();
    for line in read_text(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
    {
        match Row::parse(line) {
            Some(row) => checked.rows.push(row),
            None => checked.skipped = checked.skipped.saturating_add(1),
        }
    }
    Ok(checked)
}

fn read_text(path: &Path) -> std::io::Result<String> {
    match open_for_read(path) {
        Ok(file) => read_opened_text(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => {
            if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_file()) {
                return Err(std::io::Error::other(
                    "recovery ledger is not a regular file",
                ));
            }
            Err(error)
        }
    }
}

/// Admission and the tail bound belong to the descriptor actually read. The
/// path can name a different object by the time its nonblocking open completes.
fn read_opened_text(mut file: std::fs::File) -> std::io::Result<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(std::io::Error::other(
            "recovery ledger is not a regular file",
        ));
    }
    let skip = meta.len().saturating_sub(MAX_READ_BYTES);
    if skip > 0 {
        file.seek(SeekFrom::Start(skip))?;
    }
    let cap = usize::try_from(
        meta.len()
            .saturating_sub(skip)
            .min(MAX_READ_BYTES)
            .saturating_add(1),
    )
    .unwrap_or(0);
    let mut bytes = Vec::with_capacity(cap);
    file.take(MAX_READ_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if skip > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    Ok(text.to_owned())
}

/// Open `path` for reading without following a final symlink or blocking on a FIFO.
#[cfg(unix)]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_for_read(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Append `row` to the ledger at `path` (created `0600`), then trim it to the newest [`KEEP_ROWS`] when it has passed [`TRIM_AT_BYTES`].
/// # Errors
/// The open or the write failed.
#[cfg(unix)]
pub fn append_row(path: &Path, row: &Row) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let mut line = row.to_line();
    line.push('\n');
    for _ in 0..4 {
        let file = aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
                .open(path)
        })?;
        let ours = file.metadata()?;
        if !ours.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "recovery ledger is not a regular file",
            ));
        }
        let lock = lock_briefly(&file);
        let same_inode = match std::fs::symlink_metadata(path) {
            Ok(now) => ours.ino() == now.ino() && ours.dev() == now.dev(),
            Err(_) => false,
        };
        if lock.is_some() && !same_inode {
            continue;
        }
        (&file).write_all(line.as_bytes())?;
        if lock.is_some() && file.metadata().is_ok_and(|m| m.len() > TRIM_AT_BYTES) {
            trim_locked(path);
        }
        return Ok(());
    }
    Err(std::io::Error::other(
        "the ledger was replaced under every attempt",
    ))
}

/// `flock(LOCK_EX)` without waiting, retried every 20 ms for about a second: the
/// held lock, or `None` when it stayed taken.
#[cfg(unix)]
fn lock_briefly(file: &std::fs::File) -> Option<LedgerLock<'_>> {
    use std::os::unix::io::AsRawFd as _;
    for _ in 0..50 {
        // SAFETY: `file` owns a live descriptor for the call; LOCK_NB never waits.
        #[expect(
            clippy::disallowed_methods,
            reason = "the take lands in a LedgerLock, whose Drop releases by LOCK_UN"
        )]
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if taken {
            return Some(LedgerLock(file));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    None
}

/// A held [`lock_briefly`], released via `LOCK_UN` on drop rather than by
/// `close` alone: `flock` lives on the open file description, and a child
/// forked concurrently holds its inherited copy of the descriptor until `exec`
/// (`O_CLOEXEC` closes at exec, not fork).
#[cfg(unix)]
struct LedgerLock<'a>(&'a std::fs::File);

#[cfg(unix)]
impl Drop for LedgerLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Rewrite the ledger at `path` with its newest [`KEEP_ROWS`] rows, by a `0600`
/// temporary beside it renamed over it.
#[cfg(unix)]
fn trim_locked(path: &Path) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let rows = read_rows(path);
    let keep = &rows[rows.len().saturating_sub(KEEP_ROWS)..];
    let mut text = String::with_capacity(keep.len().saturating_mul(160));
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(text.as_bytes())?;
            aterm_durable::durable(&f)
        });
    if written.is_err() || std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    } else if let Some(dir) = path.parent() {
        let _ = aterm_durable::durable_dir(dir);
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
    /// Of the unexpected ends, how many had an unended stall, and how many critical memory pressure, as their last word.
    pub unexpected_stalled: usize,
    /// See [`Self::unexpected_stalled`].
    pub unexpected_critical: usize,
    /// The newest row that recorded an unexpected end.
    pub last_unexpected: Option<Row>,
}

impl Summary {
    /// Whether any recorded run ended unexpectedly.
    #[must_use]
    pub const fn any_unexpected(&self) -> bool {
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

/// The ledger as the fact of `aterm doctor`'s `recovery:` row — its lines, without the label, which the row prints once — and whether any recorded run ended unexpectedly (a note there, never a failure: a past crash is not a reason to refuse to launch).
#[must_use]
pub fn doctor_lines(rows: &[Row], path: &Path) -> (Vec<String>, bool) {
    let summary = summarize(rows);
    if summary.rows == 0 {
        return (
            vec![format!(
                "no windowed launch recorded yet (installed app only): {}",
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
        format!("prior runs ended: {}", parts.join(", "))
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
            Tri::Yes => "main thread stalled at its end".to_string(),
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
            "last unexpected end, found at the {} launch: {}",
            aterm_types::rfc3339::format_rfc3339(row.at),
            words.join("; ")
        ));
        if summary.unexpected_stalled + summary.unexpected_critical > 0 {
            lines.push(format!(
                "unexpected ends: {} with a stalled main thread, {} with critical \
                 memory pressure",
                summary.unexpected_stalled, summary.unexpected_critical
            ));
        }
    }
    lines.push(format!("ledger {}", path.display()));
    (lines, summary.any_unexpected())
}

#[cfg(test)]
#[path = "recovery_ledger_tests.rs"]
mod tests;
