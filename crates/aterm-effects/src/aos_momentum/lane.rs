// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's closed-loop frame lane.

use core::fmt;
use std::time::Duration;

use aterm_aos::Digest;
use aterm_aos::wire::Enc;
use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::{
    ConfigRecord, GeomRecord, MOMENTUM_MAX_EVENTS, MomentumEvent, MomentumHost, encode_frame,
};
use crate::cursor_glow::Geom;
use crate::cursor_momentum::{
    AdmitRefusal, MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MOMENTUM_PROFILE, MomentumGlow,
    MomentumGlowConfig, MomentumGlowFrame, admit_frame_interval, admit_tau, admit_tick, admit_time,
};

/// The most calls one run makes (keys, resets, ticks and queries).
pub const LANE_MAX_EVENTS: usize = MOMENTUM_MAX_EVENTS;

/// A hang detector, not a budget: no tick comes a minute of story time after the latest input.
pub const HANG_NS: u64 = 60_000_000_000;

/// The format byte a scenario's encoding opens with.
pub const SCENARIO_FORMAT: u8 = 1;

/// The domain under which [`Scenario::digest`] digests a scenario's encoding.
pub const SCENARIO_DOMAIN: &[u8] = b"aterm-aos-m4-scenario-v1\0";

/// The lane model, as the contracts and the policy that bind it name it.
pub const LANE_MODEL: &str = "aterm frame lane v1: a key ticks at the next frame boundary; \
     after a tick, active ticks at the next frame boundary, settling queries the wake (strictly \
     after the tick) and ticks then, anything else parks; inputs before a tick at one instant; \
     a run ends at its scenario's end";

const RESET_TAG: u64 = 0x5245_5345_5400_0000;

const QUADS: usize = 512;

/// The instant `offset` ns after `origin`.
#[inline]
fn at(origin: Instant, offset: u64) -> Instant {
    origin + Duration::from_nanos(offset)
}

/// The offset of `t` from `origin`, in ns (0 before it).
#[inline]
fn offset(origin: Instant, t: Instant) -> u64 {
    u64::try_from(t.saturating_duration_since(origin).as_nanos()).unwrap_or(u64::MAX)
}

/// The first frame boundary (a multiple of `f` ns) after `t`.
#[inline]
const fn vsync_after(t: u64, f: u64) -> u64 {
    match t.checked_div(f) {
        Some(n) => n.saturating_add(1).saturating_mul(f),
        None => t.saturating_add(1),
    }
}

#[inline]
const fn count(n: usize) -> u64 {
    n as u64
}

/// What one tick presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Presentation {
    pub cur: Option<(u16, u16)>,
    pub geom: GeomRecord,
    pub cfg: ConfigRecord,
}

impl Presentation {
    /// `(cur, geom, cfg)` as records.
    #[inline]
    #[must_use]
    pub const fn of(
        cur: Option<(u16, u16)>,
        geom: &Geom,
        cfg: &MomentumGlowConfig,
    ) -> Presentation {
        Presentation {
            cur,
            geom: GeomRecord::from_geom(geom),
            cfg: ConfigRecord::from_config(cfg),
        }
    }

    /// Its canonical encoding.
    pub fn encode(&self, e: &mut Enc) {
        let (g, c) = (&self.geom, &self.cfg);
        e.opt(self.cur, |e, (row, col)| {
            e.u16(row);
            e.u16(col);
        });
        for v in [g.cw, g.ch, g.rows, g.cols] {
            e.u64(count(v));
        }
        for v in [g.origin_x, g.origin_y, g.win_w, g.win_h, g.head] {
            e.u16(v);
        }
        e.bool(c.enabled);
        e.u32(c.intensity);
        e.u32(c.tau_s);
        e.u32(c.radius_cells);
        e.u32(c.base);
        e.bool(c.dark_theme);
        e.u32(c.hue);
        e.u32(c.theme_fg);
        e.u32(c.theme_bg);
        e.bool(c.block);
        e.opt(c.glyph_span, |e, (first, end)| {
            e.i32(first);
            e.i32(end);
        });
    }

    const fn admit(&self) -> Result<(), AdmitRefusal> {
        match admit_tick(
            &MOMENTUM_PROFILE,
            self.cur,
            &self.geom.geom(),
            &self.cfg.config(),
        ) {
            Ok(_) => Ok(()),
            Err(r) => Err(r),
        }
    }
}

/// One exogenous input of a scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Exo {
    /// A printable key, with its τ (`f32` bits).
    Key { tau_bits: u32 },
    /// The glow back to rest.
    Reset,
    /// From here on, ticks present this.
    Present(Presentation),
}

/// An absolute, exogenous timeline that nothing in the code under test can move.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Scenario {
    frame_interval_ns: u64,
    start: Presentation,
    events: Vec<(u64, Exo)>,
    end_ns: u64,
}

/// Why a timeline is not a scenario.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScenarioFault {
    /// A frame interval of zero or past the profile's limit.
    FrameInterval(Duration),
    /// Admission refuses an input (`index` into the inputs) or, with `None`, the start
    /// presentation or the end time.
    Refused {
        index: Option<usize>,
        refusal: AdmitRefusal,
    },
    /// An input earlier than the one before it.
    Order { index: usize, at: u64, before: u64 },
    /// An input after the end.
    PastEnd { index: usize, at: u64, end: u64 },
    /// More inputs than a run may make calls.
    TooMany { len: usize },
}

impl fmt::Display for ScenarioFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ScenarioFault::FrameInterval(d) => {
                write!(f, "scenario refused: frame interval {d:?}")
            }
            ScenarioFault::Refused {
                index: Some(i),
                refusal,
            } => write!(f, "scenario refused: input {i}: {refusal}"),
            ScenarioFault::Refused {
                index: None,
                refusal,
            } => write!(f, "scenario refused: {refusal}"),
            ScenarioFault::Order { index, at, before } => write!(
                f,
                "scenario refused: input {index} at {at} ns comes before the previous one's {before} ns"
            ),
            ScenarioFault::PastEnd { index, at, end } => write!(
                f,
                "scenario refused: input {index} at {at} ns is after the end {end} ns"
            ),
            ScenarioFault::TooMany { len } => write!(
                f,
                "scenario refused: {len} inputs, more than a run's {LANE_MAX_EVENTS} calls"
            ),
        }
    }
}

impl std::error::Error for ScenarioFault {}

impl Scenario {
    /// The scenario.
    /// # Errors
    /// The first [`ScenarioFault`], checking the frame interval, the start, the end, the input
    /// count, then each input in order.
    pub fn new(
        frame_interval_ns: u64,
        start: Presentation,
        events: Vec<(u64, Exo)>,
        end_ns: u64,
    ) -> Result<Scenario, ScenarioFault> {
        let s = Scenario {
            frame_interval_ns,
            start,
            events,
            end_ns,
        };
        s.validate()?;
        Ok(s)
    }

    /// The checks [`Self::new`] makes.
    /// # Errors
    /// As [`Self::new`].
    pub fn validate(&self) -> Result<(), ScenarioFault> {
        let p = &MOMENTUM_PROFILE;
        let f = self.frame_interval();
        if self.frame_interval_ns == 0 || admit_frame_interval(p, f).is_err() {
            return Err(ScenarioFault::FrameInterval(f));
        }
        let refused = |index, refusal| ScenarioFault::Refused { index, refusal };
        self.start.admit().map_err(|r| refused(None, r))?;
        admit_time(p, None, i128::from(self.end_ns)).map_err(|r| refused(None, r))?;
        if self.events.len() > LANE_MAX_EVENTS {
            return Err(ScenarioFault::TooMany {
                len: self.events.len(),
            });
        }
        let mut before = 0u64;
        for (index, &(at, exo)) in self.events.iter().enumerate() {
            if at < before {
                return Err(ScenarioFault::Order { index, at, before });
            }
            if at > self.end_ns {
                return Err(ScenarioFault::PastEnd {
                    index,
                    at,
                    end: self.end_ns,
                });
            }
            before = at;
            match exo {
                Exo::Key { tau_bits } => {
                    admit_tau(p, f32::from_bits(tau_bits)).map_err(|r| refused(Some(index), r))?;
                }
                Exo::Reset => {}
                Exo::Present(pr) => pr.admit().map_err(|r| refused(Some(index), r))?,
            }
        }
        Ok(())
    }

    /// The frame interval, in ns.
    #[inline]
    #[must_use]
    pub const fn frame_interval_ns(&self) -> u64 {
        self.frame_interval_ns
    }

    /// The frame interval.
    #[inline]
    #[must_use]
    pub const fn frame_interval(&self) -> Duration {
        Duration::from_nanos(self.frame_interval_ns)
    }

    /// The presentation before any input.
    #[inline]
    #[must_use]
    pub const fn start(&self) -> &Presentation {
        &self.start
    }

    /// The inputs at their offsets.
    #[inline]
    #[must_use]
    pub const fn events(&self) -> &[(u64, Exo)] {
        self.events.as_slice()
    }

    /// The end, in ns from the origin.
    #[inline]
    #[must_use]
    pub const fn end_ns(&self) -> u64 {
        self.end_ns
    }

    /// How many keys it types.
    #[must_use]
    pub const fn keys(&self) -> usize {
        let events = self.events.as_slice();
        let mut n = 0usize;
        let mut i = 0;
        while i < events.len() {
            if matches!(events[i].1, Exo::Key { .. }) {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// Whether every presentation change comes at the instant of a key.
    #[must_use]
    pub const fn presentation_moves_only_at_keys(&self) -> bool {
        let events = self.events.as_slice();
        let mut i = 0;
        while i < events.len() {
            let (at, e) = events[i];
            if matches!(e, Exo::Present(_)) {
                let mut found = false;
                let mut j = 0;
                while j < events.len() {
                    let (k, ke) = events[j];
                    if k == at && matches!(ke, Exo::Key { .. }) {
                        found = true;
                        break;
                    }
                    j += 1;
                }
                if !found {
                    return false;
                }
            }
            i += 1;
        }
        true
    }

    /// The canonical encoding.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new();
        e.u8(SCENARIO_FORMAT);
        e.u64(self.frame_interval_ns);
        e.u64(self.end_ns);
        self.start.encode(&mut e);
        e.u64(count(self.events.len()));
        for &(at, exo) in &self.events {
            e.u64(at);
            match exo {
                Exo::Key { tau_bits } => {
                    e.u8(0);
                    e.u32(tau_bits);
                }
                Exo::Reset => e.u8(1),
                Exo::Present(p) => {
                    e.u8(2);
                    p.encode(&mut e);
                }
            }
        }
        e.finish()
    }

    /// `Digest::of(SCENARIO_DOMAIN, [the encoding])`.
    #[must_use]
    pub fn digest(&self) -> Digest {
        Digest::of(SCENARIO_DOMAIN, &[&self.encode()])
    }
}

/// The calls the host's lane makes on the momentum glow.
pub trait Glow {
    fn key(&mut self, now: Instant, tau: f32);
    fn rest(&mut self);
    fn frame(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame;
    fn active(&self) -> bool;
    fn settles(&self) -> bool;
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant>;
}

impl Glow for MomentumGlow {
    #[inline]
    fn key(&mut self, now: Instant, tau: f32) {
        self.on_key(now, tau);
    }

    #[inline]
    fn rest(&mut self) {
        self.reset();
    }

    #[inline]
    fn frame(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame {
        self.tick(cur, now, geom, cfg, out, under)
    }

    #[inline]
    fn active(&self) -> bool {
        self.is_active()
    }

    #[inline]
    fn settles(&self) -> bool {
        self.settling()
    }

    #[inline]
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant> {
        self.next_change_deadline(now, lane)
    }
}

impl Glow for MomentumHost {
    #[inline]
    fn key(&mut self, now: Instant, tau: f32) {
        self.on_key(now, tau);
    }

    #[inline]
    fn rest(&mut self) {
        self.reset();
    }

    #[inline]
    fn frame(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        geom: Geom,
        cfg: &MomentumGlowConfig,
        out: &mut Vec<GlowQuad>,
        under: &mut Vec<GlowQuad>,
    ) -> MomentumGlowFrame {
        self.tick(cur, now, geom, cfg, out, under)
    }

    #[inline]
    fn active(&self) -> bool {
        self.is_active()
    }

    #[inline]
    fn settles(&self) -> bool {
        self.settling()
    }

    #[inline]
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant> {
        self.next_change_deadline(now, lane)
    }
}

/// FNV-1a over everything a path shows the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv {
    /// The FNV-1a offset basis.
    #[inline]
    #[must_use]
    pub const fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    /// The digest so far.
    #[inline]
    #[must_use]
    pub const fn value(&self) -> u64 {
        self.0
    }

    /// Folds `v`'s eight little-endian bytes.
    #[inline]
    pub const fn u64(&mut self, v: u64) {
        let bytes = v.to_le_bytes();
        let mut i = 0;
        while i < 8 {
            self.0 ^= bytes[i] as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
    }

    /// Folds a quad stream: its length, then three words per quad.
    pub const fn quads(&mut self, qs: &[GlowQuad]) {
        self.u64(count(qs.len()));
        let mut i = 0;
        while i < qs.len() {
            let q = qs[i];
            self.u64(
                (q.row as u64) | ((q.x as u64) << 16) | ((q.y as u64) << 32) | ((q.w as u64) << 48),
            );
            self.u64((q.h as u64) | ((q.color as u64) << 16) | ((q.alpha as u64) << 48));
            self.u64((q.color2 as u64) | ((q.alpha2 as u64) << 32));
            i += 1;
        }
    }

    /// Folds a frame and the quads it appended.
    pub const fn frame(&mut self, f: MomentumGlowFrame, out: &[GlowQuad], under: &[GlowQuad]) {
        self.u64(match f.fill {
            Some(v) => v as u64,
            None => u64::MAX,
        });
        self.u64(f.hot as u64);
        self.u64(f.fp);
        self.quads(out);
        self.quads(under);
    }

    /// Folds a wake (its offset from the origin, or none).
    #[inline]
    pub const fn wake(&mut self, at: Option<u64>) {
        self.u64(match at {
            Some(v) => v,
            None => u64::MAX,
        });
    }

    /// Folds a reset.
    #[inline]
    pub const fn reset(&mut self) {
        self.u64(RESET_TAG);
    }
}

/// FNV-1a 64 over `bytes`: a frame's key in [`LaneTick::key`].
#[inline]
const fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h
}

/// The canonical encoding of a frame and the quads it appended.
pub fn encode_glass(e: &mut Enc, frame: &MomentumGlowFrame, out: &[GlowQuad], under: &[GlowQuad]) {
    encode_frame(e, frame, out, under);
}

/// [`LaneTick::key`] of an encoded frame.
#[inline]
#[must_use]
pub const fn glass_key(encoded: &[u8]) -> u64 {
    fnv1a(encoded)
}

/// What the lane did after a tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum After {
    /// The glow read active: the next tick at the next frame boundary, `at`.
    Cadence { at: u64 },
    /// The glow read settling.
    Wake { query: usize, wake: Option<u64> },
    /// Neither: the lane parked.
    Park,
}

/// One tick of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaneTick {
    /// Its time, ns from the origin.
    pub at: u64,
    /// Its index in the run's calls.
    pub call: usize,
    /// The frame it returned.
    pub frame: MomentumGlowFrame,
    /// The quads it appended to each stream.
    pub out: usize,
    pub under: usize,
    /// FNV-1a 64 over the frame's canonical encoding ([`encode_glass`]).
    pub key: u64,
    /// Its frame differs from the one already on glass (before the first tick, the inert frame).
    pub presented: bool,
    /// What the lane did after it.
    pub after: After,
}

/// What a run counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaneCounts {
    pub keys: u64,
    pub resets: u64,
    /// `T`: ticks rendered.
    pub ticks: u64,
    /// `P`: ticks whose frame differs from the frame already on glass.
    pub presented: u64,
    pub queries: u64,
    /// The lane parked with the glow cold.
    pub parks: u64,
    /// The lane parked with the glow still settling: light stranded on glass.
    pub stranded: u64,
    /// The quads the ticks appended, both streams, and the most one tick appended.
    pub quads: u64,
    pub max_quads: u64,
}

impl LaneCounts {
    /// `Z = T − P`: ticks that showed nothing new.
    #[inline]
    #[must_use]
    pub const fn wasted(&self) -> u64 {
        self.ticks.saturating_sub(self.presented)
    }

    /// Every call: keys, resets, ticks and queries.
    #[inline]
    #[must_use]
    pub const fn calls(&self) -> u64 {
        self.keys
            .saturating_add(self.resets)
            .saturating_add(self.ticks)
            .saturating_add(self.queries)
    }
}

/// Why a run stopped before its scenario's end. `ticks` is how many ticks it had recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneFault {
    /// A settle wake at or before the tick it was asked at: a busy re-arm.
    WakeNotAfter { ticks: usize, at: u64, wake: u64 },
    /// The next call would pass [`LANE_MAX_EVENTS`].
    TooManyEvents { ticks: usize, at: u64 },
    /// A tick due more than [`HANG_NS`] after the latest input (`since`).
    Hang { ticks: usize, at: u64, since: u64 },
}

impl LaneFault {
    /// How many ticks the run had recorded when it stopped.
    #[inline]
    #[must_use]
    pub const fn ticks(&self) -> usize {
        match *self {
            LaneFault::WakeNotAfter { ticks, .. }
            | LaneFault::TooManyEvents { ticks, .. }
            | LaneFault::Hang { ticks, .. } => ticks,
        }
    }
}

impl fmt::Display for LaneFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            LaneFault::WakeNotAfter { at, wake, .. } => {
                write!(f, "a wake at {wake} ns, not after the frame at {at} ns")
            }
            LaneFault::TooManyEvents { at, .. } => {
                write!(f, "the lane would pass {LANE_MAX_EVENTS} calls at {at} ns")
            }
            LaneFault::Hang { .. } => {
                f.write_str("the story's lane never parked (a minute of story time)")
            }
        }
    }
}

impl std::error::Error for LaneFault {}

/// One closed-loop run of a scenario.
#[derive(Clone, Debug)]
pub struct LaneRun {
    scenario: Scenario,
    origin: Instant,
    calls: Vec<MomentumEvent>,
    ticks: Vec<LaneTick>,
    counts: LaneCounts,
    fnv: u64,
    fault: Option<LaneFault>,
}

impl LaneRun {
    /// The scenario it ran (for [`drive_m3_story`], the timeline it made).
    #[inline]
    #[must_use]
    pub const fn scenario(&self) -> &Scenario {
        &self.scenario
    }

    /// The origin every offset is taken from.
    #[inline]
    #[must_use]
    pub const fn origin(&self) -> Instant {
        self.origin
    }

    /// Every call, as the event an attached host records for it (so no reset of a glow at rest).
    #[inline]
    #[must_use]
    pub const fn calls(&self) -> &[MomentumEvent] {
        self.calls.as_slice()
    }

    /// Every tick, in order.
    #[inline]
    #[must_use]
    pub const fn ticks(&self) -> &[LaneTick] {
        self.ticks.as_slice()
    }

    /// The counts.
    #[inline]
    #[must_use]
    pub const fn counts(&self) -> LaneCounts {
        self.counts
    }

    /// The FNV-1a digest over every frame, quad, wake and reset ([`Fnv`]).
    #[inline]
    #[must_use]
    pub const fn fnv(&self) -> u64 {
        self.fnv
    }

    /// Why it stopped early, if it did.
    #[inline]
    #[must_use]
    pub const fn fault(&self) -> Option<LaneFault> {
        self.fault
    }

    /// The story's length: its scenario's end.
    #[inline]
    #[must_use]
    pub const fn span_ns(&self) -> u64 {
        self.scenario.end_ns
    }
}

struct Stop;

struct Lane<'g, G: Glow + ?Sized> {
    g: &'g mut G,
    origin: Instant,
    f_ns: u64,
    f: Duration,
    out: Vec<GlowQuad>,
    under: Vec<GlowQuad>,
    next_frame: Option<u64>,
    calls: Vec<MomentumEvent>,
    ticks: Vec<LaneTick>,
    counts: LaneCounts,
    fnv: Fnv,
    /// The canonical encoding of the frame on glass, and the encoder of the next one.
    glass: Enc,
    drawn: Enc,
    fault: Option<LaneFault>,
    /// A key arrived since the start or the last reset.
    keyed: bool,
}

impl<'g, G: Glow + ?Sized> Lane<'g, G> {
    fn new(g: &'g mut G, origin: Instant, f_ns: u64) -> Self {
        let mut glass = Enc::new();
        encode_frame(&mut glass, &MomentumGlowFrame::default(), &[], &[]);
        Lane {
            g,
            origin,
            f_ns,
            f: Duration::from_nanos(f_ns),
            out: Vec::with_capacity(QUADS),
            under: Vec::with_capacity(QUADS),
            next_frame: None,
            calls: Vec::new(),
            ticks: Vec::new(),
            counts: LaneCounts::default(),
            fnv: Fnv::new(),
            glass,
            drawn: Enc::new(),
            fault: None,
            keyed: false,
        }
    }

    fn room(&mut self, n: usize, at: u64) -> Result<(), Stop> {
        if self.calls.len().saturating_add(n) > LANE_MAX_EVENTS {
            self.fault = Some(LaneFault::TooManyEvents {
                ticks: self.ticks.len(),
                at,
            });
            return Err(Stop);
        }
        Ok(())
    }

    fn key(&mut self, t: u64, tau_bits: u32) -> Result<(), Stop> {
        self.room(1, t)?;
        self.g.key(at(self.origin, t), f32::from_bits(tau_bits));
        self.calls.push(MomentumEvent::Key {
            at: i128::from(t),
            tau_bits,
        });
        self.counts.keys += 1;
        self.keyed = true;
        let v = vsync_after(t, self.f_ns);
        self.next_frame = Some(match self.next_frame {
            Some(f) => f.min(v),
            None => v,
        });
        Ok(())
    }

    fn reset(&mut self, t: u64) -> Result<(), Stop> {
        if !self.keyed {
            self.g.rest();
            return Ok(());
        }
        self.room(1, t)?;
        self.g.rest();
        self.calls.push(MomentumEvent::Reset);
        self.fnv.reset();
        self.counts.resets += 1;
        self.keyed = false;
        Ok(())
    }

    fn hang(&mut self, at: u64, since: u64) {
        self.fault = Some(LaneFault::Hang {
            ticks: self.ticks.len(),
            at,
            since,
        });
    }

    fn tick(&mut self, t: u64, p: &Presentation) -> Result<(), Stop> {
        self.room(2, t)?;
        let cfg = p.cfg.config();
        self.out.clear();
        self.under.clear();
        let frame = self.g.frame(
            p.cur,
            at(self.origin, t),
            p.geom.geom(),
            &cfg,
            &mut self.out,
            &mut self.under,
        );
        let call = self.calls.len();
        self.calls.push(MomentumEvent::Tick {
            cur: p.cur,
            at: i128::from(t),
            geom: p.geom,
            cfg: p.cfg,
        });
        self.fnv.frame(frame, &self.out, &self.under);
        let quads = count(self.out.len().saturating_add(self.under.len()));
        self.counts.ticks += 1;
        self.counts.quads = self.counts.quads.saturating_add(quads);
        self.counts.max_quads = self.counts.max_quads.max(quads);
        self.drawn.clear();
        encode_frame(&mut self.drawn, &frame, &self.out, &self.under);
        let key = fnv1a(self.drawn.as_bytes());
        let presented = self.drawn.as_bytes() != self.glass.as_bytes();
        if presented {
            self.counts.presented += 1;
            core::mem::swap(&mut self.glass, &mut self.drawn);
        }
        let mut stopped = false;
        let (after, next) = if self.g.active() {
            let v = vsync_after(t, self.f_ns);
            (After::Cadence { at: v }, Some(v))
        } else if self.g.settles() {
            let w = self
                .g
                .wake(at(self.origin, t), self.f)
                .map(|w| offset(self.origin, w));
            let query = self.calls.len();
            self.calls.push(MomentumEvent::Query {
                at: i128::from(t),
                frame_interval: self.f,
            });
            self.fnv.wake(w);
            self.counts.queries += 1;
            match w {
                Some(w) if w > t => (
                    After::Wake {
                        query,
                        wake: Some(w),
                    },
                    Some(w),
                ),
                Some(w) => {
                    self.fault = Some(LaneFault::WakeNotAfter {
                        ticks: self.ticks.len().saturating_add(1),
                        at: t,
                        wake: w,
                    });
                    stopped = true;
                    (
                        After::Wake {
                            query,
                            wake: Some(w),
                        },
                        None,
                    )
                }
                None => {
                    self.counts.stranded += 1;
                    (After::Wake { query, wake: None }, None)
                }
            }
        } else {
            self.counts.parks += 1;
            (After::Park, None)
        };
        self.ticks.push(LaneTick {
            at: t,
            call,
            frame,
            out: self.out.len(),
            under: self.under.len(),
            key,
            presented,
            after,
        });
        self.next_frame = next;
        if stopped { Err(Stop) } else { Ok(()) }
    }

    fn finish(self, scenario: Scenario) -> LaneRun {
        LaneRun {
            scenario,
            origin: self.origin,
            calls: self.calls,
            ticks: self.ticks,
            counts: self.counts,
            fnv: self.fnv.value(),
            fault: self.fault,
        }
    }
}

/// Runs `scenario` against `g` (expected at rest) closed loop from `origin`.
pub fn run<G: Glow + ?Sized>(g: &mut G, scenario: &Scenario, origin: Instant) -> LaneRun {
    let mut lane = Lane::new(g, origin, scenario.frame_interval_ns);
    let mut present = scenario.start;
    let events = scenario.events.as_slice();
    let mut idx = 0usize;
    let mut last_input = 0u64;
    loop {
        let (t, input) = match (events.get(idx), lane.next_frame) {
            (Some(&(e, _)), Some(f)) if f < e => (f, None),
            (Some(&(e, exo)), _) => {
                idx += 1;
                (e, Some(exo))
            }
            (None, Some(f)) => (f, None),
            (None, None) => break,
        };
        if t > scenario.end_ns {
            break;
        }
        let step = match input {
            Some(exo) => {
                last_input = t;
                match exo {
                    Exo::Key { tau_bits } => lane.key(t, tau_bits),
                    Exo::Reset => lane.reset(t),
                    Exo::Present(p) => {
                        present = p;
                        Ok(())
                    }
                }
            }
            None if t.saturating_sub(last_input) > HANG_NS => {
                lane.hang(t, last_input);
                Err(Stop)
            }
            None => lane.tick(t, &present),
        };
        if step.is_err() {
            break;
        }
    }
    lane.finish(scenario.clone())
}

/// The M3 story's frame interval, 16 666 667 ns (60 Hz): the momentum fixtures' `LANE`.
pub const S0_FRAME_INTERVAL_NS: u64 = 16_666_667;

/// The M3 story's τ, for every key and tick.
pub const S0_TAU: f32 = MOMENTUM_GLOW_TAU_S;

const S0_ROWS: u16 = 24;
const S0_COLS: u16 = 100;
const S0_KEYS_PER_BURST: u16 = 24;
const S0_IDLE_AFTER_PARK_NS: u64 = 500_000_000;
const S0_CUT_AFTER_NS: u64 = 400_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Dark,
    Light,
}

#[derive(Clone, Copy)]
enum Cool {
    /// Until the lane parks, then idle.
    Park,
    /// Cut by the next burst 400 ms after the last key.
    Cut,
}

const S0_SEGMENTS: [(Page, u64, Cool); 8] = [
    (Page::Dark, 3, Cool::Park),
    (Page::Dark, 6, Cool::Cut),
    (Page::Dark, 10, Cool::Park),
    (Page::Dark, 16, Cool::Cut),
    (Page::Light, 3, Cool::Park),
    (Page::Light, 6, Cool::Cut),
    (Page::Light, 10, Cool::Park),
    (Page::Light, 16, Cool::Park),
];

const S0_SEGMENT_STARTS: [u64; 8] = [
    0,
    11_797_395_725,
    16_030_729_043,
    23_526_157_079,
    25_363_657_079,
    37_161_053_012,
    41_394_386_330,
    48_889_814_099,
];

const S0_END_NS: u64 = 55_522_741_583;

/// The M3 story's geometry.
#[must_use]
pub const fn s0_geom() -> Geom {
    Geom {
        cw: 15,
        ch: 28,
        rows: S0_ROWS as usize,
        cols: S0_COLS as usize,
        origin_x: 24,
        origin_y: 84,
        win_w: 24 + S0_COLS * 15 + 24,
        win_h: 84 + S0_ROWS * 28 + 24,
        head: 80,
    }
}

const fn s0_config(page: Page, col: u16) -> MomentumGlowConfig {
    let dark = matches!(page, Page::Dark);
    MomentumGlowConfig {
        enabled: true,
        intensity: 1.0,
        tau_s: S0_TAU,
        radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
        base: 0x0050_FA7B,
        dark_theme: dark,
        hue: 0x0050_FA7B,
        theme_fg: if dark { 0x00C8_C8D2 } else { 0x0024_292F },
        theme_bg: if dark { 0x0010_1016 } else { 0x00FF_FFFF },
        block: true,
        glyph_span: Some((0, col as i32)),
    }
}

const fn s0_presentation(page: Page, row: u16, col: u16) -> Presentation {
    Presentation::of(Some((row, col)), &s0_geom(), &s0_config(page, col))
}

const fn s0_advance(row: u16, col: u16) -> (u16, u16) {
    let col = col + 1;
    if col >= S0_COLS - 1 {
        (if row + 1 >= S0_ROWS { 0 } else { row + 1 }, 4)
    } else {
        (row, col)
    }
}

/// The M3 story as the perf harness's closed-loop program (design §8.1).
pub fn drive_m3_story<G: Glow + ?Sized>(g: &mut G, origin: Instant) -> LaneRun {
    let mut lane = Lane::new(g, origin, S0_FRAME_INTERVAL_NS);
    let tau_bits = S0_TAU.to_bits();
    let mut inputs: Vec<(u64, Exo)> = Vec::new();
    let mut now = 0u64;
    let (mut row, mut col) = (2u16, 4u16);
    let mut page = Page::Dark;
    let start = s0_presentation(page, row, col);
    let mut present = start;
    'story: for (seg_page, rate, cool) in S0_SEGMENTS {
        if seg_page != page {
            if lane.reset(now).is_err() {
                break 'story;
            }
            inputs.push((now, Exo::Reset));
            page = seg_page;
            row = (row + 2) % S0_ROWS;
            col = 4;
            present = s0_presentation(page, row, col);
            inputs.push((now, Exo::Present(present)));
        }
        let gap = 1_000_000_000 / rate;
        let seg_start = now;
        let mut next_key = now;
        let mut typed = 0u16;
        let mut cut_at = None;
        loop {
            let key = if typed < S0_KEYS_PER_BURST {
                Some(next_key)
            } else {
                None
            };
            let (t, is_key) = match (key, lane.next_frame) {
                (Some(k), Some(f)) if f < k => (f, false),
                (Some(k), _) => (k, true),
                (None, Some(f)) => (f, false),
                (None, None) => break,
            };
            if let Some(cut) = cut_at
                && t > cut
            {
                now = cut;
                break;
            }
            if t.saturating_sub(seg_start) > HANG_NS {
                lane.hang(t, seg_start);
                break 'story;
            }
            now = t;
            if is_key {
                if lane.key(t, tau_bits).is_err() {
                    break 'story;
                }
                inputs.push((t, Exo::Key { tau_bits }));
                typed += 1;
                next_key = t + gap;
                if typed == S0_KEYS_PER_BURST && matches!(cool, Cool::Cut) {
                    cut_at = Some(t + S0_CUT_AFTER_NS);
                }
                (row, col) = s0_advance(row, col);
                present = s0_presentation(page, row, col);
                inputs.push((t, Exo::Present(present)));
                continue;
            }
            if lane.tick(t, &present).is_err() {
                break 'story;
            }
        }
        if matches!(cool, Cool::Park) {
            now += S0_IDLE_AFTER_PARK_NS;
        }
    }
    let scenario = Scenario {
        frame_interval_ns: S0_FRAME_INTERVAL_NS,
        start,
        events: inputs,
        end_ns: now,
    };
    lane.finish(scenario)
}

/// `S0`, the M3 story frozen as an absolute scenario.
#[must_use]
pub fn s0() -> Scenario {
    let tau_bits = S0_TAU.to_bits();
    let mut inputs = Vec::with_capacity(S0_SEGMENTS.len() * (2 * S0_KEYS_PER_BURST as usize + 2));
    let (mut row, mut col) = (2u16, 4u16);
    let mut page = Page::Dark;
    let start = s0_presentation(page, row, col);
    for seg_idx in 0..S0_SEGMENTS.len() {
        let (seg_page, rate, _) = S0_SEGMENTS[seg_idx];
        let seg_start = S0_SEGMENT_STARTS[seg_idx];
        if seg_page != page {
            inputs.push((seg_start, Exo::Reset));
            page = seg_page;
            row = (row + 2) % S0_ROWS;
            col = 4;
            inputs.push((seg_start, Exo::Present(s0_presentation(page, row, col))));
        }
        let gap = 1_000_000_000 / rate;
        for k in 0..u64::from(S0_KEYS_PER_BURST) {
            let t = seg_start + k * gap;
            inputs.push((t, Exo::Key { tau_bits }));
            (row, col) = s0_advance(row, col);
            inputs.push((t, Exo::Present(s0_presentation(page, row, col))));
        }
    }
    Scenario {
        frame_interval_ns: S0_FRAME_INTERVAL_NS,
        start,
        events: inputs,
        end_ns: S0_END_NS,
    }
}
