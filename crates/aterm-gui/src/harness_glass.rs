// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HARNESS'S GLASS, per session (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §9.2, §9.4): what the Autopilot chip shows beside the session's word —
//! the act the harness just took, lit for a moment, the trail of the last
//! few, and the task's face under Land.
//!
//! The App keeps one [`SessionGlass`] per session that has any
//! (`App::harness_glass`, by local id), and the frame reads it beside the
//! session's Autopilot, once a frame, as a `Copy` [`ChipGlass`]
//! (`App::chip_glass`): no lock, no clock, no allocation. An entry goes with
//! its session (`App::forget_harness_glass`, from
//! `retire_session_registration`).
//!
//! THE SEAM (design §9.6.0). What arrives here is told by the session's
//! loop through the window's host (`Wake::HarnessFact`,
//! `App::apply_harness_fact`):
//!
//! * an ACT ([`on_act`]) keeps nothing yet: the act ticker — its coalescing,
//!   its dwell and quiet, its trail and their clock times, and the one
//!   deadline that ends a label ([`App::harness_acts_deadline`]) — is slice
//!   A's (§9.6.1), and until it lands no label lights and nothing is armed;
//! * the TASK'S FACE ([`on_task`]) is kept here as the frame reads it, its
//!   clock time formatted once as it moves, beside the timeline's copy that
//!   `status task=` reads under the lock it already takes.

use std::time::Instant;

use aterm_agent::harness::acts::{Act, TaskFace};

use crate::App;
use crate::claude_lights::{ChipGlass, Clock};

/// One session's harness glass, as the frame reads it ([`Self::chip`]).
/// The default is v1's chip; an entry at the default is not kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionGlass {
    /// The task's face as the session's loop last told it (§9.4.2), and the
    /// local clock time it moved there.
    task: Option<(TaskFace, Option<Clock>)>,
}

impl SessionGlass {
    /// What the Autopilot chip reads of it this frame: a copy.
    #[must_use]
    pub(crate) fn chip(&self) -> ChipGlass {
        ChipGlass {
            task: self.task.map(|(face, _)| face),
            task_at: self.task.and_then(|(_, at)| at),
            ..ChipGlass::default()
        }
    }
}

/// An ACT the harness took on session `session` (`HarnessFact::Act`): S0
/// keeps nothing, so no label lights and no trail grows (the act ticker is
/// design §9.6.1's). Never a record: the ledger is the record of every act.
pub(crate) fn on_act(app: &mut App, session: u64, act: Act) {
    let _ = (app, session, act);
}

/// The TASK'S FACE of session `session` moved to `face` at `at_ms` (unix
/// ms; `None`: unknown), as the timeline now holds it: kept for the frame
/// with its local clock time, formatted here once. An unknown face leaves no
/// entry behind. Whether the frame's glass moved.
pub(crate) fn on_task(app: &mut App, session: u64, face: Option<TaskFace>, at_ms: i64) -> bool {
    let task = face.map(|face| (face, Clock::at(at_ms.div_euclid(1000))));
    let was = app.harness_glass.get(&session).copied().unwrap_or_default();
    let mut now = was;
    now.task = task;
    if now == was {
        return false;
    }
    if now == SessionGlass::default() {
        app.harness_glass.remove(&session);
    } else {
        app.harness_glass.insert(session, now);
    }
    true
}

impl App {
    /// When the harness's glass next needs the loop
    /// (`metrics::DeadlineOwner::HarnessActs`): the end of a label on show,
    /// or a re-read Settings ▸ Harness owes. `None`: no label is lit and no
    /// read is owed, so nothing is folded and no clock is read.
    #[must_use]
    pub(crate) fn harness_acts_deadline(&self) -> Option<Instant> {
        None
    }

    /// The labels whose dwell ended go, and their footers are posted. With
    /// no label lit, nothing is read.
    pub(crate) fn expire_harness_acts(&mut self) {}

    /// Session `id` is retired: its glass goes with it.
    pub(crate) fn forget_harness_glass(&mut self, id: u64) {
        self.harness_glass.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A face told is kept with its clock; told again it is no move; told
    /// unknown it leaves no entry; and an act keeps nothing.
    #[test]
    fn the_task_face_is_kept_for_the_frame_and_an_act_keeps_nothing() {
        let mut app = App::headless_for_test();
        on_act(&mut app, 7, Act::Approved);
        assert!(app.harness_glass.is_empty(), "an act keeps nothing in S0");
        assert_eq!(app.chip_glass(7), ChipGlass::default());

        assert!(on_task(
            &mut app,
            7,
            Some(TaskFace::Landed),
            1_792_249_200_000
        ));
        assert_eq!(app.chip_glass(7).task, Some(TaskFace::Landed));
        assert!(!on_task(
            &mut app,
            7,
            Some(TaskFace::Landed),
            1_792_249_200_000
        ));
        assert!(on_task(&mut app, 7, None, 1_792_249_260_000));
        assert!(app.harness_glass.is_empty(), "unknown keeps no entry");
        assert_eq!(app.chip_glass(7), ChipGlass::default());

        on_task(&mut app, 8, Some(TaskFace::Spent), 0);
        app.forget_harness_glass(8);
        assert!(app.harness_glass.is_empty());
        assert_eq!(app.harness_acts_deadline(), None);
    }
}
