// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typing-momentum glow as an Aos task.

use core::fmt;
use std::time::Duration;

use aterm_aos::checkpoint::CheckpointFault;
use aterm_aos::contract::{Contract, Verdict};
use aterm_aos::effects::EffectPort;
use aterm_aos::log::LogFault;
use aterm_aos::publish::{Admit, Boundary, Commit, Head, PublishRefusal, Stats};
use aterm_aos::replay::{Run, chain_link, empty_chain};
use aterm_aos::task::{Code, EntryRefusal, PausedClass, Schema, StepRefusal, state_digest};
use aterm_aos::wire::{Dec, Enc, WireFault};
use aterm_aos::{ArtifactId, ArtifactKind, Digest, TaskIdentity};
use aterm_render::GlowQuad;
use aterm_time::Instant;

use crate::cursor_glow::Geom;
use crate::cursor_momentum::{
    AdmitRefusal, GeometryFault, MOMENTUM_GLOW_SNAPSHOT_SCHEMA, MOMENTUM_PROFILE, MomentumGlow,
    MomentumGlowConfig, MomentumGlowFrame, MomentumGlowSnapshot, SnapshotRefusal,
    admit_frame_interval, admit_tau, admit_tick, admit_time,
};

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
mod host_tests;
pub mod lane;
#[cfg(test)]
mod lane_tests;
#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
pub mod m4;
#[cfg(test)]
mod m4_tests;
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

#[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
use plan::{PlanView, SchedulePlan};
pub use trace::{
    EventCounts, MomentumTrace, ReadTrace, TRACE_FILE_DOMAIN, TRACE_FILE_MAGIC, TraceFault,
    TraceReplay,
};

/// The version of the input and observation encodings ([`MomentumSchema`]).
pub const MOMENTUM_EVENT_ENCODING: u16 = 1;

/// The task artifact's name.
pub const TASK_NAME: &str = "aterm.cursor.momentum-glow";
/// The schema artifact's name.
pub const SCHEMA_NAME: &str = "momentum-glow.state+events";
/// The contract artifact's name.
pub const CONTRACT_NAME: &str = "momentum-glow.no-stranded-light";
/// The shipped code artifact's name ([`MomentumCode::Shipped`]).
pub const SHIPPED_CODE_NAME: &str = "momentum-glow.shipped";
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

/// `t − epoch` in signed nanoseconds.
#[inline]
fn offset_of(epoch: Instant, t: Instant) -> i128 {
    if let Some(after) = t.checked_duration_since(epoch) {
        i128::try_from(after.as_nanos()).unwrap_or(i128::MAX)
    } else {
        epoch.checked_duration_since(t).map_or(i128::MIN, |before| {
            i128::try_from(before.as_nanos()).map_or(i128::MIN, |n| -n)
        })
    }
}

/// `epoch + at` with checked instant arithmetic.
#[inline]
fn instant_of(epoch: Instant, at: i128) -> Option<Instant> {
    let magnitude = at.unsigned_abs();
    let secs = u64::try_from(magnitude / 1_000_000_000).ok()?;
    let nanos = u32::try_from(magnitude % 1_000_000_000).ok()?;
    let span = Duration::new(secs, nanos);
    if at >= 0 {
        epoch.checked_add(span)
    } else {
        epoch.checked_sub(span)
    }
}

/// The task's owned state: the engine and the latest admitted input's time.
#[derive(Clone, Copy, Debug, Default)]
pub struct MomentumState {
    glow: MomentumGlow,
    clock: Option<i128>,
}

impl MomentumState {
    /// A state over `glow` that has seen no input since the epoch.
    #[inline]
    #[must_use]
    pub const fn new(glow: MomentumGlow) -> Self {
        MomentumState { glow, clock: None }
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

    fn last_time(&self, epoch: Instant) -> Option<i128> {
        let seen = self
            .glow
            .snapshot(epoch)
            .map_or([None, None], |s| [s.rate_at, s.stamp]);
        [self.clock, seen[0], seen[1]].into_iter().flatten().max()
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
    pub glyph_span: Option<(i32, i32)>,
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
            glyph_span: c.glyph_span,
        }
    }

    /// The config it records, bit for bit.
    #[inline]
    #[must_use]
    pub const fn config(&self) -> MomentumGlowConfig {
        MomentumGlowConfig {
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
            glyph_span: self.glyph_span,
        }
    }
}

/// One input to the task: one host call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MomentumEvent {
    /// `on_key(now, tau_s)`.
    Key {
        at: i128,
        /// `tau_s`, as `f32` bits.
        tau_bits: u32,
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

fn observe(
    applied: &Applied,
    epoch: Instant,
    out: &[GlowQuad],
    under: &[GlowQuad],
) -> MomentumObservation {
    match *applied {
        Applied::Keyed => MomentumObservation::Keyed,
        Applied::WasReset => MomentumObservation::WasReset,
        Applied::Frame(f) => MomentumObservation::Frame {
            fill: f.fill,
            hot: f.hot,
            fp: f.fp,
            out: out.to_vec(),
            under: under.to_vec(),
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

/// The task ABI.
#[derive(Clone, Debug)]
pub struct MomentumSchema {
    id: ArtifactId,
    epoch: Instant,
}

impl MomentumSchema {
    /// The schema over the recording epoch `epoch`.
    #[inline]
    #[must_use]
    pub fn new(epoch: Instant) -> Self {
        MomentumSchema {
            id: schema_artifact(),
            epoch,
        }
    }

    /// The recording epoch.
    #[inline]
    #[must_use]
    pub const fn epoch(&self) -> Instant {
        self.epoch
    }
}

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

fn encode_quads(e: &mut Enc, quads: &[GlowQuad]) {
    e.u64(usize_u64(quads.len()));
    for q in quads {
        e.raw(&quad_bytes(q));
    }
}

fn encode_frame(e: &mut Enc, frame: &MomentumGlowFrame, out: &[GlowQuad], under: &[GlowQuad]) {
    e.u8(2);
    e.opt(frame.fill, |e, f| e.u32(f));
    e.bool(frame.hot);
    e.u64(frame.fp);
    encode_quads(e, out);
    encode_quads(e, under);
}

const fn frame_room(quads: usize) -> usize {
    FRAME_HEAD_LEN.saturating_add(QUAD_LEN.saturating_mul(quads))
}

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

impl Schema for MomentumSchema {
    type State = MomentumState;
    type Input = MomentumEvent;
    type Observation = MomentumObservation;

    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn encode_state(&self, s: &MomentumState, e: &mut Enc) {
        match s.glow.snapshot(self.epoch) {
            Ok(snap) => e.raw(&snap.encode()),
            Err(_) => e.raw(&[0u8; MomentumGlowSnapshot::LEN]),
        }
        e.opt(s.clock, |e, c| e.offset(c));
    }

    fn decode_state(&self, d: &mut Dec<'_>) -> Result<MomentumState, WireFault> {
        let bytes = d.raw("state.snapshot", MomentumGlowSnapshot::LEN)?;
        let snap = MomentumGlowSnapshot::decode(bytes).map_err(snapshot_fault)?;
        let glow = MomentumGlow::restore(&snap, self.epoch).map_err(snapshot_fault)?;
        let clock = d.opt("state.clock", |d| d.offset("state.clock"))?;
        Ok(MomentumState { glow, clock })
    }

    fn encode_input(&self, i: &MomentumEvent, e: &mut Enc) {
        match *i {
            MomentumEvent::Key { at, tau_bits } => {
                e.u8(0);
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
            }
            MomentumEvent::Query { at, frame_interval } => {
                e.u8(3);
                e.offset(at);
                e.duration(frame_interval);
            }
        }
    }

    fn decode_input(&self, d: &mut Dec<'_>) -> Result<MomentumEvent, WireFault> {
        match d.u8("event.tag")? {
            0 => Ok(MomentumEvent::Key {
                at: d.offset("event.key.at")?,
                tau_bits: d.u32("event.key.tau")?,
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
                let cfg = ConfigRecord {
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
                    glyph_span: d.opt("cfg.glyph_span", |d| {
                        Ok((d.i32("cfg.glyph_span")?, d.i32("cfg.glyph_span")?))
                    })?,
                };
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

    fn encode_observation(&self, o: &MomentumObservation, e: &mut Enc) {
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

    fn paused_class(&self, i: &MomentumEvent) -> PausedClass {
        match i {
            MomentumEvent::Key { .. } | MomentumEvent::Reset => PausedClass::Queue,
            MomentumEvent::Tick { .. } | MomentumEvent::Query { .. } => PausedClass::Skip,
        }
    }
}

/// The target this build runs on, as `arch-os[-env]` (`std::env::consts` and `target_env`).
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
#[must_use]
pub fn task_artifact() -> ArtifactId {
    let mut e = Enc::new();
    e.str("the typing-momentum glow of one window's cursor (aterm_effects::cursor_momentum::MomentumGlow)");
    ArtifactId::new(ArtifactKind::Task, TASK_NAME, 1, e.as_bytes())
}

/// The schema artifact `momentum-glow.state+events` v1.
#[must_use]
pub fn schema_artifact() -> ArtifactId {
    let mut e = Enc::new();
    e.u16(MOMENTUM_GLOW_SNAPSHOT_SCHEMA);
    e.u16(MOMENTUM_EVENT_ENCODING);
    e.str("state: snapshot then optional clock; inputs Key|Reset|Tick|Query; observations Keyed|WasReset|Frame|Schedule|Refused");
    ArtifactId::new(ArtifactKind::Schema, SCHEMA_NAME, 1, e.as_bytes())
}

/// The profile artifact.
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
#[must_use]
pub fn momentum_identity(code: &PrebuiltMomentum) -> TaskIdentity {
    TaskIdentity {
        task: task_artifact(),
        code: code.id.clone(),
        contract: NoStrandedLight::artifact(),
        schema: schema_artifact(),
        profile: profile_artifact(),
    }
}

/// The prebuilt implementations compiled into this build (design §2 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MomentumCode {
    /// The shipped engine, called directly.
    Shipped,
    /// **An intentionally introduced demonstration fault, not a defect of shipped code**.
    #[cfg(any(test, debug_assertions))]
    DemoStrandedSettle,
}

impl MomentumCode {
    /// Every implementation in this build.
    pub const ALL: &'static [MomentumCode] = &[
        MomentumCode::Shipped,
        #[cfg(any(test, debug_assertions))]
        MomentumCode::DemoStrandedSettle,
    ];

    /// The code artifact's name.
    #[inline]
    #[must_use]
    pub const fn artifact_name(self) -> &'static str {
        match self {
            MomentumCode::Shipped => SHIPPED_CODE_NAME,
            #[cfg(any(test, debug_assertions))]
            MomentumCode::DemoStrandedSettle => DEMO_CODE_NAME,
        }
    }

    /// The short name a control verb takes (`shipped`, `demo-stranded-settle`).
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            MomentumCode::Shipped => "shipped",
            #[cfg(any(test, debug_assertions))]
            MomentumCode::DemoStrandedSettle => "demo-stranded-settle",
        }
    }

    /// The implementation a short or artifact name names.
    #[must_use]
    pub fn from_name(name: &str) -> Option<MomentumCode> {
        MomentumCode::ALL
            .iter()
            .copied()
            .find(|c| c.name() == name || c.artifact_name() == name)
    }

    /// What the artifact declares about itself.
    #[inline]
    const fn note(self) -> &'static str {
        match self {
            MomentumCode::Shipped => "the shipped engine, called directly",
            #[cfg(any(test, debug_assertions))]
            MomentumCode::DemoStrandedSettle => {
                "an intentionally introduced demonstration fault, not a defect of shipped code: \
                 next_change_deadline forgets the settle forecast (Some(now + frame_interval) \
                 while is_active, None otherwise)"
            }
        }
    }

    /// Its artifact.
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
            Sched::Plan(plan) => plan.is_active(&PlanView::of(&s.glow)),
        }
    }

    /// Hot and not active, as this code classifies the state.
    #[inline]
    #[must_use]
    pub fn settling(self, s: &MomentumState) -> bool {
        match self {
            Sched::Prebuilt(code) => code.settling(s),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(plan) => plan.settling(&PlanView::of(&s.glow)),
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
    pub fn apply(
        self,
        s: &mut MomentumState,
        epoch: Instant,
        ev: &MomentumEvent,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> Applied {
        let p = &MOMENTUM_PROFILE;
        let admit_at = |s: &MomentumState, at: i128| -> Result<Instant, AdmitRefusal> {
            admit_time(p, s.last_time(epoch), at)?;
            instant_of(epoch, at).ok_or(AdmitRefusal::Horizon(at))
        };
        match *ev {
            MomentumEvent::Key { at, tau_bits } => {
                let tau = f32::from_bits(tau_bits);
                let now = match admit_at(s, at).and_then(|now| admit_tau(p, tau).map(|()| now)) {
                    Ok(now) => now,
                    Err(r) => return Applied::Refused(r),
                };
                s.glow.on_key(now, tau);
                s.clock = Some(at);
                Applied::Keyed
            }
            MomentumEvent::Reset => {
                s.glow.reset();
                Applied::WasReset
            }
            MomentumEvent::Tick { cur, at, geom, cfg } => {
                let (geom, cfg) = (geom.geom(), cfg.config());
                let admitted = admit_at(s, at)
                    .and_then(|now| admit_tick(p, cur, &geom, &cfg).map(|budget| (now, budget)));
                let (now, budget) = match admitted {
                    Ok(x) => x,
                    Err(r) => return Applied::Refused(r),
                };
                out.reserve(budget.out);
                under.reserve(budget.under);
                let frame = s.glow.tick(cur, now, geom, &cfg, out, under);
                s.clock = Some(at);
                Applied::Frame(frame)
            }
            MomentumEvent::Query { at, frame_interval } => {
                let admitted = admit_at(s, at)
                    .and_then(|now| admit_frame_interval(p, frame_interval).map(|()| now));
                let now = match admitted {
                    Ok(now) => now,
                    Err(r) => return Applied::Refused(r),
                };
                let answer = Applied::Schedule {
                    is_active: self.is_active(s),
                    settling: self.settling(s),
                    cold_at: self.cold_at(s),
                    deadline: self.next_change_deadline(s, now, frame_interval),
                };
                s.clock = Some(at);
                answer
            }
        }
    }
}

/// A prebuilt implementation bound to its artifact (which names the build it was compiled into).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrebuiltMomentum {
    sched: Sched,
    id: ArtifactId,
}

impl PrebuiltMomentum {
    /// `code` as compiled into the build `build`.
    #[inline]
    #[must_use]
    pub fn new(code: MomentumCode, build: &str) -> Self {
        PrebuiltMomentum {
            sched: Sched::Prebuilt(code),
            id: code.artifact(build),
        }
    }

    /// The plan interpreter reading `plan` as compiled into `build` ([`SchedulePlan::artifact`]), which no verb or code name reaches.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    #[inline]
    #[must_use]
    pub(crate) fn planned(plan: SchedulePlan, build: &str) -> Self {
        PrebuiltMomentum {
            sched: Sched::Plan(plan),
            id: plan.artifact(build),
        }
    }

    /// The code `sched` names, as compiled into the build `build`.
    #[inline]
    #[must_use]
    fn of(sched: Sched, build: &str) -> Self {
        match sched {
            Sched::Prebuilt(code) => Self::new(code, build),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(plan) => Self::planned(plan, build),
        }
    }

    /// Which prebuilt implementation, or `None` for a planned code.
    #[inline]
    #[must_use]
    pub const fn code(&self) -> Option<MomentumCode> {
        self.sched.prebuilt()
    }

    /// What answers the scheduler question.
    #[inline]
    #[must_use]
    pub const fn sched(&self) -> Sched {
        self.sched
    }
}

impl Code<MomentumSchema> for PrebuiltMomentum {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn entry(&self, schema: &MomentumSchema, state: &MomentumState) -> Result<(), EntryRefusal> {
        state
            .glow
            .snapshot(schema.epoch)
            .and_then(|s| s.validate())
            .map_err(|r| EntryRefusal {
                law: SNAPSHOT_VALID,
                detail: r.to_string(),
            })
    }

    fn step(
        &self,
        schema: &MomentumSchema,
        state: &mut MomentumState,
        input: &MomentumEvent,
        _port: &mut dyn EffectPort,
    ) -> Result<MomentumObservation, StepRefusal> {
        let (mut out, mut under) = (Vec::new(), Vec::new());
        let applied = self
            .sched
            .apply(state, schema.epoch, input, &mut out, &mut under);
        Ok(observe(&applied, schema.epoch, &out, &under))
    }
}

/// `momentum-glow.no-stranded-light` v1 (design §2 item 3), judged over a replay's observations.
#[derive(Clone, Debug)]
pub struct NoStrandedLight {
    id: ArtifactId,
}

impl Default for NoStrandedLight {
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
    #[must_use]
    pub fn artifact() -> ArtifactId {
        let mut e = Enc::new();
        for law in [
            HOT_HAS_WAKE,
            WAKE_NO_LATER_THAN_CLEAR,
            COLD_TICK_CLEARS,
            RUN_MATCHES_INPUTS,
        ] {
            e.str(law);
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

impl Contract<MomentumSchema> for NoStrandedLight {
    fn id(&self) -> &ArtifactId {
        &self.id
    }

    fn judge(
        &self,
        _schema: &MomentumSchema,
        inputs: &[MomentumEvent],
        run: &Run<MomentumSchema>,
    ) -> Verdict {
        let margin = nanos(SETTLE_MARGIN);
        let mut forecast: Option<(i128, ConfigRecord)> = None;
        let mut last_tick: Option<ConfigRecord> = None;
        let mut latch_current = false;
        for (i, (cursor, obs)) in run.observations.iter().enumerate() {
            let cursor = *cursor;
            let Some(input) = inputs.get(i) else {
                return fail(
                    cursor,
                    RUN_MATCHES_INPUTS,
                    format!("observation {i} has no recorded input"),
                );
            };
            match (input, obs) {
                (_, MomentumObservation::Refused(_)) => {}
                (MomentumEvent::Key { .. }, MomentumObservation::Keyed)
                | (MomentumEvent::Reset, MomentumObservation::WasReset) => {
                    forecast = None;
                    latch_current = false;
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
                    if let Some((cold, latched)) = forecast {
                        let lit = *hot
                            || *fp != 0
                            || fill.is_some()
                            || !out.is_empty()
                            || !under.is_empty();
                        if *at >= cold.saturating_add(margin) && latched == *cfg && lit {
                            return fail(
                                cursor,
                                COLD_TICK_CLEARS,
                                format!(
                                    "the tick at {at} ns, past the cold forecast {cold} ns, still draws \
                                     (hot {hot}, fp {fp:#x}, {} + {} quads)",
                                    out.len(),
                                    under.len()
                                ),
                            );
                        }
                    }
                    if forecast.is_some_and(|(_, latched)| latched != *cfg) {
                        forecast = None;
                    }
                    last_tick = Some(*cfg);
                    latch_current = true;
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
                        return fail(
                            cursor,
                            HOT_HAS_WAKE,
                            format!(
                                "the glow is hot ({}) at {at} ns and the code names no wake",
                                if *settling { "settling" } else { "active" }
                            ),
                        );
                    }
                    if *settling && let Some(d) = *deadline {
                        let floor = at.saturating_add(nanos(*frame_interval));
                        let limit = cold_at.map_or(floor, |c| c.saturating_add(margin).max(floor));
                        if d < floor || d > limit {
                            return fail(
                                cursor,
                                WAKE_NO_LATER_THAN_CLEAR,
                                format!(
                                    "the settle wake {d} ns is outside [{floor}, {limit}] ns \
                                     (at {at} ns, cold at {cold_at:?})"
                                ),
                            );
                        }
                    }
                    forecast = if latch_current {
                        cold_at.zip(last_tick)
                    } else {
                        None
                    };
                }
                (input, obs) => {
                    return fail(
                        cursor,
                        RUN_MATCHES_INPUTS,
                        format!("{input:?} observed as {obs:?}"),
                    );
                }
            }
        }
        if let Some((cursor, r)) = &run.refused {
            return fail(
                *cursor,
                RUN_MATCHES_INPUTS,
                format!("the replay stopped: {r}"),
            );
        }
        if run.observations.len() != inputs.len() {
            let cursor = run.observations.last().map_or(0, |(c, _)| *c);
            return fail(
                cursor,
                RUN_MATCHES_INPUTS,
                format!(
                    "{} observations for {} inputs",
                    run.observations.len(),
                    inputs.len()
                ),
            );
        }
        Verdict::Pass
    }
}

/// The live observation chain since the current ancestor checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiveChain {
    /// The log epoch and first cursor it folds from.
    anchor: (u64, u64),
    chain: Digest,
    len: u64,
}

/// The last delivered tick's frame and the quads it appended.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    frame: MomentumGlowFrame,
    out: Vec<GlowQuad>,
    under: Vec<GlowQuad>,
    /// The cursor cell, geometry, page (`dark_theme`) and caret shape (`block`) of the tick.
    cur: Option<(u16, u16)>,
    geom: GeomRecord,
    dark: bool,
    block: bool,
}

impl Held {
    /// Whether a paused tick at (`cur`, `geom`, `cfg`) is shown this overlay.
    #[inline]
    fn shows(&self, cur: Option<(u16, u16)>, geom: &Geom, cfg: &MomentumGlowConfig) -> bool {
        cfg.enabled
            && cfg.intensity.is_finite()
            && cfg.intensity > 0.0
            && cur.is_some()
            && geom.cw != 0
            && geom.ch != 0
            && self.cur == cur
            && self.geom == GeomRecord::from(geom)
            && self.dark == cfg.dark_theme
            && self.block == cfg.block
    }
}

/// Whether the glow is at rest.
#[inline]
fn at_rest(s: &MomentumState, epoch: Instant) -> bool {
    s.glow.snapshot(epoch) == MomentumGlow::default().snapshot(epoch)
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

struct Hosted {
    boundary: Boundary<MomentumSchema, PrebuiltMomentum, NoStrandedLight>,
    build: String,
    chain: LiveChain,
    held: Option<Held>,
    /// The latest recorded query's time and the wake it was answered with.
    last_query: Option<(i128, Option<i128>)>,
    /// How many live inputs were recorded at the logical time instead of their own.
    clamped: u64,
    /// The latest time among the keys queued in the current pause (within the horizon).
    queued_at: Option<i128>,
    /// The one encoder every live observation is encoded into before its chain link.
    scratch: Enc,
    /// The plan-only presentation guard.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    guard: Guard,
}

fn refill(v: &mut Vec<GlowQuad>, from: &[GlowQuad]) {
    if from.is_empty() {
        *v = Vec::new();
        return;
    }
    if v.capacity() < from.len() {
        let room = from.len().max(v.capacity().saturating_mul(2));
        *v = Vec::new();
        v.reserve_exact(room);
    } else {
        v.clear();
    }
    v.extend_from_slice(from);
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

enum HostMode {
    Direct(MomentumGlow),
    Hosted(Box<Hosted>),
}

/// The momentum glow as the host holds it.
pub struct MomentumHost {
    mode: HostMode,
}

impl Default for MomentumHost {
    fn default() -> Self {
        MomentumHost {
            mode: HostMode::Direct(MomentumGlow::default()),
        }
    }
}

impl fmt::Debug for MomentumHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.mode {
            HostMode::Direct(g) => f.debug_tuple("MomentumHost::Direct").field(g).finish(),
            HostMode::Hosted(h) => f
                .debug_struct("MomentumHost::Hosted")
                .field("boundary", &h.boundary)
                .finish_non_exhaustive(),
        }
    }
}

impl Hosted {
    #[inline]
    const fn epoch(&self) -> Instant {
        self.boundary.schema().epoch
    }

    #[inline]
    const fn state(&self) -> &MomentumState {
        &self.boundary.pair().state
    }

    #[inline]
    const fn sched(&self) -> Sched {
        self.boundary.pair().code.sched
    }

    /// Frame cadence as the running host reads it: `code.is_active ∨ (hot ∧ moved)` for a plan (design §10.1), the code's own answer otherwise.
    #[inline]
    fn is_active(&self) -> bool {
        let (sched, s) = (self.sched(), self.state());
        match sched {
            Sched::Prebuilt(code) => code.is_active(s),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(_) => sched.is_active(s) || (s.glow.hot() && self.guard.moved),
        }
    }

    /// Settling as the running host reads it: `code.settling ∧ ¬moved` for a plan, the code's own answer otherwise (disjoint from [`Hosted::is_active`], their union hot).
    #[inline]
    fn settling(&self) -> bool {
        let (sched, s) = (self.sched(), self.state());
        match sched {
            Sched::Prebuilt(code) => code.settling(s),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            Sched::Plan(_) => sched.settling(s) && !self.guard.moved,
        }
    }

    /// Keeps a delivered tick's presentation and whether it moved, counting a firing when that holds the cadence a live plan released.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    #[inline]
    fn guard_tick(&mut self, cur: Option<(u16, u16)>, geom: GeomRecord, cfg: ConfigRecord) {
        let shown = Some((cur, geom, cfg));
        self.guard.moved = self.guard.last != shown;
        self.guard.last = shown;
        let (sched, s) = (self.sched(), self.state());
        if let Sched::Plan(_) = sched
            && self.guard.moved
            && s.glow.hot()
            && !sched.is_active(s)
        {
            self.guard.firings = self.guard.firings.saturating_add(1);
        }
    }

    #[inline]
    const fn cursor(&self) -> u64 {
        self.boundary.pair().head.input_cursor
    }

    /// The logical time of a live call the host stamped `now`.
    #[inline]
    fn logical(&self, now: Instant) -> (i128, bool) {
        let at = offset_of(self.epoch(), now);
        let mut floor = self.state().last_time(self.epoch()).unwrap_or(0).max(0);
        if self.boundary.is_paused()
            && let Some(queued) = self.queued_at
        {
            floor = floor.max(queued);
        }
        if at < floor {
            (floor, true)
        } else {
            (at, false)
        }
    }

    fn fold(&mut self, cursor: u64, obs: &MomentumObservation) {
        self.link(cursor, |schema, e| schema.encode_observation(obs, e));
    }

    fn link(&mut self, cursor: u64, encode: impl FnOnce(&MomentumSchema, &mut Enc)) {
        let Some(log) = self.boundary.log() else {
            return;
        };
        let anchor = (log.header().log_epoch, log.header().first_cursor);
        if self.chain.anchor != anchor {
            self.chain = LiveChain {
                anchor,
                chain: empty_chain(),
                len: 0,
            };
        }
        if cursor < anchor.1 {
            return;
        }
        self.scratch.clear();
        encode(self.boundary.schema(), &mut self.scratch);
        self.chain.chain = chain_link(&self.chain.chain, cursor, self.scratch.as_bytes());
        self.chain.len = self.chain.len.saturating_add(1);
    }

    fn live_chain(&self) -> Digest {
        match self.boundary.log() {
            Some(log)
                if self.chain.anchor == (log.header().log_epoch, log.header().first_cursor) =>
            {
                self.chain.chain
            }
            _ => empty_chain(),
        }
    }

    fn fold_delivered(
        &mut self,
        first: u64,
        delivered: &[Result<MomentumObservation, StepRefusal>],
    ) {
        let mut cursor = first;
        for obs in delivered.iter().flatten() {
            self.fold(cursor, obs);
            cursor = cursor.saturating_add(1);
        }
    }

    fn deliver(
        &mut self,
        ev: &MomentumEvent,
        moved: bool,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> Option<Applied> {
        let first = self.cursor();
        let admitted = self.boundary.admit(ev);
        if moved && admitted != Admit::Skipped {
            self.clamped = self.clamped.saturating_add(1);
        }
        match admitted {
            Admit::Run => {}
            Admit::Queued => {
                // A queued time past the horizon is refused on delivery; it raises no floor.
                if let Some(at) = time_of(ev).filter(|&at| at <= horizon_ns()) {
                    self.queued_at = Some(self.queued_at.map_or(at, |q| q.max(at)));
                }
                return None;
            }
            Admit::Skipped => return None,
            Admit::Rejected { .. } => {
                let delivered = self.boundary.take_delivered();
                self.fold_delivered(first, &delivered);
                return None;
            }
        }
        let (epoch, sched) = (self.epoch(), self.sched());
        let (out_from, under_from) = (out.len(), under.len());
        let (_, state) = self.boundary.pair_mut();
        let applied = sched.apply(state, epoch, ev, out, under);
        self.boundary.advance();
        #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
        if let MomentumEvent::Tick { cur, geom, cfg, .. } = *ev {
            self.guard_tick(cur, geom, cfg);
        }
        let drawn = out.get(out_from..).unwrap_or(&[]);
        let drawn_under = under.get(under_from..).unwrap_or(&[]);
        if let (MomentumEvent::Tick { cur, geom, cfg, .. }, Applied::Frame(frame)) = (ev, applied) {
            self.link(first, |_, e| {
                e.reserve(frame_room(drawn.len().saturating_add(drawn_under.len())));
                encode_frame(e, &frame, drawn, drawn_under);
            });
            let (mut held_out, mut held_under) = self
                .held
                .take()
                .map(|h| (h.out, h.under))
                .unwrap_or_default();
            refill(&mut held_out, drawn);
            refill(&mut held_under, drawn_under);
            if drawn.is_empty() && drawn_under.is_empty() {
                self.scratch = Enc::new();
            }
            self.held = Some(Held {
                frame,
                out: held_out,
                under: held_under,
                cur: *cur,
                geom: *geom,
                dark: cfg.dark_theme,
                block: cfg.block,
            });
            return Some(applied);
        }
        let obs = observe(&applied, epoch, drawn, drawn_under);
        self.fold(first, &obs);
        match (ev, obs) {
            (MomentumEvent::Tick { cur, geom, cfg, .. }, _) => {
                self.scratch = Enc::new();
                self.held = Some(Held {
                    frame: MomentumGlowFrame::default(),
                    out: Vec::new(),
                    under: Vec::new(),
                    cur: *cur,
                    geom: *geom,
                    dark: cfg.dark_theme,
                    block: cfg.block,
                });
            }
            (MomentumEvent::Query { at, .. }, MomentumObservation::Schedule { deadline, .. }) => {
                self.last_query = Some((*at, deadline));
            }
            (MomentumEvent::Query { at, .. }, _) => self.last_query = Some((*at, None)),
            _ => {}
        }
        Some(applied)
    }

    fn reset_changes_nothing(&self) -> bool {
        if self.boundary.is_paused() {
            match self.boundary.last_queued() {
                Some(MomentumEvent::Reset) => true,
                Some(_) => false,
                None => at_rest(self.state(), self.epoch()),
            }
        } else {
            at_rest(self.state(), self.epoch())
        }
    }

    fn next_change_deadline(&mut self, now: Instant, frame_interval: Duration) -> Option<Instant> {
        let (at, moved) = self.logical(now);
        let ev = MomentumEvent::Query { at, frame_interval };
        match self.deliver(&ev, moved, &mut Vec::new(), &mut Vec::new())? {
            Applied::Schedule { deadline, .. } => deadline,
            _ => {
                let (sched, s) = (self.sched(), self.state());
                (sched.is_active(s) || sched.settling(s))
                    .then(|| now.checked_add(frame_interval))
                    .flatten()
            }
        }
    }
}

impl MomentumHost {
    /// A running attach whose live time at `now` is past the hosted horizon can admit no timed
    /// input again (logical time never goes back, and every such input is refused `Horizon`).
    #[inline]
    fn expire_past_horizon(&mut self, now: Instant) {
        if let HostMode::Hosted(h) = &self.mode
            && !h.boundary.is_paused()
            && h.logical(now).0 > horizon_ns()
        {
            self.aos_detach();
        }
    }

    /// One printable key (`MomentumGlow::on_key`).
    #[inline]
    pub fn on_key(&mut self, now: Instant, tau_s: f32) {
        self.expire_past_horizon(now);
        match &mut self.mode {
            HostMode::Direct(g) => g.on_key(now, tau_s),
            HostMode::Hosted(h) => {
                let (at, moved) = h.logical(now);
                let ev = MomentumEvent::Key {
                    at,
                    tau_bits: tau_s.to_bits(),
                };
                let _ = h.deliver(&ev, moved, &mut Vec::new(), &mut Vec::new());
            }
        }
    }

    /// Back to rest (`MomentumGlow::reset`).
    #[inline]
    pub fn reset(&mut self) {
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

    /// One frame (`MomentumGlow::tick`).
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
                    None => match &h.held {
                        Some(held) if held.shows(cur, &geom, cfg) => {
                            out.extend_from_slice(&held.out);
                            under.extend_from_slice(&held.under);
                            held.frame
                        }
                        _ => MomentumGlowFrame::default(),
                    },
                }
            }
        }
    }

    /// Frame cadence (`MomentumGlow::is_active`, or a plan's under its presentation guard); `false` while paused.
    #[inline]
    #[must_use]
    pub fn is_active(&self) -> bool {
        match &self.mode {
            HostMode::Direct(g) => g.is_active(),
            HostMode::Hosted(h) => !h.boundary.is_paused() && h.is_active(),
        }
    }

    /// Hot with no frame-to-frame change (`MomentumGlow::settling`); `false` while paused.
    #[inline]
    #[must_use]
    pub fn settling(&self) -> bool {
        match &self.mode {
            HostMode::Direct(g) => g.settling(),
            HostMode::Hosted(h) => !h.boundary.is_paused() && h.settling(),
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

    /// The wake the live code names (`MomentumGlow::next_change_deadline`).
    #[inline]
    pub fn next_change_deadline(
        &mut self,
        now: Instant,
        frame_interval: Duration,
    ) -> Option<Instant> {
        self.expire_past_horizon(now);
        match &mut self.mode {
            HostMode::Direct(g) => g.next_change_deadline(now, frame_interval),
            HostMode::Hosted(h) => h.next_change_deadline(now, frame_interval),
        }
    }

    /// Starts hosting.
    pub fn aos_attach(&mut self, epoch: Instant, code: MomentumCode, build: &str, log_epoch: u64) {
        self.attach_with(
            epoch,
            code,
            build,
            log_epoch,
            MOMENTUM_QUEUE_CAP,
            MOMENTUM_HOST_MAX_EVENTS,
        );
    }

    fn attach_with(
        &mut self,
        epoch: Instant,
        code: MomentumCode,
        build: &str,
        log_epoch: u64,
        queue_cap: usize,
        max_events: usize,
    ) {
        self.attach_sched(
            epoch,
            Sched::Prebuilt(code),
            build,
            log_epoch,
            queue_cap,
            max_events,
        );
    }

    /// Attaches as [`Self::aos_attach`] does with `plan` as the live code ([`PrebuiltMomentum::planned`]) and log epochs of `max_events` inputs, for the candidate evaluator.
    #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
    pub(crate) fn attach_planned(
        &mut self,
        epoch: Instant,
        plan: SchedulePlan,
        build: &str,
        log_epoch: u64,
        max_events: usize,
    ) {
        self.attach_sched(
            epoch,
            Sched::Plan(plan),
            build,
            log_epoch,
            MOMENTUM_QUEUE_CAP,
            max_events,
        );
    }

    fn attach_sched(
        &mut self,
        epoch: Instant,
        sched: Sched,
        build: &str,
        log_epoch: u64,
        queue_cap: usize,
        max_events: usize,
    ) {
        if let HostMode::Hosted(h) = &mut self.mode
            && h.epoch() == epoch
            && h.sched() == sched
            && h.build == build
        {
            let first = h.cursor();
            if h.boundary.attach(log_epoch, max_events).is_ok() {
                let delivered = h.boundary.take_delivered();
                h.fold_delivered(first, &delivered);
            }
            return;
        }
        let glow = self.take_glow();
        let prebuilt = PrebuiltMomentum::of(sched, build);
        let identity = momentum_identity(&prebuilt);
        let mut boundary = Boundary::new(
            MomentumSchema::new(epoch),
            identity,
            prebuilt,
            NoStrandedLight::new(),
            MomentumState::new(glow),
            queue_cap,
        );
        if boundary.attach(log_epoch, max_events).is_err() {
            self.mode = HostMode::Direct(boundary.pair().state.glow);
            return;
        }
        let chain = LiveChain {
            anchor: boundary.log().map_or((log_epoch, 0), |l| {
                (l.header().log_epoch, l.header().first_cursor)
            }),
            chain: empty_chain(),
            len: 0,
        };
        self.mode = HostMode::Hosted(Box::new(Hosted {
            boundary,
            build: build.to_owned(),
            chain,
            held: None,
            last_query: None,
            clamped: 0,
            queued_at: None,
            scratch: Enc::new(),
            #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
            guard: Guard::default(),
        }));
    }

    fn take_glow(&mut self) -> MomentumGlow {
        match core::mem::replace(&mut self.mode, HostMode::Direct(MomentumGlow::default())) {
            HostMode::Direct(g) => g,
            HostMode::Hosted(mut h) => {
                h.boundary.detach();
                h.boundary.pair().state.glow
            }
        }
    }

    /// Stops hosting.
    pub fn aos_detach(&mut self) {
        let glow = self.take_glow();
        self.mode = HostMode::Direct(glow);
    }

    /// Pauses at this step boundary.
    /// # Errors
    /// [`PublishRefusal::NotAttached`] or [`PublishRefusal::AlreadyPaused`]; nothing is recorded
    /// then.
    pub fn aos_pause(
        &mut self,
        now: Instant,
        frame_interval: Duration,
    ) -> Result<Head, PublishRefusal> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Err(PublishRefusal::NotAttached);
        };
        if h.boundary.is_paused() {
            return Err(PublishRefusal::AlreadyPaused);
        }
        let _ = h.next_change_deadline(now, frame_interval);
        h.queued_at = None;
        h.boundary.pause()
    }

    /// Prepares `candidate` against the paused head (shadow replay of the live code and the
    /// candidate from the ancestor checkpoint, judged by [`NoStrandedLight`]) and commits it.
    /// # Errors
    /// Any [`PublishRefusal`] `prepare` or `commit` returns (`NotAttached`, `NotPaused`,
    /// `CandidateFailed`, …).
    pub fn aos_repair(
        &mut self,
        candidate: MomentumCode,
    ) -> Result<Commit<MomentumSchema>, PublishRefusal> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Err(PublishRefusal::NotAttached);
        };
        let prebuilt = PrebuiltMomentum::new(candidate, &h.build);
        let prepared = h.boundary.prepare(prebuilt)?;
        let first = h.cursor();
        let commit = h.boundary.commit(prepared)?;
        h.fold_delivered(first, &commit.delivered);
        Ok(commit)
    }

    /// Rejects the pending update.
    pub fn aos_resume(&mut self) -> Vec<Result<MomentumObservation, StepRefusal>> {
        let HostMode::Hosted(h) = &mut self.mode else {
            return Vec::new();
        };
        let first = h.cursor();
        let delivered = h.boundary.reject();
        h.fold_delivered(first, &delivered);
        delivered
    }

    /// Whether the glow is attached.
    #[inline]
    #[must_use]
    pub const fn aos_attached(&self) -> bool {
        matches!(self.mode, HostMode::Hosted(_))
    }

    /// Whether the attached glow is paused.
    #[inline]
    #[must_use]
    pub const fn aos_paused(&self) -> bool {
        matches!(&self.mode, HostMode::Hosted(h) if h.boundary.is_paused())
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
    #[must_use]
    pub fn aos_status(&self) -> MomentumAosStatus {
        let h = match &self.mode {
            HostMode::Direct(g) => {
                return MomentumAosStatus {
                    code: MomentumCode::Shipped.name(),
                    hot: g.is_active() || g.settling(),
                    is_active: g.is_active(),
                    settling: g.settling(),
                    ..MomentumAosStatus::default()
                };
            }
            HostMode::Hosted(h) => h,
        };
        let b = &h.boundary;
        let (code, state, epoch) = (h.sched(), h.state(), h.epoch());
        let snapshot = state.glow.snapshot(epoch).ok();
        let log = b.log();
        // The host's reading: a plan's answer under the presentation guard, as the lane runs it.
        let (is_active, settling) = (h.is_active(), h.settling());
        #[cfg(any(test, debug_assertions, feature = "aos-candidates"))]
        let (guard_moved, guard_firings) = match code {
            Sched::Plan(_) => (Some(h.guard.moved), Some(h.guard.firings)),
            Sched::Prebuilt(_) => (None, None),
        };
        #[cfg(not(any(test, debug_assertions, feature = "aos-candidates")))]
        let (guard_moved, guard_firings) = (None, None);
        MomentumAosStatus {
            attached: true,
            code: code.name(),
            code_digest: Some(b.pair().code.id.digest.short()),
            build: Some(h.build.clone()),
            revision: b.pair().head.revision.0,
            branch: b.pair().head.branch.0,
            paused: b.is_paused(),
            queued: b.queued(),
            queue_cap: b.queue_cap(),
            log_epoch: log.map_or(0, |l| l.header().log_epoch),
            log_len: log.map_or(0, |l| l.len()),
            input_cursor: b.pair().head.input_cursor,
            broken: log.and_then(|l| l.broken()),
            checkpoint: b.checkpoint().map(|c| c.digest().short()),
            state_digest: Some(state_digest(b.schema(), state).short()),
            observation_chain: Some(h.live_chain().short()),
            latched: snapshot.map(|s| f32::from_bits(s.latched)),
            fp_last: snapshot.map(|s| s.fp_last),
            fp_prev: snapshot.map(|s| s.fp_prev),
            clock: state.clock,
            hot: is_active || settling,
            is_active,
            settling,
            guard_moved,
            guard_firings,
            cold_at: code.cold_at(state).map(|t| offset_of(epoch, t)),
            last_query_at: h.last_query.map(|(at, _)| at),
            next_deadline: h.last_query.and_then(|(_, d)| d),
            last_frame: h.held.as_ref().map(|held| FrameSummary {
                fill: held.frame.fill,
                hot: held.frame.hot,
                fp: held.frame.fp,
                out: held.out.len(),
                under: held.under.len(),
            }),
            stats: b.stats(),
            clamped: h.clamped,
        }
    }

    /// The ancestor checkpoint and the log since it.
    /// # Errors
    /// [`TraceRefusal::NotAttached`], or an encoder's `TooLarge`.
    pub fn aos_trace(&self) -> Result<MomentumTrace, TraceRefusal> {
        let HostMode::Hosted(h) = &self.mode else {
            return Err(TraceRefusal::NotAttached);
        };
        let b = &h.boundary;
        let (Some(checkpoint), Some(log)) = (b.checkpoint(), b.log()) else {
            return Err(TraceRefusal::NotAttached);
        };
        Ok(MomentumTrace {
            identity: b.identity().clone(),
            checkpoint: checkpoint.encode().map_err(TraceRefusal::Checkpoint)?,
            log: log.encode(b.schema()).map_err(TraceRefusal::Log)?,
            head_state: state_digest(b.schema(), h.state()),
            head_chain: h.live_chain(),
        })
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
        let opt = |v: Option<i128>| v.map_or_else(|| "none".to_owned(), |t| t.to_string());
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
            opt(self.cold_at),
            opt(self.next_deadline),
        )?;
        if let Some(fr) = self.last_frame {
            write!(
                f,
                " frame=fill:{} fp:{:#x} quads:{}+{}",
                fr.fill
                    .map_or_else(|| "none".to_owned(), |c| format!("{c:#08x}")),
                fr.fp,
                fr.out,
                fr.under
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

/// Why [`MomentumHost::aos_trace`] exported nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceRefusal {
    /// The glow is not attached: there is no checkpoint or log.
    NotAttached,
    /// The checkpoint does not encode.
    Checkpoint(CheckpointFault),
    /// The log does not encode.
    Log(LogFault),
}

impl fmt::Display for TraceRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceRefusal::NotAttached => {
                f.write_str("the momentum glow is not attached: there is no trace")
            }
            TraceRefusal::Checkpoint(c) => {
                write!(f, "the ancestor checkpoint does not encode: {c}")
            }
            TraceRefusal::Log(l) => write!(f, "the log does not encode: {l}"),
        }
    }
}

impl std::error::Error for TraceRefusal {}
