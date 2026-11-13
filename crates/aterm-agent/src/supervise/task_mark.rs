// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Task record publication over [`Ctl`] under [`super::claim`] ([`SuperviseOpts::tasks`],
//! `Session::supervise`, [`mark_task_exited`]).
//!
//! Publishes [`TurnEndState::landed`] and [`TurnEndState::gave_up`] points
//! ([`TurnEndState::point_number`],
//! [`TurnEndState::left_to_person`], [`TurnEndState::followed`],
//! [`TurnEndState::unlanded`]) with result snapshots
//! ([`crate::harness::result::snapshot`], [`DoneBy`], [`crate::harness::result::snapshot_screen`],
//! [`crate::harness::result::with_report`], [`result::prune_results`]) and seeds loops from
//! existing records, read ahead of the first fold ([`Session::task_ahead`],
//! [`Session::task_seed`], [`TurnEndState::set_managed`],
//! [`TurnEndState::seed_task_done`], [`TurnEndState::record_landing_holds`], and the occupancy
//! task door's standing fact word, `Session::door_task_seed`), or at a break's write before the
//! first fold ([`Session::managed_at_write`]).

use serde::{Deserialize, Serialize};

use super::*;
use crate::harness::result::{self, DoneBy, Snapshot};
use crate::supervise::tape::codec::CodecError;
use crate::supervise::tape::core::TapeValue;
use crate::supervise::tape::doors;

/// What the done edge's result came to ([`Session::publish_done`]'s round
/// trip at [`doors::TASK_RESULT`]): the transcript's settle — its waits, and
/// whether the done edge's turn was read to its end; `None` with no
/// transcript — and the snapshot written, or why none was.
struct ResultTaken {
    settle: Option<(u32, bool)>,
    snap: Result<Snapshot, String>,
}

/// The words a snapshot says it has no result by
/// (`crate::harness::result::report`'s, an older build's `no-marker`, and
/// `no-transcript`): what a tape's `unavailable` is read back as, a word no
/// build wrote refused.
const UNAVAILABLE: &[&str] = &[
    "no-check",
    "no-report",
    "no-reply",
    "no-marker",
    "empty",
    "no-transcript",
    "no-words",
];

/// [`ResultTaken`] as a tape holds it: the snapshot written, or why none
/// was (`error`), exactly one of the two.
#[derive(Serialize, Deserialize)]
struct ResultWire {
    settle: Option<(u32, bool)>,
    snap: Option<SnapWire>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SnapWire {
    path: PathBuf,
    bytes: u64,
    unavailable: Option<String>,
    by: String,
}

impl TapeValue for ResultTaken {
    fn to_tape(&self) -> Vec<u8> {
        let wire = ResultWire {
            settle: self.settle,
            snap: self.snap.as_ref().ok().map(|s| SnapWire {
                path: s.path.clone(),
                bytes: s.bytes,
                unavailable: s.unavailable.map(str::to_string),
                by: s.by.word().to_string(),
            }),
            error: self.snap.as_ref().err().cloned(),
        };
        aterm_json::to_vec(&wire).unwrap_or_default()
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        let bad = |why| CodecError { at: 0, why };
        let wire: ResultWire = aterm_json::from_slice(bytes)
            .map_err(|_| bad("a task result this build did not write"))?;
        let snap = match (wire.snap, wire.error) {
            (Some(s), None) => Ok(Snapshot {
                path: s.path,
                bytes: s.bytes,
                unavailable: match s.unavailable {
                    None => None,
                    Some(w) => Some(
                        UNAVAILABLE
                            .iter()
                            .copied()
                            .find(|u| *u == w)
                            .ok_or(bad("a result word this build does not write"))?,
                    ),
                },
                by: DoneBy::of_word(&s.by).ok_or(bad("a result rule no build writes"))?,
            }),
            (None, Some(e)) => Err(e),
            _ => {
                return Err(bad(
                    "a task result with both a snapshot and an error, or neither",
                ));
            }
        };
        Ok(Self {
            settle: wire.settle,
            snap,
        })
    }
}

/// How the window host's startup deadline's attention begins.
pub const STARTUP_HEAD: &str = "the worker never started: ";

/// How the window host's held deadline's attention begins.
pub const HELD_HEAD: &str = "the worker is held: ";

/// Whether a supervisor attention `text` is one of the window host's deadlines'.
#[must_use]
pub fn deadline_attention(text: &str) -> bool {
    text.starts_with(STARTUP_HEAD) || text.starts_with(HELD_HEAD)
}

/// A session's task record as one `task` reply gives it ([`task_read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRead {
    /// The record's revision.
    pub(crate) rev: u64,
    /// `working`, `done`, `exited`, `stalled` or `failed`.
    pub(crate) state: String,
    /// The result's size.
    pub(crate) result_bytes: u64,
    /// The result's file, as the wire spells it (pct-encoded).
    pub(crate) result_path: Option<String>,
    /// What moved it to its state (`why=`), when something said.
    pub(crate) why: Option<String>,
    /// The fact word the occupancy loop's task door noted on it (`fact=`), read as the closed
    /// word only ([`crate::supervise::occupancy::FactWord::parse`]): `-`, or any other text, is
    /// none.
    pub(crate) fact: Option<crate::supervise::occupancy::FactWord>,
}

/// The record one `task` reply names.
pub(crate) fn task_read(reply: &CtlReply) -> Option<TaskRead> {
    if !reply.ok() {
        return None;
    }
    let rest = reply.stdout.lines().next()?.strip_prefix("OK task ")?;
    let field = |key: &str| {
        rest.split_whitespace()
            .find_map(|t| t.strip_prefix(key)?.strip_prefix('='))
    };
    Some(TaskRead {
        rev: crate::parse_uint(field("rev")?)?,
        state: field("state")?.to_string(),
        result_bytes: field("result_bytes")
            .and_then(crate::parse_uint)
            .unwrap_or(0),
        result_path: field("result_path").map(str::to_string),
        why: field("why").map(str::to_string),
        fact: field("fact").and_then(crate::supervise::occupancy::FactWord::parse),
    })
}

/// The `why=` when the harness marks a task `done`.
pub const LANDED_WHY: &str = "landed";

/// The `why=` when the harness marks a task `failed` on the worker's own `Could not finish:` word.
pub const COULD_NOT_FINISH_WHY: &str = GaveUp::CouldNotFinish.word();

/// The `why=` when the harness marks a task `failed` because Land stopped typing to a reply that
/// came back the same with no work.
pub const REPEATED_WHY: &str = GaveUp::Repeated.word();

/// A carried failure from the former usage-wall policy. The successor reopens this task.
pub const USAGE_LIMIT_WHY: &str = GaveUp::UsageLimit.word();

/// The ended records someone else's turn opens again ([`Session::reopen_task`]): a `done` one, and
/// a `failed` one, whose conclusion the turn lifts.
const BY_TURN: &[&str] = &["done", "failed"];

/// The ended record Cruise's unlanding opens again ([`Session::reopen_task`],
/// [`TurnEndState::unlanded`]): a `done` one alone. Its books were landed, so the record
/// reads its landing; a `failed` one is not the unlanding's to lift.
const BY_UNLANDING: &[&str] = &["done"];

/// Whether the record `t` is a landing this harness published: `done
/// why=landed` ([`LANDED_WHY`]).
fn is_landing(t: &TaskRead) -> bool {
    t.state == "done" && t.why.as_deref() == Some(LANDED_WHY)
}

impl<C: Ctl> Session<'_, C> {
    /// The task record at a decided point ([the module](self)).
    pub(super) fn task_at_point(
        &mut self,
        seq: u64,
        was_done: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let _duty = self.metadata_duty_guard(Scope::TurnEnd);
        let Some(tasks) = opts.tasks.as_deref() else {
            return Ok(());
        };
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        let te = &self.turn_end;
        if te.task_done() {
            let at = te.point_number();
            let ending = te.gave_up();
            if !(te.landed() || ending.is_some()) || at.is_none() || at == self.task_point {
                return Ok(());
            }
            let again = self.task_point.is_some() && te.followed().is_none();
            if self.publish_done(seq, again, ending, tasks, opts, review)? {
                self.task_point = at;
            }
        } else if was_done && te.followed().is_none() {
            self.reopen_task(seq, BY_TURN, review)?;
        } else if was_done && te.unlanded() {
            // Cruise's unlanding ([`TurnEndState::unlanded`]); any other act of this loop's opens
            // nothing. It takes a landing on, never a failed conclusion.
            self.reopen_task(seq, BY_UNLANDING, review)?;
        }
        Ok(())
    }

    /// The end of this point, published, its result snapshotted under `tasks`: a landing, or with
    /// `ending` a worker's failed conclusion. `Ok(true)` once the record was read and, where it was
    /// `working` — or ended and opened `again` by someone else's turn, or ended some other way
    /// (an older build's `done`, with no landing, [`is_landing`]; a `failed` that a landing now
    /// follows) — marked; `Ok(false)` where a request failed or its reply did not confirm the mark.
    fn publish_done(
        &mut self,
        seq: u64,
        again: bool,
        ending: Option<GaveUp>,
        tasks: &Path,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let r = self.call(&["task"])?;
        if !r.ok() {
            return Ok(false);
        }
        let mut record = task_read(&r);
        let stands = |t: &TaskRead| match ending {
            None => is_landing(t),
            Some(_) => t.state == "failed",
        };
        if record
            .as_ref()
            .is_some_and(|t| matches!(t.state.as_str(), "done" | "failed") && (again || !stands(t)))
        {
            let r = self.call(&["task", "mark", "working", "why=turn"])?;
            if !r.ok() {
                return Ok(false);
            }
            record = task_read(&r);
            if record
                .as_ref()
                .is_none_or(|record| record.state != "working")
            {
                return Ok(false);
            }
            if let Some(after) = &record {
                review.note(&format!(
                    "TASK seq={seq} working why=turn rev={}",
                    after.rev
                ));
            }
        }
        let Some(record) = record.filter(|t| {
            t.state == "working"
                || (t.state == "stalled" && matches!(t.why.as_deref(), Some("startup" | "held")))
        }) else {
            return Ok(true);
        };
        let by = if ending.is_some() {
            DoneBy::Failed
        } else {
            DoneBy::Turn
        };
        let stem = result::result_stem(self.setup.sid.as_deref(), record.rev);
        let transcript = opts.idle_host.as_ref().and_then(|h| h.transcript());
        let screen = if transcript.is_none() {
            self.point_said
                .clone()
                .filter(|(program, _)| crate::supervise::policy::turn_end::screen_alone(*program))
                .map(|(program, said)| {
                    // THE CHECK'S `LANDED` adds nothing: the result is the defense it audited.
                    let said = match &self.land_defense_said {
                        Some((by, defense))
                            if *by == program
                                && crate::supervise::policy::land_check::landed_reply(Some(
                                    &said,
                                )) =>
                        {
                            defense.clone()
                        }
                        _ => said,
                    };
                    let report = self
                        .land_report
                        .as_ref()
                        .filter(|(by, _)| *by == program)
                        .map(|(_, report)| report.as_str());
                    (program, result::with_report(report, &said))
                })
        } else {
            None
        };
        let shown = self.turn_end.done_shown().map(|t| self.unix_ms_of(t));
        // WHAT ATERM CHECKED (§14.9, slice 9c): the receipt this landing told, on the record's
        // `checked=` and in the result's sidecar; none where the landing has none.
        let checked = self
            .land_receipt
            .as_ref()
            .filter(|_| ending.is_none())
            .map(crate::harness::acts::LandReceipt::checked_word);
        let mut unnoted: Option<String> = None;
        let request = || {
            let mut r = self.fx.path_bytes(tasks);
            r.push(b' ');
            r.extend_from_slice(stem.as_bytes());
            r.push(b' ');
            r.extend_from_slice(by.word().as_bytes());
            if let Some((program, said)) = &screen {
                r.extend_from_slice(format!(" screen={} ", program.name()).as_bytes());
                r.extend_from_slice(said.as_bytes());
            }
            r
        };
        let taken = self.fx.exchange(doors::TASK_RESULT, request, || {
            let mut settle = None;
            let transcript = transcript.map(|(path, first)| {
                let settled = result::settled(&path, first, shown, || self.stopped());
                settle = Some((settled.waits, settled.ended));
                (path, settled.tail)
            });
            let snap = match &screen {
                Some((program, said)) if transcript.is_none() => {
                    result::snapshot_screen(tasks, &stem, program.name(), said, by)
                }
                _ => result::snapshot(
                    tasks,
                    &stem,
                    transcript.as_ref().map(|(p, t)| (p.as_path(), t.as_str())),
                    by,
                ),
            }
            .map_err(|e| e.to_string());
            if let (Ok(s), Some(word)) = (&snap, &checked)
                && let Err(e) = result::note_checked(&s.path, word)
            {
                unnoted = Some(format!(" (its sidecar does not say checked={word}: {e})"));
            }
            if snap.is_ok() {
                result::prune_results(tasks, &stem);
            }
            ResultTaken { settle, snap }
        });
        if let Some((waits, ended)) = taken.settle.filter(|(waits, _)| *waits > 0) {
            let settled = result::Settled {
                tail: String::new(),
                waits,
                ended,
            };
            review.note(&format!("TASK seq={seq} {}", settled.note()));
        }
        let snap = taken.snap;
        let (state, why) = ending.map_or(("done", LANDED_WHY), |e| ("failed", e.word()));
        let mut args = vec![
            "task".to_string(),
            "mark".to_string(),
            state.to_string(),
            format!("why={why}"),
        ];
        if let Some(word) = &checked {
            args.push(format!("checked={word}"));
        }
        // WHAT THE LANDING'S YIELD READ OF THE AGENT'S LEFT ROWS (`left=0|kept:<n>|<n>|unchecked`),
        // only where the host was asked: a tape recorded without the door marks as it was recorded.
        if let Some(left) = self.turn_end.landed_left().filter(|_| ending.is_none()) {
            args.push(format!("left={}", left.word()));
        }
        let result = match &snap {
            Ok(s) => {
                args.push(format!("result_bytes={}", s.bytes));
                args.push(format!(
                    "result_path={}",
                    aterm_codec::pct::encode(&s.path.to_string_lossy())
                ));
                s.unavailable.map_or_else(
                    || format!("result_bytes={}", s.bytes),
                    |w| format!("result unavailable why={w}"),
                )
            }
            Err(e) => format!("no result written: {e}"),
        };
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let r = self.call(&words)?;
        if r.lost() || r.is_err("busy") {
            return Ok(false);
        }
        let Some(after) = task_read(&r).filter(|t| t.state == state && stands(t)) else {
            review.note(&format!(
                "TASK seq={seq} {state} not marked: {}",
                one_line(r.err_text())
            ));
            return Ok(false);
        };
        review.note(&format!(
            "TASK seq={seq} {state} why={why} rev={} {result}{}",
            after.rev,
            unnoted.unwrap_or_default()
        ));
        Ok(true)
    }

    /// THE TASK RECORD AHEAD OF THIS LOOP'S FIRST FOLD, read once per loop, right before that
    /// point's reading: whether a manager waits on the session is known to the point the fold
    /// judges ([`TurnEndState::set_managed`]), so a managed worker's opening exchange is its
    /// task's, never left to a person, its first point is not read as taskless by the host's
    /// record of its conversation ([`Self::record_trails_task`]), and its `Could not finish:`
    /// concludes at that point. A lost read leaves the session unmanaged, as a session with no
    /// record is, and is tried again at the next decision: the next point's, or the same point's
    /// where that one rests ([`Self::record_unread`]). The rest the record seeds waits for the
    /// fold ([`Self::task_seed`]).
    pub(super) fn task_ahead(&mut self, opts: &SuperviseOpts) -> Result<Option<TaskRead>, Fail> {
        if self.task_seeded || opts.tasks.is_none() || self.claim.watching_behind().is_some() {
            return Ok(None);
        }
        let r = self.call(&["task"])?;
        self.record_unread = r.lost();
        if self.record_unread {
            return Ok(None);
        }
        self.task_seeded = true;
        let record = task_read(&r);
        if let Some(record) = &record {
            self.turn_end.set_managed(record.state != "exited");
        }
        Ok(record)
    }

    /// The task record [`Self::task_ahead`] read before this loop's first fold, seeded once that
    /// fold has judged its point.
    pub(super) fn task_seed(&mut self, record: Option<TaskRead>, opts: &SuperviseOpts) {
        let Some(record) = record else {
            return;
        };
        self.door_task_seed(record.fact, &opts.policy);
        if record.state == "failed" {
            // A CONCLUSION THE LOOP BEFORE THIS ONE MADE ends this loop's typing too, unless a
            // turn ran since; `why=off`, `why=off-ctl` and `why=expired` are the Autopilot word's to
            // hold.
            let ending = record.why.as_deref().and_then(GaveUp::of_word);
            if let Some(ending) = ending
                && self.turn_end.seed_gave_up(ending)
            {
                self.task_point = self.turn_end.point_number();
                self.task_done_said = true;
            } else if ending.is_some() && !self.turn_end.act_in_flight() {
                self.task_done_said = true;
            }
        } else if record.state == "done" {
            let holds = self.turn_end.task_done() || self.turn_end.record_landing_holds();
            if is_landing(&record) && holds && self.turn_end.seed_task_done() {
                self.task_point = self.turn_end.point_number();
                self.task_done_said = true;
            } else if !self.turn_end.act_in_flight() {
                // The done stood before this point, and the point (a wall)
                // ends a turn after it: someone else's, where it follows no
                // act of this loop's, and the record is opened again.
                self.task_done_said = true;
            }
        }
    }

    /// THE MANAGED GUARD'S RECORD AT A LINE'S WRITE (`Session::write_guards`, past an owner's
    /// grant): before this loop's first fold has read the record ([`Self::task_ahead`]), a break's
    /// line would be judged on a worker no one has read as managed — a managed worker's first
    /// turn, or a loop that carried an open hold. So the write reads it once here and sets the
    /// managed guard as the seed does. `Some(Guard::Managed)` holds the line where the read is
    /// lost. Not on a tape slice 2's doors did not record, whose loop typed as it judged.
    pub(super) fn managed_at_write(&mut self) -> Result<Option<Guard>, Fail> {
        use crate::supervise::occupancy::fault::{Fault, armed};
        if self.task_seeded
            || self.managed_read
            || !self.tasks_tracked
            || !self.fx.slice2_recorded()
            || self.claim.watching_behind().is_some()
            || armed(Fault::ManagedUnread)
        {
            return Ok(None);
        }
        let r = self.call(&["task"])?;
        if r.lost() {
            return Ok(Some(Guard::Managed));
        }
        self.managed_read = true;
        if let Some(record) = task_read(&r) {
            self.turn_end.set_managed(record.state != "exited");
        }
        Ok(None)
    }

    /// The task opened again: a record whose state is one of `ended` marked `working why=turn`
    /// (someone else's turn opens a `done` or `failed` one, [`BY_TURN`]; Cruise unlanding it a
    /// `done` one alone, [`BY_UNLANDING`]).
    fn reopen_task(
        &mut self,
        seq: u64,
        ended: &[&str],
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let _duty = self.metadata_duty_guard(Scope::TurnEnd);
        let r = self.call(&["task"])?;
        if task_read(&r).is_none_or(|t| !ended.contains(&t.state.as_str())) {
            return Ok(());
        }
        let r = self.call(&["task", "mark", "working", "why=turn"])?;
        if let Some(after) = task_read(&r) {
            review.note(&format!(
                "TASK seq={seq} working why=turn rev={}",
                after.rev
            ));
        }
        Ok(())
    }
}

/// The agent left its tab and nothing brings it back.
///
/// # Errors
/// The transport's failure.
pub fn mark_task_exited<C: Ctl + ?Sized>(ctl: &mut C, sel: &str) -> Result<Option<String>, String> {
    let Some(record) = task_read(&ctl.call(&[sel, "task"])?).filter(|t| t.state != "exited") else {
        return Ok(None);
    };
    let blocks = ctl.call(&[sel, "blocks", "2", "--json"])?;
    let code = blocks
        .ok()
        .then(|| crate::harness::upgrade_drive::shell_exit_status("OK 1", &blocks.stdout, None))
        .flatten()
        .and_then(|code| i32::try_from(code).ok());
    let mut args = vec![
        sel.to_string(),
        "task".to_string(),
        "mark".to_string(),
        "exited".to_string(),
        "why=exit".to_string(),
    ];
    if let Some(code) = code {
        args.push(format!("exit={code}"));
    }
    if matches!(record.state.as_str(), "done" | "failed")
        && let Some(path) = &record.result_path
    {
        args.push(format!("result_bytes={}", record.result_bytes));
        args.push(format!("result_path={path}"));
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = ctl.call(&words)?;
    Ok(Some(if r.ok() {
        r.stdout.trim().to_string()
    } else {
        r.err_text().to_string()
    }))
}

#[cfg(test)]
#[path = "task_mark_tests.rs"]
mod tests;
