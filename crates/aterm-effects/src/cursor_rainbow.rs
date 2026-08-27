// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typing-reactive RAINBOW CURSOR — the block cursor glows and evolves colour
//! with your typing momentum. An ENERGY value (the caller passes
//! [`crate::cursor_trail::TypingCadence::intensity`], `0..1`, already gated by
//! reduced-motion) drives the body envelope, while a rainbow-kitty host also
//! hands it the ribbon's shared spectrum clock:
//!
//! * **hue motion** — hosted beside the ribbon, the caret samples
//!   [`crate::cursor_glow::CursorGlow::rainbow_phase`] so both marks stream on
//!   one clock. Standalone, a slow baseline spin accelerates under sustained
//!   typing and freezes at the final ember. Either path resolves through the
//!   rainbow family's one reflected sweep and continuously interpolated six
//!   anchors at the caret's own column, so the block and the ribbon leaving it
//!   are the same rainbow without hue steps;
//! * **saturation + brightness** — the block starts from WHITE (dark theme) or
//!   near-BLACK (light theme) and blooms toward a vivid rainbow as energy climbs;
//! * **an additive rainbow HALO** hugging the block — the glow, brightest while
//!   typing hard, breathing gently while idle.
//!
//! While typing with a BLINKING block, the host pins the rendered shape steady
//! and hands the raw blink flips here: a charged flip fires a short star FLARE — the fill
//! glints bright while additive star arms and a couple of glitter dots wink
//! just past the block's edges (the fill is opaque, so only the overhang light
//! shows: a little star flashing behind the block) — in place of the old
//! black-and-white vanish. The flare is a pure clock function (no RNG — the
//! comet-glint precedent) and completes well inside one blink half-period. Once
//! typing energy settles, flips remain ordinary terminal blinks and arm no
//! effect work at all.
//!
//! When you stop typing the cadence energy decays over ~1–2 s, so the cursor COOLS
//! OFF smoothly — the spin slows, the colour desaturates back toward the base, the
//! halo dims — settling to a dim "ready" rainbow ember. It is **text-safe by
//! construction**: the block FILL is returned as a colour the renderer runs through
//! its `floor_cursor_fill` contrast floor (so the cut-out glyph stays razor-sharp),
//! and the HALO is purely additive [`GlowQuad`] light around the cell that never
//! touches the glyph. Like the aurora it is a CLOCKLESS pure function of an injected
//! `now`, decays to a stable fingerprint so a still cursor costs nothing beyond the
//! blink cadence, and emits the SAME premultiplied quads on both the CPU and Metal
//! backends (byte-exact).

use aterm_time::Instant;

use aterm_render::{GlowQuad, premul_rgb};

use crate::cursor_glow::OVER_INK_COV_CAP;

use crate::cursor_glow::Geom;
use crate::cursor_glow::{
    rainbow_phase_from_unit_turn, rainbow_spectrum_of, rainbow_sweep_at, rainbow_sweep_reflect,
};

/// The block-cursor base the rainbow blooms FROM when the host names none:
/// white on a dark theme, a soft near-black on a light theme — so the "start
/// from white or black" reads on either.
///
/// A host that KNOWS the cursor's resolved colour (OSC 12, else the configured
/// theme cursor) passes it as [`RainbowConfig::base`] instead, and these two
/// stand only for the raw/embedder callers that have no such value.
const BASE_DARK_THEME: u32 = 0x00FF_FFFF; // white block on a dark background
const BASE_LIGHT_THEME: u32 = 0x0016_161C; // near-black block on a light background

/// Hue rotation in turns/second: a slow baseline while charged, plus up to a
/// full brisk spin at peak energy (≈one rotation/sec typing flat-out).
const IDLE_SPIN: f32 = 0.05;
const ACTIVE_SPIN: f32 = 1.05;

/// Idle breath (turns/sec of the halo pulse) + its depth — the gentle "ready" pulse.
const PULSE_HZ: f32 = 0.34;
const PULSE_DEPTH: f32 = 0.55;

/// Saturation / value ramps from the calm idle ember to the vivid typing bloom.
const SAT_IDLE: f32 = 0.32;
/// The LIGHT theme's idle saturation — see the emit site for why it is so much
/// higher than the dark one.
const SAT_IDLE_LIGHT: f32 = 0.88;
const SAT_MAX: f32 = 1.0;
const VAL_IDLE: f32 = 0.82;
const VAL_MAX: f32 = 1.0;

/// How far the block FILL tints from the base (white/black) toward the live rainbow:
/// a whisper at rest, vivid under the keys.
const MIX_IDLE: f32 = 0.16;
const MIX_MAX: f32 = 0.82;
/// The LIGHT-THEME mixes, which are far higher — and have to be.
///
/// The two bases are not symmetric. Mixing a saturated hue toward WHITE gives a
/// pastel of that hue: still obviously the hue, just gentler. Mixing the same
/// hue toward a NEAR-BLACK gives mud — at the dark ramp's 0.16..0.82 a
/// mid-energy caret on white composited to a drab olive-brown, which three
/// white-ground reviews called out ("a dirt-brown caret", "an opaque vermilion
/// with no relationship to the trail palette"). The caret is the anchor of this
/// style's palette; on white it has to be a RAINBOW block, and the near-black
/// base's job is only to keep it dark enough to invert its glyph.
///
/// Only the TOP of the ramp moves. At rest the light block stays the quiet
/// near-black it has always been (pinned by `light_theme_base_is_dark`) — an
/// idle caret should not be a lit lamp — and the mud was never the idle state
/// anyway: every capture that showed it was mid-run, where `e` is high.
const MIX_IDLE_LIGHT: f32 = MIX_IDLE;
const MIX_MAX_LIGHT: f32 = 0.95;

/// Halo geometry + brightness. A stack of thin concentric additive rings whose
/// coverage falls off QUADRATICALLY from the block outward — brightest hugging the
/// cell, fading to nothing by the radius — so the overlapping thin bars read as one
/// SOFT rainbow rim, not a few hard nested rectangles. The radius grows and the light
/// intensifies with energy, over a small always-on idle floor so a focused idle cursor
/// keeps a dim rainbow glow.
///
/// LEGIBILITY, 2026-07-24 (owner, twice: "the rainbow it too bright when I type
/// so I can't read the text very easily" / "the rainbow and stars are too
/// bright ... I can't see the letters still"): THE RINGS STACK, and the old
/// "hugs the block and never washes the neighbouring text" claim was checked
/// per-quad, never per-PIXEL. With six layers at radius 0.48 the LEFT bars of
/// layers 0/1/2 all covered the pixel column one px outside the cursor cell,
/// summing 93+60+33 = 186/255 of SATURATED additive light onto the edge of the
/// just-typed glyph; the full-width TOP bars dumped 48 across a 20px band up to
/// 10px INTO THE ROW ABOVE. Four layers at radius 0.22 and base 28 sum to 46 at
/// that same worst column — inside [`crate::cursor_glow::OVER_INK_COV_CAP`] —
/// and the rim now stays in the inter-character gutter (0.22*14 = 3px) instead
/// of reaching most of the way across the neighbour cell. The rim reads SOFTER,
/// not absent: the hue spread, the spin, the breath and the energy ramp are all
/// untouched.
const HALO_LAYERS: i32 = 4;
const HALO_RADIUS_IDLE: f32 = 0.06; // cells
const HALO_RADIUS_MAX: f32 = 0.22;
const HALO_BASE_COV: f32 = 28.0; // innermost-layer peak coverage (× energy)
/// Brightness kept while idle (× the breath). RAISED 0.16 -> 0.30 alongside the
/// `HALO_BASE_COV` 82 -> 28 cut, NOT lowered with it: the settled ember's
/// coverage is `HALO_BASE_COV · 1.0 · HALO_IDLE_FLOOR · (0.35 + PULSE_DEPTH ·
/// breath)`, and `as u8` TRUNCATES. A first pass took the floor to 0.10, which
/// put the innermost ring at 0.98..2.52 — so the resting rainbow ember
/// quantized to literally ZERO across most of its breath and the idle cursor
/// simply lost its glow. At 0.30 the ember sits at 2.9..7.6, comparable to the
/// retired 4.6..11.8, while the ACTIVE halo — the layer the legibility complaint
/// is actually about — still drops with the base.
const HALO_IDLE_FLOOR: f32 = 0.30;
/// Hue spread across the halo rings (turns): each ring sits a step further
/// along the wheel than the one inside it, so the rim reads as an actual
/// RAINBOW rippling outward from the block (it used to be six rings of one
/// single hue — a monochrome glow that only *cycled* through rainbow colours).
const HALO_HUE_SPREAD: f32 = 0.20;

/// The energy below which the cursor is considered SETTLED — the animator reports
/// itself inactive so the host stops arming the 60 fps tick (the idle rainbow then
/// rides the slow blink cadence, at zero extra wakeup cost).
const SETTLED_ENERGY: f32 = 0.02;

// ── blink twinkle (the "glitter star" blink) ────────────────────────────────
/// Flare length (seconds) of one blink-flip twinkle. Comfortably shorter than
/// the host's ~530 ms blink half-period, so every flare completes — and the
/// 60 fps tick disarms — before the next flip can fire one.
const TWINKLE_DUR: f32 = 0.16;
/// How far the block fill glints toward the star colour at the flare peak.
/// Lowered 0.6 -> 0.35 on 2026-07-24 with the rest of the legibility retune.
const TWINKLE_MIX: f32 = 0.35;
/// Star-arm overhang past the block edge, as a fraction of the cell's OWN axis
/// (the halo's per-axis discipline). Narrowed 0.45 -> 0.20 on 2026-07-24: at
/// 0.45 the "never washes the neighbour glyphs" claim was simply false — the
/// arms are a 4px bar THROUGH the cell centre overhanging BOTH neighbours.
const TWINKLE_REACH: f32 = 0.20;
/// Peak additive coverage of the star arms / glitter dots. Lowered 150/130 ->
/// 44/38 on 2026-07-24 and bounded by
/// [`crate::cursor_glow::OVER_INK_COV_CAP`]. The retired comment claimed these
/// were "≤ the halo's cap" — but that cap was 160, so this was a 150-coverage
/// white bar drawn across the letters on either side of the cursor, fired on
/// every blink flip while typing.
const TWINKLE_ARM_COV: f32 = 44.0;
const TWINKLE_DOT_COV: f32 = 38.0;
/// Scintillation cycles across one flare — the "glitter" wobble layered over
/// the smooth pop envelope, phase-shifted per flare by the flip counter so
/// consecutive twinkles don't repeat exactly. Deterministic: a pure sine of
/// the injected clock, no RNG (the comet-glint precedent).
///
/// PHOTOSENSITIVITY BOUND: this is cycles per [`TWINKLE_DUR`], so the on-screen
/// flash rate is `TWINKLE_SCINT / TWINKLE_DUR` Hz. At the retired 2.4 that was
/// 15 Hz — five times the WCAG 2.3.1 general-flash threshold (3 Hz), and by far
/// the fastest oscillator anywhere in the effect family. It very likely sat
/// under the standard's small-safe-area exemption (the star arms cover few
/// pixels), so this is not a claimed conformance failure — but it was
/// undocumented, unbounded, and the fix is one constant. 0.5 puts it at 3.1 Hz:
/// the flare still glints (one wobble over a 160 ms pop is exactly the
/// "catches the light" read), it simply no longer strobes.
///
/// INVARIANT: keep `TWINKLE_SCINT / TWINKLE_DUR <= 3.2` if either is retuned.
/// Pinned by `twinkle_flash_rate_stays_under_the_photosensitivity_bound`.
const TWINKLE_SCINT: f32 = 0.5;

/// Per-tick dt clamp (seconds) across a continuously charged interval. A fully
/// settled cursor freezes its hue/breath, and a fresh charge starts from that
/// frozen phase; neither path integrates time spent idle. The cap still prevents
/// a charged but background-stalled window from flinging the phase forward.
const MAX_DT: f32 = 0.6;

/// Resolved per-frame inputs (Copy so the host reads it out before borrowing state).
#[derive(Clone, Copy, Debug)]
pub struct RainbowConfig {
    /// Master on/off (the style opted into the rainbow cursor AND the cursor is a
    /// focused, visible block).
    pub enabled: bool,
    /// Overall scale `0..1` — the reduced-motion / load-shed amplitude, folded in by
    /// the host exactly like the aurora. 0 ⇒ effectively off (no spin, no halo).
    pub intensity: f32,
    /// The terminal reports a BLINKING block. The host pins the rendered shape
    /// steady while charged and passes the raw blink phase to
    /// [`CursorRainbow::tick`]; charged phase flips fire a twinkle flare here.
    /// Settled flips remain ordinary terminal blinks. `false` (a steady block)
    /// never twinkles — there is no blink to replace.
    pub blinking: bool,
    /// The colour the block wears AT REST, `0x00RRGGBB` — the terminal's
    /// resolved cursor colour (OSC 12 when set, else the configured theme
    /// cursor, else the live OSC 10 foreground). The spectrum is a TINT over
    /// this base, so a settled caret is the user's cursor colour and typing
    /// blooms it toward the rainbow.
    ///
    /// This closes the hole the shipped default fell into: the rainbow block
    /// owns [`aterm_render::RenderInput::cursor_fill_override`], which the
    /// renderer applies INSTEAD of the frame cursor colour — so a hard-coded
    /// base meant OSC 12 and the theme cursor reached every cursor shape
    /// EXCEPT the default one. The aurora and the comet already recolour off
    /// the same live value (`aterm-gui`'s `glow_cfg.color` / `trail_cfg.color`);
    /// the block simply never did.
    ///
    /// `None` keeps the historical theme-polar base ([`BASE_DARK_THEME`] /
    /// [`BASE_LIGHT_THEME`]) for callers with no cursor colour to hand — every
    /// such frame is byte-identical to before.
    pub base: Option<u32>,
    /// The actual ribbon-head colour emitted by `CursorGlow` in this frame,
    /// `0x00RRGGBB`. The hot block blooms toward this exact colour so the caret
    /// cannot run a plausible-but-different rainbow beside the trail it leads.
    /// `None` keeps the standalone/embedder family-sweep resolver.
    pub head_rgb: Option<u32>,
}

/// What a tick produced: the block FILL colour to hand the renderer (it floors it for
/// contrast) and a fingerprint that changes on every visible step (0 when dormant).
#[derive(Clone, Copy, Debug)]
pub struct RainbowFrame {
    /// The evolving block-fill colour `0x00RRGGBB`, or `None` when the rainbow cursor
    /// is off (the renderer then keeps the ordinary themed cursor fill).
    pub fill: Option<u32>,
    /// Fingerprint of the emitted fill + halo (0 ⇒ nothing to show this frame).
    pub fp: u64,
}

/// Per-window rainbow-cursor animation state — the standalone hue fallback,
/// idle breath, and last clock reading. Tiny + Copy-cheap.
#[derive(Default)]
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
    /// The blink phase seen last tick — the twinkle's flip edge detector.
    /// `None` (fresh / just re-enabled) seeds without firing a flare.
    last_blink: Option<bool>,
    /// Start of the in-flight twinkle flare (`None` between flares).
    twinkle_at: Option<Instant>,
    /// Blink-flip counter — the deterministic per-flare variation seed (dot
    /// corners + scintillation phase), and the fingerprint's flare identity.
    twinkle_seq: u32,
    /// Latched "a flare is mid-flight" at the last tick (the [`is_active`]
    /// clockless answer, like `energy`).
    twinkling: bool,
}

impl CursorRainbow {
    /// Whether the host must keep arming the animation tick: while the cursor is
    /// still CHARGED (typing or cooling) or a blink-twinkle flare is mid-flight.
    /// Once settled it returns false and the idle rainbow rides the ordinary
    /// blink cadence — no rainbow-kitty-specific wakeups on a focused idle window.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.energy > SETTLED_ENERGY || self.twinkling
    }

    /// Advance one frame at `now` with the current typing `energy` (`0..1`), the
    /// host's raw cursor `blink_phase` (the twinkle's flip source — constant for a
    /// steady block), the block cursor cell `cur` (`None` ⇒ hidden), the theme
    /// darkness, grid `geom`, and the resolved `cfg`. Appends the additive rainbow
    /// HALO (+ any twinkle star) to `out` and returns the block FILL colour + a
    /// fingerprint. Pure: no wall-clock, unit-testable by injecting `now`/`energy`.
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
            Some(family_phase),
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
        family_phase: Option<f32>,
        blink_phase: bool,
        dark_theme: bool,
        geom: Geom,
        cfg: &RainbowConfig,
        out: &mut Vec<GlowQuad>,
    ) -> RainbowFrame {
        let e = (energy.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        // Fully inert — byte-identical to the plain themed cursor — when off, when
        // the geometry is degenerate, OR when the amplitude is zero (reduced motion
        // / load-shed). The intensity gate mirrors cursor_glow: without it a focused
        // block cursor would keep an idle-floor halo + hue/breath drift under Reduce
        // Motion, which the "0 ⇒ off" contract (and the aurora/comet siblings) forbid.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.energy = 0.0; // inert: report settled so the host disarms the tick
            self.last = Some(now);
            // Twinkle state clears too, so the first flip after a re-enable
            // seeds the edge detector instead of flaring off stale phase.
            self.last_blink = None;
            self.twinkle_at = None;
            self.twinkling = false;
            return RainbowFrame { fill: None, fp: 0 };
        }
        let was_active = self.energy > SETTLED_ENERGY;
        self.energy = e;

        // Advance hue + breath only across a continuously CHARGED interval.
        // Once the host disarms at settled energy, sampling a later unrelated
        // present must not integrate a hidden clock: that produced the reported
        // idle rainbow snap and byte-different late captures. Resumed typing
        // likewise starts from the frozen ember instead of charging for the
        // entire wall-clock gap.
        let active = e > SETTLED_ENERGY;
        let dt = if was_active && active {
            self.last
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(0.0)
                .min(MAX_DT)
        } else {
            0.0
        };
        self.last = Some(now);
        if active {
            self.phase = (self.phase + dt * (IDLE_SPIN + ACTIVE_SPIN * e)).fract();
            self.pulse = (self.pulse + dt * PULSE_HZ).fract();
        }
        // The ribbon owns the canonical family phase whenever the host can
        // supply it. The standalone path still gets the cursor's responsive
        // energy-rate spin, but LIFTS its unit turn onto one complete reflected
        // sweep. Feeding `self.phase` directly was the regression: the family
        // resolver interpreted 0..1 as the first 1/1024 of its ring, traversed
        // only ~0.35 of a spectrum sweep, then jumped backward every wrap.
        let spectrum_phase =
            family_phase.unwrap_or_else(|| rainbow_phase_from_unit_turn(self.phase));

        // BLINK → TWINKLE: with a blinking block, a CHARGED host blink-phase
        // FLIP stamps a flare (the flip counter varies each flare
        // deterministically). A settled flip stays an ordinary terminal blink
        // and arms no effect timer — the idle-zero contract.
        // Edge-triggered — reset_blink's typing re-arms force the phase ON
        // without a flip, so ordinary typing never fires flares. A steady block
        // clears the detector: no blink, no twinkle.
        if cfg.blinking {
            if let Some(prev) = self.last_blink
                && prev != blink_phase
                && e > SETTLED_ENERGY
            {
                self.twinkle_at = Some(now);
                self.twinkle_seq = self.twinkle_seq.wrapping_add(1);
            }
            self.last_blink = Some(blink_phase);
        } else {
            self.last_blink = None;
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
        // Report ACTIVE while a flare is in flight — not merely while its
        // envelope is nonzero: the pop is exactly 0 at the flip instant (`u==0`),
        // and if `is_active` read the envelope the host would disarm the 60 fps
        // tick on the very frame that ARMS the flare, freezing it before it lit.
        // `twinkle_at` is `Some` only across the live flare (the arm above sets
        // it; the `u>=1.0` arm clears it), so this is exactly "flare in flight".
        self.twinkling = self.twinkle_at.is_some();

        // The live rainbow: vivid saturation/brightness under the keys, calm at rest.
        // SATURATION HOLDS ON LIGHT. The light block blooms from a NEAR-BLACK
        // base, and mixing a PALE hue (idle saturation 0.32) into near-black is
        // what produces brown — the mud reviews reported on a caret sitting in a
        // drained delete run, where `e` is low by construction. A saturated hue
        // mixed into near-black is simply a DARK version of that hue, which is
        // what a rainbow caret should be at any energy. The dark theme keeps its
        // ramp: mixing toward WHITE pastels gracefully, so it never had this
        // problem.
        let sat = if dark_theme {
            lerp(SAT_IDLE, SAT_MAX, e)
        } else {
            lerp(SAT_IDLE_LIGHT, SAT_MAX, e)
        };
        let val = lerp(VAL_IDLE, VAL_MAX, e);
        // THE CARET'S COLUMN is its place on the family's sweep — the same
        // column the ribbon's rail under this cell resolves. A hidden cursor
        // still reports a fill, so column 0 stands in when there is no cell.
        let col = cur.map_or(0, |(_, cc)| cc);
        let band = spectrum_at(col, spectrum_phase, 0.0);
        let head_rgb = cfg.head_rgb.unwrap_or(band);
        let rainbow = shade(head_rgb, sat, val);

        // The BLOCK FILL: tint from the theme base toward the rainbow with energy. The
        // renderer floors this against the cell bg (the cut-out glyph colour), so the
        // glyph stays sharp however saturated the block gets.
        // …FROM the cursor's own colour when the host resolved one: the block IS
        // the cursor, so a settled caret must be whatever OSC 12 (or the theme
        // `cursor_color`) says it is, and the spectrum is the tint typing lays
        // over it. Only a caller that has no such value falls back to the
        // theme-polar constants.
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
        let mut fill = mix_rgb(base, rainbow, lerp(mix_idle, mix_max, e));
        // The twinkle GLINT: mid-flare the block catches the light. On a dark
        // theme it flashes toward star-white; on a light one toward the vivid
        // live hue — white would sink into a light background (the contrast
        // floor is off by default), while a saturated glint stays legible.
        if pop > 0.0 {
            let glint = if dark_theme {
                0x00FF_FFFF
            } else {
                shade(head_rgb, 1.0, 0.85)
            };
            fill = mix_rgb(fill, glint, TWINKLE_MIX * pop * cfg.intensity);
        }

        // The additive HALO: concentric rings around the block. Brightness = a small
        // breathing idle floor + the typing energy; radius grows with energy. Purely
        // additive, so it only adds photons around the cell — never over the glyph.
        let breath = 0.5 + 0.5 * (self.pulse * std::f32::consts::TAU).sin(); // 0..1
        let halo_energy = HALO_IDLE_FLOOR * (0.35 + PULSE_DEPTH * breath) + e;
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
            // SEPARATE horizontal + vertical reach. The rings grow by a fraction of
            // the cell's OWN width sideways and its OWN height vertically — a single
            // radius scaled by `ch` (cell height) grew the horizontal bars by a full
            // cell WIDTH into the neighbour glyphs (cw ≪ ch on a normal font), which
            // is exactly the "reaches a full cell into neighbours" wash. Bound to
            // ≤ HALO_RADIUS_MAX of each axis so the light HUGS the block: ≤ half a
            // cell sideways (the comment's promise) and the differing x/y growth also
            // means no two layers land the SAME rect, so the thin rings blend into a
            // soft rim instead of double-adding a stacked pair.
            let radius_x = (lerp(HALO_RADIUS_IDLE, HALO_RADIUS_MAX, e) * cw as f32).max(1.0);
            let radius_y = (lerp(HALO_RADIUS_IDLE, HALO_RADIUS_MAX, e) * ch as f32).max(1.0);
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
                // Each ring samples its own point on the FAMILY's sweep — the
                // rim IS a rainbow, and the whole spectrum still spins with the
                // phase. The step is a distance ALONG the sweep now, not an
                // angle on a private wheel.
                let ring_hue = if layer == 0 {
                    // The innermost rim touches the ribbon nozzle and therefore
                    // wears its exact emitted hue. Outer rings fan through the
                    // family spectrum, preserving the authored rainbow halo.
                    shade(head_rgb, sat, val)
                } else {
                    shade(
                        spectrum_at(cc, spectrum_phase, t * HALO_HUE_SPREAD),
                        sat,
                        val,
                    )
                };
                push_ring(
                    out,
                    geom,
                    // TIGHT per-axis growth (`gx`/`gy` from the separate
                    // horizontal/vertical reach above) so the rim HUGS the block
                    // and never bleeds a full cell into neighbour glyphs — the
                    // legibility bar — while each ring still samples its own
                    // point on the wheel (`ring_hue`) so the rim IS a rainbow.
                    cx - gx,
                    cy - gy,
                    cw + 2 * gx,
                    ch + 2 * gy,
                    premul_rgb(ring_hue, cov),
                );
            }
        }

        // The TWINKLE STAR: additive arms through the cell centre overhanging the
        // block's edges, plus two glitter dots at hash-picked corners. The fill is
        // opaque, so only the overhang light shows — a star flashing behind the
        // block. Same hug discipline as the halo: per-axis reach well under half
        // a cell, coverage under the halo's cap, every quad via the shared
        // clamped row-splitter (grid-interior, single-row, CPU/GPU byte-exact).
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
            // GLITTER dots: two per flare, corners + 1 px jitter picked by an
            // integer hash of the flip counter — different corners each blink,
            // identical for identical clocks (no RNG). Snappier envelope (pop²)
            // so they wink after the arms bloom.
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
                    let hue = shade(
                        spectrum_at(cc, spectrum_phase, 0.13 + k as f32 * 0.29),
                        0.85,
                        1.0,
                    );
                    push_ring_rect(out, geom, cx + dx, cy + dy, s, s, premul_rgb(hue, dot_cov));
                }
            }
        }

        // Fingerprint: quantized phase + energy + fill so a settled cursor early-outs
        // the present but any visible step (spin, breath, tint, twinkle) forces a
        // repaint. The flare folds its envelope + the flip counter ONLY while lit
        // (`pop > 0`), so a settled cursor's key is byte-identical to a never-flared
        // one — the flare leaves no fingerprint residue once it completes.
        let twinkle_fp = if pop > 0.0 {
            (((pop * 255.0) as u64) << 24).wrapping_add(u64::from(self.twinkle_seq) << 40)
        } else {
            0
        };
        // Key the spectrum by its RESOLVED period-two position, not by either
        // input clock's raw domain. Equal visible phases (including a complete
        // family-ring wrap) then have equal keys, while any visible sweep step
        // still forces a present.
        let spectrum_fp = rainbow_sweep_at(col, spectrum_phase);
        let fp = ((spectrum_fp * 1024.0) as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add(u64::from(head_rgb).rotate_left(7))
            .wrapping_add((halo_energy * 255.0) as u64)
            .wrapping_add(((fill as u64) << 12) ^ ((self.pulse * 64.0) as u64))
            .wrapping_add(twinkle_fp);

        RainbowFrame {
            fill: Some(fill),
            fp,
        }
    }
}

/// Push one additive halo ring as pixel rects, CLAMPED + row-split via the shared
/// [`push_ring_rect`] so every quad is single-row and grid-interior (the invariants
/// the renderer's row gate + CPU/GPU parity depend on). Emits the rect as four thin
/// bars (top/bottom/left/right) so the ring HUGS the block instead of filling a solid
/// block of light over neighbouring cells.
fn push_ring(out: &mut Vec<GlowQuad>, geom: Geom, x: i32, y: i32, w: i32, h: i32, premul: u32) {
    if w <= 0 || h <= 0 || premul == 0 {
        return;
    }
    let th = ((geom.ch as i32) / 8).max(2); // ring thickness in px
    // top + bottom bars
    push_ring_rect(out, geom, x, y, w, th, premul);
    push_ring_rect(out, geom, x, y + h - th, w, th, premul);
    // left + right bars (between the top/bottom bars to avoid double-adding corners)
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

/// Clamp a pixel rect to the WINDOW interior and split it into per-cell-row
/// [`GlowQuad`]s (so the dirty gate + scissor stay exact) — the same contract as the
/// aurora's internal `push_rect`, kept local so this module needs no cross-import.
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
    // EFFECTS BOX (grid + head band): identity-exact at head 0; a below-grid
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
        // Grid-row DAMAGE HINT, anchored at origin_y (above-grid bands tag row 0).
        let row = (yy - oy).div_euclid(ch);
        let band_end = (oy + (row + 1) * ch).min(y1);
        out.push(GlowQuad {
            row: row.max(0) as u16,
            x: x0 as u16,
            y: yy as u16,
            w: (x1 - x0) as u16,
            h: (band_end - yy) as u16,
            color: premul,
        });
        yy = band_end;
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

/// THE FAMILY'S SPECTRUM, at the caret's own place on it.
///
/// The block used to run its OWN colour wheel — a private `hsv2rgb_turns`
/// sampled at a private unit-turn clock — while the ribbon leaving that same
/// cell resolved the family's six anchors. Two spectrums, two clocks, meeting
/// at one cell: the caret could be teal while the underline directly beneath it
/// was green, which is the most literally visible "different rainbows" this
/// family had.
///
/// So the caret now asks the SAME question every other smooth mark of this style
/// asks: where is this COLUMN on the sweep, and what is the continuous colour
/// between its two surrounding anchors? `phase` is the ribbon's shared
/// phase-ring clock on the locked path. A standalone host gets the block's
/// energy-responsive spin law (see [`IDLE_SPIN`] / [`ACTIVE_SPIN`]), lifted by
/// [`rainbow_phase_from_unit_turn`] onto one complete family sweep so its unit
/// wrap is seamless. `off` steps a further distance ALONG that sweep — the halo
/// rings walking outward, the glitter dots — folded by the family's own
/// reflection so an offset can never wrap violet into red.
#[inline]
fn spectrum_at(col: u16, phase: f32, off: f32) -> u32 {
    rainbow_spectrum_of(rainbow_sweep_reflect(rainbow_sweep_at(col, phase) + off))
}

/// A family colour re-mixed at saturation `s` and value `v`, hue intact — the
/// block's ENERGY LAW applied to a colour it did not choose.
///
/// This is HSV's own S/V re-application written for an RGB input: each channel
/// is pulled toward the colour's peak by `1 − s` (the achromatic direction) and
/// then scaled by `v`. At `s = 1, v = 1` it is the IDENTITY, so a caret at full
/// energy is EXACTLY the spectrum colour the ribbon under it draws — which is the property
/// `caret_ribbon_and_streaks_share_one_spectrum` pins.
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

/// Clamped per-channel RGB mix (`t` from a → b).
fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |sh: u32| {
        let ca = ((a >> sh) & 0xff) as f32;
        let cb = ((b >> sh) & 0xff) as f32;
        ((ca + (cb - ca) * t).round().clamp(0.0, 255.0) as u32) << sh
    };
    ch(16) | ch(8) | ch(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    fn geom() -> Geom {
        // Identity layout: origin 0 + win == grid extents ⇒ byte-identical to
        // the historical pad-relative emissions.
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
            // No host base: these fixtures pin the historical theme-polar
            // bloom, so they stay byte-identical to the pre-`base` tick.
            base: None,
            head_rgb: None,
        }
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

    /// Disabled ⇒ no fill, no halo, no fingerprint (byte-identical to the plain cursor).
    #[test]
    fn disabled_is_inert() {
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let f = cr.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            true,
            true,
            geom(),
            &RainbowConfig {
                enabled: false,
                intensity: 1.0,
                blinking: false,
                base: None,
                head_rgb: None,
            },
            &mut out,
        );
        assert!(f.fill.is_none());
        assert_eq!(f.fp, 0);
        assert!(out.is_empty());
        assert!(!cr.is_active());
    }

    /// Reduced motion / load-shed (`intensity == 0`) ⇒ fully inert: no fill, no halo,
    /// fp 0, settled — byte-identical to the plain cursor, even with full energy.
    #[test]
    fn zero_intensity_is_inert() {
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let f = cr.tick(
            Some((1, 1)),
            Instant::now(),
            1.0,
            true,
            true,
            geom(),
            &RainbowConfig {
                enabled: true,
                intensity: 0.0,
                blinking: false,
                base: None,
                head_rgb: None,
            },
            &mut out,
        );
        assert!(
            f.fill.is_none(),
            "reduced motion keeps the plain themed cursor"
        );
        assert_eq!(f.fp, 0);
        assert!(out.is_empty(), "no halo under reduced motion");
        assert!(!cr.is_active());
    }

    /// The block fill starts NEAR the base (white on dark) at rest and moves markedly
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

    /// THE HOST'S CURSOR COLOUR IS THE BLOCK, on either theme polarity.
    ///
    /// The block fill leaves this tick as `RenderInput::cursor_fill_override`,
    /// which the renderer applies INSTEAD of `frame_cursor(input)` — so
    /// whatever base this returns is literally the cursor the user sees. With
    /// the base hard-coded to white/near-black, OSC 12 and the configured
    /// `cursor_color` reached every cursor shape except the one the shipped
    /// default paints. A settled caret must therefore BE the host's colour,
    /// and two colours must never collapse to one block.
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
        // …and energy still blooms the spectrum OVER that base rather than
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
    /// emitted head hue, while OSC 12 remains the colour they bloom FROM.
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
        assert_eq!(
            green_a.fill, green_b.fill,
            "the actual emitted head, not a different phase helper, owns the block hue"
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

    /// Energy drives BOTH the halo brightness and the hue-spin RATE: a hot run spins
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
        // A cell at the grid EDGE so clamping is exercised.
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
            "settled cursor idles (rides the blink cadence)"
        );
    }

    /// REGRESSION: settled cursor pixels are frame-gap invariant. Sparse
    /// captures and a fresh charge after a long idle interval never integrate
    /// an unpresented clock slice (the old behavior snapped the hue on input).
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

    // ───────────────────────── blink twinkle (glitter star) ─────────────────────────

    /// A blink-phase FLIP while the cursor is CHARGED fires a twinkle flare:
    /// star quads land in the scratch and the block fill GLINTS brighter than
    /// the unflared rainbow.
    #[test]
    fn blink_flip_fires_twinkle_star() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        let calm = cr
            .tick(Some((2, 20)), t, 0.8, true, true, g, &c, &mut out)
            .fill
            .unwrap();
        assert!(
            cr.is_active(),
            "typing energy already owns the frame cadence"
        );
        let calm_quads = out.len();
        // The blink flips OFF while charged: instead of vanishing, the star flares.
        out.clear();
        let flip = t + Duration::from_millis(16);
        let mid = flip + Duration::from_secs_f32(TWINKLE_DUR / 2.0);
        cr.tick(Some((2, 20)), flip, 0.8, false, true, g, &c, &mut out);
        assert!(cr.twinkling, "a charged flip arms the flare");
        out.clear();
        let flared = cr
            .tick(Some((2, 20)), mid, 0.8, false, true, g, &c, &mut out)
            .fill
            .unwrap();
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

    /// IDLE-ZERO REGRESSION: recurring terminal blink flips at settled energy
    /// never arm the rainbow kitty's effect timer. This is the exact permanent-wakeup bug:
    /// twenty half-periods must leave the animator idle after every flip.
    #[test]
    fn idle_blink_flips_never_arm_effect_timer() {
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

    /// Tier-1: project the genuine cursor animator's flare generation counter
    /// through a reachable charged-flare → cool → idle-blink trace. The idle
    /// blink deliberately lands while the earlier flare is still active, so a
    /// Boolean-only projection would see `twinkle == 1` both before and after.
    /// `twinkle_seq` makes a forbidden restart observable and rejectable.
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
            // Two abstract fuel ticks split the real flare window in half.
            // This is derived from the shipping timestamp, not test-owned
            // state: once `tick` clears `twinkle_at`, the projection is 0.
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

        // Reach Charge from the model's genuine initial state. This first
        // engine tick also seeds the blink-edge detector without flaring.
        let before = project(&rainbow, t, 0, 0);
        rainbow.tick(Some((1, 1)), t, 0.8, true, true, g, &c, &mut out);
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

        // A charged blink edge starts generation 1.
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

        // An idle blink while generation 1 is active must preserve generation
        // 1. Derive the restart observation from the REAL counter delta; it is
        // no longer an always-zero synthetic test field.
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

        // Negative control the former projection MISSED: keep the coarse
        // twinkle Boolean at 1 but advance the real generation identity as a
        // buggy idle restart would. Its corrupted countdown also witnesses
        // that the fuel obligation is non-vacuous.
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

        // The real clock crosses the flare's halfway point with one abstract
        // fuel tick left, then TWINKLE_DUR clears the arm completely. These
        // are the shipping Age/Finish transitions behind `CanFinish`.
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

    /// The flare is BOUNDED: once `TWINKLE_DUR` passes with no further flip the
    /// animator re-settles (the 60 fps tick disarms) and the emitted light is
    /// byte-identical to a twin that never flared — the flare leaves no residue.
    #[test]
    fn twinkle_completes_and_resettles() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let step16 = Duration::from_millis(16);
        let mut flared = CursorRainbow::default();
        let mut control = CursorRainbow::default();
        let (mut out_f, mut out_c) = (Vec::new(), Vec::new());
        // Identical clocks; only the phase argument differs (one flip vs none).
        flared.tick(Some((1, 1)), t, 0.8, true, true, g, &c, &mut out_f);
        control.tick(Some((1, 1)), t, 0.8, true, true, g, &c, &mut out_c);
        flared.tick(
            Some((1, 1)),
            t + step16,
            0.8,
            false,
            true,
            g,
            &c,
            &mut out_f,
        );
        control.tick(Some((1, 1)), t + step16, 0.8, true, true, g, &c, &mut out_c);
        assert!(flared.twinkling && !control.twinkling);
        // Past the flare end: both settle and emit identical light.
        let after = t + step16 + Duration::from_secs_f32(TWINKLE_DUR + 0.05);
        out_f.clear();
        out_c.clear();
        let ff = flared.tick(Some((1, 1)), after, 0.0, false, true, g, &c, &mut out_f);
        let fc = control.tick(Some((1, 1)), after, 0.0, true, true, g, &c, &mut out_c);
        assert!(!flared.is_active(), "the flare completes and disarms");
        assert_eq!(out_f, out_c, "no residue: post-flare light == never-flared");
        assert_eq!(ff.fill, fc.fill, "post-flare fill == never-flared");
    }

    /// A STEADY block never twinkles: with `blinking: false` even a flipping
    /// phase argument is ignored (there is no blink to replace).
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
    /// coverage-capped, and hugging within ~half a cell of the block on BOTH
    /// sides — a twinkle must never wash the neighbour glyphs.
    #[test]
    fn twinkle_star_hugs_and_caps() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((2, 20)), t, 0.8, true, true, g, &c, &mut out);
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
        // Mid-flare at FULL pop: the widest reach + brightest light of the flare.
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

    /// The twinkle is a pure clock function: identical instants + identical flip
    /// sequences ⇒ byte-identical quads and equal fingerprints (the CPU/GPU
    /// parity + repaint-key contract; no RNG anywhere in the flare).
    /// PHOTOSENSITIVITY BOUND (UX audit, 2026-07-24). The twinkle's
    /// scintillation was the fastest oscillator in the whole effect family at
    /// 15 Hz — five times the WCAG 2.3.1 general-flash threshold. Nothing
    /// bounded it, and nothing named it. This pins the RATE rather than either
    /// constant, so retuning the flare length can never silently re-introduce a
    /// strobe.
    #[test]
    fn twinkle_flash_rate_stays_under_the_photosensitivity_bound() {
        let hz = TWINKLE_SCINT / TWINKLE_DUR;
        assert!(
            hz <= 3.2,
            "twinkle scintillation is {hz} Hz — over the 3 Hz general-flash bound"
        );
        // …and it must still WOBBLE: a rate of zero would be a silent removal
        // of the glint rather than a bound on it. Both sides are constants, so
        // this is checked at build time — a retune to zero never compiles.
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
            let mut phase = true;
            for i in 0..40u64 {
                if i % 8 == 7 {
                    phase = !phase; // a flip every ~128 ms
                }
                let f = cr.tick(
                    Some((2, 10)),
                    t + Duration::from_millis(i * 16),
                    0.8,
                    phase,
                    true,
                    g,
                    &c,
                    &mut out,
                );
                fps.push(f.fp);
            }
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

    /// On a LIGHT theme the glint goes toward the vivid hue, not white — a
    /// white flash would sink into a light background with the contrast floor
    /// off (its default).
    #[test]
    fn light_theme_glint_stays_saturated() {
        let g = geom();
        let c = blink_cfg();
        let t = Instant::now();
        let mut cr = CursorRainbow::default();
        let mut out = Vec::new();
        cr.tick(Some((1, 1)), t, 0.8, true, false, g, &c, &mut out);
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

    /// A standalone caret keeps its energy-responsive ~one-turn/second clock,
    /// but that UNIT turn must traverse one COMPLETE period-two family sweep.
    ///
    /// This drives 1.28 seconds of real ticks, crosses the old one-second wrap,
    /// and checks both sides of that seam. Before the lift through
    /// `rainbow_phase_from_unit_turn`, the same state covered only ~0.35 of the
    /// sweep then jumped backward by that whole distance at the wrap. The
    /// direct-unit negative control proves this is not a vacuous smoothness
    /// bound, and visiting many interpolated colours proves “seamless” did not
    /// freeze colour.
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

            let colour = spectrum_at(col, family_phase, 0.0);
            colours.insert(colour);
            assert_eq!(
                emitted.fill,
                Some(mix_rgb(BASE_DARK_THEME, colour, MIX_MAX)),
                "frame {frame}: emitted caret must use the lifted family phase"
            );
            wrong_domain_disagreements += usize::from(spectrum_at(col, cr.phase, 0.0) != colour);
        }

        assert!(
            last_at.saturating_duration_since(t0) > Duration::from_secs(1),
            "the run must cross the historical one-second seam"
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

    /// The explicit shared-phase path closes on the ribbon's exact 1024-unit
    /// ring. This is a COMPLETE emitted frame (fill, halo bytes, fingerprint),
    /// not a helper equality; the quarter-sweep control proves the clock still
    /// animates.
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

    /// The standalone/embedder fallback is visible on the block itself, so its
    /// phase sweep may not hide six full-colour jumps behind a smooth coordinate
    /// clock. A dense real-tick sample crosses every anchor and the reflected
    /// endpoint; the old `rainbow_band_of` lookup jumped by as much as 255 here.
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
        assert!(
            max_step <= 3,
            "a 2.5 ms phase step changed one fill channel by {max_step}"
        );
        assert!(
            colours.len() > 256,
            "continuity must not mean a frozen/six-colour cursor: {} colours",
            colours.len()
        );
    }

    /// Outer halo rings and the two glitter dots take offsets along the same
    /// spectrum. Every offset must interpolate continuously too; fixing only the
    /// block/head would leave coloured rings popping around an otherwise smooth
    /// cursor.
    #[test]
    fn halo_and_glitter_offsets_have_no_anchor_colour_steps() {
        for off in [
            HALO_HUE_SPREAD / 3.0,
            2.0 * HALO_HUE_SPREAD / 3.0,
            0.13,
            0.42,
        ] {
            let mut previous = spectrum_at(17, 0.0, off);
            let mut max_step = 0;
            for sample in 1..=2400 {
                let phase = sample as f32 * (6.0 / 2400.0);
                let colour = spectrum_at(17, phase, off);
                max_step = max_step.max(rgb_max_delta(previous, colour));
                previous = colour;
            }
            assert!(
                max_step <= 3,
                "offset {off} jumped one spectrum channel by {max_step}"
            );
        }
    }

    /// Real engine-to-engine lock over more than a second of typed advances.
    /// CursorGlow owns the ribbon clock; CursorRainbow consumes that public
    /// sample after the same frame's tick. Every hot caret fill must therefore
    /// be the theme base mixed toward the continuous family colour at that
    /// cell — throughout motion, not just at the all-zero seed phase.
    #[test]
    fn caret_and_ribbon_share_one_live_clock_through_a_streaming_run() {
        use crate::cursor_glow::{CursorGlow, GlowConfig, GlowStyle};

        let g = geom();
        let glow_cfg = GlowConfig {
            enabled: true,
            style: GlowStyle::RainbowKitty,
            color: 0x0050_FA7B,
            accent: 0x007A_A2F7,
            duration: Duration::from_millis(240),
            length: 18,
            intensity: 1.0,
            radius: 0.6,
            ring: true,
            dark_theme: true,
            theme_fg: 0x00C8_D3F5,
            theme_bg: 0x001A_1B26,
            beam: false,
            head_dx: 0.5,
            pack: None,
            wake_persist_s: 2.4,
            ribbon_tall: false,
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
            let body_frame = body.tick_with_family_phase(
                Some((row, col)),
                frame_at,
                1.0,
                family_phase,
                false,
                true,
                g,
                &body_cfg,
                &mut body_out,
            );
            let ribbon_colour = spectrum_at(col, family_phase, 0.0);
            colours.insert(ribbon_colour);
            assert_eq!(
                body_frame.fill,
                Some(mix_rgb(BASE_DARK_THEME, ribbon_colour, MIX_MAX)),
                "key {key}: caret and ribbon diverged at phase {family_phase}, col {col}"
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
            colours.len() >= 12,
            "the streaming run must traverse many interpolated colours: {colours:#x?}"
        );
    }

    /// ONE RAINBOW, from the caret outward. The block cursor and its halo resolve
    /// one reflected spectrum position through one continuous interpolation —
    /// the property the block's private HSV wheel made impossible.
    ///
    /// Three claims, and all three are needed:
    ///   1. the caret's spectrum lookup is deterministic at the family's own
    ///      column and phase;
    ///   2. the energy law is a pure SHADE of that colour — the identity at full
    ///      energy — so the agreement is exact and not merely close;
    ///   3. the whole tick honours it: a hot block's FILL is the theme base
    ///      mixed toward exactly that interpolated colour.
    #[test]
    fn caret_uses_the_continuous_family_spectrum_end_to_end() {
        for &col in &[0u16, 1, 5, 17, 22, 39, 137, 400] {
            for i in 0..9 {
                let phase = i as f32 * 0.37;
                let colour = spectrum_at(col, phase, 0.0);
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
            assert_eq!(
                f,
                mix_rgb(BASE_DARK_THEME, spectrum_at(col, 0.0, 0.0), MIX_MAX),
                "the caret at col {col} uses the family spectrum at col {col}"
            );
        }
    }

    /// The additive halo HUGS the block: even at full energy no ring reaches more than
    /// ~half a cell WIDTH past the cursor cell, so it never washes the neighbour
    /// glyphs (its own contract). Regression for the full-cell-wide horizontal reach.
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
}
