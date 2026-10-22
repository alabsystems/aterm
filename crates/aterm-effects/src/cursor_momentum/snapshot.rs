// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The momentum glow's semantic snapshot: [`MomentumGlow`]'s state as plain
//! values, their canonical bytes, and a restore that refuses every state that
//! breaks an invariant the engine's transitions keep (or a bound of the hosted
//! profile).
//!
//! # What the snapshot holds
//!
//! [`MomentumGlowSnapshot`] has one field for each piece of state the engine's
//! future reads: the typing rate (`value`, `at`), the two-frame fingerprint
//! history (`fp_last`, `fp_prev`), and the latched amplitude with its stamp and
//! τ (`latched`, `stamp`, `tau`). A float is kept as its IEEE bits, so a round
//! trip is bit exact. `tau` is kept raw even while it is dead, that is while
//! nothing is latched. An instant is kept as a signed nanosecond offset from an
//! epoch `Instant` that the caller names (`i128`, so `TypingRate::at` may predate
//! the epoch).
//!
//! The engine reads time only through differences:
//! `saturating_duration_since`, `stamp + τ·ln(…)`, comparisons and `min`. So a
//! state restored onto another epoch, with its later inputs shifted by the same
//! amount, behaves identically (`a_snapshot_restored_onto_another_epoch_continues_exactly`).
//!
//! # What it leaves out, and why
//!
//! It leaves out `light_ink_memo`. That field caches the light page's
//! `(ink, text_alpha)`, keyed by `(hue, theme_fg, theme_bg)` with the alpha byte
//! masked off, and restore leaves it `None`. That leaving it out changes nothing
//! is tested and argued, not proven. The argument: on a miss the value is
//! computed again from the same key, and every function behind it masks the
//! alpha byte too, so a hit and a miss return the same pair. The test: a cold
//! cache draws byte for byte what a warm one does on the light-page fixture
//! (`a_restored_engine_with_a_cold_memo_draws_the_light_page_byte_for_byte`),
//! and a poisoned cache shows the test would notice a difference.
//!
//! # Encoding, schema [`MOMENTUM_GLOW_SNAPSHOT_SCHEMA`] = 1
//!
//! The layout is fixed, little-endian and [`MomentumGlowSnapshot::LEN`] = 64
//! bytes long:
//!
//! | offset | bytes | field | encoding |
//! |---|---|---|---|
//! | 0 | 2 | schema | `u16`, 1 |
//! | 2 | 4 | `rate_value` | `f32` bits |
//! | 6 | 1 | `rate_at` tag | 0 = `None`, 1 = `Some` |
//! | 7 | 16 | `rate_at` | `i128` ns from the epoch; all zero under tag 0 |
//! | 23 | 8 | `fp_last` | `u64` |
//! | 31 | 8 | `fp_prev` | `u64` |
//! | 39 | 4 | `latched` | `f32` bits |
//! | 43 | 1 | `stamp` tag | 0 = `None`, 1 = `Some` |
//! | 44 | 16 | `stamp` | `i128` ns from the epoch; all zero under tag 0 |
//! | 60 | 4 | `tau` | `f32` bits |
//!
//! Each value has exactly one encoding. [`MomentumGlowSnapshot::decode`] reads
//! the fields in layout order and refuses the first fault it meets:
//! * a length other than 64 bytes ([`SnapshotRefusal::Length`]);
//! * a schema other than 1 ([`SnapshotRefusal::Schema`]);
//! * an option tag other than 0 or 1 ([`SnapshotRefusal::Tag`]);
//! * a non-zero payload under tag 0 ([`SnapshotRefusal::NonCanonical`]);
//! * a NaN float in any pattern ([`SnapshotRefusal::NotANumber`]);
//! * `-0.0` ([`SnapshotRefusal::NegativeZero`]).
//!
//! The engine never stores NaN or `-0.0`, so these patterns have no canonical
//! meaning. Decode checks bytes only. Whether the state is reachable is
//! [`MomentumGlowSnapshot::validate`]'s job.
//!
//! # What restore refuses
//!
//! [`MomentumGlow::restore`] first runs [`MomentumGlowSnapshot::validate`]. It
//! refuses every state that breaks an invariant the engine's own transitions
//! (`on_key`, `tick`, its inert arm, and `reset`) keep, and every state outside
//! two bounds of the hosted profile. These are necessary conditions, not a
//! decision of reachability: some states it accepts are produced by no input
//! sequence (a fingerprint is checked only for being 0 or odd, and the latch is
//! not checked against the rate it was drawn from).
//!
//! The invariants, each kept by every transition from the default state:
//! * `rate_value` is `+0.0`, exactly [`MOMENTUM_GLOW_KEY_CREDIT`], or in
//!   `[fl(SNAP_ZERO + MOMENTUM_GLOW_KEY_CREDIT), 1]`, where `fl` is `f32`
//!   rounding (about 0.105). A key stores `fl(v + 0.1)` clamped to 1, where the
//!   decayed value `v` reads exactly 0 or at least `SNAP_ZERO` (0.005), and
//!   `f32` addition is monotone; nothing else writes a non-zero rate;
//! * `rate_at` is present exactly when `rate_value ≠ 0`;
//! * `latched` is `+0.0` or lies in `[MOMENTUM_GLOW_HOT, 1]`, because only a hot
//!   tick latches, and it clamps to 1;
//! * `latched = 0` ⇔ `stamp = None` ⇔ `fp_last = 0`, because a hot tick sets all
//!   three and the inert arm clears all three;
//! * each fingerprint is 0 or odd, because a hot fingerprint is `… | 1`;
//! * a latched glow has a non-resting rate, because only `reset` clears the rate
//!   and it clears the latch with it;
//! * `tau` is finite and non-negative, and positive while latched: a hot tick
//!   passed the rate's own `τ > 0` gate;
//! * at rest (`rate_value = 0`), `fp_prev = 0` and `tau = 0`: only the default
//!   state and `reset` reach rest, both zero every field, and every tick from
//!   rest is inert, which copies `fp_last = 0` into `fp_prev` and leaves τ
//!   alone. And a non-zero `fp_prev` has a non-zero `tau` beside it: it is a hot
//!   tick's fingerprint, that tick set τ to a positive value, and only `reset`,
//!   which zeroes both, sets τ to 0.
//!
//! The profile's bounds, which the engine does not keep by itself but which no
//! hosted input crosses:
//! * while latched, `tau ≤ MOMENTUM_PROFILE.max_tau_s`: hosted ticks pass
//!   [`super::admit_tau`] (production τ is 1.2 s);
//! * a positive `rate_at` or `stamp` offset is at most
//!   `MOMENTUM_PROFILE.horizon` ([`SnapshotRefusal::Horizon`]): `admit_time`
//!   keeps every hosted input in `[0, horizon]`. A negative offset of any
//!   magnitude passes: a glow attached after it ran, or snapshotted onto a later
//!   epoch, holds instants from before its epoch.
//!
//! Positive finite `f32`s order as their bits do, so each range check is an
//! integer comparison on the bits. That also refuses NaN, infinities, `-0.0`
//! and every negative value, whether the snapshot came from
//! [`MomentumGlowSnapshot::decode`] or was built by hand.
//!
//! Restore then maps each offset back onto the restoring host's epoch with
//! `Instant::checked_add` or `Instant::checked_sub`. It refuses
//! ([`SnapshotRefusal::Instant`]) an offset that falls outside that host's
//! `Instant` range, and a latched stamp from which the settle schedule could
//! not be formed: `epoch + stamp + τ·ln 255 + SETTLE_MARGIN` must be an
//! `Instant`. Every instant [`MomentumGlow::cold_at`] and
//! [`MomentumGlow::next_change_deadline`] derive from the stamp is at most that
//! (their decay addend is `τ·ln(latched / level)` with `latched / level` at most
//! 50, and `ln 50 < ln 255`), so neither overflows after a restore, whatever
//! the epoch. `now + frame_interval` is formed from the caller's own `now`.
//!
//! A refusal leaves everything untouched. Snapshot only reads, decode is a pure
//! function of the bytes, and restore builds a new engine only on success. None
//! of the three panics on any input.

use std::fmt;
use std::time::Duration;

use aterm_time::Instant;

use super::{
    MOMENTUM_ENVELOPE, MOMENTUM_GLOW_HOT, MOMENTUM_GLOW_KEY_CREDIT, MOMENTUM_PROFILE, MomentumGlow,
    SETTLE_MARGIN, SNAP_ZERO, TypingRate,
};

/// The schema of [`MomentumGlowSnapshot`]'s encoding. Bump it only when the
/// layout or the meaning of a field changes; a decoder refuses any other
/// number.
pub const MOMENTUM_GLOW_SNAPSHOT_SCHEMA: u16 = 1;

/// The least rate a key stores over a value that has not snapped to 0:
/// `SNAP_ZERO + MOMENTUM_GLOW_KEY_CREDIT`, rounded in `f32` as `on_key`
/// rounds it (about 0.105). A key over a value that reads 0 stores exactly
/// [`MOMENTUM_GLOW_KEY_CREDIT`].
pub(super) const LIVE_CREDIT_FLOOR: f32 = SNAP_ZERO + MOMENTUM_GLOW_KEY_CREDIT;

/// One field of [`MomentumGlowSnapshot`], as a refusal names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotField {
    RateValue,
    RateAt,
    FpLast,
    FpPrev,
    Latched,
    Stamp,
    Tau,
}

impl SnapshotField {
    /// The field's name in [`MomentumGlowSnapshot`].
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RateValue => "rate_value",
            Self::RateAt => "rate_at",
            Self::FpLast => "fp_last",
            Self::FpPrev => "fp_prev",
            Self::Latched => "latched",
            Self::Stamp => "stamp",
            Self::Tau => "tau",
        }
    }
}

impl fmt::Display for SnapshotField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// [`MomentumGlow`]'s semantic state, fieldwise. `light_ink_memo` is a pure
/// cache: it is left out, and [`MomentumGlow::restore`] sets it to `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MomentumGlowSnapshot {
    /// `TypingRate::value`, as `f32` bits.
    pub rate_value: u32,
    /// `TypingRate::at − epoch`, in signed nanoseconds.
    pub rate_at: Option<i128>,
    /// The last tick's fingerprint (0 when that tick was inert).
    pub fp_last: u64,
    /// The fingerprint of the tick before it.
    pub fp_prev: u64,
    /// The last hot tick's post-intensity amplitude, as `f32` bits (0 once inert).
    pub latched: u32,
    /// That tick's instant `− epoch`, in signed nanoseconds.
    pub stamp: Option<i128>,
    /// That tick's τ, as `f32` bits, kept raw even while it is dead.
    pub tau: u32,
}

/// Why a snapshot could not be taken, decoded, validated or restored. Every
/// refusal leaves the engine and the bytes untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotRefusal {
    /// Snapshot: an instant's distance from the epoch does not fit in `i128`
    /// nanoseconds. No std `Duration` is that long, so this is a fail-closed
    /// branch that no supported target reaches.
    OffsetUnrepresentable { field: SnapshotField },
    /// Decode: the input is not [`MomentumGlowSnapshot::LEN`] bytes.
    Length { expected: usize, found: usize },
    /// Decode: the schema is not [`MOMENTUM_GLOW_SNAPSHOT_SCHEMA`].
    Schema { found: u16 },
    /// Decode: an option tag is neither 0 (`None`) nor 1 (`Some`).
    Tag { field: SnapshotField, tag: u8 },
    /// Decode: a `None` tag carries a non-zero payload.
    NonCanonical { field: SnapshotField },
    /// Decode: a float is NaN, in any of its bit patterns.
    NotANumber { field: SnapshotField, bits: u32 },
    /// Decode: a float is `-0.0`, which the engine never stores.
    NegativeZero { field: SnapshotField },
    /// Validate: `rate_value` is neither `+0.0`, nor exactly
    /// [`MOMENTUM_GLOW_KEY_CREDIT`], nor in
    /// `[fl(SNAP_ZERO + MOMENTUM_GLOW_KEY_CREDIT), 1]`.
    RateValue { bits: u32 },
    /// Validate: `rate_at` is present for a resting rate, or absent for a
    /// non-zero one.
    RateStamp { value_bits: u32, stamped: bool },
    /// Validate: `latched` is neither `+0.0` nor in `[MOMENTUM_GLOW_HOT, 1]`.
    Latched { bits: u32 },
    /// Validate: `latched`, `stamp` and `fp_last` disagree on whether the last
    /// tick was hot.
    Latch {
        latched_bits: u32,
        stamped: bool,
        fp_last: u64,
    },
    /// Validate: a fingerprint is non-zero and even.
    Fingerprint { field: SnapshotField, value: u64 },
    /// Validate: a latched glow's rate is at rest.
    HotWithoutRate { latched_bits: u32 },
    /// Validate: `tau` is NaN, infinite or negative, or, while latched, not in
    /// `(0, MOMENTUM_PROFILE.max_tau_s]`.
    Tau { bits: u32, hot: bool },
    /// Validate: `fp_prev` or `tau` records a hot tick the rest of the state
    /// rules out. At rest both are 0, and a non-zero `fp_prev` has a non-zero
    /// `tau` beside it.
    History {
        rate_bits: u32,
        fp_prev: u64,
        tau: u32,
    },
    /// Validate: a positive offset lies past `MOMENTUM_PROFILE.horizon`, where
    /// no admitted input falls.
    Horizon { field: SnapshotField, offset: i128 },
    /// Restore: the offset lands outside the restoring host's `Instant` range,
    /// or (for `stamp`) the settle schedule's latest instant
    /// `epoch + stamp + τ·ln 255 + SETTLE_MARGIN` does not.
    Instant { field: SnapshotField, offset: i128 },
}

impl fmt::Display for SnapshotRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::OffsetUnrepresentable { field } => write!(
                f,
                "momentum snapshot refused: {field} is further from the epoch than i128 nanoseconds reach"
            ),
            Self::Length { expected, found } => write!(
                f,
                "momentum snapshot refused: {found} bytes where the layout is {expected}"
            ),
            Self::Schema { found } => write!(
                f,
                "momentum snapshot refused: schema {found} is not {MOMENTUM_GLOW_SNAPSHOT_SCHEMA}"
            ),
            Self::Tag { field, tag } => write!(
                f,
                "momentum snapshot refused: {field} has option tag {tag}, not 0 or 1"
            ),
            Self::NonCanonical { field } => write!(
                f,
                "momentum snapshot refused: {field} is None but its payload is not zero"
            ),
            Self::NotANumber { field, bits } => write!(
                f,
                "momentum snapshot refused: {field} is NaN (bits {bits:#010x})"
            ),
            Self::NegativeZero { field } => write!(
                f,
                "momentum snapshot refused: {field} is -0.0, which the engine never stores"
            ),
            Self::RateValue { bits } => write!(
                f,
                "momentum snapshot refused: rate_value {} is neither 0, nor 0.1, nor in [{LIVE_CREDIT_FLOOR}, 1]",
                f32::from_bits(bits)
            ),
            Self::RateStamp {
                value_bits,
                stamped,
            } => write!(
                f,
                "momentum snapshot refused: rate_value {} with rate_at {}",
                f32::from_bits(value_bits),
                if stamped { "present" } else { "absent" }
            ),
            Self::Latched { bits } => write!(
                f,
                "momentum snapshot refused: latched {} is neither 0 nor in [0.02, 1]",
                f32::from_bits(bits)
            ),
            Self::Latch {
                latched_bits,
                stamped,
                fp_last,
            } => write!(
                f,
                "momentum snapshot refused: latched {}, stamp {} and fp_last {fp_last:#x} disagree on whether the last tick was hot",
                f32::from_bits(latched_bits),
                if stamped { "present" } else { "absent" }
            ),
            Self::Fingerprint { field, value } => write!(
                f,
                "momentum snapshot refused: {field} {value:#x} is neither 0 nor odd"
            ),
            Self::HotWithoutRate { latched_bits } => write!(
                f,
                "momentum snapshot refused: latched {} with a resting typing rate",
                f32::from_bits(latched_bits)
            ),
            Self::Tau { bits, hot } => write!(
                f,
                "momentum snapshot refused: tau {} is out of range for a {} glow",
                f32::from_bits(bits),
                if hot { "latched" } else { "resting" }
            ),
            Self::History {
                rate_bits,
                fp_prev,
                tau,
            } => write!(
                f,
                "momentum snapshot refused: fp_prev {fp_prev:#x} and tau {} beside rate_value {} record a hot tick no transition leaves there",
                f32::from_bits(tau),
                f32::from_bits(rate_bits)
            ),
            Self::Horizon { field, offset } => write!(
                f,
                "momentum snapshot refused: {field} at {offset} ns from the epoch is past the hosted horizon"
            ),
            Self::Instant { field, offset } => write!(
                f,
                "momentum snapshot refused: {field} at {offset} ns from the epoch is outside this host's instants, or its settle schedule is"
            ),
        }
    }
}

impl std::error::Error for SnapshotRefusal {}

/// Bytes per field, in layout order (the table in the module docs).
const SCHEMA_BYTES: usize = 2;
const F32_BYTES: usize = 4;
const TAG_BYTES: usize = 1;
const OFFSET_BYTES: usize = 16;
const U64_BYTES: usize = 8;

const NANOS_PER_SEC: u128 = 1_000_000_000;

impl MomentumGlowSnapshot {
    /// The encoded length: schema, two floats and τ, two tagged offsets, two
    /// fingerprints.
    pub const LEN: usize = SCHEMA_BYTES
        + F32_BYTES
        + TAG_BYTES
        + OFFSET_BYTES
        + U64_BYTES
        + U64_BYTES
        + F32_BYTES
        + TAG_BYTES
        + OFFSET_BYTES
        + F32_BYTES;

    /// The canonical encoding (the module docs' table).
    #[inline]
    #[must_use]
    pub const fn encode(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        let mut w = Writer(&mut out);
        w.put(MOMENTUM_GLOW_SNAPSHOT_SCHEMA.to_le_bytes());
        w.put(self.rate_value.to_le_bytes());
        w.offset(self.rate_at);
        w.put(self.fp_last.to_le_bytes());
        w.put(self.fp_prev.to_le_bytes());
        w.put(self.latched.to_le_bytes());
        w.offset(self.stamp);
        w.put(self.tau.to_le_bytes());
        out
    }

    /// Decodes [`Self::encode`]'s bytes and refuses anything else, naming the
    /// first fault in layout order. Checks bytes only: whether the state is
    /// reachable is [`Self::validate`]'s question.
    ///
    /// # Errors
    ///
    /// [`SnapshotRefusal::Length`], [`SnapshotRefusal::Schema`],
    /// [`SnapshotRefusal::Tag`], [`SnapshotRefusal::NonCanonical`],
    /// [`SnapshotRefusal::NotANumber`] or [`SnapshotRefusal::NegativeZero`].
    #[inline]
    pub const fn decode(bytes: &[u8]) -> Result<Self, SnapshotRefusal> {
        if bytes.len() != Self::LEN {
            return Err(SnapshotRefusal::Length {
                expected: Self::LEN,
                found: bytes.len(),
            });
        }
        let mut r = Reader(bytes);
        let schema_bytes = match r.take() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let schema = u16::from_le_bytes(schema_bytes);
        if schema != MOMENTUM_GLOW_SNAPSHOT_SCHEMA {
            return Err(SnapshotRefusal::Schema { found: schema });
        }
        let rate_bytes = match r.take() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let rate_value = match canonical_f32(rate_bytes, SnapshotField::RateValue) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let rate_at = match r.offset(SnapshotField::RateAt) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let fp_last_bytes = match r.take() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let fp_last = u64::from_le_bytes(fp_last_bytes);
        let fp_prev_bytes = match r.take() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let fp_prev = u64::from_le_bytes(fp_prev_bytes);
        let latched_bytes = match r.take() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let latched = match canonical_f32(latched_bytes, SnapshotField::Latched) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let stamp = match r.offset(SnapshotField::Stamp) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        let tau_bytes = match r.take() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let tau = match canonical_f32(tau_bytes, SnapshotField::Tau) {
            Ok(v) => v,
            Err(e) => return Err(e),
        };
        Ok(Self {
            rate_value,
            rate_at,
            fp_last,
            fp_prev,
            latched,
            stamp,
            tau,
        })
    }

    /// Refuses every state that breaks an invariant the engine's transitions
    /// keep, or a bound of the hosted profile (the module docs' lists), checked
    /// in this order, naming the first one broken. Needs no epoch.
    ///
    /// # Errors
    ///
    /// [`SnapshotRefusal::RateValue`], [`SnapshotRefusal::RateStamp`],
    /// [`SnapshotRefusal::Latched`], [`SnapshotRefusal::Latch`],
    /// [`SnapshotRefusal::Fingerprint`], [`SnapshotRefusal::HotWithoutRate`],
    /// [`SnapshotRefusal::Tau`], [`SnapshotRefusal::History`] or
    /// [`SnapshotRefusal::Horizon`].
    pub const fn validate(&self) -> Result<(), SnapshotRefusal> {
        let one = 1.0f32.to_bits();
        let credited = MOMENTUM_GLOW_KEY_CREDIT.to_bits();
        let live_floor = LIVE_CREDIT_FLOOR.to_bits();
        let hot_floor = MOMENTUM_GLOW_HOT.to_bits();
        let rate_ok = self.rate_value == 0
            || self.rate_value == credited
            || (self.rate_value >= live_floor && self.rate_value <= one);
        if !rate_ok {
            return Err(SnapshotRefusal::RateValue {
                bits: self.rate_value,
            });
        }
        if (self.rate_value != 0) != self.rate_at.is_some() {
            return Err(SnapshotRefusal::RateStamp {
                value_bits: self.rate_value,
                stamped: self.rate_at.is_some(),
            });
        }
        if self.latched != 0 && !(self.latched >= hot_floor && self.latched <= one) {
            return Err(SnapshotRefusal::Latched { bits: self.latched });
        }
        let hot = self.latched != 0;
        if hot != self.stamp.is_some() || hot != (self.fp_last != 0) {
            return Err(SnapshotRefusal::Latch {
                latched_bits: self.latched,
                stamped: self.stamp.is_some(),
                fp_last: self.fp_last,
            });
        }
        if self.fp_last != 0 && self.fp_last & 1 == 0 {
            return Err(SnapshotRefusal::Fingerprint {
                field: SnapshotField::FpLast,
                value: self.fp_last,
            });
        }
        if self.fp_prev != 0 && self.fp_prev & 1 == 0 {
            return Err(SnapshotRefusal::Fingerprint {
                field: SnapshotField::FpPrev,
                value: self.fp_prev,
            });
        }
        if hot && self.rate_value == 0 {
            return Err(SnapshotRefusal::HotWithoutRate {
                latched_bits: self.latched,
            });
        }
        // Finite and non-negative: `+0.0` up to `f32::MAX` in bits. Latched: a
        // positive τ (subnormals included, as the rate's `τ > 0` gate admits
        // them) up to the profile's ceiling.
        let tau_ok = if hot {
            self.tau >= 1 && self.tau <= MOMENTUM_PROFILE.max_tau_s.to_bits()
        } else {
            self.tau <= f32::MAX.to_bits()
        };
        if !tau_ok {
            return Err(SnapshotRefusal::Tau {
                bits: self.tau,
                hot,
            });
        }
        let at_rest = self.rate_value == 0;
        if (at_rest && (self.fp_prev != 0 || self.tau != 0)) || (self.fp_prev != 0 && self.tau == 0)
        {
            return Err(SnapshotRefusal::History {
                rate_bits: self.rate_value,
                fp_prev: self.fp_prev,
                tau: self.tau,
            });
        }
        // Every `Duration` is below 2^94 ns, so the horizon's nanoseconds fit.
        let p_ns = MOMENTUM_PROFILE.horizon.as_nanos();
        let env_ns = MOMENTUM_ENVELOPE.horizon.as_nanos();
        let min_ns = if p_ns < env_ns { p_ns } else { env_ns };
        let horizon = if min_ns > i128::MAX as u128 {
            i128::MAX
        } else {
            min_ns as i128
        };
        if let Some(offset) = self.rate_at
            && offset > horizon
        {
            return Err(SnapshotRefusal::Horizon {
                field: SnapshotField::RateAt,
                offset,
            });
        }
        if let Some(offset) = self.stamp
            && offset > horizon
        {
            return Err(SnapshotRefusal::Horizon {
                field: SnapshotField::Stamp,
                offset,
            });
        }
        Ok(())
    }
}

impl MomentumGlow {
    /// The engine's semantic state, with every instant as a signed offset from
    /// `epoch`. The light-ink memo, a cache, is left out.
    ///
    /// # Errors
    ///
    /// [`SnapshotRefusal::OffsetUnrepresentable`], which no std `Instant` pair
    /// reaches (an `i128` holds every `Duration`'s nanoseconds).
    pub fn snapshot(&self, epoch: Instant) -> Result<MomentumGlowSnapshot, SnapshotRefusal> {
        Ok(MomentumGlowSnapshot {
            rate_value: self.rate.value.to_bits(),
            rate_at: self
                .rate
                .at
                .map(|at| offset_from(epoch, at, SnapshotField::RateAt))
                .transpose()?,
            fp_last: self.fp_last,
            fp_prev: self.fp_prev,
            latched: self.latched.to_bits(),
            stamp: self
                .stamp
                .map(|stamp| offset_from(epoch, stamp, SnapshotField::Stamp))
                .transpose()?,
            tau: self.tau.to_bits(),
        })
    }

    /// An engine in `s`'s state, its instants placed on this host's `epoch`.
    /// The light-ink memo starts cold (`None`). Validates `s` first
    /// ([`MomentumGlowSnapshot::validate`]).
    ///
    /// # Errors
    ///
    /// Any of [`MomentumGlowSnapshot::validate`]'s refusals, or
    /// [`SnapshotRefusal::Instant`] when an offset lands outside the
    /// `Instant` range, or a stamp's settle schedule would (the module docs).
    pub fn restore(s: &MomentumGlowSnapshot, epoch: Instant) -> Result<Self, SnapshotRefusal> {
        s.validate()?;
        let at = s
            .rate_at
            .map(|offset| instant_at(epoch, offset, SnapshotField::RateAt))
            .transpose()?;
        let stamp = s
            .stamp
            .map(|offset| {
                let refused = SnapshotRefusal::Instant {
                    field: SnapshotField::Stamp,
                    offset,
                };
                let stamp = instant_at(epoch, offset, SnapshotField::Stamp)?;
                // The latest instant the settle schedule forms from the stamp
                // (validate bounded τ while latched, so the product is finite).
                let reach = Duration::try_from_secs_f32(f32::from_bits(s.tau) * 255f32.ln())
                    .ok()
                    .and_then(|d| d.checked_add(SETTLE_MARGIN))
                    .ok_or(refused)?;
                stamp.checked_add(reach).ok_or(refused)?;
                Ok(stamp)
            })
            .transpose()?;
        Ok(Self {
            rate: TypingRate {
                value: f32::from_bits(s.rate_value),
                at,
            },
            fp_last: s.fp_last,
            fp_prev: s.fp_prev,
            latched: f32::from_bits(s.latched),
            stamp,
            tau: f32::from_bits(s.tau),
            light_ink_memo: None,
        })
    }
}

/// `t − epoch` in signed nanoseconds.
fn offset_from(epoch: Instant, t: Instant, field: SnapshotField) -> Result<i128, SnapshotRefusal> {
    let refused = SnapshotRefusal::OffsetUnrepresentable { field };
    let nanos = |d: Duration| i128::try_from(d.as_nanos()).map_err(|_| refused);
    if let Some(after) = t.checked_duration_since(epoch) {
        nanos(after)
    } else {
        let before = epoch.checked_duration_since(t).ok_or(refused)?;
        nanos(before)?.checked_neg().ok_or(refused)
    }
}

/// `epoch + offset` with checked instant arithmetic.
fn instant_at(
    epoch: Instant,
    offset: i128,
    field: SnapshotField,
) -> Result<Instant, SnapshotRefusal> {
    let refused = SnapshotRefusal::Instant { field, offset };
    let magnitude = offset.unsigned_abs();
    let secs = u64::try_from(magnitude / NANOS_PER_SEC).map_err(|_| refused)?;
    let nanos = u32::try_from(magnitude % NANOS_PER_SEC).map_err(|_| refused)?;
    // `nanos < 1e9`, so `Duration::new` carries nothing into `secs`.
    let span = Duration::new(secs, nanos);
    if offset >= 0 {
        epoch.checked_add(span)
    } else {
        epoch.checked_sub(span)
    }
    .ok_or(refused)
}

/// A float's bits, refused when NaN or `-0.0`.
#[inline]
const fn canonical_f32(
    bytes: [u8; F32_BYTES],
    field: SnapshotField,
) -> Result<u32, SnapshotRefusal> {
    let bits = u32::from_le_bytes(bytes);
    if f32::from_bits(bits).is_nan() {
        return Err(SnapshotRefusal::NotANumber { field, bits });
    }
    if bits == (-0.0f32).to_bits() {
        return Err(SnapshotRefusal::NegativeZero { field });
    }
    Ok(bits)
}

/// Writes the fixed layout front to back. Each `put` takes a fixed-size
/// chunk; the layout's total is [`MomentumGlowSnapshot::LEN`], so no chunk
/// ever runs short, and if one did it would be skipped rather than panic.
struct Writer<'a>(&'a mut [u8]);

impl Writer<'_> {
    #[inline]
    const fn put<const N: usize>(&mut self, bytes: [u8; N]) {
        #[expect(clippy::mem_replace_with_default, reason = "mem::take is not const fn")]
        let rest = std::mem::replace(&mut self.0, &mut []);
        if let Some((head, tail)) = rest.split_first_chunk_mut::<N>() {
            *head = bytes;
            self.0 = tail;
        }
    }

    #[inline]
    const fn offset(&mut self, offset: Option<i128>) {
        self.put([offset.is_some() as u8]);
        let v = match offset {
            Some(v) => v,
            None => 0,
        };
        self.put(v.to_le_bytes());
    }
}

/// Reads the fixed layout front to back; a short read (unreachable after the
/// length check) is a [`SnapshotRefusal::Length`], never a panic.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    #[inline]
    const fn take<const N: usize>(&mut self) -> Result<[u8; N], SnapshotRefusal> {
        let Some((head, tail)) = self.0.split_first_chunk::<N>() else {
            return Err(SnapshotRefusal::Length {
                expected: MomentumGlowSnapshot::LEN,
                found: self.0.len(),
            });
        };
        self.0 = tail;
        Ok(*head)
    }

    #[inline]
    const fn offset(&mut self, field: SnapshotField) -> Result<Option<i128>, SnapshotRefusal> {
        let [tag] = match self.take::<TAG_BYTES>() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let payload = match self.take::<OFFSET_BYTES>() {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        let v = i128::from_le_bytes(payload);
        match tag {
            0 if v == 0 => Ok(None),
            0 => Err(SnapshotRefusal::NonCanonical { field }),
            1 => Ok(Some(v)),
            tag => Err(SnapshotRefusal::Tag { field, tag }),
        }
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
