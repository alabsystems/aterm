// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING'S ENDS, the window's side
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.4): what a move of a
//! session's task face does past the glass the frame reads.
//!
//! The face itself — open, landed, done, spent, theirs — is the engine's
//! (`aterm_agent::harness::acts::TaskFace`), told by the session's loop as it
//! moves and kept on the session's timeline for `status task=` and in the
//! harness's glass for the chip (`App::apply_harness_fact`). A move here:
//!
//! * REPUBLISHES Settings ▸ Harness where a view shows it: each SESSIONS
//!   row's state line reads the face (`landed 17:42`, `done 17:42`,
//!   `quiet: …`). One check, nothing built, while no view shows the page.
//! * RECORDS the two ends a person may want to read later, and only under
//!   Land: a landing that LANDED (`Autopilot landed in tab 3`) and one SPENT
//!   with no done reply (`Autopilot quiet in tab 3`). Each is a record in the
//!   message log, never a row and never an exceptions `Entry` (§9.4.3, the
//!   alert rule): a landing that finished did what the owner asked, and one
//!   spent is the usual end of a landing whose token the done readers
//!   missed — nobody is needed now, and there is no one action. Every other
//!   move (open, done, a person's own exchange, unknown) is no record: the
//!   chip, the page and `status` say it.
//! * ONE RECORD AN END, judged by the face it moved FROM ([`task_record`]):
//!   the log never replaces a record by its key, so a face that comes back
//!   without a new end is no record. A landing restated by a loop's start
//!   (the face after "unknown": a restart, a seamless update, a panicked
//!   body, an Off, each of which seeds the landing) is none; nor is a spent
//!   landing come back from a person's exchange (`theirs`) or a done task —
//!   a landing is spent where its last land act's turn ENDED with no done,
//!   which the engine tells as a move from open, once an episode.
//!
//! The card's line follows the face through the footer's repaint key, which
//! carries it (`claude_card::LineEnd`); it arms no episode.

use aterm_agent::harness::acts::TaskFace;
use aterm_agent::supervise::policy::turn_end::LAND_NUDGES;
use aterm_messages::{Message, Severity, tags};

use crate::App;
use crate::autopilot::AutopilotView;

impl App {
    /// Session `session`'s task face MOVED from `was` to `face` (`None`:
    /// unknown) — the timeline holds it already, and the footer is posted by
    /// the caller (`App::apply_harness_fact`). The page is republished where
    /// a view shows it, and a landing that landed or was spent under Land is
    /// recorded ([`task_record`]: once an end, by the face it moved from).
    pub(crate) fn harness_task_moved(
        &mut self,
        session: u64,
        was: Option<TaskFace>,
        face: Option<TaskFace>,
    ) {
        self.harness_screen_moved();
        let Some(face) = face else {
            return;
        };
        if !is_an_end(was, face) {
            return;
        }
        if self.session_autopilot(session).1 != AutopilotView::Land {
            return;
        }
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
///   loop told first;
/// * to [`TaskFace::Spent`] from [`TaskFace::Open`] alone: the engine tells
///   a landing spent where its last land act's turn ended with no done, a
///   move from open once an episode; spent come back from a person's own
///   exchange (`theirs`) or a done task is the same spent landing.
///
/// No other move is an end.
#[must_use]
pub(crate) fn is_an_end(was: Option<TaskFace>, face: TaskFace) -> bool {
    match face {
        TaskFace::Landed => was.is_some(),
        TaskFace::Spent => was == Some(TaskFace::Open),
        TaskFace::Open | TaskFace::Done | TaskFace::Theirs => false,
    }
}

/// THE RECORD OF A LANDING'S END for session `sid` in its tab `place` (`in
/// tab 3`, `in window 2, tab 3`, `in its tab`; `App::upgrade_place`) — its
/// line led by `line_place`, which names its pane too where the tab holds two
/// agents (`in tab 3, left pane`; `App::session_place`) — a
/// `harness`-tagged RECORD, never a row, keyed `harness.landing.<sid>` (one
/// session's landings, as the log filters them; the log never replaces a
/// record by its key, so a repeat is kept out by [`is_an_end`]) — for
/// [`TaskFace::Landed`] (`Autopilot landed in tab 3`) and
/// [`TaskFace::Spent`] (`Autopilot quiet in tab 3`) alone; `None` for every
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
            "the agent finished after the land instruction; the harness types nothing more \
             until someone else's turn"
                .to_string(),
        ),
        TaskFace::Spent => (
            "Autopilot quiet",
            format!(
                "no done reply after the land instruction and {LAND_NUDGES} nudges; the harness \
                 is quiet, but for a land answer to a question"
            ),
        ),
        TaskFace::Open | TaskFace::Done | TaskFace::Theirs => return None,
    };
    Some(
        Message::new(tags::HARNESS, Severity::Info, placed(head, place))
            .line(atpkg::progress::sanitize_for_tty(
                &tab_led(line_place, &what),
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

/// `{Place}: {text}` (`Tab 3: the agent finished …`), or `text` alone for a
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
