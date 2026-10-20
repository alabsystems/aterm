// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TASK RECORD, PUBLISHED BY THE HARNESS
//! (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.2-§2.3). A session
//! started with a task — a worker — carries a record the server keeps (`task`,
//! `task mark`: aterm-gui's `task_record`), and the window harness is the
//! writer of its `done` and `exited`. The loop reads and marks it over the
//! one [`Ctl`] it drives the session through, as every request it makes —
//! `task`, then `task mark …` — so the in-GUI host and a test's scripted
//! server see the same requests. (The host's connection is an Owner one, and
//! `task mark` is Owner-only.)
//!
//! * DONE — at a point the turn-end policy judges done
//!   ([`TurnEndState::task_done`]: the policy stays the one decider), once a
//!   point ([`TurnEndState::point_number`]), AS THE POINT IS FOLDED IN —
//!   before the host's step or the accounts probe can take the point and
//!   type into it (the live upgrade's READY, a relaunched agent's
//!   continuation): the record is read, and only a `working` one (or one
//!   the window host's deadlines stalled — `stalled why=startup`, the worker
//!   having started after all, or `stalled why=held`, a box a person then
//!   answered — that finished) is marked —
//!   `task mark done why=<marker|done-check> result_bytes=<n>
//!   result_path=<pct>` — after its result is snapshotted from the agent's
//!   transcript by the rule the point followed — the check's wherever the
//!   newest turn is a check, though a loop's first point follows nothing
//!   ([`crate::harness::result::snapshot`], [`DoneBy`]). A record already
//!   `done` at this loop's first done is this loop restarted on a point
//!   published before it, and nothing is marked; a session with no record
//!   has no task.
//! * DONE AGAIN — a later done of this loop's that someone else's turn ended
//!   (a manager's follow-up, a person's question at the window, answered
//!   with the marker): the record a `turn` moved back to `working` is
//!   published as any; one a `send`, a `paste` or a keyboard left `done` is
//!   marked `working why=turn` first, then published — its own snapshot,
//!   under the record's new rev.
//! * NEVER A PERSON'S OWN EXCHANGE ([`TurnEndState::left_to_person`]): that
//!   `done` is theirs. It can come on a tasked session only when a person
//!   types at the worker's window and is answered short — the task's first
//!   turn is argv and a manager's follow-up is a `turn`, neither a window
//!   gesture — and the record is left as it stands.
//! * WORKING AGAIN — where the policy's `done` ends because someone else's
//!   turn ran (the point follows no act of the harness's,
//!   [`TurnEndState::followed`]): a `done` record is marked `working
//!   why=turn`. The server marks the same at once when a `turn` the harness
//!   did not type lands in a done worker (aterm-gui's `cmd_turn`); the second
//!   mark changes nothing.
//! * A LOOP STARTED ON A DONE RECORD TAKES THE DONE ON — the record is read
//!   once a loop, at its first fold ([`Session::task_seed`]): a worker's
//!   policy hears that a manager waits on it (its done check and its
//!   answers go without the short back-off for its first few short turns
//!   in a row, [`TurnEndState::set_managed`]), and a record that reads
//!   `done` already (published by a loop before this one: the harness's
//!   fault restart, a seamless update's successor) is this loop's done at
//!   the point it starts at ([`TurnEndState::seed_task_done`]) — published
//!   and said, so neither the done check nor a continuation is typed into
//!   the finished worker, and someone else's turn after it opens the record
//!   again as above. Not at a point that shows a wall: a done point never
//!   shows one, so that point ended someone else's turn after the done (a
//!   `send`, a paste or keystrokes, which the server does not reopen on),
//!   and it is read as one — the record marked `working why=turn` as above,
//!   the wall's retry, resume or `/login` typed. Nor with an act of this
//!   loop's owed (the read served only after the loop acted): nothing is
//!   read into that point.
//! * NO CHECK INTO A DONE RECORD — the backstop where the record reads
//!   `done` though the policy decided the check (the seed's read was not
//!   served): the check is not typed ([`Session::check_into_done_task`]).
//!   Typed, it is a pointless turn into a finished worker — and, only where
//!   it goes out by the `turn` fallback (a host without the fenced `send`,
//!   a read with no generation) outside the harness's hand, one the server
//!   cannot tell from someone else's and opens the record again on. The
//!   Codex save-then-wait switch's steps are the other harness write that
//!   can land in a done record by that fallback.
//! * DONE OVER BACKGROUND WORK — the worker's ATERM-TASK-DONE read on a
//!   busy screen whose turn is over and only a shell or a monitor it left
//!   keeps busy ([`TurnEndState::finished_over_background`]) is the done of
//!   a point of its own, published as any (`turn_end_loop`'s
//!   `finish_over_background`; a mark not served is asked again at the next
//!   such read). The agent's own wake when that work ends is part of the
//!   task: nothing is marked or published for it
//!   ([`TurnEndState::live_turn`]).
//! * EXITED — the window's host's, where its agent exited and no relaunch
//!   follows ([`mark_task_exited`]).
//!
//! The requests ride the claim's rules ([`super::claim`]): sent while the
//! claim is pending, never by a loop that watches behind another supervisor.
//! A request not served leaves the point unpublished, and the point decided
//! again asks again. A loop with nowhere to snapshot a result
//! ([`SuperviseOpts::tasks`] `None`: a [`Session::supervise`] loop with no
//! tasks dir, as the engine's tests run one) leaves the record alone, marking
//! neither `done` nor `working`: the window's harness publishes the done,
//! with its result, once it holds the session again (its loop, started
//! again, folds the done point first).

use serde::{Deserialize, Serialize};

use super::*;
use crate::harness::result::{self, DoneBy, Snapshot};
use crate::supervise::policy::turn_end::RULE_DONE_CHECK;
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
/// (`crate::harness::result::report`'s, and `no-transcript`): what a tape's
/// `unavailable` is read back as, a word this build does not write refused.
const UNAVAILABLE: &[&str] = &[
    "no-check",
    "no-report",
    "no-reply",
    "no-marker",
    "empty",
    "no-transcript",
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
        // A path that is not UTF-8 has no JSON form: no answer a replay can take.
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
                by: [DoneBy::Check, DoneBy::Marker]
                    .into_iter()
                    .find(|b| b.word() == s.by)
                    .ok_or(bad("a done rule this build does not write"))?,
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

/// How the window host's STARTUP DEADLINE's attention begins (aterm-gui's
/// `harness_host::look_at_startups`): a worker that never started, its task
/// `stalled why=startup`.
pub const STARTUP_HEAD: &str = "the worker never started: ";

/// How the window host's HELD DEADLINE's attention begins: a started worker
/// held where nothing moves, its task `stalled why=held`.
pub const HELD_HEAD: &str = "the worker is held: ";

/// Whether a supervisor attention `text` is one of the window host's
/// deadlines' ([`STARTUP_HEAD`], [`HELD_HEAD`]). The host raises it with the
/// stall and takes it down where the stall ends, so a loop that starts over
/// it leaves it as it stands (its reconcile at a watch's start): its point
/// is the task record's, never a box or a question this loop escalated.
#[must_use]
pub fn deadline_attention(text: &str) -> bool {
    text.starts_with(STARTUP_HEAD) || text.starts_with(HELD_HEAD)
}

/// A session's task record as one `task` reply gives it ([`task_read`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRead {
    /// The record's revision.
    pub(crate) rev: u64,
    /// `working`, `done`, `exited` or `stalled`.
    pub(crate) state: String,
    /// The result's size.
    pub(crate) result_bytes: u64,
    /// The result's file, as the wire spells it (pct-encoded).
    pub(crate) result_path: Option<String>,
    /// What moved it to its state (`why=`), when something said.
    pub(crate) why: Option<String>,
}

/// The record one `task` reply names (`OK task rev=<n> state=<s> …
/// result_bytes=<n> [result_path=<pct>] …`, read by key); `None` for `OK task
/// -` (no task), a refusal, and a server before the verb.
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
        rev: field("rev")?.parse().ok()?,
        state: field("state")?.to_string(),
        result_bytes: field("result_bytes")
            .and_then(|n| n.parse().ok())
            .unwrap_or(0),
        result_path: field("result_path").map(str::to_string),
        why: field("why").map(str::to_string),
    })
}

/// The result file's stem for the task the record at `rev` stands for, in
/// session `sel` (`@s-…`): `<gsid>-<rev>`, the task file's own pairing.
fn result_stem(sel: Option<&str>, rev: u64) -> String {
    let sid: String = sel
        .unwrap_or_default()
        .trim_start_matches('@')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let sid = if sid.is_empty() { "session" } else { &sid };
    format!("{sid}-{rev}")
}

impl<C: Ctl> Session<'_, C> {
    /// THE TASK RECORD AT A DECIDED POINT ([the module](self)), as it is
    /// folded in and again before its act (once a point either way):
    /// `was_done` is the policy's `done` as the point before left it.
    pub(super) fn task_at_point(
        &mut self,
        seq: u64,
        was_done: bool,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<(), Fail> {
        let Some(tasks) = opts.tasks.as_deref() else {
            return Ok(());
        };
        if self.claim.watching_behind().is_some() {
            return Ok(());
        }
        let te = &self.turn_end;
        if te.task_done() {
            let at = te.point_number();
            if te.left_to_person() || at.is_none() || at == self.task_point {
                return Ok(());
            }
            // A later done of this loop's, on someone else's turn.
            let again = self.task_point.is_some() && te.followed().is_none();
            if self.publish_done(seq, again, tasks, opts, review)? {
                self.task_point = at;
            }
        } else if was_done && te.followed().is_none() {
            self.reopen_task(seq, review)?;
        }
        Ok(())
    }

    /// The `done` of this point, published, its result snapshotted under
    /// `tasks`: `Ok(true)` once the record was read and, where it was
    /// `working` — or `done` and opened `again` by someone else's turn —
    /// marked (or refused for good); `Ok(false)` where a request went
    /// unserved or a committing update held the mark back.
    fn publish_done(
        &mut self,
        seq: u64,
        again: bool,
        tasks: &Path,
        opts: &SuperviseOpts,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let r = self.call(&["task"])?;
        if r.lost() {
            return Ok(false);
        }
        let mut record = task_read(&r);
        if again && record.as_ref().is_some_and(|t| t.state == "done") {
            let r = self.call(&["task", "mark", "working", "why=turn"])?;
            if r.lost() || r.is_err("busy") {
                return Ok(false);
            }
            record = task_read(&r);
            if let Some(after) = &record {
                review.note(&format!(
                    "TASK seq={seq} working why=turn rev={}",
                    after.rev
                ));
            }
        }
        // A worker the window host's deadlines stalled that moved after all —
        // `stalled why=startup` (the person answered its dialog), `stalled
        // why=held` (the box no rule knows, answered) — is working: its done
        // is published as any (the host marks it `working` again as it sees
        // it move, and this is the mark that lands first). The launcher's
        // `stalled why=shell` typed nothing: no done of this loop's is that
        // task's.
        let Some(record) = record.filter(|t| {
            t.state == "working"
                || (t.state == "stalled" && matches!(t.why.as_deref(), Some("startup" | "held")))
        }) else {
            return Ok(true);
        };
        let by = if self.turn_end.followed() == Some(RULE_DONE_CHECK) {
            DoneBy::Check
        } else {
            DoneBy::Marker
        };
        let stem = result_stem(self.setup.sid.as_deref(), record.rev);
        let transcript = opts.idle_host.as_ref().and_then(|h| h.transcript());
        // The screen's done point can come before Claude Code wrote the
        // turn's last rows: read on until the turn's end row, at most the
        // bound (`result::settled`). The turn waited for is the one the done
        // was read on: none a row stamped after that read started (a
        // background shell's notification that landed once the screen showed
        // the done) — the read that decided the done, never the point's
        // first showing, which comes before the turn of a carry-on the host
        // typed that the worker answered with the marker.
        let shown = self.turn_end.done_shown().map(|t| self.unix_ms_of(t));
        // The settle and the snapshot are one round trip through the door
        // ([`doors::TASK_RESULT`]): the transcript read on and the result
        // written live, their outcome taped — a replay reads no transcript
        // and writes no result, and is handed what the run's were.
        let request = || {
            let mut r = self.fx.path_bytes(tasks);
            r.push(b' ');
            r.extend_from_slice(stem.as_bytes());
            r.push(b' ');
            r.extend_from_slice(by.word().as_bytes());
            r
        };
        let taken = self.fx.exchange(doors::TASK_RESULT, request, || {
            let mut settle = None;
            let transcript = transcript.map(|(path, first)| {
                let settled = result::settled(&path, first, shown, || self.stopped());
                settle = Some((settled.waits, settled.ended));
                (path, settled.tail)
            });
            let snap = result::snapshot(
                tasks,
                &stem,
                transcript.as_ref().map(|(p, t)| (p.as_path(), t.as_str())),
                by,
            )
            .map_err(|e| e.to_string());
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
        // The rule it was read by: the check's wherever its newest turn is
        // one, though this loop's first point followed no check of its own.
        let why = snap.as_ref().map_or(by, |s| s.by).word();
        let mut args = vec![
            "task".to_string(),
            "mark".to_string(),
            "done".to_string(),
            format!("why={why}"),
        ];
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
        match task_read(&r) {
            Some(after) => review.note(&format!(
                "TASK seq={seq} done why={why} rev={} {result}",
                after.rev
            )),
            None => review.note(&format!(
                "TASK seq={seq} done not marked: {}",
                one_line(r.err_text())
            )),
        }
        Ok(true)
    }

    /// THE DONE CHECK IS NOT ASKED OF A FINISHED WORKER: the check the policy
    /// decided at a point where the session's task record reads `done`
    /// already — the backstop of [`Self::task_seed`], which takes a done
    /// record on at a loop's first fold, for a loop whose seed read was not
    /// served: its first point follows no act of its own, and its policy
    /// reads the worker's answer to the earlier check (`Yes, it's all
    /// done.`) as a first done report. Typed, the check is a pointless turn
    /// into a finished worker; and where it goes out by the `turn` fallback
    /// (a host without the fenced `send`, a read with no generation) outside
    /// the harness's hand, the server cannot tell it from someone else's
    /// turn and moves the record back to `working`, and the done is
    /// published again under a new rev, waking every `await task`. `true`:
    /// held back, `SKIPPED … rule=done-check` said once a point, and nothing
    /// typed until the next point. A record not read as `done` (none, a
    /// server before the verb, a request not served) types the check as ever.
    pub(super) fn check_into_done_task(
        &mut self,
        seq: u64,
        review: &mut dyn Review,
    ) -> Result<bool, Fail> {
        let point = self.turn_end.point_number();
        if point.is_some() && self.check_held_at == point {
            return Ok(true);
        }
        let r = self.call(&["task"])?;
        if task_read(&r).is_none_or(|t| t.state != "done") {
            return Ok(false);
        }
        review.note(&format!(
            "SKIPPED seq={seq} rule={RULE_DONE_CHECK} the task is marked done: no check is asked \
             of a finished worker"
        ));
        self.check_held_at = point;
        Ok(true)
    }

    /// THE TASK RECORD AT THIS LOOP'S FIRST FOLD, read once a loop (the
    /// module's "a loop started on a done record"): the policy hears that a
    /// manager waits on the session ([`TurnEndState::set_managed`]: a record
    /// that is not `exited`), and a record that reads `done` is this loop's
    /// done at the point it stands at ([`TurnEndState::seed_task_done`]) —
    /// published there (nothing marked again) and said (no `DONE` line
    /// again). Where the point shows a wall the done stood BEFORE it, and
    /// the edge reads the point as the end of a turn after it
    /// ([`Self::task_at_point`]: someone else's, the record opened again);
    /// with an act of this loop's owed (a seed read served only after the
    /// loop acted) nothing is read into the point. Called after the fold,
    /// before the decision and the edge. A request not served asks again at the next fold; no record, a
    /// server before the verb, and a loop with nowhere to publish (`tasks`
    /// `None`) read nothing more.
    pub(super) fn task_seed(&mut self, opts: &SuperviseOpts) -> Result<(), Fail> {
        if self.task_seeded || opts.tasks.is_none() || self.claim.watching_behind().is_some() {
            return Ok(());
        }
        let r = self.call(&["task"])?;
        if r.lost() {
            return Ok(());
        }
        self.task_seeded = true;
        let Some(record) = task_read(&r) else {
            return Ok(());
        };
        self.turn_end.set_managed(record.state != "exited");
        if record.state == "done" {
            if self.turn_end.seed_task_done() {
                self.task_point = self.turn_end.point_number();
                self.task_done_said = true;
            } else if !self.turn_end.act_in_flight() {
                // The done stood before this point, and the point (a wall)
                // ends a turn after it: someone else's, where it follows no
                // act of this loop's, and the record is opened again.
                self.task_done_said = true;
            }
        }
        Ok(())
    }

    /// The task opened again by someone else's turn: a `done` record marked
    /// `working why=turn`.
    fn reopen_task(&mut self, seq: u64, review: &mut dyn Review) -> Result<(), Fail> {
        let r = self.call(&["task"])?;
        if task_read(&r).is_none_or(|t| t.state != "done") {
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

/// THE AGENT LEFT ITS TAB AND NOTHING BRINGS IT BACK — the window's host's
/// word: an exit left to a person or a holder, its launch's own end, one the
/// relaunch can never make. The task of session `sel` (`@s-…`) not marked
/// `exited` yet is marked `exited why=exit [exit=<code>]`, the code the
/// command block its shell is back from ended with — there only where the
/// shell's block recorded one, its prompt drawn again; the mark is not held
/// for it — keeping a done task's result
/// (`result_bytes=`, `result_path=`), which `result` still answers. A session
/// with no task is left as it is. Returns the mark's reply line, `None` where
/// nothing was marked.
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
    if record.state == "done"
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
mod tests {
    use super::*;

    /// A server that answers `task` with `record` (`None`: `OK task -`), the
    /// tab's `blocks 2 --json` with `blocks` (`None`: refused), and every
    /// mark `OK task … changed=1`, keeping the requests.
    struct Tab {
        record: Option<&'static str>,
        blocks: Option<&'static str>,
        asked: Vec<String>,
    }

    impl Ctl for Tab {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let line = args.join(" ");
            self.asked.push(line.clone());
            let ok = |stdout: String| CtlReply {
                code: 0,
                stdout,
                stderr: String::new(),
            };
            let words: Vec<&str> = args.iter().copied().skip(1).collect();
            Ok(match words.as_slice() {
                ["task"] => ok(self.record.map_or_else(
                    || "OK task -\n".to_string(),
                    |r| format!("OK task {r} agent=- attention=-\n"),
                )),
                ["task", "mark", ..] => ok("OK task rev=9 state=exited changed=1\n".to_string()),
                ["blocks", "2", "--json"] => match self.blocks {
                    Some(b) => ok(format!("{b}\n")),
                    None => CtlReply {
                        code: 1,
                        stdout: String::new(),
                        stderr: "aterm-ctl: ERR no blocks\n".to_string(),
                    },
                },
                _ => return Err(format!("unscripted: {line}")),
            })
        }
    }

    /// The shell back at its prompt after the agent's block ended with 130.
    const BACK: &str = r#"{"blocks":[{"id":3,"state":"complete","prompt":10,"end":20,"exit":130},{"id":4,"state":"entering","prompt":20}]}"#;

    fn marks(tab: &Tab) -> Vec<&str> {
        tab.asked
            .iter()
            .map(String::as_str)
            .filter(|r| r.contains(" task mark "))
            .collect()
    }

    /// THE AGENT GONE FOR GOOD MARKS ITS TASK `exited`: with the code its
    /// shell's block ended with, keeping a done task's result. NEGATIVE
    /// CONTROLS: no task, and one marked `exited` already, mark nothing; a
    /// tab whose shell has not drawn its prompt again says no code.
    #[test]
    fn an_agent_gone_for_good_marks_its_task_exited_with_its_code() {
        let run = |record, blocks| {
            let mut tab = Tab {
                record,
                blocks,
                asked: Vec::new(),
            };
            let said = mark_task_exited(&mut tab, "@s-1").expect("served");
            (tab, said)
        };
        let (tab, said) = run(
            Some("rev=1 state=working from=ctl since_ms=5 result_bytes=0"),
            Some(BACK),
        );
        assert_eq!(marks(&tab), ["@s-1 task mark exited why=exit exit=130"]);
        assert_eq!(
            said.as_deref(),
            Some("OK task rev=9 state=exited changed=1")
        );
        let (tab, _) = run(
            Some(
                "rev=2 state=done from=ctl since_ms=5 result_bytes=17 why=marker result_path=/s/tasks/s-1-1.result.md",
            ),
            Some(BACK),
        );
        assert_eq!(
            marks(&tab),
            [
                "@s-1 task mark exited why=exit exit=130 result_bytes=17 result_path=/s/tasks/s-1-1.result.md"
            ]
        );
        let (tab, _) = run(
            Some("rev=1 state=working from=ctl since_ms=5 result_bytes=0"),
            Some(r#"{"blocks":[{"id":3,"state":"executing","prompt":10}]}"#),
        );
        assert_eq!(marks(&tab), ["@s-1 task mark exited why=exit"]);
        let (tab, _) = run(
            Some("rev=1 state=stalled from=ctl since_ms=5 result_bytes=0"),
            None,
        );
        assert_eq!(marks(&tab), ["@s-1 task mark exited why=exit"]);
        // NEGATIVE CONTROLS.
        for record in [
            None,
            Some("rev=3 state=exited from=ctl since_ms=5 result_bytes=0 why=exit"),
        ] {
            let (tab, said) = run(record, Some(BACK));
            assert_eq!(said, None);
            assert_eq!(tab.asked, ["@s-1 task"], "{record:?}");
        }
    }

    /// The record is read by key off the `task` reply and the mark's own;
    /// `OK task -`, a refusal and a server before the verb are no record.
    #[test]
    fn a_task_reply_is_read_by_key() {
        let reply = |code: i32, stdout: &str| CtlReply {
            code,
            stdout: stdout.to_string(),
            stderr: String::new(),
        };
        assert_eq!(
            task_read(&reply(
                0,
                "OK task rev=4 state=done from=s-1a since_ms=9 result_bytes=12 why=marker result_path=/t/x.md agent=idle attention=- changed=1\n"
            )),
            Some(TaskRead {
                rev: 4,
                state: "done".to_string(),
                result_bytes: 12,
                result_path: Some("/t/x.md".to_string()),
                why: Some("marker".to_string()),
            })
        );
        for none in [
            reply(0, "OK task -\n"),
            reply(1, ""),
            reply(0, "OK\n"),
            reply(0, "OK task state=done\n"),
        ] {
            assert_eq!(task_read(&none), None, "{none:?}");
        }
        assert_eq!(result_stem(Some("@s-0a1b"), 3), "s-0a1b-3");
        assert_eq!(result_stem(None, 1), "session-1");
        assert_eq!(result_stem(Some("@../x"), 1), "x-1");
    }
}
