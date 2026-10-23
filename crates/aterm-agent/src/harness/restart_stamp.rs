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
        aterm_json::to_string(&aterm_json::Value::Object(o)).unwrap_or_default()
    }

    /// A stamp read back; `None` for anything else (a torn or foreign file).
    #[must_use]
    pub fn from_json(text: &str) -> Option<Self> {
        let v: aterm_json::Value = aterm_json::from_str(text).ok()?;
        Some(Self {
            pid: u32::try_from(v.get("pid")?.as_u64()?).ok()?,
            start: v.get("start")?.as_str()?.to_string(),
            cause: Cause::parse(v.get("cause")?.as_str()?)?,
            at: v.get("at")?.as_u64()?,
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
    let path = path(state, tab).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "a tab key names a file")
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = unique(&path, "tmp");
    let written = std::fs::write(&tmp, stamp.to_json()).and_then(|()| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Tab `tab`'s stamp, if one stands.
#[must_use]
pub fn read(state: &Path, tab: &str) -> Option<Stamp> {
    Stamp::from_json(&std::fs::read_to_string(path(state, tab)?).ok()?)
}

/// The stamp standing for process `pid` started at `start` in tab `tab` at `now`.
#[must_use]
pub fn standing(state: &Path, tab: &str, pid: u32, start: &str, now: u64) -> Option<Stamp> {
    read(state, tab).filter(|s| !s.stale(now) && s.names(pid, start))
}

/// Take tab `tab`'s stamp when `ours` says it is the one meant.
pub fn take_if(state: &Path, tab: &str, ours: impl FnOnce(&Stamp) -> bool) -> Option<Stamp> {
    let path = path(state, tab)?;
    let judged = read(state, tab).filter(ours)?;
    let taken = unique(&path, "taken");
    std::fs::rename(&path, &taken).ok()?;
    let stamp = std::fs::read_to_string(&taken)
        .ok()
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

/// The kernel's start time of `pid` in the spelling the harness's snapshot names an agent.
#[must_use]
pub fn kernel_start(pid: u32) -> Option<String> {
    #[cfg(any(target_vendor = "apple", windows))]
    {
        let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).ok()?)?;
        Some(super::footer::lstart_utc(birth.seconds))
    }
    #[cfg(not(any(target_vendor = "apple", windows)))]
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
