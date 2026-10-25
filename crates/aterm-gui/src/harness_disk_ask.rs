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

/// After a look: step the home volume's lease with this process's sessions `sids` (the
/// supervised ones, Autopilot not Off), as [`Looked`] says the look went. The look judges the
/// volume short against this process's own floor ([`disk::Thresholds::short_below`]); the lease
/// holds an open episode to the floor it recorded ([`ask::Entry::floor`]), so a look here that
/// reads it clear of this process's floor steps the episode on until it clears that one too, and
/// records the floor it held it to, which the line and the owner's note then say.
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
    if level < Level::Short
        && ask::read(&state)
            .entry(&key)
            .is_none_or(|e| e.episode_ms.is_none())
    {
        // At or above the floor with no episode open: nothing to step, no lock to take.
        crate::disk_board::publish(|b| {
            b.asks.retain(|_, o| o.key != key);
            b.unfit.retain(|sid, _| sids.contains(sid));
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
    } = rule(store, sids, &key, &holders);
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
    let owner = entry.as_ref().and_then(ask::owner_line);
    *OWNER.lock().unwrap_or_else(PoisonError::into_inner) = owner.clone();
    // The pass's own line is `disk.low`; out of inodes or with removal off, the owner's line is
    // the one the look raises itself (`disk.full`, `disk.low-cleanup-off`), which says
    // [`owner_said`] at each look.
    if matches!(said, ask::Said::Out { .. })
        && thresholds.floor > 0
        && entry.as_ref().and_then(|e| e.done) == Some(ask::Done::Spent)
    {
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
    /// Why its worker found it unfit at its last turn break (`Board::unfit`).
    unfit: Option<&'static str>,
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
}

/// The seat rule: a supervised Claude Code (the line is written for its composer) working on the
/// volume, whose task is not done or exited ([`task_bar_of`]) and whose worker did not find it
/// unfit at its last turn break ([`super::WorkerIdle`]: its task finished or landed, it has none,
/// or its Autopilot is Off). The task record bars a session whether or not it is still
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
            | Program::Gemini
            | Program::Grok
            | Program::Kimi
            | Program::Generic,
        ) => return Judged::Barred("not-claude"),
        None => return Judged::Unseated,
    }
    match f.on_volume {
        Some(true) => Judged::Seat,
        Some(false) => Judged::Barred("off-volume"),
        None => Judged::Unseated,
    }
}

/// Why the seat rule bars a session by its task record: its task is done (it landed) or
/// exited. `None`: it has no task record, or its task goes on.
fn task_bar_of(state: Option<TaskState>) -> Option<&'static str> {
    match state? {
        TaskState::Done => Some("task-done"),
        TaskState::Exited => Some("task-exited"),
        TaskState::Working | TaskState::Stalled => None,
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
            let sid = h.sid.as_str().to_owned();
            let supervised = sids.contains(&sid);
            let cwd = supervised
                .then(|| match h.term.try_lock() {
                    Ok(t) => t
                        .native_working_directory()
                        .filter(|c| !c.is_empty())
                        .map(|c| PathBuf::from(c.into_owned())),
                    Err(_) => known
                        .iter()
                        .find(|(id, _)| *id == h.local_id)
                        .map(|(_, cwd)| cwd.clone()),
                })
                .flatten()
                .filter(|cwd| cwd.is_absolute());
            Read {
                sid,
                local: h.local_id,
                supervised,
                program,
                task,
                cwd,
                at_break: word == "idle",
            }
        })
        .collect()
}

/// The seat rule ([`judge`]) over `read` on volume `key` (its spelling), as the board places
/// each session and says what its worker found.
fn judged(read: Vec<Read>, key: &str) -> Vec<(Read, Judged)> {
    let b = crate::disk_board::SHELF.board_lock();
    read.into_iter()
        .map(|r| {
            let facts = Facts {
                supervised: r.supervised,
                program: r.program,
                task: r.task,
                unfit: b.unfit.get(&r.sid).copied(),
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
}

/// The seat rule over this process's live sessions on volume `key` (its spelling), `sids` the
/// supervised ones and `holders` what the pass left.
fn rule(store: &Store, sids: &[String], key: &str, holders: &[Hog]) -> Rule {
    let mut out = Rule {
        seated: Vec::new(),
        barred: Vec::new(),
        live: Vec::new(),
    };
    for (r, judged) in judged(read_sessions(store, sids, None), key) {
        out.live.push(r.sid.clone());
        match (judged, r.cwd) {
            (Judged::Barred(_), _) => out.barred.push(r.sid),
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

/// Why the seat rule bars this process's session `sid` on volume `key` (its spelling) now, read
/// as the look reads it ([`judge`]), so a session the look would not seat by a lasting fact is
/// never typed its line. `None`: it may be asked, or it is not this process's.
fn bar_at(store: &Store, sid: &str, key: &str) -> Option<&'static str> {
    judged(read_sessions(store, &[sid.to_owned()], Some(sid)), key)
        .into_iter()
        .find_map(|(_, judged)| match judged {
            Judged::Barred(why) => Some(why),
            Judged::Seat | Judged::Unseated => None,
        })
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
/// worker's finding, or what else the seat rule bars it by, [`bar_at`], so a session the look
/// would not seat by a lasting fact is never typed its line, nor is one the loop holds for
/// someone else for good: its `exchange` or `managed`, which the worker hands in as `unfit`). A
/// line `held` at this turn break for a while (the loop's word, `person`: a person's grace or
/// another driver's hand, the occupancy door's guard) is not typed and stays owed,
/// `wait:<held>`. The claim, the typing and its record hold the lease's
/// lock as one step ([`ask::at_break`]), so no other host hands the line on while it is typed.
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
    let unfit = unfit.or_else(|| bar_at(store, sid, &owed.key));
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
            task: Some(TaskState::Working),
            unfit: None,
            on_volume: Some(true),
        };
        assert_eq!(judge(&seat), Judged::Seat);
        assert_eq!(
            judge(&Facts { task: None, ..seat }),
            Judged::Seat,
            "no task record: the worker's finding decides"
        );
        assert_eq!(
            judge(&Facts {
                task: Some(TaskState::Stalled),
                ..seat
            }),
            Judged::Seat
        );
        for (facts, why) in [
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
        ] {
            assert_eq!(judge(&facts), Judged::Barred(why), "{facts:?}");
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
        ] {
            assert_eq!(
                judge(&facts),
                Judged::Unseated,
                "nothing that lasts: an owed line stays owed while supervised: {facts:?}"
            );
        }
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
        // worker found it unfit; 3 and 4: working, supervised or not.
        mark(0, TaskState::Done);
        mark(1, TaskState::Exited);
        for i in 2..5 {
            mark(i, TaskState::Working);
        }
        let sids = [sid(0), sid(2), sid(3)];
        let store: Store = std::sync::Arc::new(std::sync::RwLock::new(sessions));
        crate::disk_board::SHELF
            .board_lock()
            .unfit
            .insert(sid(2), "finished");
        let key = "fsid:9000";
        let found = rule(&store, &sids, key, &[]);
        let bars: Vec<Option<&'static str>> =
            (0..5).map(|i| bar_at(&store, &sid(i), key)).collect();
        crate::disk_board::SHELF.board_lock().unfit.remove(&sid(2));
        let mut barred = found.barred.clone();
        barred.sort();
        let mut want = vec![sid(0), sid(1), sid(2)];
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
                Some("finished"),
                None,
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
}
