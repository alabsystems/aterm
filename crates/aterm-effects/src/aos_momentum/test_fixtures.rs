// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test fixtures, deterministic call-stream generator, and host lane runner for AOS momentum glow.

use std::time::Duration;

use aterm_aos::effects::NoEffects;
use aterm_aos::replay::{Run, replay};
use aterm_render::GlowQuad;
use aterm_time::Instant;

use super::{HostMode, Hosted, MomentumCode, MomentumHost, MomentumSchema, PrebuiltMomentum};
use crate::cursor_glow::Geom;
use crate::cursor_momentum::{
    MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MomentumGlow, MomentumGlowConfig,
    MomentumGlowFrame,
};

pub(super) const TAU: f32 = MOMENTUM_GLOW_TAU_S;

/// The shipped 120 Hz effect lane.
pub(super) const LANE: Duration = Duration::from_nanos(16_666_667);

/// The build id the tests attach under.
pub(super) const BUILD: &str = "aos-momentum-test-build";

pub(super) fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// 10×20 px cells, 24×80, at the window origin.
pub(super) fn geom() -> Geom {
    Geom {
        cw: 10,
        ch: 20,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 800,
        win_h: 480,
        head: 0,
    }
}

/// A padded, headed window: the grid below a chrome band, as a real window lays it.
pub(super) fn headed_geom() -> Geom {
    Geom {
        cw: 9,
        ch: 18,
        rows: 30,
        cols: 100,
        origin_x: 6,
        origin_y: 40,
        win_w: 912,
        win_h: 590,
        head: 28,
    }
}

/// The host's config for a block caret on a dark or light page.
pub(super) fn cfg(dark: bool) -> MomentumGlowConfig {
    MomentumGlowConfig {
        enabled: true,
        intensity: 1.0,
        tau_s: TAU,
        radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
        base: 0x0050_FA7B,
        dark_theme: dark,
        hue: 0x0050_FA7B,
        theme_fg: if dark { 0x00C8_C8D2 } else { 0x0024_292F },
        theme_bg: if dark { 0x0010_1016 } else { 0x00FF_FFFF },
        block: true,
        glyph_span: None,
    }
}

/// splitmix64: deterministic, seedable, no dependency.
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub(super) fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    pub(super) fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        let i = usize::try_from(self.below(xs.len() as u64)).unwrap_or(0);
        xs[i]
    }
}

/// One host call, at a time after the stream's origin.
#[derive(Clone, Copy)]
pub(super) enum Call {
    Key {
        at: Duration,
        tau: f32,
    },
    Reset,
    Tick {
        cur: Option<(u16, u16)>,
        at: Duration,
        geom: Geom,
        cfg: MomentumGlowConfig,
        /// Other effects' quads already in each stream.
        prefix: usize,
    },
    Query {
        at: Duration,
        lane: Duration,
    },
}

/// What the host saw for one call.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Seen {
    Key,
    Reset,
    Frame {
        frame: MomentumGlowFrame,
        out: Vec<GlowQuad>,
        under: Vec<GlowQuad>,
    },
    Schedule {
        is_active: bool,
        settling: bool,
        cold_at: Option<Instant>,
        deadline: Option<Instant>,
    },
}

/// Generate a deterministic stream of `len` host calls.
pub(super) fn random_stream(seed: u64, len: usize, in_profile: bool) -> Vec<Call> {
    let mut r = Rng::new(seed);
    let mut at = Duration::ZERO;
    let mut calls = Vec::with_capacity(len);
    let geoms = [geom(), headed_geom()];
    for _ in 0..len {
        at += Duration::from_millis(r.pick(&[0, 0, 4, 8, 16, 17, 33, 60, 90, 120, 400, 1500]));
        let roll = r.below(100);
        let call = if roll < 30 {
            let tau = if in_profile {
                r.pick(&[TAU, TAU, TAU, 0.4, 3.0, 60.0])
            } else {
                r.pick(&[TAU, TAU, f32::NAN, 0.0, -1.0, 90.0, f32::INFINITY])
            };
            Call::Key { at, tau }
        } else if roll < 32 {
            Call::Reset
        } else if roll < 77 {
            let mut g = r.pick(&geoms);
            if !in_profile && r.below(10) == 0 {
                g.cw = 0;
            }
            let mut c = cfg(r.below(2) == 0);
            c.intensity = r.pick(&[1.0, 1.0, 0.6, 0.0]);
            c.enabled = r.below(12) != 0;
            c.block = r.below(4) != 0;
            c.tau_s = r.pick(&[TAU, TAU, TAU, 2.0]);
            c.radius_cells = r.pick(&[MOMENTUM_GLOW_RADIUS_CELLS, 1.0, 4.0, 0.0]);
            c.hue = r.pick(&[0x0050_FA7B, 0x00FF_4FA0, 0x0040_80FF]);
            let cols = i32::try_from(g.cols).unwrap_or(0);
            c.glyph_span = match r.below(3) {
                0 => None,
                1 => Some((0, cols / 2)),
                _ => Some((3, cols)),
            };
            if !in_profile && r.below(8) == 0 {
                c.intensity = f32::NAN;
                c.glyph_span = Some((9, 2));
            }
            let row = u16::try_from(r.below(g.rows as u64 + 2)).unwrap_or(0);
            let col = u16::try_from(r.below(g.cols as u64 + 2)).unwrap_or(0);
            let cur = if r.below(10) == 0 {
                None
            } else {
                Some((row, col))
            };
            Call::Tick {
                cur,
                at,
                geom: g,
                cfg: c,
                prefix: usize::try_from(r.below(3)).unwrap_or(0),
            }
        } else {
            let lane = if in_profile {
                r.pick(&[
                    LANE,
                    ms(33),
                    Duration::from_nanos(8_333_333),
                    Duration::from_secs(1),
                ])
            } else {
                r.pick(&[LANE, ms(33), Duration::from_secs(2)])
            };
            Call::Query { at, lane }
        };
        calls.push(call);
    }
    calls
}

/// The calls the host makes on the glow.
pub(super) trait Glow {
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
    fn cold(&self) -> Option<Instant>;
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant>;
}

impl Glow for MomentumGlow {
    fn key(&mut self, now: Instant, tau: f32) {
        self.on_key(now, tau);
    }
    fn rest(&mut self) {
        self.reset();
    }
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
    fn active(&self) -> bool {
        self.is_active()
    }
    fn settles(&self) -> bool {
        self.settling()
    }
    fn cold(&self) -> Option<Instant> {
        self.cold_at()
    }
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant> {
        self.next_change_deadline(now, lane)
    }
}

impl Glow for MomentumHost {
    fn key(&mut self, now: Instant, tau: f32) {
        self.on_key(now, tau);
    }
    fn rest(&mut self) {
        self.reset();
    }
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
    fn active(&self) -> bool {
        self.is_active()
    }
    fn settles(&self) -> bool {
        self.settling()
    }
    fn cold(&self) -> Option<Instant> {
        self.cold_at()
    }
    fn wake(&mut self, now: Instant, lane: Duration) -> Option<Instant> {
        self.next_change_deadline(now, lane)
    }
}

/// `n` distinct quads standing for other effects' light already in a stream.
pub(super) fn sentinels(n: usize, tag: u32) -> Vec<GlowQuad> {
    (0..n)
        .map(|i| {
            let i = u16::try_from(i).unwrap_or(0);
            GlowQuad::flat(i, 700 + i, 3 * i, 2, 1, tag + u32::from(i), 0)
        })
        .collect()
}

/// Draw one frame into streams with `prefix` sentinel quads.
pub(super) fn draw(
    g: &mut impl Glow,
    cur: Option<(u16, u16)>,
    now: Instant,
    geom: Geom,
    cfg: &MomentumGlowConfig,
    prefix: usize,
) -> Seen {
    let (pre_out, pre_under) = (
        sentinels(prefix, 0x0011_0000),
        sentinels(prefix, 0x0022_0000),
    );
    let (mut out, mut under) = (pre_out.clone(), pre_under.clone());
    let frame = g.frame(cur, now, geom, cfg, &mut out, &mut under);
    assert_eq!(
        out.get(..prefix),
        Some(&pre_out[..]),
        "another effect's light was touched"
    );
    assert_eq!(
        under.get(..prefix),
        Some(&pre_under[..]),
        "another effect's ink was touched"
    );
    Seen::Frame {
        frame,
        out: out.split_off(prefix),
        under: under.split_off(prefix),
    }
}

/// Plays `calls` from `t0` and returns what the host saw for each.
pub(super) fn play(g: &mut impl Glow, t0: Instant, calls: &[Call]) -> Vec<Seen> {
    calls
        .iter()
        .map(|call| match *call {
            Call::Key { at, tau } => {
                g.key(t0 + at, tau);
                Seen::Key
            }
            Call::Reset => {
                g.rest();
                Seen::Reset
            }
            Call::Tick {
                cur,
                at,
                geom,
                cfg,
                prefix,
            } => draw(g, cur, t0 + at, geom, &cfg, prefix),
            Call::Query { at, lane } => Seen::Schedule {
                is_active: g.active(),
                settling: g.settles(),
                cold_at: g.cold(),
                deadline: g.wake(t0 + at, lane),
            },
        })
        .collect()
}

/// Simulate `keys` keystrokes 60 ms apart starting at `start`.
pub(super) fn burst(
    g: &mut impl Glow,
    start: Instant,
    keys: u16,
    cfg: &MomentumGlowConfig,
    seen: &mut Vec<Seen>,
) -> (Instant, (u16, u16)) {
    let mut now = start;
    let mut cell = (4, 10);
    for k in 0..keys {
        now = start + ms(60 * u64::from(k));
        cell = (4, 10 + k);
        g.key(now, TAU);
        seen.push(draw(g, Some(cell), now, geom(), cfg, 1));
    }
    (now, cell)
}

/// Where the host's lane parked.
pub(super) struct Parked {
    /// The instant of the last frame.
    pub(super) at: Instant,
    /// The glow was still hot when the lane let go: light stranded on glass.
    pub(super) stranded: bool,
    /// The last frame the lane drew.
    pub(super) last: Option<Seen>,
}

/// Run the host frame lane from `now` until it parks.
pub(super) fn run_lane(
    g: &mut impl Glow,
    mut now: Instant,
    lane: Duration,
    cell: (u16, u16),
    cfg: &MomentumGlowConfig,
    seen: &mut Vec<Seen>,
) -> Parked {
    let start = now;
    let mut last = None;
    loop {
        let next = if g.active() {
            Some(now + lane)
        } else if g.settles() {
            g.wake(now, lane)
        } else {
            None
        };
        let Some(next) = next else {
            return Parked {
                at: now,
                stranded: g.settles() || g.active(),
                last,
            };
        };
        assert!(next > now, "a wake at or before now is a busy re-arm");
        assert!(
            next.saturating_duration_since(start) < Duration::from_secs(60),
            "the lane never parked"
        );
        now = next;
        let shown = draw(g, Some(cell), now, geom(), cfg, 1);
        seen.push(shown.clone());
        last = Some(shown);
    }
}

/// The attached task inside a host.
pub(super) fn hosted(h: &MomentumHost) -> &Hosted {
    match &h.mode {
        HostMode::Hosted(h) => h,
        HostMode::Direct(_) => panic!("fixture: the host is attached"),
    }
}

/// The attached task inside a host, mutably.
pub(super) fn hosted_mut(h: &mut MomentumHost) -> &mut Hosted {
    match &mut h.mode {
        HostMode::Hosted(h) => h,
        HostMode::Direct(_) => panic!("fixture: the host is attached"),
    }
}

/// `code` replayed from the host's ancestor checkpoint over its log.
pub(super) fn replay_log(h: &MomentumHost, code: MomentumCode) -> Run<MomentumSchema> {
    let b = &hosted(h).boundary;
    let checkpoint = b.checkpoint().expect("fixture: attached");
    let log = b.log().expect("fixture: attached");
    let start = checkpoint
        .restore(b.schema(), b.identity())
        .expect("the ancestor restores");
    replay(
        b.schema(),
        &PrebuiltMomentum::new(code, BUILD),
        start,
        log.inputs(),
        log.header().first_cursor,
        &mut NoEffects,
    )
}

/// A host attached at `epoch` running `code`.
pub(super) fn attached(epoch: Instant, code: MomentumCode) -> MomentumHost {
    let mut h = MomentumHost::default();
    h.aos_attach(epoch, code, BUILD, 1);
    h
}
