// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Root-owned machine agent state (`floors.toml`, run lock, and latest run records).

use super::Layout;
use crate::model::Read;
use crate::port::System;
use crate::protocol::Floor;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FloorsFile {
    #[serde(default)]
    floor: Vec<Floor>,
}

/// The floors.
#[must_use]
pub fn floors(sys: &dyn System, layout: &Layout) -> Read<Vec<Floor>> {
    match sys.read_file(&layout.floors()) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Read::Absent,
        Err(e) => Read::Unusable(format!("{}: {e}", layout.floors().display())),
        Ok(bytes) => {
            let parsed = std::str::from_utf8(&bytes)
                .map_err(|_| "not UTF-8".to_owned())
                .and_then(|t| aterm_toml::from_str::<FloorsFile>(t).map_err(|e| e.to_string()));
            match parsed {
                Ok(f) => Read::Value(f.floor),
                Err(why) => Read::Unusable(format!("{}: {why}", layout.floors().display())),
            }
        }
    }
}

/// `principal`'s floor in `floors` (0 when it has none).
#[must_use]
pub fn floor_of(floors: &[Floor], principal: &str) -> u64 {
    floors
        .iter()
        .filter(|f| f.principal == principal)
        .map(|f| f.serial)
        .max()
        .unwrap_or(0)
}

/// The floors as a list, or why they cannot be trusted.
/// # Errors
/// One line when the file exists and cannot be read.
pub fn floors_or_refuse(sys: &dyn System, layout: &Layout) -> Result<Vec<Floor>, String> {
    match floors(sys, layout) {
        Read::Value(f) => Ok(f),
        Read::Absent => Ok(Vec::new()),
        Read::Unusable(why) => Err(format!(
            "the serial floors cannot be read ({why}); every envelope is refused until \
             state/floors.toml is repaired"
        )),
    }
}

/// Raise `principal`'s floor to `serial` (never lowers it).
/// # Errors
/// One line; the caller must not act on the envelope then.
pub fn raise_floor(
    sys: &dyn System,
    layout: &Layout,
    principal: &str,
    serial: u64,
) -> Result<(), String> {
    let mut list = floors_or_refuse(sys, layout)?;
    match list.iter_mut().find(|f| f.principal == principal) {
        Some(f) => f.serial = f.serial.max(serial),
        None => list.push(Floor {
            principal: principal.to_owned(),
            serial,
        }),
    }
    let text =
        aterm_toml::to_string(&FloorsFile { floor: list }).map_err(|e| format!("floors: {e}"))?;
    sys.write_file(&layout.floors(), text.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.floors().display()))
}

/// Raise each of `principals` to at least the highest floor any principal has.
/// # Errors
/// One line when `floors.toml` cannot be read or written; the caller must not go on as if
/// they had been raised.
pub fn inherit_floors(
    sys: &dyn System,
    layout: &Layout,
    principals: &[String],
) -> Result<(), String> {
    let list = floors_or_refuse(sys, layout)?;
    let top = list.iter().map(|f| f.serial).max().unwrap_or(0);
    if top == 0 {
        return Ok(());
    }
    for p in principals {
        if floor_of(&list, p) < top {
            raise_floor(sys, layout, p, top)?;
        }
    }
    Ok(())
}

/// Who holds `state/run.lock`: `pid=<n> run=<id> [start=<when>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The holding process.
    pub pid: u32,
    /// Its run.
    pub run: String,
    /// When the holding process started, as [`started`] read it (`None` when it could not be read).
    pub start: Option<String>,
}

impl Holder {
    fn text(&self) -> String {
        match &self.start {
            Some(start) => format!("pid={} run={} start={start}\n", self.pid, self.run),
            None => format!("pid={} run={}\n", self.pid, self.run),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let mut pid = None;
        let mut run = None;
        let mut start = None;
        for word in text.split_whitespace() {
            if let Some(p) = word.strip_prefix("pid=") {
                pid = crate::parse_uint(p);
            } else if let Some(r) = word.strip_prefix("run=") {
                run = Some(r.to_owned());
            } else if let Some(s) = word.strip_prefix("start=") {
                start = Some(s.to_owned()).filter(|s| !s.is_empty());
            }
        }
        Some(Self {
            pid: pid?,
            run: run.filter(|r| super::is_run_id(r))?,
            start,
        })
    }
}

/// When `pid` started, as one word.
#[must_use]
pub fn started(sys: &dyn System, pid: u32) -> Option<String> {
    sys.process_started(pid).map(lstart_word)
}

/// `unix` seconds as `ps -o lstart=` prints them in UTC under the C locale (`%a %b %e %T
/// %Y`, the day unpadded once the spaces are `_`): `Fri_Oct_16_22:36:10_2026`.
#[must_use]
pub fn lstart_word(unix: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (days, secs) = (unix / 86_400, unix % 86_400);
    let (year, month, day) =
        aterm_types::rfc3339::civil_from_days(i64::try_from(days).unwrap_or(0));
    let weekday = usize::try_from(days % 7)
        .ok()
        .and_then(|i| DAYS.get(i))
        .unwrap_or(&"?");
    let month = usize::try_from(month - 1)
        .ok()
        .and_then(|i| MONTHS.get(i))
        .unwrap_or(&"?");
    format!(
        "{weekday}_{month}_{day}_{:02}:{:02}:{:02}_{year}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Whether `h`'s process is still the one that took the lock.
#[must_use]
pub fn holder_live(sys: &dyn System, h: &Holder) -> bool {
    if !sys.pid_alive(h.pid) {
        return false;
    }
    match (&h.start, started(sys, h.pid)) {
        (Some(then), Some(now)) => *then == now,
        _ => true,
    }
}

/// The lock's holder, if the file names one.
#[must_use]
pub fn holder(sys: &dyn System, layout: &Layout) -> Option<Holder> {
    let bytes = sys.read_file(&layout.lock()).ok()?;
    Holder::parse(&String::from_utf8_lossy(&bytes))
}

/// The run in progress: the lock's run when its holder is live.
#[must_use]
pub fn busy(sys: &dyn System, layout: &Layout) -> Option<String> {
    holder(sys, layout)
        .filter(|h| holder_live(sys, h))
        .map(|h| h.run)
}

/// Whether `run`'s job is still going: the run lock names it and its holder is live.
#[must_use]
pub fn running(sys: &dyn System, layout: &Layout, run: &str) -> bool {
    holder(sys, layout).is_some_and(|h| h.run == run && holder_live(sys, &h))
}

/// How long a take, hand-off or release of the run lock waits for `state/run.lock.guard`.
const GUARD_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

fn guard(sys: &dyn System, layout: &Layout) -> Result<Box<dyn std::any::Any>, String> {
    guard_started(sys, layout, std::time::Instant::now())
}

fn guard_started(
    sys: &dyn System,
    layout: &Layout,
    started: std::time::Instant,
) -> Result<Box<dyn std::any::Any>, String> {
    let remaining = GUARD_LIMIT
        .checked_sub(started.elapsed())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| {
            format!(
                "{}: required run-lock guard did not finish",
                layout.lock_guard().display()
            )
        })?;
    sys.exclusive(&layout.lock_guard(), remaining)
        .map_err(|e| format!("{}: {e}", layout.lock_guard().display()))
}

/// What [`take_lock`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lock {
    /// This process holds it now.
    Taken,
    /// Another live run holds it.
    Busy {
        /// Its run id.
        run: String,
    },
}

/// Take `state/run.lock` for `run`, held by `pid`: an exclusive create.
/// # Errors
/// One line when the guard or the lock file cannot be had.
pub fn take_lock(sys: &dyn System, layout: &Layout, run: &str, pid: u32) -> Result<Lock, String> {
    let mine = Holder {
        pid,
        run: run.to_owned(),
        start: started(sys, pid),
    };
    let _guard = guard(sys, layout)?;
    match sys.create_new(&layout.lock(), mine.text().as_bytes(), 0o644) {
        Ok(()) => return Ok(Lock::Taken),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", layout.lock().display())),
    }
    if let Some(h) = holder(sys, layout).filter(|h| holder_live(sys, h)) {
        return Ok(Lock::Busy { run: h.run });
    }
    sys.remove(&layout.lock())
        .map_err(|e| format!("{}: {e}", layout.lock().display()))?;
    sys.create_new(&layout.lock(), mine.text().as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.lock().display()))?;
    Ok(Lock::Taken)
}

/// Hand only the exact observed holder to a known, still-uncommitted child.
/// # Errors
/// The holder changed, the child lacks a start identity, or the atomic write failed.
pub(super) fn hand_lock_from(
    sys: &dyn System,
    layout: &Layout,
    from: &Holder,
    to: &Holder,
) -> Result<(), String> {
    if from.run != to.run || to.pid == 0 || to.start.is_none() {
        return Err("a gated job needs the same run and a known child start identity".into());
    }
    let _guard = guard(sys, layout)?;
    if holder(sys, layout).as_ref() != Some(from) {
        return Err("the exact run-lock holder changed before the job handoff".into());
    }
    sys.write_file(&layout.lock(), to.text().as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.lock().display()))
}

/// Release only this exact holder, never a later owner with the same run name.
/// # Errors
/// The lock guard or removal failed; no unowned replacement is removed.
pub(super) fn release_holder(
    sys: &dyn System,
    layout: &Layout,
    expected: &Holder,
) -> Result<(), String> {
    let _guard = guard(sys, layout)?;
    if holder(sys, layout).as_ref() == Some(expected) {
        sys.remove(&layout.lock())
            .map_err(|e| format!("{}: {e}", layout.lock().display()))?;
    }
    Ok(())
}

/// Join an internal gated job to its current PID and known recorded start identity.
/// # Errors
/// Missing/changed holder, another PID, or an unmeasured/mismatched start identity.
pub(super) fn job_holder(
    sys: &dyn System,
    layout: &Layout,
    run: &str,
    pid: u32,
) -> Result<Holder, String> {
    let start = started(sys, pid)
        .ok_or_else(|| "the run job's start identity could not be read".to_owned())?;
    let expected = Holder {
        pid,
        run: run.to_owned(),
        start: Some(start),
    };
    if holder(sys, layout).as_ref() != Some(&expected) {
        return Err("the run lock does not name this exact job process".into());
    }
    Ok(expected)
}

/// The newest finished run.
#[must_use]
pub fn last_run(sys: &dyn System, layout: &Layout) -> Option<String> {
    let bytes = sys.read_file(&layout.last_run()).ok()?;
    let run = String::from_utf8_lossy(&bytes).trim().to_owned();
    super::is_run_id(&run).then_some(run)
}

/// Record `run` as the newest finished run.
pub fn set_last_run(sys: &dyn System, layout: &Layout, run: &str) {
    let _ = sys.write_file(&layout.last_run(), format!("{run}\n").as_bytes(), 0o644);
}

/// The newest signed request refused `busy` (by the id its run would have had).
#[must_use]
pub fn last_busy(sys: &dyn System, layout: &Layout) -> Option<String> {
    let bytes = sys.read_file(&layout.last_busy()).ok()?;
    let run = String::from_utf8_lossy(&bytes).trim().to_owned();
    super::is_run_id(&run).then_some(run)
}

/// Record `run` as the newest signed request refused `busy`.
pub fn set_last_busy(sys: &dyn System, layout: &Layout, run: &str) {
    let _ = sys.write_file(&layout.last_busy(), format!("{run}\n").as_bytes(), 0o644);
}

/// `managed.toml`: what enrollment recorded, and what the agent has managed since.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Managed {
    /// [`crate::SCHEMA`].
    pub schema: u32,
    /// Admin account the fleet key is authorized for.
    pub admin: String,
    /// SHA-256 of the agent archive enrollment installed.
    pub payload_sha256: String,
    /// The agent's version.
    pub agent_version: String,
    /// The agent executable, relative to the agent root.
    pub executable: String,
    /// Unix seconds of the install.
    pub installed: u64,
    /// Unix seconds of the last upgrade.
    #[serde(default)]
    pub upgraded: u64,
    /// Every item id a run has managed on this host.
    #[serde(default)]
    pub items: Vec<String>,
}

/// Read `managed.toml`.
/// # Errors
/// One line when it is missing or does not parse (the host was not enrolled by this agent).
pub fn managed(sys: &dyn System, layout: &Layout) -> Result<Managed, String> {
    let bytes = sys
        .read_file(&layout.managed())
        .map_err(|e| format!("{}: {e}", layout.managed().display()))?;
    let text = String::from_utf8(bytes).map_err(|_| "managed.toml is not UTF-8".to_owned())?;
    aterm_toml::from_str(&text).map_err(|e| format!("managed.toml: {e}"))
}

/// Write `managed.toml`.
/// # Errors
/// One line.
pub fn save_managed(sys: &dyn System, layout: &Layout, m: &Managed) -> Result<(), String> {
    let text = aterm_toml::to_string(m).map_err(|e| format!("managed.toml: {e}"))?;
    sys.write_file(&layout.managed(), text.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.managed().display()))
}

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
