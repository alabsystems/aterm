// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING'S ENDS, the window's side (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4): what a move of a session's task face does past the glass the frame reads.

use std::sync::Arc;

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::harness::result;
use aterm_messages::{Message, Severity, tags};

use crate::App;
use crate::autopilot::AutopilotView;
use crate::harness_glass::EndFace;
use crate::task_record::{TaskMark, TaskRecord, TaskState};

/// CODEX-ALAB'S LANDING (Autopilot Land): the act a proven self-claim's holder records with `meta set harness_act landed@v1` once its own decider lands (a clean `Nothing left:` defense over a repository its own check, not aterm's, found no work left to land in or could not read: codex-alab's `repo.rs` lands past its limit) — never typed, and the one act that makes the session's face `landed` ([`self_claim_act_face`]).
pub(crate) const SELF_CLAIM_LANDED: &str = aterm_agent::harness::acts::RULE_LANDED;

/// THE FACE A PROVEN SELF-CLAIM'S ACT `act` TELLS (Autopilot Land): [`TaskFace::Landed`] for its landing ([`SELF_CLAIM_LANDED`]), [`TaskFace::Theirs`] for a point it left to a person's own exchange (`left-to-person@v1`, aterm's own loop's `LEFT` rule: recorded, never typed, as aterm's loop tells `theirs` there), none for its re-exec onto a new build (`upgrade@v1`: the same conversation resumed, a landing carried across, the one act it records at rest after a landing), and [`TaskFace::Open`] for any other act it records — a continuation, a land act, an answer, an approval, a resume after a wall: each types into a turn, and whoever acted after a landing reopened the work.
#[must_use]
pub(crate) fn self_claim_act_face(act: &str) -> Option<TaskFace> {
    if act == SELF_CLAIM_LANDED {
        Some(TaskFace::Landed)
    } else if act == aterm_agent::supervise::policy::turn_end::RULE_LEFT {
        Some(TaskFace::Theirs)
    } else if act == aterm_agent::harness::acts::RULE_UPGRADE {
        None
    } else {
        Some(TaskFace::Open)
    }
}

/// A FACE THAT STANDS UNTIL SOMEONE ELSE'S TURN, as a proven self-claim tells it: its landing ([`TaskFace::Landed`]), and a point it left to a person's own exchange ([`TaskFace::Theirs`]).
#[must_use]
pub(crate) const fn holds_until_another_turn(face: TaskFace) -> bool {
    matches!(face, TaskFace::Landed | TaskFace::Theirs)
}

/// THE LANDING'S OWN TURN, IN THE ORDER THE PROGRAM SAW IT (Autopilot Land §10.4): the count of turns begun a proven self-claim's landing is marked at, from `counted`, the engine's count as its `landed@v1` arrived, and `typed_at`, the count its holder's last act that starts a turn stood at as it arrived with the program's report at rest (`SessionTimeline::typed_turn`; an act that starts none, such as a report mailed after the land turn ended, moves nothing: `aterm_agent::harness::acts::self_claim_types_turn`).
#[must_use]
pub(crate) const fn landing_turns(counted: u64, typed_at: Option<u64>) -> u64 {
    match typed_at {
        Some(at) if at == counted => counted.saturating_add(1),
        _ => counted,
    }
}

/// THE LANDING'S MARK a reopen reads ([`App::reopen_self_claim_landing`]): [`landing_turns`] against the count the holder's last turn-starting act stood at. The program's own reports are the one count: a proven self-claim is codex-alab's, whose screen aterm's reader reads no phase from (`aterm_phase::CodexReader`).
#[must_use]
pub(crate) const fn landing_mark(
    counted: crate::session_timeline::LandingMark,
    typed_at: Option<crate::session_timeline::LandingMark>,
) -> crate::session_timeline::LandingMark {
    let turns = match typed_at {
        Some(at) => Some(at.program_turns),
        None => None,
    };
    crate::session_timeline::LandingMark {
        program_turns: landing_turns(counted.program_turns, turns),
    }
}

/// THE WORK FLOOR OF AN END (§11.2.3, items 1 and 3): a landing marks the tab only after this much of the agent's busy time since its task opened — the kitty's own line between a quick success and a build (`kitty_pet::CHEER_BIG_MS`), kept apart and named for what it guards: a cold restore's first point, a move from open over whatever busy time this process saw of the session, arms nothing for a reply under it.
pub(crate) const END_WORK_MS: u64 = 30_000;

/// THE ONE END (§11.2.3, item 1): the move from `was` to `face`, after `worked_ms` of the agent's busy time since the task opened, is a task that CAME DOWN — under Autopilot Cruise / Land only a landing: `face` is landed, `was` a face the window knew (open, theirs; never "unknown", `None`, the host's word between loop runs and a new process's first, whose next face is a restatement: [`is_an_end`]'s rule), and the work at least [`END_WORK_MS`].
#[must_use]
pub(crate) fn is_a_task_end(
    was: Option<TaskFace>,
    face: TaskFace,
    worked_ms: u64,
) -> Option<EndFace> {
    let known = matches!(was, Some(TaskFace::Open | TaskFace::Theirs));
    EndFace::of(face).filter(|_| known && worked_ms >= END_WORK_MS)
}

impl App {
    /// Whether session `session`'s live supervisor claim is a PROVEN self-claim (`SessionMeta::supervisor_proven`): an agent that supervises itself (codex-alab), beside which this window's hosted loop only watches, so the task's face is the self-claim's (Autopilot Land).
    pub(crate) fn beside_proven_self_claim(&self, session: u64) -> bool {
        self.pool.get(session).is_some_and(|entry| {
            entry
                .ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .supervisor_proven(crate::metrics::now_us())
        })
    }

    /// Whether session `session` is in the gap of its proven self-claim's own re-exec (`SessionTimeline::in_self_reexec`): the claim given back right after its holder's `upgrade@v1`, not yet taken again, at most `session_timeline::SELF_REEXEC_GAP_US` ago.
    pub(crate) fn in_self_claim_reexec(&self, session: u64) -> bool {
        self.pool.get(session).is_some_and(|entry| {
            entry
                .ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .in_self_reexec(crate::metrics::now_us())
        })
    }

    /// THE TASK'S FACE IS NOW `face` on session `session` (`None`: unknown), told by its loop or its proven self-claim (`App::apply_harness_fact`): on the session's timeline, where `status task=` reads it under the lock it already takes, and into the glass the frame reads with none — and, where it moved, the session's footer is posted and the move goes past the glass ([`Self::harness_task_moved`]).
    pub(crate) fn apply_task_face(
        &mut self,
        session: u64,
        face: Option<TaskFace>,
        floor: Option<TaskFace>,
        noted_ms: Option<i64>,
    ) {
        let Some(entry) = self.pool.get(session) else {
            return;
        };
        let now_ms = crate::harness_screen::unix_ms_now();
        let stamp_ms = noted_ms.map_or(now_ms, |noted| noted.min(now_ms));
        let (was, at_ms) = {
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let was = timeline.task_face().map(|(f, _)| f);
            if !timeline.set_task_face(face, stamp_ms) {
                return;
            }
            (was, timeline.task_face().map_or(stamp_ms, |(_, at)| at))
        };
        let hover_was = self.driven_by_aterm(session);
        let landed_was = self.chip_face_landed(session);
        let stood = self.end_unseen(session);
        let moved = crate::harness_glass::on_task(self, session, face, at_ms);
        if moved {
            crate::claude_footer::post_changed(session);
        }
        let cleared = stood && !self.end_unseen(session);
        if cleared {
            self.restamp_for_end();
        }
        self.note_harness_arm(session);
        let unseen_before_end = self.end_unseen(session);
        let was = was.or(floor);
        self.harness_task_moved(session, was, face);
        self.reconcile_self_claim_task(session);
        let end_restamped = cleared || (self.end_unseen(session) && !unseen_before_end);
        if moved && !end_restamped {
            if self.driven_by_aterm(session) != hover_was {
                self.restamp_for_hover(session);
            } else if self.chip_face_landed(session) != landed_was {
                self.mark_status_dirty();
            }
        }
        if was.is_some() && matches!(face, Some(TaskFace::Open | TaskFace::Landed)) {
            self.harness_screen
                .ab
                .task_opened(session, crate::turn_ledger::now_ms());
        }
    }

    /// A PROVEN SELF-CLAIM'S FACE ON ITS WORKER'S TASK RECORD (codex-alab under Autopilot Land), read as a level, not an edge, so a mark an update's fence refused is made at a later pass while the timeline holds the face (at the new build's, the face a seamless update carried, from the glass's end or the claim's own: `App::end_carry_rows`, `SessionTimeline::carried_task_face`): a landed face beside a record open to it since before the landing ([`open_before`]) marks it `done why=landed` with the report the claim's rollout holds ([`land_self_claim_task`], one thread per session at a time), and an open face beside a record a landing marked done before that turn ([`landed_before`]) reopens it `working why=turn` ([`reopen_self_claim_task`]). Called at each move of the face and at each status sweep's visit.
    pub(crate) fn reconcile_self_claim_task(&self, session: u64) {
        if !self.beside_proven_self_claim(session) {
            return;
        }
        let Some(entry) = self.pool.get(session) else {
            return;
        };
        let (face, landed_at, record) = {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                told_or_carried(&timeline),
                timeline.task_face_at(TaskFace::Landed),
                timeline.task().cloned(),
            )
        };
        match (face, landed_at) {
            (_, Some(at_ms)) if open_before(record.as_ref(), at_ms) => {
                let entered = record.map_or(0, |r| r.entered);
                land_self_claim_task(&entry.ctx, session, Landing { entered, at_ms });
            }
            (Some((TaskFace::Open, at_ms)), _) if landed_before(record.as_ref(), at_ms) => {
                reopen_self_claim_task(&entry.ctx, session);
            }
            _ => {}
        }
    }

    /// A TURN BEGUN SINCE A PROVEN SELF-CLAIM'S LANDING on session `session` reopens it (Autopilot Land).
    pub(crate) fn reopen_self_claim_landing(&mut self, session: u64) {
        let Some(entry) = self.pool.get(session) else {
            return;
        };
        let (held, mark) = {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                timeline
                    .task_face()
                    .map(|(f, _)| f)
                    .filter(|f| holds_until_another_turn(*f)),
                timeline.landing_mark().unwrap_or_default(),
            )
        };
        let Some(held) = held else {
            return;
        };
        let program_turns = self.session_status.program_turns(session);
        if program_turns > mark.program_turns && self.beside_proven_self_claim(session) {
            aterm_log::info!(
                "task: @{}: a turn begun past its {held:?} face's mark reopens it (program turns {program_turns}, marked {})",
                entry.ctx.self_id.as_str(),
                mark.program_turns
            );
            self.apply_task_face(session, Some(TaskFace::Open), None, None);
        }
    }

    /// SESSION `session`'S TASK FACE MOVED from `was` to `face` (§11.2.3, items 1, 4 and 6): where the move is an end ([`is_a_task_end`], over the work the A/B book summed since the task opened) under Land, it is told to the session's glass as a new, unseen end, and it is JUDGED AT ONCE by the one looking rule against the frontmost window (`App::see_front`: an end in a pane someone looks at is seen as it lands, and marks nothing).
    pub(crate) fn note_harness_end(
        &mut self,
        session: u64,
        was: Option<TaskFace>,
        face: Option<TaskFace>,
    ) {
        let Some(face) = face.filter(|face| EndFace::of(*face).is_some()) else {
            return;
        };
        if self.session_autopilot(session).1 != AutopilotView::Land {
            return;
        }
        let now = crate::turn_ledger::now_ms();
        let worked = self.harness_screen.ab.worked(session, now);
        let Some(end) = is_a_task_end(was, face, worked) else {
            return;
        };
        let stood = self.end_unseen(session);
        self.harness_glass.entry(session).or_default().tell_end(end);
        let seen = self
            .frontmost_window
            .map(|front| self.see_front(front))
            .unwrap_or_default();
        if seen.task {
            self.mark_status_dirty();
        }
        if stood || self.end_unseen(session) {
            self.restamp_for_end();
        }
    }

    /// Session `session`'s task face MOVED from `was` to `face` (`None`: unknown) — the timeline holds it already, and the footer is posted by the caller (`App::apply_harness_fact`), which has read the A/B book's calendar and the session's mode (`App::note_harness_arm`).
    pub(crate) fn harness_task_moved(
        &mut self,
        session: u64,
        was: Option<TaskFace>,
        face: Option<TaskFace>,
    ) {
        self.note_harness_end(session, was, face);
        let end = face
            .filter(|face| is_an_end(was, *face))
            .filter(|_| self.session_autopilot(session).1 == AutopilotView::Land);
        if end.is_some() {
            let now = crate::turn_ledger::now_ms();
            self.harness_screen.ab.landed(session, now);
        }
        if end.is_some() && !self.beside_proven_self_claim(session) {
            self.note_harness_act_for_page(std::time::Instant::now(), true);
        } else {
            self.harness_screen_moved();
        }
        let Some(face) = end else {
            return;
        };
        let Some(sid) = self
            .pool
            .get(session)
            .map(|s| s.ctx.self_id.as_str().to_string())
        else {
            return;
        };
        let place = self.upgrade_place(&sid);
        let line_place = self.session_place(&sid);
        if let Some(record) = task_record(&sid, &place, &line_place, face) {
            self.record_message(record);
        }
    }
}

/// Whether `record` is a worker's task still open to a landing: `working`, or stalled by a deadline (`startup`, `held`), as aterm's own loop reads one at its done edge.
fn open_to_landing(record: Option<&TaskRecord>) -> bool {
    record.is_some_and(|r| {
        r.state == TaskState::Working
            || (r.state == TaskState::Stalled && matches!(r.why, Some("startup" | "held")))
    })
}

/// Whether `record` is open to the landing the face moved to at `at_ms` (unix ms): open to a landing ([`open_to_landing`]) since before it, so a task reopened after the landing (a `turn` into it, a manager's `task mark working`) waits for a landing of its own.
fn open_before(record: Option<&TaskRecord>, at_ms: i64) -> bool {
    open_to_landing(record) && record.is_some_and(|r| since_by(r, at_ms))
}

/// Whether `record` is a task a landing marked done ([`crate::task_record::WHY_LANDED`]) before the turn the face moved to open for at `at_ms` (unix ms), so a done mark made after that turn began (a manager's `task mark done why=landed`) stands.
fn landed_before(record: Option<&TaskRecord>, at_ms: i64) -> bool {
    record.is_some_and(|r| {
        r.state == TaskState::Done
            && r.why == Some(crate::task_record::WHY_LANDED)
            && since_by(r, at_ms)
    })
}

/// Whether `record` entered its state at or before `at_ms` (unix ms).
fn since_by(record: &TaskRecord, at_ms: i64) -> bool {
    i64::try_from(record.since_unix_ms).is_ok_and(|since| since <= at_ms)
}

/// The landing a [`land_self_claim_task`] thread marks: the open state its record `entered` at (an attention change moves the record's `rev`, never this), and the wall time (unix ms) the face moved to landed (`SessionTimeline::task_face_at`: the face shown, or the one a seamless update carried and holds until it is told again), which a later landing restamps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Landing {
    entered: u64,
    at_ms: i64,
}

/// The landing each session's [`land_self_claim_task`] thread is marking now, by sid.
static LANDING: std::sync::Mutex<std::collections::BTreeMap<String, Landing>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Tests: whether a [`land_self_claim_task`] thread is marking a landing of session `sid` now.
#[cfg(test)]
pub(crate) fn landing_in_flight(sid: &str) -> bool {
    LANDING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains_key(sid)
}

/// A session's entry in [`LANDING`], taken out again when its thread ends however it ends.
struct LandingSlot(String);

impl Drop for LandingSlot {
    fn drop(&mut self) {
        LANDING
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

/// A PROVEN SELF-CLAIM'S LANDING, ON THE TASK RECORD of session `local` ([`App::reconcile_self_claim_task`]): unless a thread already marks one for the session, the report the claim's rollout holds is snapshotted under `<state>/tasks/` (`result::snapshot_rollout`, once its newest turn has ended or [`result::SETTLE_WITHIN`] ran out) on a thread of its own, and the record marked `done why=landed` with it, if, read together under the mark's lock, the record is still open in the state it `entered`, since before the landing, and the face still shows this `landing`.
fn land_self_claim_task(ctx: &Arc<crate::SessionCtx>, local: u64, landing: Landing) {
    let sid = ctx.self_id.as_str().to_string();
    {
        let mut marking = LANDING.lock().unwrap_or_else(|p| p.into_inner());
        if marking.contains_key(&sid) {
            return;
        }
        marking.insert(sid.clone(), landing);
    }
    let slot = LandingSlot(sid);
    let Some(tasks) = crate::task_record::tasks_dir() else {
        return;
    };
    let target = crate::self_claim::rollout_target(ctx);
    let ctx = Arc::clone(ctx);
    let spawned = std::thread::Builder::new()
        .name("aterm-codex-result".into())
        .spawn(move || {
            let _slot = slot;
            crate::qos::set_self(crate::qos::Role::Background);
            let stem = result::result_stem(Some(ctx.self_id.as_str()), landing.entered);
            let rollout = target.as_ref().and_then(|t| {
                let path = aterm_agent::harness::codex_tap::rollout_path(&t.home, &t.thread)?;
                let tail = settled_rollout(&path)?;
                Some((path, tail))
            });
            let thread = target.as_ref().map_or("-", |t| t.thread.as_str());
            let snap = result::snapshot_rollout(
                &tasks,
                &stem,
                rollout.as_ref().map(|(path, tail)| (path.as_path(), tail.as_str())),
                thread,
                result::DoneBy::Turn,
            );
            if snap.is_ok() {
                result::prune_results(&tasks, &stem);
            }
            let mut mark = TaskMark::new(TaskState::Done);
            mark.why = Some(crate::task_record::WHY_LANDED);
            match &snap {
                Ok(s) => {
                    mark.result_bytes = s.bytes;
                    mark.result_path = Some(s.path.clone());
                }
                Err(e) => aterm_log::warn!("task: codex-alab's landing kept no result: {e}"),
            }
            let this_landing = |timeline: &crate::session_timeline::SessionTimeline| {
                let record = timeline.task();
                open_before(record, landing.at_ms)
                    && record.is_some_and(|r| r.entered == landing.entered)
                    && timeline.task_face_at(TaskFace::Landed) == Some(landing.at_ms)
            };
            match crate::task_record::mark_session_task_seen(
                &ctx,
                local,
                &mark,
                ctx.fabric.fence_wait(),
                this_landing,
            ) {
                Ok(Some(_)) => {}
                Ok(None) => aterm_log::info!(
                    "task: codex-alab's landing at {} was not marked: its record or face moved on",
                    landing.at_ms
                ),
                Err(_) => aterm_log::warn!(
                    "task: codex-alab's landing was not marked done: an update is committing (a later pass marks it)"
                ),
            }
        });
    if let Err(e) = spawned {
        aterm_log::warn!("task: codex-alab's landing was not marked done: {e}");
    }
}

/// The tail of the Codex rollout at `path` once its newest turn has ended (`result::codex_turn_ended`), read again every [`result::SETTLE_EVERY`] for at most [`result::SETTLE_WITHIN`]: the turn a landing follows may still be on its way to the file.
fn settled_rollout(path: &std::path::Path) -> Option<String> {
    let until = std::time::Instant::now() + result::SETTLE_WITHIN;
    loop {
        let tail =
            aterm_agent::harness::codex_rollout::tail_of(path, result::TRANSCRIPT_READ_BYTES)?;
        if result::codex_turn_ended(&tail) || std::time::Instant::now() >= until {
            return Some(tail);
        }
        std::thread::sleep(result::SETTLE_EVERY);
    }
}

/// The face a turn begun after a landing is read from: the one shown, or the one a seamless update carried while none is ([`crate::session_timeline::SessionTimeline::carried_task_face`]); never a face held only because the shown one went unknown, which may be aterm's own watch-only loop's start face, not the self-claim's.
fn told_or_carried(timeline: &crate::session_timeline::SessionTimeline) -> Option<(TaskFace, i64)> {
    timeline.task_face().or(timeline.carried_task_face())
}

/// A TURN BEGUN AFTER A PROVEN SELF-CLAIM'S LANDING, ON THE TASK RECORD of session `local`: a record a landing marked done is `working why=turn` again, as aterm's own loop reopens one, if, under the mark's lock, the face still shows the turn ([`told_or_carried`]). On the window's thread, so it never waits on an update's fence: a refused reopen is made at a later pass.
fn reopen_self_claim_task(ctx: &crate::SessionCtx, local: u64) {
    let turn_begun =
        |timeline: &crate::session_timeline::SessionTimeline| match told_or_carried(timeline) {
            Some((TaskFace::Open, at_ms)) => landed_before(timeline.task(), at_ms),
            _ => false,
        };
    let mut mark = TaskMark::new(TaskState::Working);
    mark.why = Some("turn");
    if crate::task_record::mark_session_task_seen(
        ctx,
        local,
        &mark,
        std::time::Duration::ZERO,
        turn_begun,
    )
    .is_err()
    {
        aterm_log::info!(
            "task: codex-alab's next turn did not reopen its task yet: an update is committing"
        );
    }
}

/// THE MOVE FROM `was` TO `face` IS A LANDING'S END, to be recorded once:
///
/// * to [`TaskFace::Landed`] from a face the window KNEW, never from "unknown" (`None`): landed
///   right after unknown restates a landing an earlier loop or process made, so every new
///   landing is a move from a face its loop told first.
///
/// No other move is an end.
#[must_use]
pub(crate) const fn is_an_end(was: Option<TaskFace>, face: TaskFace) -> bool {
    match face {
        TaskFace::Landed => was.is_some(),
        TaskFace::Open | TaskFace::Theirs => false,
    }
}

/// THE RECORD OF A LANDING'S END for session `sid` in its tab `place` (`in tab 3`, `in window 2, tab 3`, `in its tab`; `App::upgrade_place`) — its line led by `line_place`, which names its pane too where the tab holds two agents (`in tab 3, left pane`; `App::session_place`) — a `harness`-tagged RECORD, never a row, keyed `harness.landing.<sid>` (one session's landings, as the log filters them; the log never replaces a record by its key, so a repeat is kept out by [`is_an_end`]) — for [`TaskFace::Landed`] (`Autopilot landed in tab 3`) alone; `None` for every other face.
#[must_use]
pub(crate) fn task_record(
    sid: &str,
    place: &str,
    line_place: &str,
    face: TaskFace,
) -> Option<Message> {
    let (head, what) = match face {
        TaskFace::Landed => (
            "Autopilot landed",
            aterm_agent::supervise::mode_words::LANDED_WORDS,
        ),
        TaskFace::Open | TaskFace::Theirs => return None,
    };
    Some(
        Message::new(tags::HARNESS, Severity::Info, placed(head, place))
            .line(atpkg::progress::sanitize_for_tty(
                &tab_led(line_place, what),
                aterm_messages::DETAIL_LINE_CAP,
            ))
            .record()
            .key(format!("harness.landing.{sid}").as_str()),
    )
}

/// `{head} {place}` (`Autopilot landed in tab 3`), or where that breaks the
/// glass title rule (a place that names a window), `{head}, {place past its
/// "in "}`; a session no tab shows, `{head}` alone.
fn placed(head: &str, place: &str) -> String {
    let Some(bare) = place.strip_prefix("in ").filter(|b| *b != "its tab") else {
        return head.to_string();
    };
    let title = format!("{head} {place}");
    if aterm_messages::text::glass_title_fault(&title).is_none() {
        title
    } else {
        format!("{head}, {bare}")
    }
}

/// `{Place}: {text}` (`Tab 3: it said nothing is left, …`), or `text` alone for a
/// session no tab shows: the line leads with the tab it is about.
fn tab_led(place: &str, text: &str) -> String {
    match place.strip_prefix("in ") {
        Some(bare) if bare != "its tab" => {
            let mut chars = bare.chars();
            let cap: String = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            format!("{cap}: {text}")
        }
        _ => text.to_string(),
    }
}

#[cfg(test)]
#[path = "harness_task_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "harness_end_tests.rs"]
mod end_tests;

#[cfg(test)]
#[path = "landed_words_tests.rs"]
mod landed_words_tests;

#[cfg(test)]
#[path = "harness_task_reopen_tests.rs"]
mod reopen_tests;
