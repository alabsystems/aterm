// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typing-reactive rainbow cursor: the block cursor glows and changes colour with typing momentum.

use std::time::Duration;

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::OVER_INK_COV_CAP;

use crate::cursor_glow::Geom;
use crate::cursor_glow::{
    RAINBOW_CARET_LIGHT_FLOOR, RAINBOW_FIELD_LEVEL, RAINBOW_SPARKLE_LIGHT_SHARE,
    rainbow_phase_from_unit_turn, rainbow_sweep_at, rainbow_sweep_reflect, rainbow_thing_of,
};
use crate::effect_util::{lerp, mix_rgb};
use crate::rainbow_kitty::timing::spring_snap;

/// The base the rainbow blooms from when the host names none: white on a dark
/// theme, a soft near-black on a light one.
const BASE_DARK_THEME: u32 = 0x00FF_FFFF;
const BASE_LIGHT_THEME: u32 = 0x0016_161C;

/// The page the caret's light lands on when the host names none: the default
/// background of each polarity (`#111318` dark, `#FDF6E3` light).
const GROUND_DARK_THEME: u32 = 0x0011_1318;
const GROUND_LIGHT_THEME: u32 = 0x00FD_F6E3;

/// Hue rotation in turns/second: a slow baseline while charged, plus up to a
/// full brisk spin at peak energy (≈one rotation/sec typing flat-out).
const IDLE_SPIN: f32 = 0.05;
const ACTIVE_SPIN: f32 = 1.05;

/// Idle breath (turns/sec of the halo pulse) + its depth — the gentle "ready" pulse.
const PULSE_HZ: f32 = 0.34;
const PULSE_DEPTH: f32 = 0.55;

/// Saturation / value ramps from the calm idle ember to the vivid typing bloom.
const SAT_IDLE: f32 = 0.32;
/// The light theme's idle saturation — see the emit site for why it is so much
/// higher than the dark one.
const SAT_IDLE_LIGHT: f32 = 0.88;
const SAT_MAX: f32 = 1.0;
const VAL_IDLE: f32 = 0.82;
const VAL_MAX: f32 = 1.0;

/// How far the block fill tints from the base (white/black) toward the live rainbow:
/// a whisper at rest, vivid under the keys.
const MIX_IDLE: f32 = 0.16;
const MIX_MAX: f32 = 0.82;
/// Light-theme mixes, higher than the dark ones: a hue mixed toward near-black
/// turns to mud, so on a light page the active block must be a saturated
/// rainbow.
const MIX_IDLE_LIGHT: f32 = MIX_IDLE;
const MIX_MAX_LIGHT: f32 = 0.95;

/// Halo geometry and brightness: thin concentric additive rings whose coverage falls off
/// quadratically from the block, so they read as one soft rim.
const HALO_LAYERS: i32 = 4;
const HALO_RADIUS_IDLE: f32 = 0.06;
const HALO_RADIUS_MAX: f32 = 0.22;
const HALO_BASE_COV: f32 = 28.0;
/// Brightness kept while idle (× the breath). The settled ember's coverage is
/// `HALO_BASE_COV · HALO_IDLE_FLOOR · (0.35 + PULSE_DEPTH · breath)` truncated
/// to `u8`; much below 0.30 the innermost ring quantizes to zero and the idle
/// glow vanishes.
const HALO_IDLE_FLOOR: f32 = 0.30;
/// Hue spread across the halo rings (turns): each ring sits further along the
/// wheel than the one inside it, so the rim ripples outward as a rainbow.
const HALO_HUE_SPREAD_IDLE: f32 = 0.10;
const HALO_HUE_SPREAD_MAX: f32 = 0.34;
/// Momentum spin (turns per second at full paint): the rim's spectrum rotates in proportion to
/// the display spine, on top of the family phase, so speed reads as motion.
const RIM_SPIN_TURNS_PER_S: f32 = 0.45;

/// The rim's light is hue-invariant: every ring is laid at the light a grey ring of this
/// relative luminance (linear, `0..1`) would add to the page at the same coverage, solved on
/// the premultiplied bytes over the page by [`ring_light_on_page`].
const RIM_LIGHT: f32 = 0.462;
/// The grey of relative luminance [`RIM_LIGHT`] (sRGB byte `181`).
const RIM_LIGHT_GREY: u32 = 0x00B5_B5B5;

/// Energy below which the cursor is settled: the animator reports inactive so the host stops
/// the frame tick.
const SETTLED_ENERGY: f32 = 0.02;

/// Momentum rungs: the caret sparkles on upward crossings of these by its
/// paint, with hysteresis.
const MOMENTUM_FLARE_STEPS: [f32; 4] = [0.25, 0.50, 0.75, 1.00];
/// A rung re-arms only once the paint has fallen this far below it, so a spine
/// hovering on a rung cannot re-fire on jitter.
const MOMENTUM_FLARE_HYST: f32 = 0.06;
/// Flare length (seconds). Shorter than the spine's travel between two rungs
/// at any human typing rate, so each flare completes before the next fires.
const TWINKLE_DUR: f32 = 0.16;
/// The white hold on a v2 meteor's birth frame. `flare_at` is the tick's own
/// `now`, so frame 0 sees age 0; under half a 120 Hz frame, so frame 1 is on
/// the spring at any refresh rate while frame-0 jitter still reads white.
const FLARE_WHITE_HOLD_S: f32 = 0.004;
/// §7.1: the four halo rings pop radius ×1.4 on the flare's edge …
const FLARE_RING_POP: f32 = 0.4;
/// … and relax on τ 120 ms.
const FLARE_RING_TAU_S: f32 = 0.120;
/// How far the block fill glints toward the star colour at the flare peak.
const TWINKLE_MIX: f32 = 0.35;
/// Star-arm overhang past the block edge, as a fraction of the cell's own axis.
const TWINKLE_REACH: f32 = 0.20;
/// Peak additive coverage of the star arms and glitter dots, bounded by
/// [`crate::cursor_glow::OVER_INK_COV_CAP`] because the arms cross neighbouring
/// glyphs.
const TWINKLE_ARM_COV: f32 = 44.0;
const TWINKLE_DOT_COV: f32 = 38.0;
/// Scintillation cycles across one flare: a deterministic sine wobble over the
/// pop envelope, phase-shifted per flare so consecutive twinkles differ.
const TWINKLE_SCINT: f32 = 0.45;
const _: () = assert!(
    TWINKLE_SCINT / TWINKLE_DUR <= crate::effect_util::TWINKLE_FLASH_BOUND_HZ,
    "twinkle scintillation over the WCAG 2.3.1 general-flash bound"
);
const _: () = assert!(TWINKLE_SCINT > 0.0, "the flare must still scintillate");

/// Per-tick dt clamp (seconds) across a charged interval, so a stalled but
/// charged window does not fling the phase forward.
const MAX_DT: f32 = 0.6;

/// How many frame steps [`CursorRainbow::next_change_deadline`] dry-runs before
/// it hands the host a poll.
const SETTLE_LOOKAHEAD: u32 = 24;
/// The coarsest the colour release is sampled. Bounds the sleep a probe asks
/// for at the fade's slow end and how far a mispredicted paint drifts before the
/// next real tick.
const SETTLE_POLL_CAP: Duration = Duration::from_millis(250);

/// The inputs of the last real tick, latched so
/// [`CursorRainbow::next_change_deadline`] can dry-run the engine without the
/// host re-deriving them.
#[derive(Clone, Copy)]
struct SettleProbe {
    cur: Option<(u16, u16)>,
    energy: f32,
    energy_prev: f32,
    family: Option<(f32, f32)>,
    blink_phase: bool,
    dark_theme: bool,
    geom: Geom,
    cfg: RainbowConfig,
    /// `cfg.paint` at the tick before the latched one.
    paint_prev: Option<f32>,
    /// Wall seconds between those two ticks — the base of the extrapolation.
    sample_dt: f32,
}

/// What one dry run of the colour release answered, memoised until the next
/// real tick (see [`CursorRainbow::next_change_deadline`]).
#[derive(Clone, Copy)]
enum SettleWake {
    /// Wake here: one frame before the first frame that differs from the one on
    /// glass, or the probe's horizon when none did.
    At(Instant),
    /// A host envelope is rising — not a release, and not something a release
    /// model may guess at.
    Cadence,
}

/// Exact input key for [`CursorRainbow::equalise_rim_light`] and (when no
/// twinkle is active) [`CursorRainbow::rim_light_peak`].
#[derive(Clone, Copy, PartialEq, Eq)]
struct RimMemoKey {
    cur: (u16, u16),
    cw: usize,
    ch: usize,
    rows: usize,
    cols: usize,
    origin_x: u16,
    origin_y: u16,
    head: u16,
    gxy: [(i32, i32); HALO_LAYERS as usize],
    page: u32,
    n_rings: u8,
    rings: [(usize, u8, u32, u32); HALO_LAYERS as usize],
}

#[derive(Clone, Copy)]
struct RimMemo {
    key: RimMemoKey,
    best: Option<[u32; HALO_LAYERS as usize]>,
    peak_dark_no_twinkle: Option<f32>,
}

/// Extrapolate a released envelope `ahead` seconds past its last sample pair
/// with a first-order log-linear fit.
#[inline]
fn release_forecast(now_v: f32, prev_v: f32, dt: f32, ahead: f32) -> Option<f32> {
    if now_v <= 0.0 {
        return Some(0.0);
    }
    if prev_v < now_v {
        return None;
    }
    if dt <= 0.0 || prev_v <= now_v {
        // A live (> `SETTLED_ENERGY`) envelope that has not begun decaying
        return (now_v <= SETTLED_ENERGY).then_some(now_v);
    }
    let ratio = (now_v / prev_v).clamp(1e-3, 1.0);
    Some(now_v * ratio.powf(ahead / dt))
}

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct RainbowConfig {
    /// Master on/off (the style opted into the rainbow cursor and the cursor is a focused, visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in by the host exactly like the aurora. 0 ⇒ effectively off (no spin, no halo).
    pub intensity: f32,
    /// The terminal reports a blinking block.
    pub blinking: bool,
    /// The colour the block wears at rest, `0x00RRGGBB`: the terminal's resolved cursor colour (OSC 12, else the theme cursor, else the OSC 10 foreground).
    pub base: Option<u32>,
    /// The ribbon-head colour `CursorGlow` emitted this frame, `0x00RRGGBB`.
    pub head_rgb: Option<u32>,
    /// The trail's paint spine, `0..1`: how strongly the ribbon beside the caret is coloured now.
    pub paint: Option<f32>,
    /// The page the caret's light lands on, `0x00RRGGBB`: the resolved terminal background.
    pub ground: Option<u32>,
    /// The instant a v2 meteor's frame-0 flare fired (`RAINBOW-KITTY-V2.md` §7.1), copied from [`crate::cursor_glow::CursorGlow::caret_flare_at`], which reads `None` once the relax settles.
    pub flare_at: Option<Instant>,
}

/// What a tick produced: the block fill colour to hand the renderer and a fingerprint that changes on every visible step (0 when dormant).
#[derive(Clone, Copy, Debug)]
pub struct RainbowFrame {
    /// The evolving block-fill colour `0x00RRGGBB`, or `None` when the rainbow cursor is off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + halo (0 ⇒ nothing to show this frame).
    pub fp: u64,
}

/// Per-window rainbow-cursor animation state: the standalone hue fallback, idle breath, twinkle, and the colour release's settle probe.
#[derive(Clone, Default)]
pub struct CursorRainbow {
    /// Responsive standalone spin in unit turns `0..1`. It is lifted onto one
    /// complete family sweep by [`rainbow_phase_from_unit_turn`] when a host
    /// does not provide the ribbon's shared phase.
    phase: f32,
    /// Idle-breath phase in turns `0..1`.
    pulse: f32,
    last: Option<Instant>,
    /// Latched energy at the last tick (so [`Self::is_active`] answers without a clock).
    energy: f32,
    /// Latched colour envelope at the last tick (the paint spine folded with
    /// the energy, [`RainbowConfig::paint`]).
    paint: f32,
    /// The highest momentum rung ([`MOMENTUM_FLARE_STEPS`]) crossed upward and
    /// not yet released: the twinkle's edge detector.
    flare_rung: u8,
    /// Start of the in-flight twinkle flare (`None` between flares).
    twinkle_at: Option<Instant>,
    /// Flare counter — the deterministic per-flare variation seed (dot
    /// corners + scintillation phase), and the fingerprint's flare identity.
    twinkle_seq: u32,
    /// The momentum spin's accumulated offset along the family sweep, in turns.
    spin: f32,
    /// Latched "a flare is mid-flight" at the last tick (the [`Self::is_active`] clockless
    /// answer, like `energy`).
    twinkling: bool,
    /// Latched "a v2 meteor flare (`cfg.flare_at`) is mid-flight" at the last tick
    /// (`age < SPRING_SNAP_RESPONSE_S`), so [`Self::is_active`] keeps the frame train through
    /// the spring-snap relax and `caret_paint`'s 180 ms landing hold.
    flaring: bool,
    /// The fingerprints of the last two ticks, so [`Self::is_active`] answers from the u8-level
    /// delta the host would present rather than from `paint > SETTLED_ENERGY`: the colour
    /// envelope cools slowly while the u8 fill often repeats frame after frame.
    fp_last: u64,
    fp_prev: u64,
    /// Scratch pixel buffer for [`Self::rim_light_peak`], retained across
    /// frames.
    rim_scratch: Vec<u32>,
    /// Memoised [`Self::equalise_rim_light`] and no-twinkle [`Self::rim_light_peak`]
    /// for the last integer ring geometry and palette.
    rim_memo: Option<RimMemo>,
    /// The last real tick's inputs, for the colour release's dry run. `None`
    /// before the first tick and across every inert early-out.
    settle_probe: Option<SettleProbe>,
    /// The dry run's answer for the frame interval it was asked at, cleared by
    /// every real tick, so repeated plans between two ticks cost one dry run.
    settle_memo: Option<(Duration, SettleWake)>,
    /// The dry run's quad sink, retained like `rim_scratch`: the probe lays the
    /// caret's halo exactly as a real tick does and throws it away.
    settle_sink: Vec<GlowQuad>,
}

impl CursorRainbow {
    /// Whether the host must keep arming the animation tick: while the cursor is charged or a twinkle is in flight.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.energy > SETTLED_ENERGY
            || self.twinkling
            || self.flaring
            || (self.paint > SETTLED_ENERGY && self.fp_last != self.fp_prev)
    }

    /// The colour release still owes frames, but not at frame cadence: the caret is painted (`paint > SETTLED_ENERGY`) and its last two frames were identical.
    #[must_use]
    #[inline]
    pub const fn settling(&self) -> bool {
        !self.is_active() && self.paint > SETTLED_ENERGY
    }

    /// When this caret's next visible step is due, so a colour release neither freezes mid-fade nor pins the frame cadence: `Some(now + frame_interval)` while [`Self::is_active`], `None` once nothing is painted, and across a [`Self::settling`] release one frame before the first dry-run frame that differs from the one on glass.
    #[must_use]
    pub fn next_change_deadline(
        &mut self,
        now: Instant,
        frame_interval: Duration,
    ) -> Option<Instant> {
        if self.is_active() {
            return Some(now + frame_interval);
        }
        if !self.settling() {
            return None;
        }
        if frame_interval.is_zero() {
            return Some(now);
        }
        let Some(probe_in) = self.settle_probe else {
            // A painted caret with no latched tick cannot come out of `tick`
            return Some(now + frame_interval);
        };
        let wake = match self.settle_memo {
            Some((interval, wake)) if interval == frame_interval => wake,
            _ => {
                let origin = self.last.unwrap_or(now);
                let wake = self.dry_run_release(probe_in, origin, frame_interval);
                self.settle_memo = Some((frame_interval, wake));
                wake
            }
        };
        Some(match wake {
            SettleWake::At(at) => at.max(now + frame_interval),
            SettleWake::Cadence => now + frame_interval,
        })
    }

    /// The dry run behind [`Self::next_change_deadline`]: tick a copy of this
    /// caret along the frame grid from `origin` until its fingerprint leaves
    /// the one on glass or its release ends.
    fn dry_run_release(
        &mut self,
        probe_in: SettleProbe,
        origin: Instant,
        frame_interval: Duration,
    ) -> SettleWake {
        let steps = ((SETTLE_POLL_CAP.as_secs_f32() / frame_interval.as_secs_f32()) as u32)
            .clamp(1, SETTLE_LOOKAHEAD);
        let mut out = std::mem::take(&mut self.settle_sink);
        let mut probe = CursorRainbow {
            rim_scratch: std::mem::take(&mut self.rim_scratch),
            settle_sink: Vec::new(),
            ..*self
        };
        let mut at = origin;
        let mut wake = None;
        for _ in 0..steps {
            at += frame_interval;
            let ahead = at.saturating_duration_since(origin).as_secs_f32();
            let paint = match (probe_in.paint_prev, probe_in.cfg.paint) {
                (Some(prev), Some(p)) => release_forecast(p, prev, probe_in.sample_dt, ahead),
                (None, Some(p)) if p > SETTLED_ENERGY => None,
                (_, p) => Some(p.unwrap_or(0.0)),
            };
            let energy = release_forecast(
                probe_in.energy,
                probe_in.energy_prev,
                probe_in.sample_dt,
                ahead,
            );
            let (Some(paint), Some(energy)) = (paint, energy) else {
                wake = Some(SettleWake::Cadence);
                break;
            };
            let cfg = RainbowConfig {
                paint: probe_in.cfg.paint.map(|_| paint),
                flare_at: probe_in.cfg.flare_at.filter(|&t| {
                    at.saturating_duration_since(t).as_secs_f32()
                        < crate::rainbow_kitty::timing::SPRING_SNAP_RESPONSE_S
                }),
                ..probe_in.cfg
            };
            out.clear();
            let frame = probe.tick_inner(
                probe_in.cur,
                at,
                energy,
                probe_in.family,
                probe_in.blink_phase,
                probe_in.dark_theme,
                probe_in.geom,
                &cfg,
                &mut out,
            );
            if frame.fp != self.fp_last || probe.is_active() || !probe.settling() {
                wake = Some(SettleWake::At(at - frame_interval));
                break;
            }
        }
        out.clear();
        self.settle_sink = out;
        self.rim_scratch = std::mem::take(&mut probe.rim_scratch);
        wake.unwrap_or(SettleWake::At(at))
    }

    /// Advance one frame at `now` with the typing `energy` (`0..1`), the host's `blink_phase` (unread; kept for seam arity), the block cursor cell `cur` (`None` = hidden), the theme darkness, grid `geom` and `cfg`.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        energy: f32,
        blink_phase: bool,
        dark_theme: bool,
        geom: Geom,
        cfg: &RainbowConfig,
        out: &mut Vec<GlowQuad>,
    ) -> RainbowFrame {
        self.tick_inner(
            cur,
            now,
            energy,
            None,
            blink_phase,
            dark_theme,
            geom,
            cfg,
            out,
        )
    }

    /// [`Self::tick`] phase-locked to the rainbow kitty ribbon's family clock.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn tick_with_family_phase(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        energy: f32,
        family_phase: f32,
        family_field: f32,
        blink_phase: bool,
        dark_theme: bool,
        geom: Geom,
        cfg: &RainbowConfig,
        out: &mut Vec<GlowQuad>,
    ) -> RainbowFrame {
        self.tick_inner(
            cur,
            now,
            energy,
            Some((family_phase, family_field)),
            blink_phase,
            dark_theme,
            geom,
            cfg,
            out,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn tick_inner(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        energy: f32,
        family: Option<(f32, f32)>,
        blink_phase: bool,
        dark_theme: bool,
        geom: Geom,
        cfg: &RainbowConfig,
        out: &mut Vec<GlowQuad>,
    ) -> RainbowFrame {
        let e = (energy.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.energy = 0.0;
            self.paint = 0.0;
            self.last = Some(now);
            self.flare_rung = 0;
            self.twinkle_at = None;
            self.twinkling = false;
            self.flaring = false;
            self.fp_prev = self.fp_last;
            self.fp_last = 0;
            self.settle_probe = None;
            self.settle_memo = None;
            return RainbowFrame { fill: None, fp: 0 };
        }
        let emitted_from = out.len();
        let was_active = self.energy > SETTLED_ENERGY;
        self.energy = e;
        let paint = cfg.paint.map_or(e, |p| {
            (p.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).max(e)
        });
        let was_painted = self.paint > SETTLED_ENERGY;
        self.paint = paint;

        let active = e > SETTLED_ENERGY;
        let since_last = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0);
        let dt = if was_active && active {
            since_last.min(MAX_DT)
        } else {
            0.0
        };
        let painted = paint > SETTLED_ENERGY;
        let dt_spin = if was_painted && painted {
            since_last.min(MAX_DT)
        } else {
            0.0
        };
        self.last = Some(now);
        if active {
            self.phase = (self.phase + dt * (IDLE_SPIN + ACTIVE_SPIN * e)).fract();
            self.pulse = (self.pulse + dt * PULSE_HZ).fract();
        }
        if painted {
            // Wrap on the reflected sweep's own period (2.0,
            self.spin = (self.spin + dt_spin * RIM_SPIN_TURNS_PER_S * paint).rem_euclid(2.0);
        }
        let spectrum_phase = family
            .map(|(phase, _)| phase)
            .unwrap_or_else(|| rainbow_phase_from_unit_turn(self.phase));
        let spectrum_field = family.map(|(_, field)| field);

        if cfg.blinking {
            let rung_reached = MOMENTUM_FLARE_STEPS
                .iter()
                .take_while(|&&step| paint >= step)
                .count() as u8;
            if rung_reached > self.flare_rung {
                self.flare_rung = rung_reached;
                self.twinkle_at = Some(now);
                self.twinkle_seq = self.twinkle_seq.wrapping_add(1);
            } else {
                while self.flare_rung > 0
                    && paint
                        < MOMENTUM_FLARE_STEPS[usize::from(self.flare_rung) - 1]
                            - MOMENTUM_FLARE_HYST
                {
                    self.flare_rung -= 1;
                }
            }
        } else {
            self.flare_rung = 0;
            self.twinkle_at = None;
        }
        let (pop, shimmer) = match self.twinkle_at {
            Some(t0) => {
                let u = now.saturating_duration_since(t0).as_secs_f32() / TWINKLE_DUR;
                if u >= 1.0 {
                    self.twinkle_at = None;
                    (0.0, 0.0)
                } else {
                    let scint = (u * TWINKLE_SCINT + self.twinkle_seq as f32 * 0.37)
                        * std::f32::consts::TAU;
                    ((u * std::f32::consts::PI).sin(), 0.72 + 0.28 * scint.sin())
                }
            }
            None => (0.0, 0.0),
        };
        self.twinkling = self.twinkle_at.is_some();

        let sat = if dark_theme {
            lerp(SAT_IDLE, SAT_MAX, paint)
        } else {
            lerp(SAT_IDLE_LIGHT, SAT_MAX, paint)
        };
        let val = lerp(VAL_IDLE, VAL_MAX, paint);
        let col = cur.map_or(0, |(_, cc)| cc);
        let sweep = spectrum_field.unwrap_or_else(|| rainbow_sweep_at(col, spectrum_phase));
        let head_rgb = cfg.head_rgb.unwrap_or_else(|| spectrum_at(sweep, 0.0));
        let rainbow = shade(head_rgb, sat, val);

        let base = cfg.base.unwrap_or(if dark_theme {
            BASE_DARK_THEME
        } else {
            BASE_LIGHT_THEME
        });
        let (mix_idle, mix_max) = if dark_theme {
            (MIX_IDLE, MIX_MAX)
        } else {
            (MIX_IDLE_LIGHT, MIX_MAX_LIGHT)
        };
        let mut fill = mix_rgb(base, rainbow, lerp(mix_idle, mix_max, paint));
        if let Some(pinned) = cfg.base {
            fill = crate::color_math::wear_pinned(pinned, fill, paint, SETTLED_ENERGY);
        }
        if pop > 0.0 {
            let glint = if dark_theme {
                0x00FF_FFFF
            } else {
                shade(head_rgb, 1.0, 0.85)
            };
            fill = mix_rgb(fill, glint, TWINKLE_MIX * pop * cfg.intensity);
        }
        let caret_floor =
            RAINBOW_CARET_LIGHT_FLOOR * aterm_render::smoothstep01(paint / CARET_LIGHT_KNEE);
        let fill = if dark_theme {
            lift_to_light_floor(fill, caret_floor)
        } else {
            fill
        };
        let flare_age = cfg
            .flare_at
            .map(|t| now.saturating_duration_since(t).as_secs_f32());
        self.flaring =
            flare_age.is_some_and(|age| age < crate::rainbow_kitty::timing::SPRING_SNAP_RESPONSE_S);
        let mut fill = match flare_age {
            None => fill,
            Some(age) => {
                let flash = if dark_theme {
                    0x00FF_FFFF
                } else {
                    shade(head_rgb, 1.0, 0.85)
                };
                if age < FLARE_WHITE_HOLD_S {
                    flash
                } else {
                    mix_rgb(flash, fill, spring_snap(age))
                }
            }
        };

        let breath = 0.5 + 0.5 * (self.pulse * std::f32::consts::TAU).sin();
        let halo_energy = HALO_IDLE_FLOOR * (0.35 + PULSE_DEPTH * breath) + paint;
        let mut flare_pop = 1.0f32;
        let mut rim_key: Option<RimMemoKey> = None;
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && halo_energy > 0.01
        {
            let cw = geom.cw as i32;
            let ch = geom.ch as i32;
            let cx = geom.origin_x as i32 + cc as i32 * cw;
            let cy = geom.origin_y as i32 + cr as i32 * ch;
            flare_pop = flare_age.map_or(1.0, |age| {
                1.0 + FLARE_RING_POP * (-age / FLARE_RING_TAU_S).exp()
            });
            let radius_x =
                (lerp(HALO_RADIUS_IDLE, HALO_RADIUS_MAX, paint) * cw as f32 * flare_pop).max(1.0);
            let radius_y =
                (lerp(HALO_RADIUS_IDLE, HALO_RADIUS_MAX, paint) * ch as f32 * flare_pop).max(1.0);
            let hue_spread = lerp(HALO_HUE_SPREAD_IDLE, HALO_HUE_SPREAD_MAX, paint);
            let ring_sweep = sweep + self.spin;
            let page = cfg.ground.unwrap_or(if dark_theme {
                GROUND_DARK_THEME
            } else {
                GROUND_LIGHT_THEME
            }) & 0x00FF_FFFF;
            let rings_from = out.len();
            let mut rings = [(0usize, 0u8, 0u32, 0u32); HALO_LAYERS as usize];
            let mut gxy = [(0i32, 0i32); HALO_LAYERS as usize];
            let mut n_rings = 0usize;
            let prev_rings = self.rim_memo.filter(|m| m.key.page == page).map(|m| {
                (
                    usize::from(m.key.n_rings).min(HALO_LAYERS as usize),
                    m.key.rings,
                )
            });
            for layer in 0..HALO_LAYERS {
                let t = layer as f32 / (HALO_LAYERS - 1) as f32;
                let gx = (t * radius_x) as i32 + 1;
                let gy = (t * radius_y) as i32 + 1;
                let falloff = (1.0 - t) * (1.0 - t);
                let cov = (HALO_BASE_COV * falloff * halo_energy).min(OVER_INK_COV_CAP) as u8;
                if cov == 0 {
                    continue;
                }
                let ring_arc = if layer == 0 {
                    // The innermost ring touches the ribbon nozzle and wears
                    rainbow
                } else {
                    shade(spectrum_at(ring_sweep, t * hue_spread), sat, val)
                };
                let ring_light = prev_rings
                    .and_then(|(prev_n, prev)| {
                        prev[..prev_n]
                            .iter()
                            .find(|&&(_, pcov, parc, _)| pcov == cov && parc == ring_arc)
                            .map(|&(_, _, _, plight)| plight)
                    })
                    .unwrap_or_else(|| ring_light_on_page(ring_arc, cov, page, 1.0));
                rings[n_rings] = (out.len() - rings_from, cov, ring_arc, ring_light);
                gxy[n_rings] = (gx, gy);
                n_rings += 1;
                push_ring(
                    out,
                    geom,
                    cx - gx,
                    cy - gy,
                    cw + 2 * gx,
                    ch + 2 * gy,
                    ring_light,
                );
            }
            let key = RimMemoKey {
                cur: (cr, cc),
                cw: geom.cw,
                ch: geom.ch,
                rows: geom.rows,
                cols: geom.cols,
                origin_x: geom.origin_x,
                origin_y: geom.origin_y,
                head: geom.head,
                gxy,
                page,
                n_rings: n_rings as u8,
                rings,
            };
            rim_key = Some(key);
            if let Some(memo) = self.rim_memo.filter(|m| m.key == key) {
                if let Some(best) = memo.best {
                    Self::apply_equalised_rings(&mut out[rings_from..], &rings[..n_rings], &best);
                }
            } else {
                let best = self.equalise_rim_light(&mut out[rings_from..], &rings[..n_rings], page);
                self.rim_memo = Some(RimMemo {
                    key,
                    best,
                    peak_dark_no_twinkle: None,
                });
            }
        }

        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && pop > 0.0
        {
            let cw = geom.cw as i32;
            let ch = geom.ch as i32;
            let cx = geom.origin_x as i32 + cc as i32 * cw;
            let cy = geom.origin_y as i32 + cr as i32 * ch;
            let arm_cov =
                (TWINKLE_ARM_COV * pop * shimmer * cfg.intensity).min(OVER_INK_COV_CAP) as u8;
            if arm_cov > 0 {
                let arm_rgb = if dark_theme {
                    0x00FF_FFFF
                } else {
                    shade(head_rgb, 1.0, 0.9)
                };
                let star = premul_rgb(arm_rgb, arm_cov);
                let reach_x = ((TWINKLE_REACH * pop * cw as f32) as i32).max(1);
                let reach_y = ((TWINKLE_REACH * pop * ch as f32) as i32).max(1);
                let th = (ch / 9).max(2);
                push_ring_rect(
                    out,
                    geom,
                    cx - reach_x,
                    cy + (ch - th) / 2,
                    cw + 2 * reach_x,
                    th,
                    star,
                );
                push_ring_rect(
                    out,
                    geom,
                    cx + (cw - th) / 2,
                    cy - reach_y,
                    th,
                    ch + 2 * reach_y,
                    star,
                );
            }
            let dot_cov = (TWINKLE_DOT_COV * pop * pop * cfg.intensity).min(OVER_INK_COV_CAP) as u8;
            if dot_cov > 0 {
                let s = (ch / 8).max(2);
                for k in 0..2u32 {
                    let h = self
                        .twinkle_seq
                        .wrapping_mul(0x9E37_79B9)
                        .wrapping_add(k.wrapping_mul(0x85EB_CA6B));
                    let jit = ((h >> 4) & 1) as i32;
                    let (dx, dy) = match h & 3 {
                        0 => (-s - jit, -s - jit),
                        1 => (cw + jit, -s - jit),
                        2 => (-s - jit, ch + jit),
                        _ => (cw + jit, ch + jit),
                    };
                    let hue = shade(spectrum_at(sweep, 0.13 + k as f32 * 0.29), 0.85, 1.0);
                    push_ring_rect(out, geom, cx + dx, cy + dy, s, s, premul_rgb(hue, dot_cov));
                }
            }
        }

        if dark_theme && caret_floor > 0.0 {
            let rim_peak = if pop == 0.0
                && let Some(key) = rim_key
                && self.rim_memo.is_some_and(|m| m.key == key)
            {
                if let Some(peak) = self.rim_memo.and_then(|m| m.peak_dark_no_twinkle) {
                    peak
                } else {
                    let peak = self.rim_light_peak(&out[emitted_from..], dark_theme);
                    if let Some(m) = self.rim_memo.as_mut() {
                        m.peak_dark_no_twinkle = Some(peak);
                    }
                    peak
                }
            } else {
                self.rim_light_peak(&out[emitted_from..], dark_theme)
            };
            let floor_weight = (caret_floor / RAINBOW_CARET_LIGHT_FLOOR).clamp(0.0, 1.0);
            let promised_margin =
                RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
            fill = lift_to_light_floor(
                fill,
                ((rim_peak + promised_margin) * floor_weight).max(caret_floor),
            );
        }

        let twinkle_fp = if pop > 0.0 {
            (((pop * 255.0) as u64) << 24).wrapping_add(u64::from(self.twinkle_seq) << 40)
        } else {
            0
        };
        let cur_fp = match cur {
            Some((cr, cc)) if (cr as usize) < geom.rows && (cc as usize) < geom.cols => {
                (((u64::from(cr) << 16) | u64::from(cc) | (1 << 32))
                    ^ ((u64::from(geom.origin_x) << 33) | (u64::from(geom.origin_y) << 49))
                    ^ ((geom.cw as u64).rotate_left(21) ^ (geom.ch as u64).rotate_left(37)))
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            }
            _ => 0,
        };
        let spectrum_fp = sweep;
        let fp = ((spectrum_fp * 1024.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(head_rgb).rotate_left(7))
            .wrapping_add((halo_energy * 255.0) as u64)
            .wrapping_add(((fill as u64) << 12) ^ ((self.pulse * 64.0) as u64))
            .wrapping_add(((self.spin * 1024.0) as u64).wrapping_mul(7_919))
            .wrapping_add((((flare_pop - 1.0) * 1024.0) as u64).wrapping_mul(131_071))
            .wrapping_add(twinkle_fp)
            .wrapping_add(cur_fp);
        self.fp_prev = self.fp_last;
        self.fp_last = fp;
        let previous = self.settle_probe;
        self.settle_probe = Some(SettleProbe {
            cur,
            energy,
            energy_prev: previous.map_or(energy, |p| p.energy),
            family,
            blink_phase,
            dark_theme,
            geom,
            cfg: *cfg,
            paint_prev: previous.and_then(|p| p.cfg.paint),
            sample_dt: since_last,
        });
        self.settle_memo = None;

        RainbowFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// The second half of the rim's light law ([`RIM_LIGHT`]): the pile's light
    /// equals the grey pile's.
    #[inline]
    fn apply_equalised_rings(
        quads: &mut [GlowQuad],
        rings: &[(usize, u8, u32, u32)],
        best: &[u32; HALO_LAYERS as usize],
    ) {
        for (i, &(start, _, _, _)) in rings.iter().enumerate() {
            let end = rings
                .get(i + 1)
                .map_or(quads.len(), |&(next, _, _, _)| next);
            for q in &mut quads[start..end] {
                q.color = best[i];
                q.color2 = best[i];
            }
        }
    }

    fn equalise_rim_light(
        &mut self,
        quads: &mut [GlowQuad],
        rings: &[(usize, u8, u32, u32)],
        page: u32,
    ) -> Option<[u32; HALO_LAYERS as usize]> {
        if rings.len() < 2 {
            return None;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for q in quads.iter() {
            x0 = x0.min(u32::from(q.x));
            y0 = y0.min(u32::from(q.y));
            x1 = x1.max(u32::from(q.x) + u32::from(q.w));
            y1 = y1.max(u32::from(q.y) + u32::from(q.h));
        }
        let (w, h) = (
            (x1.saturating_sub(x0)) as usize,
            (y1.saturating_sub(y0)) as usize,
        );
        if w == 0 || h == 0 || w * h > Self::CARET_RASTER_MAX {
            return None;
        }
        let table = crate::color_math::relative_luminance_table();
        let base = light_on_glass(page, 0);
        let mut scratch = std::mem::take(&mut self.rim_scratch);
        let mut pile_light = |quads: &[GlowQuad], colour: &dyn Fn(usize) -> u32| -> f32 {
            scratch.clear();
            scratch.resize(w * h, page);
            let mut aurora = 0.0f32;
            for (i, &(start, _, _, _)) in rings.iter().enumerate() {
                let end = rings
                    .get(i + 1)
                    .map_or(quads.len(), |&(next, _, _, _)| next);
                let c = colour(i);
                let c_light = crate::color_math::relative_luminance_with(table, c);
                for q in &quads[start..end] {
                    let x_off = (u32::from(q.x) - x0) as usize;
                    let qw = usize::from(q.w);
                    for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                        let row_start = (yy - y0) as usize * w + x_off;
                        for px in &mut scratch[row_start..row_start + qw] {
                            *px = crate::spectrum::compose_on_glass(*px, c, q.alpha);
                            aurora += c_light;
                        }
                    }
                }
            }
            scratch
                .iter()
                .map(|&px| {
                    if px == page {
                        0.0
                    } else {
                        (crate::color_math::relative_luminance_with(table, px) - base).max(0.0)
                    }
                })
                .sum::<f32>()
                + aterm_render::hdr::HDR_GLOW_BOOST * aurora
        };
        let grey_rings: [u32; HALO_LAYERS as usize] =
            std::array::from_fn(|i| rings.get(i).map_or(0, |r| premul_rgb(RIM_LIGHT_GREY, r.1)));
        let target = pile_light(quads, &|i| grey_rings[i]);
        let have = pile_light(quads, &|i| rings[i].3);
        if target <= 0.0 || (have - target).abs() <= target * Self::RIM_LIGHT_SLACK {
            self.rim_scratch = scratch;
            return None;
        }
        let solve = |scale: f32| -> [u32; HALO_LAYERS as usize] {
            let mut c = [0u32; HALO_LAYERS as usize];
            for (i, &(_, cov, arc, _)) in rings.iter().enumerate() {
                c[i] = ring_light_on_page(arc, cov, page, scale);
            }
            c
        };
        let (mut lo, mut hi) = (0.5f32, 1.5f32);
        let mut lo_solved: Option<([u32; HALO_LAYERS as usize], f32)> = None;
        let mut hi_solved: Option<([u32; HALO_LAYERS as usize], f32)> = None;
        for _ in 0..8 {
            let mid = 0.5 * (lo + hi);
            let c = solve(mid);
            let l = pile_light(quads, &|i| c[i]);
            if l < target {
                lo = mid;
                lo_solved = Some((c, l));
            } else {
                hi = mid;
                hi_solved = Some((c, l));
            }
        }
        let (a, la) = lo_solved.unwrap_or_else(|| {
            let a = solve(lo);
            let la = pile_light(quads, &|i| a[i]);
            (a, la)
        });
        let (b, lb) = hi_solved.unwrap_or_else(|| {
            let b = solve(hi);
            let lb = pile_light(quads, &|i| b[i]);
            (b, lb)
        });
        let best = if (la - target).abs() <= (lb - target).abs() {
            a
        } else {
            b
        };
        Self::apply_equalised_rings(quads, rings, &best);
        self.rim_scratch = scratch;
        Some(best)
    }

    /// How far the ring-by-ring rim may sit from the grey pile's light before
    /// [`Self::equalise_rim_light`] re-solves it: under the pile's finest byte
    /// step.
    const RIM_LIGHT_SLACK: f32 = 0.005;

    /// The rim's peak light over the field, which §8(d)'s lift of the opaque
    /// block is measured against, in the `0..255` luminance units
    /// [`lift_to_light_floor`] takes.
    fn rim_light_peak(&mut self, quads: &[GlowQuad], dark_theme: bool) -> f32 {
        if quads.is_empty() || !dark_theme {
            return 0.0;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for q in quads {
            x0 = x0.min(u32::from(q.x));
            y0 = y0.min(u32::from(q.y));
            x1 = x1.max(u32::from(q.x) + u32::from(q.w));
            y1 = y1.max(u32::from(q.y) + u32::from(q.h));
        }
        let (w, h) = (
            (x1.saturating_sub(x0)) as usize,
            (y1.saturating_sub(y0)) as usize,
        );
        if w == 0 || h == 0 || w * h > Self::CARET_RASTER_MAX {
            return 255.0;
        }
        let table = crate::color_math::relative_luminance_table();
        let level = RAINBOW_FIELD_LEVEL.round() as u32;
        let field = (level << 16) | (level << 8) | level;
        let field_peak = crate::color_math::relative_luminance_with(table, field) * 255.0;
        let mut scratch = std::mem::take(&mut self.rim_scratch);
        scratch.clear();
        scratch.resize(w * h, field);
        for q in quads {
            let x_off = (u32::from(q.x) - x0) as usize;
            let qw = usize::from(q.w);
            for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                let row_start = (yy - y0) as usize * w + x_off;
                for px in &mut scratch[row_start..row_start + qw] {
                    *px = crate::spectrum::compose_on_glass(*px, q.color, q.alpha);
                }
            }
        }
        let peak = scratch.iter().copied().fold(field_peak, |peak, px| {
            if px == field {
                peak
            } else {
                peak.max(crate::color_math::relative_luminance_with(table, px) * 255.0)
            }
        });
        self.rim_scratch = scratch;
        peak
    }

    /// The largest rim, in pixels, [`Self::rim_light_peak`] lays out. A
    /// backstop: the emitters already bound the rim to one cell plus a hem
    /// ([`HALO_RADIUS_MAX`], [`TWINKLE_REACH`]).
    const CARET_RASTER_MAX: usize = 1 << 16;
}

/// Push one additive halo ring as four thin bars (top/bottom/left/right), each
/// clamped and row-split via [`push_ring_rect`] so every quad is single-row and
/// grid-interior.
#[inline]
fn push_ring(out: &mut Vec<GlowQuad>, geom: Geom, x: i32, y: i32, w: i32, h: i32, premul: u32) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    let th = ((geom.ch as i32) / 8).max(2);
    push_ring_rect(out, geom, x, y, w, th, premul);
    push_ring_rect(out, geom, x, y + h - th, w, th, premul);
    push_ring_rect(out, geom, x, y + th, th, (h - 2 * th).max(0), premul);
    push_ring_rect(
        out,
        geom,
        x + w - th,
        y + th,
        th,
        (h - 2 * th).max(0),
        premul,
    );
}

/// Clamp a pixel rect to the effects box and split it into per-cell-row [`GlowQuad`]s, so the
/// dirty gate and scissor stay exact (the `effect_util::push_fx_rect` contract).
#[inline]
fn push_ring_rect(
    out: &mut Vec<GlowQuad>,
    geom: Geom,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    premul: u32,
) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    let x0 = x.max(geom.fx_left());
    let x1 = (x + w).min(geom.fx_right());
    let y0 = y.max(geom.fx_top());
    let y1 = (y + h).min(geom.fx_bot());
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let ch = geom.ch as i32;
    let oy = geom.origin_y as i32;
    let mut yy = y0;
    while yy < y1 {
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
            alpha: 0,
            color2: premul,
            alpha2: 0,
        });
        yy = band_end;
    }
}

/// The family's spectrum at the caret's column, so the block and the ribbon leaving its cell
/// resolve the same seven anchors.
#[inline]
fn spectrum_at(sweep: f32, off: f32) -> u32 {
    rainbow_thing_of(rainbow_sweep_reflect(sweep + off))
}

/// A family colour re-mixed at saturation `s` and value `v`, hue intact: each channel is pulled
/// toward the peak by `1 − s`, then scaled by `v`.
#[inline]
const fn shade(rgb: u32, s: f32, v: f32) -> u32 {
    if s == 1.0 && v == 1.0 {
        return rgb & 0x00FF_FFFF;
    }
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as f32,
        ((rgb >> 8) & 0xff) as f32,
        (rgb & 0xff) as f32,
    );
    let hi = r.max(g).max(b);
    const fn ch(hi: f32, s: f32, v: f32, c: f32) -> u32 {
        (((hi - s * (hi - c)) * v) + 0.5).clamp(0.0, 255.0) as u32
    }
    (ch(hi, s, v, r) << 16) | (ch(hi, s, v, g) << 8) | ch(hi, s, v, b)
}

/// How much of the energy range the caret's light floor takes to arrive. Short,
/// because the sparkle field competes from the first keystroke; the floor is
/// exactly zero at rest, where the block is the cursor's own colour.
const CARET_LIGHT_KNEE: f32 = 0.25;

/// Lift a colour toward white until its relative luminance reaches `floor`.
pub(crate) fn lift_to_light_floor(rgb: u32, floor: f32) -> u32 {
    let floor = floor.clamp(0.0, 255.0);
    if floor <= 0.0 {
        return rgb;
    }
    let table = crate::color_math::relative_luminance_table();
    let light = |c: u32| crate::color_math::relative_luminance_with(table, c) * 255.0;
    if light(rgb) >= floor {
        return rgb;
    }
    if floor >= 255.0 {
        return 0x00FF_FFFF;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let mid = 0.5 * (lo + hi);
        if light(mix_rgb(rgb, 0x00FF_FFFF, mid)) < floor {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    mix_rgb(rgb, 0x00FF_FFFF, hi)
}

/// The light one additive quad-pixel puts on the glass, in relative luminance
/// over `page`'s own: the functional the rim's light law ([`RIM_LIGHT`])
/// equalises.
#[inline]
fn light_on_glass(page: u32, premul: u32) -> f32 {
    light_on_glass_with(crate::color_math::relative_luminance_table(), page, premul)
}

#[inline]
fn light_on_glass_with(
    table: &crate::color_math::RelativeLuminanceTable,
    page: u32,
    premul: u32,
) -> f32 {
    crate::color_math::relative_luminance_with(
        table,
        crate::spectrum::compose_on_glass(page, premul, 0),
    ) + aterm_render::hdr::HDR_GLOW_BOOST
        * crate::color_math::relative_luminance_with(table, premul)
}

/// One ring's premultiplied light, solved on the page: the rim's light law
/// ([`RIM_LIGHT`]).
fn ring_light_on_page(arc: u32, cov: u8, page: u32, scale: f32) -> u32 {
    debug_assert!(
        (crate::color_math::relative_luminance(RIM_LIGHT_GREY) - RIM_LIGHT).abs() < 0.005,
        "RIM_LIGHT_GREY is not the encode of RIM_LIGHT"
    );
    let table = crate::color_math::relative_luminance_table();
    let light = |premul: u32| -> f32 { light_on_glass_with(table, page, premul) };
    let base = light(0);
    let target = (light(premul_rgb(RIM_LIGHT_GREY, cov)) - base) * scale;
    let candidate = |u: f32| -> u32 {
        if u < 0.0 {
            premul_rgb(mix_rgb(arc, 0x0000_0000, -u), cov)
        } else {
            premul_rgb(mix_rgb(arc, 0x00FF_FFFF, u), cov)
        }
    };
    let (mut lo, mut hi) = (-1.0f32, 1.0f32);
    let mut lo_eval: Option<(u32, f32)> = None;
    let mut hi_eval: Option<(u32, f32)> = None;
    for _ in 0..22 {
        let mid = 0.5 * (lo + hi);
        let c = candidate(mid);
        let added = light(c) - base;
        if added < target {
            lo = mid;
            lo_eval = Some((c, added));
        } else {
            hi = mid;
            hi_eval = Some((c, added));
        }
    }
    let err = |premul: u32| (light(premul) - base - target).abs();
    let (a, err_a) = match lo_eval {
        Some((c, added)) => (c, (added - target).abs()),
        None => {
            let c = candidate(lo);
            (c, err(c))
        }
    };
    let (b, err_b) = match hi_eval {
        Some((c, added)) => (c, (added - target).abs()),
        None => {
            let c = candidate(hi);
            (c, err(c))
        }
    };
    let (mut best, mut best_err) = if err_a <= err_b {
        (a, err_a)
    } else {
        (b, err_b)
    };
    let ch = |c: u32| {
        [
            ((c >> 16) & 0xff) as i32,
            ((c >> 8) & 0xff) as i32,
            (c & 0xff) as i32,
        ]
    };
    let sector_of = |c: [i32; 3]| {
        [
            (c[0] - c[1]).signum(),
            (c[1] - c[2]).signum(),
            (c[0] - c[2]).signum(),
        ]
    };
    let seed = ch(best);
    let sector = sector_of(seed);
    for dr in -1..=1 {
        let r = seed[0] + dr;
        if !(0..=255).contains(&r) {
            continue;
        }
        let r_packed = (r as u32) << 16;
        for dg in -1..=1 {
            let g = seed[1] + dg;
            if !(0..=255).contains(&g) {
                continue;
            }
            let rg_sig = (r - g).signum();
            if rg_sig != 0 && rg_sig != sector[0] {
                continue;
            }
            let rg_packed = r_packed | ((g as u32) << 8);
            for db in -1..=1 {
                if dr == 0 && dg == 0 && db == 0 {
                    continue;
                }
                let b = seed[2] + db;
                if !(0..=255).contains(&b) {
                    continue;
                }
                let gb_sig = (g - b).signum();
                let rb_sig = (r - b).signum();
                if (gb_sig != 0 && gb_sig != sector[1]) || (rb_sig != 0 && rb_sig != sector[2]) {
                    continue;
                }
                let packed = rg_packed | b as u32;
                let e = err(packed);
                if e < best_err {
                    best_err = e;
                    best = packed;
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    fn geom() -> Geom {
        Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: (40 * 8) as u16,
            win_h: (6 * 16) as u16,
            head: 0,
        }
    }
    fn cfg() -> RainbowConfig {
        RainbowConfig {
            enabled: true,
            intensity: 1.0,
            blinking: false,
            base: None,
            head_rgb: None,
            paint: None,
            ground: None,
            flare_at: None,
        }
    }

    /// §7.1: `Some(now)` fills the block white on that frame, and the relaxed
    /// flare is byte-identical to the `None` frame, halo included.
    #[test]
    fn the_flare_is_white_on_frame_zero_and_the_identity_once_settled() {
        let g = geom();
        let t0 = Instant::now();
        let plain = cfg();
        let flared = RainbowConfig {
            flare_at: Some(t0),
            ..plain
        };
        let mut out = Vec::new();
        let f0 = CursorRainbow::default()
            .tick(Some((1, 1)), t0, 1.0, true, true, g, &flared, &mut out)
            .fill
            .unwrap();
        assert_eq!(f0, 0x00FF_FFFF, "frame 0 of the flare is white");
        let late = t0 + Duration::from_secs(2);
        let mut a = Vec::new();
        let mut b = Vec::new();
        let fa = CursorRainbow::default()
            .tick(Some((1, 1)), late, 1.0, true, true, g, &flared, &mut a)
            .fill;
        let fb = CursorRainbow::default()
            .tick(Some((1, 1)), late, 1.0, true, true, g, &plain, &mut b)
            .fill;
        assert_eq!(fa, fb, "a settled flare is the plain fill");
        assert_eq!(a, b, "a settled flare's halo is the plain halo");
    }

    /// §7.1 and D4, the landing's caret law on the real seam: after a v2 meteor's one white
    /// frame the block mixes toward its own field stop (`spectrum(tri(field_t))`) and stays
    /// there for the whole dwell with no colour step, even when the abandoned band retires
    /// mid-dwell.
    #[test]
    fn the_caret_settles_on_its_own_stop_without_a_pop() {
        use crate::cursor_glow::{CursorGlow, GlowConfig, GlowStyle};
        use crate::rainbow_kitty::ribbon::walk_arc;
        use crate::spectrum::spectrum;

        let g = geom();
        let glow_cfg = GlowConfig {
            enabled: true,
            classic_mono: false,
            style: GlowStyle::RainbowKitty,
            color: 0x0050_FA7B,
            accent: 0x007A_A2F7,
            duration: Duration::from_millis(240),
            length: 18,
            intensity: 1.0,
            audible: true,
            radius: 0.6,
            ring: true,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            beam: false,
            head_dx: 0.5,
            ribbon_tall: true,
            ribbon_flat: false,
        };
        fn host_cfg(glow: &CursorGlow, glow_cfg: &GlowConfig, now: Instant) -> RainbowConfig {
            RainbowConfig {
                enabled: true,
                intensity: 1.0,
                blinking: false,
                base: None,
                head_rgb: glow.rainbow_head_rgb(glow_cfg),
                paint: Some(glow.caret_paint(now)),
                ground: Some(glow_cfg.theme_bg),
                flare_at: glow.caret_flare_at(),
            }
        }
        fn frame(
            glow: &mut CursorGlow,
            body: &mut CursorRainbow,
            glow_cfg: &GlowConfig,
            g: Geom,
            now: Instant,
            cell: (u16, u16),
        ) -> u32 {
            let mut glow_out = Vec::new();
            let mut body_out = Vec::new();
            glow.tick(Some(cell), now, glow_cfg, g, &mut glow_out);
            let cfg = host_cfg(glow, glow_cfg, now);
            body.tick_with_family_phase(
                Some(cell),
                now,
                0.0,
                glow.rainbow_phase(),
                glow.rainbow_field(),
                false,
                true,
                g,
                &cfg,
                &mut body_out,
            )
            .fill
            .expect("enabled caret fill")
        }
        let max_delta = |a: u32, b: u32| -> u32 {
            (0..3)
                .map(|s| ((a >> (8 * s)) & 0xff).abs_diff((b >> (8 * s)) & 0xff))
                .max()
                .unwrap_or(0)
        };

        const HZ: u64 = 8_333;
        let t0 = Instant::now();
        let row = 2u16;
        let mut glow = CursorGlow::default();
        let mut body = CursorRainbow::default();
        frame(&mut glow, &mut body, &glow_cfg, g, t0, (row, 4));
        assert!(glow.v2_status().is_some(), "v2 must own the frame");
        let mut last_key = t0;
        for k in 1..=9u64 {
            last_key = t0 + Duration::from_millis(70 * k);
            glow.note_typed(last_key);
            frame(
                &mut glow,
                &mut body,
                &glow_cfg,
                g,
                last_key,
                (row, 4 + k as u16),
            );
        }
        let mut now = last_key;
        let jump_at = last_key + Duration::from_millis(1_200);
        while now + Duration::from_micros(HZ) < jump_at {
            now += Duration::from_micros(HZ);
            frame(&mut glow, &mut body, &glow_cfg, g, now, (row, 13));
        }
        glow.note_motion(jump_at);
        let landing = (row, 33);
        let f0 = frame(&mut glow, &mut body, &glow_cfg, g, jump_at, landing);
        assert_eq!(
            glow.caret_flare_at(),
            Some(jump_at),
            "a credited jump flares on the frame it is observed"
        );
        assert_eq!(f0, 0x00FF_FFFF, "frame 0 of the landing is white");
        let field_t = glow.rainbow_field();
        assert!(
            field_t > 0.25,
            "non-vacuous: the walk laid several stops before the jump (t = {field_t})"
        );
        let own_stop = spectrum(walk_arc(field_t));
        assert_eq!(
            glow.rainbow_head_rgb(&glow_cfg),
            Some(own_stop),
            "on the landing frame the seam's head colour IS the caret's own stop"
        );

        let mut fills = vec![f0];
        let mut now = jump_at;
        let dwell_end = jump_at + Duration::from_millis(400);
        while now + Duration::from_micros(HZ) <= dwell_end {
            now += Duration::from_micros(HZ);
            fills.push(frame(&mut glow, &mut body, &glow_cfg, g, now, landing));
        }

        let mut worst = (0u32, 0usize);
        for (i, pair) in fills.windows(2).enumerate().skip(1) {
            let d = max_delta(pair[0], pair[1]);
            if d > worst.0 {
                worst = (d, i + 1);
            }
        }
        assert!(
            worst.0 <= 40,
            "the caret popped {}/255 on dwell frame {} (+{:.1} ms): {:06X} -> {:06X}",
            worst.0,
            worst.1,
            worst.1 as f32 * HZ as f32 / 1000.0,
            fills[worst.1 - 1],
            fills[worst.1]
        );

        let settled = *fills.last().unwrap();
        let mut law = CursorRainbow::default();
        let mut out = Vec::new();
        let law_cfg = RainbowConfig {
            head_rgb: Some(own_stop),
            ..host_cfg(&glow, &glow_cfg, now)
        };
        let expected = law
            .tick_with_family_phase(
                Some(landing),
                now,
                0.0,
                glow.rainbow_phase(),
                field_t,
                false,
                true,
                g,
                &law_cfg,
                &mut out,
            )
            .fill
            .unwrap();
        assert_eq!(
            settled, expected,
            "settled on {settled:06X}, but the paint law on the caret's own stop \
             {own_stop:06X} (t = {field_t}) gives {expected:06X}"
        );
    }

    /// The continuity ceiling for a walk whose samples are `dt` apart on the
    /// spectrum: the band's derived byte-rate times the step, plus a level for
    /// rounding.
    fn continuity_ceiling(dt: f32) -> u32 {
        (crate::spectrum::spectrum_max_byte_rate() * dt).ceil() as u32 + 1
    }

    /// The same bound for a walk of the block's fill (the arc mixed with the
    /// base and lifted to the light floor), scanned from [`caret_law`] at the
    /// caller's own `dt` so it tracks palette and pace changes.
    fn caret_continuity_ceiling(base: u32, mix: f32, dt: f32) -> u32 {
        const OVERSAMPLE: usize = 8;
        let steps = (1.0 / dt.max(f32::MIN_POSITIVE)).ceil() as usize * OVERSAMPLE;
        let at = |i: usize| {
            caret_law(
                base,
                crate::spectrum::spectrum(i as f32 / steps as f32),
                mix,
                1.0,
            )
        };
        let mut worst = 0u32;
        for i in 0..steps.saturating_sub(OVERSAMPLE) {
            worst = worst.max(rgb_max_delta(at(i), at(i + OVERSAMPLE)));
        }
        worst + 1
    }

    /// The caret's colour law as the tick composes it: what the block emits for a base, rainbow
    /// and mix at energy `e`.
    fn caret_law(base: u32, rainbow: u32, mix: f32, e: f32) -> u32 {
        lift_to_light_floor(
            mix_rgb(base, rainbow, mix),
            RAINBOW_CARET_LIGHT_FLOOR * aterm_render::smoothstep01(e / CARET_LIGHT_KNEE),
        )
    }

    /// The rim-headroom law may only move the composed caret toward white. The
    /// phase and ribbon-head tests care which chromatic ray owns the block, not
    /// how far §8(d) must climb that ray for this frame's ornament.
    fn assert_white_lift_of(actual: u32, source: u32, context: &str) {
        let channels = |rgb: u32| {
            [
                ((rgb >> 16) & 0xff) as f32,
                ((rgb >> 8) & 0xff) as f32,
                (rgb & 0xff) as f32,
            ]
        };
        let from = channels(source);
        let to = channels(actual);
        assert!(
            to.into_iter().zip(from).all(|(to, from)| to >= from),
            "{context}: {actual:#08x} is not a non-darkening lift of {source:#08x}"
        );
        let axis = (0..3)
            .max_by(|&a, &b| (255.0 - from[a]).total_cmp(&(255.0 - from[b])))
            .unwrap_or(0);
        let t = if from[axis] < 255.0 {
            (to[axis] - from[axis]) / (255.0 - from[axis])
        } else {
            0.0
        };
        let rebuilt = mix_rgb(source, 0x00FF_FFFF, t);
        assert!(
            rgb_max_delta(actual, rebuilt) <= 1,
            "{context}: {actual:#08x} left the white-lift ray from {source:#08x} (rebuilt {rebuilt:#08x})"
        );
    }

    fn rgb_max_delta(a: u32, b: u32) -> u32 {
        [16, 8, 0]
            .into_iter()
            .map(|shift| ((a >> shift) & 0xff).abs_diff((b >> shift) & 0xff))
            .max()
            .unwrap_or(0)
    }

    /// The blinking-block variant: the only one whose momentum rungs fire twinkles.
    fn blink_cfg() -> RainbowConfig {
        RainbowConfig {
            blinking: true,
            ..cfg()
        }
    }

    /// Disabled, or reduced motion / load-shed (`intensity == 0`) even at full
    /// strength ⇒ fully inert: no fill, no halo, fp 0, settled —
    /// byte-identical to the plain cursor.
    #[test]
    fn disabled_or_zero_intensity_is_inert() {
        for (why, config) in [
            (
                "disabled",
                RainbowConfig {
                    enabled: false,
                    ..cfg()
                },
            ),
            (
                "zero intensity",
                RainbowConfig {
                    intensity: 0.0,
                    ..cfg()
                },
            ),
        ] {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            let f = cr.tick(
                Some((1, 1)),
                Instant::now(),
                1.0,
                true,
                true,
                geom(),
                &config,
                &mut out,
            );
            assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
            assert_eq!(f.fp, 0, "{why}");
            assert!(out.is_empty(), "{why}: no additive light");
            assert!(!cr.is_active(), "{why}: settled");
        }
    }

    /// A hot rainbow caret rasterizes its rim's light peak every frame. Its
    /// output and raster buffers are resident scratch, so a warmed frame must
    /// not grow either allocation even as the rim hue advances.
    #[test]
    fn active_rim_reuses_its_warmed_scratch() {
        let g = geom();
        let c = cfg();
        let mut cursor = CursorRainbow::default();
        let mut out = Vec::new();
        let mut now = Instant::now();
        for step in 0..64 {
            out.clear();
            now += Duration::from_millis(8);
            let _ = cursor.tick(
                Some((1, (step % 8) as u16)),
                now,
                1.0,
                step % 2 == 0,
                true,
                g,
                &c,
                &mut out,
            );
        }
        let before = (out.capacity(), cursor.rim_scratch.capacity());
        assert!(before.1 > 0, "fixture must exercise the active rim");
        out.clear();
        now += Duration::from_millis(8);
        let _ = cursor.tick(Some((1, 0)), now, 1.0, true, true, g, &c, &mut out);
        assert_eq!(
            before,
            (out.capacity(), cursor.rim_scratch.capacity()),
            "a warmed active rim frame must not grow scratch storage"
        );
    }

    /// The block fill starts near the base (white on dark) at rest and moves markedly
    /// toward a saturated rainbow under full energy — the "white → rainbow" bloom.
    #[test]
    fn fill_blooms_from_base_with_energy() {
        let g = geom();
        let c = cfg();
        let mut idle = CursorRainbow::default();
        let mut out = Vec::new();
        let t = Instant::now();
        let f_idle = idle
            .tick(Some((1, 1)), t, 0.0, true, true, g, &c, &mut out)
            .fill
            .unwrap();
        let minch = |c: u32| {
            [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
                .into_iter()
                .min()
                .unwrap()
        };
        assert!(
            minch(f_idle) > 150,
            "idle block stays near white on dark, got {f_idle:#08x}"
        );
        let mut hot = CursorRainbow::default();
        out.clear();
        let f_hot = hot
            .tick(Some((1, 1)), t, 1.0, true, true, g, &c, &mut out)
            .fill
            .unwrap();
        let spread = |c: u32| {
            let (r, gg, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
            r.max(gg).max(b) - r.min(gg).min(b)
        };
        assert!(
            spread(f_hot) > spread(f_idle) + 40,
            "energy saturates the fill"
        );
        let f_light = CursorRainbow::default()
            .tick(Some((1, 1)), t, 0.0, true, false, g, &c, &mut out)
            .fill
            .unwrap();
        let maxch = [
            (f_light >> 16) & 0xff,
            (f_light >> 8) & 0xff,
            f_light & 0xff,
        ]
        .into_iter()
        .max()
        .unwrap();
        assert!(
            maxch < 90,
            "idle block near black on a light theme, got {f_light:#08x}"
        );
    }

    /// The host's cursor colour is the block, on either theme polarity. The
    /// fill becomes `RenderInput::cursor_fill_override`, which the renderer
    /// applies instead of `frame_cursor(input)`, so a settled caret must be the
    /// host's colour, and two colours never collapse to one block.
    #[test]
    fn host_base_is_the_settled_block_on_either_polarity() {
        let g = geom();
        let mut out = Vec::new();
        let t = Instant::now();
        let settled = |base: u32, dark: bool, out: &mut Vec<GlowQuad>| {
            let c = RainbowConfig {
                base: Some(base),
                ..cfg()
            };
            out.clear();
            CursorRainbow::default()
                .tick(Some((1, 1)), t, 0.0, true, dark, g, &c, out)
                .fill
                .unwrap()
        };
        let chans = |c: u32| ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        for dark in [true, false] {
            let (rr, rg, rb) = chans(settled(0x00FF_0000, dark, &mut out));
            assert!(
                rr > 200 && rg < 110 && rb < 110,
                "a red cursor colour settles red (dark={dark})"
            );
            let (br, bg, bb) = chans(settled(0x0000_00FF, dark, &mut out));
            assert!(
                bb > 200 && br < 110 && bg < 110,
                "a blue cursor colour settles blue (dark={dark})"
            );
            assert_ne!(
                settled(0x00FF_0000, dark, &mut out),
                settled(0x0000_00FF, dark, &mut out),
                "two cursor colours must not paint one identical block (dark={dark})"
            );
        }
    }

    /// A host with real ribbon pixels is authoritative over a merely plausible
    /// phase-derived band.
    #[test]
    fn emitted_ribbon_head_is_the_hot_caret_authority() {
        use crate::cursor_glow::RAINBOW_PHASE_RING;

        let g = geom();
        let now = Instant::now();
        let render = |head_rgb: u32, family_phase: f32| {
            let c = RainbowConfig {
                base: Some(0x0020_2020),
                head_rgb: Some(head_rgb),
                ..cfg()
            };
            let mut out = Vec::new();
            let frame = CursorRainbow::default().tick_with_family_phase(
                Some((1, 7)),
                now,
                1.0,
                family_phase,
                rainbow_sweep_at(7, family_phase),
                false,
                true,
                g,
                &c,
                &mut out,
            );
            (frame, out)
        };

        let (green_a, halo_a) = render(0x0033_FF00, 0.0);
        let (green_b, _) = render(0x0033_FF00, RAINBOW_PHASE_RING * 0.37);
        let (violet, _) = render(0x0066_33FF, 0.0);
        let green_source = caret_law(0x0020_2020, 0x0033_FF00, MIX_MAX, 1.0);
        assert_white_lift_of(
            green_a.fill.expect("green caret fill"),
            green_source,
            "green head at phase A owns the block hue",
        );
        assert_white_lift_of(
            green_b.fill.expect("green caret fill"),
            green_source,
            "green head at phase B owns the block hue",
        );
        assert_white_lift_of(
            violet.fill.expect("violet caret fill"),
            caret_law(0x0020_2020, 0x0066_33FF, MIX_MAX, 1.0),
            "violet head owns the block hue",
        );
        assert_ne!(
            green_a.fill, violet.fill,
            "two emitted head hues must paint two different hot carets"
        );
        let fill = green_a.fill.expect("rainbow block fill");
        let (fr, fg, fb) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
        assert!(
            fg > fr && fg > fb,
            "green emitted head must produce a green-dominant caret: {fill:#08x}"
        );
        assert!(
            halo_a.iter().any(|q| {
                let r = (q.color >> 16) & 0xff;
                let g = (q.color >> 8) & 0xff;
                let b = q.color & 0xff;
                g > r && g > b
            }),
            "the innermost rim must visibly carry the emitted head hue"
        );
    }

    /// The caret's white lift (§8 d) is a dark page's law: on a light page a
    /// full-paint block over a deep violet head keeps the head's hue, under
    /// the lift's floor, for the host's page floor to hold.
    #[test]
    fn a_light_page_caret_is_not_lifted_toward_white() {
        let g = geom();
        let c = RainbowConfig {
            head_rgb: Some(0x0060_20C0),
            ..cfg()
        };
        let fill = |dark: bool| {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            cr.tick(
                Some((1, 1)),
                Instant::now(),
                1.0,
                true,
                dark,
                g,
                &c,
                &mut out,
            )
            .fill
            .expect("a painted rainbow fills the block")
        };
        let floor = RAINBOW_CARET_LIGHT_FLOOR / 255.0;
        let lum = crate::color_math::relative_luminance;
        let (on_dark, on_light) = (fill(true), fill(false));
        assert!(
            lum(on_dark) >= floor - 0.005,
            "control: a dark page lifts the caret to the floor, got {on_dark:06x}"
        );
        assert!(
            lum(on_light) < floor,
            "a light page keeps the hue unlifted, got {on_light:06x}"
        );
        let (_, s, _) = crate::color_math::rgb2hsv(on_light);
        assert!(
            s > 0.7,
            "the light caret stays saturated, got {on_light:06x}"
        );
    }

    /// THE BLOCK OUTSHINES ITS OWN RIM (§8 d) on a dark page: at full paint,
    /// steady and at a flare's peak, wherever the family's sweep parks the
    /// caret, on every shipped cursor colour, the opaque block's light leads
    /// every pixel its rings, arms and dots put on a fully spent sparkle
    /// field ([`RAINBOW_FIELD_LEVEL`]) by the margin the caret's floor
    /// promises over that field (`RAINBOW_CARET_LIGHT_FLOOR · (1 -
    /// RAINBOW_SPARKLE_LIGHT_SHARE)` less two levels of byte rounding, the
    /// brightest-pixel law's margin), painted by the renderer's own glow
    /// path ([`aterm_render::fx::paint_quads`]).
    #[test]
    fn the_block_outshines_its_own_rim_on_a_spent_field() {
        const POSITIONS: u32 = 24;
        let (row, col) = (2u16, 17u16);
        let light = |rgb: u32| crate::color_math::relative_luminance(rgb) * 255.0;
        let level = RAINBOW_FIELD_LEVEL.round() as u32;
        let field_grey = (level << 16) | (level << 8) | level;
        let giant = Geom {
            cw: 160,
            ch: 320,
            win_w: 40 * 160,
            win_h: 6 * 320,
            ..geom()
        };
        let polar = vec![("<none: theme-polar>".to_string(), None)];
        let cases = [
            (geom(), shipped_caret_bases(), POSITIONS, &[false, true][..]),
            (giant, polar, 4, &[false][..]),
        ];
        let promised = RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
        let mut checked = 0usize;
        let mut past_the_floor = 0usize;
        let mut past_the_raster = 0usize;
        for (g, bases, positions, flares) in cases {
            let cell = (
                usize::from(col) * g.cw,
                usize::from(row) * g.ch,
                usize::from(col + 1) * g.cw,
                usize::from(row + 1) * g.ch,
            );
            for (name, base) in bases {
                for step in 0..=positions {
                    let field = step as f32 / positions as f32;
                    for &flare in flares {
                        let c = RainbowConfig {
                            base,
                            paint: Some(1.0),
                            blinking: flare,
                            ..cfg()
                        };
                        let mut caret = CursorRainbow::default();
                        let mut out = Vec::new();
                        let now = Instant::now();
                        let mut tick = |blink: bool, at: Instant, out: &mut Vec<GlowQuad>| {
                            out.clear();
                            caret
                                .tick_with_family_phase(
                                    Some((row, col)),
                                    at,
                                    1.0,
                                    0.0,
                                    field,
                                    blink,
                                    true,
                                    g,
                                    &c,
                                    out,
                                )
                                .fill
                                .expect("an enabled block fills")
                        };
                        let fill = if flare {
                            tick(false, now, &mut out);
                            tick(true, now, &mut out);
                            tick(true, now + Duration::from_millis(70), &mut out)
                        } else {
                            tick(false, now, &mut out)
                        };
                        let x0 = out.iter().map(|q| usize::from(q.x)).min().unwrap_or(0);
                        let y0 = out.iter().map(|q| usize::from(q.y)).min().unwrap_or(0);
                        let x1 = out
                            .iter()
                            .map(|q| usize::from(q.x + q.w))
                            .max()
                            .unwrap_or(0);
                        let y1 = out
                            .iter()
                            .map(|q| usize::from(q.y + q.h))
                            .max()
                            .unwrap_or(0);
                        assert!(x1 > x0 && y1 > y0, "{name}: no rim at full paint");
                        let (w, h) = (x1 - x0, y1 - y0);
                        let moved: Vec<GlowQuad> = out
                            .iter()
                            .map(|q| GlowQuad {
                                x: q.x - x0 as u16,
                                y: q.y - y0 as u16,
                                ..*q
                            })
                            .collect();
                        let mut px = vec![field_grey; w * h];
                        aterm_render::fx::paint_quads(&mut px, w, h, &moved, (0, 0));
                        let mut peak = (0.0f32, 0usize, 0usize, 0u32);
                        for (i, &p) in px.iter().enumerate() {
                            let (x, y) = (x0 + i % w, y0 + i / w);
                            let in_cell =
                                (cell.0..cell.2).contains(&x) && (cell.1..cell.3).contains(&y);
                            if !in_cell && light(p) > peak.0 {
                                peak = (light(p), x, y, p);
                            }
                        }
                        let lead = light(fill) - peak.0;
                        assert!(
                            lead + 1e-3 >= promised,
                            "{name} at a {}x{} cell, sweep {step}/{positions}, flare {flare}: \
                             the block #{fill:06X} (luminance {:.1}) leads its rim's #{:06X} \
                             (luminance {:.1}) at ({}, {}) by {lead:.1}, under the {promised:.0} \
                             the caret's floor promises",
                            g.cw,
                            g.ch,
                            light(fill),
                            peak.3,
                            peak.0,
                            peak.1,
                            peak.2
                        );
                        past_the_floor += usize::from(peak.0 > RAINBOW_CARET_LIGHT_FLOOR);
                        past_the_raster += usize::from(w * h > CursorRainbow::CARET_RASTER_MAX);
                        checked += 1;
                    }
                }
            }
        }
        assert!(
            checked >= 12 * 2 * (POSITIONS as usize + 1) + 5
                && past_the_floor > 0
                && past_the_raster == 5,
            "CONTROL: {past_the_floor} of {checked} rims past the plain floor, \
             {past_the_raster} past the raster"
        );
    }

    /// Every emitted halo and star quad is single-row and inside the grid
    /// interior (the renderer row-gate + parity invariant), additive coverage
    /// is bounded for legibility, and at full energy mid-flare (the widest
    /// reach and brightest light) no quad reaches more than about half a cell
    /// width past the cursor cell.
    #[test]
    fn halo_quads_respect_grid_and_cap() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let gw = (g.cols * g.cw) as u32;
        let gh = (g.rows * g.ch) as u32;
        let cw = g.cw as i32;
        for (row, col) in [(0u16, 0u16), (2, 20)] {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            cr.tick(Some((row, col)), t, 0.1, true, true, g, &c, &mut out);
            let cross = t + Duration::from_millis(16);
            cr.tick(Some((row, col)), cross, 1.0, false, true, g, &c, &mut out);
            out.clear();
            let mid = cross + Duration::from_secs_f32(TWINKLE_DUR / 2.0);
            cr.tick(Some((row, col)), mid, 1.0, false, true, g, &c, &mut out);
            assert!(!out.is_empty(), "a hot cursor glows");
            let (cell_l, cell_r) = (i32::from(col) * cw, (i32::from(col) + 1) * cw);
            let max_reach = cw / 2 + 1;
            for q in &out {
                let band = q.row as u32 * g.ch as u32;
                assert!(
                    q.y as u32 >= band && q.y as u32 + q.h as u32 <= band + g.ch as u32,
                    "single-row: {q:?}"
                );
                assert!(
                    q.x as u32 + q.w as u32 <= gw && q.y as u32 + q.h as u32 <= gh,
                    "in grid: {q:?}"
                );
                for sh in [16, 8, 0] {
                    assert!((q.color >> sh) & 0xff <= 180, "coverage capped: {q:?}");
                }
                assert!(
                    cell_l - q.x as i32 <= max_reach
                        && (q.x as i32 + q.w as i32) - cell_r <= max_reach,
                    "reaches past half a cell: {q:?}"
                );
            }
        }
    }

    /// An upward crossing of a momentum rung while charged fires a twinkle:
    /// star quads land in the scratch and, on a dark theme, the fill glints
    /// toward white against a steady-block twin on the identical clock and
    /// paint (which cannot flare).
    #[test]
    fn momentum_rung_crossing_fires_twinkle_star() {
        let g = geom();
        let t = Instant::now();
        let cross = t + Duration::from_millis(16);
        let mid = cross + Duration::from_secs_f32(TWINKLE_DUR / 2.0);
        let minch = |c: u32| {
            [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
                .into_iter()
                .min()
                .unwrap()
        };
        let run = |c: &RainbowConfig, dark: bool, e: f32| {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            cr.tick(Some((2, 20)), t, 0.1, true, dark, g, c, &mut out);
            let calm_quads = out.len();
            cr.tick(Some((2, 20)), cross, e, true, dark, g, c, &mut out);
            out.clear();
            let fill = cr
                .tick(Some((2, 20)), mid, e, true, dark, g, c, &mut out)
                .fill
                .unwrap();
            (cr.twinkling, fill, out.len(), calm_quads)
        };
        let (twinkling, flared, quads, calm_quads) = run(&blink_cfg(), true, 0.3);
        assert!(twinkling, "a rung crossing arms the flare");
        assert!(
            quads > calm_quads,
            "the flare adds star quads over the idle halo ({quads} vs {calm_quads})"
        );
        let (steady_twinkling, calm, _, _) = run(&cfg(), true, 0.3);
        assert!(!steady_twinkling, "the steady twin never flares");
        assert!(
            minch(flared) >= minch(calm) && flared != calm,
            "mid-flare the dark-theme fill glints toward white ({flared:#08x} vs {calm:#08x})"
        );
        let (_, light, _, _) = run(&blink_cfg(), false, 0.8);
        assert!(
            minch(light) < 160,
            "the light-theme glint keeps saturation (never washes to white), got {light:#08x}"
        );
    }

    /// Only a blinking block's charged rung crossing twinkles. Idle-zero:
    /// phase flips at settled energy never arm the effect timer (the phase is
    /// not read and a resting paint crosses no rung), so twenty half-periods
    /// leave the animator idle.
    #[test]
    fn idle_phase_flips_never_arm_effect_timer() {
        let g = geom();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let mut phase = true;
        cr.tick(Some((1, 1)), t, 0.0, phase, true, g, &blink_cfg(), &mut out);
        for i in 1..=20u64 {
            phase = !phase;
            out.clear();
            cr.tick(
                Some((1, 1)),
                t + Duration::from_millis(530 * i),
                0.0,
                phase,
                true,
                g,
                &blink_cfg(),
                &mut out,
            );
            assert!(!cr.is_active(), "idle flip {i} armed an effect wake");
            assert_eq!(cr.twinkle_seq, 0, "idle flip {i} consumed a flare identity");
        }
        let mut steady = CursorRainbow::default();
        steady.tick(Some((1, 1)), t, 0.0, true, true, g, &cfg(), &mut out);
        steady.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            1.0,
            false,
            true,
            g,
            &cfg(),
            &mut out,
        );
        assert_eq!(
            steady.twinkle_seq, 0,
            "a steady block's wind-up fires no star"
        );
    }

    /// Tier-1: project the real animator's flare generation counter through a
    /// charged-flare → cool → idle-tick trace.
    #[test]
    fn idle_blink_transition_conforms_to_model() {
        let model = aterm_models_effects::rainbow_idle_twinkle_model();
        let state = |charged: i64,
                     twinkle: i64,
                     remaining: i64,
                     flare_seq: i64,
                     idle_restarts: i64,
                     steps: i64| {
            BTreeMap::from([
                ("charged", charged),
                ("twinkle", twinkle),
                ("remaining", remaining),
                ("flare_seq", flare_seq),
                ("idle_restarts", idle_restarts),
                ("steps", steps),
            ])
        };
        let project = |rainbow: &CursorRainbow, now: Instant, idle_restarts: i64, steps: i64| {
            let remaining = rainbow.twinkle_at.map_or(0, |started| {
                let u = now.saturating_duration_since(started).as_secs_f32() / TWINKLE_DUR;
                if u < 0.5 { 2 } else { 1 }
            });
            state(
                i64::from(rainbow.energy > SETTLED_ENERGY),
                i64::from(rainbow.twinkling),
                remaining,
                i64::from(rainbow.twinkle_seq),
                idle_restarts,
                steps,
            )
        };
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();

        let mut rainbow = CursorRainbow::default();
        let mut out = Vec::new();

        let before = project(&rainbow, t, 0, 0);
        rainbow.tick(Some((1, 1)), t, 0.2, true, true, g, &c, &mut out);
        let after = project(&rainbow, t, 0, 1);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &after,
            Some("Charge"),
            "Nyan charge conformance",
        );
        assert!(ok, "shipping charge transition rejected: {why}");

        let before = after;
        rainbow.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.8,
            false,
            true,
            g,
            &c,
            &mut out,
        );
        let after = project(&rainbow, t + Duration::from_millis(16), 0, 2);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &after,
            Some("BlinkCharged"),
            "Nyan charged-blink conformance",
        );
        assert!(ok, "shipping charged-blink transition rejected: {why}");
        assert!(rainbow.twinkling, "the fixture has a live charged flare");
        assert_eq!(rainbow.twinkle_seq, 1, "exactly one flare generation");

        let before = after;
        rainbow.tick(
            Some((1, 1)),
            t + Duration::from_millis(32),
            0.0,
            false,
            true,
            g,
            &c,
            &mut out,
        );
        let after = project(&rainbow, t + Duration::from_millis(32), 0, 3);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before,
            &after,
            Some("Cool"),
            "Nyan cool conformance",
        );
        assert!(ok, "shipping cool transition rejected: {why}");
        assert!(rainbow.twinkling, "cooling preserves the in-flight flare");

        let before_idle = after;
        let seq_before = rainbow.twinkle_seq;
        rainbow.tick(
            Some((1, 1)),
            t + Duration::from_millis(48),
            0.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        let idle_restarts = i64::from(rainbow.twinkle_seq != seq_before);
        let after_idle = project(&rainbow, t + Duration::from_millis(48), idle_restarts, 4);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before_idle,
            &after_idle,
            Some("BlinkIdle"),
            "Nyan active-to-idle blink conformance",
        );
        assert!(ok, "shipping idle-blink transition rejected: {why}");
        assert_eq!(idle_restarts, 0, "an idle blink never restarts a flare");
        assert_eq!(rainbow.twinkle_seq, seq_before);

        let corrupted = state(0, 1, 6, i64::from(seq_before) + 1, 1, 4);
        assert_eq!(
            before_idle["twinkle"], corrupted["twinkle"],
            "the old Boolean-only projection cannot distinguish this restart"
        );
        let (ok, _) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &before_idle,
            &corrupted,
            Some("BlinkIdle"),
            "Nyan idle-restart negative control",
        );
        assert!(!ok, "an idle generation restart must fail conformance");

        let aged_at = t + Duration::from_millis(112);
        rainbow.tick(Some((1, 1)), aged_at, 0.0, true, true, g, &c, &mut out);
        let aged = project(&rainbow, aged_at, 0, 5);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &after_idle,
            &aged,
            Some("Age"),
            "Nyan flare-age conformance",
        );
        assert!(ok, "shipping flare-age transition rejected: {why}");
        assert_eq!(aged["remaining"], 1);

        let finished_at = t + Duration::from_millis(200);
        rainbow.tick(Some((1, 1)), finished_at, 0.0, true, true, g, &c, &mut out);
        let finished = project(&rainbow, finished_at, 0, 6);
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &aged,
            &finished,
            Some("Finish"),
            "Nyan flare-finish conformance",
        );
        assert!(ok, "shipping flare-finish transition rejected: {why}");
        assert_eq!(finished["remaining"], 0);
        assert!(
            !rainbow.is_active(),
            "the bounded flare disarms the host wake"
        );
    }

    /// The flare is bounded: once `TWINKLE_DUR` passes with no further rung
    /// crossing, the animator re-settles and its light is byte-identical to a
    /// twin that never flared.
    #[test]
    fn twinkle_completes_and_resettles() {
        let g = geom();
        let t = Instant::now();
        let step16 = Duration::from_millis(16);
        let on_rung = RainbowConfig {
            paint: Some(0.25),
            ..blink_cfg()
        };
        let under_rung = RainbowConfig {
            paint: Some(0.24),
            ..blink_cfg()
        };
        let at_rest = RainbowConfig {
            paint: Some(0.0),
            ..blink_cfg()
        };
        let mut flared = CursorRainbow::default();
        let mut control = CursorRainbow::default();
        let (mut out_f, mut out_c) = (Vec::new(), Vec::new());
        flared.tick(Some((1, 1)), t, 0.0, true, true, g, &at_rest, &mut out_f);
        control.tick(Some((1, 1)), t, 0.0, true, true, g, &at_rest, &mut out_c);
        flared.tick(
            Some((1, 1)),
            t + step16,
            0.2,
            true,
            true,
            g,
            &on_rung,
            &mut out_f,
        );
        control.tick(
            Some((1, 1)),
            t + step16,
            0.2,
            true,
            true,
            g,
            &under_rung,
            &mut out_c,
        );
        assert!(flared.twinkling && !control.twinkling);
        let after = t + step16 + Duration::from_secs_f32(TWINKLE_DUR + 0.05);
        out_f.clear();
        out_c.clear();
        let ff = flared.tick(
            Some((1, 1)),
            after,
            0.0,
            true,
            true,
            g,
            &at_rest,
            &mut out_f,
        );
        let fc = control.tick(
            Some((1, 1)),
            after,
            0.0,
            true,
            true,
            g,
            &at_rest,
            &mut out_c,
        );
        assert_eq!(flared.flare_rung, 0, "the rung released on the way down");
        assert!(!flared.is_active(), "the flare completes and disarms");
        assert_eq!(out_f, out_c, "no residue: post-flare light == never-flared");
        assert_eq!(ff.fill, fc.fill, "post-flare fill == never-flared");
    }

    /// The halo rides the paint (the ribbon's display spine), not the cadence:
    /// with the cadence at zero a host paint of 0.8 still swells the rim well
    /// past the resting ember, and along a decaying paint the innermost ring's
    /// coverage falls monotonically.
    #[test]
    fn the_halo_rides_the_paint_not_the_ignition_heat() {
        let g = geom();
        let t = Instant::now();
        let ink_and_reach = |paint: Option<f32>| -> (u64, i32) {
            let c = RainbowConfig { paint, ..cfg() };
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            cr.tick(Some((2, 20)), t, 0.0, true, true, g, &c, &mut out);
            let ink = out
                .iter()
                .map(|q| {
                    (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64
                })
                .sum::<u64>();
            let cell_l = 20 * g.cw as i32;
            let reach = out.iter().map(|q| cell_l - q.x as i32).max().unwrap_or(0);
            (ink, reach)
        };
        let (ember_ink, ember_reach) = ink_and_reach(None);
        let (hot_ink, hot_reach) = ink_and_reach(Some(0.8));
        assert!(
            hot_ink > ember_ink * 3,
            "at zero cadence a painted caret still glows ({hot_ink} vs ember {ember_ink})"
        );
        assert!(
            hot_reach > ember_reach,
            "…and its rim reaches further ({hot_reach}px vs ember {ember_reach}px)"
        );

        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let mut last_cov = u32::MAX;
        let mut steps_down = 0;
        for i in 0..200u64 {
            let at = t + Duration::from_millis(i * 16);
            let paint = (-(i as f32 * 0.016) / 0.85).exp();
            let c = RainbowConfig {
                paint: Some(paint),
                ..cfg()
            };
            out.clear();
            cr.tick(Some((2, 20)), at, 0.0, true, true, g, &c, &mut out);
            let cov = out
                .iter()
                .map(|q| {
                    ((q.color >> 16) & 0xff)
                        .max((q.color >> 8) & 0xff)
                        .max(q.color & 0xff)
                })
                .max()
                .unwrap_or(0);
            assert!(
                cov <= last_cov,
                "frame {i}: the halo brightened on a falling paint ({cov} > {last_cov})"
            );
            if cov < last_cov {
                steps_down += 1;
            }
            last_cov = cov;
        }
        assert!(
            steps_down >= 6,
            "the glow bleeds off in visible steps, not one ({steps_down})"
        );
        assert!(
            !cr.is_active(),
            "and at the end of the release the caret rests"
        );
    }

    /// Momentum rungs are edge-triggered with hysteresis: one flare per upward
    /// crossing, none while hovering or falling, one (not four) when a single
    /// tick crosses every rung, and a rung re-arms only under its hysteresis
    /// band.
    #[test]
    fn momentum_rungs_fire_once_each_and_never_on_the_way_down() {
        let g = geom();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let mut i = 0u64;
        let mut tick = |cr: &mut CursorRainbow, paint: f32| {
            i += 1;
            let c = RainbowConfig {
                paint: Some(paint),
                ..blink_cfg()
            };
            cr.tick(
                Some((1, 1)),
                t + Duration::from_millis(i * 16),
                0.0,
                true,
                true,
                g,
                &c,
                &mut out,
            );
            cr.twinkle_seq
        };
        assert_eq!(tick(&mut cr, 0.0), 0, "at rest nothing fires");
        assert_eq!(tick(&mut cr, 0.24), 0, "under the first rung nothing fires");
        assert_eq!(tick(&mut cr, 0.25), 1, "reaching the first rung fires once");
        assert_eq!(tick(&mut cr, 0.26), 1, "hovering above it does not re-fire");
        assert_eq!(
            tick(&mut cr, 0.22),
            1,
            "dipping inside the hysteresis band does not release"
        );
        assert_eq!(
            tick(&mut cr, 0.25),
            1,
            "…so climbing back onto it does not re-fire"
        );
        assert_eq!(tick(&mut cr, 0.55), 2, "the second rung fires once");
        assert_eq!(
            tick(&mut cr, 0.10),
            2,
            "falling through every rung fires nothing"
        );
        assert_eq!(cr.flare_rung, 0, "…and releases them all");
        assert_eq!(
            tick(&mut cr, 1.0),
            3,
            "a one-tick jump to full paint fires ONE flare"
        );
        assert_eq!(cr.flare_rung, 4, "…holding the top rung");
        assert_eq!(tick(&mut cr, 0.0), 3, "a full stop fires nothing");
        assert_eq!(tick(&mut cr, 0.0), 3, "and rest stays silent");
    }

    /// The momentum spin turns only while painted and is constant at rest:
    /// once the paint is zero, two further ticks emit the same fingerprint and
    /// bytes, and the caret reports settled.
    #[test]
    fn the_rim_turns_with_momentum_and_freezes_at_rest() {
        let g = geom();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let hot = RainbowConfig {
            paint: Some(1.0),
            ..cfg()
        };
        let rest = RainbowConfig {
            paint: Some(0.0),
            ..cfg()
        };
        for i in 0..30u64 {
            cr.tick(
                Some((1, 1)),
                t + Duration::from_millis(i * 16),
                0.0,
                true,
                true,
                g,
                &hot,
                &mut out,
            );
        }
        let turned = cr.spin;
        assert!(
            turned > 0.1,
            "half a second at full paint turns the rim ({turned} turns)"
        );
        let expect = RIM_SPIN_TURNS_PER_S * 29.0 * 0.016;
        assert!((turned - expect).abs() < 1e-3, "{turned} vs {expect}");
        out.clear();
        let a = cr.tick(
            Some((1, 1)),
            t + Duration::from_secs(2),
            0.0,
            true,
            true,
            g,
            &rest,
            &mut out,
        );
        let quads_a = out.clone();
        out.clear();
        let b = cr.tick(
            Some((1, 1)),
            t + Duration::from_secs(4),
            0.0,
            true,
            true,
            g,
            &rest,
            &mut out,
        );
        assert_eq!(
            cr.spin, turned,
            "the offset is frozen at rest, not integrated"
        );
        assert_eq!(a.fp, b.fp, "a resting rim's fingerprint is constant");
        assert_eq!(quads_a, out, "…and so are its bytes");
        assert!(!cr.is_active(), "the fingerprint law releases the tick");
    }

    /// The spin never snaps: the accumulator wraps on the reflected sweep's
    /// period (2.0), so at constant full paint every ring colour moves per
    /// frame by no more than the continuity ceiling for a step of
    /// `RIM_SPIN_TURNS_PER_S · 16 ms`, through 5 s of spin crossing 1.0 and
    /// 2.0.
    #[test]
    fn spin_is_continuous_across_its_wrap() {
        let g = geom();
        let t = Instant::now();
        let hot = RainbowConfig {
            paint: Some(1.0),
            ..cfg()
        };
        let mut cr = CursorRainbow::default();
        let mut prev: Option<Vec<u32>> = None;
        let mut out = Vec::new();
        let ceiling = continuity_ceiling(RIM_SPIN_TURNS_PER_S * 0.016) + 2;
        let mut worst = 0u32;
        let mut wrapped = false;
        for i in 0..320u64 {
            out.clear();
            let before = cr.spin;
            cr.tick(
                Some((2, 20)),
                t + Duration::from_millis(i * 16),
                0.0,
                true,
                true,
                g,
                &hot,
                &mut out,
            );
            assert!(
                (0.0..2.0).contains(&cr.spin),
                "spin left the sweep's period: {}",
                cr.spin
            );
            wrapped |= cr.spin < before;
            let colours: Vec<u32> = out.iter().map(|q| q.color).collect();
            if let Some(p) = &prev
                && p.len() == colours.len()
            {
                for (a, b) in p.iter().zip(&colours) {
                    let d = rgb_max_delta(*a, *b);
                    worst = worst.max(d);
                    assert!(
                        d <= ceiling,
                        "frame {i}: a ring stepped one channel by {d} (ceiling {ceiling}) \
                         at spin {} — {a:#08x} -> {b:#08x}",
                        cr.spin
                    );
                }
            }
            prev = Some(colours);
        }
        assert!(
            cr.spin > 0.0 && wrapped,
            "the walk must cross the wrap to prove it"
        );
        assert!(worst > 0, "…and the rim must actually turn");
    }

    /// The shared-phase path closes on the ribbon's 1024-unit ring, checked on
    /// a complete emitted frame (fill, halo bytes, fingerprint); the
    /// quarter-sweep control shows the clock still animates.
    #[test]
    fn shared_phase_complete_frame_is_exact_across_the_family_ring() {
        use crate::cursor_glow::RAINBOW_PHASE_RING;

        let g = geom();
        let c = cfg();
        let now = Instant::now();
        let render = |phase: f32| {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            let frame = cr.tick_with_family_phase(
                Some((2, 11)),
                now,
                1.0,
                phase,
                rainbow_sweep_at(11, phase),
                false,
                true,
                g,
                &c,
                &mut out,
            );
            (frame.fill, frame.fp, out)
        };

        assert_eq!(render(0.0), render(RAINBOW_PHASE_RING));
        assert_ne!(
            render(0.0),
            render(rainbow_phase_from_unit_turn(0.25)),
            "a quarter-sweep must change a complete caret frame"
        );
    }

    /// The standalone fallback's phase sweep has no full-colour jumps on the
    /// block itself: a dense real-tick sample crosses every anchor and the
    /// reflected endpoint within the continuity ceiling.
    #[test]
    fn fallback_fill_flows_through_the_anchors_without_temporal_hue_steps() {
        let g = geom();
        let c = cfg();
        let now = Instant::now();
        let mut previous = None;
        let mut max_step = 0;
        let mut colours = BTreeSet::new();
        for sample in 0..=2400 {
            let phase = sample as f32 * (6.0 / 2400.0);
            let mut cursor = CursorRainbow::default();
            let mut out = Vec::new();
            let fill = cursor
                .tick_with_family_phase(
                    Some((2, 17)),
                    now,
                    1.0,
                    phase,
                    rainbow_sweep_at(17, phase),
                    false,
                    true,
                    g,
                    &c,
                    &mut out,
                )
                .fill
                .expect("enabled block fill");
            if let Some(prior) = previous {
                max_step = max_step.max(rgb_max_delta(prior, fill));
            }
            previous = Some(fill);
            colours.insert(fill);
        }
        let dt = (rainbow_sweep_at(17, 6.0 / 2400.0) - rainbow_sweep_at(17, 0.0)).abs();
        let ceiling = caret_continuity_ceiling(BASE_DARK_THEME, MIX_MAX, dt);
        assert!(
            max_step <= ceiling,
            "a 2.5 ms phase step changed one fill channel by {max_step} \
             (ceiling {ceiling})"
        );
        assert!(
            colours.len() > 256,
            "continuity must not mean a frozen/six-colour cursor: {} colours",
            colours.len()
        );
    }

    /// The rim's light is a function of the paint alone ([`RIM_LIGHT`]): for every shipped caret
    /// base, both shipped dark pages and five paints, the rim rasterized over the page at 129
    /// sweep positions agrees within `RIM_LIGHT_TOLERANCE` and rises strictly with paint.
    #[test]
    fn the_rim_light_is_hue_invariant_and_monotone_in_paint() {
        /// Ratio `max/min` of the pile's light across the sweep. The budget is
        /// the premultiply's rounding: the two bracketing candidates are one
        /// byte apart on the inner ring, so the nearest pile sits within about
        /// ±2 % of the grey's.
        const RIM_LIGHT_TOLERANCE: f64 = 1.05;
        const RIM_LIGHT_TOLERANCE_IDLE: f64 = 1.25;
        const GROUNDS: [u32; 2] = [0x0011_1318, 0x001A_1B26];
        const PAINTS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
        const POSITIONS: u32 = 128;
        let lum = |rgb: u32| f64::from(crate::color_math::relative_luminance(rgb));
        let raster_light = |quads: &[GlowQuad], ground: u32| -> f64 {
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
            for q in quads {
                x0 = x0.min(u32::from(q.x));
                y0 = y0.min(u32::from(q.y));
                x1 = x1.max(u32::from(q.x) + u32::from(q.w));
                y1 = y1.max(u32::from(q.y) + u32::from(q.h));
            }
            if x1 <= x0 || y1 <= y0 {
                return 0.0;
            }
            let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
            let mut px = vec![ground; w * h];
            let mut aurora = 0.0f64;
            for q in quads {
                for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                    for xx in u32::from(q.x)..u32::from(q.x) + u32::from(q.w) {
                        let i = (yy - y0) as usize * w + (xx - x0) as usize;
                        px[i] = crate::spectrum::compose_on_glass(px[i], q.color, q.alpha);
                        aurora += lum(q.color);
                    }
                }
            }
            let floor = lum(ground);
            let bytes: f64 = px.iter().map(|&p| (lum(p) - floor).max(0.0)).sum();
            bytes + f64::from(aterm_render::hdr::HDR_GLOW_BOOST) * aurora
        };

        let g = geom();
        let now = Instant::now();
        let mut worst_spread = [1.0f64; PAINTS.len()];
        let mut worst_at: [String; PAINTS.len()] = Default::default();
        for (name, base) in shipped_caret_bases() {
            for ground in GROUNDS {
                let mut light = vec![[0.0f64; PAINTS.len()]; POSITIONS as usize + 1];
                for (pi, &paint) in PAINTS.iter().enumerate() {
                    let c = RainbowConfig {
                        base,
                        paint: Some(paint),
                        blinking: false,
                        ground: Some(ground),
                        ..cfg()
                    };
                    for step in 0..=POSITIONS {
                        let field = step as f32 / POSITIONS as f32;
                        let mut cursor = CursorRainbow::default();
                        let mut out = Vec::new();
                        cursor.tick_with_family_phase(
                            Some((2, 17)),
                            now,
                            0.0,
                            0.0,
                            field,
                            false,
                            true,
                            g,
                            &c,
                            &mut out,
                        );
                        light[step as usize][pi] = raster_light(&out, ground);
                    }
                }
                for (pi, &paint) in PAINTS.iter().enumerate() {
                    let col: Vec<f64> = light.iter().map(|row| row[pi]).collect();
                    let lo = col.iter().copied().fold(f64::INFINITY, f64::min);
                    let hi = col.iter().copied().fold(0.0, f64::max);
                    assert!(
                        lo > 0.0,
                        "{name} over #{ground:06X} at paint {paint}: no rim light"
                    );
                    let spread = hi / lo;
                    if spread > worst_spread[pi] {
                        worst_spread[pi] = spread;
                        worst_at[pi] = format!("{name} over #{ground:06X} {lo:.4}..{hi:.4}");
                    }
                }
                for (step, row) in light.iter().enumerate() {
                    for pi in 1..PAINTS.len() {
                        assert!(
                            row[pi] > row[pi - 1],
                            "{name} over #{ground:06X} at sweep {}/{POSITIONS}: the rim's \
                             light did not rise with the paint {} -> {}: {:.4} -> {:.4}",
                            step,
                            PAINTS[pi - 1],
                            PAINTS[pi],
                            row[pi - 1],
                            row[pi]
                        );
                    }
                }
            }
        }
        for (pi, &paint) in PAINTS.iter().enumerate() {
            let tolerance = if paint > 0.0 {
                RIM_LIGHT_TOLERANCE
            } else {
                RIM_LIGHT_TOLERANCE_IDLE
            };
            assert!(
                worst_spread[pi] <= tolerance,
                "at paint {paint} the rim's light depends on where the spin parked it: \
                 ×{:.3} at {} (tolerance ×{tolerance})",
                worst_spread[pi],
                worst_at[pi]
            );
        }

        let mut lo = f64::INFINITY;
        let mut hi = 0.0f64;
        for step in 0..=POSITIONS {
            let sweep = step as f32 / POSITIONS as f32;
            let l = lum(shade(spectrum_at(sweep, 0.0), SAT_MAX, VAL_MAX));
            lo = lo.min(l);
            hi = hi.max(l);
        }
        assert!(
            hi / lo > 10.0,
            "the un-normalised arc spans only ×{:.2} in light — this gate cannot \
             see the defect it exists for",
            hi / lo
        );
        let mid = crate::spectrum::spectrum_crossing_position();
        let half = crate::spectrum::spectrum_crossing_span() * 0.5;
        let flank_lo = lum(shade(
            crate::spectrum::spectrum(mid - half),
            SAT_MAX,
            VAL_MAX,
        ));
        let flank_hi = lum(shade(
            crate::spectrum::spectrum(mid + half),
            SAT_MAX,
            VAL_MAX,
        ));
        let (dim, bright) = (flank_lo.min(flank_hi), flank_lo.max(flank_hi));
        assert!(
            bright / dim > 2.0,
            "the raw arc's light steps only ×{:.2} across the crossing — the incident \
             this law answers for could not have happened on it",
            bright / dim
        );
    }

    /// The caret cools with its trail, not with the ignition heat: after a 43-key burst the
    /// ribbon's momentum spine is still high while `TypingCadence::intensity` is zero, and a
    /// caret riding the cadence would drop to its idle mix beside a painted ribbon.
    #[test]
    fn the_caret_cools_with_its_trail_and_not_with_the_ignition_heat() {
        use crate::cursor_trail::TypingCadence;
        use crate::typing_momentum::TypingMomentum;

        /// The shipped Default theme's `cursor_color`, which is what
        /// `app_render` hands the block as `base`.
        const BASE: u32 = 0x0050_FA7B;
        /// The ribbon's laid field at the caret.
        const FIELD: f32 = 0.05;
        /// The burst: 43 characters at about 92 ms/key.
        const KEYS: u32 = 43;
        const GAP: Duration = Duration::from_millis(92);

        let g = geom();
        let t0 = Instant::now();
        let mut cadence = TypingCadence::default();
        let mut spine = TypingMomentum::default();
        let mut last_key = t0;
        for k in 0..KEYS {
            last_key = t0 + GAP * k;
            cadence.on_keystroke(last_key);
            spine.advance(last_key);
        }

        let arc = spectrum_at(FIELD, 0.0);
        let render = |at: Instant, paint: Option<f32>| -> (u32, CursorRainbow) {
            let c = RainbowConfig {
                base: Some(BASE),
                head_rgb: Some(arc),
                paint,
                ..cfg()
            };
            let mut cursor = CursorRainbow::default();
            let mut out = Vec::new();
            let fill = cursor
                .tick_with_family_phase(
                    Some((2, 17)),
                    at,
                    cadence.intensity(at),
                    0.0,
                    FIELD,
                    false,
                    true,
                    g,
                    &c,
                    &mut out,
                )
                .fill
                .expect("enabled block fill");
            (fill, cursor)
        };

        let quarter = last_key + Duration::from_millis(250);
        assert_eq!(
            cadence.intensity(quarter),
            0.0,
            "the ignition heat must really be dead a quarter second after a burst \
             — if it is not, this test is not exercising the defect"
        );
        assert!(
            spine.value(quarter) > 0.8,
            "…while the ribbon's own spine is still all but full: {}",
            spine.value(quarter)
        );

        let (old, _) = render(quarter, None);
        assert!(
            rgb_max_delta(old, BASE) < 32,
            "the OLD law must still collapse to the cursor colour (#{old:06X} vs \
             #{BASE:06X}) — the control has stopped controlling"
        );
        assert!(
            rgb_max_delta(old, arc) > 3 * rgb_max_delta(old, BASE),
            "…and it must be nowhere near the arc it is leading (#{arc:06X})"
        );

        let (new, state) = render(quarter, Some(spine.value(quarter)));
        assert!(
            rgb_max_delta(new, arc) < rgb_max_delta(new, BASE),
            "the caret must wear its trail's colour while the trail is painted: \
             #{new:06X} is {} from the arc #{arc:06X} and {} from the base \
             #{BASE:06X}",
            rgb_max_delta(new, arc),
            rgb_max_delta(new, BASE)
        );
        assert!(
            rgb_max_delta(new, old) > 64,
            "…and that must be a REAL change from the shipped caret, not a nudge"
        );
        assert!(
            state.is_active(),
            "a painted caret must keep the host's tick armed, or it freezes \
             mid-cool and snaps to the base on the next unrelated frame"
        );

        const STEP: Duration = Duration::from_millis(16);
        let colour_span =
            ((MIX_MAX - MIX_IDLE) + (SAT_MAX - SAT_IDLE) + (VAL_MAX - VAL_IDLE)) * 255.0;
        let ceiling_for = |dpaint: f32| (colour_span * dpaint.abs()).ceil() as u32 + 4;
        let mut previous: Option<(u32, u32, f32)> = None;
        let mut worst_step = 0u32;
        let mut cooled = 0usize;
        let mut frames = 0usize;
        for frame in 0..=190u32 {
            let at = last_key + STEP * frame;
            let paint = spine.value(at);
            let (fill, _) = render(at, Some(paint));
            let toward_base = rgb_max_delta(fill, BASE);
            if paint >= CARET_LIGHT_KNEE
                && let Some((prev_fill, prev_base_delta, prev_paint)) = previous
            {
                let step = rgb_max_delta(prev_fill, fill);
                let ceiling = ceiling_for(paint - prev_paint);
                worst_step = worst_step.max(step);
                assert!(
                    step <= ceiling,
                    "frame {frame}: the cooling caret jumped one channel by \
                     {step} (ceiling {ceiling}) — #{prev_fill:06X} -> #{fill:06X}"
                );
                assert!(
                    toward_base <= prev_base_delta + 2,
                    "frame {frame}: the caret warmed back up mid-release \
                     ({prev_base_delta} -> {toward_base})"
                );
                cooled += usize::from(toward_base < prev_base_delta);
            }
            frames += 1;
            previous = Some((fill, toward_base, paint));
        }
        assert!(
            frames > 180 && cooled > 40,
            "the release walk must actually move: {cooled} cooling steps over \
             {frames} frames"
        );
        assert!(
            worst_step > 0,
            "…and it must not be a frozen block: worst channel step {worst_step}"
        );

        let long = last_key + Duration::from_secs(20);
        let (settled, settled_state) = render(long, Some(spine.value(long)));
        assert_eq!(
            spine.value(long),
            0.0,
            "the spine must be provably at rest before the settled claim"
        );
        assert_eq!(
            settled,
            render(long, None).0,
            "a dead spine must leave the caret exactly where the energy law puts \
             it — the block IS the cursor at rest"
        );
        assert!(
            !settled_state.is_active(),
            "…and the host's tick must disarm again"
        );
    }

    /// The bases the product hands the block: `app_render` passes
    /// `base: Some(live_cursor_rgb)`, so the domain is every built-in theme's
    /// resolved cursor colour, read from [`aterm_types::scheme`] through the
    /// host's `to_theme_parts` projection (new themes are swept
    /// automatically).
    fn shipped_caret_bases() -> Vec<(String, Option<u32>)> {
        let mut bases: Vec<(String, Option<u32>)> = vec![
            ("<none: theme-polar>".to_string(), None),
            ("<white>".to_string(), Some(0x00FF_FFFF)),
            ("<near-black>".to_string(), Some(0x0016_161C)),
        ];
        for name in aterm_types::scheme::builtin_names() {
            let scheme = aterm_types::scheme::builtin(name)
                .unwrap_or_else(|| panic!("built-in theme {name} must resolve"));
            bases.push((name.to_string(), Some(scheme.to_theme_parts().cursor)));
        }
        assert!(
            bases.len() >= 12,
            "the shipped theme roster must be enumerated, not empty: {bases:?}"
        );
        bases
    }

    /// Real engine-to-engine lock over more than a second of typed advances:
    /// `CursorGlow` owns the ribbon clock and `CursorRainbow` consumes its
    /// sample after the same frame's tick, so every hot fill is the base mixed
    /// toward the continuous family colour at that cell throughout motion.
    #[test]
    fn caret_and_ribbon_share_one_live_clock_through_a_streaming_run() {
        use crate::cursor_glow::{CursorGlow, GlowConfig, GlowStyle};

        let g = geom();
        let glow_cfg = GlowConfig {
            enabled: true,
            classic_mono: false,
            style: GlowStyle::RainbowKitty,
            color: 0x0050_FA7B,
            accent: 0x007A_A2F7,
            duration: Duration::from_millis(240),
            length: 18,
            intensity: 1.0,
            audible: true,
            radius: 0.6,
            ring: true,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            beam: false,
            head_dx: 0.5,
            ribbon_tall: true,
            ribbon_flat: false,
        };
        let body_cfg = cfg();
        let t0 = Instant::now();
        let row = 2u16;
        let mut glow = CursorGlow::default();
        let mut body = CursorRainbow::default();
        let mut glow_out = Vec::new();
        let mut body_out = Vec::new();
        glow.tick(Some((row, 0)), t0, &glow_cfg, g, &mut glow_out);

        let mut phases = Vec::new();
        let mut colours = BTreeSet::new();
        for key in 0..18u64 {
            let press_at = t0 + Duration::from_millis(key * 72 + 1);
            let frame_at = press_at + Duration::from_millis(8);
            let col = (key + 1) as u16;
            glow.note_typed(press_at);
            glow.tick(Some((row, col)), frame_at, &glow_cfg, g, &mut glow_out);
            let family_phase = glow.rainbow_phase();
            phases.push(family_phase);

            body_out.clear();
            let family_field = glow.rainbow_field();
            let body_frame = body.tick_with_family_phase(
                Some((row, col)),
                frame_at,
                1.0,
                family_phase,
                family_field,
                false,
                true,
                g,
                &body_cfg,
                &mut body_out,
            );
            let ribbon_colour = spectrum_at(family_field, 0.0);
            colours.insert(ribbon_colour);
            assert_white_lift_of(
                body_frame.fill.expect("enabled caret fill"),
                caret_law(BASE_DARK_THEME, ribbon_colour, MIX_MAX, 1.0),
                &format!("key {key}: caret and ribbon diverged at phase {family_phase}, col {col}"),
            );
        }

        assert!(
            Duration::from_millis(17 * 72 + 9) > Duration::from_secs(1),
            "fixture must exercise more than one second of live engine time"
        );
        assert!(
            phases.last().copied().unwrap_or(0.0) > phases[1] + 0.05,
            "the real ribbon clock must advance non-vacuously: {phases:?}"
        );
        assert!(
            colours.len() >= 5,
            "the streaming run must traverse the unroll's colours: {colours:#x?}"
        );
    }

    /// The settle split is a partition: [`CursorRainbow::settling`] is disjoint
    /// from [`CursorRainbow::is_active`], and together they are exactly
    /// `energy > SETTLED || paint > SETTLED || twinkling || flaring` for every
    /// latched combination, with the last two frames equal or not.
    #[test]
    fn the_settle_split_is_a_partition_of_still_moving() {
        let levels = [0.0_f32, SETTLED_ENERGY, SETTLED_ENERGY + 1e-4, 0.3, 1.0];
        for &energy in &levels {
            for &paint in &levels {
                for twinkling in [false, true] {
                    for flaring in [false, true] {
                        for repeated in [false, true] {
                            let rb = CursorRainbow {
                                energy,
                                paint,
                                twinkling,
                                flaring,
                                fp_last: 7,
                                fp_prev: if repeated { 7 } else { 8 },
                                ..CursorRainbow::default()
                            };
                            let moving = energy > SETTLED_ENERGY
                                || paint > SETTLED_ENERGY
                                || twinkling
                                || flaring;
                            let at = format!(
                                "energy={energy} paint={paint} twinkling={twinkling} \
                                 flaring={flaring} repeated={repeated}"
                            );
                            assert_eq!(rb.is_active() || rb.settling(), moving, "cover: {at}");
                            assert!(!(rb.is_active() && rb.settling()), "disjoint: {at}");
                        }
                    }
                }
            }
        }
    }

    /// The host spine integrated at 1 ms from a burst that reached `peak`: the
    /// momentum metric decaying on its 2.0 s tau, chased by the ribbon's
    /// display ease (0.85 s on a fall), snapping at 0.005.
    fn host_paint_spine(peak: f32) -> Vec<f32> {
        let mut disp = peak;
        let mut out = vec![disp];
        for ms in 1..=20_000_u32 {
            let target = peak * (-(ms as f32 / 1000.0) / 2.0).exp();
            let rise = target - disp;
            let ease = if rise > 0.0 { 0.10 } else { 0.85 };
            disp += rise * (1.0 - (-0.001_f32 / ease).exp());
            if disp < 0.005 {
                disp = 0.0;
            }
            out.push(disp);
        }
        out
    }

    /// The colour release is drawn to its last step, on demand. The caret keeps cooling on the
    /// paint spine one u8 step every few frames, so two identical frames arrive long before the
    /// fade ends.
    #[test]
    fn the_caret_colour_release_is_drawn_to_its_last_step_on_demand() {
        const HEAD: u32 = 0x00FF_5A4E;
        let hands: [(f32, u64, bool, Option<u32>, bool); 6] = [
            (1.00, 8, true, Some(0x0050_FA7B), true),
            (1.00, 16, true, Some(0x0050_FA7B), true),
            (0.45, 8, true, Some(0x0050_FA7B), false),
            (1.00, 8, false, Some(0x0016_161C), true),
            (0.70, 16, true, None, false),
            (1.00, 33, true, Some(0x0050_FA7B), true),
        ];
        for (hand, &(peak, step_ms, dark, base, blinking)) in hands.iter().enumerate() {
            let frame = Duration::from_millis(step_ms);
            let spine = host_paint_spine(peak);
            let t0 = Instant::now();
            let cur = Some((2_u16, 5_u16));
            let cfg_at = |ms: u64| RainbowConfig {
                enabled: true,
                intensity: 1.0,
                blinking,
                base,
                head_rgb: Some(HEAD),
                paint: Some(spine[(ms as usize).min(spine.len() - 1)]),
                ground: None,
                flare_at: None,
            };
            let energy_at = |ms: u64| peak * 0.5_f32.powf(ms as f32 / 1000.0 / 0.22);
            let tick = |rb: &mut CursorRainbow, ms: u64, out: &mut Vec<GlowQuad>| {
                out.clear();
                rb.tick_with_family_phase(
                    cur,
                    t0 + Duration::from_millis(ms),
                    energy_at(ms),
                    412.0,
                    0.37,
                    false,
                    dark,
                    geom(),
                    &cfg_at(ms),
                    out,
                )
                .fp
            };
            let mut out = Vec::new();

            let mut level = CursorRainbow::default();
            let mut level_fp: BTreeMap<u64, u64> = BTreeMap::new();
            let mut ms = 0_u64;
            let level_release = loop {
                level_fp.insert(ms, tick(&mut level, ms, &mut out));
                if !level.is_active() && !level.settling() {
                    break ms;
                }
                ms += step_ms;
                assert!(ms < 30_000, "hand {hand}: the level twin never released");
            };
            let changes: BTreeSet<u64> = level_fp
                .iter()
                .zip(level_fp.iter().skip(1))
                .filter(|((_, a), (_, b))| a != b)
                .map(|(_, (at, _))| *at)
                .collect();
            assert!(
                changes.len() > 30,
                "hand {hand}: {} visible steps is not the release under test",
                changes.len()
            );

            let mut parked = CursorRainbow::default();
            let mut ms = 0_u64;
            loop {
                tick(&mut parked, ms, &mut out);
                if !parked.is_active() {
                    break;
                }
                ms += step_ms;
            }
            let parked_at = ms;
            let never_drawn = changes.iter().filter(|&&at| at > parked_at).count();
            assert!(
                parked.settling() && parked_at < level_release && never_drawn > 0,
                "hand {hand}: negative control — the fingerprint law alone parked at \
                 {parked_at} ms (settling {}), level release {level_release} ms, {never_drawn} \
                 steps never drawn",
                parked.settling()
            );

            let mut demand = CursorRainbow::default();
            let mut demand_ticks = 0_usize;
            let mut ms = 0_u64;
            let demand_release = loop {
                let fp = tick(&mut demand, ms, &mut out);
                demand_ticks += 1;
                let now = t0 + Duration::from_millis(ms);
                let next = if demand.is_active() {
                    Some(now + frame)
                } else {
                    demand.next_change_deadline(now, frame)
                };
                let Some(next) = next else {
                    break ms;
                };
                let step = next.saturating_duration_since(now).as_millis() as u64;
                assert!(
                    step > 0 && step.is_multiple_of(step_ms),
                    "hand {hand}: a deadline lands on the frame grid (+{step} ms at {ms} ms)"
                );
                assert!(
                    step <= SETTLE_POLL_CAP.as_millis() as u64,
                    "hand {hand}: a deadline stays inside the poll cap (+{step} ms)"
                );
                let mut fork = demand.clone();
                let mut k = ms + step_ms;
                while k < ms + step {
                    let forked = tick(&mut fork, k, &mut out);
                    assert_eq!(
                        forked,
                        fp,
                        "hand {hand}: the demand lane slept through a visible step at {k} ms \
                         (woke {ms} ms, next {} ms)",
                        ms + step
                    );
                    k += step_ms;
                }
                ms += step;
                assert!(ms < 30_000, "hand {hand}: the demand twin never released");
            };
            assert!(
                !demand.is_active() && !demand.settling(),
                "hand {hand}: the demand lane parks only on the caret's own release"
            );
            assert_eq!(
                demand_release, level_release,
                "hand {hand}: the demand lane releases on the level lane's frame"
            );
            assert!(
                demand_ticks <= level_fp.len(),
                "hand {hand}: the demand lane may never cost more than the frame train"
            );
        }
    }

    /// The dry run is memoised per real tick and borrows the caret's own
    /// scratch: planning twice between two ticks answers the same instant, a
    /// real tick clears the memo, and an inert tick drops the latched inputs.
    #[test]
    fn the_release_deadline_is_one_dry_run_per_tick() {
        let spine = host_paint_spine(1.0);
        let t0 = Instant::now();
        let mut rb = CursorRainbow::default();
        let mut out = Vec::new();
        let frame = Duration::from_millis(16);
        let cfg_at = |ms: u64| RainbowConfig {
            paint: Some(spine[ms as usize]),
            ..cfg()
        };
        let mut ms = 0_u64;
        while !rb.settling() {
            out.clear();
            rb.tick(
                Some((2, 5)),
                t0 + Duration::from_millis(ms),
                0.0,
                false,
                true,
                geom(),
                &cfg_at(ms),
                &mut out,
            );
            ms += 16;
            assert!(ms < 10_000, "fixture: the caret never settled");
        }
        let now = t0 + Duration::from_millis(ms - 16);
        let first = rb
            .next_change_deadline(now, frame)
            .expect("a settling caret owns a wake");
        assert_eq!(
            rb.next_change_deadline(now + Duration::from_millis(1), frame),
            Some(first.max(now + Duration::from_millis(1) + frame)),
            "a second plan before the next tick reads the memo"
        );
        out.clear();
        rb.tick(
            Some((2, 5)),
            first,
            0.0,
            false,
            true,
            geom(),
            &cfg_at(ms),
            &mut out,
        );
        out.clear();
        rb.tick(
            Some((2, 5)),
            first + frame,
            0.0,
            false,
            true,
            geom(),
            &RainbowConfig {
                enabled: false,
                ..cfg_at(ms)
            },
            &mut out,
        );
        assert!(
            !rb.settling() && rb.next_change_deadline(first + frame, frame).is_none(),
            "an inert tick drops the latched inputs and the release with them"
        );
    }

    #[test]
    fn flare_pop_drives_fingerprint_on_a_pinned_white_cursor() {
        let t0 = Instant::now();
        let wide_geom = Geom {
            cw: 93,
            ch: 186,
            rows: 12,
            cols: 40,
            win_w: 3720,
            win_h: 2232,
            ..geom()
        };
        let mut out_0 = Vec::new();
        let f_0 = CursorRainbow::default().tick(
            Some((2, 5)),
            t0,
            0.0,
            false,
            true,
            wide_geom,
            &RainbowConfig {
                paint: Some(1.0),
                flare_at: Some(t0),
                ..cfg()
            },
            &mut out_0,
        );
        let mut out_3ms = Vec::new();
        let f_3ms = CursorRainbow::default().tick(
            Some((2, 5)),
            t0,
            0.0,
            false,
            true,
            wide_geom,
            &RainbowConfig {
                paint: Some(1.0),
                flare_at: Some(t0 - Duration::from_millis(3)),
                ..cfg()
            },
            &mut out_3ms,
        );
        assert_eq!(
            f_0.fill, f_3ms.fill,
            "fixture: white hold keeps fill identical across the first 3 ms of the flare"
        );
        assert_ne!(
            out_0, out_3ms,
            "fixture: flare_pop decay moves the halo rings during the white hold"
        );
        assert_ne!(
            f_0.fp, f_3ms.fp,
            "the flare ring pop must change the frame fingerprint even when fill and phase are identical"
        );
    }

    #[test]
    fn meteor_flare_and_relight_hold_keep_caret_active_and_wake_for_decay() {
        let t0 = Instant::now();
        let frame = Duration::from_millis(20);
        let mut rb = CursorRainbow::default();
        let mut out = Vec::new();
        let steady_cfg = |paint: f32, flare_at: Option<Instant>| RainbowConfig {
            blinking: false,
            paint: Some(paint),
            flare_at,
            ..cfg()
        };

        for step in 0..9_u64 {
            let at = t0 + Duration::from_millis(step * 20);
            out.clear();
            rb.tick_with_family_phase(
                Some((2, 5)),
                at,
                0.0,
                256.0,
                0.25,
                false,
                true,
                geom(),
                &steady_cfg(1.0, Some(t0)),
                &mut out,
            );
            assert!(
                rb.is_active() && !rb.settling(),
                "step {step} ({} ms): caret must stay active while flare_at is within 180 ms",
                step * 20
            );
        }

        let boundary = t0 + Duration::from_millis(180);
        out.clear();
        rb.tick_with_family_phase(
            Some((2, 5)),
            boundary,
            0.0,
            256.0,
            0.25,
            false,
            true,
            geom(),
            &steady_cfg(1.0, None),
            &mut out,
        );
        assert_eq!(
            rb.next_change_deadline(boundary, frame),
            Some(boundary + frame),
            "flat live paint plateau at the end of the 180 ms relight hold must wake on the next frame"
        );
    }

    #[test]
    fn cursor_cell_and_visibility_drive_fp_and_rim_memo_skips_repeat_rasters() {
        let t0 = Instant::now();
        let g = geom();
        let c = RainbowConfig {
            blinking: false,
            paint: Some(0.8),
            ..cfg()
        };
        let mut rb = CursorRainbow::default();
        let mut out_a = Vec::new();
        let mut out_b = Vec::new();

        let fa = rb.tick_with_family_phase(
            Some((2, 5)),
            t0,
            0.0,
            128.0,
            0.25,
            false,
            true,
            g,
            &c,
            &mut out_a,
        );
        let fb = rb.tick_with_family_phase(
            Some((3, 5)),
            t0,
            0.0,
            128.0,
            0.25,
            false,
            true,
            g,
            &c,
            &mut out_b,
        );
        assert_eq!(fa.fill, fb.fill);
        assert_ne!(out_a, out_b, "halo rings move with the cursor row");
        assert_ne!(
            fa.fp, fb.fp,
            "a cursor row move must change RainbowFrame.fp"
        );

        let mut rest_rb = CursorRainbow::default();
        out_a.clear();
        let f_vis = rest_rb.tick(Some((0, 0)), t0, 0.0, false, true, g, &cfg(), &mut out_a);
        out_b.clear();
        let f_hid = rest_rb.tick(None, t0, 0.0, false, true, g, &cfg(), &mut out_b);
        assert!(!out_a.is_empty() && out_b.is_empty());
        assert_ne!(
            f_vis.fp, f_hid.fp,
            "hiding the cursor must change RainbowFrame.fp"
        );
        assert!(
            !rest_rb.is_active(),
            "a resting cursor visibility change does not arm the animation timer"
        );

        let mut memo_rb = CursorRainbow::default();
        out_a.clear();
        let first = memo_rb.tick_with_family_phase(
            Some((2, 5)),
            t0,
            0.0,
            128.0,
            0.25,
            false,
            true,
            g,
            &c,
            &mut out_a,
        );
        let first_quads = out_a.clone();
        memo_rb.rim_scratch = Vec::new();
        out_b.clear();
        let second = memo_rb.tick_with_family_phase(
            Some((2, 5)),
            t0,
            0.0,
            128.0,
            0.25,
            false,
            true,
            g,
            &c,
            &mut out_b,
        );
        assert_eq!(
            memo_rb.rim_scratch.capacity(),
            0,
            "rim_memo hit must skip both equalise_rim_light and rim_light_peak rasterizations"
        );
        assert_eq!((first.fill, first.fp), (second.fill, second.fp));
        assert_eq!(first_quads, out_b);
    }
}
