// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless-update carry for a supervised session's task end, a proven self-claim's face, and worked time ([`EndCarry::row`]).

use aterm_agent::harness::acts::TaskFace;

use crate::harness_glass::EndFace;

/// One session's end and work as a seamless update carries them ([`Self::row`], [`Self::parse`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct EndCarry {
    /// The end standing on the glass at the park, if one did.
    pub(crate) end: Option<CarriedEnd>,
    /// A proven self-claim's face where no end stood on the glass (a landing under the work floor, a turn begun since one), and the wall time (unix ms) it moved there: nothing restates it at the successor, whose status sweep marks the task record by it (`App::reconcile_self_claim_task`).
    pub(crate) face: Option<(TaskFace, i64)>,
    /// The agent's busy time since the task opened, ledger ms
    /// (`AbBook::worked` at the park).
    pub(crate) worked_ms: u64,
}

/// A CARRIED END: its kind, the wall time (unix ms) the task's face moved there.
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
    /// THE FACE THE SUCCESSOR'S TIMELINE HOLDS for the loop to restate.
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
    /// THE ROW the park stamps (`SessionRecord::autopilot_end`).
    #[cfg(any(unix, test))]
    #[must_use]
    pub(crate) fn row(&self) -> Option<String> {
        match (self.end, self.face) {
            (Some(end), _) => Some(format!(
                "end={}@{} seen={} worked={}",
                word(end.face),
                end.at_ms,
                u8::from(end.seen),
                self.worked_ms
            )),
            (None, Some((face, at_ms))) => Some(format!(
                "end=- face={}@{at_ms} worked={}",
                face.status(),
                self.worked_ms
            )),
            (None, None) if self.worked_ms > 0 => Some(format!("end=- worked={}", self.worked_ms)),
            (None, None) => None,
        }
    }

    /// READ A CARRIED ROW ([`Self::row`]), or `None` where it does not read.
    #[must_use]
    pub(crate) fn parse(row: &str) -> Option<Self> {
        let (mut end, mut face, mut seen, mut worked) = (None, None, None, None);
        for field in row.split_whitespace() {
            let (key, value) = field.split_once('=')?;
            match key {
                "end" if end.is_none() => {
                    end = Some(match value {
                        "-" => None,
                        _ => Some(face_at(value)?),
                    });
                }
                "face" if face.is_none() => face = Some(task_face_at(value)?),
                "seen" if seen.is_none() => {
                    seen = Some(match value {
                        "0" => false,
                        "1" => true,
                        _ => return None,
                    });
                }
                "worked" if worked.is_none() => worked = Some(value.parse::<u64>().ok()?),
                "end" | "face" | "seen" | "worked" => return None,
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
            face,
            worked_ms: worked.unwrap_or(0),
        })
    }
}

fn face_at(value: &str) -> Option<(EndFace, i64)> {
    let (kind, at) = value.split_once('@')?;
    let face = match kind {
        "landed" => EndFace::Landed,
        _ => return None,
    };
    Some((face, unix_ms(at)?))
}

/// A carried `face=` value: a task face by its `status` word, at a wall time.
fn task_face_at(value: &str) -> Option<(TaskFace, i64)> {
    let (word, at) = value.split_once('@')?;
    let face = TaskFace::ALL
        .into_iter()
        .find(|face| face.status() == word)?;
    Some((face, unix_ms(at)?))
}

/// A wall time after the epoch, unix ms.
fn unix_ms(at: &str) -> Option<i64> {
    at.parse::<i64>().ok().filter(|at| *at > 0)
}

#[cfg(test)]
#[path = "harness_end_carry_tests.rs"]
mod tests;
