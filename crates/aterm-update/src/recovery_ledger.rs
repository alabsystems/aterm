// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Recovery ledger — records how each run of the installed app ended.
//!
//! One row is written per windowed launch of the installed app and per update
//! successor at its commit. Each row records the launch (pid, birth, build,
//! version) and classifies the run before it: how it ended ([`EndClass`]), its
//! build and version, how long it ran ([`Uptime`]), whether the main-thread
//! watchdog's last word for it was an unended stall ([`Tri`]), the last
//! memory-pressure level the OS reported ([`Pressure`]), and how many launches
//! the updater's boot trial had counted against its build. `aterm doctor`
//! summarizes the ledger via [`doctor_lines`].
//!
//! Stored at [`LEDGER_FILE_NAME`] in [`aterm_types::dirs::logs_dir`] (`0600`),
//! one space-separated `key=value` line per row, newest last, trimmed to the
//! newest [`KEEP_ROWS`] once it passes [`TRIM_AT_BYTES`]. Unparseable lines are
//! skipped.

use std::path::{Path, PathBuf};

/// The ledger's file name in the log directory. Deliberately not `crash-*.log`: the
/// crash scan reads every non-empty file of that shape as a crash report.
pub const LEDGER_FILE_NAME: &str = "recovery-ledger.log";

/// How many rows a trim keeps (the newest).
pub const KEEP_ROWS: usize = 256;

/// A ledger larger than this is trimmed to [`KEEP_ROWS`] after the append that
/// crossed it. A row is about 250 bytes, so this is roughly twice [`KEEP_ROWS`].
pub const TRIM_AT_BYTES: u64 = 128 * 1024;

/// The most bytes [`read_rows`] reads — the tail of an oversized file, so no
/// read in the log directory is unbounded.
pub const MAX_READ_BYTES: u64 = 512 * 1024;

/// The format version every row this build writes carries (`v=1`).
///
/// New fields ride as optional keys under the same version (`started_us=`,
/// `restored=`, …): every reader skips a row whose version is not its own, so a
/// new version would hide newer rows from an older build that a crash-looping
/// update reverts to. An older row lacks the key and still parses.
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
    /// No crash signal was caught and no exit path ran: SIGKILL (Force Quit, jetsam,
    /// a harness), a kernel kill, power loss — and SIGTERM, SIGINT or SIGHUP, whose
    /// handler (aterm-gui's `owned_endpoint`) takes only the control socket, token
    /// and alias away before re-raising the default action. Its empty marker
    /// outlived it.
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

/// How a launch's keeper recovery brought its screens back (`aterm-gui`'s
/// `crash_restore`), summing the per-session `status` fields across every
/// session offered by a keeper. Each token is optional on its own (an older row
/// lacking a key still parses; a malformed value rejects the row):
///
/// * `restored=<exact>:<screen>:<meta>`: how many sessions came back exactly, as a screen
///   with a loss counted, or blank (the META rung);
/// * `restore_lost=<in_flight_lost>:<tail_overflows>:<marks_lost>:<resizes_unjournaled>`:
///   the PTY bytes read and never processed, the sessions whose tail overflowed, the
///   verified marks left in a tail, and the resizes nothing journaled, over every session;
/// * `restore_why=<word>:<n>,…`: how many sessions each `status restore_why=` word named
///   (every rung but exact; `-` when every session was exact), in word order;
/// * `restore_history=<lines>:<sessions>`: the scrollback lines the restores could not bring
///   back (each session's first `status history_lost=`), and how many sessions lost any;
/// * `restore_refused=<n>` (written when non-zero): the sessions a keeper offered that this
///   launch refused and released (unreadable tag, missing session id, reused pid, or non-tty
///   fd). A recovery that refused every offer writes `restored=0:0:0` beside it.
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
    pub fn recovered(&self) -> u32 {
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
            exact: parts.next()?.parse().ok()?,
            screen: parts.next()?.parse().ok()?,
            meta: parts.next()?.parse().ok()?,
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
        self.history_lost = parts.next()?.parse().ok()?;
        self.history_short = parts.next()?.parse().ok()?;
        parts.next().is_none().then_some(())
    }

    fn parse_lost(&mut self, token: &str) -> Option<()> {
        let mut parts = token.split(':');
        self.in_flight_lost = parts.next()?.parse().ok()?;
        self.tail_overflows = parts.next()?.parse().ok()?;
        self.marks_lost = parts.next()?.parse().ok()?;
        self.resizes_unjournaled = parts.next()?.parse().ok()?;
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
            if word.is_empty() {
                return None;
            }
            self.whys.push((word.to_string(), n.parse().ok()?));
        }
        Some(())
    }
}

/// Whether the launch that wrote a row kept its terminals with a PTY keeper (`keeper=`):
/// the state of its keeper link (`aterm-gui`'s `keeper_link`) when the row was written.
/// The crash restore's soak ([`crate::restore_soak`]) reads it, because a ledger of
/// recoveries alone cannot tell a run with no keeper from one whose keeper took its
/// terminals and then failed to give them back, nor say how long the keeper has been on.
/// Optional on the row like [`Restores`]: an older row has none and still parses, and a
/// word this build cannot read refuses the row rather than misread it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeeperAtLaunch {
    /// The link is off: `[keeper] enabled` is absent or false, or the build has no code
    /// identity to check a keeper by (it fails closed). Nothing was registered.
    #[default]
    Off,
    /// The link is on and no keeper answered the boot HELLO (none running, or one that
    /// is not this build's): terminals are registered once one answers. The link polls
    /// (`aterm_keeper::client::LINK_POLL`, backing off to 5 s), so a keeper started
    /// while the run lives (`aterm keeper start` in one of its tabs) holds every terminal
    /// within seconds: such a run's kill is one its keeper held.
    Absent,
    /// The link is on and a keeper answered the boot HELLO: every terminal is registered
    /// with it.
    On,
    /// The link is on, in an update's successor, which says no boot HELLO and registers
    /// what it adopted at its Commit, with whichever keeper answers then.
    Armed,
}

impl KeeperAtLaunch {
    /// The row word (`off`, `absent`, `on`, `armed`).
    #[must_use]
    pub fn token(self) -> &'static str {
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
    pub fn link_on(self) -> bool {
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
    /// The microseconds within [`Self::started`] of the kernel's birth record
    /// (`started_us=`), when the row's writer had one: with [`Self::pid`] it names the
    /// process exactly, where the second alone matches a pid reused within it. `None`
    /// on a row an older build wrote, or a birth that came from the clock.
    pub started_us: Option<u32>,
    /// The launch's build number.
    pub build: u64,
    /// The launch's version.
    pub version: String,
    /// How the launch began.
    pub launch: LaunchKind,
    /// The run before it.
    pub prev: PrevRun,
    /// The launch's keeper link ([`KeeperAtLaunch`]); `None` on a row an older build
    /// wrote.
    pub keeper: Option<KeeperAtLaunch>,
    /// How this launch's keeper recovery brought its screens back, when it recovered
    /// any ([`Restores`]).
    pub restores: Option<Restores>,
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
    /// Whether this row is the process born `started` (Unix seconds) and `started_us`
    /// (its microseconds, when known) with `pid`: to the microsecond when both sides
    /// carry it, else to the second — a pid reused within the second matches only then.
    #[must_use]
    pub fn is_process(&self, pid: u32, started: u64, started_us: Option<u32>) -> bool {
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
            // Optional (an older row has none); one out of range refuses the row.
            started_us: match get("started_us") {
                None => None,
                Some(token) => Some(token.parse().ok().filter(|us| *us < 1_000_000)?),
            },
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
            // Optional (an older row has none); one this build cannot read refuses the row.
            keeper: match get("keeper") {
                None => None,
                Some(token) => Some(KeeperAtLaunch::parse(token)?),
            },
            // Optional (an older row has none); one this build cannot read refuses the row.
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
                        restores.refused = refused.parse().ok()?;
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

/// The ledger's rows, oldest first; empty when there is no ledger or it cannot be
/// read. Reads at most [`MAX_READ_BYTES`] — the tail of a larger file, from its first
/// whole line — and only a regular file (never a FIFO, a device or a symlink's target).
#[must_use]
pub fn read_rows(path: &Path) -> Vec<Row> {
    read_text(path)
        .map(|text| parse_rows(&text))
        .unwrap_or_default()
}

/// The doctor's ledger read: a missing ledger is empty history, while an unreadable
/// file or an unrecognized complete row is missing evidence, not a clean history.
/// Unlike [`read_rows`], this never silently discards a failed read or malformed row.
///
/// # Errors
/// The read error, or a row this build cannot interpret.
pub fn read_rows_checked(path: &Path) -> std::io::Result<Vec<Row>> {
    read_text(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            Row::parse(line)
                .ok_or_else(|| std::io::Error::other("unrecognized recovery ledger row"))
        })
        .collect()
}

fn read_text(path: &Path) -> std::io::Result<String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error),
    };
    if !meta.file_type().is_file() {
        return Err(std::io::Error::other(
            "recovery ledger is not a regular file",
        ));
    }
    read_opened_text(open_for_read(path)?)
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
    let mut bytes = Vec::new();
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
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        // Bound after `file`, so it drops first on every exit — the `continue`, the
        // `?` and the return all release the lock before the descriptor closes.
        let lock = lock_briefly(&file);
        // A trim that renamed a new file over the path between our open and our lock
        // left us holding the retired inode: go round and open the path again.
        let same_inode = match (file.metadata(), std::fs::symlink_metadata(path)) {
            (Ok(ours), Ok(now)) => ours.ino() == now.ino() && ours.dev() == now.dev(),
            _ => false,
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
/// (`O_CLOEXEC` closes at exec, not fork). `LOCK_UN` releases the lock across
/// every copy of the open file description.
#[cfg(unix)]
struct LedgerLock<'a>(&'a std::fs::File);

#[cfg(unix)]
impl Drop for LedgerLock<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
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
/// unexpectedly (a note there, never a failure: a past crash is not a reason to
/// refuse to launch). `rows` is [`read_rows`]'s answer for `path`.
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
mod tests {
    use super::*;

    fn row(at: u64, pid: u32, prev: PrevRun) -> Row {
        Row {
            at,
            pid,
            started: at - 1,
            started_us: None,
            build: 1_790_305_290,
            version: "0.93.0".into(),
            launch: LaunchKind::Cold,
            prev,
            keeper: None,
            restores: None,
        }
    }

    /// A launch records its keeper link (`keeper=`) before any restores; a row without it
    /// (every older row) still parses, and a word this build cannot read refuses the row.
    #[test]
    fn a_launch_s_keeper_link_rides_the_row() {
        for keeper in [
            KeeperAtLaunch::Off,
            KeeperAtLaunch::Absent,
            KeeperAtLaunch::On,
            KeeperAtLaunch::Armed,
        ] {
            let mut r = row(1_790_500_000, 88, prev(EndClass::Killed));
            r.keeper = Some(keeper);
            r.restores = Some(Restores {
                exact: 1,
                ..Restores::default()
            });
            let line = r.to_line();
            let word = format!(" keeper={} restored=1:0:0", keeper.token());
            assert!(line.contains(&word), "{line}");
            assert_eq!(Row::parse(&line), Some(r.clone()), "{line}");
            assert_eq!(KeeperAtLaunch::parse(keeper.token()), Some(keeper));
            let older = line.replace(&format!(" keeper={}", keeper.token()), "");
            assert_eq!(
                Row::parse(&older).map(|row| row.keeper),
                Some(None),
                "an older row"
            );
            let unknown = line.replace(&format!("keeper={}", keeper.token()), "keeper=maybe");
            assert_eq!(Row::parse(&unknown), None, "{unknown}");
        }
        assert!(!KeeperAtLaunch::Off.link_on());
        assert!(KeeperAtLaunch::Absent.link_on());
        assert!(KeeperAtLaunch::Armed.link_on());
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

    /// A row carries the kernel's birth microseconds (`started_us=`) beside its
    /// second under the same row version; an older row without the key still
    /// parses (`None`), and an out-of-range value rejects the row. When both
    /// sides carry microseconds, `is_process` matches to the microsecond so a
    /// pid reused within the same second does not match; when either side lacks
    /// microseconds, it matches to the second.
    #[test]
    fn a_row_names_its_process_to_the_microsecond() {
        let mut r = row(1_790_500_000, 4242, prev(EndClass::Clean));
        r.started_us = Some(123_456);
        let line = r.to_line();
        assert!(
            line.starts_with(&format!(
                "v={ROW_VERSION} at=1790500000 pid=4242 started=1790499999 started_us=123456 "
            )),
            "{line}"
        );
        assert_eq!(Row::parse(&line), Some(r.clone()));
        let older = line.replace(" started_us=123456", "");
        assert_eq!(Row::parse(&older).map(|row| row.started_us), Some(None));
        assert_eq!(Row::parse(&line.replace("123456", "1000000")), None);
        assert_eq!(Row::parse(&line.replace("123456", "x")), None);

        assert!(r.is_process(4242, 1_790_499_999, Some(123_456)));
        assert!(
            !r.is_process(4242, 1_790_499_999, Some(654_321)),
            "a pid reused within the second"
        );
        assert!(r.is_process(4242, 1_790_499_999, None), "the second alone");
        assert!(!r.is_process(4242, 1_790_499_998, Some(123_456)));
        assert!(!r.is_process(4243, 1_790_499_999, Some(123_456)));
        let older = Row::parse(&older).expect("an older row");
        assert!(older.is_process(4242, 1_790_499_999, Some(654_321)));
    }

    /// A launch that recovered sessions records how their screens came back
    /// (`restored=<exact>:<screen>:<meta>`); a row without it (every older row, and a
    /// launch that recovered nothing) still parses, and a token this build cannot read
    /// refuses the row rather than misread it.
    #[test]
    fn a_recovery_s_restores_ride_the_row() {
        let mut r = row(1_790_500_000, 77, prev(EndClass::Killed));
        r.restores = Some(Restores {
            exact: 3,
            screen: 2,
            meta: 1,
            in_flight_lost: 4096,
            tail_overflows: 1,
            marks_lost: 2,
            resizes_unjournaled: 0,
            history_lost: 1200,
            history_short: 2,
            whys: vec![
                ("in-flight".into(), 1),
                ("no-region".into(), 1),
                ("tail-overflow".into(), 1),
            ],
            refused: 0,
        });
        let line = r.to_line();
        let tokens = " restored=3:2:1 restore_lost=4096:1:2:0 \
                      restore_why=in-flight:1,no-region:1,tail-overflow:1 restore_history=1200:2";
        assert!(line.ends_with(tokens), "{line}");
        assert_eq!(Row::parse(&line), Some(r.clone()));

        let mut exact = r.clone();
        exact.restores = Some(Restores {
            exact: 2,
            ..Restores::default()
        });
        let exact_line = exact.to_line();
        assert!(
            exact_line.ends_with(
                " restored=2:0:0 restore_lost=0:0:0:0 restore_why=- restore_history=0:0"
            ),
            "{exact_line}"
        );
        assert_eq!(Row::parse(&exact_line), Some(exact));

        let older = line.replace(tokens, "");
        assert_eq!(
            Row::parse(&older).map(|row| row.restores),
            Some(None),
            "an older row"
        );
        let rungs_only = line.replace(tokens, " restored=3:2:1");
        assert_eq!(
            Row::parse(&rungs_only).and_then(|row| row.restores),
            Some(Restores {
                exact: 3,
                screen: 2,
                meta: 1,
                ..Restores::default()
            }),
            "a row with the rungs alone"
        );
        // A row from before the history ring: the three tokens, no fourth.
        let before_ring = line.replace(" restore_history=1200:2", "");
        assert_eq!(
            Row::parse(&before_ring).and_then(|row| row.restores),
            Some(Restores {
                history_lost: 0,
                history_short: 0,
                ..r.restores.clone().expect("restores")
            }),
            "a row from before the history ring"
        );
        for (key, good, bad) in [
            ("restored", "3:2:1", "3:1"),
            ("restored", "3:2:1", "3:1:0:9"),
            ("restored", "3:2:1", "3:x:0"),
            ("restored", "3:2:1", ""),
            ("restore_lost", "4096:1:2:0", "4096:1:2"),
            ("restore_lost", "4096:1:2:0", "4096:1:2:0:5"),
            (
                "restore_why",
                "in-flight:1,no-region:1,tail-overflow:1",
                "in-flight",
            ),
            (
                "restore_why",
                "in-flight:1,no-region:1,tail-overflow:1",
                ":1",
            ),
            (
                "restore_why",
                "in-flight:1,no-region:1,tail-overflow:1",
                "in-flight:x",
            ),
            ("restore_history", "1200:2", "1200"),
            ("restore_history", "1200:2", "1200:2:3"),
            ("restore_history", "1200:2", "x:2"),
        ] {
            let bad_line = line.replace(&format!("{key}={good}"), &format!("{key}={bad}"));
            assert_ne!(bad_line, line, "{key}={bad}");
            assert_eq!(Row::parse(&bad_line), None, "{bad_line}");
        }
    }

    /// The sessions a keeper offered and the launch refused ride the row
    /// (`restore_refused=`), a recovery that refused every offer included; a row without the
    /// word (every older row, and one that refused none) reads 0, and one this build cannot
    /// read refuses the row.
    #[test]
    fn a_recovery_s_refusals_ride_the_row() {
        let mut r = row(1_790_500_000, 77, prev(EndClass::Killed));
        r.restores = Some(Restores {
            exact: 3,
            refused: 1,
            ..Restores::default()
        });
        let line = r.to_line();
        assert!(
            line.ends_with(" restored=3:0:0 restore_lost=0:0:0:0 restore_why=- restore_history=0:0 restore_refused=1"),
            "{line}"
        );
        assert_eq!(Row::parse(&line), Some(r.clone()));
        assert_eq!(r.restores.as_ref().map(Restores::recovered), Some(3));

        let mut all_refused = r.clone();
        all_refused.restores = Some(Restores {
            refused: 4,
            ..Restores::default()
        });
        let all_line = all_refused.to_line();
        assert!(all_line.contains(" restored=0:0:0 "), "{all_line}");
        assert_eq!(Row::parse(&all_line), Some(all_refused));

        let older = line.replace(" restore_refused=1", "");
        assert_eq!(
            Row::parse(&older).and_then(|row| row.restores),
            Some(Restores {
                exact: 3,
                ..Restores::default()
            }),
            "a row from before the word"
        );
        let bad = line.replace("restore_refused=1", "restore_refused=x");
        assert_eq!(Row::parse(&bad), None, "{bad}");
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
        let (_, unexpected) = doctor_lines(&rows, Path::new("/x/recovery-ledger.log"));
        assert!(unexpected);
    }

    #[test]
    fn an_empty_or_all_clean_ledger_is_no_note() {
        let (_, unexpected) = doctor_lines(&[], Path::new("/x/l"));
        assert!(!unexpected);
        let rows = vec![row(1, 1, PrevRun::none()), row(2, 2, prev(EndClass::Clean))];
        let (_, unexpected) = doctor_lines(&rows, Path::new("/x/l"));
        assert!(!unexpected);
    }

    #[cfg(unix)]
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aterm-recovery-ledger-{tag}-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[cfg(unix)]
    #[test]
    fn checked_history_distinguishes_absence_and_empty_from_unreadable_or_malformed() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = scratch("checked");
        let path = ledger_path(&dir);
        assert!(
            read_rows_checked(&path).unwrap().is_empty(),
            "absent is normal"
        );
        std::fs::create_dir(&path).unwrap();
        assert!(
            read_rows_checked(&path).is_err(),
            "a directory is not history"
        );
        assert!(
            read_rows(&path).is_empty(),
            "legacy reader remains tolerant"
        );
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, "").unwrap();
        assert!(read_rows_checked(&path).unwrap().is_empty());
        let expected = row(1, 1, PrevRun::none());
        std::fs::write(&path, format!("{}\n", expected.to_line())).unwrap();
        assert_eq!(read_rows_checked(&path).unwrap(), vec![expected.clone()]);
        std::fs::write(&path, format!("{}\nunreadable row\n", expected.to_line())).unwrap();
        assert!(read_rows_checked(&path).is_err());
        assert_eq!(
            read_rows(&path),
            vec![expected.clone()],
            "legacy parsing is unchanged"
        );

        // A nonblocking open after the path's regular-file observation may
        // actually yield a FIFO. Reject the opened object even with no writer,
        // where a read would otherwise return EOF and invent empty history.
        let fifo = dir.join("replacement.fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: an owned scratch pathname, a NUL-terminated name and mode only.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_opened_text(open_for_read(&fifo).unwrap()).is_err());
        assert!(read_rows_checked(&fifo).is_err());

        // The same opened inode can shrink after a caller's earlier metadata
        // observation. Its current length, not that old bound, must decide the
        // tail offset; seeking past this malformed row would falsely look empty.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(MAX_READ_BYTES + 4096)
            .unwrap();
        let opened = open_for_read(&path).unwrap();
        assert!(opened.metadata().unwrap().len() > MAX_READ_BYTES);
        std::fs::write(&path, "unreadable row\n").unwrap();
        assert_eq!(read_opened_text(opened).unwrap(), "unreadable row\n");
        assert!(read_rows_checked(&path).is_err());

        // A later rename does not change the identity being admitted/read.
        // Read the retained valid handle, while the new pathname is malformed.
        let valid = format!("{}\n", expected.to_line());
        std::fs::write(&path, &valid).unwrap();
        let opened = open_for_read(&path).unwrap();
        std::fs::rename(&path, dir.join("retired.log")).unwrap();
        std::fs::write(&path, "unreadable row\n").unwrap();
        assert_eq!(read_opened_text(opened).unwrap(), valid);
        assert!(read_rows_checked(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
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
        let mut grown = 0;
        // Append until the file crosses TRIM_AT_BYTES and the append trims it: the
        // one append after which the file is shorter than before.
        loop {
            n += 1;
            append_row(&path, &row(u64::from(n) + 10, n, prev(EndClass::Clean))).expect("append");
            let len = std::fs::metadata(&path).expect("meta").len();
            if len < grown {
                let rows = read_rows(&path);
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
            grown = len;
            assert!(n < 10_000, "the ledger never trimmed");
        }
        assert!(std::fs::metadata(&path).expect("meta").len() <= TRIM_AT_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A ledger lock is free for a second open file description the instant its
    /// guard drops, even while a `dup`'d copy of the first description lives (as
    /// in a concurrently forked child before `exec`).
    #[cfg(unix)]
    #[test]
    fn a_released_ledger_lock_is_free_while_a_copy_of_its_descriptor_lives() {
        let dir = scratch("lock-copy");
        let path = ledger_path(&dir);
        let open = || {
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .expect("open the ledger")
        };
        let file = open();
        let held = lock_briefly(&file).expect("a free ledger locks");
        #[expect(
            clippy::disallowed_methods,
            reason = "a probe of a fresh description that is refused: it never holds the lock"
        )]
        let refused = open().try_lock().is_err();
        assert!(refused, "a held ledger lock refuses a second description");
        let childs_copy = file.try_clone().expect("dup the ledger's descriptor");
        // Released by `LOCK_UN` (the guard's drop), then closed, while the copy lives.
        drop(held);
        drop(file);
        let probe = open();
        #[expect(
            clippy::disallowed_methods,
            reason = "a probe of a fresh description, let go by unlock() below"
        )]
        let free = probe.try_lock().is_ok();
        let _ = probe.unlock();
        assert!(
            free,
            "a released ledger lock must be free while a copy of its descriptor lives"
        );
        drop(childs_copy);
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
