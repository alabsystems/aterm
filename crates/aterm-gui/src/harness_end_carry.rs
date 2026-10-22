// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CARRIED END (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.2.3,
//! item 8; slice 2b): what a seamless update carries of a supervised
//! session's task, so the successor shows what the outgoing process did.
//!
//! * THE END standing on the session's glass (`EndGlass`, the tab's `•`
//!   while nobody has looked): under Autopilot Land the one end, Land's
//!   landing; the wall time its face moved there, and whether it was seen.
//!   Without the carry the successor knew none of it — the loop restates the
//!   face from "unknown", which is no end (`harness_task::is_a_task_end`) —
//!   so a landing nobody had looked at lost its `•` across the update, and
//!   the restated face's clock read the update's time (`landed 17:42` became
//!   `landed 18:05`, §9.4.8's wart).
//! * THE WORK since the task opened (`AbBook::worked`, the end's 30 s floor):
//!   the book is the process's, so a task 25 s into its work would have
//!   started again from nothing (the review's R12).
//!
//! One row per session on its manifest record (`SessionRecord::autopilot_end`,
//! [`EndCarry::row`]): `end=landed@<unix ms> seen=<0|1> worked=<ms>`, or
//! `end=- worked=<ms>` where no end stands. The park stamps it
//! (`App::end_carry_rows`, beside the status item's row); the successor puts
//! the face's time on the timeline before the session is registered
//! (`spawn::seed_adopted_end`, through `SessionTimeline::seed_task_face_held`)
//! and spends the rest at the session's first publication
//! (`App::adopt_end_carry`, beside `App::adopt_status_carry`): the end onto
//! the glass as it stood, the work into the book.

use aterm_agent::harness::acts::TaskFace;

use crate::harness_glass::EndFace;

/// One session's end and work as a seamless update carries them
/// ([`Self::row`], [`Self::parse`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EndCarry {
    /// The end standing on the glass at the park, if one did.
    pub(crate) end: Option<CarriedEnd>,
    /// The agent's busy time since the task opened, ledger ms
    /// (`AbBook::worked` at the park).
    pub(crate) worked_ms: u64,
}

/// A CARRIED END: its kind, the wall time (unix ms) the task's face moved
/// there — the timeline's own, which the chip's, title's and page's clock
/// read — and whether anyone had looked at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedEnd {
    /// Landed.
    pub(crate) face: EndFace,
    /// When the task's face moved there, unix ms.
    pub(crate) at_ms: i64,
    /// Whether it was looked at (`EndGlass::unseen` was false).
    pub(crate) seen: bool,
}

impl CarriedEnd {
    /// THE FACE THE SUCCESSOR'S TIMELINE HOLDS for the loop to restate
    /// (`spawn::seed_adopted_end`) and its wall time: the end's own. The
    /// spend holds a face the loop restated first to exactly this
    /// (`App::adopt_end_carry`): a restate of the held face takes this very
    /// time back, so another face, or the same face at another time
    /// (withdrawn and read again), is not this end's.
    #[must_use]
    pub(crate) const fn told(self) -> (TaskFace, i64) {
        (self.face.task_face(), self.at_ms)
    }
}

/// The row's word for an end's kind.
#[cfg(any(unix, test))]
const fn word(face: EndFace) -> &'static str {
    match face {
        EndFace::Landed => "landed",
    }
}

impl EndCarry {
    /// THE ROW the park stamps (`SessionRecord::autopilot_end`): `None`
    /// where there is nothing to carry (no end and no work), so a session
    /// with neither — every shell, every agent at rest since its last end —
    /// writes no key, and an older reader sees the wire it always saw.
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn row(&self) -> Option<String> {
        match self.end {
            Some(end) => Some(format!(
                "end={}@{} seen={} worked={}",
                word(end.face),
                end.at_ms,
                u8::from(end.seen),
                self.worked_ms
            )),
            None if self.worked_ms > 0 => Some(format!("end=- worked={}", self.worked_ms)),
            None => None,
        }
    }

    /// READ A CARRIED ROW ([`Self::row`]), or `None` where it does not
    /// read: `end=` missing or no kind (`landed`; Autopilot Land has no other
    /// end), an end with no wall time after the epoch or no `seen=0|1`, a
    /// value that is no count, a key twice. A key this build does not
    /// know is skipped (a later build may add one); a row that does not read
    /// carries nothing — the end is lost and the work starts again, as before
    /// the carry — never the manifest.
    #[must_use]
    pub(crate) fn parse(row: &str) -> Option<Self> {
        let (mut end, mut seen, mut worked) = (None, None, None);
        for field in row.split_whitespace() {
            let (key, value) = field.split_once('=')?;
            match key {
                "end" if end.is_none() => {
                    end = Some(match value {
                        "-" => None,
                        _ => Some(face_at(value)?),
                    });
                }
                "seen" if seen.is_none() => {
                    seen = Some(match value {
                        "0" => false,
                        "1" => true,
                        _ => return None,
                    });
                }
                "worked" if worked.is_none() => worked = Some(value.parse::<u64>().ok()?),
                "end" | "seen" | "worked" => return None,
                _ => {}
            }
        }
        let end = match end? {
            Some((face, at_ms)) => Some(CarriedEnd {
                face,
                at_ms,
                seen: seen?,
            }),
            None => None,
        };
        Some(Self {
            end,
            worked_ms: worked.unwrap_or(0),
        })
    }
}

/// An end's kind and the wall time (unix ms, after the epoch) a row says it
/// moved there: `landed@<ms>`.
fn face_at(value: &str) -> Option<(EndFace, i64)> {
    let (kind, at) = value.split_once('@')?;
    let face = match kind {
        "landed" => EndFace::Landed,
        _ => return None,
    };
    let at_ms = at.parse::<i64>().ok().filter(|at| *at > 0)?;
    Some((face, at_ms))
}

#[cfg(test)]
#[path = "harness_end_carry_tests.rs"]
mod tests;
