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
use crate::effect_util::{lerp, lerp_rgb as mix_rgb};
use crate::rainbow_kitty::timing::spring_snap;

/// Base colour the rainbow blooms from when the host names none.
const BASE_DARK_THEME: u32 = 0x00FF_FFFF;
const BASE_LIGHT_THEME: u32 = 0x0016_161C;

/// Default background colour of each theme polarity (`#111318` dark, `#FDF6E3` light).
const GROUND_DARK_THEME: u32 = 0x0011_1318;
const GROUND_LIGHT_THEME: u32 = 0x00FD_F6E3;

/// Hue rotation in turns/second: baseline while charged, plus peak spin.
const IDLE_SPIN: f32 = 0.05;
const ACTIVE_SPIN: f32 = 1.05;

/// Idle breath (turns/sec of the halo pulse) and its depth.
const PULSE_HZ: f32 = 0.34;
const PULSE_DEPTH: f32 = 0.55;

/// Saturation and value ramps from idle ember to peak typing bloom.
const SAT_IDLE: f32 = 0.32;
const SAT_IDLE_LIGHT: f32 = 0.88;
const SAT_MAX: f32 = 1.0;
const VAL_IDLE: f32 = 0.82;
const VAL_MAX: f32 = 1.0;

/// Block fill tint mix from the base toward the live rainbow.
const MIX_IDLE: f32 = 0.16;
const MIX_MAX: f32 = 0.82;
const MIX_IDLE_LIGHT: f32 = MIX_IDLE;
const MIX_MAX_LIGHT: f32 = 0.95;

/// Concentric additive halo rings with quadratic coverage falloff.
const HALO_LAYERS: i32 = 4;
const HALO_RADIUS_IDLE: f32 = 0.06;
const HALO_RADIUS_MAX: f32 = 0.22;
const HALO_BASE_COV: f32 = 28.0;
/// Minimum idle halo brightness factor before breath modulation.
const HALO_IDLE_FLOOR: f32 = 0.30;
/// Hue spread across the halo rings in turns.
const HALO_HUE_SPREAD_IDLE: f32 = 0.10;
const HALO_HUE_SPREAD_MAX: f32 = 0.34;
/// Momentum spin in turns per second at full paint.
const RIM_SPIN_TURNS_PER_S: f32 = 0.45;

/// Target relative luminance (`0..1`) of a reference grey ring for hue-invariant rim light.
const RIM_LIGHT: f32 = 0.462;
/// Reference grey of relative luminance [`RIM_LIGHT`] (sRGB byte `181`).
const RIM_LIGHT_GREY: u32 = 0x00B5_B5B5;
const _: () = assert!(
    (crate::color_math::relative_luminance_with(
        crate::color_math::relative_luminance_table(),
        RIM_LIGHT_GREY,
    ) - RIM_LIGHT)
        .abs()
        < 0.005,
    "RIM_LIGHT_GREY is not the encode of RIM_LIGHT"
);

/// Energy threshold below which the cursor reports inactive.
const SETTLED_ENERGY: f32 = 0.02;

/// Momentum rungs on upward crossings of which the caret sparkles.
const MOMENTUM_FLARE_STEPS: [f32; 4] = [0.25, 0.50, 0.75, 1.00];
/// Hysteresis margin below a rung before it re-arms.
const MOMENTUM_FLARE_HYST: f32 = 0.06;
/// Flare duration in seconds.
const TWINKLE_DUR: f32 = 0.16;
/// White hold duration on a v2 meteor's birth frame.
const FLARE_WHITE_HOLD_S: f32 = 0.004;
/// Halo ring radius pop factor on a flare edge.
const FLARE_RING_POP: f32 = 0.4;
/// Halo ring pop relaxation time constant in seconds.
const FLARE_RING_TAU_S: f32 = 0.120;
/// Block fill glint mix toward the star colour at the flare peak.
const TWINKLE_MIX: f32 = 0.35;
/// Star-arm overhang past the block edge, as a fraction of the cell's own axis.
const TWINKLE_REACH: f32 = 0.20;
/// Peak additive coverage of the star arms and glitter dots, bounded by [`OVER_INK_COV_CAP`].
const TWINKLE_ARM_COV: f32 = 44.0;
const TWINKLE_DOT_COV: f32 = 38.0;
/// Scintillation cycles across one flare.
const TWINKLE_SCINT: f32 = 0.45;
const _: () = assert!(
    TWINKLE_SCINT / TWINKLE_DUR <= crate::effect_util::TWINKLE_FLASH_BOUND_HZ,
    "twinkle scintillation over the WCAG 2.3.1 general-flash bound"
);
const _: () = assert!(TWINKLE_SCINT > 0.0, "the flare must still scintillate");

/// Per-tick dt clamp in seconds across a charged interval.
const MAX_DT: f32 = 0.6;

/// Maximum frame steps [`CursorRainbow::next_change_deadline`] dry-runs.
const SETTLE_LOOKAHEAD: u32 = 24;
/// Coarsest colour-release sampling interval.
const SETTLE_POLL_CAP: Duration = Duration::from_millis(250);

/// Latched inputs of the last real tick for [`CursorRainbow::next_change_deadline`].
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
    /// Wall seconds between those two ticks.
    sample_dt: f32,
}

/// Memoised result of a colour-release dry run.
#[derive(Clone, Copy)]
enum SettleWake {
    /// Wake one frame before the first frame that differs from glass, or at the probe horizon.
    At(Instant),
    /// Host envelope is rising; stay on frame cadence.
    Cadence,
}

/// Input key for [`CursorRainbow::equalise_rim_light`] and [`CursorRainbow::rim_light_peak`].
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

/// Extrapolate a released envelope `ahead` seconds past its last sample pair.
#[inline]
const fn release_forecast(now_v: f32, prev_v: f32, dt: f32, ahead: f32) -> Option<f32> {
    if now_v <= 0.0 {
        return Some(0.0);
    }
    if prev_v < now_v {
        return None;
    }
    if dt <= 0.0 || prev_v <= now_v {
        return if now_v <= SETTLED_ENERGY {
            Some(now_v)
        } else {
            None
        };
    }
    let ratio = (now_v / prev_v).clamp(1e-3, 1.0);
    Some(now_v * crate::color_math::powf_pos_f32(ratio, ahead / dt))
}

/// Resolved per-frame inputs.
#[derive(Clone, Copy, Debug)]
pub struct RainbowConfig {
    /// Master on/off when the rainbow cursor is enabled on a focused, visible block.
    pub enabled: bool,
    /// Reduced-motion / load-shed amplitude (`0..1`).
    pub intensity: f32,
    /// The terminal reports a blinking block.
    pub blinking: bool,
    /// Resting block colour (`0x00RRGGBB`).
    pub base: Option<u32>,
    /// Ribbon-head colour `CursorGlow` emitted this frame (`0x00RRGGBB`).
    pub head_rgb: Option<u32>,
    /// Trail paint spine (`0..1`).
    pub paint: Option<f32>,
    /// Resolved terminal background colour (`0x00RRGGBB`).
    pub ground: Option<u32>,
    /// Instant a v2 meteor's frame-0 flare fired.
    pub flare_at: Option<Instant>,
    /// The frame's light (`docs/design/PRISM-GLASS.md` §15.2 1.3, `GlowConfig::light`), which
    /// the settle probe latches with the rest of the config and the state words leave unhashed,
    /// as they leave the glow config's. Off on every dark ground; nothing reads it yet.
    pub light: crate::cursor_glow::Light,
}

/// The dormant config: off at no intensity, steady, with no base, head, paint, ground, flare or
/// light.
/// A test names the fields it is about and takes the rest from here, so a field added later
/// takes its default here and no test literal has to name it (§15.0.6).
impl Default for RainbowConfig {
    #[inline]
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 0.0,
            blinking: false,
            base: None,
            head_rgb: None,
            paint: None,
            ground: None,
            flare_at: None,
            light: crate::cursor_glow::Light::OFF,
        }
    }
}

/// Tick output: the block fill colour and a frame fingerprint (`0` when dormant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RainbowFrame {
    /// Block-fill colour (`0x00RRGGBB`), or `None` when the rainbow cursor is off.
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + halo (0 ⇒ nothing to show this frame).
    pub fp: u64,
}

impl RainbowFrame {
    /// Dormant frame output (`fill: None, fp: 0`).
    pub const DORMANT: Self = Self { fill: None, fp: 0 };
}

impl Default for RainbowFrame {
    #[inline]
    fn default() -> Self {
        Self::DORMANT
    }
}

/// Per-window rainbow-cursor animation state.
#[derive(Clone)]
pub struct CursorRainbow {
    /// Standalone spin in unit turns (`0..1`).
    phase: f32,
    /// Idle-breath phase in turns `0..1`.
    pulse: f32,
    last: Option<Instant>,
    /// Latched energy at the last tick.
    energy: f32,
    /// Latched colour envelope at the last tick.
    paint: f32,
    /// Highest momentum rung crossed upward and not yet released.
    flare_rung: u8,
    /// Start of the in-flight twinkle flare (`None` between flares).
    twinkle_at: Option<Instant>,
    /// Flare counter used for deterministic variation and fingerprinting.
    twinkle_seq: u32,
    /// The momentum spin's accumulated offset along the family sweep, in turns.
    spin: f32,
    /// Latched indicator that a twinkle flare is mid-flight.
    twinkling: bool,
    /// Latched indicator that a v2 meteor flare (`cfg.flare_at`) is mid-flight.
    flaring: bool,
    /// Fingerprints of the last two ticks.
    fp_last: u64,
    fp_prev: u64,
    /// Scratch pixel buffer for [`Self::rim_light_peak`].
    rim_scratch: Vec<u32>,
    /// Memoised [`Self::equalise_rim_light`] and no-twinkle [`Self::rim_light_peak`].
    rim_memo: Option<RimMemo>,
    /// Latched inputs from the last real tick for the colour release dry run.
    settle_probe: Option<SettleProbe>,
    /// Memoised dry-run answer for the frame interval.
    settle_memo: Option<(Duration, SettleWake)>,
    /// Retained quad sink for dry-run probes.
    settle_sink: Vec<GlowQuad>,
}

impl Default for CursorRainbow {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CursorRainbow {
    /// The light the settle probe latched with its config, `None` with no probe latched: for
    /// the host's dark pin (§15.0.3). Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn settle_light(&self) -> Option<crate::cursor_glow::Light> {
        match &self.settle_probe {
            Some(probe) => Some(probe.cfg.light),
            None => None,
        }
    }

    /// Fresh dormant rainbow-cursor animation state.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: 0.0,
            pulse: 0.0,
            last: None,
            energy: 0.0,
            paint: 0.0,
            flare_rung: 0,
            twinkle_at: None,
            twinkle_seq: 0,
            spin: 0.0,
            twinkling: false,
            flaring: false,
            fp_last: 0,
            fp_prev: 0,
            rim_scratch: Vec::new(),
            rim_memo: None,
            settle_probe: None,
            settle_memo: None,
            settle_sink: Vec::new(),
        }
    }

    /// Reset animation state while preserving pooled scratch buffers and the rim LUT memo.
    #[inline]
    pub fn reset(&mut self) {
        let mut rim_scratch = std::mem::take(&mut self.rim_scratch);
        rim_scratch.clear();
        let mut settle_sink = std::mem::take(&mut self.settle_sink);
        settle_sink.clear();
        let rim_memo = self.rim_memo;
        *self = Self {
            rim_scratch,
            rim_memo,
            settle_sink,
            ..Self::new()
        };
    }

    /// Whether the host must keep arming the animation tick.
    #[must_use]
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.energy > SETTLED_ENERGY
            || self.twinkling
            || self.flaring
            || (self.paint > SETTLED_ENERGY && self.fp_last != self.fp_prev)
    }

    /// Whether the caret is not [`Self::is_active`] but still painted (`paint > SETTLED_ENERGY`).
    #[must_use]
    #[inline]
    pub const fn settling(&self) -> bool {
        !self.is_active() && self.paint > SETTLED_ENERGY
    }

    /// Returns when this caret's next visible step is due.
    #[must_use]
    #[inline]
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

    /// Tick a copy of this caret along the frame grid from `origin` until its fingerprint changes or release ends.
    #[inline]
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
        let mut step_idx = 0;
        while step_idx < steps {
            step_idx += 1;
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
                paint: if probe_in.cfg.paint.is_some() {
                    Some(paint)
                } else {
                    None
                },
                flare_at: if let Some(t) = probe_in.cfg.flare_at
                    && at.saturating_duration_since(t).as_secs_f32()
                        < crate::rainbow_kitty::timing::SPRING_SNAP_RESPONSE_S
                {
                    Some(t)
                } else {
                    None
                },
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

    /// Advance one frame at `now` with typing `energy` (`0..1`), cursor cell `cur`, theme polarity, grid `geom`, and `cfg`.
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
    #[inline]
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
        let paint = match cfg.paint {
            Some(p) => (p.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).max(e),
            None => e,
        };
        let was_painted = self.paint > SETTLED_ENERGY;
        self.paint = paint;

        let active = e > SETTLED_ENERGY;
        let since_last = match self.last {
            Some(t) => now.saturating_duration_since(t).as_secs_f32(),
            None => 0.0,
        };
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
            let next_spin = self.spin + dt_spin * RIM_SPIN_TURNS_PER_S * paint;
            self.spin = if (0.0..2.0).contains(&next_spin) {
                next_spin
            } else {
                next_spin - (next_spin * 0.5).floor() * 2.0
            };
        }
        let (spectrum_phase, spectrum_field) = match family {
            Some((phase, field)) => (phase, Some(field)),
            None => (rainbow_phase_from_unit_turn(self.phase), None),
        };

        if cfg.blinking {
            let mut rung_reached = 0u8;
            let mut si = 0;
            while si < MOMENTUM_FLARE_STEPS.len() {
                if paint >= MOMENTUM_FLARE_STEPS[si] {
                    rung_reached += 1;
                } else {
                    break;
                }
                si += 1;
            }
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
                const INV_TWINKLE_DUR: f32 = 1.0 / TWINKLE_DUR;
                let u = now.saturating_duration_since(t0).as_secs_f32() * INV_TWINKLE_DUR;
                if u >= 1.0 {
                    self.twinkle_at = None;
                    (0.0, 0.0)
                } else {
                    let scint = (u * TWINKLE_SCINT + self.twinkle_seq as f32 * 0.37)
                        * std::f32::consts::TAU;
                    (
                        crate::supernova::sin_cos_f32(u * std::f32::consts::PI).0,
                        0.72 + 0.28 * crate::supernova::sin_cos_f32(scint).0,
                    )
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
        let col = match cur {
            Some((_, cc)) => cc,
            None => 0,
        };
        let sweep = match spectrum_field {
            Some(s) => s,
            None => rainbow_sweep_at(col, spectrum_phase),
        };
        let head_rgb = match cfg.head_rgb {
            Some(rgb) => rgb,
            None => spectrum_at(sweep, 0.0),
        };
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
        let caret_floor = if dark_theme && paint > 0.0 {
            const INV_CARET_LIGHT_KNEE: f32 = 1.0 / CARET_LIGHT_KNEE;
            RAINBOW_CARET_LIGHT_FLOOR * aterm_render::smoothstep01(paint * INV_CARET_LIGHT_KNEE)
        } else {
            0.0
        };
        let fill = if caret_floor > 0.0 {
            lift_to_light_floor(fill, caret_floor)
        } else {
            fill
        };
        let flare_age = cfg
            .flare_at
            .map(|t| now.saturating_duration_since(t).as_secs_f32());
        self.flaring = matches!(
            flare_age,
            Some(age) if age < crate::rainbow_kitty::timing::SPRING_SNAP_RESPONSE_S
        );
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

        let breath = if self.pulse == 0.0 {
            0.5
        } else {
            0.5 + 0.5 * crate::supernova::sin_cos_f32(self.pulse * std::f32::consts::TAU).0
        };
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
            flare_pop = match flare_age {
                Some(age) if age < 20.0 * FLARE_RING_TAU_S => {
                    const INV_FLARE_RING_TAU_S: f32 = 1.0 / FLARE_RING_TAU_S;
                    1.0 + FLARE_RING_POP * crate::color_math::exp_f32(-age * INV_FLARE_RING_TAU_S)
                }
                _ => 1.0,
            };
            let halo_r = lerp(HALO_RADIUS_IDLE, HALO_RADIUS_MAX, paint);
            let radius_x = (halo_r * cw as f32 * flare_pop).max(1.0);
            let radius_y = (halo_r * ch as f32 * flare_pop).max(1.0);
            let hue_spread = lerp(HALO_HUE_SPREAD_IDLE, HALO_HUE_SPREAD_MAX, paint);
            let ring_sweep = sweep + self.spin;
            let page = cfg.ground.unwrap_or(if dark_theme {
                GROUND_DARK_THEME
            } else {
                GROUND_LIGHT_THEME
            }) & 0x00FF_FFFF;
            let page_rgb = (
                ((page >> 16) & 0xff) as usize,
                ((page >> 8) & 0xff) as usize,
                (page & 0xff) as usize,
            );
            let table = crate::color_math::relative_luminance_table();
            let base = crate::color_math::relative_luminance_rgb(
                table, page_rgb.0, page_rgb.1, page_rgb.2,
            );
            let rings_from = out.len();
            let mut rings = [(0usize, 0u8, 0u32, 0u32); HALO_LAYERS as usize];
            let mut gxy = [(0i32, 0i32); HALO_LAYERS as usize];
            let mut n_rings = 0usize;
            let prev_rings = match self.rim_memo {
                Some(m) if m.key.page == page => Some((
                    usize::from(m.key.n_rings).min(HALO_LAYERS as usize),
                    m.key.rings,
                )),
                _ => None,
            };
            const INV_HALO_SPAN: f32 = 1.0 / (HALO_LAYERS - 1) as f32;
            let mut layer = 0;
            while layer < HALO_LAYERS {
                let t = layer as f32 * INV_HALO_SPAN;
                let falloff = (1.0 - t) * (1.0 - t);
                let cov = (HALO_BASE_COV * falloff * halo_energy).min(OVER_INK_COV_CAP) as u8;
                if cov == 0 {
                    break;
                }
                let gx = (t * radius_x) as i32 + 1;
                let gy = (t * radius_y) as i32 + 1;
                let ring_arc = if layer == 0 {
                    rainbow
                } else {
                    shade(spectrum_at(ring_sweep, t * hue_spread), sat, val)
                };
                let ring_light = if let Some((prev_n, prev)) = prev_rings {
                    let mut found = None;
                    let mut pi = 0;
                    while pi < prev_n {
                        let (_, pcov, parc, plight) = prev[pi];
                        if pcov == cov && parc == ring_arc {
                            found = Some(plight);
                            break;
                        }
                        pi += 1;
                    }
                    match found {
                        Some(plight) => plight,
                        None => {
                            let target = light_on_glass_with(
                                table,
                                page_rgb,
                                premul_rgb(RIM_LIGHT_GREY, cov),
                            ) - base;
                            ring_light_on_page_with(table, page_rgb, base, ring_arc, cov, target)
                        }
                    }
                } else {
                    let target =
                        light_on_glass_with(table, page_rgb, premul_rgb(RIM_LIGHT_GREY, cov))
                            - base;
                    ring_light_on_page_with(table, page_rgb, base, ring_arc, cov, target)
                };
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
                layer += 1;
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
            if let Some(memo) = self.rim_memo
                && memo.key == key
            {
                if let Some(best) = memo.best {
                    Self::apply_equalised_rings(&mut out[rings_from..], &rings[..n_rings], &best);
                }
            } else {
                let (best, peak_dark_no_twinkle) =
                    self.equalise_rim_light(&mut out[rings_from..], &rings[..n_rings], page);
                self.rim_memo = Some(RimMemo {
                    key,
                    best,
                    peak_dark_no_twinkle,
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
                let mut k = 0u32;
                while k < 2 {
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
                    k += 1;
                }
            }
        }

        if dark_theme && caret_floor > 0.0 {
            let rim_peak = if pop == 0.0
                && let Some(key) = rim_key
                && let Some(m) = self.rim_memo
                && m.key == key
            {
                if let Some(peak) = m.peak_dark_no_twinkle {
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
            const INV_RAINBOW_CARET_LIGHT_FLOOR: f32 = 1.0 / RAINBOW_CARET_LIGHT_FLOOR;
            let floor_weight = (caret_floor * INV_RAINBOW_CARET_LIGHT_FLOOR).clamp(0.0, 1.0);
            const PROMISED_MARGIN: f32 =
                RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
            fill = lift_to_light_floor(
                fill,
                ((rim_peak + PROMISED_MARGIN) * floor_weight).max(caret_floor),
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
            energy_prev: match previous {
                Some(p) => p.energy,
                None => energy,
            },
            family,
            blink_phase,
            dark_theme,
            geom,
            cfg: *cfg,
            paint_prev: match previous {
                Some(p) => p.cfg.paint,
                None => None,
            },
            sample_dt: since_last,
        });
        self.settle_memo = None;

        RainbowFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// Apply equalised ring colours across each ring's quad slice.
    #[inline]
    const fn apply_equalised_rings(
        quads: &mut [GlowQuad],
        rings: &[(usize, u8, u32, u32)],
        best: &[u32; HALO_LAYERS as usize],
    ) {
        let mut i = 0;
        while i < rings.len() {
            let start = rings[i].0;
            let end = if i + 1 < rings.len() {
                rings[i + 1].0
            } else {
                quads.len()
            };
            let col = best[i];
            let mut j = start;
            while j < end {
                quads[j].color = col;
                quads[j].color2 = col;
                j += 1;
            }
            i += 1;
        }
    }

    #[inline]
    fn equalise_rim_light(
        &mut self,
        quads: &mut [GlowQuad],
        rings: &[(usize, u8, u32, u32)],
        page: u32,
    ) -> (Option<[u32; HALO_LAYERS as usize]>, Option<f32>) {
        if rings.len() < 2 {
            return (None, None);
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut qi = 0;
        while qi < quads.len() {
            let q = &quads[qi];
            qi += 1;
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
            return (None, None);
        }
        let table = crate::color_math::relative_luminance_table();
        let page_rgb = (
            ((page >> 16) & 0xff) as usize,
            ((page >> 8) & 0xff) as usize,
            (page & 0xff) as usize,
        );
        let base =
            crate::color_math::relative_luminance_rgb(table, page_rgb.0, page_rgb.1, page_rgb.2);
        let mut scratch = std::mem::take(&mut self.rim_scratch);
        scratch.clear();
        scratch.resize(w * h, 0);
        let mut ring_pixels = [0u32; HALO_LAYERS as usize];
        let mut i = 0;
        while i < rings.len() {
            let start = rings[i].0;
            let end = if i + 1 < rings.len() {
                rings[i + 1].0
            } else {
                quads.len()
            };
            let bit = 1u32 << (i * 8);
            let mut q_idx = start;
            while q_idx < end {
                let q = &quads[q_idx];
                q_idx += 1;
                if q.alpha != 0 {
                    continue;
                }
                let x_off = (u32::from(q.x) - x0) as usize;
                let qw = usize::from(q.w);
                ring_pixels[i] += u32::from(q.w) * u32::from(q.h);
                let mut row_start = (u32::from(q.y) - y0) as usize * w + x_off;
                let mut row_idx = 0;
                while row_idx < q.h {
                    let row_end = row_start + qw;
                    let mut p_idx = row_start;
                    while p_idx < row_end {
                        scratch[p_idx] += bit;
                        p_idx += 1;
                    }
                    row_start += w;
                    row_idx += 1;
                }
            }
            i += 1;
        }
        let mut buckets = [(0u32, 0u32); 32];
        let mut n_buckets = 0usize;
        let mut buckets_complete = true;
        let mut last_bucket: (u32, usize) = (0, 0);
        let mut si = 0;
        while si < scratch.len() {
            let mask = scratch[si];
            si += 1;
            if mask == 0 {
                continue;
            }
            if last_bucket.0 == mask {
                buckets[last_bucket.1].1 += 1;
                continue;
            }
            let mut found = false;
            let mut idx = 0;
            while idx < n_buckets {
                if buckets[idx].0 == mask {
                    buckets[idx].1 += 1;
                    last_bucket = (mask, idx);
                    found = true;
                    break;
                }
                idx += 1;
            }
            if !found {
                if n_buckets < buckets.len() {
                    buckets[n_buckets] = (mask, 1);
                    last_bucket = (mask, n_buckets);
                    n_buckets += 1;
                } else {
                    buckets_complete = false;
                }
            }
        }
        let rings_len = rings.len();
        #[inline]
        const fn rim_peak_of(
            table: &crate::color_math::RelativeLuminanceTable,
            cols: &[u32; HALO_LAYERS as usize],
            buckets: &[(u32, u32)],
            rings_len: usize,
            buckets_complete: bool,
        ) -> Option<f32> {
            if !buckets_complete {
                return None;
            }
            const FIELD: u32 = {
                let level = RAINBOW_FIELD_LEVEL.round() as u32;
                (level << 16) | (level << 8) | level
            };
            const FIELD_LUM: f32 = crate::color_math::relative_luminance(FIELD);
            let mut peak_lum = FIELD_LUM;
            let mut bi = 0;
            while bi < buckets.len() {
                let mask = buckets[bi].0;
                bi += 1;
                let mut px = FIELD;
                let mut m = mask;
                let mut ri = 0;
                while ri < rings_len {
                    if m == 0 {
                        break;
                    }
                    let c = cols[ri];
                    ri += 1;
                    let hits = m & 0xff;
                    m >>= 8;
                    let mut hi = 0;
                    while hi < hits {
                        px = crate::spectrum::compose_on_glass(px, c, 0);
                        hi += 1;
                    }
                }
                if px != FIELD {
                    peak_lum = peak_lum.max(crate::color_math::relative_luminance_with(table, px));
                }
            }
            Some(peak_lum * 255.0)
        }
        #[inline]
        const fn rim_pile_light(
            table: &crate::color_math::RelativeLuminanceTable,
            cols: &[u32; HALO_LAYERS as usize],
            ring_pixels: &[u32; HALO_LAYERS as usize],
            buckets: &[(u32, u32)],
            rings_len: usize,
            page: u32,
            base: f32,
        ) -> f32 {
            let mut aurora = 0.0f32;
            let mut i = 0;
            while i < rings_len {
                aurora += ring_pixels[i] as f32
                    * crate::color_math::relative_luminance_with(table, cols[i]);
                i += 1;
            }
            let mut sum = 0.0f32;
            let mut bi = 0;
            while bi < buckets.len() {
                let (mask, count) = buckets[bi];
                bi += 1;
                let mut px = page;
                let mut m = mask;
                let mut ri = 0;
                while ri < rings_len {
                    if m == 0 {
                        break;
                    }
                    let c = cols[ri];
                    ri += 1;
                    let hits = m & 0xff;
                    m >>= 8;
                    let mut hi = 0;
                    while hi < hits {
                        px = crate::spectrum::compose_on_glass(px, c, 0);
                        hi += 1;
                    }
                }
                if px != page {
                    sum += count as f32
                        * (crate::color_math::relative_luminance_with(table, px) - base).max(0.0);
                }
            }
            sum + aterm_render::hdr::HDR_GLOW_BOOST * aurora
        }
        #[inline]
        const fn solve_rim_cols(
            table: &crate::color_math::RelativeLuminanceTable,
            page_rgb: (usize, usize, usize),
            base: f32,
            rings: &[(usize, u8, u32, u32)],
            unscaled_targets: &[f32; HALO_LAYERS as usize],
            scale: f32,
        ) -> [u32; HALO_LAYERS as usize] {
            let mut c = [0u32; HALO_LAYERS as usize];
            let mut i = 0;
            while i < rings.len() {
                let (_, cov, arc, _) = rings[i];
                c[i] = ring_light_on_page_with(
                    table,
                    page_rgb,
                    base,
                    arc,
                    cov,
                    unscaled_targets[i] * scale,
                );
                i += 1;
            }
            c
        }
        let buckets = &buckets[..n_buckets];
        let mut grey_rings = [0u32; HALO_LAYERS as usize];
        let mut init_cols = [0u32; HALO_LAYERS as usize];
        let mut unscaled_targets = [0.0f32; HALO_LAYERS as usize];
        let mut i = 0;
        while i < rings_len {
            let (_, cov, _, light) = rings[i];
            let grey = premul_rgb(RIM_LIGHT_GREY, cov);
            grey_rings[i] = grey;
            init_cols[i] = light;
            unscaled_targets[i] = light_on_glass_with(table, page_rgb, grey) - base;
            i += 1;
        }
        let target = rim_pile_light(
            table,
            &grey_rings,
            &ring_pixels,
            buckets,
            rings_len,
            page,
            base,
        );
        let have = rim_pile_light(
            table,
            &init_cols,
            &ring_pixels,
            buckets,
            rings_len,
            page,
            base,
        );
        if target <= 0.0 || (have - target).abs() <= target * Self::RIM_LIGHT_SLACK {
            let peak = rim_peak_of(table, &init_cols, buckets, rings_len, buckets_complete);
            self.rim_scratch = scratch;
            return (None, peak);
        }
        let (mut lo, mut hi) = (0.5f32, 1.5f32);
        let (mut lo_solved, mut hi_solved) = if have < target {
            lo = 1.0;
            (Some((init_cols, have)), None)
        } else {
            hi = 1.0;
            (None, Some((init_cols, have)))
        };
        let mut iter = 1;
        while iter < 8 {
            iter += 1;
            let mid = 0.5 * (lo + hi);
            let c = solve_rim_cols(table, page_rgb, base, rings, &unscaled_targets, mid);
            if matches!(lo_solved, Some((lc, _)) if lc == c) {
                lo = mid;
                continue;
            }
            if matches!(hi_solved, Some((hc, _)) if hc == c) {
                hi = mid;
                continue;
            }
            let l = rim_pile_light(table, &c, &ring_pixels, buckets, rings_len, page, base);
            if l < target {
                lo = mid;
                lo_solved = Some((c, l));
            } else {
                hi = mid;
                hi_solved = Some((c, l));
            }
            if let (Some((lc, _)), Some((hc, _))) = (lo_solved, hi_solved)
                && lc == hc
            {
                break;
            }
        }
        let (a, la) = match lo_solved {
            Some(pair) => pair,
            None => {
                let a = solve_rim_cols(table, page_rgb, base, rings, &unscaled_targets, lo);
                let la = rim_pile_light(table, &a, &ring_pixels, buckets, rings_len, page, base);
                (a, la)
            }
        };
        let (b, lb) = match hi_solved {
            Some(pair) => pair,
            None => {
                let b = solve_rim_cols(table, page_rgb, base, rings, &unscaled_targets, hi);
                let lb = rim_pile_light(table, &b, &ring_pixels, buckets, rings_len, page, base);
                (b, lb)
            }
        };
        let best = if (la - target).abs() <= (lb - target).abs() {
            a
        } else {
            b
        };
        Self::apply_equalised_rings(quads, rings, &best);
        let peak = rim_peak_of(table, &best, buckets, rings_len, buckets_complete);
        self.rim_scratch = scratch;
        (Some(best), peak)
    }

    /// Relative tolerance before [`Self::equalise_rim_light`] re-solves ring light.
    const RIM_LIGHT_SLACK: f32 = 0.005;

    /// Peak rim luminance over the field in `0..255` units.
    #[inline]
    fn rim_light_peak(&mut self, quads: &[GlowQuad], dark_theme: bool) -> f32 {
        if quads.is_empty() || !dark_theme {
            return 0.0;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        let mut qi = 0;
        while qi < quads.len() {
            let q = &quads[qi];
            qi += 1;
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
        const FIELD: u32 = {
            let level = RAINBOW_FIELD_LEVEL.round() as u32;
            (level << 16) | (level << 8) | level
        };
        const FIELD_LUM: f32 = crate::color_math::relative_luminance(FIELD);
        let mut scratch = std::mem::take(&mut self.rim_scratch);
        scratch.clear();
        scratch.resize(w * h, FIELD);
        let mut qi = 0;
        while qi < quads.len() {
            let q = &quads[qi];
            qi += 1;
            let x_off = (u32::from(q.x) - x0) as usize;
            let qw = usize::from(q.w);
            let mut row_start = (u32::from(q.y) - y0) as usize * w + x_off;
            let mut row_idx = 0;
            while row_idx < q.h {
                let row_end = row_start + qw;
                let mut p_idx = row_start;
                while p_idx < row_end {
                    scratch[p_idx] =
                        crate::spectrum::compose_on_glass(scratch[p_idx], q.color, q.alpha);
                    p_idx += 1;
                }
                row_start += w;
                row_idx += 1;
            }
        }
        let table = crate::color_math::relative_luminance_table();
        let mut last_px = FIELD;
        let mut peak_lum = FIELD_LUM;
        let mut si = 0;
        while si < scratch.len() {
            let px = scratch[si];
            si += 1;
            if px != FIELD && px != last_px {
                last_px = px;
                peak_lum = peak_lum.max(crate::color_math::relative_luminance_with(table, px));
            }
        }
        self.rim_scratch = scratch;
        peak_lum * 255.0
    }

    /// Maximum pixel count [`Self::rim_light_peak`] rasterizes.
    const CARET_RASTER_MAX: usize = 1 << 16;
}

/// Push one additive halo ring as four clamped, row-split bars.
#[inline]
fn push_ring(out: &mut Vec<GlowQuad>, geom: Geom, x: i32, y: i32, w: i32, h: i32, premul: u32) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    let th = ((geom.ch as i32) / 8).max(2);
    push_ring_rect(out, geom, x, y, w, th, premul);
    push_ring_rect(out, geom, x, y + h - th, w, th, premul);
    let side_h = h - 2 * th;
    if side_h > 0 {
        push_ring_rect(out, geom, x, y + th, th, side_h, premul);
        push_ring_rect(out, geom, x + w - th, y + th, th, side_h, premul);
    }
}

/// Clamp a pixel rect to the effects box and split it into per-cell-row [`GlowQuad`]s.
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
    let x0_u = x0 as u16;
    let qw = (x1 - x0) as u16;
    let mut row = (y0 - oy).div_euclid(ch);
    let mut next_band = oy + (row + 1) * ch;
    let mut yy = y0;
    while yy < y1 {
        let band_end = next_band.min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0_u,
            y: yy as u16,
            w: qw,
            h: (band_end - yy) as u16,
            color: premul,
            alpha: 0,
            color2: premul,
            alpha2: 0,
        });
        yy = band_end;
        row += 1;
        next_band += ch;
    }
}

/// Sample the family spectrum at `sweep + off`.
#[inline]
const fn spectrum_at(sweep: f32, off: f32) -> u32 {
    rainbow_thing_of(rainbow_sweep_reflect(sweep + off))
}

/// Re-mix `rgb` at saturation `s` and value `v`.
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
    #[inline]
    const fn ch(hi: f32, s: f32, v: f32, c: f32) -> u32 {
        (((hi - s * (hi - c)) * v) + 0.5).clamp(0.0, 255.0) as u32
    }
    (ch(hi, s, v, r) << 16) | (ch(hi, s, v, g) << 8) | ch(hi, s, v, b)
}

/// Energy knee over which the caret's light floor ramps in.
const CARET_LIGHT_KNEE: f32 = 0.25;

/// Lift a colour toward white until its relative luminance reaches `floor`.
#[inline]
pub(crate) const fn lift_to_light_floor(rgb: u32, floor: f32) -> u32 {
    let floor = floor.clamp(0.0, 255.0);
    if floor <= 0.0 || (rgb & 0x00FF_FFFF) == 0x00FF_FFFF {
        return rgb;
    }
    let table = crate::color_math::relative_luminance_table();
    #[inline]
    const fn light(table: &crate::color_math::RelativeLuminanceTable, c: u32) -> f32 {
        crate::color_math::relative_luminance_with(table, c) * 255.0
    }
    if light(table, rgb) >= floor {
        return rgb;
    }
    if floor >= 255.0 {
        return 0x00FF_FFFF;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (rgb & 0x00FF_FFFF, 0x00FF_FFFF);
    let mut step = 0;
    while step < 20 {
        step += 1;
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix_rgb(rgb, 0x00FF_FFFF, mid);
        if mid_rgb == lo_rgb {
            lo = mid;
            continue;
        }
        if mid_rgb == hi_rgb {
            hi = mid;
            continue;
        }
        if light(table, mid_rgb) < floor {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
        }
        if lo_rgb == hi_rgb {
            break;
        }
    }
    hi_rgb
}

/// Relative luminance of one additive quad-pixel over `page`.
#[inline]
const fn light_on_glass_with(
    table: &crate::color_math::RelativeLuminanceTable,
    (pr, pg, pb): (usize, usize, usize),
    premul: u32,
) -> f32 {
    let r = ((premul >> 16) & 0xff) as usize;
    let g = ((premul >> 8) & 0xff) as usize;
    let b = (premul & 0xff) as usize;
    let r_sum = if pr + r < 255 { pr + r } else { 255 };
    let g_sum = if pg + g < 255 { pg + g } else { 255 };
    let b_sum = if pb + b < 255 { pb + b } else { 255 };
    crate::color_math::relative_luminance_rgb(table, r_sum, g_sum, b_sum)
        + aterm_render::hdr::HDR_GLOW_BOOST
            * crate::color_math::relative_luminance_rgb(table, r, g, b)
}

/// Solve one ring's premultiplied colour on `page` for [`RIM_LIGHT`].
#[inline]
const fn ring_light_on_page_with(
    table: &crate::color_math::RelativeLuminanceTable,
    page_rgb: (usize, usize, usize),
    base: f32,
    arc: u32,
    cov: u8,
    target: f32,
) -> u32 {
    #[inline]
    const fn candidate(arc: u32, cov: u8, u: f32) -> u32 {
        if u < 0.0 {
            premul_rgb(mix_rgb(arc, 0x0000_0000, -u), cov)
        } else {
            premul_rgb(mix_rgb(arc, 0x00FF_FFFF, u), cov)
        }
    }
    let (mut lo, mut hi) = (-1.0f32, 1.0f32);
    let hi_c = premul_rgb(0x00FF_FFFF, cov);
    let mut lo_eval = (0u32, 0.0f32);
    let mut hi_eval = (hi_c, light_on_glass_with(table, page_rgb, hi_c) - base);
    let mut step = 0;
    while step < 22 {
        step += 1;
        let mid = 0.5 * (lo + hi);
        let c = candidate(arc, cov, mid);
        if c == lo_eval.0 && lo_eval.1 < target {
            lo = mid;
            continue;
        }
        if c == hi_eval.0 && hi_eval.1 >= target {
            hi = mid;
            continue;
        }
        let added = light_on_glass_with(table, page_rgb, c) - base;
        if added < target {
            lo = mid;
            lo_eval = (c, added);
        } else {
            hi = mid;
            hi_eval = (c, added);
        }
        if lo_eval.0 == hi_eval.0 {
            break;
        }
    }
    let target_light = base + target;
    let (a, err_a) = (lo_eval.0, (lo_eval.1 - target).abs());
    let (b, err_b) = (hi_eval.0, (hi_eval.1 - target).abs());
    let (mut best, mut best_err) = if err_a <= err_b {
        (a, err_a)
    } else {
        (b, err_b)
    };
    let seed = [
        ((best >> 16) & 0xff) as i32,
        ((best >> 8) & 0xff) as i32,
        (best & 0xff) as i32,
    ];
    let sector = [
        (seed[0] - seed[1]).signum(),
        (seed[1] - seed[2]).signum(),
        (seed[0] - seed[2]).signum(),
    ];
    let mut dr = -1;
    while dr <= 1 {
        let r = seed[0] + dr;
        if r >= 0 && r <= 255 {
            let r_u = r as usize;
            let r_packed = (r as u32) << 16;
            let r_sum = if page_rgb.0 + r_u < 255 {
                page_rgb.0 + r_u
            } else {
                255
            };
            let r_sum_lum = table.red[r_sum];
            let r_lum = table.red[r_u];
            let mut dg = -1;
            while dg <= 1 {
                let g = seed[1] + dg;
                if g >= 0 && g <= 255 {
                    let rg_sig = (r - g).signum();
                    if rg_sig == 0 || rg_sig == sector[0] {
                        let g_u = g as usize;
                        let rg_packed = r_packed | ((g as u32) << 8);
                        let g_sum = if page_rgb.1 + g_u < 255 {
                            page_rgb.1 + g_u
                        } else {
                            255
                        };
                        let rg_sum_lum = r_sum_lum + table.green[g_sum];
                        let rg_lum = r_lum + table.green[g_u];
                        let mut db = -1;
                        while db <= 1 {
                            if !(dr == 0 && dg == 0 && db == 0) {
                                let b = seed[2] + db;
                                if b >= 0 && b <= 255 {
                                    let gb_sig = (g - b).signum();
                                    let rb_sig = (r - b).signum();
                                    if (gb_sig == 0 || gb_sig == sector[1])
                                        && (rb_sig == 0 || rb_sig == sector[2])
                                    {
                                        let b_u = b as usize;
                                        let b_sum = if page_rgb.2 + b_u < 255 {
                                            page_rgb.2 + b_u
                                        } else {
                                            255
                                        };
                                        let lum = (rg_sum_lum + table.blue[b_sum])
                                            + aterm_render::hdr::HDR_GLOW_BOOST
                                                * (rg_lum + table.blue[b_u]);
                                        let e = (lum - target_light).abs();
                                        if e < best_err {
                                            best_err = e;
                                            best = rg_packed | b as u32;
                                        }
                                    }
                                }
                            }
                            db += 1;
                        }
                    }
                }
                dg += 1;
            }
        }
        dr += 1;
    }
    best
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "cursor_rainbow_tests.rs"]
mod tests;
