// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typing-momentum glow as an Aos task: [`MomentumEngine`], hosted by the generic
//! [`crate::aos_effect::Hosted`] as [`MomentumHost`].

use core::fmt;
use std::time::Duration;

use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::EffectPort;
use aterm_aos::publish::Stats;
use aterm_aos::replay::{FoldedRun, Run, replay_fold};
use aterm_aos::task::{Code, EntryRefusal, PausedClass, StepRefusal, state_digest};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, ArtifactKind, TaskIdentity};
use aterm_render::GlowQuad;
use aterm_time::Instant;

use crate::aos_effect::{
    Attached, EffectCode, EffectSchema, HostMode, Hosted, HostedEngine, identity_of, instant_of,
    offset_of,
};
use crate::cursor_glow::Geom;
use crate::cursor_momentum::{
    AdmitRefusal, GeometryFault, MOMENTUM_GLOW_SNAPSHOT_SCHEMA, MOMENTUM_PROFILE, MomentumGlow,
    MomentumGlowConfig, MomentumGlowFrame, MomentumGlowSnapshot, MomentumProfile, SnapshotRefusal,
    admit_frame_interval, admit_tau, admit_tick, admit_time,
};

pub use crate::aos_effect::{PAST_HORIZON, RAW_KEY, TraceRefusal};

#[cfg(test)]
mod admission_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod bundle;
#[cfg(test)]
mod bundle_tests;
#[cfg(test)]
mod clock_tests;
#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod contract_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod corpus;
#[cfg(test)]
mod corpus_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod fidelity;
#[cfg(test)]
mod fidelity_tests;
#[cfg(test)]
mod hold_conformance_tests;
#[cfg(test)]
mod hold_tests;
#[cfg(test)]
mod host_tests;
pub mod lane;
#[cfg(test)]
mod lane_tests;
#[cfg(test)]
mod locks_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod m4;
#[cfg(test)]
mod m4_tests;
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
pub mod m6;
#[cfg(any(test, debug_assertions, feature = "aos-learn"))]
pub mod m6_policy;
#[cfg(test)]
mod m6_policy_tests;
#[cfg(test)]
mod m6_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod plan;
#[cfg(test)]
mod plan_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod policy;
#[cfg(test)]
mod policy_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod publish;
#[cfg(test)]
mod publish_tests;
#[cfg(test)]
mod repair_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod search;
#[cfg(test)]
mod search_tests;
#[cfg(test)]
mod test_fixtures;
mod trace;
#[cfg(test)]
mod trace_tests;
#[cfg(test)]
mod vivid_tests;
mod watch;
#[cfg(test)]
mod watch_tests;

#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
use plan::{PlanView, SchedulePlan};
pub use trace::{
    EventCounts, MOMENTUM_TRACE_MAX_BYTES, MomentumTrace, ReadTrace, TRACE_FILE_DOMAIN,
    TRACE_FILE_MAGIC, TraceCheck, TraceFault, TraceReplay,
};
pub use watch::{Segment, SegmentJudgement};

/// The version of the input and observation encodings ([`MomentumSchema`]).
pub const MOMENTUM_EVENT_ENCODING: u16 = 3;

/// The task artifact's name.
pub const TASK_NAME: &str = "aterm.cursor.momentum-glow";
/// The schema artifact's name.
pub const SCHEMA_NAME: &str = "momentum-glow.state+events";
/// The contract artifact's name.
pub const CONTRACT_NAME: &str = "momentum-glow.no-stranded-light";
/// The shipped code artifact's name ([`MomentumCode::Shipped`]).
pub const SHIPPED_CODE_NAME: &str = "momentum-glow.shipped";
/// The safe code artifact's name ([`MomentumCode::Safe`]).
pub const SAFE_CODE_NAME: &str = "momentum-glow.safe";
/// The demonstration fault's code artifact name, defined in every build so the acceptance policy binds the same bases even where the fault's code is compiled out.
pub const DEMO_CODE_NAME: &str = "momentum-glow.demo-stranded-settle";
/// The numerical half of the profile artifact's name, after the target.
pub const NUMERIC_PROFILE: &str = "f32-ieee754/system-libm/single-thread";

/// How many keys and resets may wait during a pause.
pub const MOMENTUM_QUEUE_CAP: usize = 1024;

/// The most events a reader of a momentum log accepts (`EventLog::decode`'s limit).
pub const MOMENTUM_MAX_EVENTS: usize = 1 << 16;

/// How many inputs one log epoch of [`MomentumHost::aos_attach`] holds before the recorder
/// re-anchors.
pub const MOMENTUM_HOST_MAX_EVENTS: usize = 1 << 12;

/// The contract's settle margin.
pub const SETTLE_MARGIN: Duration = Duration::from_millis(1);

/// The law: at every schedule query, a hot glow (`is_active || settling`) names a wake.
pub const HOT_HAS_WAKE: &str = "HotHasWake";
/// The law.
pub const WAKE_NO_LATER_THAN_CLEAR: &str = "WakeNoLaterThanClear";
/// The law.
pub const COLD_TICK_CLEARS: &str = "ColdTickClears";
/// The law: the run observed each recorded input once, with the observation its kind makes.
pub const RUN_MATCHES_INPUTS: &str = "RunMatchesInputs";
/// The entry law: the state is one the engine can reach (its snapshot validates).
pub const SNAPSHOT_VALID: &str = "momentum-glow.snapshot-valid";

/// The task's owned state: the engine, the latest admitted input's time and how many of its
/// keys aterm could not see echoed.
#[derive(Clone, Copy, Debug, Default)]
pub struct MomentumState {
    glow: MomentumGlow,
    clock: Option<i128>,
    /// Keys this state took that were logged [`KeyEcho::Unseen`], since its recording began: every
    /// checkpoint and state digest carries the count, so a trace cut anywhere says it.
    unseen: u32,
}

impl MomentumState {
    /// A state over `glow` that has seen no input since the epoch, and so no unseen key: each new
    /// recording starts its count from zero.
    #[inline]
    #[must_use]
    pub const fn new(glow: MomentumGlow) -> Self {
        MomentumState {
            glow,
            clock: None,
            unseen: 0,
        }
    }

    /// How many keys this state took that were logged [`KeyEcho::Unseen`].
    #[inline]
    #[must_use]
    pub const fn unseen(&self) -> u32 {
        self.unseen
    }

    /// The engine.
    #[inline]
    #[must_use]
    pub const fn glow(&self) -> &MomentumGlow {
        &self.glow
    }

    /// The latest admitted input's time (signed ns from the epoch), if any.
    #[inline]
    #[must_use]
    pub const fn clock(&self) -> Option<i128> {
        self.clock
    }

    #[inline]
    fn last_time(&self, epoch: Instant) -> Option<i128> {
        match self.glow.snapshot(epoch) {
            Ok(s) => self.clock.max(s.rate_at).max(s.stamp),
            Err(_) => self.clock,
        }
    }
}

/// [`Geom`], every field, as plain encodable data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GeomRecord {
    pub cw: usize,
    pub ch: usize,
    pub rows: usize,
    pub cols: usize,
    pub origin_x: u16,
    pub origin_y: u16,
    pub win_w: u16,
    pub win_h: u16,
    pub head: u16,
}

impl From<&Geom> for GeomRecord {
    #[inline]
    fn from(g: &Geom) -> Self {
        Self::from_geom(g)
    }
}

impl GeomRecord {
    /// Record `g` as plain encodable data.
    #[inline]
    #[must_use]
    pub const fn from_geom(g: &Geom) -> Self {
        GeomRecord {
            cw: g.cw,
            ch: g.ch,
            rows: g.rows,
            cols: g.cols,
            origin_x: g.origin_x,
            origin_y: g.origin_y,
            win_w: g.win_w,
            win_h: g.win_h,
            head: g.head,
        }
    }

    /// The geometry it records.
    #[inline]
    #[must_use]
    pub const fn geom(&self) -> Geom {
        Geom {
            cw: self.cw,
            ch: self.ch,
            rows: self.rows,
            cols: self.cols,
            origin_x: self.origin_x,
            origin_y: self.origin_y,
            win_w: self.win_w,
            win_h: self.win_h,
            head: self.head,
        }
    }
}

/// [`MomentumGlowConfig`], every field, floats as their IEEE bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConfigRecord {
    pub enabled: bool,
    /// `intensity`, as `f32` bits.
    pub intensity: u32,
    /// `tau_s`, as `f32` bits.
    pub tau_s: u32,
    /// `radius_cells`, as `f32` bits.
    pub radius_cells: u32,
    pub base: u32,
    pub dark_theme: bool,
    pub hue: u32,
    pub theme_fg: u32,
    pub theme_bg: u32,
    pub block: bool,
    /// Written as the ink byte's [`INK_BAR`] bit.
    pub bar: bool,
    pub glyph_span: Option<(i32, i32)>,
    pub light_vivid: bool,
    pub chosen_hue: bool,
    pub chosen_base: bool,
}

/// [`ConfigRecord::ink_byte`]'s bit for `light_vivid`.
const INK_VIVID: u8 = 1;
/// [`ConfigRecord::ink_byte`]'s bit for `chosen_hue`.
const INK_CHOSEN_HUE: u8 = 2;
/// [`ConfigRecord::ink_byte`]'s bit for `chosen_base`.
const INK_CHOSEN_BASE: u8 = 4;
/// [`ConfigRecord::ink_byte`]'s bit for `bar` (where the halo sits, not its ink, but the one
/// byte a recording made before writes `0` in, so it reads the same).
const INK_BAR: u8 = 8;

impl ConfigRecord {
    /// The one byte a Tick input writes for the glow's ink and seat: `light_vivid`,
    /// `chosen_hue`, `chosen_base` and `bar` as bits 0, 1, 2 and 3. A glow that reads no chosen
    /// colour round a block writes `0` or `1`, the byte `light_vivid` was written as alone, so
    /// every recording made before reads the same and its digests and pins stand.
    #[inline]
    #[must_use]
    pub const fn ink_byte(&self) -> u8 {
        (if self.light_vivid { INK_VIVID } else { 0 })
            | (if self.chosen_hue { INK_CHOSEN_HUE } else { 0 })
            | (if self.chosen_base { INK_CHOSEN_BASE } else { 0 })
            | (if self.bar { INK_BAR } else { 0 })
    }

    /// The ink byte's four bits, `(light_vivid, chosen_hue, chosen_base, bar)`; a byte with
    /// any other bit set is `BadTag` for `field`.
    #[inline]
    fn read_ink_byte(
        d: &mut Dec<'_>,
        field: &'static str,
    ) -> Result<(bool, bool, bool, bool), WireFault> {
        let tag = d.u8(field)?;
        if tag & !(INK_VIVID | INK_CHOSEN_HUE | INK_CHOSEN_BASE | INK_BAR) != 0 {
            return Err(WireFault::BadTag { field, tag });
        }
        Ok((
            tag & INK_VIVID != 0,
            tag & INK_CHOSEN_HUE != 0,
            tag & INK_CHOSEN_BASE != 0,
            tag & INK_BAR != 0,
        ))
    }
}

impl From<&MomentumGlowConfig> for ConfigRecord {
    #[inline]
    fn from(c: &MomentumGlowConfig) -> Self {
        Self::from_config(c)
    }
}

impl ConfigRecord {
    /// Record `c` bit-for-bit.
    #[inline]
    #[must_use]
    pub const fn from_config(c: &MomentumGlowConfig) -> Self {
        ConfigRecord {
            enabled: c.enabled,
            intensity: c.intensity.to_bits(),
            tau_s: c.tau_s.to_bits(),
            radius_cells: c.radius_cells.to_bits(),
            base: c.base,
            dark_theme: c.dark_theme,
            hue: c.hue,
            theme_fg: c.theme_fg,
            theme_bg: c.theme_bg,
            block: c.block,
            bar: c.bar,
            glyph_span: c.glyph_span,
            light_vivid: c.light_vivid,
            chosen_hue: c.chosen_hue,
            chosen_base: c.chosen_base,
        }
    }

    /// The config it records, bit for bit.
    #[inline]
    #[must_use]
    pub const fn config(&self) -> MomentumGlowConfig {
        MomentumGlowConfig {
            light: crate::cursor_glow::Light::OFF,
            enabled: self.enabled,
            intensity: f32::from_bits(self.intensity),
            tau_s: f32::from_bits(self.tau_s),
            radius_cells: f32::from_bits(self.radius_cells),
            base: self.base,
            dark_theme: self.dark_theme,
            hue: self.hue,
            theme_fg: self.theme_fg,
            theme_bg: self.theme_bg,
            block: self.block,
            bar: self.bar,
            glyph_span: self.glyph_span,
            light_vivid: self.light_vivid,
            chosen_hue: self.chosen_hue,
            chosen_base: self.chosen_base,
        }
    }
}

/// Whether aterm saw a typed key echoed before its recording logged it (ATERM-KI-1025-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyEcho {
    /// Logged at its press where the tty or an open prompt of aterm's shell echoes it (or it
    /// carries no text), or at the frame that proved its own echo.
    Seen,
    /// Logged under the person's grant for its attach ([`MomentumHost::aos_release_unseen`]), at a
    /// frame that showed neither its echo nor a mask where it was typed: it may be a password key.
    Unseen,
}

/// One input to the task: one host call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MomentumEvent {
    /// `on_key(now, tau_s)`.
    Key {
        at: i128,
        /// `tau_s`, as `f32` bits.
        tau_bits: u32,
        /// Whether aterm saw it echoed: every code applies an unseen key exactly as a seen one,
        /// and the state counts it ([`MomentumState::unseen`]).
        echo: KeyEcho,
    },
    /// `reset()`.
    Reset,
    /// `tick(cur, now, geom, cfg, ..)`.
    Tick {
        cur: Option<(u16, u16)>,
        at: i128,
        geom: GeomRecord,
        cfg: ConfigRecord,
    },
    /// `next_change_deadline(now, frame_interval)`: the host's one scheduler question.
    Query { at: i128, frame_interval: Duration },
}

/// What one step makes visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MomentumObservation {
    /// A key was credited.
    Keyed,
    /// The glow went back to rest.
    WasReset,
    /// One tick: its frame and the quads it appended to each stream.
    Frame {
        fill: Option<u32>,
        hot: bool,
        fp: u64,
        out: Vec<GlowQuad>,
        under: Vec<GlowQuad>,
    },
    /// One scheduler query: the predicates, the cold forecast and the wake the code names.
    Schedule {
        is_active: bool,
        settling: bool,
        cold_at: Option<i128>,
        deadline: Option<i128>,
    },
    /// Admission refused the input; the state is untouched and nothing was appended.
    Refused(AdmitRefusal),
}

/// What [`MomentumCode::apply`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// A key was credited.
    Keyed,
    /// The glow went back to rest.
    WasReset,
    /// A tick's frame; its quads were appended to the vectors `apply` was handed.
    Frame(MomentumGlowFrame),
    /// A scheduler query's answer.
    Schedule {
        is_active: bool,
        settling: bool,
        cold_at: Option<Instant>,
        deadline: Option<Instant>,
    },
    /// Admission refused the input.
    Refused(AdmitRefusal),
}

#[inline]
fn observe_owned(
    applied: &Applied,
    epoch: Instant,
    out: Vec<GlowQuad>,
    under: Vec<GlowQuad>,
) -> MomentumObservation {
    match *applied {
        Applied::Keyed => MomentumObservation::Keyed,
        Applied::WasReset => MomentumObservation::WasReset,
        Applied::Frame(f) => MomentumObservation::Frame {
            fill: f.fill,
            hot: f.hot,
            fp: f.fp,
            out,
            under,
        },
        Applied::Schedule {
            is_active,
            settling,
            cold_at,
            deadline,
        } => MomentumObservation::Schedule {
            is_active,
            settling,
            cold_at: cold_at.map(|t| offset_of(epoch, t)),
            deadline: deadline.map(|t| offset_of(epoch, t)),
        },
        Applied::Refused(r) => MomentumObservation::Refused(r),
    }
}

#[inline]
fn observe(
    applied: &Applied,
    epoch: Instant,
    out: &[GlowQuad],
    under: &[GlowQuad],
) -> MomentumObservation {
    match *applied {
        Applied::Frame(_) => observe_owned(applied, epoch, out.to_vec(), under.to_vec()),
        _ => observe_owned(applied, epoch, Vec::new(), Vec::new()),
    }
}

/// The task ABI: the generic adapter over [`MomentumEngine`].
pub type MomentumSchema = EffectSchema<MomentumEngine>;

/// A prebuilt implementation bound to its artifact (which names the build it was compiled into).
pub type PrebuiltMomentum = EffectCode<MomentumEngine>;

/// The momentum glow as the host holds it.
pub type MomentumHost = Hosted<MomentumEngine>;

/// A paused glow's boundary, copied, with the candidate to prepare against it away from the host's main thread.
pub type RepairJob = crate::aos_effect::RepairJob<MomentumEngine>;

/// What [`RepairJob::prepare`] made, for [`MomentumHost::aos_commit`].
pub type PreparedRepair = crate::aos_effect::PreparedRepair<MomentumEngine>;

/// The typing-momentum glow's hosted engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MomentumEngine;

#[inline]
const fn snapshot_fault(r: SnapshotRefusal) -> WireFault {
    match r {
        SnapshotRefusal::Length { expected, found } => WireFault::Truncated {
            field: "state.snapshot",
            need: expected,
            have: found,
        },
        SnapshotRefusal::Schema { .. } => WireFault::NonCanonical {
            field: "state.snapshot.schema",
        },
        SnapshotRefusal::Tag { field, tag } => WireFault::BadTag {
            field: field.name(),
            tag,
        },
        SnapshotRefusal::OffsetUnrepresentable { field }
        | SnapshotRefusal::NonCanonical { field }
        | SnapshotRefusal::NotANumber { field, .. }
        | SnapshotRefusal::NegativeZero { field }
        | SnapshotRefusal::Fingerprint { field, .. }
        | SnapshotRefusal::Horizon { field, .. }
        | SnapshotRefusal::Instant { field, .. } => WireFault::NonCanonical {
            field: field.name(),
        },
        SnapshotRefusal::History {
            rate_bits: 0,
            fp_prev: 1..,
            ..
        } => WireFault::NonCanonical { field: "fp_prev" },
        SnapshotRefusal::History { .. } => WireFault::NonCanonical { field: "tau" },
        SnapshotRefusal::RateValue { .. } | SnapshotRefusal::HotWithoutRate { .. } => {
            WireFault::NonCanonical {
                field: "rate_value",
            }
        }
        SnapshotRefusal::RateStamp { .. } => WireFault::NonCanonical { field: "rate_at" },
        SnapshotRefusal::Latched { .. } => WireFault::NonCanonical { field: "latched" },
        SnapshotRefusal::Latch { .. } => WireFault::NonCanonical { field: "stamp" },
        SnapshotRefusal::Tau { .. } => WireFault::NonCanonical { field: "tau" },
    }
}

#[inline]
fn usize_field(d: &mut Dec<'_>, field: &'static str) -> Result<usize, WireFault> {
    let v = d.u64(field)?;
    usize::try_from(v).map_err(|_| WireFault::TooLong {
        field,
        len: v,
        limit: u64::try_from(usize::MAX).unwrap_or(u64::MAX),
    })
}

#[inline]
const fn usize_u64(v: usize) -> u64 {
    v as u64
}

const QUAD_LEN: usize = 20;

const FRAME_HEAD_LEN: usize = 1 + 5 + 1 + 8 + 8 + 8;

#[inline]
const fn quad_bytes(q: &GlowQuad) -> [u8; QUAD_LEN] {
    let [r0, r1] = q.row.to_le_bytes();
    let [x0, x1] = q.x.to_le_bytes();
    let [y0, y1] = q.y.to_le_bytes();
    let [w0, w1] = q.w.to_le_bytes();
    let [h0, h1] = q.h.to_le_bytes();
    let [c0, c1, c2, c3] = q.color.to_le_bytes();
    let [d0, d1, d2, d3] = q.color2.to_le_bytes();
    [
        r0, r1, x0, x1, y0, y1, w0, w1, h0, h1, c0, c1, c2, c3, q.alpha, d0, d1, d2, d3, q.alpha2,
    ]
}

#[inline]
fn encode_quads(e: &mut Enc, quads: &[GlowQuad]) {
    e.u64(usize_u64(quads.len()));
    let mut qi = 0;
    while qi < quads.len() {
        e.raw(&quad_bytes(&quads[qi]));
        qi += 1;
    }
}

#[inline]
fn encode_frame(e: &mut Enc, frame: &MomentumGlowFrame, out: &[GlowQuad], under: &[GlowQuad]) {
    e.u8(2);
    e.opt(frame.fill, |e, f| e.u32(f));
    e.bool(frame.hot);
    e.u64(frame.fp);
    encode_quads(e, out);
    encode_quads(e, under);
}

#[inline]
const fn frame_room(quads: usize) -> usize {
    FRAME_HEAD_LEN.saturating_add(QUAD_LEN.saturating_mul(quads))
}

#[inline]
fn encode_refusal(e: &mut Enc, r: &AdmitRefusal) {
    match *r {
        AdmitRefusal::Tau(bits) => {
            e.u8(0);
            e.u32(bits);
        }
        AdmitRefusal::Radius(bits) => {
            e.u8(1);
            e.u32(bits);
        }
        AdmitRefusal::Intensity(bits) => {
            e.u8(2);
            e.u32(bits);
        }
        AdmitRefusal::Geometry(g) => {
            e.u8(3);
            match g {
                GeometryFault::CellWidthZero => e.u8(0),
                GeometryFault::CellHeightZero => e.u8(1),
                GeometryFault::GridWidth {
                    origin_x,
                    cols,
                    cw,
                    limit,
                } => {
                    e.u8(2);
                    e.u16(origin_x);
                    e.u64(usize_u64(cols));
                    e.u64(usize_u64(cw));
                    e.u32(limit);
                }
                GeometryFault::GridHeight {
                    origin_y,
                    rows,
                    ch,
                    limit,
                } => {
                    e.u8(3);
                    e.u16(origin_y);
                    e.u64(usize_u64(rows));
                    e.u64(usize_u64(ch));
                    e.u32(limit);
                }
                GeometryFault::HeadAboveOrigin { head, origin_y } => {
                    e.u8(4);
                    e.u16(head);
                    e.u16(origin_y);
                }
                GeometryFault::Unbounded => e.u8(5),
            }
        }
        AdmitRefusal::GlyphSpan(first, end) => {
            e.u8(4);
            e.i32(first);
            e.i32(end);
        }
        AdmitRefusal::FrameInterval(d) => {
            e.u8(5);
            e.duration(d);
        }
        AdmitRefusal::Horizon(at) => {
            e.u8(6);
            e.offset(at);
        }
        AdmitRefusal::TimeWentBack { last, at } => {
            e.u8(7);
            e.offset(last);
            e.offset(at);
        }
    }
}

/// What a momentum tick was drawn at: the cursor cell, geometry, page (`dark_theme`) and caret shape (`block`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MomentumPresentation {
    pub cur: Option<(u16, u16)>,
    pub geom: GeomRecord,
    pub dark: bool,
    pub block: bool,
}

/// What the host keeps per momentum attachment beside the boundary.
#[derive(Debug, Default)]
pub struct MomentumLive {
    /// The latest recorded query's time and the wake it was answered with.
    last_query: Option<(i128, Option<i128>)>,
    /// The plan-only presentation guard.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    guard: Guard,
}

impl HostedEngine for MomentumEngine {
    type Engine = MomentumGlow;
    type State = MomentumState;
    type Snapshot = MomentumGlowSnapshot;
    type Input = MomentumEvent;
    type Frame = MomentumObservation;
    type Profile = MomentumProfile;
    type Applied = Applied;
    type Code = Sched;
    type Prebuilt = MomentumCode;
    type Contract = NoStrandedLight;
    type Shown = MomentumGlowFrame;
    type Presentation = MomentumPresentation;
    type Counts = EventCounts;
    type Live = MomentumLive;

    const TASK_NAME: &'static str = TASK_NAME;
    const SHORT_NAME: &'static str = "momentum";
    const QUEUE_CAP: usize = MOMENTUM_QUEUE_CAP;
    const HOST_MAX_EVENTS: usize = MOMENTUM_HOST_MAX_EVENTS;
    const MAX_EVENTS: usize = MOMENTUM_MAX_EVENTS;
    const TRACE_MAGIC: [u8; 8] = TRACE_FILE_MAGIC;
    const TRACE_DOMAIN: &'static [u8] = TRACE_FILE_DOMAIN;
    const ALL: &'static [MomentumCode] = MomentumCode::ALL;

    #[inline]
    fn profile() -> &'static MomentumProfile {
        &MOMENTUM_PROFILE
    }

    #[inline]
    fn horizon_ns() -> i128 {
        horizon_ns()
    }

    #[inline]
    fn task_artifact() -> ArtifactId {
        task_artifact()
    }

    #[inline]
    fn schema_artifact() -> ArtifactId {
        schema_artifact()
    }

    #[inline]
    fn profile_artifact() -> ArtifactId {
        profile_artifact()
    }

    #[inline]
    fn contract() -> NoStrandedLight {
        NoStrandedLight::new()
    }

    #[inline]
    fn state_of(engine: MomentumGlow) -> MomentumState {
        MomentumState::new(engine)
    }

    #[inline]
    fn engine_of(state: MomentumState) -> MomentumGlow {
        state.glow
    }

    #[inline]
    fn snapshot(state: &MomentumState, epoch: Instant) -> Option<MomentumGlowSnapshot> {
        state.glow.snapshot(epoch).ok()
    }

    #[inline]
    fn encode_state(s: &MomentumState, epoch: Instant, e: &mut Enc) {
        match s.glow.snapshot(epoch) {
            Ok(snap) => e.raw(&snap.encode()),
            Err(_) => e.raw(&[0u8; MomentumGlowSnapshot::LEN]),
        }
        e.opt(s.clock, |e, c| e.offset(c));
        e.u32(s.unseen);
    }

    #[inline]
    fn decode_state(epoch: Instant, d: &mut Dec<'_>) -> Result<MomentumState, WireFault> {
        let bytes = d.raw("state.snapshot", MomentumGlowSnapshot::LEN)?;
        let snap = MomentumGlowSnapshot::decode(bytes).map_err(snapshot_fault)?;
        let glow = MomentumGlow::restore(&snap, epoch).map_err(snapshot_fault)?;
        let clock = d.opt("state.clock", |d| d.offset("state.clock"))?;
        let unseen = d.u32("state.unseen")?;
        Ok(MomentumState {
            glow,
            clock,
            unseen,
        })
    }

    #[inline]
    fn encode_input(i: &MomentumEvent, e: &mut Enc) {
        match *i {
            MomentumEvent::Key { at, tau_bits, echo } => {
                e.u8(match echo {
                    KeyEcho::Seen => 0,
                    KeyEcho::Unseen => 4,
                });
                e.offset(at);
                e.u32(tau_bits);
            }
            MomentumEvent::Reset => e.u8(1),
            MomentumEvent::Tick { cur, at, geom, cfg } => {
                e.u8(2);
                e.opt(cur, |e, (row, col)| {
                    e.u16(row);
                    e.u16(col);
                });
                e.offset(at);
                e.u64(usize_u64(geom.cw));
                e.u64(usize_u64(geom.ch));
                e.u64(usize_u64(geom.rows));
                e.u64(usize_u64(geom.cols));
                e.u16(geom.origin_x);
                e.u16(geom.origin_y);
                e.u16(geom.win_w);
                e.u16(geom.win_h);
                e.u16(geom.head);
                e.bool(cfg.enabled);
                e.u32(cfg.intensity);
                e.u32(cfg.tau_s);
                e.u32(cfg.radius_cells);
                e.u32(cfg.base);
                e.bool(cfg.dark_theme);
                e.u32(cfg.hue);
                e.u32(cfg.theme_fg);
                e.u32(cfg.theme_bg);
                e.bool(cfg.block);
                e.opt(cfg.glyph_span, |e, (first, end)| {
                    e.i32(first);
                    e.i32(end);
                });
                e.u8(cfg.ink_byte());
            }
            MomentumEvent::Query { at, frame_interval } => {
                e.u8(3);
                e.offset(at);
                e.duration(frame_interval);
            }
        }
    }

    #[inline]
    fn decode_input(d: &mut Dec<'_>) -> Result<MomentumEvent, WireFault> {
        match d.u8("event.tag")? {
            tag @ (0 | 4) => Ok(MomentumEvent::Key {
                at: d.offset("event.key.at")?,
                tau_bits: d.u32("event.key.tau")?,
                echo: if tag == 0 {
                    KeyEcho::Seen
                } else {
                    KeyEcho::Unseen
                },
            }),
            1 => Ok(MomentumEvent::Reset),
            2 => {
                let cur = d.opt("event.tick.cur", |d| {
                    Ok((d.u16("event.tick.row")?, d.u16("event.tick.col")?))
                })?;
                let at = d.offset("event.tick.at")?;
                let geom = GeomRecord {
                    cw: usize_field(d, "geom.cw")?,
                    ch: usize_field(d, "geom.ch")?,
                    rows: usize_field(d, "geom.rows")?,
                    cols: usize_field(d, "geom.cols")?,
                    origin_x: d.u16("geom.origin_x")?,
                    origin_y: d.u16("geom.origin_y")?,
                    win_w: d.u16("geom.win_w")?,
                    win_h: d.u16("geom.win_h")?,
                    head: d.u16("geom.head")?,
                };
                let mut cfg = ConfigRecord {
                    enabled: d.bool("cfg.enabled")?,
                    intensity: d.u32("cfg.intensity")?,
                    tau_s: d.u32("cfg.tau_s")?,
                    radius_cells: d.u32("cfg.radius_cells")?,
                    base: d.u32("cfg.base")?,
                    dark_theme: d.bool("cfg.dark_theme")?,
                    hue: d.u32("cfg.hue")?,
                    theme_fg: d.u32("cfg.theme_fg")?,
                    theme_bg: d.u32("cfg.theme_bg")?,
                    block: d.bool("cfg.block")?,
                    bar: false,
                    glyph_span: d.opt("cfg.glyph_span", |d| {
                        Ok((d.i32("cfg.glyph_span")?, d.i32("cfg.glyph_span")?))
                    })?,
                    light_vivid: false,
                    chosen_hue: false,
                    chosen_base: false,
                };
                (cfg.light_vivid, cfg.chosen_hue, cfg.chosen_base, cfg.bar) =
                    ConfigRecord::read_ink_byte(d, "cfg.ink")?;
                Ok(MomentumEvent::Tick { cur, at, geom, cfg })
            }
            3 => Ok(MomentumEvent::Query {
                at: d.offset("event.query.at")?,
                frame_interval: d.duration("event.query.frame_interval")?,
            }),
            tag => Err(WireFault::BadTag {
                field: "event.tag",
                tag,
            }),
        }
    }

    fn encode_frame(o: &MomentumObservation, e: &mut Enc) {
        match o {
            MomentumObservation::Keyed => e.u8(0),
            MomentumObservation::WasReset => e.u8(1),
            MomentumObservation::Frame {
                fill,
                hot,
                fp,
                out,
                under,
            } => encode_frame(
                e,
                &MomentumGlowFrame {
                    fill: *fill,
                    hot: *hot,
                    fp: *fp,
                },
                out,
                under,
            ),
            MomentumObservation::Schedule {
                is_active,
                settling,
                cold_at,
                deadline,
            } => {
                e.u8(3);
                e.bool(*is_active);
                e.bool(*settling);
                e.opt(*cold_at, |e, t| e.offset(t));
                e.opt(*deadline, |e, t| e.offset(t));
            }
            MomentumObservation::Refused(r) => {
                e.u8(4);
                encode_refusal(e, r);
            }
        }
    }

    #[inline]
    fn paused_class(i: &MomentumEvent) -> PausedClass {
        match i {
            MomentumEvent::Key { .. } | MomentumEvent::Reset => PausedClass::Queue,
            MomentumEvent::Tick { .. } | MomentumEvent::Query { .. } => PausedClass::Skip,
        }
    }

    #[inline]
    fn time_of(ev: &MomentumEvent) -> Option<i128> {
        time_of(ev)
    }

    #[inline]
    fn is_reset(ev: &MomentumEvent) -> bool {
        matches!(ev, MomentumEvent::Reset)
    }

    #[inline]
    fn last_time(state: &MomentumState, epoch: Instant) -> Option<i128> {
        state.last_time(epoch)
    }

    #[inline]
    fn at_rest(state: &MomentumState, _epoch: Instant) -> bool {
        at_rest(state)
    }

    #[inline]
    fn unseen(state: &MomentumState) -> u32 {
        state.unseen()
    }

    #[inline]
    fn unseen_logged(counts: &EventCounts) -> usize {
        counts.unseen
    }

    #[inline]
    fn counts(inputs: &[MomentumEvent]) -> EventCounts {
        EventCounts::of(inputs)
    }

    #[inline]
    fn code_of(p: MomentumCode) -> Sched {
        Sched::Prebuilt(p)
    }

    #[inline]
    fn prebuilt_of(c: Sched) -> Option<MomentumCode> {
        c.prebuilt()
    }

    #[inline]
    fn code_name(c: Sched) -> &'static str {
        c.name()
    }

    #[inline]
    fn prebuilt_name(p: MomentumCode) -> &'static str {
        p.name()
    }

    #[inline]
    fn prebuilt_named(name: &str) -> Option<MomentumCode> {
        MomentumCode::from_name(name)
    }

    #[inline]
    fn code_artifact(c: Sched, build: &str) -> ArtifactId {
        match c {
            Sched::Prebuilt(code) => code.artifact(build),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(plan) => plan.artifact(build),
        }
    }

    fn entry(_c: Sched, state: &MomentumState, epoch: Instant) -> Result<(), EntryRefusal> {
        let res = match state.glow.snapshot(epoch) {
            Ok(s) => s.validate(),
            Err(r) => Err(r),
        };
        res.map_err(|r| EntryRefusal {
            law: SNAPSHOT_VALID,
            detail: r.to_string(),
        })
    }

    #[inline]
    fn apply(
        c: Sched,
        state: &mut MomentumState,
        epoch: Instant,
        input: &MomentumEvent,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> Applied {
        c.apply(state, epoch, input, out, under)
    }

    #[inline]
    fn observe(
        applied: &Applied,
        epoch: Instant,
        out: Vec<GlowQuad>,
        under: Vec<GlowQuad>,
    ) -> MomentumObservation {
        observe_owned(applied, epoch, out, under)
    }

    #[inline]
    fn encode_applied(
        applied: &Applied,
        epoch: Instant,
        out: &[GlowQuad],
        under: &[GlowQuad],
        e: &mut Enc,
    ) {
        match applied {
            Applied::Frame(frame) => {
                e.reserve(frame_room(out.len().saturating_add(under.len())));
                encode_frame(e, frame, out, under);
            }
            other => Self::encode_frame(&observe(other, epoch, &[], &[]), e),
        }
    }

    #[inline]
    fn held_of(
        input: &MomentumEvent,
        applied: &Applied,
    ) -> Option<(MomentumGlowFrame, MomentumPresentation)> {
        let MomentumEvent::Tick { cur, geom, cfg, .. } = *input else {
            return None;
        };
        let frame = match applied {
            Applied::Frame(f) => *f,
            _ => MomentumGlowFrame::default(),
        };
        Some((
            frame,
            MomentumPresentation {
                cur,
                geom,
                dark: cfg.dark_theme,
                block: cfg.block,
            },
        ))
    }

    #[inline]
    fn shows(at: &MomentumPresentation, input: &MomentumEvent) -> bool {
        let MomentumEvent::Tick { cur, geom, cfg, .. } = *input else {
            return false;
        };
        let Some((cr, cc)) = cur else {
            return false;
        };
        let Some((hr, hc)) = at.cur else {
            return false;
        };
        let intensity = f32::from_bits(cfg.intensity);
        cfg.enabled
            && intensity.is_finite()
            && intensity > 0.0
            && geom.cw != 0
            && geom.ch != 0
            && hr == cr
            && hc == cc
            && at.geom == geom
            && at.dark == cfg.dark_theme
            && at.block == cfg.block
    }

    #[inline]
    fn noted(
        live: &mut MomentumLive,
        _c: Sched,
        _state: &MomentumState,
        input: &MomentumEvent,
        applied: &Applied,
        epoch: Instant,
    ) {
        #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
        if let MomentumEvent::Tick { cur, geom, cfg, .. } = *input {
            live.guard.tick(cur, geom, cfg, _c, _state);
        }
        if let MomentumEvent::Query { at, .. } = *input {
            let deadline = match *applied {
                Applied::Schedule { deadline, .. } => deadline.map(|t| offset_of(epoch, t)),
                _ => None,
            };
            live.last_query = Some((at, deadline));
        }
    }

    #[inline]
    fn before_pause(a: &mut Attached<Self>, now: Instant, frame_interval: Duration) {
        let _ = a.next_change_deadline(now, frame_interval);
    }
}

/// The target this build runs on, as `arch-os[-env]` (`std::env::consts` and `target_env`).
#[inline]
#[must_use]
pub fn target_name() -> String {
    let env = if cfg!(target_env = "gnu") {
        "-gnu"
    } else if cfg!(target_env = "musl") {
        "-musl"
    } else if cfg!(target_env = "msvc") {
        "-msvc"
    } else {
        ""
    };
    format!("{}-{}{env}", std::env::consts::ARCH, std::env::consts::OS)
}

/// The task artifact `aterm.cursor.momentum-glow` v1.
#[inline]
#[must_use]
pub fn task_artifact() -> ArtifactId {
    let mut e = Enc::new();
    e.str("the typing-momentum glow of one window's cursor (aterm_effects::cursor_momentum::MomentumGlow)");
    ArtifactId::new(ArtifactKind::Task, TASK_NAME, 1, e.as_bytes())
}

/// The schema artifact `momentum-glow.state+events` v3: an older build refuses a v3 trace by its
/// identity, so it never misreads an unseen key or the state's count.
#[inline]
#[must_use]
pub fn schema_artifact() -> ArtifactId {
    let mut e = Enc::new();
    e.u16(MOMENTUM_GLOW_SNAPSHOT_SCHEMA);
    e.u16(MOMENTUM_EVENT_ENCODING);
    e.str("state: snapshot, optional clock, unseen-key count; inputs Key (seen tag 0, unseen tag 4)|Reset|Tick|Query; observations Keyed|WasReset|Frame|Schedule|Refused");
    ArtifactId::new(ArtifactKind::Schema, SCHEMA_NAME, 3, e.as_bytes())
}

/// The profile artifact.
#[inline]
#[must_use]
pub fn profile_artifact() -> ArtifactId {
    let target = target_name();
    let p = MOMENTUM_PROFILE;
    let mut e = Enc::new();
    e.str(&target);
    e.str(NUMERIC_PROFILE);
    e.u32(p.max_extent_px);
    e.f32_bits(p.max_radius_cells);
    e.f32_bits(p.max_tau_s);
    e.duration(p.max_frame_interval);
    e.duration(p.horizon);
    ArtifactId::new(
        ArtifactKind::Profile,
        &format!("{target}/{NUMERIC_PROFILE}"),
        1,
        e.as_bytes(),
    )
}

/// The five artifacts a momentum task runs under, with `code` running it.
#[inline]
#[must_use]
pub fn momentum_identity(code: &PrebuiltMomentum) -> TaskIdentity {
    identity_of(code)
}

/// The prebuilt implementations compiled into this build (design §2 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MomentumCode {
    /// The shipped engine, called directly.
    Shipped,
    /// The shipped engine with a frame-interval wake whenever the glow is hot, so no settle forecast can strand it.
    Safe,
    /// **An intentionally introduced demonstration fault, not a defect of shipped code**.
    #[cfg(any(test, debug_assertions))]
    DemoStrandedSettle,
}

impl MomentumCode {
    /// Every implementation in this build.
    pub const ALL: &'static [MomentumCode] = &[
        MomentumCode::Shipped,
        MomentumCode::Safe,
        #[cfg(any(test, debug_assertions))]
        MomentumCode::DemoStrandedSettle,
    ];

    /// The code artifact's name.
    #[inline]
    #[must_use]
    pub const fn artifact_name(self) -> &'static str {
        match self {
            MomentumCode::Shipped => SHIPPED_CODE_NAME,
            MomentumCode::Safe => SAFE_CODE_NAME,
            #[cfg(any(test, debug_assertions))]
            MomentumCode::DemoStrandedSettle => DEMO_CODE_NAME,
        }
    }

    /// The short name a control verb takes (`shipped`, `safe`, `demo-stranded-settle`).
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            MomentumCode::Shipped => "shipped",
            MomentumCode::Safe => "safe",
            #[cfg(any(test, debug_assertions))]
            MomentumCode::DemoStrandedSettle => "demo-stranded-settle",
        }
    }

    /// The implementation a short or artifact name names.
    #[inline]
    #[must_use]
    pub const fn from_name(name: &str) -> Option<MomentumCode> {
        match name.as_bytes() {
            b"shipped" | b"momentum-glow.shipped" => Some(MomentumCode::Shipped),
            b"safe" | b"momentum-glow.safe" => Some(MomentumCode::Safe),
            #[cfg(any(test, debug_assertions))]
            b"demo-stranded-settle" | b"momentum-glow.demo-stranded-settle" => {
                Some(MomentumCode::DemoStrandedSettle)
            }
            _ => None,
        }
    }

    /// What the artifact declares about itself.
    #[inline]
    const fn note(self) -> &'static str {
        match self {
            MomentumCode::Shipped => "the shipped engine, called directly",
            MomentumCode::Safe => {
                "the shipped engine, its scheduler answer replaced: next_change_deadline is \
                 Some(now + frame_interval) while is_active or settling, None otherwise"
            }
            #[cfg(any(test, debug_assertions))]
            MomentumCode::DemoStrandedSettle => {
                "an intentionally introduced demonstration fault, not a defect of shipped code: \
                 next_change_deadline forgets the settle forecast (Some(now + frame_interval) \
                 while is_active, None otherwise)"
            }
        }
    }

    /// Its artifact.
    #[inline]
    #[must_use]
    pub fn artifact(self, build: &str) -> ArtifactId {
        let mut e = Enc::new();
        e.str(self.artifact_name());
        e.u32(1);
        e.str(build);
        e.str(self.note());
        ArtifactId::new(ArtifactKind::Code, self.artifact_name(), 1, e.as_bytes())
    }

    /// Frame cadence: the glow is hot and its last two frames differed.
    #[inline]
    #[must_use]
    pub const fn is_active(self, s: &MomentumState) -> bool {
        s.glow.is_active()
    }

    /// Hot, but its last two frames were identical.
    #[inline]
    #[must_use]
    pub const fn settling(self, s: &MomentumState) -> bool {
        s.glow.settling()
    }

    /// The analytic instant the latched amplitude goes cold.
    #[inline]
    #[must_use]
    pub fn cold_at(self, s: &MomentumState) -> Option<Instant> {
        s.glow.cold_at()
    }

    /// The wake this code names for the host's lane.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(
        self,
        s: &MomentumState,
        now: Instant,
        frame_interval: Duration,
    ) -> Option<Instant> {
        match self {
            MomentumCode::Shipped => s.glow.next_change_deadline(now, frame_interval),
            MomentumCode::Safe => {
                if s.glow.is_active() || s.glow.settling() {
                    now.checked_add(frame_interval)
                } else {
                    None
                }
            }
            #[cfg(any(test, debug_assertions))]
            MomentumCode::DemoStrandedSettle => {
                // The demonstration fault: the settle forecast is forgotten.
                if s.glow.is_active() {
                    now.checked_add(frame_interval)
                } else {
                    None
                }
            }
        }
    }

    /// One input, admitted then applied under this code: [`Sched::apply`] of [`Sched::Prebuilt`].
    #[inline]
    pub fn apply(
        self,
        s: &mut MomentumState,
        epoch: Instant,
        ev: &MomentumEvent,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> Applied {
        Sched::Prebuilt(self).apply(s, epoch, ev, out, under)
    }
}

/// What answers the scheduler question for a code: a prebuilt implementation or a schedule plan (design §10.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sched {
    /// A prebuilt implementation compiled into this build.
    Prebuilt(MomentumCode),
    /// A schedule plan, built only by the candidate evaluator (`m4`) and by publication of an admitted result.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    Plan(SchedulePlan),
}

impl Sched {
    /// The prebuilt implementation, or `None` for a plan.
    #[inline]
    #[must_use]
    pub const fn prebuilt(self) -> Option<MomentumCode> {
        match self {
            Sched::Prebuilt(code) => Some(code),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(_) => None,
        }
    }

    /// The short name a status line shows.
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Sched::Prebuilt(code) => code.name(),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(_) => plan::PLAN_NAME,
        }
    }

    /// Frame cadence, as this code classifies the state.
    #[inline]
    #[must_use]
    pub fn is_active(self, s: &MomentumState) -> bool {
        match self {
            Sched::Prebuilt(code) => code.is_active(s),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(plan) => plan.is_glow_active(&s.glow),
        }
    }

    /// Hot and not active, as this code classifies the state.
    #[inline]
    #[must_use]
    pub fn settling(self, s: &MomentumState) -> bool {
        match self {
            Sched::Prebuilt(code) => code.settling(s),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(plan) => plan.is_glow_settling(&s.glow),
        }
    }

    /// The analytic instant the latched amplitude goes cold: the engine's, for every code.
    #[inline]
    #[must_use]
    pub fn cold_at(self, s: &MomentumState) -> Option<Instant> {
        s.glow.cold_at()
    }

    /// The wake this code names for the host's lane.
    #[inline]
    #[must_use]
    pub fn next_change_deadline(
        self,
        s: &MomentumState,
        now: Instant,
        frame_interval: Duration,
    ) -> Option<Instant> {
        match self {
            Sched::Prebuilt(code) => code.next_change_deadline(s, now, frame_interval),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(plan) => {
                plan.next_change_deadline(&PlanView::of(&s.glow), now, frame_interval)
            }
        }
    }

    /// One input.
    #[inline]
    pub fn apply(
        self,
        s: &mut MomentumState,
        epoch: Instant,
        ev: &MomentumEvent,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> Applied {
        #[inline]
        fn admit_at(
            p: &crate::cursor_momentum::MomentumProfile,
            s: &MomentumState,
            epoch: Instant,
            at: i128,
        ) -> Result<Instant, AdmitRefusal> {
            admit_time(p, s.last_time(epoch), at)?;
            instant_of(epoch, at).ok_or(AdmitRefusal::Horizon(at))
        }
        let p = &MOMENTUM_PROFILE;
        match *ev {
            MomentumEvent::Key { at, tau_bits, echo } => {
                let tau = f32::from_bits(tau_bits);
                let now = match admit_at(p, s, epoch, at) {
                    Ok(now) => match admit_tau(p, tau) {
                        Ok(()) => now,
                        Err(r) => return Applied::Refused(r),
                    },
                    Err(r) => return Applied::Refused(r),
                };
                s.glow.on_key(now, tau);
                s.clock = Some(at);
                s.unseen = s.unseen.saturating_add(u32::from(echo == KeyEcho::Unseen));
                Applied::Keyed
            }
            MomentumEvent::Reset => {
                s.glow.reset();
                Applied::WasReset
            }
            MomentumEvent::Tick { cur, at, geom, cfg } => {
                let (geom, cfg) = (geom.geom(), cfg.config());
                let now = match admit_at(p, s, epoch, at) {
                    Ok(now) => now,
                    Err(r) => return Applied::Refused(r),
                };
                let budget = match admit_tick(p, cur, &geom, &cfg) {
                    Ok(budget) => budget,
                    Err(r) => return Applied::Refused(r),
                };
                out.reserve(budget.out);
                under.reserve(budget.under);
                let frame = s.glow.tick(cur, now, geom, &cfg, out, under);
                s.clock = Some(at);
                Applied::Frame(frame)
            }
            MomentumEvent::Query { at, frame_interval } => {
                let now = match admit_at(p, s, epoch, at) {
                    Ok(now) => match admit_frame_interval(p, frame_interval) {
                        Ok(()) => now,
                        Err(r) => return Applied::Refused(r),
                    },
                    Err(r) => return Applied::Refused(r),
                };
                let answer = match self {
                    Sched::Prebuilt(code) => Applied::Schedule {
                        is_active: code.is_active(s),
                        settling: code.settling(s),
                        cold_at: code.cold_at(s),
                        deadline: code.next_change_deadline(s, now, frame_interval),
                    },
                    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
                    Sched::Plan(plan) => {
                        let view = PlanView::of(&s.glow);
                        Applied::Schedule {
                            is_active: plan.is_active(&view),
                            settling: plan.settling(&view),
                            cold_at: view.cold_at(),
                            deadline: plan.next_change_deadline(&view, now, frame_interval),
                        }
                    }
                };
                s.clock = Some(at);
                answer
            }
        }
    }
}

impl EffectCode<MomentumEngine> {
    /// `code` as compiled into the build `build`.
    #[inline]
    #[must_use]
    pub fn new(code: MomentumCode, build: &str) -> Self {
        Self::prebuilt(code, build)
    }

    /// The plan interpreter reading `plan` as compiled into `build` ([`SchedulePlan::artifact`]), which no verb or code name reaches.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    #[inline]
    #[must_use]
    pub(crate) fn planned(plan: SchedulePlan, build: &str) -> Self {
        Self::compiled(Sched::Plan(plan), build)
    }

    /// Which prebuilt implementation, or `None` for a planned code.
    #[inline]
    #[must_use]
    pub const fn code(&self) -> Option<MomentumCode> {
        self.code.prebuilt()
    }

    /// What answers the scheduler question.
    #[inline]
    #[must_use]
    pub const fn sched(&self) -> Sched {
        self.code
    }
}

/// `momentum-glow.no-stranded-light` v1 (design §2 item 3), judged over a replay's observations.
#[derive(Clone, Debug)]
pub struct NoStrandedLight {
    id: ArtifactId,
}

impl Default for NoStrandedLight {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl NoStrandedLight {
    /// The contract.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        NoStrandedLight {
            id: Self::artifact(),
        }
    }

    /// Its artifact: the descriptor names the laws and the settle margin.
    #[inline]
    #[must_use]
    pub fn artifact() -> ArtifactId {
        const LAWS: [&str; 4] = [
            HOT_HAS_WAKE,
            WAKE_NO_LATER_THAN_CLEAR,
            COLD_TICK_CLEARS,
            RUN_MATCHES_INPUTS,
        ];
        let mut e = Enc::new();
        let mut li = 0;
        while li < LAWS.len() {
            e.str(LAWS[li]);
            li += 1;
        }
        e.duration(SETTLE_MARGIN);
        ArtifactId::new(ArtifactKind::Contract, CONTRACT_NAME, 1, e.as_bytes())
    }
}

#[inline]
const fn fail(cursor: u64, law: &'static str, detail: String) -> Verdict {
    Verdict::Fail {
        cursor,
        law,
        detail,
    }
}

#[inline]
const fn nanos(d: Duration) -> i128 {
    let n = d.as_nanos();
    if n > i128::MAX as u128 {
        i128::MAX
    } else {
        n as i128
    }
}

struct StrandedFold {
    margin: i128,
    forecast: Option<(i128, ConfigRecord)>,
    last_tick: Option<ConfigRecord>,
    latch_current: bool,
    failed: Option<Verdict>,
}

impl StrandedFold {
    #[inline]
    const fn new() -> Self {
        StrandedFold {
            margin: nanos(SETTLE_MARGIN),
            forecast: None,
            last_tick: None,
            latch_current: false,
            failed: None,
        }
    }

    #[inline]
    fn step(
        &mut self,
        cursor: u64,
        input: Option<&MomentumEvent>,
        index: usize,
        obs: &MomentumObservation,
    ) {
        if self.failed.is_some() {
            return;
        }
        let Some(input) = input else {
            self.failed = Some(fail(
                cursor,
                RUN_MATCHES_INPUTS,
                format!("observation {index} has no recorded input"),
            ));
            return;
        };
        let margin = self.margin;
        match (input, obs) {
            (_, MomentumObservation::Refused(_)) => {}
            (MomentumEvent::Key { .. }, MomentumObservation::Keyed)
            | (MomentumEvent::Reset, MomentumObservation::WasReset) => {
                self.forecast = None;
                self.latch_current = false;
            }
            (
                MomentumEvent::Tick { at, cfg, .. },
                MomentumObservation::Frame {
                    hot,
                    fp,
                    out,
                    under,
                    fill,
                },
            ) => {
                if let Some((cold, latched)) = self.forecast {
                    let lit =
                        *hot || *fp != 0 || fill.is_some() || !out.is_empty() || !under.is_empty();
                    if *at >= cold.saturating_add(margin) && latched == *cfg && lit {
                        self.failed = Some(fail(
                            cursor,
                            COLD_TICK_CLEARS,
                            format!(
                                "the tick at {at} ns, past the cold forecast {cold} ns, still draws \
                                 (hot {hot}, fp {fp:#x}, {} + {} quads)",
                                out.len(),
                                under.len()
                            ),
                        ));
                        return;
                    }
                }
                if matches!(self.forecast, Some((_, latched)) if latched != *cfg) {
                    self.forecast = None;
                }
                self.last_tick = Some(*cfg);
                self.latch_current = true;
            }
            (
                MomentumEvent::Query { at, frame_interval },
                MomentumObservation::Schedule {
                    is_active,
                    settling,
                    cold_at,
                    deadline,
                },
            ) => {
                if (*is_active || *settling) && deadline.is_none() {
                    self.failed = Some(fail(
                        cursor,
                        HOT_HAS_WAKE,
                        format!(
                            "the glow is hot ({}) at {at} ns and the code names no wake",
                            if *settling { "settling" } else { "active" }
                        ),
                    ));
                    return;
                }
                if *settling && let Some(d) = *deadline {
                    let floor = at.saturating_add(nanos(*frame_interval));
                    let limit = match cold_at {
                        Some(c) => c.saturating_add(margin).max(floor),
                        None => floor,
                    };
                    if d < floor || d > limit {
                        self.failed = Some(fail(
                            cursor,
                            WAKE_NO_LATER_THAN_CLEAR,
                            format!(
                                "the settle wake {d} ns is outside [{floor}, {limit}] ns \
                                 (at {at} ns, cold at {cold_at:?})"
                            ),
                        ));
                        return;
                    }
                }
                self.forecast = match (self.latch_current, *cold_at, self.last_tick) {
                    (true, Some(c), Some(t)) => Some((c, t)),
                    _ => None,
                };
            }
            (input, obs) => {
                self.failed = Some(fail(
                    cursor,
                    RUN_MATCHES_INPUTS,
                    format!("{input:?} observed as {obs:?}"),
                ));
            }
        }
    }

    #[inline]
    fn finish(
        self,
        inputs_len: usize,
        observations_len: usize,
        last_cursor: Option<u64>,
        refused: Option<&(u64, StepRefusal)>,
    ) -> Verdict {
        if let Some(v) = self.failed {
            return v;
        }
        if let Some((cursor, r)) = refused {
            return fail(
                *cursor,
                RUN_MATCHES_INPUTS,
                format!("the replay stopped: {r}"),
            );
        }
        if observations_len != inputs_len {
            let cursor = last_cursor.unwrap_or(0);
            return fail(
                cursor,
                RUN_MATCHES_INPUTS,
                format!("{observations_len} observations for {inputs_len} inputs"),
            );
        }
        Verdict::Pass
    }
}

impl Contract<MomentumSchema> for NoStrandedLight {
    #[inline]
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    #[inline]
    fn judge(
        &self,
        _schema: &MomentumSchema,
        inputs: &[MomentumEvent],
        run: &Run<MomentumSchema>,
    ) -> Verdict {
        let mut fold = StrandedFold::new();
        let mut i = 0;
        while i < run.observations.len() {
            let (cursor, ref obs) = run.observations[i];
            fold.step(cursor, inputs.get(i), i, obs);
            if fold.failed.is_some() {
                break;
            }
            i += 1;
        }
        fold.finish(
            inputs.len(),
            run.observations.len(),
            run.observations.last().map(|&(c, _)| c),
            run.refused.as_ref(),
        )
    }

    #[inline]
    fn replay_judged<C: Code<MomentumSchema>>(
        &self,
        schema: &MomentumSchema,
        code: &C,
        start: MomentumState,
        inputs: &[MomentumEvent],
        first_cursor: u64,
        port: &mut dyn EffectPort,
    ) -> (FoldedRun<MomentumSchema>, Verdict) {
        let mut fold = StrandedFold::new();
        let run = replay_fold(
            schema,
            code,
            start,
            inputs,
            first_cursor,
            port,
            |i, cursor, input, obs| {
                fold.step(cursor, Some(input), i, &obs);
            },
        );
        let verdict = fold.finish(
            inputs.len(),
            run.observations,
            run.last_cursor,
            run.refused.as_ref(),
        );
        (run, verdict)
    }
}

/// Whether the glow is at rest.
#[inline]
const fn at_rest(s: &MomentumState) -> bool {
    s.glow.is_at_rest()
}

/// What one tick presents: its cursor cell, geometry and config.
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
type Presentation = (Option<(u16, u16)>, GeomRecord, ConfigRecord);

/// The plan-only presentation guard (design §10.1), which lets a plan read a frame as settling only while the presentation is unchanged since the last tick.
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Guard {
    /// The last delivered tick's presentation.
    last: Option<Presentation>,
    /// That presentation differs from the one before it (a first tick moves).
    moved: bool,
    /// The ticks after which the guard held the frame cadence a live plan released, because the presentation moved.
    firings: u64,
}

#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
impl Guard {
    /// Keeps a delivered tick's presentation and whether it moved, counting a firing when that holds the cadence a live plan (`sched`, after the tick at `s`) released.
    #[inline]
    fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        geom: GeomRecord,
        cfg: ConfigRecord,
        sched: Sched,
        s: &MomentumState,
    ) {
        let shown = Some((cur, geom, cfg));
        self.moved = self.last != shown;
        self.last = shown;
        if let Sched::Plan(_) = sched
            && self.moved
            && s.glow.hot()
            && !sched.is_active(s)
        {
            self.firings = self.firings.saturating_add(1);
        }
    }
}

/// The hosted horizon in signed nanoseconds.
#[inline]
const fn horizon_ns() -> i128 {
    nanos(MOMENTUM_PROFILE.horizon)
}

/// An input's time, if it has one.
#[inline]
const fn time_of(ev: &MomentumEvent) -> Option<i128> {
    match *ev {
        MomentumEvent::Key { at, .. }
        | MomentumEvent::Tick { at, .. }
        | MomentumEvent::Query { at, .. } => Some(at),
        MomentumEvent::Reset => None,
    }
}

#[cfg(test)]
thread_local! {
    /// Whether a test lets a held glow's ticks reach the boundary, the host binding's mutant.
    pub(crate) static HELD_TICK_DELIVERS: core::cell::Cell<bool> =
        const { core::cell::Cell::new(false) };
}

/// Whether a held glow's tick is delivered: never, but under a test's mutant.
#[inline]
fn held_tick_delivers() -> bool {
    #[cfg(test)]
    {
        HELD_TICK_DELIVERS.with(core::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        false
    }
}

impl Attached<MomentumEngine> {
    /// What answers the live code's scheduler question.
    #[inline]
    pub(crate) const fn sched(&self) -> Sched {
        self.code()
    }

    /// Frame cadence as the running host reads it: `code.is_active ∨ (hot ∧ moved)` for a plan (design §10.1), the code's own answer otherwise.
    #[inline]
    fn is_active(&self) -> bool {
        let (sched, s) = (self.sched(), self.state());
        match sched {
            Sched::Prebuilt(code) => code.is_active(s),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(_) => sched.is_active(s) || (s.glow.hot() && self.live.guard.moved),
        }
    }

    /// Settling as the running host reads it: `code.settling ∧ ¬moved` for a plan, the code's own answer otherwise (disjoint from [`Attached::is_active`], their union hot).
    #[inline]
    fn settling(&self) -> bool {
        let (sched, s) = (self.sched(), self.state());
        match sched {
            Sched::Prebuilt(code) => code.settling(s),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(_) => sched.settling(s) && !self.live.guard.moved,
        }
    }

    /// The presentation guard's firings so far.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    #[inline]
    pub(crate) const fn guard_firings(&self) -> u64 {
        self.live.guard.firings
    }

    #[inline]
    fn next_change_deadline(&mut self, now: Instant, frame_interval: Duration) -> Option<Instant> {
        let (at, moved) = self.logical(now);
        let ev = MomentumEvent::Query { at, frame_interval };
        match self.deliver(&ev, moved, &mut Vec::new(), &mut Vec::new())? {
            Applied::Schedule { deadline, .. } => deadline,
            _ => {
                let (sched, s) = (self.sched(), self.state());
                if sched.is_active(s) || sched.settling(s) {
                    now.checked_add(frame_interval)
                } else {
                    None
                }
            }
        }
    }
}

impl Hosted<MomentumEngine> {
    /// One printable key (`MomentumGlow::on_key`). A key while a key is held stops the recording
    /// first ([`RAW_KEY`]): only [`Self::aos_release`] logs a held run.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AosKeyHold",
            action = "Key",
            project = "aterm_effects::aos_momentum::hold_conformance_tests::Real::project"
        )
    )]
    #[inline]
    pub fn on_key(&mut self, now: Instant, tau_s: f32) {
        self.expire_past_horizon(now);
        if self.aos_holding() {
            self.detach_by_itself(RAW_KEY);
        }
        match &mut self.mode {
            HostMode::Direct(g) => g.on_key(now, tau_s),
            HostMode::Hosted(h) => {
                let (at, moved) = h.logical(now);
                let ev = MomentumEvent::Key {
                    at,
                    tau_bits: tau_s.to_bits(),
                    echo: KeyEcho::Seen,
                };
                let _ = h.deliver(&ev, moved, &mut Vec::new(), &mut Vec::new());
            }
        }
    }

    /// Back to rest (`MomentumGlow::reset`). A reset while a key is held stops the recording first
    /// ([`RAW_KEY`]).
    #[inline]
    pub fn reset(&mut self) {
        if self.aos_holding() {
            self.detach_by_itself(RAW_KEY);
        }
        match &mut self.mode {
            HostMode::Direct(g) => g.reset(),
            HostMode::Hosted(h) => {
                if h.reset_changes_nothing() {
                    return;
                }
                let _ = h.deliver(
                    &MomentumEvent::Reset,
                    false,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
            }
        }
    }

    /// One frame (`MomentumGlow::tick`). While a key is held nothing is delivered: the frame is the
    /// code's own for this caret, geometry and config at the latest admitted instant, drawn on a
    /// copy of the state, so the glow holds still on the caret. While paused it is the last
    /// delivered tick's where that is still shown.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AosKeyHold",
            action = "Tick",
            project = "aterm_effects::aos_momentum::hold_conformance_tests::Real::project"
        )
    )]
    #[inline]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame {
        self.expire_past_horizon(now);
        match &mut self.mode {
            HostMode::Direct(g) => g.tick(cur, now, geom, cfg, out, under),
            HostMode::Hosted(h) if h.awaiting_echo.is_some() && !held_tick_delivers() => {
                // The glow holds still on the tick's caret: the code's own frame at the latest
                // admitted instant, drawn on a copy of the state.
                let ev = MomentumEvent::Tick {
                    cur,
                    at: h.last_admitted(),
                    geom: GeomRecord::from(&geom),
                    cfg: ConfigRecord::from(cfg),
                };
                match h.still(&ev, out, under) {
                    Applied::Frame(frame) => frame,
                    _ => MomentumGlowFrame::default(),
                }
            }
            HostMode::Hosted(h) => {
                let (at, moved) = h.logical(now);
                let ev = MomentumEvent::Tick {
                    cur,
                    at,
                    geom: GeomRecord::from(&geom),
                    cfg: ConfigRecord::from(cfg),
                };
                match h.deliver(&ev, moved, out, under) {
                    Some(Applied::Frame(frame)) => frame,
                    Some(_) => MomentumGlowFrame::default(),
                    None => match h.shown(&ev) {
                        Some(held) => {
                            out.extend_from_slice(&held.out);
                            under.extend_from_slice(&held.under);
                            held.frame
                        }
                        None => MomentumGlowFrame::default(),
                    },
                }
            }
        }
    }

    /// Frame cadence (`MomentumGlow::is_active`, or a plan's under its presentation guard); `false` while paused or while a key is held.
    #[inline]
    #[must_use]
    pub fn is_active(&self) -> bool {
        match &self.mode {
            HostMode::Direct(g) => g.is_active(),
            HostMode::Hosted(h) => {
                !h.boundary.is_paused() && h.awaiting_echo.is_none() && h.is_active()
            }
        }
    }

    /// Hot with no frame-to-frame change (`MomentumGlow::settling`); `false` while paused, and
    /// `true` while a key is held, so the lane wakes by the hold's deadline
    /// ([`Self::next_change_deadline`]).
    #[inline]
    #[must_use]
    pub fn settling(&self) -> bool {
        match &self.mode {
            HostMode::Direct(g) => g.settling(),
            HostMode::Hosted(h) => {
                !h.boundary.is_paused() && (h.awaiting_echo.is_some() || h.settling())
            }
        }
    }

    /// The analytic cold instant (`MomentumGlow::cold_at`).
    #[inline]
    #[must_use]
    pub fn cold_at(&self) -> Option<Instant> {
        match &self.mode {
            HostMode::Direct(g) => g.cold_at(),
            HostMode::Hosted(h) => h.sched().cold_at(h.state()),
        }
    }

    /// The decayed typing rate at `now` (`MomentumGlow::value`), for diagnostics.
    #[inline]
    #[must_use]
    pub fn value(&self, now: Instant, tau_s: f32) -> f32 {
        match &self.mode {
            HostMode::Direct(g) => g.value(now, tau_s),
            HostMode::Hosted(h) => h.state().glow.value(now, tau_s),
        }
    }

    /// The wake the live code names (`MomentumGlow::next_change_deadline`); while a key is held,
    /// the hold's deadline but never sooner than a frame from `now`, asked of no code and recorded
    /// nowhere.
    #[inline]
    pub fn next_change_deadline(
        &mut self,
        now: Instant,
        frame_interval: Duration,
    ) -> Option<Instant> {
        self.expire_past_horizon(now);
        match &mut self.mode {
            HostMode::Direct(g) => g.next_change_deadline(now, frame_interval),
            HostMode::Hosted(h) => match h.awaiting_echo {
                Some(deadline) => Some(match now.checked_add(frame_interval) {
                    Some(frame) => deadline.max(frame),
                    None => deadline,
                }),
                None => h.next_change_deadline(now, frame_interval),
            },
        }
    }

    /// Ends a hold: the `keys` oldest held keys, their echo proven, are logged as `Key` inputs at
    /// `now`, then the hold opens again until `rest` if keys still wait. With no hold open the keys
    /// reach only the glow, a recording stopped first ([`RAW_KEY`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AosKeyHold",
            action = "Release",
            project = "aterm_effects::aos_momentum::hold_conformance_tests::Real::project"
        )
    )]
    #[inline]
    pub fn aos_release(&mut self, now: Instant, keys: u32, tau_s: f32, rest: Option<Instant>) {
        self.release_held(now, keys, tau_s, KeyEcho::Seen, rest);
    }

    /// Ends a hold whose keys no frame proved, under the person's grant for this attach: the
    /// `keys` held keys are logged as [`KeyEcho::Unseen`] inputs, all at `now` (the count and one
    /// frame instant, never their presses), and the glow is tainted, so the next attach starts it
    /// from rest and no later recording carries an unseen key's momentum unmarked. With no hold
    /// open the keys reach only the glow, a recording stopped first ([`RAW_KEY`]), as
    /// [`Self::aos_release`] does.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AosKeyHold",
            action = "Unproven",
            project = "aterm_effects::aos_momentum::hold_conformance_tests::Real::project"
        )
    )]
    #[inline]
    pub fn aos_release_unseen(&mut self, now: Instant, keys: u32, tau_s: f32) {
        self.release_held(now, keys, tau_s, KeyEcho::Unseen, None);
        self.taint();
    }

    /// [`Self::aos_release`]'s and [`Self::aos_release_unseen`]'s body: `keys` held keys logged at
    /// `now` as `echo`, the hold then open until `rest`.
    #[inline]
    fn release_held(
        &mut self,
        now: Instant,
        keys: u32,
        tau_s: f32,
        echo: KeyEcho,
        rest: Option<Instant>,
    ) {
        self.expire_past_horizon(now);
        if let HostMode::Hosted(h) = &mut self.mode
            && h.awaiting_echo.take().is_some()
        {
            let (at, moved) = h.logical(now);
            let ev = MomentumEvent::Key {
                at,
                tau_bits: tau_s.to_bits(),
                echo,
            };
            for _ in 0..keys {
                let _ = h.deliver(&ev, moved, &mut Vec::new(), &mut Vec::new());
            }
            h.awaiting_echo = rest;
            return;
        }
        if self.aos_attached() {
            self.detach_by_itself(RAW_KEY);
        }
        self.tainted = true;
        for _ in 0..keys {
            self.on_key(now, tau_s);
        }
    }

    /// Attaches as [`Self::aos_attach`] does with `plan` as the live code ([`PrebuiltMomentum::planned`]) and log epochs of `max_events` inputs, for the candidate evaluator.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    #[inline]
    pub(crate) fn attach_planned(
        &mut self,
        epoch: Instant,
        plan: SchedulePlan,
        build: &str,
        log_epoch: u64,
        max_events: usize,
    ) {
        self.attach_code(
            epoch,
            Sched::Plan(plan),
            build,
            log_epoch,
            MOMENTUM_QUEUE_CAP,
            max_events,
        );
    }

    /// The prebuilt code running the glow (always [`MomentumCode::Shipped`] unattached), or `None` while the live code is a schedule plan.
    #[inline]
    #[must_use]
    pub const fn aos_code(&self) -> Option<MomentumCode> {
        self.aos_sched().prebuilt()
    }

    /// What answers the live code's scheduler question (always the shipped engine unattached).
    #[inline]
    #[must_use]
    pub const fn aos_sched(&self) -> Sched {
        match &self.mode {
            HostMode::Direct(_) => Sched::Prebuilt(MomentumCode::Shipped),
            HostMode::Hosted(h) => h.sched(),
        }
    }

    /// What the wrapper and its boundary hold, as plain data.
    #[inline]
    #[must_use]
    pub fn aos_status(&self) -> MomentumAosStatus {
        let h = match &self.mode {
            HostMode::Direct(g) => {
                let is_active = g.is_active();
                let settling = g.settling();
                return MomentumAosStatus {
                    code: MomentumCode::Shipped.name(),
                    hot: is_active || settling,
                    is_active,
                    settling,
                    ..MomentumAosStatus::default()
                };
            }
            HostMode::Hosted(h) => h,
        };
        let b = &h.boundary;
        let (code, state, epoch) = (h.sched(), h.state(), h.epoch());
        let (latched, fp_last, fp_prev) = match state.glow.snapshot(epoch) {
            Ok(s) => (
                Some(f32::from_bits(s.latched)),
                Some(s.fp_last),
                Some(s.fp_prev),
            ),
            Err(_) => (None, None, None),
        };
        let (log_epoch, log_len, broken) = match b.log() {
            Some(l) => (l.header().log_epoch, l.len(), l.broken()),
            None => (0, 0, None),
        };
        let (last_query_at, next_deadline) = match h.live.last_query {
            Some((at, d)) => (Some(at), d),
            None => (None, None),
        };
        // The host's reading: a plan's answer under the presentation guard, as the lane runs it.
        let (is_active, settling) = (h.is_active(), h.settling());
        #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
        let (guard_moved, guard_firings) = match code {
            Sched::Plan(_) => (Some(h.live.guard.moved), Some(h.live.guard.firings)),
            Sched::Prebuilt(_) => (None, None),
        };
        #[cfg(not(any(test, debug_assertions, feature = "aos-candidates")))]
        let (guard_moved, guard_firings) = (None, None);
        let mut checkpoint = None;
        if let Some(c) = b.checkpoint() {
            checkpoint = Some(c.digest().short());
        }
        let mut cold_at = None;
        if let Some(t) = code.cold_at(state) {
            cold_at = Some(offset_of(epoch, t));
        }
        let mut last_frame = None;
        if let Some(held) = h.held.as_ref() {
            last_frame = Some(FrameSummary {
                fill: held.frame.fill,
                hot: held.frame.hot,
                fp: held.frame.fp,
                out: held.out.len(),
                under: held.under.len(),
            });
        }
        MomentumAosStatus {
            attached: true,
            code: code.name(),
            code_digest: Some(b.pair().code.id().digest.short()),
            build: Some(h.build.clone()),
            revision: b.pair().head.revision.0,
            branch: b.pair().head.branch.0,
            paused: b.is_paused(),
            queued: b.queued(),
            queue_cap: b.queue_cap(),
            log_epoch,
            log_len,
            input_cursor: b.pair().head.input_cursor,
            broken,
            checkpoint,
            state_digest: Some(state_digest(b.schema(), state).short()),
            observation_chain: Some(h.live_chain().short()),
            latched,
            fp_last,
            fp_prev,
            clock: state.clock,
            hot: is_active || settling,
            is_active,
            settling,
            guard_moved,
            guard_firings,
            cold_at,
            last_query_at,
            next_deadline,
            last_frame,
            stats: b.stats(),
            clamped: h.clamped,
        }
    }
}

/// The last delivered tick's frame and how many quads it appended to each stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameSummary {
    pub fill: Option<u32>,
    pub hot: bool,
    pub fp: u64,
    pub out: usize,
    pub under: usize,
}

/// [`MomentumHost::aos_status`]: plain data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MomentumAosStatus {
    pub attached: bool,
    /// The running code's short name.
    pub code: &'static str,
    /// Its artifact digest, short.
    pub code_digest: Option<String>,
    pub build: Option<String>,
    pub revision: u64,
    pub branch: u64,
    pub paused: bool,
    pub queued: usize,
    pub queue_cap: usize,
    pub log_epoch: u64,
    pub log_len: usize,
    pub input_cursor: u64,
    /// Why the log is broken, if it is (a repair across it refuses).
    pub broken: Option<&'static str>,
    /// The ancestor checkpoint's digest, short.
    pub checkpoint: Option<String>,
    pub state_digest: Option<String>,
    /// The live observation chain since the ancestor, short.
    pub observation_chain: Option<String>,
    pub latched: Option<f32>,
    pub fp_last: Option<u64>,
    pub fp_prev: Option<u64>,
    pub clock: Option<i128>,
    pub hot: bool,
    pub is_active: bool,
    pub settling: bool,
    /// Whether the last tick's presentation moved, while a plan is live (`None` otherwise).
    pub guard_moved: Option<bool>,
    /// The presentation guard's firings, while a plan is live (`None` otherwise).
    pub guard_firings: Option<u64>,
    pub cold_at: Option<i128>,
    /// The latest recorded scheduler query's time…
    pub last_query_at: Option<i128>,
    /// …and the wake it was answered with.
    pub next_deadline: Option<i128>,
    pub last_frame: Option<FrameSummary>,
    pub stats: Stats,
    /// How many live inputs were recorded at the task's logical time instead of their own earlier
    /// one.
    pub clamped: u64,
}

impl fmt::Display for MomentumAosStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.attached {
            return write!(
                f,
                "aos detached code={} hot={} active={} settling={}",
                self.code, self.hot, self.is_active, self.settling
            );
        }
        #[inline]
        const fn fmt_opt_i128(v: Option<i128>) -> impl fmt::Display {
            fmt::from_fn(move |f| match v {
                Some(t) => write!(f, "{t}"),
                None => f.write_str("none"),
            })
        }
        write!(
            f,
            "aos attached code={} build={} rev={} branch={} paused={} queued={}/{} log={}@{} cursor={} broken={} \
             checkpoint={} state={} chain={} hot={} active={} settling={} latched={} fp={:#x}/{:#x} \
             cold_at={} deadline={}",
            self.code,
            self.build.as_deref().unwrap_or("none"),
            self.revision,
            self.branch,
            self.paused,
            self.queued,
            self.queue_cap,
            self.log_len,
            self.log_epoch,
            self.input_cursor,
            self.broken.unwrap_or("no"),
            self.checkpoint.as_deref().unwrap_or("none"),
            self.state_digest.as_deref().unwrap_or("none"),
            self.observation_chain.as_deref().unwrap_or("none"),
            self.hot,
            self.is_active,
            self.settling,
            self.latched.unwrap_or(0.0),
            self.fp_last.unwrap_or(0),
            self.fp_prev.unwrap_or(0),
            fmt_opt_i128(self.cold_at),
            fmt_opt_i128(self.next_deadline),
        )?;
        if let Some(fr) = self.last_frame {
            let fill = fmt::from_fn(move |f| match fr.fill {
                Some(c) => write!(f, "{c:#08x}"),
                None => f.write_str("none"),
            });
            write!(
                f,
                " frame=fill:{fill} fp:{:#x} quads:{}+{}",
                fr.fp, fr.out, fr.under
            )?;
        }
        if let (Some(moved), Some(firings)) = (self.guard_moved, self.guard_firings) {
            write!(f, " guard=moved:{moved},firings:{firings}")?;
        }
        let s = &self.stats;
        write!(
            f,
            " replays={} publications={} rejections={} delivered={} skipped={} reanchors={} breaks={} clamped={}",
            s.replays,
            s.publications,
            s.rejections,
            s.delivered,
            s.skipped,
            s.reanchors,
            s.breaks,
            self.clamped
        )
    }
}
