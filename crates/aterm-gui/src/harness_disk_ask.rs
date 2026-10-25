// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's side of the ask (`disk-free@v1`, [`aterm_agent::harness::disk::ask`],
//! `docs/DESIGN-disk-free-ask-2026-10-24.md`): after each disk look ([`super::disk_look`]) this
//! process steps the machine-wide lease with its own sessions, journals what it decided, keeps
//! the line owed one of its sessions on the board for that session's worker
//! ([`super::WorkerIdle`]), and says on the owner's disk line what the asks did.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use aterm_agent::harness::disk::{self, Hog, Level, ask};
use aterm_agent::supervise::Guard;
use aterm_phase::Program;

use crate::task_record::TaskState;

use super::{HostNote, NoteFn, Store, agent_of, harness_state};

/// What the disk look read that the ask needs ([`super::disk_look`]).
#[derive(Debug, Default)]
pub(super) struct Looked {
    /// The thresholds in force on the home volume; `None`: the look stopped before it read them.
    pub(super) thresholds: Option<disk::Thresholds>,
    /// The look ran in a private state root ([`super::disk_private`]): this process takes no
    /// part in the ask, as it removes nothing.
    pub(super) private: bool,
    /// Why aterm's own freeing is done, where the look itself said (`[disk]` turned removal off,
    /// or the volume is out of inodes).
    pub(super) done: Option<ask::Done>,
    /// Another instance's pass held the pass lock: it may be freeing space now.
    pub(super) contended: bool,
}

/// One of this process's sessions the ask could go to, and where it works.
struct Seated {
    seat: ask::Seat,
    cwd: PathBuf,
}

/// The last word each volume's step said here, so a standing state is journalled once.
static SAID: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// What the owner's disk line says of the ask now ([`ask::owner_line`]), as this process's
/// last step read the lease.
static OWNER: Mutex<Option<String>> = Mutex::new(None);

/// What the owner's disk line says of the ask now: [`HostNote::DiskLow`]'s `asked`.
pub(super) fn owner_said() -> Option<String> {
    OWNER.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// The unix ms now.
fn now_ms() -> i64 {
    aterm_time::unix_millis()
        .and_then(|ms| i64::try_from(ms).ok())
        .unwrap_or(0)
}

/// As a look begins, before its pass: take over what a host that ended left in the home volume's
/// open episode of this process's sessions `sids` (the supervised ones, Autopilot not Off), a
/// seamless update's predecessor's pick, nominee and held records among them
/// ([`ask::take_over`]). It reads only what the pass does not change: the volume's key, the lease
/// and this process's seat rule. A look's pass can run a minute or more (discovery, profile
/// walks), and the step after it ([`after_look`]) was the first a successor adopted at, so
/// another host's look while its first pass ran, [`ask::ADOPT_MS`] after it first read the
/// predecessor ended, handed the pick off as undelivered, or dropped its nominee. The lease is
/// read first without its lock, and its lock taken only when there is something to take over; a
/// contended lock leaves it to the step after the pass, which adopts as well.
pub(super) fn before_pass(sids: &[String], store: &Store) {
    let (Some(home), Some(state)) = (aterm_primer::home_dir(), harness_state()) else {
        return;
    };
    if super::disk_private(&home).is_some() {
        return;
    }
    let Ok(reading) = disk::volume_of(&home, &aterm_sysprobe::volumes::table()) else {
        return;
    };
    let key = reading.key.spelled();
    let host = ask::HostId::this();
    let lease = ask::read(&state);
    let open = lease.entry(&key).is_some_and(|e| e.episode_ms.is_some());
    if !open {
        return;
    }
    let Rule {
        barred, live, held, ..
    } = rule(store, sids, &key, &[], false);
    let alive = |h: &ask::HostId| h.alive();
    let roster = ask::Roster {
        host: &host,
        key: &key,
        supervised: sids,
        barred: &barred,
        live: &live,
        held: &held,
        alive: &alive,
    };
    if ask::take_over(&mut lease.clone(), &roster).is_empty() {
        return;
    }
    match ask::transact(&state, &ask::LOCK_LADDER, |l| ask::take_over(l, &roster)) {
        Ok(Some(took)) if !took.is_empty() => aterm_log::info!(
            "harness disk watch: {} took over {} from a host that ended, before this look's pass",
            ask::RULE_DISK_FREE,
            took.iter()
                .map(|s| format!("@{s}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Ok(Some(_)) => {}
        Ok(None) => aterm_log::debug!(
            "harness disk watch: {} is held; the ask's take-over waits for the step after the pass",
            state.join(ask::LOCK_FILE).display()
        ),
        Err(e) => aterm_log::warn!(
            "harness disk watch: the ask's lease in {} could not take over before the pass ({e}); \
             the step after the pass adopts",
            state.display()
        ),
    }
}

/// After a look: step the home volume's lease with this process's sessions `sids` (the
/// supervised ones, Autopilot not Off), as [`Looked`] says the look went. The look judges the
/// volume short against this process's own floor ([`disk::Thresholds::short_below`]); the lease
/// holds an open episode to the floor it recorded ([`ask::Entry::floor`]), so a look here that
/// reads it clear of this process's floor steps the episode on until it clears that one too, and
/// records the floor it held it to, which the line and the owner's note then say. A look that
/// reads the volume clear takes no lock only when the lease has no episode open there and holds
/// every line this process typed on it ([`ask::rests`]): a line whose record a full disk kept from
/// the lease, and whose episode another host then ended as the space came back, is still recorded
/// ([`ask::typed_unrecorded`]), so its cooldown starts and the next shortage types no second line
/// within it.
pub(super) fn after_look(looked: &Looked, sids: &[String], store: &Store, note: &NoteFn) {
    let Some(thresholds) = looked.thresholds else {
        return;
    };
    if looked.private {
        return;
    }
    let (Some(home), Some(state)) = (aterm_primer::home_dir(), harness_state()) else {
        return;
    };
    let table = aterm_sysprobe::volumes::table();
    let Ok(reading) = disk::volume_of(&home, &table) else {
        return;
    };
    let level = thresholds.level(Some(reading.free), reading.files_free);
    let key = reading.key.spelled();
    let host = ask::HostId::this();
    let now = now_ms();
    if look_rests(&state, &key, level) {
        // At or above the floor with no episode open, and no line of this process's that the
        // lease has yet to record: nothing to step, no lock to take.
        crate::disk_board::publish(|b| {
            b.asks.retain(|_, o| o.key != key);
            b.unfit.retain(|sid, _| sids.contains(sid));
            b.walls.retain(|sid, _| sids.contains(sid));
        });
        *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = None;
        journal(
            &state,
            now,
            &key,
            &ask::Said::Quiet,
            level,
            reading.free,
            &host,
        );
        return;
    }
    let (spent, holders) = {
        let b = crate::disk_board::SHELF.board_lock();
        let pass = b.last_pass.as_ref().filter(|p| p.key == reading.key);
        (
            pass.is_some_and(|p| p.ran_out && thresholds.level(p.free_after, None) >= Level::Short),
            pass.map(|p| p.left.clone()).unwrap_or_default(),
        )
    };
    let done = if looked.contended {
        None
    } else {
        looked.done.or(spent.then_some(ask::Done::Spent))
    };
    let Rule {
        seated,
        barred,
        live,
        held,
    } = rule(store, sids, &key, &holders, true);
    let seats: Vec<ask::Seat> = seated.iter().map(|s| s.seat.clone()).collect();
    let alive = |h: &ask::HostId| h.alive();
    let look = ask::Look {
        now_ms: now,
        host: &host,
        key: &key,
        level,
        free: reading.free,
        floor: thresholds.short_below(),
        files_free: reading.files_free,
        files_floor: thresholds.files_emergency,
        done,
        seats: &seats,
        supervised: sids,
        barred: &barred,
        live: &live,
        held: &held,
        alive: &alive,
    };
    let unrecorded = typed_last(&key);
    let stepped = ask::transact(&state, &ask::LOCK_LADDER, |lease| {
        if let Some(t) = &unrecorded {
            ask::typed_unrecorded(lease, &key, &host, &t.sid, t.at_ms, t.free);
        }
        let said = ask::step(lease, &look);
        (said, lease.entry(&key).cloned())
    });
    let (said, entry) = match stepped {
        Ok(Some(stepped)) => stepped,
        Ok(None) => {
            aterm_log::debug!(
                "harness disk watch: {} is held; the ask steps at the next look",
                state.join(ask::LOCK_FILE).display()
            );
            return;
        }
        Err(e) => {
            aterm_log::warn!(
                "harness disk watch: the ask's lease in {} could not be stepped ({e}); it \
                 steps at the next look",
                state.display()
            );
            return;
        }
    };
    journal(&state, now, &key, &said, level, reading.free, &host);
    let owed_here = entry.as_ref().and_then(|e| match &e.phase {
        ask::Phase::Owed { pick, .. } if pick.host == host => Some(pick.sid.clone()),
        _ => None,
    });
    let owed = owed_here.and_then(|sid| {
        let s = seated.iter().find(|s| s.seat.sid == sid)?;
        let checkout = checkout_of(&holders, &s.cwd);
        let text = ask::line(&ask::Line {
            mount: &reading.mount,
            total: reading.total,
            free: reading.free,
            // The floor this look held the episode to ([`ask::Entry::floor`]).
            floor: if level == Level::Emergency {
                thresholds.emergency
            } else {
                entry
                    .as_ref()
                    .and_then(|e| e.floor.as_ref())
                    .map_or(thresholds.short_below(), |f| f.bytes)
            },
            emergency: level == Level::Emergency,
            done: entry
                .as_ref()
                .and_then(|e| e.done)
                .or(done)
                .unwrap_or(ask::Done::Spent),
            holders: &holders,
            checkout,
            home: Some(&home),
        });
        Some((sid, text))
    });
    crate::disk_board::publish(|b| {
        b.unfit.retain(|sid, _| sids.contains(sid));
        b.walls.retain(|sid, _| sids.contains(sid));
        b.asks
            .retain(|sid, o| o.key != key || owed.as_ref().is_some_and(|(s, _)| s == sid));
        if let Some((sid, text)) = &owed {
            b.asks.insert(
                sid.clone(),
                crate::disk_board::AskOwed {
                    key: key.clone(),
                    text: text.clone(),
                },
            );
        }
    });
    let owner = entry.as_ref().and_then(|e| ask::owner_line(e, now));
    *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = owner.clone();
    if out_raises_low(
        &said,
        level,
        thresholds.floor,
        entry.as_ref().and_then(|e| e.done),
    ) {
        let glass: Vec<String> = holders
            .iter()
            .map(|h| h.line_within(Some(&home), crate::message_reporters::DISK_LOW_HOG_CHARS))
            .collect();
        // The floor this look held the episode to: a host whose own floor is met may be the one
        // whose look ran the asks out.
        let floor = entry
            .as_ref()
            .and_then(|e| e.floor.as_ref())
            .map_or(thresholds.floor, |f| f.bytes);
        note(HostNote::DiskLow {
            free_gib: reading.free >> 30,
            floor_gib: floor >> 30,
            hogs: glass,
            asked: owner,
        });
    }
}

/// Whether a look whose step said `said`, reading the volume at `level` against this process's
/// floor `floor` (`[disk] auto_free_gib`, in bytes), raises the owner's `disk.low` line itself
/// with the ask's out sentence: the episode ran out at this look after a spent pass, and this look
/// reads the volume short. The pass's own line is `disk.low`; out of inodes or with removal off,
/// the owner's line is the one the look raises itself (`disk.full`, `disk.low-cleanup-off`), which
/// says [`owner_said`] at each look. A look that reads the volume at or above its own floor (an
/// episode another host's floor holds open, within [`ask::END_MARGIN`]) raises nothing: its
/// `disk.low` said the home volume was still short, the next look found it plenty and cleared it,
/// and the owner kept a false alert's record. The out sentence then goes with the next short
/// look's line ([`owner_said`]).
fn out_raises_low(said: &ask::Said, level: Level, floor: u64, done: Option<ask::Done>) -> bool {
    matches!(said, ask::Said::Out { .. })
        && level >= Level::Short
        && floor > 0
        && done == Some(ask::Done::Spent)
}

/// Whether a look that reads volume `key` at `level` leaves the lease in `state` as it is, its lock
/// untaken ([`after_look`]): the volume is at or above this process's floor, no episode is open
/// there, and the lease holds the last line this process typed on it ([`ask::rests`] of
/// [`typed_last`]). A line whose record a full disk kept from the lease, and whose episode another
/// host then ended as the space came back, so steps the lease at this look, which records it.
fn look_rests(state: &Path, key: &str, level: Level) -> bool {
    level < Level::Short && ask::rests(&ask::read(state), key, typed_last(key).map(|t| t.at_ms))
}

/// Journal `said` when it moved the episode, or when its standing word changed here.
fn journal(
    state: &Path,
    now_ms: i64,
    key: &str,
    said: &ask::Said,
    level: Level,
    free: u64,
    host: &ask::HostId,
) {
    let word = format!("{} {}", said.word(), said.sid());
    let changed = {
        let mut seen = SAID.lock().unwrap_or_else(PoisonError::into_inner);
        let before = seen.insert(key.to_owned(), word.clone());
        match before {
            Some(before) => before != word,
            None => !matches!(said, ask::Said::Quiet),
        }
    };
    if !said.moves() && !changed {
        return;
    }
    if said.moves() {
        aterm_log::info!(
            "harness disk watch: {} {}: {}",
            ask::RULE_DISK_FREE,
            said.word(),
            said.describe()
        );
    }
    aterm_agent::harness::cli::disk_ask_journal(
        state,
        &[ask::row(now_ms / 1000, key, said, level, free, host)],
    );
}

/// What the seat rule reads of one of this process's sessions ([`judge`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Facts {
    /// The harness supervises it, its Autopilot not Off.
    supervised: bool,
    /// The supervised agent its publication names ([`agent_of`]).
    program: Option<Program>,
    /// Its task record's state.
    task: Option<TaskState>,
    /// Why its worker found it unfit at its last turn break (`Board::unfit`), as that finding
    /// holds now ([`crate::disk_board::Unfit::bars`]).
    unfit: Option<&'static str>,
    /// What holds it where it cannot take a line now and passes ([`held_of`]): `limited`, an
    /// unspent usage-limit wall; `box`, a box its loop escalated to its owner; `hold`, the owner's
    /// hold or a person's take-over; `draft`, a draft standing in its composer at a rest.
    held: Option<ask::HeldBy>,
    /// Whether the board places its directory on the volume asked about; `None`: its directory,
    /// or the volume it is on, was not read.
    on_volume: Option<bool>,
}

/// What the seat rule says of one session ([`judge`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Judged {
    /// It may be nominated.
    Seat,
    /// It may not be, by a fact that lasts past this look: a line owed it is released at this
    /// look ([`ask::Look::barred`]) or its turn break ([`act`]), never typed.
    Barred(&'static str),
    /// It is not seated this look, by nothing that need last: it is not supervised, its
    /// publication names no agent, or its directory or its volume was not read. A line owed it
    /// stays owed while it is supervised, and is handed on once it is not.
    Unseated,
    /// It would be seated but for what holds it now and passes ([`Facts::held`]): it is not
    /// nominated, a line owed it goes back to the episode at this look, once an episode, and is
    /// released the next time ([`ask::Look::held`]), as it could not take it, and a round with
    /// nobody else to elect waits for it to be past.
    Held(ask::HeldBy),
}

/// The seat rule: a supervised Claude Code (the line is written for its composer) working on the
/// volume, with no task record that stands ([`task_bar_of`]: done, exited, or a manager's
/// worker's) and whose worker did not find it unfit at its last turn break
/// ([`super::WorkerIdle`]: its task finished or landed, it has none, its Autopilot is Off, or it
/// is the person's own exchange). The task record bars a session whether or not it is still
/// supervised, so an agent that exited, and left the supervised roster, is released, not handed
/// on as undelivered.
fn judge(f: &Facts) -> Judged {
    if let Some(why) = task_bar_of(f.task).or(f.unfit) {
        return Judged::Barred(why);
    }
    if !f.supervised {
        return Judged::Unseated;
    }
    match f.program {
        Some(Program::Claude) => {}
        // The line is written for Claude Code's composer; another agent gets no seat until it
        // is checked against its own.
        Some(
            Program::Codex
            | Program::Agy
            | Program::Muse
            | Program::Grok
            | Program::Kimi
            | Program::Generic,
        ) => return Judged::Barred("not-claude"),
        None => return Judged::Unseated,
    }
    match f.on_volume {
        Some(true) => f.held.map_or(Judged::Seat, Judged::Held),
        Some(false) => Judged::Barred("off-volume"),
        None => Judged::Unseated,
    }
}

/// What holds a session where it cannot take a line now, as a look reads it ([`Facts::held`], each
/// [`ask::HeldBy`] here by its word):
/// - `hold` while the owner's hold or a person's take-over stands on it (`halted`: the fabric's
///   hold, `status hold=1`), whatever its agent reads;
/// - `limited` while its agent reads `limited` at a wall its loop has open and has not yet taken
///   as spent (`wall`, [`crate::disk_board::Wall::holds`] at `now_s`: before its reset and the
///   loop's grace, when the loop rests there and its line could go);
/// - `box` while its agent reads a box (`prompt`, `question`) its loop escalated to its owner
///   (`escalated`: the supervisor's own attention stands on it, not a limit's);
/// - `draft` while its agent reads `idle` with a draft standing in its composer (`drafted`: the
///   same typed text this process's last look read there, [`Draft::stands`]).
///
/// A box no rule answers waits on a person, and a wall on its reset: neither turn ends, so a line
/// owed would wait out its delivery window. A hold and a draft keep the session at a turn break,
/// but the fenced write refuses every try there (`wait:held`, `wait:draft`) while they last, so a
/// line owed would wait it out as well, its claim stretching it to the delivery window and a
/// grace. All of them pass, so the session is held, not barred.
fn held_of(
    word: &str,
    escalated: bool,
    wall: Option<&crate::disk_board::Wall>,
    halted: bool,
    drafted: bool,
    now_s: i64,
) -> Option<ask::HeldBy> {
    if halted {
        return Some(ask::HeldBy::Hold);
    }
    match word {
        "limited" if wall.is_some_and(|w| w.holds(now_s)) => Some(ask::HeldBy::Limit),
        "prompt" | "question" if escalated => Some(ask::HeldBy::Box),
        "idle" if drafted => Some(ask::HeldBy::Draft),
        _ => None,
    }
}

/// What a session's composer held as a look read it ([`draft_in`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Draft {
    /// Its screen was not read: its terminal was busy, scrolled back or mid-frame, or the session
    /// is not supervised.
    Unread,
    /// No typed text: an empty composer, its placeholder, or none drawn.
    Clear,
    /// Typed text stands there.
    Text(String),
}

impl Draft {
    /// Whether this reading is a draft standing since `last`, what this process's last look read
    /// of the composer ([`DRAFTS`]): the same typed text, a look (a minute) apart, as the loop's
    /// own guard reads a draft that stands. A draft a person is still typing reads as no draft
    /// that stands.
    fn stands(&self, last: Option<&str>) -> bool {
        matches!(self, Draft::Text(t) if last == Some(t.as_str()))
    }
}

/// What the composer drawn in `rows` holds, the caret at `cursor` and `dim` saying whether a
/// row's first composer glyph is dim (Claude Code's placeholder, [`glyph_dim`]): read as the
/// fenced write reads it ([`aterm_agent::harness::upgrade::composer_is_empty`]), so a draft the
/// look reads is one the write would refuse.
fn draft_in(rows: &[String], cursor: (usize, usize), dim: impl Fn(usize) -> bool) -> Draft {
    let Some((caret, lines)) = aterm_phase::phase::composer_draft(rows) else {
        return Draft::Clear;
    };
    if aterm_agent::harness::upgrade::composer_is_empty(rows, Some(cursor), dim(caret)) {
        Draft::Clear
    } else {
        Draft::Text(lines.join("\n"))
    }
}

/// What `t`'s live screen shows in its composer ([`draft_in`]); [`Draft::Unread`] while it is
/// scrolled back or mid-frame.
fn draft_on(t: &aterm_core::terminal::Terminal) -> Draft {
    if t.grid().display_offset() > 0 || crate::frame_unfinished_now(t) {
        return Draft::Unread;
    }
    let rows: Vec<String> = (0..usize::from(t.rows()))
        .map(|r| crate::claude_lights::row_text(t, r))
        .collect();
    let cursor = t.cursor();
    draft_in(
        &rows,
        (usize::from(cursor.row), usize::from(cursor.col)),
        |row| {
            let cells = u16::try_from(row)
                .ok()
                .and_then(|row| t.grid().row_at_screen(row));
            glyph_dim(|col| {
                let col = u16::try_from(col).ok()?;
                cells.and_then(|cells| cells.get(col)).map(|c| c.flags())
            })
        },
    )
}

/// Whether the composer's first glyph is dim, `flags(col)` the composer row's cell flags at
/// `col`: the glyph at [`aterm_agent::supervise::screen::COMPOSER_COL`], or, where the program
/// draws its own caret over it (`inverse`, as Claude Code draws its suggestion's first glyph), the
/// glyph after it, as the fenced write reads the same cells
/// ([`aterm_agent::supervise::screen::composer_glyph_cell`]). Read at the caret alone, a
/// suggestion's inverse first glyph reads plain, and the suggestion as a draft.
fn glyph_dim(flags: impl Fn(usize) -> Option<aterm_core::grid::CellFlags>) -> bool {
    use aterm_agent::supervise::screen::COMPOSER_COL;
    use aterm_core::grid::CellFlags;
    let glyph = match flags(COMPOSER_COL) {
        Some(first) if first.contains(CellFlags::INVERSE) => flags(
            COMPOSER_COL
                + if first.contains(CellFlags::WIDE) {
                    2
                } else {
                    1
                },
        ),
        first => first,
    };
    glyph.is_some_and(|f| f.contains(CellFlags::DIM))
}

/// The typed text each session's composer held at this process's last look ([`after_look`]), so
/// the next look knows a draft that stands ([`Draft::stands`]).
static DRAFTS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Why the seat rule bars a session by its task record: its task is done (it landed) or
/// exited, or it goes on, so the session is a manager's worker (`managed`, as its loop's guard
/// reads the same record at its first turn break: `Guards::theirs`). This is read at every look,
/// so a managed worker is barred from its first look on, mid-turn or not, and in a seamless
/// update's successor before any turn break of its own; no finding of its worker's need stand
/// for it. `None`: it has no task record.
fn task_bar_of(state: Option<TaskState>) -> Option<&'static str> {
    match state? {
        TaskState::Done => Some("task-done"),
        TaskState::Exited => Some("task-exited"),
        TaskState::Working | TaskState::Stalled => Some(Guard::Managed.word()),
    }
}

/// One of this process's live sessions as a look reads it, before the board places it.
struct Read {
    sid: String,
    local: u64,
    supervised: bool,
    program: Option<Program>,
    task: Option<TaskState>,
    /// Its directory, read only for a supervised session.
    cwd: Option<PathBuf>,
    /// It rests at a turn break.
    at_break: bool,
    /// Its agent reads busy: it works.
    working: bool,
    /// Its agent's word ([`held_of`]).
    word: &'static str,
    /// The supervisor's own attention stands on it, and is no usage limit's: its loop escalated a
    /// box to its owner ([`held_of`]).
    escalated: bool,
    /// The owner's hold or a person's take-over stands on it ([`held_of`]).
    halted: bool,
    /// What its composer holds, read only for a supervised session ([`Draft`]).
    draft: Draft,
}

/// This process's live sessions (`only`: that one alone), each as [`Read`] says; `sids` are the
/// supervised ones.
fn read_sessions(store: &Store, sids: &[String], only: Option<&str>) -> Vec<Read> {
    use crate::cwd_native::ReportedCwd as _;
    let handles: Vec<_> = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        g.live_handles()
            .filter(|h| only.is_none_or(|sid| h.sid.as_str() == sid))
            .cloned()
            .collect()
    };
    let known = super::DISK_ALL_CWDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    handles
        .iter()
        .map(|h| {
            let (program, word, task) = {
                let t = h
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let p = t.agent();
                (agent_of(p), p.word, t.task().map(|r| r.state))
            };
            let escalated = h
                .ctx
                .meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .attention_owners
                .get(aterm_agent::supervise::ATTENTION_OWNER)
                .is_some_and(|t| !t.starts_with(aterm_agent::supervise::ATTENTION_PREFIX));
            let halted = h.ctx.fabric.hold().is_some();
            let sid = h.sid.as_str().to_owned();
            let supervised = sids.contains(&sid);
            let (cwd, draft) = if supervised {
                match h.term.try_lock() {
                    Ok(t) => (
                        t.native_working_directory()
                            .filter(|c| !c.is_empty())
                            .map(|c| PathBuf::from(c.into_owned())),
                        draft_on(&t),
                    ),
                    Err(_) => (
                        known
                            .iter()
                            .find(|(id, _)| *id == h.local_id)
                            .map(|(_, cwd)| cwd.clone()),
                        Draft::Unread,
                    ),
                }
            } else {
                (None, Draft::Unread)
            };
            let cwd = cwd.filter(|cwd| cwd.is_absolute());
            Read {
                sid,
                local: h.local_id,
                supervised,
                program,
                task,
                cwd,
                at_break: word == "idle",
                working: word == "busy",
                word,
                escalated,
                halted,
                draft,
            }
        })
        .collect()
}

/// The seat rule ([`judge`]) over `read` on volume `key` (its spelling), as the board places
/// each session and says what its worker found at its last turn break, as that holds now: all of
/// it until the session works again, and only whose it is (the person's own exchange, a manager's
/// wait) while its agent reads busy ([`crate::disk_board::Unfit::bars`]), so a session given
/// something to do since is seated again.
fn judged(read: Vec<Read>, key: &str) -> Vec<(Read, Judged)> {
    let now_s = aterm_time::unix_secs()
        .and_then(|s| i64::try_from(s).ok())
        .unwrap_or(0);
    let drafts = DRAFTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let b = crate::disk_board::SHELF.board_lock();
    read.into_iter()
        .map(|r| {
            let drafted = r.draft.stands(drafts.get(&r.sid).map(String::as_str));
            let facts = Facts {
                supervised: r.supervised,
                program: r.program,
                task: r.task,
                unfit: b.unfit.get(&r.sid).and_then(|u| u.bars(r.working)),
                held: held_of(
                    r.word,
                    r.escalated,
                    b.walls.get(&r.sid),
                    r.halted,
                    drafted,
                    now_s,
                ),
                on_volume: r
                    .cwd
                    .as_ref()
                    .and_then(|cwd| b.row_of(r.local, cwd.to_str()))
                    .map(|row| row.reading.key.spelled() == key),
            };
            let judged = judge(&facts);
            (r, judged)
        })
        .collect()
}

/// What the seat rule found for this process's sessions on one volume ([`rule`]).
struct Rule {
    /// Those the ask could go to, each with what `holders` hold for its checkout and whether it
    /// rests at a turn break.
    seated: Vec<Seated>,
    /// Those it bars by a fact that lasts ([`ask::Look::barred`]).
    barred: Vec<String>,
    /// Every live session.
    live: Vec<String>,
    /// Those it would seat but for what holds them now and passes, each with what holds it
    /// ([`ask::Look::held`]).
    held: Vec<(String, ask::HeldBy)>,
}

/// The seat rule over this process's live sessions on volume `key` (its spelling), `sids` the
/// supervised ones and `holders` what the pass left. A look's own rule (`remember`, after its
/// pass) keeps what each composer held for the next look ([`DRAFTS`]); the take-over before the
/// pass reads it alone.
fn rule(store: &Store, sids: &[String], key: &str, holders: &[Hog], remember: bool) -> Rule {
    let mut out = Rule {
        seated: Vec::new(),
        barred: Vec::new(),
        live: Vec::new(),
        held: Vec::new(),
    };
    let judged = judged(read_sessions(store, sids, None), key);
    if remember {
        let mut drafts = DRAFTS.lock().unwrap_or_else(PoisonError::into_inner);
        for (r, _) in &judged {
            match &r.draft {
                Draft::Text(t) => {
                    drafts.insert(r.sid.clone(), t.clone());
                }
                Draft::Clear => {
                    drafts.remove(&r.sid);
                }
                Draft::Unread => {}
            }
        }
        drafts.retain(|sid, _| judged.iter().any(|(r, _)| r.sid == *sid));
    }
    for (r, judged) in judged {
        out.live.push(r.sid.clone());
        match (judged, r.cwd) {
            (Judged::Barred(_), _) => out.barred.push(r.sid),
            (Judged::Held(why), _) => out.held.push((r.sid, why)),
            (Judged::Seat, Some(cwd)) => out.seated.push(Seated {
                seat: ask::Seat {
                    held: held_under(holders, &cwd),
                    sid: r.sid,
                    at_break: r.at_break,
                },
                cwd,
            }),
            (Judged::Seat | Judged::Unseated, _) => {}
        }
    }
    out
}

/// What the seat rule says of this process's session `sid` on volume `key` (its spelling) now,
/// read as the look reads it ([`judge`]), so a session the look would not seat by a lasting fact
/// is never typed its line, and one it holds is neither claimed nor typed it.
/// [`Judged::Unseated`]: it is not this process's.
fn judge_at(store: &Store, sid: &str, key: &str) -> Judged {
    judged(read_sessions(store, &[sid.to_owned()], Some(sid)), key)
        .into_iter()
        .map(|(_, judged)| judged)
        .next()
        .unwrap_or(Judged::Unseated)
}

/// The checkout `cwd` works in, of those `holders` serve: the deepest one it sits under, so a
/// worktree nested in another checkout is its own.
fn checkout_of<'a>(holders: &'a [Hog], cwd: &Path) -> Option<&'a Path> {
    holders
        .iter()
        .filter_map(|h| h.serves.as_deref())
        .filter(|c| cwd.starts_with(c))
        .max_by_key(|c| c.components().count())
}

/// What `holders` hold for the checkout `cwd` works in ([`checkout_of`]).
fn held_under(holders: &[Hog], cwd: &Path) -> u64 {
    let Some(checkout) = checkout_of(holders, cwd) else {
        return 0;
    };
    holders
        .iter()
        .filter(|h| h.serves.as_deref() == Some(checkout))
        .fold(0u64, |a, h| a.saturating_add(h.bytes))
}

/// A line this process typed.
#[derive(Debug, Clone)]
struct Typed {
    sid: String,
    /// Unix ms.
    at_ms: i64,
    /// The volume's free bytes then.
    free: u64,
}

/// The last line this process typed, by volume. A line whose record could not be written to
/// the lease (a full disk) is never typed twice, and the next look records it
/// ([`ask::typed_unrecorded`]).
static TYPED: Mutex<BTreeMap<String, Typed>> = Mutex::new(BTreeMap::new());

/// The last line this process typed on volume `key`.
fn typed_last(key: &str) -> Option<Typed> {
    TYPED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key)
        .cloned()
}

/// Whether this process typed session `sid` a line on volume `key` within the cooldown.
fn typed_here(key: &str, sid: &str, now: i64) -> bool {
    typed_last(key).is_some_and(|t| {
        t.sid == sid && (0..ask::COOLDOWN_MS).contains(&now.saturating_sub(t.at_ms))
    })
}

/// What the act's step under the lease's lock came to.
enum Stepped {
    /// The lease handed the ask on.
    Released(ask::Said),
    /// The step's word: `typed`, `wait:<why>` or `gone`.
    Step(String),
}

/// The live act ([`super::Acts::disk_free`]): at session `sid`'s turn break, type `owed`'s line
/// when the lease still names it, or release it when the session may not be asked (`unfit`, the
/// worker's finding, or what else the seat rule bars it by, [`judge_at`], so a session the look
/// would not seat by a lasting fact is never typed its line, nor is one the loop holds for
/// someone else for good: its `exchange` or `managed`, which the worker hands in as `unfit`). A
/// line `held` at this turn break for a while (the loop's word, `person`: a person's grace or
/// another driver's hand, the occupancy door's guard; or what the seat rule holds it by,
/// [`held_of`]: the owner's hold, a draft standing in its composer) is not claimed or typed and
/// stays owed, `wait:<held>`, until the look gives it back to the episode. The claim, the typing
/// and its record hold the lease's lock as one step ([`ask::at_break`]), so no other host hands
/// the line on while it is typed.
/// The step: `typed`, `wait:<why>` (tried again at its next turn break), `released:<why>`, or
/// `gone` (the lease names it no more).
pub(super) fn act(
    store: &Store,
    sock: &str,
    sid: &str,
    owed: &crate::disk_board::AskOwed,
    unfit: Option<&'static str>,
    held: Option<&'static str>,
) -> String {
    let Some(state) = harness_state() else {
        return "wait:no-state".to_owned();
    };
    let host = ask::HostId::this();
    let now = now_ms();
    let (free, level) = volume_on(&owed.key);
    let (unfit, held) = guards(judge_at(store, sid, &owed.key), unfit, held);
    let token = super::harness_drive_token();
    let at = AtBreak {
        state: &state,
        host: &host,
        sid,
        owed,
        now,
        unfit,
        held,
        free,
        level,
    };
    let step = at.step(
        || disk::type_resume(sock, token.as_ref(), sid, &owed.text),
        || free_now(&owed.key),
    );
    if step == "typed" {
        super::log_label(store, sid, aterm_agent::harness::acts::DISK_FREE_LABEL);
    }
    aterm_agent::harness::cli::disk_ask_journal(
        &state,
        &[ask::step_row(now / 1000, sid, &owed.key, &step, &host)],
    );
    step
}

/// What bars and what holds session `sid`'s line at its turn break ([`act`]): the worker's
/// `unfit` and `held`, joined with the seat rule's reading of the session now (`judged`,
/// [`judge_at`]): one it bars is released, and one it holds (a wall, a box, the owner's hold, a
/// draft that stood a look) waits, `wait:<why>`, unclaimed, so its claim does not stretch its time.
fn guards(
    judged: Judged,
    unfit: Option<&'static str>,
    held: Option<&'static str>,
) -> (Option<&'static str>, Option<&'static str>) {
    match judged {
        Judged::Barred(why) => (unfit.or(Some(why)), held),
        Judged::Held(why) => (unfit, held.or(Some(why.word()))),
        Judged::Seat | Judged::Unseated => (unfit, held),
    }
}

/// One turn break's step for the line [`Self::owed`] holds for session [`Self::sid`]
/// ([`act`]), as this host reads it.
struct AtBreak<'a> {
    /// The harness state directory: the lease's.
    state: &'a Path,
    host: &'a ask::HostId,
    sid: &'a str,
    owed: &'a crate::disk_board::AskOwed,
    /// Unix ms.
    now: i64,
    /// Why the session may not be asked ([`act`]).
    unfit: Option<&'static str>,
    /// What holds the line at this turn break ([`act`]).
    held: Option<&'static str>,
    /// The free bytes and the level the board last read on the volume ([`volume_on`]).
    free: u64,
    level: Level,
}

impl AtBreak<'_> {
    /// The step's word: a line already typed here is `gone`; one a guard holds waits, untyped
    /// and unclaimed; otherwise the lease releases or claims it, and a claimed one is typed
    /// (`type_line`, the fenced write's word back). A line that went is recorded with the free
    /// bytes `measure` reads as it went ([`free_now`]), or the board's where it reads none, so
    /// the rise the asked session must make is measured from the volume it was asked about,
    /// never from a look a minute older.
    fn step(
        &self,
        type_line: impl FnOnce() -> String,
        measure: impl FnOnce() -> Option<u64>,
    ) -> String {
        let key = &self.owed.key;
        if self.unfit.is_none() && typed_here(key, self.sid, self.now) {
            return "gone".to_owned();
        }
        if self.unfit.is_none()
            && let Some(why) = self.held
        {
            return format!("wait:{why}");
        }
        let stepped = ask::transact(self.state, &ask::LOCK_LADDER, |l| {
            match ask::at_break(l, key, self.host, self.sid, self.now, self.unfit.is_some()) {
                ask::Break::Released(said) => Stepped::Released(said),
                ask::Break::Gone => Stepped::Step("gone".to_owned()),
                ask::Break::Yours => {
                    let step = type_line();
                    if step == "typed" {
                        let at = now_ms();
                        let free = measure().unwrap_or(self.free);
                        TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
                            key.clone(),
                            Typed {
                                sid: self.sid.to_owned(),
                                at_ms: at,
                                free,
                            },
                        );
                        ask::typed(l, key, self.host, self.sid, at, free);
                    }
                    Stepped::Step(step)
                }
            }
        });
        match stepped {
            Ok(Some(Stepped::Released(said))) => {
                journal(
                    self.state, self.now, key, &said, self.level, self.free, self.host,
                );
                format!("released:{}", self.unfit.unwrap_or("unfit"))
            }
            Ok(Some(Stepped::Step(step))) => step,
            Ok(None) => "wait:lease-held".to_owned(),
            Err(e) if self.unfit.is_none() && typed_here(key, self.sid, now_ms()) => {
                aterm_log::warn!(
                    "harness @{}: {} typed, but the lease could not record it ({e}); the next \
                     look records it",
                    self.sid,
                    ask::RULE_DISK_FREE
                );
                "typed".to_owned()
            }
            Err(_) => "wait:lease-unwritten".to_owned(),
        }
    }
}

/// The free bytes of the volume the lease keys `key`, read now: the home volume's, the one the
/// ask covers; `None` where it cannot be read or is no longer that volume.
fn free_now(key: &str) -> Option<u64> {
    let home = aterm_primer::home_dir()?;
    let reading = disk::volume_of(&home, &aterm_sysprobe::volumes::table()).ok()?;
    (reading.key.spelled() == key).then_some(reading.free)
}

/// The free bytes and the level the board last read on the volume the lease keys `key`.
fn volume_on(key: &str) -> (u64, Level) {
    let b = crate::disk_board::SHELF.board_lock();
    b.volumes
        .iter()
        .find(|r| r.reading.key.spelled() == key)
        .map_or((0, Level::Unknown), |r| (r.reading.free, r.level))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why the seat rule bars this process's session `sid` on volume `key` now ([`judge_at`]).
    /// `None`: it may be asked, or it is not this process's.
    fn bar_at(store: &Store, sid: &str, key: &str) -> Option<&'static str> {
        match judge_at(store, sid, key) {
            Judged::Barred(why) => Some(why),
            Judged::Seat | Judged::Unseated | Judged::Held(_) => None,
        }
    }

    fn rows(state: &Path) -> Vec<String> {
        std::fs::read_to_string(state.join(aterm_agent::harness::cli::DISK_LEDGER))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_standing_word_is_journalled_once_and_every_move_each_time() {
        let state = aterm_tempfile::TempDir::new().expect("owned disk journal");
        let host = ask::HostId {
            pid: 1,
            start: String::new(),
        };
        let key = "dev:journal-once";
        let at = |said: &ask::Said| journal(state.path(), 0, key, said, Level::Short, 0, &host);
        at(&ask::Said::Quiet);
        assert!(
            rows(state.path()).is_empty(),
            "nothing to say while all is well"
        );
        at(&ask::Said::Freeing);
        at(&ask::Said::Freeing);
        at(&ask::Said::Opened {
            done: ask::Done::Spent,
        });
        at(&ask::Said::Opened {
            done: ask::Done::Spent,
        });
        at(&ask::Said::Settled);
        at(&ask::Said::Settled);
        at(&ask::Said::Quiet);
        let words: Vec<String> = rows(state.path())
            .iter()
            .filter_map(|r| {
                let v: aterm_json::Value = aterm_json::from_str(r).ok()?;
                Some(v.get("said")?.as_str()?.to_owned())
            })
            .collect();
        assert_eq!(words, ["freeing", "opened", "opened", "settled", "quiet"]);
    }

    #[test]
    fn the_seat_rule_seats_a_supervised_claude_on_the_volume_and_bars_only_by_lasting_facts() {
        let seat = Facts {
            supervised: true,
            program: Some(Program::Claude),
            task: None,
            unfit: None,
            held: None,
            on_volume: Some(true),
        };
        assert_eq!(
            judge(&seat),
            Judged::Seat,
            "no task record: the worker's finding decides"
        );
        for (facts, why) in [
            (
                Facts {
                    task: Some(TaskState::Working),
                    ..seat
                },
                "managed",
            ),
            (
                Facts {
                    task: Some(TaskState::Stalled),
                    supervised: false,
                    ..seat
                },
                "managed",
            ),
            (
                Facts {
                    task: Some(TaskState::Done),
                    ..seat
                },
                "task-done",
            ),
            (
                Facts {
                    task: Some(TaskState::Exited),
                    supervised: false,
                    program: None,
                    on_volume: None,
                    ..seat
                },
                "task-exited",
            ),
            (
                Facts {
                    unfit: Some("finished"),
                    ..seat
                },
                "finished",
            ),
            (
                Facts {
                    program: Some(Program::Codex),
                    ..seat
                },
                "not-claude",
            ),
            (
                Facts {
                    on_volume: Some(false),
                    ..seat
                },
                "off-volume",
            ),
            (
                Facts {
                    task: Some(TaskState::Done),
                    held: Some(ask::HeldBy::Limit),
                    ..seat
                },
                "task-done",
            ),
            (
                Facts {
                    on_volume: Some(false),
                    held: Some(ask::HeldBy::Box),
                    ..seat
                },
                "off-volume",
            ),
        ] {
            assert_eq!(judge(&facts), Judged::Barred(why), "{facts:?}");
        }
        for why in [ask::HeldBy::Limit, ask::HeldBy::Box] {
            assert_eq!(
                judge(&Facts {
                    held: Some(why),
                    ..seat
                }),
                Judged::Held(why),
                "a seat but for what passes: held, not barred"
            );
        }
        for facts in [
            Facts {
                supervised: false,
                ..seat
            },
            Facts {
                program: None,
                ..seat
            },
            Facts {
                on_volume: None,
                ..seat
            },
            Facts {
                on_volume: None,
                held: Some(ask::HeldBy::Limit),
                ..seat
            },
            Facts {
                supervised: false,
                held: Some(ask::HeldBy::Box),
                ..seat
            },
        ] {
            assert_eq!(
                judge(&facts),
                Judged::Unseated,
                "nothing that lasts: an owed line stays owed while supervised: {facts:?}"
            );
        }
    }

    /// A session at a usage-limit wall its loop has open, before its reset and the loop's grace,
    /// or at a box its loop escalated to its owner, cannot take a line before its delivery window
    /// ends: the look holds it out of its seats. A seat rule that seated such a tab elected it,
    /// often first (its checkout the largest), and the line waited out its window and was handed
    /// on as undelivered, spending an election with nothing typed.
    #[test]
    fn a_session_at_an_unspent_wall_or_an_escalated_box_is_held() {
        use crate::disk_board::Wall;
        let wall = |until_s| Wall { until_s };
        let now = 1_000_000;
        let held = |word, escalated, wall: Option<&Wall>| {
            held_of(word, escalated, wall, false, false, now)
        };
        assert_eq!(
            held("limited", false, Some(&wall(Some(now + 1)))),
            Some(ask::HeldBy::Limit)
        );
        assert_eq!(
            held("limited", false, Some(&wall(None))),
            Some(ask::HeldBy::Limit),
            "no reset known: held until the loop closes the wall"
        );
        assert_eq!(
            held("limited", false, Some(&wall(Some(now)))),
            None,
            "spent: the loop rests there, and the line can go"
        );
        assert_eq!(
            held("limited", false, None),
            None,
            "no wall the loop has open"
        );
        assert_eq!(
            held("idle", false, Some(&wall(None))),
            None,
            "past the wall: its agent no longer reads limited"
        );
        for word in ["prompt", "question"] {
            assert_eq!(held(word, true, None), Some(ask::HeldBy::Box), "{word}");
            assert_eq!(
                held(word, false, None),
                None,
                "{word}: a box a rule answers is no person's to wait on"
            );
        }
        assert_eq!(held("busy", true, None), None);
    }

    /// A session the owner holds (or a person took over: `status hold=1`), or whose composer
    /// holds a draft that stands at a rest, keeps to a turn break, and the fenced write refuses
    /// every try there (`wait:held`, `wait:draft`): the look holds it, as a wall or a box. Read as
    /// fit, it was nominated before a working session in the same GiB (it rests at a turn break),
    /// elected, refused at every step of the wait for the delivery window and a grace, and handed
    /// on as undelivered, spending an election with no line typed.
    #[test]
    fn a_held_session_or_one_whose_draft_stands_at_a_rest_is_held() {
        let now = 1_000_000;
        for word in ["idle", "busy", "prompt", "limited"] {
            assert_eq!(
                held_of(word, false, None, true, false, now),
                Some(ask::HeldBy::Hold),
                "{word}: the hold refuses any typed line"
            );
        }
        assert_eq!(
            held_of("idle", false, None, false, true, now),
            Some(ask::HeldBy::Draft)
        );
        assert_eq!(
            held_of("busy", false, None, false, true, now),
            None,
            "a draft queued while it works goes with its turn"
        );
        assert_eq!(held_of("idle", false, None, false, false, now), None);
        let seat = Facts {
            supervised: true,
            program: Some(Program::Claude),
            task: None,
            unfit: None,
            held: None,
            on_volume: Some(true),
        };
        for why in [ask::HeldBy::Hold, ask::HeldBy::Draft] {
            assert_eq!(
                judge(&Facts {
                    held: Some(why),
                    ..seat
                }),
                Judged::Held(why),
                "neither seated nor barred"
            );
        }
        // A draft stands when the look before read the same typed text there.
        let text = Draft::Text("fix the flaky test".to_owned());
        assert!(text.stands(Some("fix the flaky test")));
        assert!(!text.stands(None), "first read: it may be a person typing");
        assert!(!text.stands(Some("fix the")), "still being typed");
        assert!(!Draft::Clear.stands(Some("fix the flaky test")));
        assert!(!Draft::Unread.stands(Some("fix the flaky test")));
    }

    /// The composer as the fenced write reads it: typed text is a draft; an empty composer, or
    /// Claude Code's dim placeholder with the caret at its start, is none.
    #[test]
    fn a_draft_is_read_off_the_composer_as_the_fenced_write_reads_it() {
        let rule = "\u{2500}".repeat(60);
        let screen = |line: &str| vec![rule.clone(), format!("\u{276f} {line}"), rule.clone()];
        assert_eq!(
            draft_in(&screen("fix the flaky test"), (1, 20), |_| false),
            Draft::Text("fix the flaky test".to_owned())
        );
        assert_eq!(draft_in(&screen(""), (1, 2), |_| false), Draft::Clear);
        assert_eq!(
            draft_in(&screen("Try \"refactor this\""), (1, 2), |row| row == 1),
            Draft::Clear,
            "the dim placeholder"
        );
        assert_eq!(
            draft_in(&screen("Try \"refactor this\""), (1, 2), |_| false),
            Draft::Text("Try \"refactor this\"".to_owned()),
            "the same words typed"
        );
        assert_eq!(
            draft_in(&["$ ls".to_owned()], (0, 4), |_| false),
            Draft::Clear,
            "no composer drawn"
        );
    }

    /// Through the look: the owner's hold reads on the session at once, and its composer off its
    /// own screen, a look's own rule keeping what it held for the next look ([`DRAFTS`]), so the
    /// second look to read the same typed text reads it standing; the take-over's rule before the
    /// pass keeps nothing, and a cleared composer is forgotten.
    #[test]
    fn a_look_reads_the_owners_hold_and_a_draft_that_stands_since_its_last_look() {
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_300);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let sids = [sid.clone()];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        let key = "fsid:9300";
        let draw = |line: &str| {
            let rule = "\u{2500}".repeat(60);
            let col = line.chars().count() + 3;
            h.term
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process(
                    format!("\x1b[2J\x1b[H{rule}\r\n\u{276f} {line}\r\n{rule}\x1b[2;{col}H")
                        .as_bytes(),
                );
        };
        let read = || {
            let r = read_sessions(&store, &sids, Some(&sid))
                .pop()
                .expect("the session");
            let last = DRAFTS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&sid)
                .cloned();
            let stands = r.draft.stands(last.as_deref());
            (r.halted, r.draft, stands)
        };
        draw("");
        let clear = read();
        assert!(crate::fabric::apply_hold_for_test(
            &*h.ctx,
            Some(crate::fabric::Hold {
                reason: "-".into(),
                origin: "local".into(),
            })
        ));
        let halted = read().0;
        assert!(crate::fabric::apply_hold_for_test(&*h.ctx, None));
        draw("fix the flaky test");
        let first = read();
        let _ = rule(&store, &sids, key, &[], false);
        let before_pass = read();
        let _ = rule(&store, &sids, key, &[], true);
        let second = read();
        draw("");
        let _ = rule(&store, &sids, key, &[], true);
        let forgotten = DRAFTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&sid)
            .is_none();
        assert_eq!(
            clear,
            (false, Draft::Clear, false),
            "CONTROL: nothing holds it"
        );
        assert!(halted, "the owner's hold: status hold=1");
        let text = Draft::Text("fix the flaky test".to_owned());
        assert_eq!(
            first,
            (false, text.clone(), false),
            "first read: maybe a person typing"
        );
        assert_eq!(
            before_pass,
            (false, text.clone(), false),
            "the take-over keeps nothing"
        );
        assert_eq!(second, (false, text, true), "it stood since the last look");
        assert!(forgotten, "cleared: forgotten");
    }

    /// Through the seat rule itself ([`rule`], [`judge_at`]) and the turn break ([`guards`],
    /// [`AtBreak`]): a fit Claude Code tab resting at a turn break on the volume is seated; under
    /// the owner's hold (`status hold=1`) it is held (`hold`), not seated, and a line owed it waits
    /// there (`wait:hold`), unclaimed; lifted, it is seated again. With a draft typed in its prompt
    /// it is seated at the look that first reads the draft (a person may be typing), held
    /// (`draft`) from the next look that reads the same text, its owed line waiting the same way,
    /// and seated again once the prompt is cleared. A seat rule that left the hold or the draft out
    /// seated such a tab, which, at rest, outranked a working one in the same GiB, was elected, and
    /// was refused at every turn break until the delivery window handed it on as undelivered.
    #[cfg(unix)]
    #[test]
    fn the_seat_rule_holds_a_tab_on_hold_or_with_a_standing_draft_and_its_line_waits_unclaimed() {
        use aterm_sysprobe::volumes::{Reading, VolumeKey};
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_400);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let sids = [sid.clone()];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_agent(
                "idle",
                None,
                None,
                Some(Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        let draw = |line: &str| {
            let rule = "\u{2500}".repeat(60);
            let col = line.chars().count() + 3;
            h.term
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .process(
                    format!(
                        "\x1b]7;file://localhost/u/held-tab\x07\x1b[2J\x1b[H{rule}\r\n\u{276f} \
                         {line}\r\n{rule}\x1b[2;{col}H"
                    )
                    .as_bytes(),
                );
        };
        // The board places the tab on the volume asked about, as a pass does.
        let volume = VolumeKey::Fsid(9_400);
        let key = volume.spelled();
        {
            let mut b = crate::disk_board::SHELF.board_lock();
            b.volumes.push(crate::disk_board::VolumeRow {
                reading: Reading {
                    mount: PathBuf::from("/u"),
                    key: volume.clone(),
                    total: 2000 * GIB,
                    free: 10 * GIB,
                    files_free: Some(1 << 30),
                    dev: 9_400,
                    read_only: false,
                },
                thresholds: disk::Config::default().thresholds(Some(2000 * GIB)),
                level: Level::Short,
                room: None,
                as_of: std::time::Instant::now(),
                as_of_unix: 0,
                reclaimed_here: true,
            });
            let at = b.volumes.len() - 1;
            b.sessions.insert(h.local_id, at);
        }
        let read = || {
            let r = rule(&store, &sids, &key, &[], true);
            (
                r.seated
                    .iter()
                    .map(|s| s.seat.sid.clone())
                    .collect::<Vec<_>>(),
                r.held,
            )
        };
        let hold = |on: bool| {
            assert!(crate::fabric::apply_hold_for_test(
                &*h.ctx,
                on.then(|| crate::fabric::Hold {
                    reason: "-".into(),
                    origin: "local".into(),
                })
            ));
        };
        draw("");
        let clear = read();
        hold(true);
        let on_hold = read();
        let judged_hold = judge_at(&store, &sid, &key);
        hold(false);
        let lifted = read();
        draw("refactor the parser");
        let first = read();
        let second = read();
        let judged_draft = judge_at(&store, &sid, &key);
        draw("");
        let cleared = read();
        {
            let mut b = crate::disk_board::SHELF.board_lock();
            b.sessions.remove(&h.local_id);
            b.volumes.retain(|r| r.reading.key != volume);
        }
        DRAFTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&sid);
        let seated = (vec![sid.clone()], Vec::new());
        let held = |why| (Vec::new(), vec![(sid.clone(), why)]);
        assert_eq!(clear, seated, "CONTROL: a fit tab at rest is seated");
        assert_eq!(on_hold, held(ask::HeldBy::Hold), "the owner's hold");
        assert_eq!(lifted, seated, "the hold lifted");
        assert_eq!(first, seated, "a draft read once: a person may be typing");
        assert_eq!(second, held(ask::HeldBy::Draft), "the same draft a look on");
        assert_eq!(cleared, seated, "the prompt cleared");
        assert_eq!(judged_hold, Judged::Held(ask::HeldBy::Hold));
        assert_eq!(judged_draft, Judged::Held(ask::HeldBy::Draft));
        // A line owed it waits at its turn break, unclaimed, so its time is not stretched.
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 9,
            start: "start-9".to_owned(),
        };
        let now = now_ms();
        owed_at(state.path(), &key, &host, &sid, now);
        let before = ask::read(state.path());
        let owed = crate::disk_board::AskOwed {
            key: key.clone(),
            text: "the line".to_owned(),
        };
        for (judged, why) in [(judged_hold, "hold"), (judged_draft, "draft")] {
            let (unfit, held) = guards(judged, None, None);
            let at = AtBreak {
                state: state.path(),
                host: &host,
                sid: &sid,
                owed: &owed,
                now,
                unfit,
                held,
                free: 10 * GIB,
                level: Level::Short,
            };
            assert_eq!(
                at.step(
                    || panic!("typed a tab {why} holds"),
                    || panic!("measured at a tab {why} holds")
                ),
                format!("wait:{why}")
            );
        }
        assert_eq!(
            ask::read(state.path()),
            before,
            "unclaimed, and owed until the look gives it back to the episode"
        );
        assert_eq!(
            guards(Judged::Seat, None, None),
            (None, None),
            "CONTROL: a seat is typed"
        );
    }

    /// The composer's first glyph is read past a caret the program draws over it, as the fenced
    /// write reads it: Claude Code draws its suggestion's first glyph inverse and the rest dim, and
    /// a typed draft's first glyph inverse and the rest plain.
    #[test]
    fn the_composers_first_glyph_is_read_past_a_drawn_caret() {
        use aterm_core::grid::CellFlags;
        let at = |cells: Vec<(usize, CellFlags)>| {
            move |col: usize| cells.iter().find(|(c, _)| *c == col).map(|(_, f)| *f)
        };
        let (dim, plain, inverse) = (CellFlags::DIM, CellFlags::empty(), CellFlags::INVERSE);
        assert!(glyph_dim(at(vec![(2, dim)])), "dim at the caret");
        assert!(
            glyph_dim(at(vec![(2, inverse), (3, dim)])),
            "a suggestion under a drawn caret"
        );
        assert!(
            !glyph_dim(at(vec![(2, inverse), (3, plain)])),
            "a typed draft under a drawn caret"
        );
        assert!(
            glyph_dim(at(vec![
                (2, inverse.union(CellFlags::WIDE)),
                (3, plain),
                (4, dim),
            ])),
            "past a wide glyph"
        );
        assert!(!glyph_dim(at(vec![(2, plain)])));
        assert!(!glyph_dim(at(Vec::new())), "no cell");
    }

    /// A look that runs the episode out raises the owner's `disk.low` itself only when it reads
    /// the volume short against its own floor. A look that read it at or above its floor (an
    /// episode another host's higher floor held open) raised "the home volume is still short",
    /// and the next look, finding it plenty, cleared it: a false alert kept as a message record.
    #[test]
    fn a_look_that_runs_the_episode_out_raises_disk_low_only_reading_the_volume_short() {
        let out = ask::Said::Out {
            last: Some(ask::Miss::NothingFreed),
            asked: true,
        };
        let spent = Some(ask::Done::Spent);
        let floor = 32 << 30;
        assert!(out_raises_low(&out, Level::Short, floor, spent));
        assert!(out_raises_low(&out, Level::Emergency, floor, spent));
        for level in [Level::Low, Level::Ok, Level::Unknown] {
            assert!(
                !out_raises_low(&out, level, floor, spent),
                "{level:?}: not read short against this host's floor"
            );
        }
        assert!(
            !out_raises_low(&out, Level::Short, 0, spent),
            "the floor at 0"
        );
        assert!(
            !out_raises_low(&out, Level::Short, floor, Some(ask::Done::CleanupOff)),
            "removal off: its own line"
        );
        assert!(!out_raises_low(
            &ask::Said::Spent,
            Level::Short,
            floor,
            spent
        ));
    }

    /// The look reads a box as escalated by the supervisor's own attention entry on the session,
    /// which its loop writes as it asks the owner, and not by the badge of a usage limit it
    /// raised, which a wall the board holds says instead.
    #[test]
    fn a_look_reads_a_box_its_loop_escalated_and_not_a_limits_badge() {
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_200);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .publish_agent(
                "prompt",
                None,
                None,
                Some(Program::Claude),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        let read = |badge: Option<&str>| {
            h.ctx
                .meta
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .set_attention_owned(
                    aterm_agent::supervise::ATTENTION_OWNER,
                    badge.map(str::to_owned),
                )
                .expect("a slot");
            let r = read_sessions(&store, std::slice::from_ref(&sid), Some(&sid))
                .pop()
                .expect("the session");
            (r.word, r.escalated)
        };
        assert_eq!(read(None), ("prompt", false), "CONTROL: nobody asked");
        assert_eq!(read(Some("approve: rm -rf target")), ("prompt", true));
        let limit = format!("{} resets 3pm", aterm_agent::supervise::ATTENTION_PREFIX);
        assert_eq!(read(Some(&limit)), ("prompt", false), "a limit's badge");
    }

    #[test]
    fn a_look_and_a_turn_break_bar_a_done_an_exited_and_an_unfit_session_alike() {
        use crate::task_record::{TaskMark, mark_session_task};
        let mut sessions = crate::session_store::SessionStore::default();
        let handles: Vec<_> = (0..5u64)
            .map(|local| {
                let h = crate::session_store::test_handle(9_000 + local);
                h.ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .set_agent_reader_program_for_test(Some(Program::Claude));
                sessions.register(h.clone());
                h
            })
            .collect();
        let sid = |i: usize| handles[i].sid.as_str().to_owned();
        let mark = |i: usize, state: TaskState| {
            mark_session_task(&handles[i].ctx, handles[i].local_id, &TaskMark::new(state))
                .expect("unfenced");
        };
        // 0: its task landed; 1: its agent exited, so it left the supervised roster; 2: its
        // worker found it unfit; 3: a manager's worker, its task working, its turn break not yet
        // found; 4: no task record, not supervised.
        mark(0, TaskState::Done);
        mark(1, TaskState::Exited);
        mark(3, TaskState::Working);
        let sids = [sid(0), sid(2), sid(3)];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        crate::disk_board::SHELF.board_lock().unfit.insert(
            sid(2),
            crate::disk_board::Unfit {
                own: None,
                theirs: Some("exchange"),
            },
        );
        let key = "fsid:9000";
        let found = rule(&store, &sids, key, &[], false);
        let bars: Vec<Option<&'static str>> =
            (0..5).map(|i| bar_at(&store, &sid(i), key)).collect();
        crate::disk_board::SHELF.board_lock().unfit.remove(&sid(2));
        let mut barred = found.barred.clone();
        barred.sort();
        let mut want = vec![sid(0), sid(1), sid(2), sid(3)];
        want.sort();
        assert_eq!(barred, want, "what the look releases an owed line of");
        assert!(
            found.seated.is_empty(),
            "no directory on the volume was read"
        );
        for i in 0..5 {
            assert!(found.live.contains(&sid(i)), "every live session: {i}");
        }
        assert_eq!(
            bars,
            [
                Some("task-done"),
                Some("task-exited"),
                Some("exchange"),
                Some("managed"),
                None
            ],
            "the turn break bars what the look bars"
        );
        assert_eq!(bar_at(&store, "s-not-here", key), None);
    }

    #[test]
    fn a_session_holds_what_its_own_checkout_holds_and_a_line_typed_here_is_never_typed_twice() {
        let hog = |path: &str, gib: u64, serves: Option<&str>| Hog {
            path: PathBuf::from(path),
            bytes: gib << 30,
            bytes_partial: false,
            why: String::new(),
            serves: serves.map(PathBuf::from),
        };
        let holders = [
            hog("/c/aterm-build", 40, Some("/u/aterm")),
            hog("/u/aterm/target", 2, Some("/u/aterm")),
            hog("/u/trust/build", 90, Some("/u/trust")),
            hog("/u/loose/target", 7, None),
            hog(
                "/u/aterm/.claude/worktrees/w/target",
                3,
                Some("/u/aterm/.claude/worktrees/w"),
            ),
        ];
        assert_eq!(
            held_under(&holders, Path::new("/u/aterm/crates/x")),
            42 << 30
        );
        assert_eq!(held_under(&holders, Path::new("/u/atermx")), 0);
        assert_eq!(held_under(&holders, Path::new("/u/loose")), 0);
        // A worktree nested in a checkout is its own checkout, not the one around it.
        let nested = Path::new("/u/aterm/.claude/worktrees/w/crates");
        assert_eq!(held_under(&holders, nested), 3 << 30);
        assert_eq!(
            checkout_of(&holders, nested),
            Some(Path::new("/u/aterm/.claude/worktrees/w"))
        );
        assert_eq!(
            checkout_of(&holders, Path::new("/u/aterm")),
            Some(Path::new("/u/aterm"))
        );
        let key = "dev:typed-here";
        assert!(!typed_here(key, "s-a", 1_000));
        TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
            key.to_owned(),
            Typed {
                sid: "s-a".to_owned(),
                at_ms: 1_000,
                free: 0,
            },
        );
        assert!(typed_here(key, "s-a", 1_000 + ask::COOLDOWN_MS - 1));
        assert!(!typed_here(key, "s-b", 2_000));
        assert!(!typed_here(key, "s-a", 1_000 + ask::COOLDOWN_MS));
    }

    /// A lease in `state` that owes host `host`'s session `sid` its line on volume `key`, elected
    /// at `now`.
    fn owed_at(state: &Path, key: &str, host: &ask::HostId, sid: &str, now: i64) {
        let pick = ask::Pick {
            host: host.clone(),
            sid: sid.to_owned(),
        };
        let lease = ask::Lease {
            schema: 0,
            volumes: vec![ask::Entry {
                key: key.to_owned(),
                episode_ms: Some(now - ask::NOMINATE_MS),
                done: Some(ask::Done::Spent),
                phase: ask::Phase::Owed {
                    pick: pick.clone(),
                    elected_ms: now,
                    until_ms: now + ask::DELIVER_MS,
                    orphan_ms: None,
                },
                picks: vec![pick],
                ..ask::Entry::default()
            }],
        };
        ask::transact(state, &ask::LOCK_LADDER, |l| *l = lease)
            .expect("the lease is written")
            .expect("nobody holds its lock");
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn a_typed_line_records_the_free_figure_read_as_it_went_not_the_boards() {
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 7,
            start: "start-7".to_owned(),
        };
        let now = now_ms();
        for (key, measured, recorded) in [
            ("dev:free-measured", Some(40 * GIB), 40 * GIB),
            ("dev:free-unmeasured", None, 10 * GIB),
        ] {
            owed_at(state.path(), key, &host, "s-fresh", now);
            let owed = crate::disk_board::AskOwed {
                key: key.to_owned(),
                text: "the line".to_owned(),
            };
            let at = AtBreak {
                state: state.path(),
                host: &host,
                sid: "s-fresh",
                owed: &owed,
                now,
                unfit: None,
                held: None,
                free: 10 * GIB,
                level: Level::Short,
            };
            assert_eq!(at.step(|| "typed".to_owned(), || measured), "typed");
            let lease = ask::read(state.path());
            assert!(
                matches!(
                    &lease.entry(key).expect("entry").phase,
                    ask::Phase::Asked { free_at, best_free, .. }
                        if *free_at == recorded && *best_free == recorded
                ),
                "{key}: the rise is measured from the line's own reading, the board's a fallback: \
                 {lease:?}"
            );
            assert_eq!(typed_last(key).map(|t| t.free), Some(recorded), "{key}");
        }
    }

    #[test]
    fn a_turn_break_the_loop_holds_types_nothing_and_claims_nothing() {
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let host = ask::HostId {
            pid: 8,
            start: "start-8".to_owned(),
        };
        let now = now_ms();
        let key = "dev:held-break";
        owed_at(state.path(), key, &host, "s-held", now);
        let before = ask::read(state.path());
        let owed = crate::disk_board::AskOwed {
            key: key.to_owned(),
            text: "the line".to_owned(),
        };
        let at = |unfit: Option<&'static str>, held: Option<&'static str>| AtBreak {
            state: state.path(),
            host: &host,
            sid: "s-held",
            owed: &owed,
            now,
            unfit,
            held,
            free: 10 * GIB,
            level: Level::Short,
        };
        for why in ["person", "exchange", "managed"] {
            assert_eq!(
                at(None, Some(why)).step(
                    || panic!("typed at a turn break {why} holds"),
                    || panic!("measured at a turn break {why} holds")
                ),
                format!("wait:{why}")
            );
        }
        assert_eq!(
            ask::read(state.path()),
            before,
            "unclaimed, so its time is not stretched, and still owed"
        );
        assert_eq!(
            at(Some("task-done"), Some("person"))
                .step(|| panic!("typed a barred session"), || None),
            "released:task-done",
            "a session the seat rule bars is released whatever holds its turn break"
        );
        assert!(
            !matches!(
                ask::read(state.path()).entry(key).map(|e| &e.phase),
                Some(ask::Phase::Owed { .. })
            ),
            "released"
        );
    }

    /// What a session's worker found of the session itself at its last turn break (its task
    /// finished, nobody gave it one) holds only until it works again, as a session that works was
    /// given something to do: a look while its agent reads busy seats it. Whose it is (the
    /// person's own exchange, a manager's wait) holds across its turns until its next turn break.
    /// A finding kept mid-turn barred a tab through the whole of the turn its first task started,
    /// so an episode that opened then found nobody to ask, said only the owner could free the
    /// space, and stayed out for its cooldown.
    #[test]
    fn a_finding_of_the_session_itself_holds_until_it_works_and_whose_it_is_across_turns() {
        use crate::disk_board::Unfit;
        let own = |why| Unfit {
            own: Some(why),
            theirs: None,
        };
        for why in ["taskless", "finished", "autopilot-off"] {
            assert_eq!(
                own(why).bars(false),
                Some(why),
                "resting, or held at a wall"
            );
            assert_eq!(own(why).bars(true), None, "{why} while it works: seated");
        }
        let theirs = Unfit {
            own: None,
            theirs: Some("exchange"),
        };
        assert_eq!(theirs.bars(false), Some("exchange"));
        assert_eq!(theirs.bars(true), Some("exchange"), "theirs while it works");
        let both = Unfit {
            own: Some("taskless"),
            theirs: Some("managed"),
        };
        assert_eq!(both.bars(false), Some("taskless"));
        assert_eq!(both.bars(true), Some("managed"));
        assert_eq!(Unfit::default().bars(false), None);
        // Through the look: the same tab found taskless, at rest and then working.
        let mut sessions = crate::session_store::SessionStore::default();
        let h = crate::session_store::test_handle(9_100);
        sessions.register(h.clone());
        let sid = h.sid.as_str().to_owned();
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        let key = "fsid:9100";
        let reads = |word: &'static str, seq: u64| {
            h.ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .publish_agent(
                    word,
                    None,
                    None,
                    Some(Program::Claude),
                    crate::session_timeline::AgentStamp {
                        generation: crate::control::ScreenGen { epoch: 1, seq },
                        fp: seq,
                    },
                );
            bar_at(&store, &sid, key)
        };
        crate::disk_board::SHELF
            .board_lock()
            .unfit
            .insert(sid.clone(), own("taskless"));
        let resting = reads("idle", 1);
        let working = reads("busy", 2);
        crate::disk_board::SHELF.board_lock().unfit.remove(&sid);
        assert_eq!(working, None, "given a task since, it may be asked");
        assert_eq!(resting, Some("taskless"), "resting where it was found");
    }

    /// A look that reads the volume clear takes no lock only while the lease holds the last line
    /// this process typed on it ([`look_rests`]). A line whose record a full disk lost, and whose
    /// episode another host ended as the space came back, steps the lease at this process's next
    /// look, which records it, so its cooldown holds the next shortage shut. The look before
    /// rested on such a lease, as no episode was open, and the next shortage asked a second agent
    /// inside the cooldown.
    #[test]
    fn a_clear_look_steps_a_lease_that_lacks_this_processs_last_line_and_rests_once_it_holds_it() {
        let state = aterm_tempfile::TempDir::new().expect("owned lease dir");
        let key = "dev:rests-unrecorded";
        let host = ask::HostId {
            pid: 1,
            start: String::new(),
        };
        let at = 1_000_000;
        assert!(look_rests(state.path(), key, Level::Ok), "nothing to step");
        assert!(!look_rests(state.path(), key, Level::Short), "short");
        TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
            key.to_owned(),
            Typed {
                sid: "s-a".to_owned(),
                at_ms: at,
                free: 0,
            },
        );
        // An earlier line of the volume's, its episode ended: no episode open, the lease still
        // without the line this process typed since.
        ask::transact(state.path(), &ask::LOCK_LADDER, |l| {
            ask::typed(l, key, &host, "s-b", at - 60_000, 0);
        })
        .expect("written")
        .expect("locked");
        let lacks = look_rests(state.path(), key, Level::Ok);
        ask::transact(state.path(), &ask::LOCK_LADDER, |l| {
            ask::typed_unrecorded(l, key, &host, "s-a", at, 0)
        })
        .expect("written")
        .expect("locked");
        let holds = look_rests(state.path(), key, Level::Ok);
        TYPED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
        assert!(!lacks, "the lease lacks the line: the look steps it");
        assert!(holds, "recorded: the look rests");
        assert_eq!(
            ask::read(state.path())
                .entry(key)
                .and_then(|e| e.last_ask_ms),
            Some(at),
            "its cooldown runs from the lost line"
        );
    }
}
