// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typing-reactive rainbow cursor: the block cursor glows and changes colour
//! with typing momentum. Two envelopes drive it:
//!
//! * the body (hue spin, halo, flare) rides the caller's cadence `energy`
//!   ([`crate::cursor_trail::TypingCadence::intensity`], `0..1`, already gated
//!   by reduced motion), a fast ignition heat that is zero shortly after the
//!   last key;
//! * the colour rides [`RainbowConfig::paint`], the ribbon's display spine, so
//!   the block wears its trail's colour for as long as the trail wears it.
//!
//! Hosted beside the ribbon, the hue samples
//! [`crate::cursor_glow::CursorGlow::rainbow_phase`] so both marks share one
//! spectrum clock; standalone, a slow baseline spin accelerates under typing.
//! Either way the hue resolves through the rainbow family's reflected sweep at
//! the caret's field position, so block and ribbon are one continuous rainbow.
//! The block blooms from its base colour toward a vivid rainbow as energy
//! climbs, wrapped in an additive rainbow halo whose rings are each solved to
//! one light ([`RIM_LIGHT`]).
//!
//! Under the rainbow the block never blinks (owner, 2026-09-08): the host pins
//! the shape steady.
//! A short star flare fires on upward crossings of [`MOMENTUM_FLARE_STEPS`]
//! instead, as a pure clock function completing in [`TWINKLE_DUR`].
//!
//! Text safety: the fill goes through the renderer's `floor_cursor_fill`
//! contrast floor, and the halo is additive [`GlowQuad`] light that never
//! touches the glyph. The engine is a pure function of an injected `now`,
//! settles to a stable fingerprint when idle, and emits the same premultiplied
//! quads on the CPU and Metal backends.

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
/// theme, a soft near-black on a light one. Hosts that know the resolved cursor
/// colour pass it as [`RainbowConfig::base`].
const BASE_DARK_THEME: u32 = 0x00FF_FFFF; // white block on a dark background
const BASE_LIGHT_THEME: u32 = 0x0016_161C; // near-black block on a light background

/// The page the caret's light lands on when the host names none: the default
/// background of each polarity (`#111318` dark, `#FDF6E3` light). The rim's
/// light law ([`ring_light_on_page`]) needs the real page; hosts pass it as
/// [`RainbowConfig::ground`].
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
/// rainbow. Only the top of the ramp moves; at rest the block stays near-black
/// (pinned by `light_theme_base_is_dark`).
const MIX_IDLE_LIGHT: f32 = MIX_IDLE;
const MIX_MAX_LIGHT: f32 = 0.95;

/// Halo geometry and brightness: thin concentric additive rings whose coverage
/// falls off quadratically from the block, so they read as one soft rim. Radius
/// and intensity grow with energy over a small idle floor.
///
/// The rings stack per pixel, so the budget is set on the summed coverage at
/// the column just outside the cell: four layers at radius 0.22 and base 28
/// stay within [`crate::cursor_glow::OVER_INK_COV_CAP`] and keep the rim in the
/// inter-character gutter (owner, 2026-07-24).
const HALO_LAYERS: i32 = 4;
const HALO_RADIUS_IDLE: f32 = 0.06; // cells
const HALO_RADIUS_MAX: f32 = 0.22;
const HALO_BASE_COV: f32 = 28.0; // innermost-layer peak coverage (× energy)
/// Brightness kept while idle (× the breath). The settled ember's coverage is
/// `HALO_BASE_COV · HALO_IDLE_FLOOR · (0.35 + PULSE_DEPTH · breath)` truncated
/// to `u8`; much below 0.30 the innermost ring quantizes to zero and the idle
/// glow vanishes.
const HALO_IDLE_FLOOR: f32 = 0.30;
/// Hue spread across the halo rings (turns): each ring sits further along the
/// wheel than the one inside it, so the rim ripples outward as a rainbow. Lerped
/// on the caret's paint: a tenth of the wheel at rest, a third under a fast hand.
const HALO_HUE_SPREAD_IDLE: f32 = 0.10;
const HALO_HUE_SPREAD_MAX: f32 = 0.34;
/// Momentum spin (turns per second at full paint): the rim's spectrum rotates
/// in proportion to the display spine, on top of the family phase, so speed
/// reads as motion. Integrated into [`CursorRainbow::spin`] only across a
/// painted interval; `Spine::at_rest` requires `disp == 0.0` exactly, so a
/// resting rim's offset is constant, its fingerprint settles and
/// [`CursorRainbow::is_active`] releases the tick. No idle spin or breath.
const RIM_SPIN_TURNS_PER_S: f32 = 0.45;

/// The rim's light is hue-invariant: every ring is laid at the light a grey
/// ring of this relative luminance (linear, `0..1`) would add to the page at
/// the same coverage, solved on the premultiplied bytes over the page by
/// [`ring_light_on_page`]. The arc colour is pulled toward white where dark and
/// toward black where bright, hue intact.
///
/// The spinning arc's own luminance spans about thirty-fold (indigo to
/// yellow); without this law the rim would brighten and dim with wherever the
/// spin parked it, reading as a pulse after every pause. The solve is done on
/// the glass, not the colour: additive light lands in sRGB bytes and the EDR
/// aurora re-emits it linearly ([`light_on_glass`] names both), so only the
/// displayed light is hue-flat
/// (`the_rim_light_is_hue_invariant_and_monotone_in_paint`).
///
/// With every ring at one light, the rim's light is a function of
/// `halo_energy` alone, which is monotone in the paint. `0.462` is the arc's
/// mean light, re-derived by `rim_light_grey_is_the_arcs_mean`, so the average
/// over a turn is unchanged and only the variance is gone.
const RIM_LIGHT: f32 = 0.462;
/// The grey of relative luminance [`RIM_LIGHT`] (sRGB byte `181`), pinned by
/// `rim_light_grey_is_the_arcs_mean`.
const RIM_LIGHT_GREY: u32 = 0x00B5_B5B5;

/// Energy below which the cursor is settled: the animator reports inactive so
/// the host stops the 60 fps tick. A settled caret is a solid block inside its
/// resting rim; the blink is never armed while the rainbow owns the caret.
const SETTLED_ENERGY: f32 = 0.02;

// ── the twinkle (the "glitter star") ────────────────────────────────────────
/// Momentum rungs: the caret sparkles on upward crossings of these by its
/// paint, with hysteresis. No timer and no idle cost; never fires at rest.
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
/// The arms cross the cell centre, so this bounds how far they reach into both
/// neighbours.
const TWINKLE_REACH: f32 = 0.20;
/// Peak additive coverage of the star arms and glitter dots, bounded by
/// [`crate::cursor_glow::OVER_INK_COV_CAP`] because the arms cross neighbouring
/// glyphs.
const TWINKLE_ARM_COV: f32 = 44.0;
const TWINKLE_DOT_COV: f32 = 38.0;
/// Scintillation cycles across one flare: a deterministic sine wobble over the
/// pop envelope, phase-shifted per flare so consecutive twinkles differ.
///
/// Photosensitivity bound: the flash rate is `TWINKLE_SCINT / TWINKLE_DUR` Hz;
/// keep it `<= 3.2` (WCAG 2.3.1's general-flash threshold is 3 Hz). Pinned by
/// `twinkle_flash_rate_stays_under_the_photosensitivity_bound`.
const TWINKLE_SCINT: f32 = 0.5;

/// Per-tick dt clamp (seconds) across a charged interval, so a stalled but
/// charged window does not fling the phase forward. Idle time is never
/// integrated: a settled cursor freezes its hue and breath.
const MAX_DT: f32 = 0.6;

/// How many frame steps [`CursorRainbow::next_change_deadline`] dry-runs before
/// it hands the host a poll. Each step is one tick of this engine, so the bound
/// is what keeps a scheduling decision O(1) in the length of the fade.
const SETTLE_LOOKAHEAD: u32 = 24;
/// The coarsest the colour release is sampled. Bounds the sleep a probe asks
/// for at the fade's slow end and how far a mispredicted paint drifts before the
/// next real tick.
const SETTLE_POLL_CAP: Duration = Duration::from_millis(250);

/// The inputs of the last real tick, latched so
/// [`CursorRainbow::next_change_deadline`] can dry-run the engine without the
/// host re-deriving them. Across a colour release only `energy` and `cfg.paint`
/// move; both are extrapolated by [`release_forecast`]. Everything else holds
/// while the caret rests, and changes arrive with a real tick that re-latches.
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
    /// model may guess at. Ride the frame cadence and measure again.
    Cadence,
}

/// Extrapolate a released envelope `ahead` seconds past its last sample pair
/// with a first-order log-linear fit.
///
/// * A spent (`now <= 0`) or flat pair is exact.
/// * A rising pair is `None`: the host is driving it up, not releasing it; the
///   caller falls back to the frame cadence.
///
/// The display spine is an eased chase, so the fit has a small residual of
/// either sign; the caller absorbs it by waking one frame early.
fn release_forecast(now_v: f32, prev_v: f32, dt: f32, ahead: f32) -> Option<f32> {
    if now_v <= 0.0 {
        return Some(0.0);
    }
    if prev_v < now_v {
        return None;
    }
    if dt <= 0.0 || prev_v <= now_v {
        return Some(now_v);
    }
    let ratio = (now_v / prev_v).clamp(1e-3, 1.0);
    Some(now_v * ratio.powf(ahead / dt))
}

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct RainbowConfig {
    /// Master on/off (the style opted into the rainbow cursor and the cursor is a
    /// focused, visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in by
    /// the host exactly like the aurora. 0 ⇒ effectively off (no spin, no halo).
    pub intensity: f32,
    /// The terminal reports a blinking block. While the rainbow owns the caret
    /// (`fill.is_some()`) the host pins it steady and never arms the blink; the
    /// twinkle, sourced from [`MOMENTUM_FLARE_STEPS`], takes its place. `false`
    /// (a steady block) never twinkles.
    pub blinking: bool,
    /// The colour the block wears at rest, `0x00RRGGBB`: the terminal's
    /// resolved cursor colour (OSC 12, else the theme cursor, else the OSC 10
    /// foreground). The spectrum is a tint over this base. The renderer applies
    /// [`aterm_render::RenderInput::cursor_fill_override`] instead of the frame
    /// cursor colour, so the base must come from here.
    ///
    /// `None` uses the theme-polar base ([`BASE_DARK_THEME`] /
    /// [`BASE_LIGHT_THEME`]) for callers with no cursor colour.
    pub base: Option<u32>,
    /// The ribbon-head colour `CursorGlow` emitted this frame, `0x00RRGGBB`.
    /// The hot block blooms toward this exact colour so caret and trail agree.
    /// `None` uses the standalone family-sweep resolver.
    pub head_rgb: Option<u32>,
    /// The trail's paint spine, `0..1`: how strongly the ribbon beside the
    /// caret is coloured now. Feed
    /// [`crate::cursor_glow::CursorGlow::momentum_display`], so the block wears
    /// its trail's colour on the trail's schedule rather than on the much faster
    /// `energy` cadence (which reaches zero while the ribbon is still painted).
    ///
    /// `None` makes the colour envelope `energy`. With `Some`, the envelope is
    /// `max(paint · intensity, energy · intensity)`: the cadence gives the fast
    /// attack and the ribbon owns the release.
    pub paint: Option<f32>,
    /// The page the caret's light lands on, `0x00RRGGBB`: the resolved terminal
    /// background. The rim's light law ([`RIM_LIGHT`]) solves each ring's
    /// premultiplied bytes for the light they add to this page.
    ///
    /// `None` uses the default page for the polarity
    /// ([`GROUND_DARK_THEME`] / [`GROUND_LIGHT_THEME`]).
    pub ground: Option<u32>,
    /// The instant a v2 meteor's frame-0 flare fired (`RAINBOW-KITTY-V2.md`
    /// §7.1), copied from [`crate::cursor_glow::CursorGlow::caret_flare_at`].
    /// While `Some`: the block fills `#FFFFFF` on the firing frame (the vivid
    /// hue on a light theme), then springs back to its field stop on a
    /// critically damped `spring-snap` (response 0.18 s, no undershoot), and the
    /// halo rings pop radius ×1.4 relaxing τ 120 ms. The engine clears it once
    /// the relax settles.
    ///
    /// `None` is the identity: the tick emits exactly the no-flare bytes.
    pub flare_at: Option<Instant>,
}

/// What a tick produced: the block fill colour to hand the renderer (it floors it for
/// contrast) and a fingerprint that changes on every visible step (0 when dormant).
#[derive(Clone, Copy, Debug)]
pub struct RainbowFrame {
    /// The evolving block-fill colour `0x00RRGGBB`, or `None` when the rainbow cursor
    /// is off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + halo (0 ⇒ nothing to show this frame).
    pub fp: u64,
}

/// Per-window rainbow-cursor animation state: the standalone hue fallback,
/// idle breath, twinkle, and the colour release's settle probe.
#[derive(Clone, Default)]
pub struct CursorRainbow {
    /// Responsive standalone spin in unit turns `0..1`. It is lifted onto one
    /// complete family sweep by [`rainbow_phase_from_unit_turn`] when a host
    /// does not provide the ribbon's shared phase.
    phase: f32,
    /// Idle-breath phase in turns `0..1`.
    pulse: f32,
    last: Option<Instant>,
    /// Latched energy at the last tick (so [`is_active`] answers without a clock).
    energy: f32,
    /// Latched colour envelope at the last tick (the paint spine folded with
    /// the energy, [`RainbowConfig::paint`]). It outlives `energy`, so
    /// `is_active` reads both; otherwise a host would freeze a hot block
    /// mid-cool and snap it to the base on a later frame.
    paint: f32,
    /// The highest momentum rung ([`MOMENTUM_FLARE_STEPS`]) crossed upward and
    /// not yet released: the twinkle's edge detector. Climbs on the tick the
    /// paint reaches the next rung (one flare however many rungs it crossed) and
    /// steps down only once the paint falls [`MOMENTUM_FLARE_HYST`] below it.
    /// `0` at rest.
    flare_rung: u8,
    /// Start of the in-flight twinkle flare (`None` between flares).
    twinkle_at: Option<Instant>,
    /// Flare counter — the deterministic per-flare variation seed (dot
    /// corners + scintillation phase), and the fingerprint's flare identity.
    twinkle_seq: u32,
    /// The momentum spin's accumulated offset along the family sweep, in turns.
    /// Advanced by `dt · RIM_SPIN_TURNS_PER_S · paint` across a painted
    /// interval, frozen otherwise, like `phase`.
    spin: f32,
    /// Latched "a flare is mid-flight" at the last tick (the [`is_active`]
    /// clockless answer, like `energy`).
    twinkling: bool,
    /// The fingerprints of the last two ticks, so [`is_active`] answers from
    /// the u8-level delta the host would present rather than from
    /// `paint > SETTLED_ENERGY`: the colour envelope cools slowly while the u8
    /// fill often repeats frame after frame. A frame equal to the previous one
    /// asks for no tick; a cooling caret then names its next change
    /// ([`Self::settling`], [`Self::next_change_deadline`]).
    fp_last: u64,
    fp_prev: u64,
    /// Scratch pixel buffer for [`Self::rim_light_peak`], retained across
    /// frames.
    rim_scratch: Vec<u32>,
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
    /// Whether the host must keep arming the animation tick: while the cursor
    /// is charged or a twinkle is in flight. Once settled the caret sits solid
    /// inside its resting rim with no wakeups.
    #[must_use]
    pub fn is_active(&self) -> bool {
        // Fingerprint law: charged or mid-flare keeps the tick; a cooling block
        // keeps it only while its last two frames differed.
        self.energy > SETTLED_ENERGY
            || self.twinkling
            || (self.paint > SETTLED_ENERGY && self.fp_last != self.fp_prev)
    }

    /// The colour release still owes frames, but not at frame cadence: the
    /// caret is painted (`paint > SETTLED_ENERGY`) and its last two frames were
    /// identical.
    ///
    /// Not "settled": a fading fill steps one u8 level every few frames, so two
    /// identical frames occur long before the fade ends. The host keeps the
    /// lane live and paces it by [`Self::next_change_deadline`] on its coarse
    /// one-shot arm.
    ///
    /// Disjoint from [`Self::is_active`]; their OR is "anything still moving":
    /// `energy > SETTLED || twinkling || paint > SETTLED`.
    #[must_use]
    pub fn settling(&self) -> bool {
        !self.is_active() && self.paint > SETTLED_ENERGY
    }

    /// When this caret's next visible step is due, so a colour release neither
    /// freezes mid-fade nor pins the frame cadence.
    ///
    /// `Some(now + frame_interval)` while [`Self::is_active`], `None` once
    /// nothing is painted, and across a [`Self::settling`] release one frame
    /// before the first dry-run frame that differs from the one on glass.
    ///
    /// The deadline is a dry run of this engine's own tick on a copy, stepped
    /// along the frame grid with the latched inputs and the two host envelopes
    /// extrapolated ([`release_forecast`]), for at most [`SETTLE_LOOKAHEAD`]
    /// steps and never past [`SETTLE_POLL_CAP`]. Using the tick itself keeps
    /// the schedule in step with every colour law.
    ///
    /// The wake is one frame early because the extrapolation's residual has no
    /// fixed sign and a late step is visible; the real tick re-measures. Every
    /// drawn frame is a real tick; the probe predicts only the schedule.
    /// Memoised per real tick; allocates nothing after the first call.
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
            // (which latches on every non-inert return); fail onto the cadence.
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
        // The copy takes this caret's scratch rather than cloning it, so a
        // scheduling decision allocates nothing; both come back below.
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

    /// Advance one frame at `now` with the typing `energy` (`0..1`), the host's
    /// `blink_phase` (unread; kept for seam arity), the block cursor cell `cur`
    /// (`None` = hidden), the theme darkness, grid `geom` and `cfg`. Appends the
    /// additive halo (and any twinkle) to `out` and returns the block fill
    /// colour and a fingerprint. Pure in the injected inputs.
    #[allow(clippy::too_many_arguments)]
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
    ///
    /// Read `family_phase` from [`crate::cursor_glow::CursorGlow::rainbow_phase`]
    /// immediately after ticking that engine for the same frame. The block,
    /// halo rings, glitter, fresh-ink rail, and ribbon then all sample the same
    /// `0..1024` clock and the same reflected spectrum resolver. The private
    /// energy clock still advances while locked so falling back later is
    /// continuous in its own domain; it never contributes colour on this path.
    #[allow(clippy::too_many_arguments)]
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
        // Unread by the frame; latched so the release dry run replays the tick.
        blink_phase: bool,
        dark_theme: bool,
        geom: Geom,
        cfg: &RainbowConfig,
        out: &mut Vec<GlowQuad>,
    ) -> RainbowFrame {
        let e = (energy.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Fully inert (the plain themed cursor) when off, when the geometry is
        // degenerate, or at zero amplitude (reduced motion / load shed): no
        // idle halo or hue drift under Reduce Motion.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.energy = 0.0; // inert: report settled so the host disarms the tick
            self.paint = 0.0;
            self.last = Some(now);
            // Twinkle state clears too: a re-enable starts from rung 0, so the
            // paint it comes back at fires at most one flare, never a stale one.
            self.flare_rung = 0;
            self.twinkle_at = None;
            self.twinkling = false;
            self.fp_prev = self.fp_last;
            self.fp_last = 0;
            // Drop the dry run's inputs so a re-enable never runs this caret
            // forward from a frame it did not draw.
            self.settle_probe = None;
            self.settle_memo = None;
            return RainbowFrame { fill: None, fp: 0 };
        }
        // `out` is the window's shared glow scratch, already filled by
        // `CursorGlow::tick`; the caret's quads are the tail from here.
        // [`Self::rim_light_peak`] measures that tail as one pile.
        let emitted_from = out.len();
        let was_active = self.energy > SETTLED_ENERGY;
        self.energy = e;
        // Two envelopes: the cadence (`e`) sizes the halo, spin and flare; the
        // trail's paint ([`RainbowConfig::paint`]) decides how much of the
        // arc's colour the block wears. `max` keeps the cadence's fast attack
        // while the ribbon owns the release; with no host spine both are `e`.
        let paint = cfg.paint.map_or(e, |p| {
            (p.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).max(e)
        });
        let was_painted = self.paint > SETTLED_ENERGY;
        self.paint = paint;

        // Advance hue and breath only across a continuously charged interval,
        // so a later unrelated present never integrates idle time and resumed
        // typing starts from the frozen ember.
        let active = e > SETTLED_ENERGY;
        // Raw interval since the last tick: both integrations clamp it, and the
        // release dry run extrapolates the host envelopes over it
        // (`SettleProbe::sample_dt`).
        let since_last = self
            .last
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            .unwrap_or(0.0);
        let dt = if was_active && active {
            since_last.min(MAX_DT)
        } else {
            0.0
        };
        // The momentum spin integrates over the painted interval, which
        // outlives the charged one: the rim slows with the spine and stops as a
        // deceleration. Same gate shape and `MAX_DT` clamp as `dt`; the offset
        // is folded into the fingerprint so a turning rim is never early-outed.
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
            // `rainbow_sweep_reflect`); `fract()` would jump half a period.
            // Pinned by `spin_is_continuous_across_its_wrap`.
            self.spin = (self.spin + dt_spin * RIM_SPIN_TURNS_PER_S * paint).rem_euclid(2.0);
        }
        // The ribbon owns the family phase when the host supplies it. Standalone,
        // the energy-rate spin's unit turn is lifted onto one full reflected
        // sweep; fed directly it would cover a fraction of the ring and jump
        // back every wrap.
        let spectrum_phase = family
            .map(|(phase, _)| phase)
            .unwrap_or_else(|| rainbow_phase_from_unit_turn(self.phase));
        // The caret reads the field position it is about to lay (§2.1), so
        // block and ribbon share a colour by construction. With no host ribbon
        // it falls back to the sweep at its own column on its own clock.
        let spectrum_field = family.map(|(_, field)| field);

        // Momentum twinkle: fires on an upward crossing of the next rung by the
        // paint, at most four times per wind-up and never at rest. One flare
        // per tick however many rungs were crossed; a rung re-arms once the
        // paint falls `HYST` below it. A decaying scalar crosses nothing upward,
        // so idle cost is zero. Gated on `cfg.blinking`: a steady block has no
        // blink to replace (`steady_block_never_twinkles`).
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
        // The flare envelope: a peaked pop (0 at both ends, brightest mid-flare)
        // with a per-flare-phased scintillation wobble — the "glitter" read.
        let (pop, shimmer) = match self.twinkle_at {
            Some(t0) => {
                let u = now.saturating_duration_since(t0).as_secs_f32() / TWINKLE_DUR;
                if u >= 1.0 {
                    self.twinkle_at = None; // flare complete — re-settle
                    (0.0, 0.0)
                } else {
                    let scint = (u * TWINKLE_SCINT + self.twinkle_seq as f32 * 0.37)
                        * std::f32::consts::TAU;
                    ((u * std::f32::consts::PI).sin(), 0.72 + 0.28 * scint.sin())
                }
            }
            None => (0.0, 0.0),
        };
        // Active while a flare is in flight, not while its envelope is nonzero:
        // the pop is 0 at `u == 0`, and reading the envelope would disarm the
        // tick on the frame that arms the flare.
        self.twinkling = self.twinkle_at.is_some();

        // Vivid under the keys, calm at rest. On a light theme saturation holds:
        // a pale hue mixed into the near-black base turns brown, while a
        // saturated one just darkens. Mixing toward white (dark theme) pastels
        // gracefully, so the dark ramp stays.
        let sat = if dark_theme {
            lerp(SAT_IDLE, SAT_MAX, paint)
        } else {
            lerp(SAT_IDLE_LIGHT, SAT_MAX, paint)
        };
        let val = lerp(VAL_IDLE, VAL_MAX, paint);
        // The caret's column is its place on the family sweep, as the ribbon's
        // rail under this cell resolves it. A hidden cursor uses column 0.
        let col = cur.map_or(0, |(_, cc)| cc);
        let sweep = spectrum_field.unwrap_or_else(|| rainbow_sweep_at(col, spectrum_phase));
        let band = spectrum_at(sweep, 0.0);
        let head_rgb = cfg.head_rgb.unwrap_or(band);
        let rainbow = shade(head_rgb, sat, val);

        // Block fill: tint from the base toward the rainbow with the paint. The
        // renderer floors it against the cell background so the cut-out glyph
        // stays sharp. The base is the cursor's own colour when the host
        // resolved one; the theme-polar constants otherwise.
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
        // The straight line from an unrelated base to the arc colour can cross
        // a hue neither end has (a green cursor toward blue passes turquoise);
        // `caret_fill` below corrects the emitted byte.
        let mut fill = mix_rgb(base, rainbow, lerp(mix_idle, mix_max, paint));
        // The twinkle glint: toward star-white on a dark theme, toward the vivid
        // hue on a light one, where white would sink into the page.
        if pop > 0.0 {
            let glint = if dark_theme {
                0x00FF_FFFF
            } else {
                shade(head_rgb, 1.0, 0.85)
            };
            fill = mix_rgb(fill, glint, TWINKLE_MIX * pop * cfg.intensity);
        }
        // The caret is the brightest thing in the effect (§8 d), a statement
        // about light, so it is enforced last and in luminance. The floor rides
        // the colour envelope (the ribbon's sparkle field lives as long as the
        // ribbon does) and is zero at rest, where the block is the cursor's own
        // colour. The knee is a quarter of the range.
        let caret_floor =
            RAINBOW_CARET_LIGHT_FLOOR * aterm_render::smoothstep01(paint / CARET_LIGHT_KNEE);
        let fill = lift_to_light_floor(fill, caret_floor);
        // The flare (§7.1): on a v2 meteor's birth frame the block fills white
        // (the vivid hue on a light theme), then springs back to `fill` on a
        // critically damped `spring-snap`. `None` leaves `fill` untouched.
        let flare_age = cfg
            .flare_at
            .map(|t| now.saturating_duration_since(t).as_secs_f32());
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

        // The additive halo: concentric rings around the block, brightness a
        // small breathing idle floor plus the paint, radius growing with the
        // paint. Riding `paint` (the ribbon's display spine, floored by the
        // cadence) makes the glow a momentum read: it builds under a fast hand
        // and bleeds off over a few seconds when the hand stops.
        let breath = 0.5 + 0.5 * (self.pulse * std::f32::consts::TAU).sin(); // 0..1
        let halo_energy = HALO_IDLE_FLOOR * (0.35 + PULSE_DEPTH * breath) + paint;
        if let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
            && halo_energy > 0.01
        {
            let cw = geom.cw as i32;
            let ch = geom.ch as i32;
            // Window-absolute cell anchor (the window-space effects layer).
            let cx = geom.origin_x as i32 + cc as i32 * cw;
            let cy = geom.origin_y as i32 + cr as i32 * ch;
            // Separate horizontal and vertical reach: each axis grows by a
            // fraction of the cell's own size, bounded by `HALO_RADIUS_MAX`, so
            // the light hugs the block instead of reaching a full cell width
            // into neighbours, and no two layers land the same rect.
            // §7.1: rings pop ×1.4 on the flare edge and relax τ 120 ms; `1.0`
            // with no flare is the bit-exact identity.
            let flare_pop = flare_age.map_or(1.0, |age| {
                1.0 + FLARE_RING_POP * (-age / FLARE_RING_TAU_S).exp()
            });
            let radius_x =
                (lerp(HALO_RADIUS_IDLE, HALO_RADIUS_MAX, paint) * cw as f32 * flare_pop).max(1.0);
            let radius_y =
                (lerp(HALO_RADIUS_IDLE, HALO_RADIUS_MAX, paint) * ch as f32 * flare_pop).max(1.0);
            // Speed wears more arc: the rings fan across a tenth of the wheel at
            // rest and a third of it under a fast hand …
            let hue_spread = lerp(HALO_HUE_SPREAD_IDLE, HALO_HUE_SPREAD_MAX, paint);
            // … and the fan turns with the momentum, on top of the family phase
            // (`self.spin`, integrated above; a constant at rest).
            let ring_sweep = sweep + self.spin;
            // The page the rings' light is solved over (`RIM_LIGHT`).
            let page = cfg.ground.unwrap_or(if dark_theme {
                GROUND_DARK_THEME
            } else {
                GROUND_LIGHT_THEME
            }) & 0x00FF_FFFF;
            // Where each ring's quads start in `out`, its coverage and its arc
            // colour — what `equalise_rim_light` needs to re-solve the pile.
            let rings_from = out.len();
            let mut rings = [(0usize, 0u8, 0u32); HALO_LAYERS as usize];
            let mut n_rings = 0usize;
            for layer in 0..HALO_LAYERS {
                // t: 0 = innermost ring hugging the block, 1 = outermost at `radius`.
                // Coverage falls off as (1-t)² so the overlapping thin rings blend into
                // a soft rim that is bright at the block and gone by the radius.
                let t = layer as f32 / (HALO_LAYERS - 1) as f32;
                let gx = (t * radius_x) as i32 + 1;
                let gy = (t * radius_y) as i32 + 1;
                let falloff = (1.0 - t) * (1.0 - t);
                let cov = (HALO_BASE_COV * falloff * halo_energy).min(OVER_INK_COV_CAP) as u8;
                if cov == 0 {
                    continue;
                }
                // Each ring samples its own point along the family's sweep, so
                // the rim is a rainbow that spins with the phase.
                let ring_arc = if layer == 0 {
                    // The innermost ring touches the ribbon nozzle and wears
                    // its exact emitted hue; outer rings fan through the family.
                    shade(head_rgb, sat, val)
                } else {
                    shade(spectrum_at(ring_sweep, t * hue_spread), sat, val)
                };
                // Every ring is laid at one light (`RIM_LIGHT`): hue from the
                // arc, added light from a grey ring; `cov` alone sets
                // brightness.
                let ring_light = ring_light_on_page(ring_arc, cov, page, 1.0);
                rings[n_rings] = (out.len() - rings_from, cov, ring_arc);
                n_rings += 1;
                push_ring(
                    out,
                    geom,
                    // Tight per-axis growth (`gx`/`gy`) so the rim hugs the
                    // block and stays out of neighbour glyphs.
                    cx - gx,
                    cy - gy,
                    cw + 2 * gx,
                    ch + 2 * gy,
                    ring_light,
                );
            }
            // Equalise the pile as a whole: stacked rings gain from the
            // transfer's convexity by a colour-dependent amount.
            self.equalise_rim_light(&mut out[rings_from..], &rings[..n_rings], page);
        }

        // The twinkle star: additive arms through the cell centre overhanging
        // the block, plus two glitter dots at hash-picked corners. The fill is
        // opaque, so only the overhang shows. Same per-axis hug and coverage
        // cap as the halo; every quad goes through the clamped row-splitter.
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
                // Star-white arms on dark themes; the vivid live hue on light
                // ones (additive white is invisible over a light background).
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
            // Glitter dots: two per flare, corners and 1 px jitter picked by an
            // integer hash of the flip counter (deterministic). A pop² envelope
            // makes them wink after the arms bloom.
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

        // The rim's own light, measured on the pixel: every ring, arm and dot
        // this tick emitted, composited as `aterm_render` will.
        let rim_peak = self.rim_light_peak(&out[emitted_from..], dark_theme);
        // The rim lands outside the opaque block, after the ribbon has spent
        // the field's light budget, so its peak can exceed a blue-end block's
        // floor. Lift the block's centre above the measured peak of the final
        // pile by the family's usual margin, weighted by the live floor so a
        // settled OSC-12 cursor stays exactly its configured colour.
        if dark_theme {
            let floor_weight = (caret_floor / RAINBOW_CARET_LIGHT_FLOOR).clamp(0.0, 1.0);
            let promised_margin =
                RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
            fill = lift_to_light_floor(
                fill,
                ((rim_peak + promised_margin) * floor_weight).max(caret_floor),
            );
        }

        // Fingerprint: quantized phase, energy and fill, so a settled cursor
        // early-outs the present while any visible step forces a repaint. The
        // flare folds in only while lit (`pop > 0`), leaving no residue.
        let twinkle_fp = if pop > 0.0 {
            (((pop * 255.0) as u64) << 24).wrapping_add(u64::from(self.twinkle_seq) << 40)
        } else {
            0
        };
        // Key the spectrum by its resolved period-two position, so equal
        // visible phases (including a full ring wrap) share a key.
        let spectrum_fp = sweep;
        let fp = ((spectrum_fp * 1024.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(head_rgb).rotate_left(7))
            .wrapping_add((halo_energy * 255.0) as u64)
            .wrapping_add(((fill as u64) << 12) ^ ((self.pulse * 64.0) as u64))
            .wrapping_add(((self.spin * 1024.0) as u64).wrapping_mul(7_919))
            .wrapping_add(twinkle_fp);
        self.fp_prev = self.fp_last;
        self.fp_last = fp;
        // Latch this tick's inputs and the previous sample of each host
        // envelope for the release dry run; drop any memoised answer.
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
    ///
    /// [`ring_light_on_page`] matches each ring alone, but the rings overlap
    /// and stacked light gains a colour-dependent bonus from the transfer's
    /// convexity. So the pile is laid twice into the scratch over the page
    /// through the family's blend: as emitted, and with every ring wearing
    /// [`RIM_LIGHT_GREY`]. If the totals differ by more than
    /// [`Self::RIM_LIGHT_SLACK`], one scalar on every ring's target is bisected
    /// (eight halvings) and the rings re-solved; hues are untouched. Skipped
    /// when already within the slack.
    fn equalise_rim_light(
        &mut self,
        quads: &mut [GlowQuad],
        rings: &[(usize, u8, u32)],
        page: u32,
    ) {
        if rings.len() < 2 {
            return;
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
            return;
        }
        let base = light_on_glass(page, 0);
        let mut scratch = std::mem::take(&mut self.rim_scratch);
        // The pile's light over the page with ring `i` wearing `colour(i)`,
        // the same two terms as [`light_on_glass`]: the byte composite is laid
        // into the scratch and read once at the end (bytes saturate and stack);
        // the EDR aurora's linear re-emission is summed per quad-pixel.
        let mut pile_light = |quads: &[GlowQuad], colour: &dyn Fn(usize) -> u32| -> f32 {
            scratch.clear();
            scratch.resize(w * h, page);
            let mut aurora = 0.0f32;
            for (i, &(start, _, _)) in rings.iter().enumerate() {
                let end = rings.get(i + 1).map_or(quads.len(), |&(next, _, _)| next);
                let c = colour(i);
                let c_light = crate::color_math::relative_luminance(c);
                for q in &quads[start..end] {
                    for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                        for xx in u32::from(q.x)..u32::from(q.x) + u32::from(q.w) {
                            let k = (yy - y0) as usize * w + (xx - x0) as usize;
                            scratch[k] = crate::spectrum::compose_on_glass(scratch[k], c, q.alpha);
                            aurora += c_light;
                        }
                    }
                }
            }
            scratch
                .iter()
                .map(|&px| (crate::color_math::relative_luminance(px) - base).max(0.0))
                .sum::<f32>()
                + aterm_render::hdr::HDR_GLOW_BOOST * aurora
        };
        let target = pile_light(quads, &|i| premul_rgb(RIM_LIGHT_GREY, rings[i].1));
        let emitted: Vec<u32> = rings
            .iter()
            .map(|&(start, _, _)| quads[start].color)
            .collect();
        let have = pile_light(quads, &|i| emitted[i]);
        if target <= 0.0 || (have - target).abs() <= target * Self::RIM_LIGHT_SLACK {
            self.rim_scratch = scratch;
            return;
        }
        let solve = |scale: f32| -> [u32; HALO_LAYERS as usize] {
            let mut c = [0u32; HALO_LAYERS as usize];
            for (i, &(_, cov, arc)) in rings.iter().enumerate() {
                c[i] = ring_light_on_page(arc, cov, page, scale);
            }
            c
        };
        let (mut lo, mut hi) = (0.5f32, 1.5f32);
        for _ in 0..8 {
            let mid = 0.5 * (lo + hi);
            let c = solve(mid);
            if pile_light(quads, &|i| c[i]) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (a, b) = (solve(lo), solve(hi));
        let (la, lb) = (pile_light(quads, &|i| a[i]), pile_light(quads, &|i| b[i]));
        let best = if (la - target).abs() <= (lb - target).abs() {
            a
        } else {
            b
        };
        for (i, &(start, _, _)) in rings.iter().enumerate() {
            let end = rings.get(i + 1).map_or(quads.len(), |&(next, _, _)| next);
            for q in &mut quads[start..end] {
                q.color = best[i];
                // The ring's quads are flat: both ends take the equalised colour.
                q.color2 = best[i];
            }
        }
        self.rim_scratch = scratch;
    }

    /// How far the ring-by-ring rim may sit from the grey pile's light before
    /// [`Self::equalise_rim_light`] re-solves it: under the pile's finest byte
    /// step.
    const RIM_LIGHT_SLACK: f32 = 0.005;

    /// The rim's peak light over the field, which §8(d)'s lift of the opaque
    /// block is measured against, in the `0..255` luminance units
    /// [`lift_to_light_floor`] takes.
    ///
    /// The rim is a pile (`HALO_LAYERS` rings, two arms, two dots) whose peak
    /// belongs to no single quad, so it is composited with the emitted colours
    /// over the grey level of a fully spent field ([`RAINBOW_FIELD_LEVEL`])
    /// through [`crate::spectrum::compose_on_glass`] (`aterm_render`'s
    /// arithmetic), and the brightest pixel's relative luminance is returned.
    /// With every ring at [`RIM_LIGHT`], the result depends on coverage, not
    /// hue. Nothing here recolours a quad.
    ///
    /// A light page returns `0.0`: the additive-brightest ordering is the
    /// dark-page law. Past [`Self::CARET_RASTER_MAX`] the pass fails bright
    /// (`255.0`), so the caller lifts the centre to white.
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
        let level = RAINBOW_FIELD_LEVEL.round() as u32;
        let field = (level << 16) | (level << 8) | level;
        let mut scratch = std::mem::take(&mut self.rim_scratch);
        scratch.clear();
        scratch.resize(w * h, field);
        for q in quads {
            for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                for xx in u32::from(q.x)..u32::from(q.x) + u32::from(q.w) {
                    let i = (yy - y0) as usize * w + (xx - x0) as usize;
                    scratch[i] = crate::spectrum::compose_on_glass(scratch[i], q.color, q.alpha);
                }
            }
        }
        let peak = scratch.iter().copied().fold(0.0f32, |peak, px| {
            peak.max(crate::color_math::relative_luminance(px) * 255.0)
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
fn push_ring(out: &mut Vec<GlowQuad>, geom: Geom, x: i32, y: i32, w: i32, h: i32, premul: u32) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    let th = ((geom.ch as i32) / 8).max(2); // ring thickness in px
    push_ring_rect(out, geom, x, y, w, th, premul);
    push_ring_rect(out, geom, x, y + h - th, w, th, premul);
    // The side bars sit between the top and bottom ones so corners add once.
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

/// Clamp a pixel rect to the window interior and split it into per-cell-row
/// [`GlowQuad`]s, so the dirty gate and scissor stay exact (the aurora's
/// `push_rect` contract).
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
    // Effects box (grid + head band): identity-exact at head 0; a below-grid
    // band would only be skipped by the renderers' row gates.
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
        // Grid-row damage hint, anchored at origin_y (above-grid bands tag row 0).
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
            // Additive light — this emitter has no other mode (see
            // [`GlowQuad::alpha`]).
            alpha: 0,
            color2: premul,
            alpha2: 0,
        });
        yy = band_end;
    }
}

/// The family's spectrum at the caret's column, so the block and the ribbon
/// leaving its cell resolve the same seven anchors.
///
/// `phase` is the ribbon's shared phase-ring clock on the locked path; a
/// standalone host passes the block's energy-responsive spin ([`IDLE_SPIN`] /
/// [`ACTIVE_SPIN`]) lifted by [`rainbow_phase_from_unit_turn`] onto one full
/// sweep. `off` steps further along the sweep (halo rings, glitter dots),
/// folded by the family's reflection so it never wraps violet into red.
#[inline]
fn spectrum_at(sweep: f32, off: f32) -> u32 {
    rainbow_thing_of(rainbow_sweep_reflect(sweep + off))
}

/// A family colour re-mixed at saturation `s` and value `v`, hue intact: each
/// channel is pulled toward the peak by `1 − s`, then scaled by `v`. At
/// `s = 1, v = 1` it is the identity, so a full-energy caret is exactly the
/// ribbon's colour (`caret_ribbon_and_streaks_share_one_spectrum`).
#[inline]
fn shade(rgb: u32, s: f32, v: f32) -> u32 {
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as f32,
        ((rgb >> 8) & 0xff) as f32,
        (rgb & 0xff) as f32,
    );
    let hi = r.max(g).max(b);
    let ch = |c: f32| (((hi - s * (hi - c)) * v) + 0.5).clamp(0.0, 255.0) as u32;
    (ch(r) << 16) | (ch(g) << 8) | ch(b)
}

/// How much of the energy range the caret's light floor takes to arrive. Short,
/// because the sparkle field competes from the first keystroke; the floor is
/// exactly zero at rest, where the block is the cursor's own colour.
const CARET_LIGHT_KNEE: f32 = 0.25;

/// Lift a colour toward white until its relative luminance reaches `floor`.
/// Toward white adds light without moving hue; only saturation drops.
/// Bisected (twenty halvings) because the sRGB transfer is nonlinear.
fn lift_to_light_floor(rgb: u32, floor: f32) -> u32 {
    let light = |c: u32| crate::color_math::relative_luminance(c) * 255.0;
    // A caller may ask above the displayable range (the raster backstop adds a
    // margin); white is the answer there.
    let floor = floor.clamp(0.0, 255.0);
    if floor <= 0.0 || light(rgb) >= floor {
        return rgb;
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
/// equalises. Two terms, because the glass draws the glow stream twice:
///
/// 1. `fs_glow` (aterm-gpu `renderer.rs`) adds the premultiplied bytes raw
///    (byte-exact [`aterm_render::add_sat`], as
///    [`crate::spectrum::compose_on_glass`] computes); the blit decodes to
///    linear, so this term is `s2l(page + b) − s2l(page)`.
/// 2. On an EDR panel the aurora pass (`fs_hdr_glow`, twin of
///    `aterm_render::hdr::hdr_additive_encode`) re-emits the quads as
///    `s2l(b) · HDR_GLOW_BOOST` above reference white: page-independent and
///    convex in the bytes.
///
/// Equalising only the first term leaves a visible swell on EDR panels. The
/// headroom clamp is not modelled (rim bytes are far below it). SDR panels and
/// the CPU renderer skip the aurora pass.
fn light_on_glass(page: u32, premul: u32) -> f32 {
    crate::color_math::relative_luminance(crate::spectrum::compose_on_glass(page, premul, 0))
        + aterm_render::hdr::HDR_GLOW_BOOST * crate::color_math::relative_luminance(premul)
}

/// One ring's premultiplied light, solved on the page: the rim's light law
/// ([`RIM_LIGHT`]).
///
/// Returns the premultiplied colour for a ring with `arc`'s hue at coverage
/// `cov` that adds to `page` the light a [`RIM_LIGHT_GREY`] ring would add.
/// Candidates are `arc` pulled toward black (`u < 0`) or white (`u > 0`), and
/// their light is read by [`light_on_glass`], so transfer, aurora and rounding
/// are all measured. Monotone in `u` to rounding, so bisection finds it; the
/// nearer bracketing candidate is returned.
///
/// `scale` multiplies the target; [`CursorRainbow::equalise_rim_light`]
/// bisects it for the whole pile.
fn ring_light_on_page(arc: u32, cov: u8, page: u32, scale: f32) -> u32 {
    // The byte and the law it encodes are one number; `rim_light_grey_is_the_arcs_mean`
    // pins it in the suite, this pins it on every debug call.
    debug_assert!(
        (crate::color_math::relative_luminance(RIM_LIGHT_GREY) - RIM_LIGHT).abs() < 0.005,
        "RIM_LIGHT_GREY is not the encode of RIM_LIGHT"
    );
    let light = |premul: u32| -> f32 { light_on_glass(page, premul) };
    let base = light_on_glass(page, 0);
    let target = (light(premul_rgb(RIM_LIGHT_GREY, cov)) - base) * scale;
    let candidate = |u: f32| -> u32 {
        if u < 0.0 {
            premul_rgb(mix_rgb(arc, 0x0000_0000, -u), cov)
        } else {
            premul_rgb(mix_rgb(arc, 0x00FF_FFFF, u), cov)
        }
    };
    let added = |u: f32| light(candidate(u)) - base;
    let (mut lo, mut hi) = (-1.0f32, 1.0f32);
    for _ in 0..22 {
        let mid = 0.5 * (lo + hi);
        if added(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (a, b) = (candidate(lo), candidate(hi));
    let err = |premul: u32| (light(premul) - base - target).abs();
    let mut best = if err(a) <= err(b) { a } else { b };
    // Then the last byte: the ray steps all three channels together, so its
    // light is quantised coarsely. Among triples one level away per channel
    // in the same hue sector, pick the nearest to the target (blue's small
    // weight makes it a fine knob).
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
    let mut best_err = err(best);
    for dr in -1..=1 {
        for dg in -1..=1 {
            for db in -1..=1 {
                let c = [seed[0] + dr, seed[1] + dg, seed[2] + db];
                if c.iter().any(|&v| !(0..=255).contains(&v)) {
                    continue;
                }
                if sector_of(c)
                    .iter()
                    .zip(sector)
                    .any(|(&now, was)| now != 0 && now != was)
                {
                    continue;
                }
                let packed = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | c[2] as u32;
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
        // Identity layout: origin 0, window extents == grid extents.
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
            // No host base: these fixtures pin the theme-polar bloom.
            base: None,
            head_rgb: None,
            // No host ribbon spine: the colour envelope is `energy`. The
            // `Some(_)` path is covered by `the_caret_cools_with_its_trail`.
            paint: None,
            // …and no host page either, so the light-law solves against the
            // shipped ground for the polarity each fixture names.
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
        // Settled: 2 s past the edge the spring is at 1.0 and the ring pop is
        // below f32 resolution — byte-identical to the plain config.
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
        // Mid-relax the fill is strictly between: no longer white, not yet home.
        let mid = t0 + Duration::from_millis(40);
        let mut m = Vec::new();
        let fm = CursorRainbow::default()
            .tick(Some((1, 1)), mid, 1.0, true, true, g, &flared, &mut m)
            .fill
            .unwrap();
        assert_ne!(fm, 0x00FF_FFFF, "40 ms in, the flare has left white");
        assert_ne!(Some(fm), fb, "40 ms in, the flare is not yet home");
    }

    /// §7.1 and D4, the landing's caret law on the real seam: after a v2
    /// meteor's one white frame the block mixes toward its own field stop
    /// (`spectrum(tri(field_t))`, phase-locked at the spawn) and stays there
    /// for the whole dwell, with no colour step, even when the abandoned band
    /// retires mid-dwell.
    ///
    /// Drives the host's order: `CursorGlow` with v2 engaged, then the caret
    /// at the same `now` with the `RainbowConfig` `app_render.rs` builds, at
    /// 120 Hz: nine typed cells, an idle long enough that the band's exit
    /// swoosh ends inside the dwell, a 20-cell nav jump and a 400 ms dwell.
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
            pack: None,
            ribbon_tall: true,
            ribbon_flat: false,
        };
        // The caret's config as the host builds it (`app_render.rs`): an
        // unpinned Default-theme cursor, the seam's head colour, paint and
        // flare, the page as the ground.
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
        // One host frame: the seam ticks, then the caret block reads it.
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

        const HZ: u64 = 8_333; // 120 Hz, in microseconds
        let t0 = Instant::now();
        let row = 2u16;
        let mut glow = CursorGlow::default();
        let mut body = CursorRainbow::default();
        frame(&mut glow, &mut body, &glow_cfg, g, t0, (row, 4));
        assert!(glow.v2_status().is_some(), "v2 must own the frame");
        // Nine typed cells at 70 ms: cols 4..=12, the caret parked at 13.
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
        // Idle 1.20 s at 120 Hz: the band is retracting when the jump comes
        // and retires at last-key + 1.54 s, 340 ms into the dwell.
        let mut now = last_key;
        let jump_at = last_key + Duration::from_millis(1_200);
        while now + Duration::from_micros(HZ) < jump_at {
            now += Duration::from_micros(HZ);
            frame(&mut glow, &mut body, &glow_cfg, g, now, (row, 13));
        }
        // The nav jump: 20 cells on the row (≥ JUMP_MIN_CELLS), credited.
        glow.note_motion(jump_at);
        let landing = (row, 33);
        let f0 = frame(&mut glow, &mut body, &glow_cfg, g, jump_at, landing);
        assert_eq!(
            glow.caret_flare_at(),
            Some(jump_at),
            "a credited jump flares on the frame it is observed"
        );
        assert_eq!(f0, 0x00FF_FFFF, "frame 0 of the landing is white");
        // The caret's own stop: the field the seam handed out ON the landing
        // frame, which is also the meteor's `t_land` (D4, §6.4).
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

        // The 400 ms dwell at 120 Hz, nothing pressed.
        let mut fills = vec![f0];
        let mut now = jump_at;
        let dwell_end = jump_at + Duration::from_millis(400);
        while now + Duration::from_micros(HZ) <= dwell_end {
            now += Duration::from_micros(HZ);
            fills.push(frame(&mut glow, &mut body, &glow_cfg, g, now, landing));
        }
        assert!(
            fills.len() >= 47,
            "120 Hz over 400 ms: {} frames",
            fills.len()
        );

        // Law 1, no pop: after the white frame every per-frame fill step is
        // under 40/255 per channel (spring-snap from white peaks near 27/255
        // per 120 Hz frame).
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

        // Law 2: the settled fill is the paint law applied to the caret's own
        // stop; a fresh block handed `spectrum(tri(field_t))` as head colour
        // emits the same byte.
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
    /// rounding. Derived so re-pacing the arc is not read as a regression; a
    /// real discontinuity lands far above it.
    fn continuity_ceiling(dt: f32) -> u32 {
        (crate::spectrum::spectrum_max_byte_rate() * dt).ceil() as u32 + 1
    }

    /// The same bound for a walk of the block's fill (the arc mixed with the
    /// base and lifted to the light floor), scanned from [`caret_law`] at the
    /// caller's own `dt` so it tracks palette and pace changes. The lift can be
    /// steeper than the colour it lifts; the `+ 1` is the tick's glint
    /// rounding.
    fn caret_continuity_ceiling(base: u32, mix: f32, dt: f32) -> u32 {
        // Oversampled: the caller's samples fall on an arbitrary grid of
        // spacing `dt`, and the largest step across a `dt` window need not land
        // on one fixed alignment. Comparing `at(i)` with `at(i + OVERSAMPLE)` on
        // a grid `OVERSAMPLE` times finer takes the supremum over alignments
        // (converged by 4).
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
        // …and one level for the gap between this reconstruction and the tick
        // the caller actually composes.
        worst + 1
    }

    /// The caret's colour law, composed: what the block emits for a base,
    /// rainbow and mix at energy `e`. The tick and every pin that
    /// reconstructs a fill go through it.
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

    /// The blinking-block variant: flips of the passed phase fire twinkles.
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
                    intensity: 1.0,
                    blinking: false,
                    base: None,
                    head_rgb: None,
                    paint: None,
                    ground: None,
                    flare_at: None,
                },
            ),
            (
                "zero intensity",
                RainbowConfig {
                    enabled: true,
                    intensity: 0.0,
                    blinking: false,
                    base: None,
                    head_rgb: None,
                    paint: None,
                    ground: None,
                    flare_at: None,
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
        // At idle on a dark theme the block stays bright/near-white (each channel high).
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
        // Under full energy the fill saturates: the min channel drops far below the max.
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
        // …and energy still blooms the spectrum over that base rather than
        // replacing the base's job: the hot fill is markedly more saturated.
        let c = RainbowConfig {
            base: Some(0x00FF_0000),
            ..cfg()
        };
        out.clear();
        let hot = CursorRainbow::default()
            .tick(Some((1, 1)), t, 1.0, true, true, g, &c, &mut out)
            .fill
            .unwrap();
        assert_ne!(hot, settled(0x00FF_0000, true, &mut out));
    }

    /// A host with real ribbon pixels is authoritative over a merely plausible
    /// phase-derived band. The cursor body and the innermost rim must wear the
    /// emitted head hue, while OSC 12 remains the colour they bloom from.
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
                // Standalone fixture: no host ribbon, so the caret reads the
                // sweep at its own column on the family clock.
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

    /// A light theme starts the block from near-BLACK (not white) when the host
    /// names no cursor colour of its own.
    #[test]
    fn light_theme_base_is_dark() {
        let g = geom();
        let c = cfg();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let f = cr
            .tick(
                Some((1, 1)),
                Instant::now(),
                0.0,
                true,
                false,
                g,
                &c,
                &mut out,
            )
            .fill
            .unwrap();
        let maxch = [(f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff]
            .into_iter()
            .max()
            .unwrap();
        assert!(
            maxch < 90,
            "idle block near black on a light theme, got {f:#08x}"
        );
    }

    /// Energy drives both the halo brightness and the hue-spin rate: a hot run spins
    /// faster and glows brighter than a cool one over the same wall-clock.
    #[test]
    fn energy_spins_faster_and_glows_brighter() {
        let g = geom();
        let c = cfg();
        let step = Duration::from_millis(16);
        let run = |energy: f32| -> (f32, u64) {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            let mut t = Instant::now();
            cr.tick(Some((2, 2)), t, energy, true, true, g, &c, &mut out); // seed last
            let mut ink = 0u64;
            for _ in 0..30 {
                t += step;
                out.clear();
                cr.tick(Some((2, 2)), t, energy, true, true, g, &c, &mut out);
                ink += out
                    .iter()
                    .map(|q| {
                        (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff))
                            as u64
                    })
                    .sum::<u64>();
            }
            (cr.phase, ink)
        };
        let (cool_phase, cool_ink) = run(0.05);
        let (hot_phase, hot_ink) = run(1.0);
        assert!(
            hot_phase > cool_phase + 0.2,
            "hot spins the hue faster ({hot_phase} vs {cool_phase})"
        );
        assert!(
            hot_ink > cool_ink * 2,
            "hot glows far brighter ({hot_ink} vs {cool_ink})"
        );
    }

    /// Every emitted halo quad is single-row and inside the grid interior (the renderer
    /// row-gate + parity invariant), and additive coverage is bounded for legibility.
    #[test]
    fn halo_quads_respect_grid_and_cap() {
        let g = geom();
        let c = cfg();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let t = Instant::now();
        // A cell at the grid edge so clamping is exercised.
        cr.tick(Some((0, 0)), t, 1.0, true, true, g, &c, &mut out);
        cr.tick(
            Some((0, 0)),
            t + Duration::from_millis(16),
            1.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        let gw = (g.cols * g.cw) as u32;
        let gh = (g.rows * g.ch) as u32;
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
                assert!((q.color >> sh) & 0xff <= 180, "halo coverage capped: {q:?}");
            }
        }
    }

    /// Charged ⇒ active (host keeps the tick armed); once energy settles it reports
    /// inactive so the focused idle cursor stops forcing 60 fps wakeups.
    #[test]
    fn settles_to_inactive_when_energy_drops() {
        let g = geom();
        let c = cfg();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let t = Instant::now();
        cr.tick(Some((1, 1)), t, 0.8, true, true, g, &c, &mut out);
        assert!(cr.is_active(), "charged cursor keeps the animation armed");
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(
            !cr.is_active(),
            "settled cursor idles (sits solid inside its resting rim)"
        );
    }

    /// Settled cursor pixels are frame-gap invariant: sparse captures and a
    /// fresh charge after a long idle never integrate an unpresented clock
    /// slice.
    #[test]
    fn settled_present_gaps_are_byte_identical_and_resume_without_a_snap() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((1, 1)), t, 0.8, true, true, g, &c, &mut out);
        out.clear();
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.8,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        out.clear();
        let first = cr.tick(
            Some((1, 1)),
            t + Duration::from_secs(5),
            0.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        let first_quads = out.clone();
        let settled_phase = cr.phase;
        out.clear();
        let late = cr.tick(
            Some((1, 1)),
            t + Duration::from_secs(30),
            0.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        assert_eq!(late.fill, first.fill);
        assert_eq!(late.fp, first.fp);
        assert_eq!(out, first_quads);
        assert_eq!(cr.phase, settled_phase);
        assert!(!cr.is_active());

        out.clear();
        let resumed = cr.tick(
            Some((1, 1)),
            t + Duration::from_secs(31),
            0.8,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        assert_eq!(
            cr.phase, settled_phase,
            "the first resumed frame cannot charge for the idle gap"
        );
        assert_eq!(
            resumed.fill,
            cr.tick(
                Some((1, 1)),
                t + Duration::from_secs(31),
                0.8,
                true,
                true,
                g,
                &c,
                &mut Vec::new(),
            )
            .fill,
            "re-sampling the same instant is stable"
        );
    }

    // ───────────────────────── the twinkle (glitter star) ─────────────────────────

    /// An upward crossing of a momentum rung while charged fires a twinkle:
    /// star quads land in the scratch and the fill glints brighter than the
    /// unflared rainbow. The phase argument is inert (constant `true` below).
    #[test]
    fn momentum_rung_crossing_fires_twinkle_star() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        // Charged, but under the first rung: no star yet.
        cr.tick(Some((2, 20)), t, 0.1, true, true, g, &c, &mut out);
        assert!(
            cr.is_active(),
            "typing energy already owns the frame cadence"
        );
        assert!(!cr.twinkling, "under the first rung nothing flares");
        let calm_quads = out.len();
        // The paint climbs through MOMENTUM_FLARE_STEPS[0]: the star flares.
        out.clear();
        let cross = t + Duration::from_millis(16);
        let mid = cross + Duration::from_secs_f32(TWINKLE_DUR / 2.0);
        cr.tick(Some((2, 20)), cross, 0.3, true, true, g, &c, &mut out);
        assert!(cr.twinkling, "a rung crossing arms the flare");
        assert_eq!(cr.flare_rung, 1, "the first rung is held");
        out.clear();
        let flared = cr
            .tick(Some((2, 20)), mid, 0.3, true, true, g, &c, &mut out)
            .fill
            .unwrap();
        // Measured against a steady-block twin on the identical clock and
        // paint (it cannot flare), so the paint's own mix is not mistaken for
        // the glint.
        let steady = cfg();
        let mut twin = CursorRainbow::default();
        let mut twin_out = Vec::new();
        twin.tick(Some((2, 20)), t, 0.1, true, true, g, &steady, &mut twin_out);
        twin.tick(
            Some((2, 20)),
            cross,
            0.3,
            true,
            true,
            g,
            &steady,
            &mut twin_out,
        );
        let calm = twin
            .tick(
                Some((2, 20)),
                mid,
                0.3,
                true,
                true,
                g,
                &steady,
                &mut twin_out,
            )
            .fill
            .unwrap();
        assert!(!twin.twinkling, "the steady twin never flares");
        assert!(
            out.len() > calm_quads,
            "the flare adds star quads over the idle halo ({} vs {calm_quads})",
            out.len()
        );
        let minch = |c: u32| {
            [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
                .into_iter()
                .min()
                .unwrap()
        };
        assert!(
            minch(flared) >= minch(calm),
            "mid-flare the dark-theme fill glints toward white ({flared:#08x} vs {calm:#08x})"
        );
        assert_ne!(flared, calm, "the glint visibly changes the fill");
    }

    /// Idle-zero: phase flips at settled energy never arm the effect timer.
    /// The phase is not read and a resting paint (exactly 0) crosses no rung;
    /// twenty half-periods leave the animator idle after every flip.
    #[test]
    fn idle_phase_flips_never_arm_effect_timer() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let mut phase = true;
        cr.tick(Some((1, 1)), t, 0.0, phase, true, g, &c, &mut out);
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
                &c,
                &mut out,
            );
            assert!(!cr.is_active(), "idle flip {i} armed an effect wake");
            assert!(cr.twinkle_at.is_none(), "idle flip {i} armed a flare");
            assert_eq!(cr.twinkle_seq, 0, "idle flips consumed flare identities");
        }
    }

    /// Tier-1: project the real animator's flare generation counter through a
    /// charged-flare → cool → idle-tick trace. The idle tick lands while the
    /// earlier flare is active, so a Boolean-only projection would see
    /// `twinkle == 1` both before and after; `twinkle_seq` makes a forbidden
    /// restart observable.
    ///
    /// The model's `BlinkCharged` / `BlinkIdle` map to an upward momentum-rung
    /// crossing while charged and any tick at rest: a charged event starts one
    /// generation, an idle event never restarts one, and every flare finishes.
    #[test]
    fn idle_blink_transition_conforms_to_model() {
        let model = aterm_spec::derive::rainbow_idle_twinkle_model();
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
            // Two abstract fuel ticks split the real flare window in half,
            // derived from the real timestamp: once `tick` clears
            // `twinkle_at`, the projection is 0.
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

        // Reach Charge from the model's genuine initial state: charged
        // (above SETTLED_ENERGY) but under the first momentum rung, so this
        // first engine tick flares nothing.
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

        // A charged rung crossing (0.2 → 0.8 crosses three rungs in one tick)
        // starts generation 1 — one generation, not three.
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

        // Cooling does not finish the still-young flare.
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

        // An idle tick while generation 1 is active must preserve it; the
        // restart observation comes from the real counter delta.
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

        // Negative control: keep the coarse twinkle Boolean at 1 but advance
        // the generation as a buggy idle restart would. The corrupted countdown
        // also shows the fuel obligation is non-vacuous.
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

        // The real clock crosses the flare's halfway point with one fuel tick
        // left, then `TWINKLE_DUR` clears the arm: the real Age/Finish
        // transitions behind `CanFinish`.
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
        // Identical clocks and identical energy; only the host paint differs,
        // by the width of a rung edge: 0.25 reaches MOMENTUM_FLARE_STEPS[0],
        // 0.24 does not. Both start from rest so neither twin integrates a
        // spin or a phase before the edge.
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
        // Past the flare end: both settle and emit identical light.
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

    /// A steady block never twinkles: with `blinking: false` a full momentum
    /// wind-up fires nothing, and the phase argument is not read.
    #[test]
    fn steady_block_never_twinkles() {
        let g = geom();
        let c = cfg(); // blinking: false
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((1, 1)), t, 0.0, true, true, g, &c, &mut out);
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            false,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(!cr.is_active(), "a steady block's phase flips fire nothing");
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(32),
            1.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(
            cr.twinkle_at.is_none(),
            "a steady block's wind-up fires no star"
        );
        assert_eq!(cr.twinkle_seq, 0, "no flare identity was consumed");
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

        // The release: paint falling on the spine's own τ from 1.0 toward 0,
        // sampled at 60 Hz. The innermost ring's coverage never rises.
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
            // The innermost ring is the brightest quad in the stream.
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
        // Roughly RIM_SPIN_TURNS_PER_S × 29 frames × 16 ms, on the dot: no
        // hidden clock, no first-frame integration.
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
        let ceiling = continuity_ceiling(RIM_SPIN_TURNS_PER_S * 0.016) + 2; // + premul rounding
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

    /// Reduced motion (`intensity == 0`) keeps the twinkle provably off too —
    /// the host then leaves the shape un-pinned and the plain blink returns.
    #[test]
    fn reduced_motion_keeps_plain_blink() {
        let g = geom();
        let c = RainbowConfig {
            intensity: 0.0,
            ..blink_cfg()
        };
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((1, 1)), t, 0.0, true, true, g, &c, &mut out);
        let f = cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.0,
            false,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(f.fill.is_none(), "inert ⇒ the host keeps the plain blink");
        assert_eq!(f.fp, 0);
        assert!(out.is_empty(), "no star under reduced motion");
        assert!(!cr.is_active());
    }

    /// Star quads obey the halo's discipline: single-row, grid-interior,
    /// coverage-capped, and within about half a cell of the block on both
    /// sides.
    #[test]
    fn twinkle_star_hugs_and_caps() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        // Under the first rung, then a crossing: the flare fires on tick two.
        cr.tick(Some((2, 20)), t, 0.1, true, true, g, &c, &mut out);
        cr.tick(
            Some((2, 20)),
            t + Duration::from_millis(16),
            0.8,
            false,
            true,
            g,
            &c,
            &mut out,
        );
        out.clear();
        // Mid-flare at full pop: the widest reach + brightest light of the flare.
        cr.tick(
            Some((2, 20)),
            t + Duration::from_millis(16) + Duration::from_secs_f32(TWINKLE_DUR / 2.0),
            0.8,
            false,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(!out.is_empty(), "mid-flare the star is lit");
        let cw = g.cw as i32;
        let (cell_l, cell_r) = (20 * cw, 21 * cw);
        let max_reach = cw / 2 + 1;
        for q in &out {
            let band = q.row as u32 * g.ch as u32;
            assert!(
                q.y as u32 >= band && q.y as u32 + q.h as u32 <= band + g.ch as u32,
                "single-row: {q:?}"
            );
            assert!(
                cell_l - q.x as i32 <= max_reach && (q.x as i32 + q.w as i32) - cell_r <= max_reach,
                "star hugs within half a cell: {q:?}"
            );
            for sh in [16, 8, 0] {
                assert!((q.color >> sh) & 0xff <= 180, "coverage capped: {q:?}");
            }
        }
    }

    /// The twinkle is a pure clock function: identical instants and paint
    /// trajectories give byte-identical quads and equal fingerprints.
    /// It also pins the photosensitivity bound as a rate, so retuning either
    /// constant cannot reintroduce a strobe above WCAG 2.3.1's threshold.
    #[test]
    fn twinkle_flash_rate_stays_under_the_photosensitivity_bound() {
        let hz = TWINKLE_SCINT / TWINKLE_DUR;
        assert!(
            hz <= 3.2,
            "twinkle scintillation is {hz} Hz — over the 3 Hz general-flash bound"
        );
        // It must still wobble: a zero rate would remove the glint. Checked at
        // build time.
        const {
            assert!(TWINKLE_SCINT > 0.0, "the flare must still scintillate");
        }
    }

    #[test]
    fn twinkle_is_deterministic() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let run = || {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            let mut fps = Vec::new();
            for i in 0..40u64 {
                // A wind-up every ~256 ms: 0.1 → 0.8 crosses three rungs
                // (one flare), 0.8 → 0.1 releases them all.
                let energy = if (i / 8) % 2 == 0 { 0.8 } else { 0.1 };
                let f = cr.tick(
                    Some((2, 10)),
                    t + Duration::from_millis(i * 16),
                    energy,
                    true,
                    true,
                    g,
                    &c,
                    &mut out,
                );
                fps.push(f.fp);
            }
            assert_eq!(cr.twinkle_seq, 3, "three wind-ups, three flares");
            (out, fps)
        };
        let (out_a, fps_a) = run();
        let (out_b, fps_b) = run();
        assert_eq!(out_a, out_b, "identical clocks ⇒ identical quads");
        assert_eq!(fps_a, fps_b, "identical clocks ⇒ identical fingerprints");
        assert!(
            fps_a.windows(2).any(|w| w[0] != w[1]),
            "a mid-flare fingerprint steps every frame"
        );
    }

    /// On a light theme the glint goes toward the vivid hue, not white, which
    /// would sink into a light background.
    #[test]
    fn light_theme_glint_stays_saturated() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        // Under the first rung, then a crossing: the flare fires on tick two.
        cr.tick(Some((1, 1)), t, 0.1, true, false, g, &c, &mut out);
        cr.tick(
            Some((1, 1)),
            t + Duration::from_millis(16),
            0.8,
            false,
            false,
            g,
            &c,
            &mut out,
        );
        let mid = t + Duration::from_millis(16) + Duration::from_secs_f32(TWINKLE_DUR / 2.0);
        let f = cr
            .tick(Some((1, 1)), mid, 0.8, false, false, g, &c, &mut out)
            .fill
            .unwrap();
        let minch = [(f >> 16) & 0xff, (f >> 8) & 0xff, f & 0xff]
            .into_iter()
            .min()
            .unwrap();
        assert!(
            minch < 160,
            "the light-theme glint keeps saturation (never washes to white), got {f:#08x}"
        );
    }

    /// A standalone caret keeps its energy-responsive, about one-turn-per-second
    /// clock, and one unit turn traverses one complete period-two family sweep.
    ///
    /// Drives 1.28 s of real ticks across the unit wrap and checks both sides.
    /// The direct-unit negative control shows the smoothness bound is not
    /// vacuous, and visiting many interpolated colours shows the colour moves.
    #[test]
    fn standalone_stream_crosses_its_one_second_wrap_without_a_spectrum_jump() {
        let g = geom();
        let c = cfg();
        let col = 11u16;
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        cr.tick(Some((2, col)), t0, 1.0, false, true, g, &c, &mut out);

        let mut previous_turn = cr.phase;
        let mut previous_sweep = rainbow_sweep_at(col, rainbow_phase_from_unit_turn(cr.phase));
        let mut max_sweep_step = 0.0f32;
        let mut wraps = 0usize;
        let mut wrong_domain_disagreements = 0usize;
        let mut colours = BTreeSet::new();
        let mut last_at = t0;
        for frame in 1..=80u64 {
            last_at = t0 + Duration::from_millis(frame * 16);
            out.clear();
            let emitted = cr.tick(Some((2, col)), last_at, 1.0, false, true, g, &c, &mut out);
            let family_phase = rainbow_phase_from_unit_turn(cr.phase);
            let sweep = rainbow_sweep_at(col, family_phase);
            max_sweep_step = max_sweep_step.max((sweep - previous_sweep).abs());
            wraps += usize::from(cr.phase < previous_turn);
            previous_turn = cr.phase;
            previous_sweep = sweep;

            let colour = spectrum_at(sweep, 0.0);
            colours.insert(colour);
            assert_white_lift_of(
                emitted.fill.expect("enabled caret fill"),
                caret_law(BASE_DARK_THEME, colour, MIX_MAX, 1.0),
                &format!("frame {frame}: emitted caret must use the lifted family phase"),
            );
            wrong_domain_disagreements +=
                usize::from(spectrum_at(rainbow_sweep_at(col, cr.phase), 0.0) != colour);
        }

        assert!(
            last_at.saturating_duration_since(t0) > Duration::from_secs(1),
            "the run must cross the one-second unit-turn wrap"
        );
        assert!(
            wraps >= 1,
            "the private unit clock must really wrap: {wraps}"
        );
        assert!(
            max_sweep_step < 0.05,
            "the reflected spectrum jumped by {max_sweep_step} at a 16 ms step"
        );
        assert!(
            colours.len() > 60,
            "one complete stream must visit a continuous spectrum: {} colours",
            colours.len()
        );
        assert!(
            wrong_domain_disagreements > 40,
            "negative control: a raw unit clock must visibly disagree with the family domain"
        );
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
        // The sweep's own step across one sample of this walk, measured from
        // the sweep function rather than assumed.
        let dt = (rainbow_sweep_at(17, 6.0 / 2400.0) - rainbow_sweep_at(17, 0.0)).abs();
        // The fill's own ceiling, not the arc's: the block is the arc mixed
        // toward `BASE_DARK_THEME` at `MIX_MAX` (full energy), then held to
        // §2.3 on the emitted byte.
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

    /// The rim's light is a function of the paint and nothing else: the pin
    /// behind [`RIM_LIGHT`], rasterized because the rim is a pile.
    ///
    /// For every shipped caret base, over both shipped dark pages, at five
    /// paints, the rim is laid at 129 positions along the family sweep (every
    /// hue the spin can park it on) with a fresh engine at one `now` and
    /// rasterized over the page through the family's blend. The pile's light
    /// (summed relative-luminance excess over the page) must agree across
    /// positions within [`RIM_LIGHT_TOLERANCE`] and rise strictly with paint.
    ///
    /// Non-vacuity: the raw arc
    /// (`shade(spectrum_at(sweep, 0), SAT_MAX, VAL_MAX)`) must spread far
    /// beyond the tolerance.
    #[test]
    fn the_rim_light_is_hue_invariant_and_monotone_in_paint() {
        /// Ratio `max/min` of the pile's light across the sweep. The budget is
        /// the premultiply's rounding: the two bracketing candidates are one
        /// byte apart on the inner ring, so the nearest pile sits within about
        /// ±2 % of the grey's. The census lines print live numbers.
        /// (`RIM_LIGHT_TOLERANCE_IDLE` covers the resting ember at coverage 5,
        /// where one byte is a fifth of the light.)
        const RIM_LIGHT_TOLERANCE: f64 = 1.05;
        const RIM_LIGHT_TOLERANCE_IDLE: f64 = 1.25;
        const GROUNDS: [u32; 2] = [0x0011_1318, 0x001A_1B26];
        const PAINTS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
        const POSITIONS: u32 = 128;
        let lum = |rgb: u32| f64::from(crate::color_math::relative_luminance(rgb));
        // (glass light, byte-composite light alone) — the second is what an SDR
        // panel or the CPU renderer shows; printed, not asserted.
        let raster_light = |quads: &[GlowQuad], ground: u32| -> (f64, f64) {
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
            for q in quads {
                x0 = x0.min(u32::from(q.x));
                y0 = y0.min(u32::from(q.y));
                x1 = x1.max(u32::from(q.x) + u32::from(q.w));
                y1 = y1.max(u32::from(q.y) + u32::from(q.h));
            }
            if x1 <= x0 || y1 <= y0 {
                return (0.0, 0.0);
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
            (
                bytes + f64::from(aterm_render::hdr::HDR_GLOW_BOOST) * aurora,
                bytes,
            )
        };

        let g = geom();
        let now = Instant::now();
        let mut worst_spread = [1.0f64; PAINTS.len()];
        let mut worst_at: [String; PAINTS.len()] = Default::default();
        // The byte-composite term alone — an SDR panel's or the CPU renderer's
        // reading — under the same law. Printed beside the glass figure.
        let mut worst_bytes_spread = [1.0f64; PAINTS.len()];
        let mut cells = 0usize;
        for (name, base) in shipped_caret_bases() {
            for ground in GROUNDS {
                // light(position, paint)
                let mut light = vec![[0.0f64; PAINTS.len()]; POSITIONS as usize + 1];
                let mut bytes = vec![[0.0f64; PAINTS.len()]; POSITIONS as usize + 1];
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
                        // Energy 0: the paint is the only envelope, the breath
                        // is constant 0.5 and no flare can fire
                        // (`blinking: false`).
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
                        let (glass, byte_add) = raster_light(&out, ground);
                        light[step as usize][pi] = glass;
                        bytes[step as usize][pi] = byte_add;
                        cells += 1;
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
                    let col: Vec<f64> = bytes.iter().map(|row| row[pi]).collect();
                    let lo = col.iter().copied().fold(f64::INFINITY, f64::min);
                    let hi = col.iter().copied().fold(0.0, f64::max);
                    if lo > 0.0 {
                        worst_bytes_spread[pi] = worst_bytes_spread[pi].max(hi / lo);
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
            println!(
                "CARET-RIM-LIGHT-CENSUS cells={cells} paint={paint} worst_spread=x{:.4} at {} \
                 (byte-add term alone, SDR/CPU: x{:.4})",
                worst_spread[pi], worst_at[pi], worst_bytes_spread[pi]
            );
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

        // Non-vacuity: the raw arc
        // (`shade(spectrum_at(sweep, off), sat, val)` at full paint) spans far
        // more than the tolerance in light.
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
        // The green→blue crossing in particular: the arc's own light steps by
        // more than ×2 between its flanks, so a rim wearing the raw arc would
        // change brightness crossing it with the paint unchanged.
        let mid = crate::spectrum::spectrum_crossing_position();
        // The flanks are the leg's two anchors, green and blue.
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
        println!(
            "CARET-RIM-LIGHT-CROSSING t={mid:.4}±{half:.4} flanks {flank_lo:.4} / {flank_hi:.4} \
             step x{:.2}",
            bright / dim
        );
        assert!(
            bright / dim > 2.0,
            "the raw arc's light steps only ×{:.2} across the crossing — the incident \
             this law answers for could not have happened on it",
            bright / dim
        );
    }

    /// [`RIM_LIGHT_GREY`] encodes [`RIM_LIGHT`], and [`RIM_LIGHT`] is the arc's
    /// mean light at full chroma, derived here so a re-paced arc or re-typed
    /// byte is caught.
    #[test]
    fn rim_light_grey_is_the_arcs_mean() {
        let lum = |rgb: u32| crate::color_math::relative_luminance(rgb);
        assert!(
            (lum(RIM_LIGHT_GREY) - RIM_LIGHT).abs() < 0.005,
            "#{RIM_LIGHT_GREY:06X} has relative luminance {}, not {RIM_LIGHT}",
            lum(RIM_LIGHT_GREY)
        );
        let n = 400;
        let mean = (0..=n)
            .map(|i| {
                lum(shade(
                    spectrum_at(i as f32 / n as f32, 0.0),
                    SAT_MAX,
                    VAL_MAX,
                ))
            })
            .sum::<f32>()
            / (n + 1) as f32;
        assert!(
            (mean - RIM_LIGHT).abs() < 0.02,
            "the arc's mean light at full chroma is {mean:.3}; RIM_LIGHT is {RIM_LIGHT}"
        );
    }

    /// The caret cools with its trail, not with the ignition heat.
    ///
    /// After a 43-key burst, the ribbon's momentum spine is still high while
    /// `TypingCadence::intensity` is already zero; a caret riding the cadence
    /// drops to its idle mix (the theme's cursor colour) beside a fully painted
    /// ribbon. The burst drives the real
    /// [`crate::cursor_trail::TypingCadence`] and
    /// [`crate::typing_momentum::TypingMomentum`], so "cadence dead, spine
    /// alive" is measured on the real clocks and re-derives on retune.
    ///
    /// Negative control: `paint: None` on the same frames still collapses to
    /// the base.
    #[test]
    fn the_caret_cools_with_its_trail_and_not_with_the_ignition_heat() {
        use crate::cursor_trail::TypingCadence;
        use crate::typing_momentum::TypingMomentum;

        /// The shipped Default theme's `cursor_color`, which is what
        /// `app_render` hands the block as `base`.
        const BASE: u32 = 0x0050_FA7B;
        /// The laid field the capture reported.
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

        // The colour the ribbon lays at this field — the thing the caret is
        // supposed to be wearing.
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

        // ── the mechanism, measured off the two shipped clocks ──────────────
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

        // ── negative control: colour on the cadence alone ───────────────────
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

        // ── the fix ─────────────────────────────────────────────────────────
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

        // ── the release: one continuous walk down the spine ─────────────────
        //
        // The ceiling is derived from the spine's own step. The colour terms
        // it drives span `MIX_MAX - MIX_IDLE`, `SAT_MAX - SAT_IDLE` and
        // `VAL_MAX - VAL_IDLE`, each moving a channel by at most 255 per unit.
        // Four flat levels are added: `2` of `f32 -> u8` rounding (`shade` and
        // `mix_rgb` at each end of a step) and `2` for
        // [`lift_to_light_floor`]'s per-frame re-solve, which is a regression
        // pin on the shipped caret rather than a derivation.
        //
        // A caret that snapped to its base moves tens of levels in a step the
        // spine barely moved.
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
            // Continuity is claimed where the light floor is constant
            // (`paint >= CARET_LIGHT_KNEE`); the floor's short knee has its own
            // contract.
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
                // It only cools: the distance back to the cursor's colour never
                // grows during a release beyond the floor's own re-solve (the
                // rim fans on the paint, so its peak light is not monotone and
                // the lift can tick up by a level). A caret snapping back toward
                // its base moves tens of levels and still fails.
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

        // ── and it really does come home ────────────────────────────────────
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
    /// automatically). Plus three achromatic entries: `None` (the raw/embedder
    /// path) and the two theme-polar literals.
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

    /// Outer halo rings and the two glitter dots take offsets along the same
    /// spectrum, and each offset interpolates continuously too.
    #[test]
    fn halo_and_glitter_offsets_have_no_anchor_colour_steps() {
        for off in [
            HALO_HUE_SPREAD_IDLE / 3.0,
            2.0 * HALO_HUE_SPREAD_IDLE / 3.0,
            HALO_HUE_SPREAD_MAX / 3.0,
            2.0 * HALO_HUE_SPREAD_MAX / 3.0,
            HALO_HUE_SPREAD_MAX,
            0.13,
            0.42,
        ] {
            let mut previous = spectrum_at(rainbow_sweep_at(17, 0.0), off);
            let mut max_step = 0;
            for sample in 1..=2400 {
                let phase = sample as f32 * (6.0 / 2400.0);
                let colour = spectrum_at(rainbow_sweep_at(17, phase), off);
                max_step = max_step.max(rgb_max_delta(previous, colour));
                previous = colour;
            }
            let dt = (rainbow_sweep_at(17, 6.0 / 2400.0) - rainbow_sweep_at(17, 0.0)).abs();
            let ceiling = continuity_ceiling(dt);
            assert!(
                max_step <= ceiling,
                "offset {off} jumped one spectrum channel by {max_step} \
                 (ceiling {ceiling})"
            );
        }
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
            pack: None,
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
            // The caret reads the position the glow engine is about to lay
            // (§2.1).
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
            // The caret walks the arc while the mark unrolls (seven distinct
            // stops) and then holds the head (owner, 2026-08-30).
            colours.len() >= 5,
            "the streaming run must traverse the unroll's colours: {colours:#x?}"
        );
    }

    /// One rainbow from the caret outward: the block and its halo resolve one
    /// reflected spectrum position through one continuous interpolation.
    ///
    ///   1. the caret's spectrum lookup is deterministic at the family's
    ///      column and phase;
    ///   2. the energy law is a pure shade of that colour, the identity at
    ///      full energy, so the agreement is exact;
    ///   3. a hot block's fill is the theme base mixed toward exactly that
    ///      interpolated colour.
    #[test]
    fn caret_uses_the_continuous_family_spectrum_end_to_end() {
        for &col in &[0u16, 1, 5, 17, 22, 39, 137, 400] {
            for i in 0..9 {
                let phase = i as f32 * 0.37;
                let colour = spectrum_at(rainbow_sweep_at(col, phase), 0.0);
                // (2) full energy ⇒ the shade is the identity.
                assert_eq!(
                    shade(colour, SAT_MAX, VAL_MAX),
                    colour,
                    "the energy law recolours nothing at full energy ({colour:06X})"
                );
            }
        }
        // (3) end to end: a hot block's fill is the base mixed toward the
        // continuous family colour at that very cell.
        let g = geom();
        let c = cfg();
        for &col in &[3u16, 11, 30] {
            let mut cr = CursorRainbow::default();
            let mut out = Vec::new();
            let f = cr
                .tick(
                    Some((1, col)),
                    Instant::now(),
                    1.0,
                    true,
                    true,
                    g,
                    &c,
                    &mut out,
                )
                .fill
                .unwrap();
            assert_white_lift_of(
                f,
                caret_law(
                    BASE_DARK_THEME,
                    // The standalone path (`tick`, no host field): the caret
                    // resolves the sweep at its own column on its own clock.
                    spectrum_at(
                        rainbow_sweep_at(col, rainbow_phase_from_unit_turn(cr.phase)),
                        0.0,
                    ),
                    MIX_MAX,
                    1.0,
                ),
                &format!("the caret at col {col} uses the family spectrum at col {col}"),
            );
        }
    }

    /// The additive halo hugs the block: at full energy no ring reaches more
    /// than about half a cell width past the cursor cell.
    #[test]
    fn halo_hugs_within_half_a_cell_width() {
        let g = geom();
        let c = cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        // A mid-row cell with room on both sides so clamping doesn't mask the reach.
        cr.tick(Some((2, 20)), t, 1.0, true, true, g, &c, &mut out);
        out.clear();
        cr.tick(
            Some((2, 20)),
            t + Duration::from_millis(16),
            1.0,
            true,
            true,
            g,
            &c,
            &mut out,
        );
        assert!(!out.is_empty(), "a hot cursor glows");
        let cw = g.cw as i32;
        let cell_l = 20 * cw; // the cursor cell's left edge
        let cell_r = 21 * cw; // the cursor cell's right edge
        let max_reach = cw / 2 + 1; // ≤ half a cell (+1 px innermost bias)
        for q in &out {
            let ql = q.x as i32;
            let qr = q.x as i32 + q.w as i32;
            assert!(
                cell_l - ql <= max_reach,
                "halo reaches too far LEFT into the neighbour ({}px): {q:?}",
                cell_l - ql
            );
            assert!(
                qr - cell_r <= max_reach,
                "halo reaches too far RIGHT into the neighbour ({}px): {q:?}",
                qr - cell_r
            );
        }
    }

    /// The settle split is a partition: [`CursorRainbow::settling`] is disjoint
    /// from [`CursorRainbow::is_active`], and together they are exactly
    /// `energy > SETTLED || paint > SETTLED || twinkling` for every latched
    /// combination, with the last two frames equal or not.
    #[test]
    fn the_settle_split_is_a_partition_of_still_moving() {
        let levels = [0.0_f32, SETTLED_ENERGY, SETTLED_ENERGY + 1e-4, 0.3, 1.0];
        for &energy in &levels {
            for &paint in &levels {
                for twinkling in [false, true] {
                    for repeated in [false, true] {
                        let rb = CursorRainbow {
                            energy,
                            paint,
                            twinkling,
                            fp_last: 7,
                            fp_prev: if repeated { 7 } else { 8 },
                            ..CursorRainbow::default()
                        };
                        let moving = energy > SETTLED_ENERGY || paint > SETTLED_ENERGY || twinkling;
                        let at = format!(
                            "energy={energy} paint={paint} twinkling={twinkling} \
                             repeated={repeated}"
                        );
                        assert_eq!(rb.is_active() || rb.settling(), moving, "cover: {at}");
                        assert!(!(rb.is_active() && rb.settling()), "disjoint: {at}");
                    }
                }
            }
        }
    }

    /// The host spine integrated at 1 ms from a burst that reached `peak`: the
    /// momentum metric decaying on its 2.0 s tau, chased by the ribbon's
    /// display ease (0.85 s on a fall), snapping at 0.005. Every twin samples
    /// this one truth, so the tests measure the schedule, not sampling.
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

    /// The colour release is drawn to its last step, on demand.
    ///
    /// The body settles quickly but the caret keeps cooling on the paint spine
    /// one u8 step every few frames, so two identical frames arrive long before
    /// the fade ends. Three twins read the same host truth per hand (burst
    /// peak, lane period, theme, caret base):
    ///
    /// * level: ticked every frame until the caret releases
    ///   (`!is_active() && !settling()`); the ground truth.
    /// * park-on-inactive: ticked while `is_active()` and parked on the first
    ///   `false`; the negative control, which parks mid-fade.
    /// * demand: the frame train while `is_active()`,
    ///   [`CursorRainbow::next_change_deadline`] while `settling()`.
    ///
    /// Demand must release on the same frame as level, and a copy forked at
    /// each wake and ticked every frame with the true inputs may not change
    /// before the next wake.
    #[test]
    fn the_caret_colour_release_is_drawn_to_its_last_step_on_demand() {
        const HEAD: u32 = 0x00FF_5A4E;
        // (burst peak, lane period ms, dark theme, caret base, blinking block)
        let hands: [(f32, u64, bool, Option<u32>, bool); 6] = [
            // A long run on the shipped 120 Hz lane, the Default theme's cursor.
            (1.00, 8, true, Some(0x0050_FA7B), true),
            // The same on a 60 Hz lane.
            (1.00, 16, true, Some(0x0050_FA7B), true),
            // A short word: the spine never reaches 1.
            (0.45, 8, true, Some(0x0050_FA7B), false),
            // The light theme's near-black block, whose mix runs to 0.95.
            (1.00, 8, false, Some(0x0016_161C), true),
            // No host cursor colour: the theme-polar bloom.
            (0.70, 16, true, None, false),
            // A 30 Hz lane, the coarsest the shipped divisor produces.
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
            // The cadence's ignition heat: a 220 ms half-life.
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

            // Level: every frame until the caret's own release.
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

            // Negative control: park on the first frame the fingerprint law
            // lets go of.
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

            // Demand: the host rule after the port.
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
                // The fork: a frame-cadence lane from here would draw this
                // caret at every frame the demand lane sleeps through.
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
            println!(
                "hand {hand}: level {} ticks / {} visible steps, release {level_release} ms; \
                 main's rule parked at {parked_at} ms ({never_drawn} steps never drawn); \
                 demand {demand_ticks} ticks",
                level_fp.len(),
                changes.len()
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
        assert!(rb.settle_memo.is_some(), "the answer is memoised");
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
        assert!(rb.settle_memo.is_none(), "a real tick clears the memo");
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
            rb.settle_probe.is_none() && !rb.settling(),
            "an inert tick drops the latched inputs and the release with them"
        );
        assert_eq!(rb.next_change_deadline(first + frame, frame), None);
    }
}
