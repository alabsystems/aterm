// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING'S ENDS, the window's side
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4): what a move of a
//! session's task face does past the glass the frame reads.
//!
//! The face itself — open, landed, theirs — is the engine's
//! (`aterm_agent::harness::acts::TaskFace`), told by the session's loop as it
//! moves (or, for a proven self-claim's holder, by its `landed@v1` act,
//! `SessionTimeline::note_harness_act`) and kept on the session's timeline for
//! `status task=` and in the harness's glass for the chip
//! (`App::apply_harness_fact`). A move here:
//!
//! * REPUBLISHES Settings ▸ Harness where a view shows it: each SESSIONS
//!   row's state line reads the face (`landed 17:42`, `quiet: …`). One
//!   check, nothing built, while no view shows the page.
//! * RECORDS the one end a person may want to read later, and only under
//!   Land: a landing that LANDED (`Autopilot landed in tab 3`), all of the
//!   work on the remote main branch. It is a record in the message log,
//!   never a row and never an exceptions `Entry` (§9.4.3, the alert rule): a
//!   landing did what the owner asked — nobody is needed now, and there is no
//!   one action. Every other move (open, a person's own exchange, unknown) is
//!   no record: the chip, the page and `status` say it. (Land is never spent
//!   any more: it nudges until it lands, so a stuck landing is the worker's
//!   NEEDS YOU on screen, never a record here.)
//! * ONE RECORD AN END, judged by the face it moved FROM ([`task_record`]):
//!   the log never replaces a record by its key, so a face that comes back
//!   without a new end is no record. A landing restated by a loop's start
//!   (the face after "unknown": a restart, a seamless update, a panicked
//!   body, an Off, each of which seeds the landing) is none.
//! * COUNTS the same end, the landing, in the A/B book (§11.6.3, item 4:
//!   `AbBook::landed`), by the same rule as the record, before the page is
//!   republished — so Settings ▸ Harness's Ends row reads a landing once
//!   however often its face is told. Under Autopilot Cruise / Land
//!   nothing else ends a task: Cruise has no done to count, and Land is
//!   never quiet.
//!
//! The card's line follows the face through the footer's repaint key, which
//! carries it (`claude_card::LineEnd`); it arms no episode.
//!
//! THE ONE END (§11.2.3, items 1 to 6), ahead of all that, at every move
//! ([`App::note_harness_end`]): whether the task CAME DOWN is decided here
//! once ([`is_a_task_end`]) — under Autopilot Cruise / Land only a
//! landing: the face moved to `landed` from a face the window knew (open,
//! theirs), under Land, after at least [`END_WORK_MS`] of the agent's busy
//! time since the task opened (`AbBook::worked`). An end is told to the
//! glass (`EndGlass`), where the tab's `•` reads it until someone looks
//! (`App::done_unseen`, `App::note_done_seen`), judged at once against the
//! frontmost window. Cruise never ends, so nothing marks, counts or
//! announces a Cruise task as ended. A move from "unknown" is never one: a
//! loop's start restates its face (the landed seed). The sum is zeroed where
//! a known face moves to open or landed (`AbBook::task_opened`, in
//! [`App::apply_task_face`]), so a reply whose busy time ran before the move
//! that opened the task counts none of it: the clip.

use aterm_agent::harness::acts::TaskFace;
use aterm_messages::{Message, Severity, tags};

use crate::App;
use crate::autopilot::AutopilotView;
use crate::harness_glass::EndFace;

/// CODEX-ALAB'S LANDING (Autopilot Land): the act a proven self-claim's
/// holder records with `meta set harness_act landed@v1` once all of its work
/// is on the remote main branch — never typed, and the one act that makes the
/// session's face `landed` ([`self_claim_act_face`]). The one spelling is the
/// act table's ([`aterm_agent::harness::acts::RULE_LANDED`]), which also
/// gives it its session-log word.
pub(crate) const SELF_CLAIM_LANDED: &str = aterm_agent::harness::acts::RULE_LANDED;

/// THE FACE A PROVEN SELF-CLAIM'S ACT `act` TELLS (Autopilot Land):
/// [`TaskFace::Landed`] for its landing ([`SELF_CLAIM_LANDED`]),
/// [`TaskFace::Theirs`] for a point it left to a person's own exchange
/// (`left-to-person@v1`, aterm's own loop's `LEFT` rule: recorded, never
/// typed, as aterm's loop tells `theirs` there), none for its re-exec onto a
/// new build (`upgrade@v1`: the same conversation resumed, a landing carried
/// across, the one act it records at rest after a landing), and
/// [`TaskFace::Open`] for any other act it records — a continuation, a land
/// act, an answer, an approval, a resume after a wall: each types into a
/// turn, and whoever acted after a landing reopened the work. `finished@v1`
/// (an older codex-alab's token act) is no landing.
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

/// A FACE THAT STANDS UNTIL SOMEONE ELSE'S TURN, as a proven self-claim tells
/// it: its landing ([`TaskFace::Landed`]), and a point it left to a person's
/// own exchange ([`TaskFace::Theirs`]). Its holder types nothing at either
/// until a turn it did not type, so the act is marked with the session's
/// counts of turns begun (`session_timeline::LandingMark`) and a turn begun
/// past them opens the task again ([`App::reopen_self_claim_landing`]), as
/// aterm's own loop reads `open` once a turn it did not start is read after
/// either (`TaskFace::of`, rule 1).
#[must_use]
pub(crate) fn holds_until_another_turn(face: TaskFace) -> bool {
    matches!(face, TaskFace::Landed | TaskFace::Theirs)
}

/// THE LANDING'S OWN TURN, IN THE ORDER THE PROGRAM SAW IT (Autopilot Land
/// §10.4): the count of turns begun a proven self-claim's landing is marked
/// at, from `counted`, the engine's count as its `landed@v1` arrived, and
/// `typed_at`, the count its holder's last act that starts a turn stood at
/// as it arrived with the program's report at rest
/// (`SessionTimeline::typed_turn`; an act that starts none, such as a report
/// mailed after the land turn ended, moves nothing:
/// `aterm_agent::harness::acts::self_claim_types_turn`).
///
/// The holder lands at the yield of the turn its last act typed, and records
/// that act once the turn has started (codex-alab's `submit`), so the
/// program saw the turn begin before it landed. aterm sees the two on two
/// channels: the acts on the control socket, the turn's begin as an OSC 7501
/// report the TUI writes to the PTY, parsed when the reader gets to it. Where
/// the landing's act outran that report — no turn counted since the act, at
/// rest — the report still to come is the landing's own turn's, and the mark
/// holds it: `typed_at + 1`. It cannot be a turn begun after the landing:
/// nobody else's turn starts between the holder's act and its turn's yield
/// (a person's turn there is their own exchange, never landed on), and the
/// holder keeps every native turn start behind the landing's acknowledgement
/// (`meta prepare harness_act`). So that report reopens nothing, and the next
/// turn's does. Every other landing is marked at `counted`, as before: the
/// typed turn was counted already (the act came with the report in that turn,
/// or a turn was counted after it).
///
/// What this cannot tell: an act whose own turn's begin and end were both
/// parsed before the act arrived reads as one that outran them, so the next
/// turn after its landing reopens nothing either, until the holder's next act
/// (its act after that turn) reopens the task. The act is recorded the moment
/// its turn starts, so the turn would have to run its whole length while the
/// act crossed the socket.
#[must_use]
pub(crate) fn landing_turns(counted: u64, typed_at: Option<u64>) -> u64 {
    match typed_at {
        Some(at) if at == counted => counted.saturating_add(1),
        _ => counted,
    }
}

/// THE WORK FLOOR OF AN END (§11.2.3, items 1 and 3): a landing marks the
/// tab only after this much of the agent's busy time since its task opened —
/// the kitty's own line between a quick success and a build
/// (`kitty_pet::CHEER_BIG_MS`), kept apart and named for what it guards: a
/// cold restore's first point, a move from open over whatever busy time this
/// process saw of the session, arms nothing for a reply under it.
/// [`is_a_task_end`] reads it; the page's Ends row says it
/// (`harness_ab::ends_words`).
pub(crate) const END_WORK_MS: u64 = 30_000;

/// THE ONE END (§11.2.3, item 1): the move from `was` to `face`, after
/// `worked_ms` of the agent's busy time since the task opened, is a task
/// that CAME DOWN — under Autopilot Cruise / Land only a landing:
/// `face` is landed, `was` a face the window knew (open, theirs; never
/// "unknown", `None`, the host's word between loop runs and a new process's
/// first, whose next face is a restatement: [`is_an_end`]'s rule), and the
/// work at least [`END_WORK_MS`]. Its kind, or `None`. Cruise ends nothing,
/// so no face of Cruise's is an end. Every reader of an end — the tab's `•`,
/// the look that quiets it — reads this one decision; the caller holds it to
/// Land ([`App::note_harness_end`]).
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
    /// Whether session `session`'s live supervisor claim is a PROVEN
    /// self-claim (`SessionMeta::supervisor_proven`): an agent that supervises
    /// itself (codex-alab), beside which this window's hosted loop only
    /// watches, so the task's face is the self-claim's (Autopilot Land).
    /// One leaf lock (the session's metadata).
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

    /// Whether session `session` is in the gap of its proven self-claim's
    /// own re-exec (`SessionTimeline::in_self_reexec`): the claim given back
    /// right after its holder's `upgrade@v1`, not yet taken again, at most
    /// `session_timeline::SELF_REEXEC_GAP_US` ago. The self-claim's face
    /// stands through it (Autopilot Land: `upgrade@v1` moves no face, the
    /// landing carried across). One leaf lock (the session's timeline).
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

    /// THE TASK'S FACE IS NOW `face` on session `session` (`None`: unknown),
    /// told by its loop or its proven self-claim
    /// (`App::apply_harness_fact`): on the session's timeline, where `status
    /// task=` reads it under the lock it already takes, and into the glass
    /// the frame reads with none — and, where it moved, the session's footer
    /// is posted and the move goes past the glass
    /// ([`Self::harness_task_moved`]). `floor` stands for the face it moved
    /// from where none was known (`None` keeps "unknown", so a loop's start
    /// face restates a seeded landing). `noted_ms`: where the loop's start
    /// restated a landing from the journal (`HarnessFact::TaskNoted`), the
    /// wall time (unix ms) the landing was noted. A face told again is no
    /// move.
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
        // The wall clock, unix ms, as the timeline keeps it and the glass
        // formats the face's clock time from it (`landed 17:42`) — never the
        // turn ledger's process-relative ms. A face restated from the
        // journal takes its note's time instead (never one past now): a
        // window that held no face of the session — a seamless update's new
        // one — shows when the work landed, not when the loop came back
        // (§10.2.7).
        let now_ms = crate::harness_screen::unix_ms_now();
        let stamp_ms = noted_ms.map_or(now_ms, |noted| noted.min(now_ms));
        // The face it moved from, and the time the face now held moved
        // there: a restated face (the same face told after "unknown") keeps
        // the first time this window held.
        let (was, at_ms) = {
            let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let was = timeline.task_face().map(|(f, _)| f);
            if !timeline.set_task_face(face, stamp_ms) {
                return;
            }
            (was, timeline.task_face().map_or(stamp_ms, |(_, at)| at))
        };
        // What the tab's hover and the menu bar say of the face, read off
        // the glass before it moves (§12.4.3, item 1): both are composed
        // away from the frame, so a move must stamp them again.
        let hover_was = self.driven_by_aterm(session);
        let landed_was = self.chip_face_landed(session);
        // An unseen end a move to open or theirs clears takes its `•` with
        // it (§11.2.3, item 4).
        let stood = self.end_unseen(session);
        let moved = crate::harness_glass::on_task(self, session, face, at_ms);
        if moved {
            crate::claude_footer::post_changed(session);
        }
        let cleared = stood && !self.end_unseen(session);
        if cleared {
            self.restamp_for_end();
        }
        // The A/B book's cause may have moved with the face (landed, at the
        // prompt under Land, §10.5.3 B.3).
        self.note_harness_arm(session);
        // Past the glass: the one end decided, the page republished, and a
        // landing under Land recorded (§9.4.4).
        let unseen_before_end = self.end_unseen(session);
        let was = was.or(floor);
        self.harness_task_moved(session, was, face);
        // THE HOVER FOLLOWS THE FACE (§12.3.3 item 3, §12.4.3 item 1): where
        // the words `driven by aterm (…)` moved (`(land)` to `(land,
        // landed)`, with no end told under the floor too), the windows that
        // show the session are stamped again; where only whether it landed
        // moved (a hover that names no driver), the menu bar's This Mac is
        // dirtied. Not where every strip was just stamped for an end: one
        // cleared above, or one told unseen now (`note_harness_end` stamps
        // every strip for it).
        let end_restamped = cleared || (self.end_unseen(session) && !unseen_before_end);
        if moved && !end_restamped {
            if self.driven_by_aterm(session) != hover_was {
                self.restamp_for_hover(session);
            } else if self.chip_face_landed(session) != landed_was {
                self.mark_status_dirty();
            }
        }
        // THE TASK OPENED (§11.2.3, item 2): a move from a face the window
        // knew to open or landed zeroes the work, after the end has read it
        // — never one from "unknown", a loop's restatement (the landed
        // seed's `None` → landed is a loop starting, not a new task).
        if was.is_some() && matches!(face, Some(TaskFace::Open | TaskFace::Landed)) {
            self.harness_screen
                .ab
                .task_opened(session, crate::turn_ledger::now_ms());
        }
    }

    /// A TURN BEGUN SINCE A PROVEN SELF-CLAIM'S LANDING on session `session`
    /// reopens it (Autopilot Land). Its holder types nothing after its
    /// landing until someone else's turn, so the turn is someone else's — a
    /// person's prompt, a driver's — and the work is open again, as aterm's
    /// own loop opens its face at the first live read of another's turn.
    ///
    /// BEGUN SINCE, never "running now": the landing's mark
    /// ([`crate::session_timeline::LandingMark`], taken as the `landed@v1`
    /// act arrived) holds the session's counts of turns begun, and one of
    /// them past it is a turn begun after the landing — the program's own
    /// (OSC 7501, counted by the engine as each report is parsed, read here as
    /// the last sweep read it: `StatusObserver::program_turns`) or the
    /// screen's (`AgentPublication::busy_entries`). So a turn that began and
    /// ended between two sweeps reopens it at the next, whatever the report
    /// standing then says, and neither the landing turn's own reports, a
    /// report written again after a handoff, nor a stretch of work that ran
    /// on through the landing (codex-alab reports `working` while background
    /// work runs) does. A landing with no mark (none recorded in this window)
    /// reads every turn the session has counted as after it. Nothing for any
    /// other face or claim. Called by the status sweep for a session whose
    /// counts may have moved, and as the landing's face is applied (a turn
    /// the sweep counted between the act and its face).
    ///
    /// IN THE ORDER THE PROGRAM SAW IT: codex-alab's acts reach aterm on the
    /// control socket, its turns' reports on the PTY, so its `landed@v1` can
    /// arrive before the begin of the very turn it lands (the land turn its
    /// last act typed, that act come at rest). That begin is the landing's
    /// own turn, and its mark holds it ([`landing_turns`]): it reopens
    /// nothing, and the turn after it does.
    ///
    /// A POINT LEFT TO A PERSON reopens the same way
    /// ([`holds_until_another_turn`]): its `left-to-person@v1` is marked as a
    /// landing is, and their next turn — "now implement it", a job of twenty
    /// minutes — is a turn begun past the mark, so the face reads `open` from
    /// its first counted report, as aterm's own loop reads `open` once it
    /// reads a turn it did not start, never `theirs` for the whole job until
    /// the holder's next act.
    ///
    /// WHAT THE PROGRAM'S COUNT CANNOT TELL: its report claims work, not
    /// turns. codex-alab reports `working` while it starts its MCP servers —
    /// at its launch, a `resume`, an in-TUI `/resume` or thread switch — with
    /// no turn running, in the very bytes a person's turn writes (`working`,
    /// then `idle`), so such a start after a landing reopens it while its
    /// holder still holds the landing (as a sweep that saw the start running
    /// did before the count). And a turn begun while its report already reads
    /// `working` (background work, or an active goal, running on through the
    /// landing) writes no report, so nothing reopens until the holder's next
    /// act. Only the holder can tell either apart — an act it recorded at a
    /// person's turn would reopen the landing at once, as any act but
    /// `landed@v1` does — and no screen helps: aterm reads no Codex screen.
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

    /// SESSION `session`'S TASK FACE MOVED from `was` to `face` (§11.2.3,
    /// items 1, 4 and 6): where the move is an end ([`is_a_task_end`], over
    /// the work the A/B book summed since the task opened) under Land, it is
    /// told to the session's glass as a new, unseen end, and it is JUDGED AT
    /// ONCE by the one looking rule against the frontmost window
    /// (`App::see_front`: an end in a pane someone looks at is seen as it
    /// lands, and marks nothing). Where the session's end stands unseen after
    /// that — or an unseen one it replaced was seen — every strip is stamped
    /// again and the glance dirtied (`App::restamp_for_end`): one re-stamp per
    /// end. Nothing for any other move — Cruise never ends, so it marks
    /// nothing — and no lock taken. The landing's count in the A/B book is
    /// [`Self::harness_task_moved`]'s.
    pub(crate) fn note_harness_end(
        &mut self,
        session: u64,
        was: Option<TaskFace>,
        face: Option<TaskFace>,
    ) {
        // A face that ends nothing reads no book.
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

    /// Session `session`'s task face MOVED from `was` to `face` (`None`:
    /// unknown) — the timeline holds it already, and the footer is posted by
    /// the caller (`App::apply_harness_fact`), which has read the A/B book's
    /// calendar and the session's mode (`App::note_harness_arm`). The one
    /// end is decided first ([`Self::note_harness_end`]). A landing under
    /// Land is counted in the A/B book and recorded ([`is_an_end`]: once an
    /// end, by the face it moved from; the count first, so the republish
    /// reads it), and the page is republished where a view shows it — a
    /// loop's landing marking its counts `Newer` and owing the re-read
    /// ([`Self::note_harness_act_for_page`]), as an act does.
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
        // WHAT THE HARNESS DID, STATUS's today line and the rows' day lines
        // count landings from the reading (§12.4.3, item 3), and a landing
        // reaches the window as this face, never as an act: a loop's landing
        // told while the page is on screen marks the counts `Newer` and owes
        // the re-read, as an act does. A codex-alab's own landing is in no
        // ledger the reading reads, so it moves the A/B book alone.
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
        // The title names the tab; the line names the pane too, where the tab
        // holds two agents (§10.3.3, item 9).
        let place = self.upgrade_place(&sid);
        let line_place = self.session_place(&sid);
        if let Some(record) = task_record(&sid, &place, &line_place, face) {
            self.record_message(record);
        }
    }
}

/// THE MOVE FROM `was` TO `face` IS A LANDING'S END, to be recorded once:
///
/// * to [`TaskFace::Landed`] from a face the window KNEW — never from
///   "unknown" (`None`): a loop's start face after unknown is the landing a
///   loop before it landed, seeded and restated (a restart, a seamless
///   update — whose new window knew no face — a panicked body, an Off), no
///   new landing; every landing that is new is a move from a face its
///   loop told first.
///
/// No other move is an end.
#[must_use]
pub(crate) fn is_an_end(was: Option<TaskFace>, face: TaskFace) -> bool {
    match face {
        TaskFace::Landed => was.is_some(),
        TaskFace::Open | TaskFace::Theirs => false,
    }
}

/// THE RECORD OF A LANDING'S END for session `sid` in its tab `place` (`in
/// tab 3`, `in window 2, tab 3`, `in its tab`; `App::upgrade_place`) — its
/// line led by `line_place`, which names its pane too where the tab holds two
/// agents (`in tab 3, left pane`; `App::session_place`) — a
/// `harness`-tagged RECORD, never a row, keyed `harness.landing.<sid>` (one
/// session's landings, as the log filters them; the log never replaces a
/// record by its key, so a repeat is kept out by [`is_an_end`]) — for
/// [`TaskFace::Landed`] (`Autopilot landed in tab 3`) alone; `None` for every
/// other face. The title names the tab within the glass title rule (six
/// words, 48 characters, no clause), the line says what happened and what
/// the harness does now, led by the tab.
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
