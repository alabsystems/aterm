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
use aterm_sysprobe::volumes::VolumeKey;

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
/// supervised ones, Autopilot not Off), as [`Looked`] says the look went.
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
    let seated = seats(store, sids, &reading.key, &holders);
    let seats: Vec<ask::Seat> = seated.iter().map(|s| s.seat.clone()).collect();
    let live = live_sids(store);
    let alive = |h: &ask::HostId| h.alive();
    let look = ask::Look {
        now_ms: now,
        host: &host,
        key: &key,
        level,
        free: reading.free,
        done,
        seats: &seats,
        supervised: sids,
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
            floor: if level == Level::Emergency {
                thresholds.emergency
            } else {
                thresholds.floor
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
        note(HostNote::DiskLow {
            free_gib: reading.free >> 30,
            floor_gib: thresholds.floor >> 30,
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

/// This process's sessions among `sids` the ask could go to on volume `key`: a Claude Code
/// (the line is written for its composer), working on that volume, whose task is not done
/// and whose worker did not find it unfit at its last turn break ([`super::WorkerIdle`]: its
/// task finished or landed, or it has none); each with what `holders` hold for its checkout and
/// whether it rests at a turn break.
fn seats(store: &Store, sids: &[String], key: &VolumeKey, holders: &[Hog]) -> Vec<Seated> {
    use crate::cwd_native::ReportedCwd as _;
    let handles: Vec<_> = {
        let g = store.read().unwrap_or_else(PoisonError::into_inner);
        sids.iter()
            .filter_map(|sid| g.by_sid(&aterm_session::SessionId::new(sid)).cloned())
            .collect()
    };
    let known = super::DISK_ALL_CWDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let read: Vec<(String, u64, PathBuf, bool)> = handles
        .iter()
        .filter_map(|h| {
            let (program, word, done) = {
                let t = h
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let p = t.agent();
                let done = matches!(
                    t.task().map(|r| r.state),
                    Some(
                        crate::task_record::TaskState::Done | crate::task_record::TaskState::Exited
                    )
                );
                (agent_of(p), p.word, done)
            };
            match program? {
                Program::Claude => {}
                // The line is written for Claude Code's composer; another agent gets no seat
                // until it is checked against its own.
                Program::Codex
                | Program::Agy
                | Program::Muse
                | Program::Gemini
                | Program::Grok
                | Program::Kimi
                | Program::Generic => return None,
            }
            if done {
                return None;
            }
            let cwd = match h.term.try_lock() {
                Ok(t) => t
                    .native_working_directory()
                    .filter(|c| !c.is_empty())
                    .map(|c| PathBuf::from(c.into_owned())),
                Err(_) => known
                    .iter()
                    .find(|(id, _)| *id == h.local_id)
                    .map(|(_, cwd)| cwd.clone()),
            }
            .filter(|cwd| cwd.is_absolute())?;
            Some((h.sid.as_str().to_owned(), h.local_id, cwd, word == "idle"))
        })
        .collect();
    let b = crate::disk_board::SHELF.board_lock();
    read.into_iter()
        .filter(|(sid, local, cwd, _)| {
            !b.unfit.contains_key(sid)
                && b.row_of(*local, cwd.to_str())
                    .is_some_and(|r| &r.reading.key == key)
        })
        .map(|(sid, _, cwd, at_break)| Seated {
            seat: ask::Seat {
                held: held_under(holders, &cwd),
                sid,
                at_break,
            },
            cwd,
        })
        .collect()
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

/// Every live session of this process.
fn live_sids(store: &Store) -> Vec<String> {
    let g = store.read().unwrap_or_else(PoisonError::into_inner);
    g.live_handles()
        .map(|h| h.sid.as_str().to_owned())
        .collect()
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

/// The live act ([`super::Acts::disk_free`]): at session `sid`'s turn break, type `owed`'s line
/// when the lease still names it, or release it (`unfit`: why it may not be asked). The claim,
/// the typing and its record hold the lease's lock as one step, so no other host hands the
/// line on while it is typed. The step: `typed`, `wait:<why>` (tried again at its next turn
/// break), `released:<why>`, or `gone` (the lease names it no more).
pub(super) fn act(
    store: &Store,
    sock: &str,
    sid: &str,
    owed: &crate::disk_board::AskOwed,
    unfit: Option<&'static str>,
) -> String {
    let Some(state) = harness_state() else {
        return "wait:no-state".to_owned();
    };
    let host = ask::HostId::this();
    let now = now_ms();
    let (free, level) = volume_on(&owed.key);
    let step = if let Some(why) = unfit {
        match ask::transact(&state, &ask::LOCK_LADDER, |l| {
            ask::release(l, &owed.key, &host, sid, now)
        }) {
            Ok(Some(Some(said))) => {
                journal(&state, now, &owed.key, &said, level, free, &host);
                format!("released:{why}")
            }
            Ok(Some(None)) => "gone".to_owned(),
            Ok(None) => "wait:lease-held".to_owned(),
            Err(_) => "wait:lease-unwritten".to_owned(),
        }
    } else if typed_here(&owed.key, sid, now) {
        "gone".to_owned()
    } else {
        let token = super::harness_drive_token();
        let stepped = ask::transact(&state, &ask::LOCK_LADDER, |l| {
            if ask::claim(l, &owed.key, &host, sid, now) == ask::Claim::Gone {
                return "gone".to_owned();
            }
            let step = disk::type_resume(sock, token.as_ref(), sid, &owed.text);
            if step == "typed" {
                let at = now_ms();
                TYPED.lock().unwrap_or_else(PoisonError::into_inner).insert(
                    owed.key.clone(),
                    Typed {
                        sid: sid.to_owned(),
                        at_ms: at,
                        free,
                    },
                );
                ask::typed(l, &owed.key, &host, sid, at, free);
            }
            step
        });
        match stepped {
            Ok(Some(step)) => step,
            Ok(None) => "wait:lease-held".to_owned(),
            Err(e) if typed_here(&owed.key, sid, now_ms()) => {
                aterm_log::warn!(
                    "harness @{sid}: {} typed, but the lease could not record it ({e}); the \
                     next look records it",
                    ask::RULE_DISK_FREE
                );
                "typed".to_owned()
            }
            Err(_) => "wait:lease-unwritten".to_owned(),
        }
    };
    if step == "typed" {
        super::log_label(store, sid, aterm_agent::harness::acts::DISK_FREE_LABEL);
    }
    aterm_agent::harness::cli::disk_ask_journal(
        &state,
        &[ask::step_row(now / 1000, sid, &owed.key, &step, &host)],
    );
    step
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
}
