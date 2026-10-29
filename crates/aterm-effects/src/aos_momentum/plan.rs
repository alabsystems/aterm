// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Schedule plans: closed, validated candidate data and the first-party interpreter for it.
//!
//! # The bytes
//!
//! ```text
//! byte 0      format = PLAN_FORMAT (1)
//! byte 1      split: 0 Shipped | 1 SettleAtStep
//! byte 2      wake:  0 NextStep | 1 ColdOnly | 2 Poll | 3 Cadence | 4 Never
//! byte 3      reserved = 0
//! bytes 4..8  u32 LE gap_us, 0..=1_000_000 (SettleAtStep), else 0
//! bytes 8..12 u32 LE period_us, 1_000..=1_000_000 (Poll), else 0
//! ```

use core::fmt;
use std::time::Duration;

use aterm_aos::wire::Enc;
use aterm_aos::{ArtifactId, ArtifactKind, Digest};
use aterm_time::Instant;

use super::SETTLE_MARGIN;
use crate::cursor_momentum::MomentumGlow;

/// The plan format this interpreter reads (byte 0).
pub const PLAN_FORMAT: u8 = 1;

/// A plan's length in bytes.
pub const PLAN_LEN: usize = 12;

/// The domain under which [`SchedulePlan::digest`] digests a plan's bytes.
pub const PLAN_DOMAIN: &[u8] = b"aterm-aos-m4-plan-v1\0";

/// The largest `SettleAtStep` gap, in microseconds (one second).
pub const MAX_GAP_US: u32 = 1_000_000;

/// The shortest `Poll` period, in microseconds (one millisecond).
pub const MIN_PERIOD_US: u32 = 1_000;

/// The longest `Poll` period, in microseconds (one second).
pub const MAX_PERIOD_US: u32 = 1_000_000;

/// The planned code artifact's name.
pub const PLAN_ARTIFACT_NAME: &str = "momentum-glow.schedule-plan";

/// The short name a planned code reports in a status line.
pub const PLAN_NAME: &str = "schedule-plan";

/// The interpreter a planned code's descriptor names, with its version.
pub const PLAN_INTERPRETER: &str = "plan interpreter v1";

/// What a planned code's artifact declares about itself.
pub const PLAN_NOTE: &str = "the prebuilt first-party schedule-plan interpreter \
     (aterm_effects::aos_momentum::plan) reading the plan bytes below for is_active, settling \
     and next_change_deadline; admission, keys, resets, ticks, cold_at and the entry invariant \
     are the shipped engine's";

/// Which frames of a hot glow read active (frame cadence) rather than settling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Split {
    /// The shipped classification: active while hot and the last two frames differ.
    Shipped,
    /// Settle a changed hot frame at once when its settle forecast is at least `gap_us` away.
    SettleAtStep {
        /// `0..=MAX_GAP_US` microseconds.
        gap_us: u32,
    },
}

/// The wake a settling glow names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wake {
    /// The shipped formula: the forecast plus the settle margin, never sooner than a frame out.
    NextStep,
    /// `max(cold_at + SETTLE_MARGIN, now + frame_interval)`: sleeps across every step.
    ColdOnly,
    /// `now + period_us`.
    Poll {
        /// `MIN_PERIOD_US..=MAX_PERIOD_US` microseconds.
        period_us: u32,
    },
    /// `now + frame_interval`: the frame cadence.
    Cadence,
    /// No wake.
    Never,
}

/// A decoded schedule plan, built only by [`SchedulePlan::decode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SchedulePlan {
    split: Split,
    wake: Wake,
}

/// The plan that is the shipped engine: `{Shipped, NextStep}`.
pub const SHIPPED_PLAN: SchedulePlan = SchedulePlan {
    split: Split::Shipped,
    wake: Wake::NextStep,
};

/// The plan that is the demonstration fault `demo-stranded-settle`, `{Shipped, Never}`.
pub const DEMO_PLAN: SchedulePlan = SchedulePlan {
    split: Split::Shipped,
    wake: Wake::Never,
};

/// A field of the plan's bytes, as a refusal names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanField {
    /// The byte string's length.
    Length,
    /// Byte 0.
    Format,
    /// Byte 1.
    Split,
    /// Byte 2.
    Wake,
    /// Byte 3.
    Reserved,
    /// Bytes 4..8.
    Gap,
    /// Bytes 8..12.
    Period,
}

impl PlanField {
    /// The field's name in a refusal.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            PlanField::Length => "length",
            PlanField::Format => "format",
            PlanField::Split => "split",
            PlanField::Wake => "wake",
            PlanField::Reserved => "reserved",
            PlanField::Gap => "gap_us",
            PlanField::Period => "period_us",
        }
    }
}

/// Why a byte string is not a schedule plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanRefusal {
    /// Not [`PLAN_LEN`] bytes.
    Length {
        /// [`PLAN_LEN`].
        expected: usize,
        /// The length found.
        found: usize,
    },
    /// A format this interpreter does not read.
    Format {
        /// The byte found.
        found: u8,
    },
    /// A split tag no split uses.
    SplitTag {
        /// The byte found.
        tag: u8,
    },
    /// A wake tag no wake uses.
    WakeTag {
        /// The byte found.
        tag: u8,
    },
    /// The reserved byte is not zero.
    Reserved {
        /// The byte found.
        found: u8,
    },
    /// A `SettleAtStep` gap past [`MAX_GAP_US`].
    Gap {
        /// The gap found, in microseconds.
        gap_us: u32,
    },
    /// A `Poll` period outside `MIN_PERIOD_US..=MAX_PERIOD_US`.
    Period {
        /// The period found, in microseconds.
        period_us: u32,
    },
    /// A parameter the tags do not use is not zero, so another encoding would name the same plan.
    Unused {
        /// [`PlanField::Gap`] or [`PlanField::Period`].
        field: PlanField,
        /// The value found.
        value: u32,
    },
}

impl PlanRefusal {
    /// The field the bytes break.
    #[inline]
    #[must_use]
    pub const fn field(&self) -> PlanField {
        match *self {
            PlanRefusal::Length { .. } => PlanField::Length,
            PlanRefusal::Format { .. } => PlanField::Format,
            PlanRefusal::SplitTag { .. } => PlanField::Split,
            PlanRefusal::WakeTag { .. } => PlanField::Wake,
            PlanRefusal::Reserved { .. } => PlanField::Reserved,
            PlanRefusal::Gap { .. } => PlanField::Gap,
            PlanRefusal::Period { .. } => PlanField::Period,
            PlanRefusal::Unused { field, .. } => field,
        }
    }
}

impl fmt::Display for PlanRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            PlanRefusal::Length { expected, found } => {
                write!(f, "schedule plan refused: {found} bytes, not {expected}")
            }
            PlanRefusal::Format { found } => write!(
                f,
                "schedule plan refused: format {found} is not {PLAN_FORMAT}"
            ),
            PlanRefusal::SplitTag { tag } => {
                write!(f, "schedule plan refused: split tag {tag} names no split")
            }
            PlanRefusal::WakeTag { tag } => {
                write!(f, "schedule plan refused: wake tag {tag} names no wake")
            }
            PlanRefusal::Reserved { found } => {
                write!(f, "schedule plan refused: reserved byte {found} is not 0")
            }
            PlanRefusal::Gap { gap_us } => write!(
                f,
                "schedule plan refused: gap {gap_us} us passes {MAX_GAP_US} us"
            ),
            PlanRefusal::Period { period_us } => write!(
                f,
                "schedule plan refused: period {period_us} us is outside {MIN_PERIOD_US}..={MAX_PERIOD_US} us"
            ),
            PlanRefusal::Unused { field, value } => write!(
                f,
                "schedule plan refused: {} is {value}, and the plan's tags do not use it",
                field.name()
            ),
        }
    }
}

impl std::error::Error for PlanRefusal {}

/// The bytes of `(split, wake)`, whether or not they decode.
#[inline]
const fn raw(split: Split, wake: Wake) -> [u8; PLAN_LEN] {
    let (s, gap) = match split {
        Split::Shipped => (0, 0),
        Split::SettleAtStep { gap_us } => (1, gap_us),
    };
    let (w, period) = match wake {
        Wake::NextStep => (0, 0),
        Wake::ColdOnly => (1, 0),
        Wake::Poll { period_us } => (2, period_us),
        Wake::Cadence => (3, 0),
        Wake::Never => (4, 0),
    };
    let g = gap.to_le_bytes();
    let p = period.to_le_bytes();
    [
        PLAN_FORMAT,
        s,
        w,
        0,
        g[0],
        g[1],
        g[2],
        g[3],
        p[0],
        p[1],
        p[2],
        p[3],
    ]
}

impl SchedulePlan {
    /// The plan `(split, wake)`, through [`Self::decode`] of its bytes.
    /// # Errors
    /// [`PlanRefusal::Gap`], [`PlanRefusal::Period`]: a parameter outside its range.
    #[inline]
    pub const fn new(split: Split, wake: Wake) -> Result<SchedulePlan, PlanRefusal> {
        Self::decode(&raw(split, wake))
    }

    /// The plan `bytes` encode, or the first field they break, in byte order.
    /// # Errors
    /// [`PlanRefusal::Length`], then [`PlanRefusal::Format`], [`PlanRefusal::SplitTag`],
    /// [`PlanRefusal::WakeTag`], [`PlanRefusal::Reserved`], the gap ([`PlanRefusal::Gap`] or
    /// [`PlanRefusal::Unused`]) and the period ([`PlanRefusal::Period`] or
    /// [`PlanRefusal::Unused`]).
    #[inline]
    pub const fn decode(bytes: &[u8]) -> Result<SchedulePlan, PlanRefusal> {
        if bytes.len() != PLAN_LEN {
            return Err(PlanRefusal::Length {
                expected: PLAN_LEN,
                found: bytes.len(),
            });
        }
        let format = bytes[0];
        let split = bytes[1];
        let wake = bytes[2];
        let reserved = bytes[3];
        if format != PLAN_FORMAT {
            return Err(PlanRefusal::Format { found: format });
        }
        let settle_at_step = match split {
            0 => false,
            1 => true,
            tag => return Err(PlanRefusal::SplitTag { tag }),
        };
        let wake = match wake {
            0 => Some(Wake::NextStep),
            1 => Some(Wake::ColdOnly),
            2 => None,
            3 => Some(Wake::Cadence),
            4 => Some(Wake::Never),
            tag => return Err(PlanRefusal::WakeTag { tag }),
        };
        if reserved != 0 {
            return Err(PlanRefusal::Reserved { found: reserved });
        }
        let gap_us = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let split = if settle_at_step {
            if gap_us > MAX_GAP_US {
                return Err(PlanRefusal::Gap { gap_us });
            }
            Split::SettleAtStep { gap_us }
        } else {
            if gap_us != 0 {
                return Err(PlanRefusal::Unused {
                    field: PlanField::Gap,
                    value: gap_us,
                });
            }
            Split::Shipped
        };
        let period_us = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let wake = match wake {
            None => {
                if period_us < MIN_PERIOD_US || period_us > MAX_PERIOD_US {
                    return Err(PlanRefusal::Period { period_us });
                }
                Wake::Poll { period_us }
            }
            Some(wake) => {
                if period_us != 0 {
                    return Err(PlanRefusal::Unused {
                        field: PlanField::Period,
                        value: period_us,
                    });
                }
                wake
            }
        };
        Ok(SchedulePlan { split, wake })
    }

    /// The plan's canonical bytes.
    #[inline]
    #[must_use]
    pub const fn encode(&self) -> [u8; PLAN_LEN] {
        raw(self.split, self.wake)
    }

    /// `Digest::of(PLAN_DOMAIN, [the plan's bytes])`: the plan's identity.
    #[inline]
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(PLAN_DOMAIN, &[&self.encode()])
    }

    /// Its split.
    #[inline]
    #[must_use]
    pub const fn split(&self) -> Split {
        self.split
    }

    /// Its wake.
    #[inline]
    #[must_use]
    pub const fn wake(&self) -> Wake {
        self.wake
    }

    /// Build the [`PLAN_ARTIFACT_NAME`] v1 artifact with the plan's bytes and host `build` id.
    #[inline]
    #[must_use]
    pub fn artifact(&self, build: &str) -> ArtifactId {
        let mut e = Enc::with_capacity(384 + build.len());
        e.str(PLAN_ARTIFACT_NAME);
        e.u32(1);
        e.str(build);
        e.str(PLAN_NOTE);
        e.u8(PLAN_FORMAT);
        e.bytes(&self.encode());
        e.str(PLAN_INTERPRETER);
        ArtifactId::new(ArtifactKind::Code, PLAN_ARTIFACT_NAME, 1, e.as_bytes())
    }

    /// Whether a changed hot frame settles at once under this plan's split.
    #[inline]
    fn settles_at_step(&self, v: &PlanView) -> bool {
        match (self.split, v.gap()) {
            (Split::SettleAtStep { gap_us }, Some(gap)) => {
                gap >= Duration::from_micros(u64::from(gap_us))
            }
            _ => false,
        }
    }

    /// Frame cadence: `hot ∧ changed ∧ ¬(SettleAtStep{g} ∧ gap ≥ g)`.
    #[inline]
    #[must_use]
    pub(crate) fn is_active(&self, v: &PlanView) -> bool {
        v.hot && v.changed && !self.settles_at_step(v)
    }

    /// `hot ∧ ¬active`: disjoint from [`Self::is_active`], their union hot.
    #[inline]
    #[must_use]
    pub(crate) fn settling(&self, v: &PlanView) -> bool {
        v.hot && !self.is_active(v)
    }

    /// The wake this plan names.
    #[inline]
    #[must_use]
    pub(crate) fn next_change_deadline(
        &self,
        v: &PlanView,
        now: Instant,
        frame_interval: Duration,
    ) -> Option<Instant> {
        if !v.hot {
            return None;
        }
        if self.is_active(v) {
            return now.checked_add(frame_interval);
        }
        match self.wake {
            Wake::NextStep => {
                let floor = now.checked_add(frame_interval)?;
                Some(match v.forecast {
                    Some(at) => match at.checked_add(SETTLE_MARGIN) {
                        Some(w) if w > floor => w,
                        _ => floor,
                    },
                    None => floor,
                })
            }
            Wake::ColdOnly => {
                let floor = now.checked_add(frame_interval)?;
                Some(match v.cold_at {
                    Some(cold) => match cold.checked_add(SETTLE_MARGIN) {
                        Some(w) if w > floor => w,
                        _ => floor,
                    },
                    None => floor,
                })
            }
            Wake::Poll { period_us } => {
                now.checked_add(Duration::from_micros(u64::from(period_us)))
            }
            Wake::Cadence => now.checked_add(frame_interval),
            Wake::Never => None,
        }
    }
}

/// What the first-party engine shows the interpreter, computed by the engine's own functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PlanView {
    /// The last real tick latched a hot amplitude.
    hot: bool,
    /// The last two frames' fingerprints differ.
    changed: bool,
    /// The earlier of the next `u8` step and `cold_at`.
    forecast: Option<Instant>,
    /// The instant of the tick that latched the amplitude.
    stamp: Option<Instant>,
    /// The analytic cold instant.
    cold_at: Option<Instant>,
}

impl PlanView {
    /// The view of `glow`.
    #[inline]
    #[must_use]
    pub(crate) fn of(glow: &MomentumGlow) -> PlanView {
        PlanView {
            hot: glow.hot(),
            changed: glow.changed(),
            forecast: glow.settle_forecast(),
            stamp: glow.stamp(),
            cold_at: glow.cold_at(),
        }
    }

    /// A view of these fields, as the interpreter's conformance vectors spell them.
    #[cfg(test)]
    #[inline]
    #[must_use]
    pub(crate) const fn new(
        hot: bool,
        changed: bool,
        forecast: Option<Instant>,
        stamp: Option<Instant>,
        cold_at: Option<Instant>,
    ) -> PlanView {
        PlanView {
            hot,
            changed,
            forecast,
            stamp,
            cold_at,
        }
    }

    /// The forecast's gap from the stamp (`None` without a forecast).
    #[inline]
    fn gap(&self) -> Option<Duration> {
        self.forecast?.checked_duration_since(self.stamp?)
    }

    #[inline]
    #[must_use]
    pub(crate) const fn cold_at(&self) -> Option<Instant> {
        self.cold_at
    }
}
