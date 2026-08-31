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
//!   rainbow family's one reflected sweep and continuously interpolated seven
//!   anchors at the caret's laid field position, so the block and the ribbon leaving it
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
//! When you stop typing the caret COOLS OFF smoothly — the spin slows, the colour
//! desaturates back toward the base, the halo dims — settling to a dim "ready"
//! rainbow ember. **TWO ENVELOPES, NOT ONE**, and this doc used to name only the
//! first: the BODY (spin, halo, flare) rides the caller's cadence `energy`
//! ([`crate::cursor_trail::TypingCadence::intensity`]), a 220 ms half-life
//! ignition heat that is zero within ~0.35 s of the last key; the COLOUR rides
//! [`RainbowConfig::paint`], the ribbon's own display spine, so the block wears
//! its trail's colour for as long as the trail wears it. Claiming one ~1–2 s
//! decay for both is what let the caret sit at its idle mix — the bare theme
//! cursor colour — beside a fully painted red ribbon, measured on a shipped
//! build. It is **text-safe by
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
    RAINBOW_CARET_LIGHT_FLOOR, RAINBOW_FIELD_LEVEL, RAINBOW_SPARKLE_LIGHT_SHARE,
    rainbow_phase_from_unit_turn, rainbow_sweep_at, rainbow_sweep_reflect, rainbow_thing_of,
};
use crate::spectrum::{clear_light_of_cyan, clear_thing_of_cyan};

/// The block-cursor base the rainbow blooms FROM when the host names none:
/// white on a dark theme, a soft near-black on a light theme — so the "start
/// from white or black" reads on either.
///
/// A host that KNOWS the cursor's resolved colour (OSC 12, else the configured
/// theme cursor) passes it as [`RainbowConfig::base`] instead, and these two
/// stand only for the raw/embedder callers that have no such value.
const BASE_DARK_THEME: u32 = 0x00FF_FFFF; // white block on a dark background
const BASE_LIGHT_THEME: u32 = 0x0016_161C; // near-black block on a light background

/// The page this caret's own light lands on when the host names none — the
/// SHIPPED default background on either polarity.
///
/// [`clear_light_of_cyan`] is a law about a `(colour, coverage, GROUND)` triple,
/// so it needs the real page. A host that has one passes it as
/// [`RainbowConfig::ground`]; these two stand for the raw/embedder callers that
/// do not: the dark one is `ColorScheme::default`'s own `#111318` (the ground
/// `spectrum`'s glass gates already solve against), the light one is the built-in
/// light scheme's `#FDF6E3`.
const GROUND_DARK_THEME: u32 = 0x0011_1318;
const GROUND_LIGHT_THEME: u32 = 0x00FD_F6E3;

/// **THE PAGES THE RIM'S LIGHT-LAW ANSWERS FOR**, beyond the one the config
/// names: the two dark grounds every glass gate in this family solves against
/// (`ColorScheme::default`'s `#111318` and Tokyo Night's `#1A1B26`).
///
/// One ground used to be enough, and the ROYGBIV roof is what ended that: the
/// crossing's samples are bright (`V ≥ 217` where the retired lerp sagged to
/// `130`), so a ring quad that composites clean over the page it was solved
/// for can now clear the 32-level chroma floor over a slightly different dark
/// page and read inside the window there — measured at `5,580` of `3.67 M`
/// composites (worst `#112C36`, hue `196.5°`, `S 0.69`) when the law was asked
/// about one ground and the gate about two. The law therefore answers for the
/// family's whole dark-page set, plus whatever ground the host actually names.
const RIM_LAW_GROUNDS: [u32; 2] = [GROUND_DARK_THEME, 0x001A_1B26];

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
    /// **THE TRAIL'S OWN PAINT SPINE** — how strongly the ribbon beside the
    /// caret is coloured right now, `0..1`. Feed
    /// [`crate::cursor_glow::CursorGlow::momentum_display`]: the eased momentum
    /// spine the ribbon's own width, wave and brightness already read, so the
    /// block wears its trail's colour on the trail's own schedule instead of on
    /// a second, much faster clock.
    ///
    /// **THE DEFECT THIS CLOSES.** The `energy` argument is
    /// [`crate::cursor_trail::TypingCadence::intensity`] — a 220 ms half-life
    /// ignition heat that is EXACTLY ZERO below two keys' worth of standing
    /// heat. Measured on a shipped build (Default theme, `cursor_color`
    /// `#50FA7B`) at the end of a 43-character burst: the ribbon a few cells
    /// left of the caret was `#722629` (red) and still fully painted, while the
    /// caret read `#92C074` at `t+0` and `#65EB7F` — the theme green, i.e. the
    /// idle mix — from `t+0.25 s` onward. The ribbon's own spine
    /// (`momentum_display`) was `0.99 / 0.96 / 0.71 / 0.23` across the same
    /// `t+0 / 0.25 / 1.0 / 2.5 s`. The rainbow was AVAILABLE the whole time
    /// (`field` never moved); the MIX collapsed, 4–8× faster than the light it
    /// was supposed to match.
    ///
    /// `None` ⇒ the colour envelope falls back to `energy`, byte-identical to
    /// the pre-`paint` tick, for the raw/embedder callers that have no ribbon.
    /// A host that passes it gets `max(paint · intensity, energy · intensity)`:
    /// the cadence keeps the fast ATTACK (it ignites within two quick keys,
    /// where momentum has barely started) and the ribbon owns the RELEASE.
    pub paint: Option<f32>,
    /// **THE PAGE THE CARET'S OWN LIGHT LANDS ON**, `0x00RRGGBB` — the resolved
    /// terminal background.
    ///
    /// §2.3's ruling is about a PIXEL, and a pixel is a colour at a coverage over
    /// a ground. The block's FILL is a *thing* and is closed on its own byte
    /// ([`clear_thing_of_cyan`], applied above after the last mix); the rings, the
    /// star arms and the glitter dots are LIGHT, and light has no colour until it
    /// is composited. So this tick's emitted quads go through
    /// [`clear_light_of_cyan`] against this ground — the same law, over the same
    /// triple, that `spend_rainbow_budget` runs on the ribbon's own quads.
    ///
    /// `None` falls back to the shipped page for the polarity the caller names
    /// ([`GROUND_DARK_THEME`] / [`GROUND_LIGHT_THEME`]), so an embedder that has
    /// no background to hand still gets a law rather than none.
    pub ground: Option<u32>,
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
    /// Latched COLOUR envelope at the last tick — the trail-paint spine folded
    /// with the energy ([`RainbowConfig::paint`]). Latched beside `energy`
    /// because the caret's colour outlives its ignition heat now: a host that
    /// disarmed the tick at `energy <= SETTLED_ENERGY` would FREEZE a hot block
    /// mid-cool and then snap it to the base on whatever unrelated frame came
    /// next, which is the one temporal discontinuity this change could have
    /// introduced. `is_active` reads both.
    paint: f32,
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
    /// The rim's pixel buffer for [`Self::clear_caret_light_of_cyan`], and the
    /// colours the rim was EMITTED with. Both are `clear`-and-refill scratch,
    /// retained across frames exactly like `RainbowLedger`'s: the law lays the
    /// rim out up to a dozen times inside one bisection, and a fresh allocation
    /// per lay would be the only heap traffic on this path.
    rim_scratch: Vec<u32>,
    rim_emitted: Vec<u32>,
}

impl CursorRainbow {
    /// Whether the host must keep arming the animation tick: while the cursor is
    /// still CHARGED (typing or cooling) or a blink-twinkle flare is mid-flight.
    /// Once settled it returns false and the idle rainbow rides the ordinary
    /// blink cadence — no rainbow-kitty-specific wakeups on a focused idle window.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.energy > SETTLED_ENERGY || self.paint > SETTLED_ENERGY || self.twinkling
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
        // Fully inert — byte-identical to the plain themed cursor — when off, when
        // the geometry is degenerate, OR when the amplitude is zero (reduced motion
        // / load-shed). The intensity gate mirrors cursor_glow: without it a focused
        // block cursor would keep an idle-floor halo + hue/breath drift under Reduce
        // Motion, which the "0 ⇒ off" contract (and the aurora/comet siblings) forbid.
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || cfg.intensity <= 0.0 {
            self.energy = 0.0; // inert: report settled so the host disarms the tick
            self.paint = 0.0;
            self.last = Some(now);
            // Twinkle state clears too, so the first flip after a re-enable
            // seeds the edge detector instead of flaring off stale phase.
            self.last_blink = None;
            self.twinkle_at = None;
            self.twinkling = false;
            return RainbowFrame { fill: None, fp: 0 };
        }
        // **WHERE THIS TICK'S OWN LIGHT STARTS IN THE SHARED STREAM.** `out` is
        // the window's one glow scratch and `CursorGlow::tick` has already filled
        // it (and already spent §2.3 over it) by the time the host gets here, so
        // the caret's quads are exactly the tail this tick appends. Marked before
        // the first push and closed after the last — see
        // [`Self::clear_caret_light_of_cyan`] for why the law cannot live in the
        // emitters and why it cannot ride the ribbon's pass either.
        let emitted_from = out.len();
        let was_active = self.energy > SETTLED_ENERGY;
        self.energy = e;
        // **THE COLOUR ENVELOPE IS THE TRAIL'S, THE BODY ENVELOPE IS THE
        // CADENCE'S.** They are two different questions and they always were:
        // "how hard is this person typing right now" sizes the halo, the spin
        // and the flare, and it is *supposed* to be twitchy. "How painted is
        // the light this caret is leading" is what decides whether the block
        // wears the arc's colour — and that is the ribbon's own question, so
        // the caret now reads the ribbon's own answer
        // ([`RainbowConfig::paint`]) instead of a second, 4–8× faster clock.
        //
        // `max` and not a replacement: the cadence ignites within two quick
        // keys, where the ribbon's spine has barely begun to build, so keeping
        // it as a floor preserves the ATTACK that already worked while the
        // ribbon owns the RELEASE. With no host spine the two are one value and
        // the whole tick is byte-identical to the pre-`paint` engine.
        let paint = cfg.paint.map_or(e, |p| {
            (p.clamp(0.0, 1.0) * cfg.intensity.clamp(0.0, 1.0)).max(e)
        });
        self.paint = paint;

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
        let spectrum_phase = family
            .map(|(phase, _)| phase)
            .unwrap_or_else(|| rainbow_phase_from_unit_turn(self.phase));
        // **THE CARET READS THE POSITION IT IS ABOUT TO LAY** (§2.1), so the
        // block and the light leaving it are the same colour BY CONSTRUCTION
        // rather than by two functions agreeing. Standalone (no host ribbon)
        // there is no field to read, so the caret falls back to the sweep at its
        // own column on its own clock — the same law it used everywhere before,
        // kept for exactly the case where nothing has laid anything.
        let spectrum_field = family.map(|(_, field)| field);

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
            lerp(SAT_IDLE, SAT_MAX, paint)
        } else {
            lerp(SAT_IDLE_LIGHT, SAT_MAX, paint)
        };
        let val = lerp(VAL_IDLE, VAL_MAX, paint);
        // THE CARET'S COLUMN is its place on the family's sweep — the same
        // column the ribbon's rail under this cell resolves. A hidden cursor
        // still reports a fill, so column 0 stands in when there is no cell.
        let col = cur.map_or(0, |(_, cc)| cc);
        let sweep = spectrum_field.unwrap_or_else(|| rainbow_sweep_at(col, spectrum_phase));
        let band = spectrum_at(sweep, 0.0);
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
        // …AND THE MIX IS A PATH, NOT A POINT. `base` is a colour the arc did not
        // choose, so the straight line to the arc's own colour can run through a
        // hue NEITHER END HAS: with the shipped Default theme's `#50FA7B` cursor
        // (hue 135°) and the arc's blue (204°), 52 % of that line lies inside
        // `HSV [165°, 200°]`, and `MIX_MAX` lands on `#17A9E7` — a solid
        // turquoise block. `caret_fill` below closes it on the EMITTED byte.
        let mut fill = mix_rgb(base, rainbow, lerp(mix_idle, mix_max, paint));
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
        // **THE THING-LAW, LAST** (§2.3) — after every mix, on the byte that
        // leaves. It is applied HERE rather than to `rainbow` because the block's
        // colour is not `rainbow`: two further straight lines run through this
        // cell (the base tint above, the light theme's saturated glint just now),
        // and either can put a hue in the window that neither of its endpoints
        // had. A guarantee taken before the last mix is a guarantee about
        // something else.
        let fill = clear_thing_of_cyan(fill);
        // **AND THE CARET IS THE BRIGHTEST THING IN THE EFFECT** (§8 d), which
        // is a statement about LIGHT and not about colour, so it is enforced
        // last and in luminance.
        //
        // It rides the energy, and it has to: at rest the block IS the cursor,
        // whatever OSC 12 says it is, and a floor that applied there would paint
        // a settled near-black caret pale grey. The knee is short — a quarter of
        // the energy range — because the sparkle field it is competing with is
        // alive from the first keystroke.
        // …and it rides the COLOUR envelope, because the field it is competing
        // with is the ribbon's sparkle field — alive for exactly as long as the
        // ribbon is. Floored on the cadence instead, the caret went dark the
        // moment the ignition heat did, with the sparkles still lit.
        let caret_floor =
            RAINBOW_CARET_LIGHT_FLOOR * aterm_render::smoothstep01(paint / CARET_LIGHT_KNEE);
        let mut fill = lift_to_light_floor(fill, caret_floor);

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
                    shade(spectrum_at(sweep, t * HALO_HUE_SPREAD), sat, val)
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
                    let hue = shade(spectrum_at(sweep, 0.13 + k as f32 * 0.29), 0.85, 1.0);
                    push_ring_rect(out, geom, cx + dx, cy + dy, s, s, premul_rgb(hue, dot_cov));
                }
            }
        }

        // **AND §2.3 LAST OF ALL, ON THE PIXEL** — every ring, arm and dot this
        // tick emitted, asked of the composite it will actually write.
        let rim_peak = self.clear_caret_light_of_cyan(&mut out[emitted_from..], dark_theme, cfg);
        // The rim is part of the cursor, but its pixels land OUTSIDE the opaque
        // block and after the ribbon has spent the field's light budget.  The
        // crossing roof made that ordering visible: a legal level-145 field plus
        // the ordinary rim reached L78, and the flare arm reached L109, while a
        // blue-end block sat at its L80 floor.  Paling the rim for §2.3 cannot
        // repair a light ordering because it deliberately preserves luminance.
        //
        // `clear_caret_light_of_cyan` therefore measures the FINAL emitted pile
        // over the field's certified destination.  Keep the ornament intact and
        // lift its opaque centre above that measured peak by the SAME margin the
        // family already promises.  Weight by the live floor so a settled OSC-12
        // cursor remains exactly its configured colour instead of jumping bright
        // merely because the idle ember exists.
        if dark_theme {
            let floor_weight = (caret_floor / RAINBOW_CARET_LIGHT_FLOOR).clamp(0.0, 1.0);
            let promised_margin =
                RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
            fill = lift_to_light_floor(
                fill,
                ((rim_peak + promised_margin) * floor_weight).max(caret_floor),
            );
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
        let spectrum_fp = sweep;
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

    /// **THE CYAN LAW ON THE CARET'S OWN LIGHT** —
    /// [`crate::spectrum::clear_light_of_cyan`], applied to every quad one tick
    /// of this engine emits.
    ///
    /// # The hole this closes, and how it hid
    ///
    /// §2.3 has two enforcers and this caret was inside exactly one of them. The
    /// block's FILL is a *thing*, and `clear_thing_of_cyan` closes it on the byte
    /// that leaves (above, after the last mix). The rings, the star arms and the
    /// glitter dots are LIGHT — premultiplied additive `GlowQuad`s — and the law
    /// for light is [`crate::spectrum::clear_light_of_cyan`], which
    /// `CursorGlow::spend_rainbow_budget` runs over the ribbon's `under`/`out`
    /// streams as the last thing it does.
    ///
    /// **But this engine's quads are not in those streams when that pass runs.**
    /// The host ticks `CursorGlow` first (which fills the window's one glow
    /// scratch and spends §2.3 over it), and only then ticks this engine, which
    /// APPENDS to the same buffer. Every quad below was therefore emitted after
    /// the only pixel law in the family had already finished, and reached glass
    /// unruled.
    ///
    /// Measured on a 136-frame capture of the shipped default (Default theme,
    /// `cursor_color` `#50FA7B`, `block_fill_rgb=65ef7e`) at the parent commit:
    /// of **2,321** cyan pixels, **656** lay within ten pixels of the caret block
    /// — including the brightest pixel in the whole capture, `(36, 113, 97)` at
    /// hue `167.5°`, `S 0.68`, `V 113`, sitting two pixels off the block's edge.
    /// That is this ring, at `HALO_LAYERS` layer `0`, wearing `shade(head_rgb, …)`
    /// — the ribbon's own emitted hue — added to the page's blue-leaning
    /// `#111318`. 436 of those 656 were above `V 38`; 147 were above `V 80`.
    ///
    /// # Why it is asked of the PAGE and not of the block
    ///
    /// Because the page is where these pixels land. Every quad here HUGS the
    /// caret cell and overhangs it by a pixel or a few — that overhang is the
    /// whole point of the emitters (*"the fill is opaque, so only the overhang
    /// light shows"*) — and `draw_cursor` is the renderer's last pass, so the
    /// part of a ring lying INSIDE the cell is replaced by the block and is never
    /// seen. Asking these quads about the block's fill would spend chroma on
    /// pixels nobody can look at, at the one place §8 d wants the effect
    /// brightest. Checked on the same capture: of 2,321 cyan pixels, **zero** lay
    /// inside the block's own fill, on any frame.
    ///
    /// # Why it cannot live in the emitters
    ///
    /// Same reason its twin cannot: the law is about a `(colour, coverage)` PAIR
    /// over a ground, and `push_ring`/`push_ring_rect` are handed a colour that is
    /// already premultiplied and then SPLIT across cell rows. Running it per push
    /// would ask the same question once per row band; running it here asks it once
    /// per quad, on the pair the rasterizer is actually handed.
    ///
    /// # AND THE DESTINATION IS NOT THE PAGE — IT IS THE PAGE PLUS THE PILE
    ///
    /// [`crate::spectrum::clear_light_of_cyan`] answers exactly for a stack of ONE
    /// quad's own light (that is what `SPECTRUM_GLASS_STACK` is), and this
    /// emitter's marks are not one quad: `HALO_LAYERS` concentric rings each push
    /// four bars, the twinkle pushes two crossed arms and two corner dots, and they
    /// are DESIGNED to overlap — the rings *"blend into a soft rim"* by landing on
    /// each other, and each ring samples its OWN point on the sweep
    /// (`spectrum_at(sweep, t * HALO_HUE_SPREAD)`), so the pixel where two of them
    /// meet carries a colour NEITHER of them has.
    ///
    /// A per-quad law over the bare page cannot see that pixel, and measured on
    /// glass it did not: asked of `cfg.ground` alone, a 251-frame capture still
    /// carried **588** cyan pixels within ten pixels of the block, peaking at
    /// `V 107`, `S 0.39`, hue `165.7°`.
    ///
    /// **AND A PER-QUAD LAW OVER "THE PAGE PLUS MY SIBLINGS" DOES NOT FIX IT
    /// EITHER, WHICH IS WORTH WRITING DOWN.** That was the next draft and it is
    /// unsound for a reason the arithmetic makes obvious once seen: the cyan window
    /// IS NOT MONOTONE IN ADDED LIGHT. Charging quad `i` for its siblings at their
    /// EMITTED colours checks a BRIGHTER destination than the one that will exist —
    /// because the siblings are about to be paled too — and a dimmer destination can
    /// be MORE cyan, not less. `the_caret_rim_is_never_cyan_where_its_own_layers_meet`
    /// refuted that draft with **14,160** cyan pixels of 1.56 M lit, worst
    /// `#112832` at hue `198.2°`, `S 0.66`. A gate refuting a law is what a gate is
    /// for.
    ///
    /// # So this law reads the pixel, because the pixel is what the ruling is about
    ///
    /// The rim is one ornament around one cell — a few dozen quads over a few
    /// thousand pixels — so it is affordable to stop modelling and COMPOSITE: lay
    /// the whole pile into a scratch buffer over the page, exactly as
    /// `aterm_render` will, and ask [`crate::spectrum::light_is_over_the_glass_ceiling`]
    /// of every pixel that comes out.
    ///
    /// The move is then pile-wide rather than per-quad: one `keep`, bisected for
    /// the LARGEST value at which no pixel of the composited rim is over the
    /// ceiling. Pile-wide is not a compromise here, it is the only sound shape —
    /// the offending pixel belongs to no single quad, so no per-quad answer exists
    /// to give it.
    ///
    /// **AND IT IS TOTAL.** At `keep == 0` every quad is achromatic
    /// ([`crate::spectrum::pale_light_at_constant_light`]), a saturating sum of
    /// greys is achromatic, and a ground displaced along the achromatic axis keeps
    /// its own hue — `222.9°` on the shipped page, far outside the window — at a
    /// saturation no greater than the ground's. So a satisfying `keep` always
    /// exists, and the fallback below makes the law total even where the predicate
    /// is not monotone in `keep`. It is also non-brightening at every `keep`, by
    /// the convexity argument its twin is built on, so nothing here can re-open a
    /// ceiling and §8 d's *"the caret is the brightest thing the effect draws"* —
    /// a statement about the block's FILL — is untouched.
    ///
    /// [`Self::CARET_RASTER_MAX`] is the backstop for a geometry that ever made the
    /// rim large: past it the law degrades to the per-quad reading over the page,
    /// which is strictly better than none.
    fn clear_caret_light_of_cyan(
        &mut self,
        quads: &mut [GlowQuad],
        dark_theme: bool,
        cfg: &RainbowConfig,
    ) -> f32 {
        let ground = cfg.ground.unwrap_or(if dark_theme {
            GROUND_DARK_THEME
        } else {
            GROUND_LIGHT_THEME
        }) & 0x00FF_FFFF;
        if quads.is_empty() {
            return 0.0;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
        for q in quads.iter() {
            x0 = x0.min(u32::from(q.x));
            y0 = y0.min(u32::from(q.y));
            x1 = x1.max(u32::from(q.x) + u32::from(q.w));
            y1 = y1.max(u32::from(q.y) + u32::from(q.h));
        }
        // The host's page plus [`RIM_LAW_GROUNDS`] — deduplicated so the
        // common case (dark theme on the shipped default) pays for two
        // rasterizations, not three.
        let mut grounds = [ground, RIM_LAW_GROUNDS[0], RIM_LAW_GROUNDS[1]];
        let n_grounds = {
            let mut n = 1;
            for i in 1..3 {
                if !grounds[..n].contains(&grounds[i]) {
                    grounds[n] = grounds[i];
                    n += 1;
                }
            }
            n
        };
        let grounds = &grounds[..n_grounds];
        let (w, h) = (
            (x1.saturating_sub(x0)) as usize,
            (y1.saturating_sub(y0)) as usize,
        );
        if w == 0 || h == 0 || w * h > Self::CARET_RASTER_MAX {
            for q in quads.iter_mut() {
                for &g in grounds {
                    q.color = clear_light_of_cyan(q.color, q.alpha, g);
                }
            }
            // The pile proof is unavailable, so fail BRIGHT: the caller lifts
            // the opaque centre to white instead of silently skipping §8(d).
            // Empty input returned above and remains the only zero-cost case.
            return 255.0;
        }
        // The colours as EMITTED. Every candidate below is derived from these, so
        // the answer cannot depend on how many times this ran.
        self.rim_emitted.clear();
        self.rim_emitted.extend(quads.iter().map(|q| q.color));
        // **IS THE RIM, LAID AT THIS `keep`, OVER THE CEILING ANYWHERE?** One
        // rasterization: the buffer starts as the page and every quad composites
        // onto it through the family's own blend.
        let mut scratch = std::mem::take(&mut self.rim_scratch);
        let mut over_anywhere = |quads: &[GlowQuad], emitted: &[u32], keep: f32| -> bool {
            for &page in grounds {
                scratch.clear();
                scratch.resize(w * h, page);
                for (q, &c) in quads.iter().zip(emitted) {
                    let c = if keep >= 1.0 {
                        c
                    } else {
                        crate::spectrum::pale_light_at_constant_light(c, keep)
                    };
                    for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                        for xx in u32::from(q.x)..u32::from(q.x) + u32::from(q.w) {
                            let i = (yy - y0) as usize * w + (xx - x0) as usize;
                            scratch[i] = crate::spectrum::compose_on_glass(scratch[i], c, q.alpha);
                        }
                    }
                }
                if scratch
                    .iter()
                    .any(|&p| crate::spectrum::light_is_over_the_glass_ceiling(p))
                {
                    return true;
                }
            }
            false
        };
        if over_anywhere(quads, &self.rim_emitted, 1.0) {
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            for _ in 0..10 {
                let mid = 0.5 * (lo + hi);
                if over_anywhere(quads, &self.rim_emitted, mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let keep = if over_anywhere(quads, &self.rim_emitted, lo) {
                0.0
            } else {
                lo
            };
            for (q, &c) in quads.iter_mut().zip(self.rim_emitted.iter()) {
                q.color = crate::spectrum::pale_light_at_constant_light(c, keep);
            }
        }
        // §8(d)'s additive-brightest ordering is the dark-page law.  A light
        // page deliberately keeps the active block saturated and dark against
        // white, so measuring it against a synthetic bright field would wash
        // out the very contrast that makes it visible.
        if !dark_theme {
            self.rim_scratch = scratch;
            return 0.0;
        }

        // Measure the pile over the exact grey level §4 derives for a fully
        // spent field.  Use each emitted lay's peak channel in all three
        // channels: the cyan projection is luminance-preserving and cannot
        // exceed that pre-law channel envelope, so this white pile dominates
        // every post-law authored hue.  Besides being conservative, the
        // envelope depends only on geometry and earned coverage; outer-ring hue
        // motion therefore cannot make the opaque block flicker or override the
        // ribbon head's authority over its colour.
        let level = RAINBOW_FIELD_LEVEL.round() as u32;
        let field = (level << 16) | (level << 8) | level;
        scratch.clear();
        scratch.resize(w * h, field);
        for (q, &emitted) in quads.iter().zip(self.rim_emitted.iter()) {
            let level = ((emitted >> 16) & 0xff)
                .max((emitted >> 8) & 0xff)
                .max(emitted & 0xff);
            let envelope = (level << 16) | (level << 8) | level;
            for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                for xx in u32::from(q.x)..u32::from(q.x) + u32::from(q.w) {
                    let i = (yy - y0) as usize * w + (xx - x0) as usize;
                    scratch[i] = crate::spectrum::compose_on_glass(scratch[i], envelope, q.alpha);
                }
            }
        }
        let peak = scratch.iter().copied().fold(0.0f32, |peak, px| {
            peak.max(crate::color_math::relative_luminance(px) * 255.0)
        });
        self.rim_scratch = scratch;
        peak
    }

    /// The largest rim, in pixels, the rasterized law above lays out.
    ///
    /// Not a tuning knob — a backstop. The emitters bound themselves already: the
    /// rings reach at most [`HALO_RADIUS_MAX`] of a cell on each axis and the
    /// twinkle arms [`TWINKLE_REACH`], so the rim is one cell plus a hem, and at
    /// the largest font this ships with that is a few thousand pixels. It exists so
    /// a future geometry that made the rim a screenful degrades to the per-quad
    /// reading rather than to a per-frame framebuffer.
    const CARET_RASTER_MAX: usize = 1 << 16;
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
            // ADDITIVE light — this emitter has no other mode (see
            // [`GlowQuad::alpha`]).
            alpha: 0,
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
/// cell resolved the family's seven anchors. Two spectrums, two clocks, meeting
/// at one cell: the caret could be teal while the underline directly beneath it
/// was green, which is the most literally visible "different rainbows" this
/// family had.
///
/// So the caret now asks the SAME question every other mark of this style asks:
/// where is this COLUMN on the sweep? `phase` is the ribbon's shared phase-ring
/// clock on the locked path. A standalone host gets the block's
/// energy-responsive spin law (see [`IDLE_SPIN`] / [`ACTIVE_SPIN`]), lifted by
/// [`rainbow_phase_from_unit_turn`] onto one complete family sweep so its unit
/// wrap is seamless. `off` steps a further distance ALONG that sweep — the halo
/// rings walking outward, the glitter dots — folded by the family's own
/// reflection so an offset can never wrap violet into red.
///
/// The positional colour is the authored family spectrum. The no-solid-cyan
/// guarantee belongs to the final emitted block fill, after [`mix_rgb`]: a straight
/// per-channel line between two colours and therefore lands on every hue between
/// them. [`crate::spectrum::clear_thing_of_cyan`] therefore runs after every base
/// mix, so its guarantee applies to the byte that leaves.
#[inline]
fn spectrum_at(sweep: f32, off: f32) -> u32 {
    rainbow_thing_of(rainbow_sweep_reflect(sweep + off))
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

/// How much of the energy range the caret's light floor takes to arrive.
///
/// SHORT on purpose. The floor exists because the sparkle field out-shines the
/// caret, and that field is alive from the first keystroke — but the floor must
/// be exactly zero at rest, where §2.1's *"the block IS the cursor"* is the whole
/// contract and a lit floor would repaint a settled near-black caret grey. A
/// quarter of the range, eased, is on by the time anything else is.
const CARET_LIGHT_KNEE: f32 = 0.25;

/// **LIFT A COLOUR TO A LUMINANCE FLOOR**, toward white, and no further.
///
/// TOWARD WHITE because that is the one direction that adds light without moving
/// hue: every channel keeps its distance from `255` in proportion, so the mix
/// gives up SATURATION and nothing else. The caret's red pales toward a coral at
/// the arc's dark end; its hue is the arc's hue throughout.
///
/// Solved rather than scaled: relative luminance is monotone in the mix and the
/// transfer function is not linear, so a closed form would have to invert the
/// sRGB curve per channel. Twenty halvings put the residue three orders under a
/// byte, and this runs ONCE per frame.
fn lift_to_light_floor(rgb: u32, floor: f32) -> u32 {
    let light = |c: u32| crate::color_math::relative_luminance(c) * 255.0;
    // A conservative caller may ask above the displayable range (the raster
    // backstop does exactly that after adding its margin).  White is the total
    // answer, not an out-of-range bisection target.
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
            // …and no host ribbon spine, so the colour envelope IS `energy`
            // and every pin below keeps measuring exactly the law it was
            // written against. The shipped `Some(_)` path is swept separately
            // (`the_caret_never_wears_cyan`, `the_caret_cools_with_its_trail`).
            paint: None,
            // …and no host page either, so the light-law solves against the
            // shipped ground for the polarity each fixture names.
            ground: None,
        }
    }

    /// THE CONTINUITY CEILING for a walk whose consecutive samples are `dt`
    /// apart on the spectrum: the steepest the arc can move, times the step,
    /// plus a level for rounding.
    ///
    /// Derived rather than written down. A fixed number pins the pace the
    /// colour law happened to run at the day the test was written, so
    /// re-pacing the arc — which is the whole of the design's lumpy fix
    /// (`docs/design/RAINBOW-TRAIL-ONE-STORY.md` §2.2) — reads as a
    /// regression instead of as the intended change. What a continuity oracle
    /// is for is catching a DISCONTINUITY, and the arc is C¹, so a real one
    /// lands far above this.
    /// The compatibility thing read is the authored band identity, so this is
    /// the band's exact derived byte-rate plus one level for rounding.
    fn continuity_ceiling(dt: f32) -> u32 {
        (crate::spectrum::spectrum_max_byte_rate() * dt).ceil() as u32 + 1
    }

    /// The same bound for a walk of the BLOCK'S FILL, which is the thing-arc
    /// MIXED with a colour the arc did not choose.
    ///
    /// The exact bound is scanned from the composed base mix and final
    /// [`crate::spectrum::clear_thing_of_cyan`] projection, so palette changes do
    /// not leave a stale hand-written ceiling behind.
    fn caret_continuity_ceiling(base: u32, mix: f32, dt: f32) -> u32 {
        (crate::spectrum::spectrum_caret_max_byte_rate(base, mix) * dt).ceil() as u32 + 1
    }

    /// **THE CARET'S COLOUR LAW, COMPOSED** — what the block emits for a
    /// resolved base, rainbow and mix at energy `e`. The tick composes exactly
    /// this, and every pin that reconstructs a fill goes through it, so the two
    /// cannot drift into agreeing about different things.
    fn caret_law(base: u32, rainbow: u32, mix: f32, e: f32) -> u32 {
        lift_to_light_floor(
            clear_thing_of_cyan(mix_rgb(base, rainbow, mix)),
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
                paint: None,
                ground: None,
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
                paint: None,
                ground: None,
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
                // STANDALONE FIXTURE: no host ribbon, so the field the caret
                // reads is the sweep at its own column on the family clock —
                // the same law it used before §2.1, kept for exactly this case.
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
        // THE FILL'S OWN CEILING, not the arc's: this walks the BLOCK, which is
        // the thing-arc mixed toward `BASE_DARK_THEME` at `MIX_MAX` (the walk
        // runs at full energy) and then held to §2.3 on the emitted byte.
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

    /// **THE CARET IS NEVER CYAN** — `docs/design/RAINBOW-TRAIL-ONE-STORY.md`
    /// §2.3: *"What must never happen is a THING being cyan."*
    ///
    /// **THE DEFECT THIS PINS.** The engine reported `block_fill_rgb=40a5ab`
    /// (HSV `183.4°`, `S = 0.63`) and `45a4c5` (`194.6°`, `S = 0.65`) on a
    /// shipped build: the caret resolved the BAND's arc, a continuous red→violet
    /// arc has to cross `HSV [165°, 200°]` (green is `108°`, blue is `204°`), and
    /// a caret fills a whole cell and holds still — so for whatever share of the
    /// arc the transit takes, the caret simply WAS cyan. Roughly one keystroke in
    /// seven at the constant-luminance arc's `15.59 %` dwell.
    ///
    /// **THE WINDOW IS §2.3.4'S, VERBATIM**: HSV hue in `[165°, 200°]` at
    /// `S > 0.3`, measured on the EMITTED colour after every shade and mix the
    /// caret applies — not on the position it resolved, and not in some other
    /// colour space at some other width.
    ///
    /// **AND THE FIRST FIX MISSED, FOR A REASON WORTH WRITING DOWN.** Taking the
    /// caret onto the thing-arc ([`crate::spectrum::spectrum_clear_of_cyan`])
    /// guaranteed the position it RESOLVES. The block does not emit that: it
    /// emits `mix_rgb(base, rainbow, …)`, where `base` is the cursor's own colour
    /// (`block_fill_base_from=cursor_color`). A straight per-channel line between
    /// two legal colours is not a legal line — **52 % of the line from the shipped
    /// Default theme's `#50FA7B` cursor to the arc's blue lies inside the
    /// window** — and the engine emitted `#17A9E7` (`197.9°`, `S = 0.90`),
    /// `#52C6E7` (`193.3°`, `S = 0.65`) and `#5DCED3` (`182.5°`, `S = 0.56`) for
    /// 19 of 500 measured keystrokes.
    ///
    /// **THE PIN COULD NOT REFUTE IT, BECAUSE THE SHIPPED CONFIGURATION WAS NOT
    /// IN ITS DOMAIN.** It swept `[None, Some(#FFFFFF), Some(#16161C)]` — all
    /// three ACHROMATIC, and a neutral base cannot rotate a hue. So the sweep
    /// below enumerates the base the product actually hands the block: the
    /// resolved `cursor_color` of **every theme aterm ships**, read out of
    /// [`aterm_types::scheme`] rather than transcribed, plus the achromatic
    /// fallbacks. Ranked by how much of each theme's mix line sits in the window:
    /// Default (shipped) 52 %, Gruvbox Dark 18 %, Solarized Dark 9 %, Solarized
    /// Light 6 %; the rest clear.
    ///
    /// **THE SWEEP IS THE WHOLE DOMAIN**, both paths the caret has:
    ///
    /// * the LAID field a host hands it, walked across a full turn — the fold
    ///   `rainbow_laid_sweep` maps one turn of the engine's hue onto exactly
    ///   `0..=1` and back, so `0..=1` IS the full turn;
    /// * the STANDALONE rail sweep on its own clock, walked across a whole ring;
    ///
    /// crossed with the energy range (the mix toward the base runs `0.16 .. 0.82`
    /// with it, `0.16 .. 0.95` on light), and both themes. The HALO RINGS and the
    /// GLITTER DOTS are checked on the same frames: they are things too,
    /// `premul_rgb` scales all three channels alike so hue and saturation survive
    /// it, and fixing only the block would leave a cyan rim around a caret that
    /// is not.
    ///
    /// **THE NEGATIVE CONTROL IS THE RETIRED EXPRESSION**, evaluated on the same
    /// samples: `mix_rgb(base, rainbow, mix)` without §2.3's law. It must land in
    /// the window, or this test is proving nothing about the fix.
    ///
    /// # WHY THIS GATE WAS GREEN OVER A VISIBLE DEFECT (2026-08-29)
    ///
    /// Two reasons, and the second is the one worth keeping in mind next time.
    ///
    /// 1. **Its bound had been moved from zero to a `5 %` SHARE**
    ///    (`ruled_cyan * 20 <= checked`) when the ROYGBIV merge made
    ///    `clear_thing_of_cyan` the identity. `0.73 %` under a `5 %` bar is a gate
    ///    that permits what it is named for. On glass at `c63e9558`, with this
    ///    green, the caret drew a SOLID 15 x 28 device-pixel block — one whole cell
    ///    — of `#5CA5C0`: hue `196.4°`, `S 0.52`, `V 192`, the brightest cyan in a
    ///    231-frame capture. That is this docstring's own `#17A9E7` defect back
    ///    verbatim, and the docstring was still describing it as fixed.
    ///
    /// 2. **It did no compositing at all.** It read `q.color` of the halo rings
    ///    and glitter dots as a COLOUR, on the argument that `premul_rgb` scales
    ///    all three channels alike so hue and saturation survive it. True, and
    ///    beside the point: those quads are additive LIGHT, and the pixel is
    ///    `add_sat(ground, q.color)` — a blue-black ground plus dim green light is
    ///    teal at a hue the quad never carried. The rings are now walked through
    ///    [`crate::spectrum::compose_on_glass`] over both shipped grounds, which is
    ///    the same reading `the_band_is_never_cyan_on_glass` takes.
    ///
    /// The FILL keeps a colour reading, and that is not the same oversight: the
    /// block is an OPAQUE cell fill, so for it the composite IS the colour.
    #[test]
    fn the_caret_never_wears_cyan() {
        const LO: f32 = 165.0;
        const HI: f32 = 200.0;
        const SAT: f32 = 0.3;
        // **AN ABSOLUTE CHROMA FLOOR BESIDE THE RELATIVE ONE**, in levels of
        // channel spread. §2.3.4 states the window as `S > 0.3`, and HSV `S` is
        // a RATIO: near black it inflates without bound, so the light theme's
        // caret — a near-black block (`BASE_LIGHT_THEME`) carrying a 16 % tint
        // at rest — reports `S = 0.43` for `#1A2E29`, whose whole channel spread
        // is 20 levels out of 255. That is a black block, not a cyan one, and
        // reading it as one would make this pin a statement about the base
        // rather than about the rainbow. 32 is the floor this crate already uses
        // for "measurable colour" (see the chromaticity reads in `cursor_glow`);
        // the defect being pinned clears it four-fold (`40a5ab` spreads 107
        // levels, `45a4c5` spreads 128).
        //
        // READ OUT OF THE LAW, not restated beside it: the same constant gates
        // `clear_thing_of_cyan`'s ramp, and its ramp is COMPLETE at the floor, so
        // every colour this predicate can flag is one the law fully treated.
        // Stating the two separately is how a law and its proof end up measuring
        // different things.
        const CHROMA_FLOOR: u32 = crate::spectrum::SPECTRUM_THING_CHROMA_FLOOR as u32;
        let hsv = |rgb: u32| -> (f32, f32, u32) {
            let (r, g, b) = (
                ((rgb >> 16) & 0xff) as f32,
                ((rgb >> 8) & 0xff) as f32,
                (rgb & 0xff) as f32,
            );
            let hi = r.max(g).max(b);
            let d = hi - r.min(g).min(b);
            if hi <= 0.0 || d <= 0.0 {
                return (0.0, 0.0, hi as u32);
            }
            let hue = if hi == r {
                60.0 * ((g - b) / d).rem_euclid(6.0)
            } else if hi == g {
                60.0 * ((b - r) / d + 2.0)
            } else {
                60.0 * ((r - g) / d + 4.0)
            };
            (hue, d / hi, d as u32)
        };
        let cyan = |rgb: u32| -> bool {
            let (hue, sat, spread) = hsv(rgb);
            spread >= CHROMA_FLOOR && (LO..=HI).contains(&hue) && sat > SAT
        };
        let g = geom();
        let now = Instant::now();
        let mut checked = 0usize;
        let mut saturated = 0usize;
        // THE RETIRED EXPRESSION's own count, on the same samples.
        let mut ruled_cyan = 0usize;
        let mut ruled_worst = 0x0000_0000_u32;
        // The rings and glitter, COMPOSITED — kept apart from the fill because
        // they are light and it is paint, and the two are read differently.
        let mut ring_checked = 0usize;
        let mut ruled_ring_cyan = 0usize;
        let mut ruled_ring_worst = 0x0000_0000_u32;
        let mut unruled_cyan = 0usize;
        let mut unruled_worst = 0x00FF_FFFF_u32;
        for (name, base) in shipped_caret_bases() {
            for &dark in &[true, false] {
                for step in 0..=512u32 {
                    // (a) THE LAID FIELD, across a full turn.
                    let field = step as f32 / 512.0;
                    // (b) THE STANDALONE RAIL, across a whole ring, on the same
                    //     walk so both paths are covered at every energy.
                    let phase = step as f32 * 2.0;
                    // **AND BOTH COLOUR ENVELOPES**, because the caret's mix is
                    // driven by `RainbowConfig::paint` when a host supplies the
                    // ribbon's spine and by `energy` when none does. Sweeping
                    // only the second would put the SHIPPED path outside this
                    // pin's domain — which is the exact way the previous
                    // version of this test failed to refute the cyan block (it
                    // swept three achromatic bases and the product hands it a
                    // green one). The `(1.0, Some(0.0))` row is the important
                    // one: a hot body with a dead trail must still be legal.
                    for &(energy, paint) in &[
                        (0.0f32, None),
                        (0.25, None),
                        (0.5, None),
                        (0.75, None),
                        (1.0, None),
                        (0.0, Some(1.0f32)),
                        (0.0, Some(0.55)),
                        (1.0, Some(0.0)),
                    ] {
                        let c = RainbowConfig {
                            base,
                            paint,
                            ..cfg()
                        };
                        // The COLOUR envelope this row resolves to — the same
                        // fold the engine applies (`cfg.intensity` is 1.0 in
                        // this fixture), so the composed-law assertion below
                        // measures the shipped expression rather than a
                        // plausible restatement of it.
                        let e = paint.map_or(energy, |p| p.clamp(0.0, 1.0).max(energy));
                        let mut cursor = CursorRainbow::default();
                        let mut out = Vec::new();
                        let frame = cursor.tick_with_family_phase(
                            Some((2, 17)),
                            now,
                            energy,
                            phase,
                            field,
                            false,
                            dark,
                            g,
                            &c,
                            &mut out,
                        );
                        let fill = frame.fill.expect("enabled block fill");
                        // **THE FILL, AS A COLOUR** — and that IS the composite:
                        // the block is an opaque cell fill, so nothing shows
                        // through it.
                        if cyan(fill) {
                            ruled_cyan += 1;
                            if hsv(fill).1 > hsv(ruled_worst).1 {
                                ruled_worst = fill;
                            }
                        }
                        // **THE RINGS AND GLITTER, AS LIGHT.** Reading `q.color`
                        // raw is what made this gate blind: the quads are
                        // additive premultiplied light, so what anyone looks at
                        // is `add_sat(ground, q.color)` over a BLUE-BLACK page,
                        // and that lands up to twenty degrees higher in hue than
                        // the quad carries.
                        for q in &out {
                            for ground in [0x0011_1318u32, 0x001A_1B26] {
                                let px =
                                    crate::spectrum::compose_on_glass(ground, q.color, q.alpha);
                                if (px >> 16) & 0xff == (ground >> 16) & 0xff
                                    && (px >> 8) & 0xff == (ground >> 8) & 0xff
                                    && px & 0xff == ground & 0xff
                                {
                                    continue;
                                }
                                ring_checked += 1;
                                if cyan(px) {
                                    ruled_ring_cyan += 1;
                                    if hsv(px).1 > hsv(ruled_ring_worst).1 {
                                        ruled_ring_worst = px;
                                    }
                                }
                            }
                        }
                        // **THE EMITTED FILL IS THE LAW, APPLIED** — the block
                        // composes this chromatic source, then on a dark page
                        // may lift only toward white to clear its emitted rim.
                        let unruled = unruled_caret_fill(base, dark, field, e);
                        let source = lift_to_light_floor(
                            clear_thing_of_cyan(unruled),
                            RAINBOW_CARET_LIGHT_FLOOR
                                * aterm_render::smoothstep01(e / CARET_LIGHT_KNEE),
                        );
                        let context = format!(
                            "the emitted caret fill is not the composed law at \
                             field {field}, energy {e}, dark {dark}, base {name}"
                        );
                        if dark {
                            assert_white_lift_of(fill, source, &context);
                        } else {
                            assert_eq!(fill, source, "{context}");
                        }
                        // …AND THE CONTROL: the same composition WITHOUT §2.3's
                        // law is what shipped, and it is cyan.
                        if cyan(unruled) {
                            unruled_cyan += 1;
                            if hsv(unruled).1 > hsv(unruled_worst).1 {
                                unruled_worst = unruled;
                            }
                        }
                        checked += 1;
                        if hsv(fill).1 > 0.5 {
                            saturated += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 15_000, "the sweep must be a real walk: {checked}");
        // NON-VACUOUS: a caret that had simply gone grey would pass the above
        // trivially. The walk spans the whole energy range and the idle end of
        // it is a near-base block by design, so the bar is a THIRD of the
        // samples carrying real colour — measured at 51.1 %.
        assert!(
            saturated * 3 > checked,
            "only {saturated} of {checked} caret fills were saturated — this \
             passes by having no colour, not by having the right one"
        );
        // **THE NEGATIVE CONTROL.** Without the law the very same walk puts the
        // caret in the window on 2.1 % of its samples, worst `S = 0.96`. A fix
        // that had only re-scoped the measurement would fail here, because this
        // number is measured with the SAME predicate on the SAME frames.
        // **THE CROSSING IS BOUNDED, NOT FORBIDDEN** — the owner's ruling
        // (*"it's not that cyan is BAD it's that I want a consistent rainbow
        // color pallet and cyan is not part of the rainbow. It's possible to
        // blend through it a little bit I guess"*), and under canonical ROYGBIV
        // a zero bar is unsatisfiable by anything except the grey hole: green
        // sits at 120° and blue at 240°, so every chromatic path between two
        // adjacent authored stops crosses the wedge. The retired
        // `clear_thing_of_cyan` scored zero here and bought it by driving 8.9 %
        // of the thing arc toward grey (worst sample chroma 130 -> 28), which is
        // the defect the palette exists to remove.
        //
        // What is asserted is the SHARE. The caret is one cell, so its crossing
        // has to be small; measured on this walk it is well under a twentieth of
        // the samples, and the census prints its own number so a regression that
        // widened it is a number rather than an opinion.
        println!(
            "CARET-CYAN-CENSUS checked={checked} fill_cyan={ruled_cyan} \
             worst=#{ruled_worst:06X} | rings_on_glass={ring_checked} \
             cyan={ruled_ring_cyan} worst=#{ruled_ring_worst:06X}"
        );
        // **THE BOUND IS ZERO, ON BOTH READINGS.** Not a share. A caret is a
        // THING — §2.3's own word — and a thing may not BE cyan; the arc's own
        // crossing is not at stake here, because `clear_thing_of_cyan` is a law
        // about the block's EMITTED byte and `spectrum_clear_of_cyan` (the arc)
        // stays the identity.
        assert_eq!(
            ruled_cyan,
            0,
            "the caret wore cyan on {ruled_cyan} of {checked} samples \
             (worst #{ruled_worst:06X}, hue {:.1}°, S {:.2})",
            hsv(ruled_worst).0,
            hsv(ruled_worst).1
        );
        assert!(
            ring_checked > 15_000,
            "only {ring_checked} lit ring composites walked — the glass reading \
             is not exercising anything"
        );
        // **AND THE GLASS READING HAS TEETH.** A zero is worth nothing until the
        // reading that produced it is shown to be able to produce something else.
        // The ring's colour is `shade(spectrum_at(...), sat, val)` — the RAW arc,
        // through no cyan law at all — so the control is the arc's own dead-centre
        // crossing at the same coverages: if THAT does not read cyan through this
        // predicate, the clause above is measuring nothing and should be deleted
        // rather than believed.
        let crossing = crate::spectrum::spectrum(0.5843);
        let control_cyan = [0x0011_1318u32, 0x001A_1B26].into_iter().any(|ground| {
            (1..=255u8).any(|cov| {
                cyan(crate::spectrum::compose_on_glass(
                    ground,
                    aterm_render::premul_rgb(crossing, cov),
                    0,
                ))
            })
        });
        assert!(
            control_cyan,
            "the ring reading cannot see cyan at all: the arc's own crossing \
             #{crossing:06X} composites clean at every coverage over both \
             grounds, so the zero above is the predicate's and not the mark's"
        );
        assert_eq!(
            ruled_ring_cyan,
            0,
            "the caret's rings put {ruled_ring_cyan} of {ring_checked} composited \
             pixels in the window (worst #{ruled_ring_worst:06X}, hue {:.1}°, \
             S {:.2})",
            hsv(ruled_ring_worst).0,
            hsv(ruled_ring_worst).1
        );
        assert!(
            unruled_cyan > 500,
            "the retired `mix_rgb` path must FAIL this pin: only {unruled_cyan} \
             of {checked} samples were cyan without §2.3's law, so the walk is \
             not exercising the defect"
        );
        assert!(
            cyan(unruled_worst) && hsv(unruled_worst).1 > 0.85,
            "the control's worst sample #{unruled_worst:06X} is not the reported \
             defect class (a HIGH-chroma turquoise block)"
        );
    }

    /// **THE CARET'S RIM, RASTERIZED — the reading its sibling gate cannot make,
    /// and the one glass makes every frame.**
    ///
    /// # Why this exists beside `the_caret_never_wears_cyan`
    ///
    /// That gate walks `compose_on_glass(ground, q.color, q.alpha)` — ONE quad
    /// over the page — over 3.6 M composites, and it is GREEN. It was green at
    /// `34f11f7c` too, while a 250-frame capture of the shipped default carried
    /// **1,239** cyan pixels within ten pixels of the caret block, peaking at
    /// `(36, 113, 95)`: hue `166°`, `S 0.68`, `V 113`, an unmistakable teal rim
    /// hugging the block on three sides.
    ///
    /// **THE READING WAS THE HOLE, NOT THE BOUND.** These quads are designed to
    /// LAND ON EACH OTHER — `HALO_LAYERS` concentric rings whose overlapping thin
    /// bars *"blend into a soft rim"*, plus two crossed twinkle arms through the
    /// same cell — and each ring samples its OWN point on the sweep, so the pixel
    /// where two of them meet carries a colour neither quad has. A per-quad
    /// reading cannot see that pixel no matter how many quads it walks.
    ///
    /// So this one does not read quads at all. It RASTERIZES the emitted stream —
    /// `add_sat` for the additive light this emitter pushes, `over_premul` for a
    /// source-over quad if one ever appears — into a real pixel buffer over both
    /// shipped grounds, and asks §2.3.4's question of every pixel. That is the
    /// same arithmetic `aterm_render` performs, so there is no model to drift.
    ///
    /// **THE WINDOW IS §2.3.4'S, VERBATIM**, and the same one its sibling uses:
    /// HSV hue in `[165°, 200°]` at `S > 0.3`, over the same absolute chroma floor
    /// (`SPECTRUM_THING_CHROMA_FLOOR`) so a near-black pixel's inflated ratio
    /// cannot be read as colour. **THE BOUND IS ZERO.** Not a dwell, not a share.
    ///
    /// **AND THE READING HAS TEETH**, which is the clause that makes the zero
    /// worth something: the control rasterizes TWO overlapping quads carrying the
    /// arc's own adjacent sweep colours at this emitter's own coverages, through
    /// no cyan law at all, and it must come out cyan. If it does not, this gate is
    /// measuring nothing and should be deleted rather than believed.
    #[test]
    fn the_caret_rim_is_never_cyan_where_its_own_layers_meet() {
        const LO: f32 = 165.0;
        const HI: f32 = 200.0;
        const SAT: f32 = 0.3;
        const CHROMA_FLOOR: u32 = crate::spectrum::SPECTRUM_THING_CHROMA_FLOOR as u32;
        // **THE PAGE IS NAMED, NOT ASSUMED.** Both shipped dark grounds (the
        // Default's `#111318` and Tokyo Night's `#1A1B26`, the pair every glass
        // gate in this family solves against) and the built-in light one. Each is
        // handed to the engine as `RainbowConfig::ground` AND used as the buffer
        // the rim is laid onto, so the law and its proof are talking about one
        // page. Rasterizing a light-theme rim over a dark page — which an earlier
        // draft of this test did — measures a frame the product cannot produce.
        const GROUNDS: [u32; 3] = [0x0011_1318, 0x001A_1B26, GROUND_LIGHT_THEME];
        let hsv = |rgb: u32| -> (f32, f32, u32) {
            let (r, g, b) = (
                ((rgb >> 16) & 0xff) as f32,
                ((rgb >> 8) & 0xff) as f32,
                (rgb & 0xff) as f32,
            );
            let hi = r.max(g).max(b);
            let d = hi - r.min(g).min(b);
            if hi <= 0.0 || d <= 0.0 {
                return (0.0, 0.0, hi as u32);
            }
            let hue = if hi == r {
                60.0 * ((g - b) / d).rem_euclid(6.0)
            } else if hi == g {
                60.0 * ((b - r) / d + 2.0)
            } else {
                60.0 * ((r - g) / d + 4.0)
            };
            (hue, d / hi, d as u32)
        };
        let cyan = |rgb: u32| -> bool {
            let (hue, sat, spread) = hsv(rgb);
            spread >= CHROMA_FLOOR && (LO..=HI).contains(&hue) && sat > SAT
        };
        // THE RASTERIZER, in the two blends `GlowQuad` names — `aterm_render`'s
        // own, so this composites rather than models.
        let raster = |quads: &[GlowQuad], ground: u32| -> Vec<u32> {
            let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
            for q in quads {
                x0 = x0.min(u32::from(q.x));
                y0 = y0.min(u32::from(q.y));
                x1 = x1.max(u32::from(q.x) + u32::from(q.w));
                y1 = y1.max(u32::from(q.y) + u32::from(q.h));
            }
            if x1 <= x0 || y1 <= y0 {
                return Vec::new();
            }
            let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
            let mut px = vec![ground; w * h];
            for q in quads {
                for yy in u32::from(q.y)..u32::from(q.y) + u32::from(q.h) {
                    for xx in u32::from(q.x)..u32::from(q.x) + u32::from(q.w) {
                        let i = (yy - y0) as usize * w + (xx - x0) as usize;
                        px[i] = crate::spectrum::compose_on_glass(px[i], q.color, q.alpha);
                    }
                }
            }
            px
        };

        let g = geom();
        let now = Instant::now();
        let mut pixels = 0usize;
        let mut lit = 0usize;
        let mut bad = 0usize;
        let mut worst = 0x0000_0000_u32;
        let mut deepest = 0usize;
        for (name, base) in shipped_caret_bases() {
            for ground in GROUNDS {
                let dark = aterm_render::theme_is_dark(ground);
                for step in 0..=128u32 {
                    let field = step as f32 / 128.0;
                    let phase = step as f32 * 8.0;
                    for &(energy, paint) in &[
                        (0.0f32, None),
                        (0.5, None),
                        (1.0, None),
                        (0.0, Some(1.0f32)),
                        (1.0, Some(0.0)),
                    ] {
                        // BLINKING, so the twinkle arms and glitter dots are in
                        // the stream too — they cross the same cell the rings
                        // ring, and a rim gate that walked the rings alone would
                        // be the same one-layer blindness one layer up.
                        let c = RainbowConfig {
                            base,
                            paint,
                            blinking: true,
                            ground: Some(ground),
                            ..cfg()
                        };
                        let mut cursor = CursorRainbow::default();
                        let mut out = Vec::new();
                        // Two ticks: the first seeds the blink edge detector, the
                        // second fires the flare, so the walk sees the FULL stream
                        // this emitter can produce and not just its resting rim.
                        cursor.tick_with_family_phase(
                            Some((2, 17)),
                            now,
                            energy,
                            phase,
                            field,
                            false,
                            dark,
                            g,
                            &c,
                            &mut out,
                        );
                        out.clear();
                        cursor.tick_with_family_phase(
                            Some((2, 17)),
                            now + Duration::from_millis(16),
                            energy,
                            phase,
                            field,
                            true,
                            dark,
                            g,
                            &c,
                            &mut out,
                        );
                        if out.is_empty() {
                            continue;
                        }
                        deepest = deepest.max(out.len());
                        for &p in &raster(&out, ground) {
                            pixels += 1;
                            if (hsv(p).2) as f32 <= crate::spectrum::SPECTRUM_GLASS_LIT_MIN {
                                continue;
                            }
                            lit += 1;
                            if cyan(p) {
                                bad += 1;
                                if hsv(p).1 > hsv(worst).1 {
                                    worst = p;
                                }
                            }
                        }
                        let _ = name.len();
                    }
                }
            }
        }
        println!(
            "CARET-RIM-RASTER-CENSUS pixels={pixels} lit={lit} cyan={bad} \
             worst=#{worst:06X} deepest_stream={deepest}"
        );
        assert!(
            pixels > 200_000,
            "only {pixels} rim pixels rasterized — the walk is not a walk"
        );
        // **THE READING HAS TEETH.** Two overlapping quads wearing the arc's own
        // adjacent sweep colours, at this emitter's own coverage scale, through no
        // law at all — rasterized by the very same `raster` above. If this comes
        // out clean the zero below is the predicate's and not the mark's.
        let a = crate::spectrum::spectrum(0.52);
        let b = crate::spectrum::spectrum(0.62);
        let control = GROUNDS.into_iter().any(|ground| {
            (8..=HALO_BASE_COV as u8).any(|cov| {
                let mk = |rgb: u32| GlowQuad {
                    row: 0,
                    x: 0,
                    y: 0,
                    w: 2,
                    h: 2,
                    color: premul_rgb(rgb, cov),
                    alpha: 0,
                };
                raster(&[mk(a), mk(b)], ground).iter().any(|&p| cyan(p))
            })
        });
        assert!(
            control,
            "two overlapping arc colours (#{a:06X} over #{b:06X}) rasterize clean \
             at every coverage over both grounds — this gate cannot see the defect \
             it exists for"
        );
        assert_eq!(
            bad,
            0,
            "the caret's rim put {bad} of {lit} lit rasterized pixels in hue \
             [{LO}, {HI}] at S > {SAT} (worst #{worst:06X}, hue {:.1}°, S {:.2}) — \
             cyan is not a rainbow colour",
            hsv(worst).0,
            hsv(worst).1
        );
    }

    /// **THE CARET COOLS WITH ITS TRAIL, NOT WITH THE IGNITION HEAT.**
    ///
    /// **THE DEFECT THIS PINS**, measured on a shipped build (Default theme,
    /// `cursor_color = #50FA7B`, `field = 0.050`) at intervals after the last
    /// key of a 43-character burst:
    ///
    /// | after last key | ribbon spine | `block_fill_rgb` |
    /// |---|---|---|
    /// | t+0     | 0.99 | `92c074` |
    /// | t+0.25s | 0.96 | `65eb7f` |
    /// | t+1.0s  | 0.71 | `65eb7f` |
    /// | t+2.5s  | 0.23 | `65ec80` |
    ///
    /// `65eb7f` is the caret's IDLE mix — the theme's own cursor green — and
    /// the ribbon three cells to its left was still a fully painted `#722629`
    /// with 30 live segments. The rainbow was available the whole time (`field`
    /// never moved); the MIX collapsed, because the caret's colour rode
    /// `TypingCadence::intensity` — a 220 ms half-life ignition heat that is
    /// EXACTLY zero below two keys' worth of standing heat, so it dies ~0.35 s
    /// after the last keystroke — while the ribbon's width, wave and brightness
    /// ride the τ = 2 s momentum spine.
    ///
    /// **BOTH LAWS ARE RUN, NOT RESTATED.** The burst below drives the real
    /// [`crate::cursor_trail::TypingCadence`] and the real
    /// [`crate::typing_momentum::TypingMomentum`], so "the cadence is dead
    /// while the spine is alive" is a MEASUREMENT of the shipped clocks at the
    /// shipped cadence, not a number transcribed from the table above. Retune
    /// either clock and this test re-derives.
    ///
    /// **AND THE OLD LAW IS THE NEGATIVE CONTROL**, evaluated on the same
    /// frames: `paint: None` still collapses to the base, or this proves
    /// nothing about the fix.
    #[test]
    fn the_caret_cools_with_its_trail_and_not_with_the_ignition_heat() {
        use crate::cursor_trail::TypingCadence;
        use crate::typing_momentum::{TYPING_MOMENTUM_TAU, TypingMomentum};

        /// The shipped Default theme's `cursor_color`, which is what
        /// `app_render` hands the block as `base`.
        const BASE: u32 = 0x0050_FA7B;
        /// The laid field the capture reported.
        const FIELD: f32 = 0.05;
        /// The measured burst: 43 characters at the harness's ~92 ms/key.
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

        // ── the negative control: the retired law ───────────────────────────
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
        // THE CEILING IS DERIVED, not chosen. The spine is exponential with
        // `TYPING_MOMENTUM_TAU`, so it moves at most `1/τ` per second; the
        // colour terms it drives span `MIX_MAX - MIX_IDLE` (the mix),
        // `SAT_MAX - SAT_IDLE` and `VAL_MAX - VAL_IDLE`, each of which can move
        // a channel by at most 255; plus two levels for the `f32 -> u8`
        // rounding at each end of a step.
        const STEP: Duration = Duration::from_millis(16);
        let step_s = STEP.as_secs_f32();
        let ceiling =
            (((MIX_MAX - MIX_IDLE) + (SAT_MAX - SAT_IDLE) + (VAL_MAX - VAL_IDLE)) * 255.0 * step_s
                / TYPING_MOMENTUM_TAU)
                .ceil() as u32
                + 2;
        let mut previous: Option<(u32, u32)> = None;
        let mut worst_step = 0u32;
        let mut cooled = 0usize;
        let mut frames = 0usize;
        for frame in 0..=190u32 {
            let at = last_key + STEP * frame;
            let paint = spine.value(at);
            let (fill, _) = render(at, Some(paint));
            let toward_base = rgb_max_delta(fill, BASE);
            // Continuity is claimed where the caret's LIGHT FLOOR is constant
            // (`smoothstep01(paint / CARET_LIGHT_KNEE) == 1` for
            // `paint >= CARET_LIGHT_KNEE`); the floor's own knee is a
            // deliberately short ramp with its own contract, and folding it in
            // here would measure two laws with one bound.
            if paint >= CARET_LIGHT_KNEE
                && let Some((prev_fill, prev_base_delta)) = previous
            {
                let step = rgb_max_delta(prev_fill, fill);
                worst_step = worst_step.max(step);
                assert!(
                    step <= ceiling,
                    "frame {frame}: the cooling caret jumped one channel by \
                     {step} (ceiling {ceiling}) — #{prev_fill:06X} -> #{fill:06X}"
                );
                // …and it only ever COOLS: the distance back to the cursor's
                // own colour never grows during a release.
                assert!(
                    toward_base <= prev_base_delta,
                    "frame {frame}: the caret warmed back up mid-release \
                     ({prev_base_delta} -> {toward_base})"
                );
                cooled += usize::from(toward_base < prev_base_delta);
            }
            frames += 1;
            previous = Some((fill, toward_base));
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

    /// **THE BASES THE PRODUCT ACTUALLY HANDS THE BLOCK.**
    ///
    /// `app_render` passes `base: Some(live_cursor_rgb)` — OSC 12 when the
    /// terminal set one, else the configured theme's `cursor_color`, else the
    /// live OSC 10 foreground — so the shipped domain is *every built-in theme's
    /// resolved cursor colour*. Read out of [`aterm_types::scheme`] through the
    /// same `to_theme_parts` projection the host resolves with, so a theme added
    /// to the product is swept here without anyone remembering to add it, and a
    /// theme whose cursor colour moves moves here too.
    ///
    /// The three achromatic entries stay: `None` is the raw/embedder path (the
    /// theme-polar constants), and the two literals are the exact bases this pin
    /// used to sweep — kept so the case that already passed keeps passing.
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

    /// **THE RETIRED COMPOSITION** — the block's fill exactly as it shipped
    /// through v0.61, `mix_rgb(base, shade(band, sat, val), mix)` with no §2.3
    /// law on the byte.
    ///
    /// Written out here rather than reached through a `cfg(test)` seam in the
    /// emitter because it is a *historical* expression: a seam would have to stay
    /// live in the module and would be one more thing that could drift into
    /// agreeing with the fix. The emitter's own arm is pinned against
    /// `clear_thing_of_cyan(this)` on every sample of the sweep, which is what
    /// keeps the two honest about being the same composition.
    /// `e` here is the COLOUR envelope the tick resolved
    /// ([`RainbowConfig::paint`] folded with the energy), not the raw energy
    /// argument — the composition it restates is the colour law, and the two
    /// coincide exactly when no host spine is supplied.
    fn unruled_caret_fill(base: Option<u32>, dark: bool, field: f32, e: f32) -> u32 {
        let base = base.unwrap_or(if dark {
            BASE_DARK_THEME
        } else {
            BASE_LIGHT_THEME
        });
        let sat = if dark {
            lerp(SAT_IDLE, SAT_MAX, e)
        } else {
            lerp(SAT_IDLE_LIGHT, SAT_MAX, e)
        };
        let val = lerp(VAL_IDLE, VAL_MAX, e);
        let (mix_idle, mix_max) = if dark {
            (MIX_IDLE, MIX_MAX)
        } else {
            (MIX_IDLE_LIGHT, MIX_MAX_LIGHT)
        };
        let rainbow = shade(spectrum_at(field, 0.0), sat, val);
        mix_rgb(base, rainbow, lerp(mix_idle, mix_max, e))
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
            ribbon_tall: true,
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
            // THE ONE FIELD: the caret reads the position the glow engine is
            // about to lay, which is what makes "caret and ribbon are one
            // rainbow" true by construction rather than by two functions
            // agreeing at one column (§2.1).
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
            // CLASSIC (owner, 2026-08-30): the caret walks the arc while the
            // mark unrolls (seven distinct stops) and then HOLDS the head — it
            // no longer spins through interpolated colours forever.
            colours.len() >= 5,
            "the streaming run must traverse the unroll's colours: {colours:#x?}"
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
                    // The STANDALONE path (`tick`, no host field), so the caret
                    // still resolves the sweep at its own column on its own
                    // clock — the fallback §2.1 keeps for exactly the case where
                    // nothing has laid anything.
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
    // RETIRED ON THE MERGE 2026-08-27: `caret_spectrum_cyan_census` resolved the
    // caret through `rainbow_spectrum_of` and called `spectrum_at(col, phase, off)`.
    // This branch deleted that door on purpose -- `cursor_rainbow` is the one module
    // that must NOT resolve the raw gradient -- so the census has no callee. Its bar
    // was also the weaker one: hue [165, 195] at S >= 0.35 and V >= 110, where the
    // ruling's window is [165, 200] at S > 0.3. The caret is now held by
    // `the_caret_never_wears_cyan`, and the band by `the_band_is_never_cyan_on_glass`,
    // which bounds the COMPOSITED pixel at zero rather than counting a table's share.
    //
    // RE-CONFIRMED ON THE ROYGBIV MERGE, mechanically and not by preference. The
    // upstream census cannot be carried across as written: `spectrum_at` here is
    // `(sweep: f32, off: f32)`, the census calls it `(col, phase, off)`, and both
    // `rainbow_spectrum_of` and `cursor_glow::RAINBOW_PHASE_RING` -- its other two
    // operands -- no longer exist in this tree. There is no version of that test
    // that compiles against this module.
    //
    // WHAT DOES CARRY ACROSS IS ITS LAW, which supersedes the one the successors
    // were written to: cyan is BOUNDED AS A CROSSING, not forbidden as a colour
    // (upstream 36cee255, on the owner's ruling that "it's possible to blend
    // through it a little bit"). Under seven-anchor ROYGBIV a zero bar is
    // unsatisfiable by construction -- the only way to score zero on the
    // green->blue interval is to desaturate it, and that grey hole is the defect
    // the seventh anchor was adopted to remove. The successors named above
    // therefore inherit the BOUND, not the prohibition; see their own headers for
    // the share each one now permits.

    /// THE TWO-GROUND RIM LAW IS PINNED, because it had no coverage: collapse
    /// [`RIM_LAW_GROUNDS`] to one ground and every test stayed green while the
    /// measured defect (5,580 cyan composites, worst #112C36 at hue 196.5,
    /// S 0.69, when the law answered for one page and the gate asked about two)
    /// silently returned. This asks the law's own question at the second
    /// ground: a premultiplied ring colour that composites CLEAN over
    /// [`GROUND_DARK_THEME`] but lands inside HSV [165,200] at S>0.3 over the
    /// second dark page must come back altered by the law. Mutating the law to
    /// consult only the first ground turns this red.
    #[test]
    fn the_rim_law_answers_for_every_dark_page_not_just_the_shipped_one() {
        use crate::spectrum::{clear_light_of_cyan, spectrum_hsv};
        let second = RIM_LAW_GROUNDS[1];
        assert_ne!(
            RIM_LAW_GROUNDS[0], second,
            "two grounds or the law is one-page"
        );
        // Sweep premultiplied candidates; keep those clean over ground 0 but
        // cyan over ground 1 BEFORE the law. The law must move every one of
        // them off the window over ground 1.
        let over = |c: u32, a: u8, g: u32| -> (f64, f64, f64) {
            let comp = |cc: u32, sh: u32, gg: u32| -> f32 {
                let s = ((cc >> sh) & 0xff) as f32;
                let d = ((gg >> sh) & 0xff) as f32;
                s + d * (255.0 - a as f32) / 255.0
            };
            let (r, g_, b) = (comp(c, 16, g), comp(c, 8, g), comp(c, 0, g));
            let px = ((r.min(255.0) as u32) << 16)
                | ((g_.min(255.0) as u32) << 8)
                | (b.min(255.0) as u32);
            spectrum_hsv(px)
        };
        // The owner's window, at the law's own absolute chroma floor: a
        // composited pixel under 32 levels of max-min cannot read as colour
        // (the same floor `the_band_is_never_cyan_on_glass` applies), and
        // asking the law to rule sub-floor near-black pixels would make this
        // pin stricter than the law it pins.
        let in_window = |(h, s, v): (f64, f64, f64)| {
            (165.0..=200.0).contains(&h) && s > 0.3 && v * 255.0 > 24.0 && s * v * 255.0 >= 32.0
        };
        let mut exercised = 0usize;
        for r in (0..64u32).step_by(4) {
            for g in (0..192u32).step_by(4) {
                for b in (0..192u32).step_by(4) {
                    let c = (r << 16) | (g << 8) | b;
                    for a in [40u8, 96, 160] {
                        if in_window(over(c, a, RIM_LAW_GROUNDS[0])) {
                            continue; // ground 0 already polices this one
                        }
                        if !in_window(over(c, a, second)) {
                            continue;
                        }
                        exercised += 1;
                        let ruled = clear_light_of_cyan(c, a, second);
                        assert!(
                            !in_window(over(ruled, a, second)),
                            "the rim law left #{c:06X} a={a} cyan over the second                              dark page #{second:06X}"
                        );
                    }
                }
            }
        }
        assert!(
            exercised > 50,
            "the sweep must actually exercise the second ground: {exercised}"
        );
    }
}
