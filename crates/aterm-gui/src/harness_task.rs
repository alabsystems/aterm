// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING'S ENDS, the window's side (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4): what a move of a session's task face does past the glass the frame reads.

use aterm_agent::harness::acts::TaskFace;
use aterm_messages::{Message, Severity, tags};

use crate::App;
use crate::autopilot::AutopilotView;
use crate::harness_glass::EndFace;

/// CODEX-ALAB'S LANDING (Autopilot Land): the act a proven self-claim's holder records with `meta set harness_act landed@v1` once all of its work is on the remote main branch — never typed, and the one act that makes the session's face `landed` ([`self_claim_act_face`]).
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
pub(crate) fn holds_until_another_turn(face: TaskFace) -> bool {
    matches!(face, TaskFace::Landed | TaskFace::Theirs)
}

/// THE LANDING'S OWN TURN, IN THE ORDER THE PROGRAM SAW IT (Autopilot Land §10.4): the count of turns begun a proven self-claim's landing is marked at, from `counted`, the engine's count as its `landed@v1` arrived, and `typed_at`, the count its holder's last act that starts a turn stood at as it arrived with the program's report at rest (`SessionTimeline::typed_turn`; an act that starts none, such as a report mailed after the land turn ended, moves nothing: `aterm_agent::harness::acts::self_claim_types_turn`).
#[must_use]
pub(crate) fn landing_turns(counted: u64, typed_at: Option<u64>) -> u64 {
    match typed_at {
        Some(at) if at == counted => counted.saturating_add(1),
        _ => counted,
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

    /// A TURN BEGUN SINCE A PROVEN SELF-CLAIM'S LANDING on session `session` reopens it (Autopilot Land).
    pub(crate) fn reopen_self_claim_landing(&mut self, session: u64) {
        let Some(entry) = self.pool.get(session) else {
            return;
        };
        let (holds, busy_entries, mark) = {
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            (
                timeline
                    .task_face()
                    .is_some_and(|(f, _)| holds_until_another_turn(f)),
                timeline.agent().busy_entries,
                timeline.landing_mark().unwrap_or_default(),
            )
        };
        if !holds {
            return;
        }
        let begun = self.session_status.program_turns(session) > mark.program_turns
            || busy_entries > mark.busy_entries;
        if begun && self.beside_proven_self_claim(session) {
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

/// THE MOVE FROM `was` TO `face` IS A LANDING'S END, to be recorded once:
#[must_use]
pub(crate) fn is_an_end(was: Option<TaskFace>, face: TaskFace) -> bool {
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
            "all of the work is on the remote main branch; the harness types nothing more until \
             someone else's turn",
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

/// `{Place}: {text}` (`Tab 3: all of the work …`), or `text` alone for a
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
