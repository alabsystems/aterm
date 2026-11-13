// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The explicit restart's stamp.

use std::path::{Path, PathBuf};

/// Why the act restarted its agent: the stall's remedy, or a person's own ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// A published stall stood for the process: the stall's remedy.
    Stall,
    /// Asked with no stall standing.
    Asked,
}

impl Cause {
    /// The word a stamp, a relaunch record and the verb's reply use.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Stall => "stall",
            Self::Asked => "asked",
        }
    }

    /// The cause a stamp's word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "stall" => Some(Self::Stall),
            "asked" => Some(Self::Asked),
            _ => None,
        }
    }
}

/// The account an act asks its Codex relaunch to start on. It is an ask, honoured once at that
/// relaunch's cold admission and only for a row that then proves it serves
/// ([`super::accounts_pool::cold_launch_env`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wanted {
    /// The roster row of this label.
    Row(String),
    /// The row a new launch would get on record ([`super::accounts::launch_row`]).
    Pool,
}

impl Wanted {
    /// The stamp's value for it: `{"row":"<label>"}` or `{"pool":true}`.
    fn to_value(&self) -> aterm_json::Value {
        let mut o = aterm_json::Map::new();
        match self {
            Self::Row(label) => o.insert("row".into(), aterm_json::Value::from(label.as_str())),
            Self::Pool => o.insert("pool".into(), aterm_json::Value::from(true)),
        };
        aterm_json::Value::Object(o)
    }

    /// The ask a stamp's value names; `None` for any other value, a row label outside the roster
    /// grammar ([`super::accounts::admissible_label`]) among them.
    fn from_value(v: &aterm_json::Value) -> Option<Self> {
        let o = v.as_object().filter(|o| o.len() == 1)?;
        match (o.get("row"), o.get("pool")) {
            (Some(label), None) => label
                .as_str()
                .filter(|label| super::accounts::admissible_label(label))
                .map(|label| Self::Row(label.to_string())),
            (None, Some(pool)) => (pool.as_bool() == Some(true)).then_some(Self::Pool),
            _ => None,
        }
    }
}

/// The host's input counter when it accepted an explicit restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Input {
    /// The host whose counter was read; another host cannot reuse it.
    pub host: String,
    /// The accepted counter; a later key changes it.
    pub seq: u64,
}

impl Input {
    /// The receipt's value in a stamp or a deferred relaunch record.
    #[must_use]
    pub fn to_value(&self) -> aterm_json::Value {
        let mut value = aterm_json::Map::new();
        value.insert("host".into(), aterm_json::Value::from(self.host.as_str()));
        value.insert("seq".into(), aterm_json::Value::from(self.seq));
        aterm_json::Value::Object(value)
    }

    /// A complete receipt; malformed evidence supplies no input exemption.
    #[must_use]
    pub fn from_value(value: &aterm_json::Value) -> Option<Self> {
        Some(Self {
            host: value.get("host")?.as_str()?.to_string(),
            seq: value.get("seq")?.as_u64()?,
        })
        .filter(|input| !input.host.is_empty() && !input.host.chars().any(char::is_whitespace))
    }
}

/// This host's process incarnation, so a saved counter cannot cross a restart or reboot.
/// An unread boot or process birth supplies no exemption from the person's input grace.
#[must_use]
pub fn input_host() -> Option<String> {
    let pid = i32::try_from(std::process::id()).ok()?;
    Some(format!(
        "{}:{}:{}",
        super::upgrade_drive::hand_name(),
        aterm_uds::process::boot_id()?,
        aterm_uds::process::read_birth(pid)?.0
    ))
}

/// One act's stamp: the process it restarts and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// The process: the tab's foreground group leader the act signals.
    pub pid: u32,
    /// Its kernel start, as the harness's snapshot names one ([`kernel_start`]).
    pub start: String,
    /// Why it is restarted.
    pub cause: Cause,
    /// When the act stamped it, unix seconds.
    pub at: u64,
    /// The account the act asked its relaunch to start on, if it named one.
    pub wanted: Option<Wanted>,
    /// Input accepted by this host before the act, when its counter was readable.
    pub input: Option<Input>,
}

impl Stamp {
    /// Whether it names the process `pid` started at `start`: that process's exit is the act's.
    #[must_use]
    pub fn names(&self, pid: u32, start: &str) -> bool {
        self.pid == pid && self.start == start
    }

    /// Seconds since it was written, at `now` (unix seconds).
    #[must_use]
    pub const fn age_s(&self, now: u64) -> u64 {
        now.saturating_sub(self.at)
    }

    /// Whether it is older than [`super::relaunch::STALE_S`] at `now`.
    #[must_use]
    pub const fn stale(&self, now: u64) -> bool {
        self.age_s(now) > super::relaunch::STALE_S
    }

    /// How long from `now` it still stands, a second short of going [`Self::stale`].
    #[must_use]
    pub const fn stands_for(&self, now: u64) -> std::time::Duration {
        let stale_at = self.at.saturating_add(super::relaunch::STALE_S);
        std::time::Duration::from_secs(stale_at.saturating_sub(now))
    }

    /// The file's text.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = aterm_json::Map::new();
        o.insert("pid".into(), aterm_json::Value::from(self.pid));
        o.insert("start".into(), aterm_json::Value::from(self.start.as_str()));
        o.insert("cause".into(), aterm_json::Value::from(self.cause.word()));
        o.insert("at".into(), aterm_json::Value::from(self.at));
        if let Some(wanted) = &self.wanted {
            o.insert("wanted".into(), wanted.to_value());
        }
        if let Some(input) = &self.input {
            o.insert("input".into(), input.to_value());
        }
        aterm_json::to_string(&aterm_json::Value::Object(o)).unwrap_or_default()
    }

    /// A stamp read back; `None` for anything else (a torn or foreign file). An unreadable
    /// `wanted` or `input` drops that field alone: a stamp read as `None` would make its
    /// process's signal exit nobody's, which is never relaunched, so the act's restart outlives a
    /// destination it cannot name (the session returns on the account it ran on) and a receipt it
    /// cannot read (the resume line keeps the person's whole grace).
    #[must_use]
    pub fn from_json(text: &str) -> Option<Self> {
        let v: aterm_json::Value = aterm_json::from_str(text).ok()?;
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?)
                .ok()
                .filter(|&pid| pid > 0)?,
            start: v
                .get("start")?
                .as_str()
                .filter(|s| !s.is_empty())?
                .to_string(),
            cause: Cause::parse(v.get("cause")?.as_str()?)?,
            at: v.get("at")?.as_u64()?,
            wanted: v.get("wanted").and_then(Wanted::from_value),
            input: v.get("input").and_then(Input::from_value),
        })
    }
}

/// Where tab tab's stamp lives under the harness state directory state.
#[must_use]
pub fn path(state: &Path, tab: &str) -> Option<PathBuf> {
    let plain = !tab.is_empty() && tab.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plain.then(|| {
        state
            .join("upgrade")
            .join(format!("{FILE_PREFIX}{tab}.json"))
    })
}

/// Write `stamp` as tab `tab`'s by temp-file rename, replacing any earlier one.
/// # Errors
/// The tab names no file, or the directory or the file cannot be written.
pub fn write(state: &Path, tab: &str, stamp: &Stamp) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = path(state, tab).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "a tab key names a file")
    })?;
    let _ = super::upgrade_drive::ensure_upgrade_tree(state, None)?;
    let tmp = unique(&path, "tmp");
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let written = open
        .open(&tmp)
        .and_then(|mut f| f.write_all(stamp.to_json().as_bytes()))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Tab `tab`'s stamp, if one stands.
#[must_use]
pub fn read(state: &Path, tab: &str) -> Option<Stamp> {
    let _ = super::upgrade_drive::check_upgrade_tree(state, None).ok()??;
    Stamp::from_json(&super::accounts::read_bounded(&path(state, tab)?, 64 * 1024).ok()??)
}

/// The stamp standing for process `pid` started at `start` in tab `tab` at `now`.
#[must_use]
pub fn standing(state: &Path, tab: &str, pid: u32, start: &str, now: u64) -> Option<Stamp> {
    read(state, tab).filter(|s| !s.stale(now) && s.names(pid, start))
}

/// The account the stamp standing for process `pid` started at `start` in tab `tab` asks its
/// relaunch to start on at `now`: `None` for a plain restart, another process's stamp, a stale one
/// or none. Asking removes nothing.
#[must_use]
pub fn wanted_for(state: &Path, tab: &str, pid: u32, start: &str, now: u64) -> Option<Wanted> {
    standing(state, tab, pid, start, now)?.wanted
}

/// Take tab `tab`'s stamp when `ours` says it is the one meant.
pub fn take_if(state: &Path, tab: &str, ours: impl FnOnce(&Stamp) -> bool) -> Option<Stamp> {
    let _ = super::upgrade_drive::check_upgrade_tree(state, None).ok()??;
    let path = path(state, tab)?;
    let judged = read(state, tab).filter(ours)?;
    let taken = unique(&path, "taken");
    std::fs::rename(&path, &taken).ok()?;
    let stamp = super::accounts::read_bounded(&taken, 64 * 1024)
        .ok()
        .flatten()
        .and_then(|text| Stamp::from_json(&text));
    let mine = stamp.as_ref() == Some(&judged);
    if !mine {
        let _ = std::fs::hard_link(&taken, &path);
    }
    let _ = std::fs::remove_file(&taken);
    stamp.filter(|_| mine)
}

/// The reader's lookup for the exit of process `pid` started at `start` in tab `tab`.
#[must_use]
pub fn lookup(state: &Path, tab: &str, pid: u32, start: &str, now: u64) -> Option<Cause> {
    let stamp = read(state, tab)?;
    if !stamp.stale(now) && stamp.names(pid, start) {
        return Some(stamp.cause);
    }
    if stamp.stale(now) || !alive_as(stamp.pid, &stamp.start) {
        let _ = take_if(state, tab, |s| *s == stamp);
    }
    None
}

/// Prune the stamp file `name` (a file name in `<state>/upgrade`) at `now`.
pub fn prune(state: &Path, name: &str, now: u64) -> bool {
    let Some(tab) = name
        .strip_prefix(FILE_PREFIX)
        .and_then(|rest| rest.strip_suffix(".json"))
        .filter(|tab| path(state, tab).is_some())
    else {
        return false;
    };
    if read(state, tab).is_some_and(|stamp| stamp.stale(now)) {
        let _ = take_if(state, tab, |stamp| stamp.stale(now));
    }
    true
}

/// A stamp's file name, before its tab key: `asked-<tab>.json`.
const FILE_PREFIX: &str = "asked-";

/// The kernel's start time of `pid` in the spelling the harness's snapshot names an agent by
/// (`relaunch::Snapshot::start`, `upgrade_drive::kernel_start`): on Unix `ps -o lstart=`
/// in UTC and the C locale, whitespace squashed; on Windows its creation `FILETIME` as a decimal
/// count, the integer Claude Code writes as its `procStart`. So a stamp and a snapshot of the same
/// process compare equal.
#[must_use]
pub fn kernel_start(pid: u32) -> Option<String> {
    #[cfg(target_vendor = "apple")]
    {
        let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
        Some(super::footer::lstart_utc(birth.seconds))
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        super::upgrade_drive::kernel_start(pid)
    }
}

/// Whether the process `pid` started at `start` still runs.
#[must_use]
pub fn alive_as(pid: u32, start: &str) -> bool {
    super::upgrade_drive::alive(pid) && kernel_start(pid).as_deref() == Some(start)
}

/// A name beside `path` no other writer takes: this process, a counter and
/// the clock.
fn unique(path: &Path, what: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = aterm_time::since_epoch().unwrap_or_default().as_nanos();
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.{n}.{nanos:x}.{what}", std::process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
#[path = "restart_stamp_tests.rs"]
mod tests;
