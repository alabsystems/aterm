// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The explicit restart's stamp: `aterm ctl @<sid> restart` writes it before it signals the agent,
//! so every reader of that agent's exit recognises the exit by lookup, never by inference (model:
//! `aterm-spec`'s `HarnessRestartStamp`). Other in-place restarts keep their own record
//! ([`super::relaunch::restarted`]).
//!
//! One file per tab, `<harness state>/upgrade/asked-<tab>.json`
//! (`{"pid":…,"start":"…","cause":"stall"|"asked","at":…}`), written by temp-file rename. It names
//! the process by pid and [`kernel_start`], so only that process's exit matches. It has no `phase`,
//! so no reader of the upgrade records takes it for one.
//!
//! A reader of an exit ([`lookup`]) that finds a stamp naming the process makes the exit the act's:
//! relaunched on its conversation, a keystroke before it not read as the person's `/exit`. The
//! relaunch that files the exit's record consumes it under the sweep lock ([`take_if`]). A stamp
//! whose process is gone (and is not the exit being read), or older than
//! [`super::relaunch::STALE_S`], is removed by whoever finds it.

use std::path::{Path, PathBuf};

/// Why the act restarted its agent: the stall's remedy (`restart
/// if-stalled`, a published stall standing for the process), or a person's
/// (or a driver's) own ask.
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
    pub fn word(self) -> &'static str {
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
    /// Whether it names the process `pid` started at `start` (a snapshot's
    /// pair): that process's exit is the act's.
    #[must_use]
    pub fn names(&self, pid: u32, start: &str) -> bool {
        self.pid == pid && self.start == start
    }

    /// Seconds since it was written, at `now` (unix seconds).
    #[must_use]
    pub fn age_s(&self, now: u64) -> u64 {
        now.saturating_sub(self.at)
    }

    /// Whether it is older than [`super::relaunch::STALE_S`] at `now`: no
    /// reader takes it for an act's any more, and whoever finds it removes it.
    #[must_use]
    pub fn stale(&self, now: u64) -> bool {
        self.age_s(now) > super::relaunch::STALE_S
    }

    /// How long from `now` it still stands, a second short of going [`Self::stale`]: what an act's
    /// watch may wait for its program (`aterm-gui`'s `restart_act::kill`).
    #[must_use]
    pub fn stands_for(&self, now: u64) -> std::time::Duration {
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

/// Where tab `tab`'s stamp lives under the harness state directory `state`
/// (`<state>/upgrade/asked-<tab>.json`); `None` for a tab key that is not a
/// plain word (letters, digits, `-`), which names no file.
#[must_use]
pub fn path(state: &Path, tab: &str) -> Option<PathBuf> {
    let plain = !tab.is_empty() && tab.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    plain.then(|| {
        state
            .join("upgrade")
            .join(format!("{FILE_PREFIX}{tab}.json"))
    })
}

/// Write `stamp` as tab `tab`'s by temp-file rename, replacing any earlier one. No lock: the rename
/// is the one write.
///
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

/// The stamp standing for process `pid` started at `start` in tab `tab` at `now`: the tab's stamp
/// when it names that process and is not [`Stamp::stale`]. An act looks for it before it writes:
/// one found means an act on that process is in flight, and the new one joins it, never rewriting
/// the stamp before its own signal nor taking it back (the first act's exit has only that stamp).
/// Once the joining act's `SIGTERM` is sent it rewrites the stamp with its own time, cause kept
/// (`aterm-gui`'s `restart_act::renew`), so its escalation has the stamp's whole life for
/// `SIGKILL`.
#[must_use]
pub fn standing(state: &Path, tab: &str, pid: u32, start: &str, now: u64) -> Option<Stamp> {
    read(state, tab).filter(|s| !s.stale(now) && s.names(pid, start))
}

/// Take tab `tab`'s stamp when `ours` says it is the one meant. It is judged in place, so a stamp
/// that is not the taker's is never moved and a watch reading the tab's stamp
/// (`aterm-gui`'s `restart_act::Watch::named`) never finds it missing and lets its program go. The
/// one judged ours is moved aside (one rename), read again and removed; if a newer stamp replaced
/// it between the look and the rename, that one is put back by hard link, which never replaces a
/// stamp written meanwhile.
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

/// The reader's lookup for the exit of process `pid` started at `start` in tab `tab`, at `now`
/// (Unix seconds): the cause of a stamp naming it (the exit is the act's), else `None`. A stale
/// stamp, or one naming a process that is gone (not this exit's), is removed as it is found
/// ([`take_if`]: never a newer one).
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

/// Prune the stamp file `name` (a file name in `<state>/upgrade`) at `now`: removed when it is a
/// stamp gone [`Stamp::stale`], whatever its process ([`take_if`]: never a newer one). Returns
/// whether `name` is a stamp's file at all. The upgrade lane runs it under its sweep lock over the
/// whole directory whenever an upgrade step runs (`upgrade_drive::prune_states`), since [`lookup`]
/// sweeps only its own tab and a stamp whose tab closed would otherwise stay for good. A stale
/// stamp's lifetime decides only litter: no reader or act's watch acts on it. A stamp still
/// standing is kept even naming a gone process: its exit may not have been read yet.
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

/// The kernel's start time of `pid` in the spelling the harness's snapshot names an agent by
/// (`relaunch::Snapshot::start`: `ps -o lstart=` in UTC and the C locale, whitespace squashed), so
/// a stamp and a snapshot of the same process compare equal. `None` when it cannot be read (on
/// macOS also a zombie or another uid's process).
///
/// On macOS it is read by syscall ([`aterm_uds::process::read_process_birth`]) and spelled by
/// [`super::footer::lstart_utc`]: no `ps` per read, and a `ps` that cannot spawn under memory
/// pressure (just when an agent freezes) is never read as the leader having moved. Elsewhere `ps`.
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

/// Whether the process `pid` started at `start` still runs: the same pid,
/// alive, with the same kernel start.
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
