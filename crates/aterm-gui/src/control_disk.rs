// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Disk-space `await sys` wait runner ([`wait_of`], [`decide`], [`ARM_FRESH`], [`FRESH`]).
//! Bounds demand against [`DEMAND_CAP_PCT`] and watch stalls against [`STALL`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::Level;
use aterm_sysprobe::volumes::{Reading, Unread, VolumeKey};
use aterm_vitals::sys::predicate::{DiskWord, VolumeTerm, VolumeWait};

use crate::disk_board::{Board, PassSummary, Shelf, VolumeRow, Watch};

/// A `vol=` this wait cannot read: not an absolute path once decoded.
pub(crate) const USAGE: &str = aterm_vitals::sys::predicate::USAGE;

/// A beat this old, while the watch runs, is a stalled watch.
pub(crate) const STALL: Duration =
    Duration::from_secs(crate::harness_host::DISK_TICK.as_secs().saturating_mul(2));

/// A parked wait reads its volume itself once the reading is this old.
pub(crate) const FRESH: Duration = Duration::from_secs(10);

/// At arm time a reading at most this old is used.
pub(crate) const ARM_FRESH: Duration = Duration::from_secs(1);

/// Window in which a spent pass answers a new wait at arm time (`why=spent`).
pub(crate) const SPENT_RECENT: Duration = Duration::from_secs(60);

/// Max demand percent of the volume beyond current free space (`why=too-large`).
pub(crate) const DEMAND_CAP_PCT: u64 = 25;

pub(crate) use aterm_vitals::sys::predicate::Need;

/// One term of the wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Term {
    /// Free space at least (`free>=`) or above (`free>`) the need.
    Free { strict: bool, need: Need },
    /// The volume at one of these levels.
    Disk(Vec<Level>),
}

/// A volume wait as this host runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SysWait {
    /// One or two terms, all of which must hold.
    pub(crate) terms: Vec<Term>,
    /// The volume named (`vol=`), else the session's own.
    pub(crate) vol: Option<PathBuf>,
    /// The caller's patience.
    pub(crate) timeout_ms: u64,
}

/// The disk watch's level a `disk=` word names.
const fn level(word: DiskWord) -> Level {
    match word {
        DiskWord::Ok => Level::Ok,
        DiskWord::Low => Level::Low,
        DiskWord::Short => Level::Short,
        DiskWord::Emergency => Level::Emergency,
    }
}

/// Build a [`SysWait`] from a
/// [`Parsed::Volume`](aterm_vitals::sys::predicate::Parsed::Volume), its `vol=`
/// decoded; a `vol=` that is not absolute is refused with the usage line.
///
/// # Errors
///
/// The reply that refuses it: [`USAGE`].
pub(crate) fn wait_of(parsed: VolumeWait) -> Result<SysWait, &'static str> {
    let vol = match parsed.vol {
        Some(v) => {
            let path = PathBuf::from(aterm_control::wire::pct_decode(&v));
            if !path.is_absolute() {
                return Err(USAGE);
            }
            Some(path)
        }
        None => None,
    };
    let terms = parsed
        .terms
        .into_iter()
        .map(|t| match t {
            VolumeTerm::Free { strict, need } => Term::Free { strict, need },
            VolumeTerm::Disk(words) => Term::Disk(words.into_iter().map(level).collect()),
        })
        .collect();
    Ok(SysWait {
        terms,
        vol,
        timeout_ms: parsed.timeout_ms,
    })
}

/// What one look of a parked wait reads ([`decide`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Facts {
    /// Every term holds on the reading.
    pub(crate) met: bool,
    /// This wait holds a demand.
    pub(crate) demand: bool,
    /// The watch is running (not suspended, not before its first look).
    pub(crate) running: bool,
    /// Its beat is [`STALL`] old.
    pub(crate) beat_old: bool,
    /// A pass adopted this wait's need, ran every tier it may and left free space below it.
    pub(crate) spent: bool,
}

/// The wait's decision on one look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Every term holds: `OK sys`.
    Met,
    /// What is left is not aterm's to free: `ERR unmet`.
    Unmet,
    /// The watch made no progress for [`STALL`]: `ERR unavailable why=watch-stalled`.
    Stalled,
    /// Park on.
    Park,
}

/// THE DECISION, pure: met on a reading that holds.
pub(crate) const fn decide(f: &Facts) -> Decision {
    if f.met {
        Decision::Met
    } else if f.demand && f.spent {
        Decision::Unmet
    } else if f.demand && f.running && f.beat_old {
        Decision::Stalled
    } else {
        Decision::Park
    }
}

/// Whether every term of `wait` holds on `row` for resolved `needs`.
fn holds(wait: &SysWait, row: &VolumeRow) -> bool {
    wait.terms.iter().all(|t| match t {
        Term::Free { strict, need } => {
            let need = need_bytes(*need, row);
            if *strict {
                row.reading.free > need
            } else {
                row.reading.free >= need
            }
        }
        Term::Disk(levels) => levels.contains(&row.level),
    })
}

/// A need in bytes on `row`'s volume ([`room_unknown`] answers when room is unknown).
fn need_bytes(need: Need, row: &VolumeRow) -> u64 {
    match need {
        Need::Bytes(n) => n,
        Need::Room => row.room.unwrap_or(u64::MAX),
    }
}

/// Whether `wait` names `room` on a volume whose room is not known.
fn room_unknown(wait: &SysWait, row: &VolumeRow) -> bool {
    row.room.is_none()
        && wait.terms.iter().any(|t| {
            matches!(
                t,
                Term::Free {
                    need: Need::Room,
                    ..
                }
            )
        })
}

/// The reply to a wait on a volume whose room is not known.
const ROOM_UNKNOWN: &str = "ERR unavailable free why=room-unknown\n";

/// The reply to a demand where no disk watch runs (`[harness]` off): no pass would ever adopt it.
const NO_WATCH: &str = "ERR unavailable free why=no-watch\n";

/// The largest free need of `wait` on `row`, if it has a `free` term.
fn largest_need(wait: &SysWait, row: &VolumeRow) -> Option<u64> {
    wait.terms
        .iter()
        .filter_map(|t| match t {
            Term::Free { strict, need } => {
                Some(need_bytes(*need, row).saturating_add(u64::from(*strict)))
            }
            Term::Disk(_) => None,
        })
        .max()
}

/// External readers injected into the wait ([`Env::live`] in production).
pub(crate) struct Env<'a> {
    /// A fresh reading of the volume holding a path (one `statfs`).
    pub(crate) read: &'a dyn Fn(&Path) -> Result<Reading, Unread>,
    /// Ask the watch to look now for a demand of so many bytes.
    pub(crate) poke: &'a dyn Fn(u64),
    /// The removal settings.
    pub(crate) settings: &'a dyn Fn() -> (bool, bool, bool),
    /// The lines in force on a volume the watch has not measured.
    pub(crate) lines: &'a dyn Fn(&Reading) -> aterm_agent::harness::disk::Thresholds,
    /// The caller hung up.
    pub(crate) hung_up: &'a dyn Fn() -> bool,
    /// The target session exited.
    pub(crate) exited: &'a dyn Fn() -> bool,
}

/// Who waits: the session and whether the caller may register a demand.
#[derive(Debug, Clone)]
pub(crate) struct Who {
    pub(crate) session: u64,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) owner_class: bool,
}

/// `hh:mm:ss` (UTC) of unix second `at`.
fn clock(at: i64) -> String {
    let s = at.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// `OK sys …`.
fn met_reply(row: &VolumeRow, freed: u64) -> String {
    format!(
        "OK sys free_bytes={} disk={} vol={} freed_bytes={freed} held_ms=0 as_of={}\n",
        row.reading.free,
        row.level.as_str(),
        aterm_control::wire::pct_encode(&row.reading.mount.to_string_lossy()),
        clock(row.as_of_unix)
    )
}

/// `ERR unmet …`.
fn unmet_reply(
    row: &VolumeRow,
    need: u64,
    freed: u64,
    why: &str,
    pass: Option<&PassSummary>,
) -> String {
    format!(
        "ERR unmet free_bytes={} need_bytes={need} vol={} freed_bytes={freed} why={why} hogs={} \
         largest_bytes={}{}\n",
        row.reading.free,
        aterm_control::wire::pct_encode(&row.reading.mount.to_string_lossy()),
        pass.map_or(0, |p| p.hogs),
        pass.map_or(0, |p| p.largest),
        if why == "held-back" {
            " key=idle_build_dirs"
        } else {
            ""
        }
    )
}

/// Build a row for `reading`, preserving `old`'s lines and room when present.
fn row_of(
    reading: Reading,
    old: Option<&VolumeRow>,
    now_unix: i64,
    lines: &dyn Fn(&Reading) -> aterm_agent::harness::disk::Thresholds,
) -> VolumeRow {
    let old = old.filter(|o| o.reading.key == reading.key);
    let thresholds = old.map_or_else(|| lines(&reading), |o| o.thresholds);
    VolumeRow {
        level: thresholds.level(Some(reading.free), reading.files_free),
        thresholds,
        room: old.and_then(|o| o.room),
        reclaimed_here: old.is_some_and(|o| o.reclaimed_here),
        reading,
        as_of: Instant::now(),
        as_of_unix: now_unix,
    }
}

/// Whether 8.3 `cwd` must be spelled long on `b` ([`Board::row_of`], [`Board::spelled`]).
fn spell_owed(b: &Board, session: u64, cwd: &Path) -> bool {
    b.row_of(session, cwd.to_str()).is_none()
        && b.spelled
            .get(&session)
            .is_none_or(|(short, _)| short != cwd)
}

/// The unix clock now.
fn unix_now() -> i64 {
    aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX))
}

/// THE WAIT over `shelf`: arm, latch or register a demand, park until it answers.
pub(crate) fn run(shelf: &Shelf, wait: &SysWait, who: &Who, env: &Env<'_>) -> String {
    let armed = Instant::now();
    let deadline = armed + Duration::from_millis(wait.timeout_ms);
    let (key, path) = {
        let b = shelf.board_lock();
        let row = match &wait.vol {
            Some(v) => b.row_at(v),
            None => b.row_of(who.session, who.cwd.as_deref().and_then(Path::to_str)),
        };
        let path = wait
            .vol
            .clone()
            .or_else(|| row.map(|r| r.reading.mount.clone()))
            .or_else(|| who.cwd.clone());
        (row.map(|r| r.reading.key.clone()), path)
    };
    let Some(path) = path else {
        return "ERR unavailable free why=unreadable\n".to_owned();
    };
    let fresh = |key: Option<&VolumeKey>| -> Result<VolumeRow, String> {
        let old = key.and_then(|k| shelf.board_lock().row_keyed(k).cloned());
        if let Some(o) = old.as_ref().filter(|o| o.as_of.elapsed() <= ARM_FRESH) {
            return Ok(o.clone());
        }
        let reading =
            (env.read)(&path).map_err(|u| format!("ERR unavailable free why={}\n", u.word()))?;
        let old = match old {
            Some(o) if o.reading.key == reading.key => Some(o),
            _ => shelf.board_lock().row_keyed(&reading.key).cloned(),
        };
        let row = row_of(reading, old.as_ref(), unix_now(), env.lines);
        let published = row.clone();
        shelf.publish(|b| b.refresh(published));
        Ok(row)
    };
    let row = match fresh(key.as_ref()) {
        Ok(row) => row,
        Err(reply) => return reply,
    };
    let key = row.reading.key.clone();
    if room_unknown(wait, &row) {
        return ROOM_UNKNOWN.to_owned();
    }
    let need = largest_need(wait, &row);
    if let Some(n) = need {
        let cap = row.reading.total / 100 * DEMAND_CAP_PCT;
        if n.saturating_sub(row.reading.free) > cap {
            return format!(
                "ERR unavailable free why=too-large cap_bytes={}\n",
                row.reading.free.saturating_add(cap)
            );
        }
    }
    if holds(wait, &row) {
        return met_reply(&row, 0);
    }
    let seen = shelf.board_lock().last_pass.as_ref().map(|p| p.ended_at);
    let mut demand: Option<u64> = None;
    if let (Some(need), true) = (need, who.owner_class)
        && row.reading.free < need
    {
        // No disk watch runs in this process (`[harness]` off): no pass would
        // ever adopt the demand, and none runs for one here.
        if !shelf.board_lock().watched {
            return NO_WATCH.to_owned();
        }
        let (cleanup_off, settings_off, idle_off) = (env.settings)();
        let recent = shelf.board_lock().last_pass.clone().filter(|p| {
            p.key == key
                && p.spent
                && p.ended_at.elapsed() < SPENT_RECENT
                && p.demand.is_some_and(|d| d >= need)
                && p.free_after.is_some_and(|f| row.reading.free <= f)
        });
        let refused = if settings_off {
            Some("settings")
        } else if cleanup_off {
            Some("cleanup-off")
        } else if !row.reclaimed_here {
            Some("not-reclaimed")
        } else if recent.is_some() {
            Some(if idle_off { "held-back" } else { "spent" })
        } else {
            None
        };
        if let Some(why) = refused {
            return unmet_reply(&row, need, 0, why, recent.as_ref());
        }
        let id = shelf
            .board_lock()
            .demand(who.session, key.clone(), need, who.cwd.clone(), armed);
        demand = Some(id);
        (env.poke)(need);
    }
    let answer = park(
        shelf,
        wait,
        &key,
        &path,
        (need, demand, seen),
        armed,
        deadline,
        env,
    );
    if let Some(id) = demand {
        shelf.publish(|b| b.drop_demand(id));
    }
    answer
}

/// The parked half of [`run`]: until [`decide`] answers.
#[allow(
    clippy::too_many_arguments,
    reason = "the arm's facts, handed on whole"
)]
fn park(
    shelf: &Shelf,
    wait: &SysWait,
    key: &VolumeKey,
    path: &Path,
    (need, demand, mut seen): (Option<u64>, Option<u64>, Option<Instant>),
    armed: Instant,
    deadline: Instant,
    env: &Env<'_>,
) -> String {
    let mut freed = 0u64;
    let mut adopted: Option<PassSummary> = None;
    let mut poked_at = Instant::now();
    loop {
        let g = shelf.board_lock();
        if let Some(p) = g.last_pass.as_ref()
            && &p.key == key
            && Some(p.ended_at) != seen
            && p.ended_at >= armed
        {
            seen = Some(p.ended_at);
            freed = freed.saturating_add(p.freed);
            adopted = Some(p.clone());
        }
        let Some(row) = g.row_keyed(key).cloned() else {
            drop(g);
            match (env.read)(path) {
                Ok(reading) if &reading.key == key => {
                    let fresh = row_of(reading, None, unix_now(), env.lines);
                    shelf.publish(|b| b.refresh(fresh));
                    if shelf.board_lock().row_keyed(key).is_some() {
                        continue;
                    }
                    return "ERR unavailable free why=unreadable\n".to_owned();
                }
                Ok(_) => return "ERR unavailable free why=unreadable\n".to_owned(),
                Err(u) => return format!("ERR unavailable free why={}\n", u.word()),
            }
        };
        if room_unknown(wait, &row) {
            drop(g);
            return ROOM_UNKNOWN.to_owned();
        }
        let (running, beat_old) = match g.watch {
            Watch::Running { beat } => (true, beat.elapsed() >= STALL),
            Watch::NotStarted | Watch::Suspended => (false, false),
        };
        let facts = Facts {
            met: holds(wait, &row),
            demand: demand.is_some(),
            running,
            beat_old,
            spent: adopted
                .as_ref()
                .is_some_and(|p| p.spent && need.is_some_and(|n| p.demand.is_some_and(|d| d >= n))),
        };
        let decision = decide(&facts);
        let now = Instant::now();
        let age = row.as_of.elapsed();
        if decision == Decision::Park && now < deadline && age < FRESH {
            let until = deadline
                .saturating_duration_since(now)
                .min(FRESH.saturating_sub(age))
                .max(Duration::from_millis(1));
            drop(shelf.park(g, crate::control::hangup_park(until)));
        } else {
            drop(g);
        }
        match decision {
            Decision::Met => return met_reply(&row, freed),
            Decision::Unmet => {
                let (_, _, idle_off) = (env.settings)();
                let why = if adopted.as_ref().is_some_and(|p| p.held_back) || idle_off {
                    "held-back"
                } else {
                    "spent"
                };
                return unmet_reply(&row, need.unwrap_or(0), freed, why, adopted.as_ref());
            }
            Decision::Stalled => return "ERR unavailable free why=watch-stalled\n".to_owned(),
            Decision::Park => {}
        }
        if (env.hung_up)() {
            return crate::control::HUNG_UP_REPLY.to_string();
        }
        if (env.exited)() {
            return "ERR exited\n".to_owned();
        }
        if Instant::now() >= deadline {
            return "OK timeout\n".to_owned();
        }
        if row.as_of.elapsed() >= FRESH {
            match (env.read)(path) {
                Ok(reading) if &reading.key == key => {
                    let fresh = row_of(reading, Some(&row), unix_now(), env.lines);
                    shelf.publish(|b| b.refresh(fresh));
                }
                Ok(_) => return "ERR unavailable free why=unreadable\n".to_owned(),
                Err(u) => return format!("ERR unavailable free why={}\n", u.word()),
            }
        }
        if let Some(n) = need.filter(|_| demand.is_some() && adopted.is_none())
            && poked_at.elapsed() >= FRESH
        {
            (env.poke)(n);
            poked_at = Instant::now();
        }
    }
}

/// Handle `await sys` on the control socket via [`run`].
pub(crate) fn cmd_await_sys(
    term: &std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    store: &crate::session_store::Store,
    session: u64,
    scope: super::Scope,
    parsed: VolumeWait,
) -> String {
    let wait = match wait_of(parsed) {
        Ok(w) => w,
        Err(reply) => return reply.to_owned(),
    };
    let cwd = {
        use crate::cwd_native::ReportedCwd as _;
        term.try_lock()
            .ok()
            .and_then(|t| {
                t.native_working_directory()
                    .map(std::borrow::Cow::into_owned)
            })
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    if let Some(c) = cwd.as_deref().filter(|c| {
        crate::harness_host::disk_short_spelled(c)
            && spell_owed(&crate::disk_board::SHELF.board_lock(), session, c)
    }) {
        let _quiet = aterm_sysprobe::quiet_errors();
        let table = aterm_sysprobe::volumes::table();
        crate::harness_host::disk_spell_now(&crate::disk_board::SHELF, &table, session, c);
    }
    let who = Who {
        session,
        cwd,
        owner_class: scope.is_owner_class(),
    };
    let was_registered = store
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .by_local(session)
        .is_some();
    let exited = || {
        let g = store
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match g.by_local(session).map(|h| h.state) {
            Some(crate::session_store::SessionState::Exited) => true,
            None => was_registered,
            _ => false,
        }
    };
    let read =
        |p: &Path| aterm_agent::harness::disk::volume_of(p, &aterm_sysprobe::volumes::table());
    let sid = session.to_string();
    let poke = |need: u64| {
        aterm_agent::harness::disk::poke::poke(aterm_agent::harness::disk::poke::Cause::Demand {
            sid: sid.clone(),
            need,
        });
    };
    let settings = || {
        let r = aterm_agent::harness::cli::disk_config_read(
            crate::app_config::config_path().as_deref(),
        );
        (
            r.config.auto_free_gib == 0 && !r.off_by_fault,
            r.off_by_fault,
            !r.config.idle_whole_on(),
        )
    };
    let lines = |r: &Reading| {
        aterm_agent::harness::cli::disk_config_read(crate::app_config::config_path().as_deref())
            .thresholds_with(Some(r.total), None)
    };
    let env = Env {
        read: &read,
        poke: &poke,
        settings: &settings,
        lines: &lines,
        hung_up: &crate::control::caller_hung_up,
        exited: &exited,
    };
    run(&crate::disk_board::SHELF, &wait, &who, &env)
}

#[cfg(test)]
#[path = "control_disk_tests.rs"]
mod tests;
