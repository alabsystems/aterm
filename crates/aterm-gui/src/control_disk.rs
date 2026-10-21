// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `await sys` (design `docs/DESIGN-disk-awareness-2026-10-17.md` §3.3): an
//! agent waits for free space without polling.
//!
//! ```text
//! aterm ctl [@<sid>|@self] await sys <term> [and <term>] [vol=<path>] [timeout=<ms>]
//! <term>  := free(>=|>)<size>|room | disk=<word>[,<word>…]
//! <size>  := <n>(B|KB|MB|GB|TB|KiB|MiB|GiB|TiB)     a bare number is ERR usage
//! <word>  := ok | low | short | emergency
//! ```
//!
//! LEVEL-TRIGGERED AND LATCHED: at arm time the wait reads a reading at most
//! [`ARM_FRESH`] old, and a condition already true answers at once. Otherwise it
//! parks on the disk board (`crate::disk_board`), which the watch notifies
//! after every look and every pass, and refreshes its volume's reading itself
//! once it is [`FRESH`] old. Each publishes its reading for other waits to
//! reuse; concurrent readers can overlap while the board lock is released
//! for the volume read.
//!
//! THE DEMAND: a `free` wait armed while free space is below the need, by an
//! owner-class caller (an in-tab `aterm ctl` is one), registers `(session,
//! volume, need)` on the board and pokes the watch, whose next pass runs tiers
//! 1-3 until free space is the need plus the floor — never tier 4, never the
//! reserve. The demand is safe because of what it can reach, not because of who
//! asks. An edge-scoped caller gets the plain wait, no demand. A need more
//! than [`DEMAND_CAP_PCT`] of the volume beyond what is free is refused at arm
//! time (`too-large`: a unit typo must not sweep the machine).
//!
//! No disk watch in this process (`[harness]` off): a wait that would register
//! a demand answers `ERR unavailable free why=no-watch`. A `room` on a volume
//! whose room is not known answers `ERR unavailable free why=room-unknown`.
//!
//! THE HANG DETECTOR: a wait that holds a demand answers
//! `ERR unavailable free why=watch-stalled` when the watch runs and its beat
//! (`aterm_agent::harness::disk::beat`) is [`STALL`] old — never while the host
//! is suspended. The beat ticks through discovery, sizing and deletion, so a
//! pass that takes many minutes never trips it; `timeout=` is the caller's
//! patience and aterm reads no time limit into it. The decision is the pure
//! [`decide`], bound to `DiskAwait` (`disk_await_conformance`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_agent::harness::disk::Level;
use aterm_sysprobe::volumes::{Reading, Unread, VolumeKey};

use crate::disk_board::{Board, PassSummary, Shelf, VolumeRow, Watch};

/// The usage line.
pub(crate) const USAGE: &str = "ERR usage: await sys <free>=<size>|free><size>|free>=room|\
                                disk=<ok|low|short|emergency>[,…]> [and <term>] \
                                [vol=<path>] [timeout=<ms>] (<size>: <n>B|KB|MB|GB|TB|KiB|MiB|GiB|TiB)\n";

/// A metric the system-awareness grammar names that this build does not
/// measure: the vocabulary grows, a second grammar never appears.
pub(crate) const UNKNOWN_METRIC: &str = "ERR usage: this build's await sys knows free and disk\n";

/// A beat this old, while the watch runs, is a stalled watch: two of the
/// watch's look periods (`harness_host::DISK_TICK`), so the gap between two
/// healthy looks with no pass — a minute, plus the host timer's lateness —
/// never reads as a stall.
pub(crate) const STALL: Duration =
    Duration::from_secs(crate::harness_host::DISK_TICK.as_secs().saturating_mul(2));

/// A parked wait reads its volume itself once the reading is this old.
pub(crate) const FRESH: Duration = Duration::from_secs(10);

/// At arm time a reading at most this old is used.
pub(crate) const ARM_FRESH: Duration = Duration::from_secs(1);

/// A pass that spent everything it may for a demand this recently answers a
/// new wait at arm time (`why=spent`), while free space has not risen since.
pub(crate) const SPENT_RECENT: Duration = Duration::from_secs(60);

/// A need more than this percent of the volume beyond what is free now is
/// refused (`why=too-large`).
pub(crate) const DEMAND_CAP_PCT: u64 = 25;

/// What a `free` term needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    /// So many bytes.
    Bytes(u64),
    /// The volume's room, as `status disk_room_mb=` prints it.
    Room,
}

/// One term of the wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Term {
    /// Free space at least (`free>=`) or above (`free>`) the need.
    Free { strict: bool, need: Need },
    /// The volume at one of these levels.
    Disk(Vec<Level>),
}

/// A parsed `await sys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SysWait {
    /// One or two terms, all of which must hold.
    pub(crate) terms: Vec<Term>,
    /// The volume named (`vol=`), else the session's own.
    pub(crate) vol: Option<PathBuf>,
    /// The caller's patience.
    pub(crate) timeout_ms: u64,
}

/// A size with its unit: `<n>B|KB|MB|GB|TB|KiB|MiB|GiB|TiB`.
fn size(text: &str) -> Option<u64> {
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let (n, unit) = text.split_at(digits);
    let n: u64 = n.parse().ok()?;
    let scale: u64 = match unit {
        "B" => 1,
        "KB" => 1_000,
        "MB" => 1_000_000,
        "GB" => 1_000_000_000,
        "TB" => 1_000_000_000_000,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    n.checked_mul(scale)
}

/// One level word.
fn level(word: &str) -> Option<Level> {
    Some(match word {
        "ok" => Level::Ok,
        "low" => Level::Low,
        "short" => Level::Short,
        "emergency" => Level::Emergency,
        _ => return None,
    })
}

/// One term, or the reply that refuses it.
fn term(tok: &str) -> Result<Term, &'static str> {
    for (op, strict) in [(">=", false), (">", true)] {
        if let Some(value) = tok.strip_prefix("free").and_then(|t| t.strip_prefix(op)) {
            let need = if value == "room" {
                Need::Room
            } else {
                Need::Bytes(size(value).ok_or(USAGE)?)
            };
            return Ok(Term::Free { strict, need });
        }
    }
    if let Some(words) = tok.strip_prefix("disk=") {
        let levels: Option<Vec<Level>> = words.split(',').map(level).collect();
        return levels
            .filter(|l| !l.is_empty())
            .map(Term::Disk)
            .ok_or(USAGE);
    }
    // `<metric><op>…` of a metric this build does not measure.
    let metric: String = tok
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    let rest = &tok[metric.len()..];
    if !metric.is_empty() && ["=", ">", "<"].iter().any(|op| rest.starts_with(op)) {
        return Err(UNKNOWN_METRIC);
    }
    Err(USAGE)
}

/// Parse `rest` (`sys <term> [and <term>] [vol=<path>] [timeout=<ms>]`).
///
/// # Errors
///
/// The reply that refuses it: the usage line, or [`UNKNOWN_METRIC`].
pub(crate) fn parse(rest: &str) -> Result<SysWait, &'static str> {
    let mut toks = rest.split_whitespace();
    if toks.next() != Some("sys") {
        return Err(USAGE);
    }
    let mut terms = Vec::new();
    let mut vol = None;
    let mut timeout_ms = aterm_control::wire::WAIT_DEFAULT_MS;
    let mut joined = true;
    while let Some(tok) = toks.next() {
        let timeout = if let Some(v) = tok.strip_prefix("timeout=") {
            Some(v)
        } else if tok == "timeout" {
            Some(toks.next().ok_or(USAGE)?)
        } else {
            None
        };
        if let Some(v) = timeout {
            timeout_ms = aterm_control::wire::wait_ms(v).ok_or(USAGE)?;
            continue;
        }
        if let Some(path) = tok.strip_prefix("vol=") {
            let path = PathBuf::from(aterm_control::wire::pct_decode(path));
            if !path.is_absolute() {
                return Err(USAGE);
            }
            vol = Some(path);
            continue;
        }
        if tok == "and" {
            if joined || terms.is_empty() {
                return Err(USAGE);
            }
            joined = true;
            continue;
        }
        if !joined {
            return Err(USAGE);
        }
        terms.push(term(tok)?);
        joined = false;
    }
    if terms.is_empty() || terms.len() > 2 || joined {
        return Err(USAGE);
    }
    Ok(SysWait {
        terms,
        vol,
        timeout_ms,
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
    /// A pass adopted this wait's need, ran every tier it may and left free
    /// space below it.
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

/// THE DECISION, pure: met on a reading that holds; unmet only once a pass
/// for this wait's demand spent what it may; stalled only for a demand, while
/// the watch runs and its beat is old; else park.
pub(crate) fn decide(f: &Facts) -> Decision {
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

/// Does every term of `wait` hold on `row`? `needs` are the terms' needs in
/// bytes (a `room` resolved).
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

/// A need in bytes on `row`'s volume; a `room` on a volume whose room is not
/// known needs everything (it never holds: [`room_unknown`] answers first).
fn need_bytes(need: Need, row: &VolumeRow) -> u64 {
    match need {
        Need::Bytes(n) => n,
        Need::Room => row.room.unwrap_or(u64::MAX),
    }
}

/// Does `wait` name `room` on a volume whose room is not known (no meter
/// reads it: the watch never measured it, or the harness state was missing)?
/// Such a wait is `ERR unavailable free why=room-unknown`, never a need of 0
/// met at once.
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

/// The reply to a demand where no disk watch runs (`[harness]` off): no pass
/// would ever adopt it.
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

/// What the wait reads beyond the board, injected (the product's is
/// [`Env::live`], a test's scripts each).
pub(crate) struct Env<'a> {
    /// A fresh reading of the volume holding a path (one `statfs`).
    pub(crate) read: &'a dyn Fn(&Path) -> Result<Reading, Unread>,
    /// Ask the watch to look now for a demand of so many bytes.
    pub(crate) poke: &'a dyn Fn(u64),
    /// The removal settings: `(cleanup off by the owner's auto_free_gib = 0,
    /// off by a [disk] value aterm cannot read, tier 3 off)`.
    pub(crate) settings: &'a dyn Fn() -> (bool, bool, bool),
    /// The lines in force on a volume the watch has not measured, from the
    /// owner's `[disk]` settings (a wait's own reading of it).
    pub(crate) lines: &'a dyn Fn(&Reading) -> aterm_agent::harness::disk::Thresholds,
    /// The caller hung up.
    pub(crate) hung_up: &'a dyn Fn() -> bool,
    /// The target session exited.
    pub(crate) exited: &'a dyn Fn() -> bool,
}

/// Who waits: the session (its local id and working directory) and whether
/// the caller may register a demand.
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

/// A row for `reading` keeping `old`'s lines and room (a wait's own reading
/// of a volume the watch measured), or the owner's lines for it (`lines`)
/// with its room unknown.
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

/// Whether a wait from session `session`, whose tab reports `cwd` with 8.3
/// short names, spells it long before it asks the board
/// (`cmd_await_sys`): only where board `b` places the session nowhere — no
/// look mapped it ([`Board::row_of`]'s sessions) and no long spelling of
/// that very spelling is kept for it ([`Board::spelled`], which may place
/// on no row yet). A tab the board places costs its wait no file-system
/// call (rev6-l-2). Pure.
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

/// THE WAIT over `shelf`: arm, latch or register a demand, park until it
/// answers. See the module docs.
pub(crate) fn run(shelf: &Shelf, wait: &SysWait, who: &Who, env: &Env<'_>) -> String {
    let armed = Instant::now();
    let deadline = armed + Duration::from_millis(wait.timeout_ms);
    // The volume: `vol=`, else the session's row, else its working
    // directory's spelling — placed on the board's table, or read.
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
    // A reading at most ARM_FRESH old, carried on from the row the board
    // holds for the volume it READ — its room, its lines and whether a pass
    // removes there. A spelling the board could not place (an 8.3 `vol=`,
    // or an 8.3 tab not spelled long yet) gave no key, and the reader places
    // it where the board's table cannot (it spells short names long): a row
    // built from nothing there answered `free>=room` `why=room-unknown` and
    // a demand `why=not-reclaimed` on a volume the watch measured (live5
    // F1). A path that now names another volume carries on that volume's
    // row, if any — never the one its spelling named before.
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
    // A need more than a quarter of the volume beyond what is free now is a
    // unit typo (TB for GB), never a build: it must not sweep the machine.
    // `cap_bytes=` is the largest need taken now.
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
    // The last pass the wait has already seen: one published after this
    // instant, the demand's own included, is counted (read before the demand
    // lands, so a pass that adopts it at once cannot be taken for an old one).
    let seen = shelf.board_lock().last_pass.as_ref().map(|p| p.ended_at);
    // THE DEMAND, for a `free` term not met, from an owner-class caller.
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

/// The parked half of [`run`]: until [`decide`] answers, the deadline passes,
/// the caller hangs up or the session exits.
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
        // The facts are read under the guard the wait then parks with: a
        // publish between the two cannot be missed.
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
            // The watch's look left its row out (no session works there
            // now): the wait reads the volume itself and goes on.
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
            // Share this reading with the other parked waits. Another reader
            // may already be in flight while the board lock is released.
            match (env.read)(path) {
                Ok(reading) if &reading.key == key => {
                    let fresh = row_of(reading, Some(&row), unix_now(), env.lines);
                    shelf.publish(|b| b.refresh(fresh));
                }
                Ok(_) => return "ERR unavailable free why=unreadable\n".to_owned(),
                Err(u) => return format!("ERR unavailable free why={}\n", u.word()),
            }
        }
        // A demand no pass has adopted yet: the watch is asked again (a look
        // already in flight when it was armed did not see it).
        if let Some(n) = need.filter(|_| demand.is_some() && adopted.is_none())
            && poked_at.elapsed() >= FRESH
        {
            (env.poke)(n);
            poked_at = Instant::now();
        }
    }
}

/// `await sys` on the control socket: the session's working directory read
/// from its terminal (a guard taken and dropped at once), the product's
/// readers, and [`run`] over the process's board.
pub(crate) fn cmd_await_sys(
    term: &std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    store: &crate::session_store::Store,
    session: u64,
    scope: super::Scope,
    rest: &str,
) -> String {
    let wait = match parse(rest) {
        Ok(w) => w,
        Err(reply) => return reply.to_owned(),
    };
    // Every platform with a volume reader waits (macOS, Linux, Windows); one
    // without answers `why=unsupported` from its reader's refusal.
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
    // A directory reported with 8.3 short names, which the board places
    // nowhere for this session — no look mapped the tab, and it is not
    // spelled long yet — is spelled long here, on the wait's own thread,
    // before the board is asked where it is (`harness_host::disk_spell_now`):
    // the wait then carries on its volume's row, and `status disk=` places
    // the tab too (live5 F2). A tab the board places already is not asked
    // about (rev6-l-2): its wait reads its row's mount, as it did before
    // spellings were kept. The spelling's one file-system call waits at
    // most `harness_host::DISK_SPELL_DEADLINE`, outside this wait's own
    // `timeout=` and hang-up checks, and once stuck answers at once.
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
