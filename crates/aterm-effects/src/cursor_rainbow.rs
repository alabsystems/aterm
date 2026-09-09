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
//!   typing hard, breathing gently while idle. Every ring is re-solved to ONE
//!   relative luminance ([`RIM_LIGHT`]) before it is laid, so the rim's light
//!   is a function of the paint alone and never of which stop of the arc the
//!   spin parked it on.
//!
//! Under the rainbow the block NEVER BLINKS (owner, 2026-09-08, twice): the
//! host pins the rendered shape steady and never arms the blink deadline while
//! this engine owns the caret. What the blink's charged flip used to fire — a
//! short star FLARE, the fill glinting bright while additive star arms and a
//! couple of glitter dots wink just past the block's edges (the fill is opaque,
//! so only the overhang light shows: a little star flashing behind the block)
//! — now fires on UPWARD crossings of the momentum rungs
//! ([`MOMENTUM_FLARE_STEPS`]) instead: you sparkle as you wind up, never at
//! rest. The flare is a pure clock function (no RNG — the comet-glint
//! precedent) and completes in [`TWINKLE_DUR`].
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
use crate::rainbow_kitty::timing::spring_snap;
use crate::spectrum::clear_thing_of_cyan;

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
/// The rim's light law ([`ring_light_on_page`]) solves each ring's premultiplied
/// bytes for the light they ADD to the page, so it needs the real page. A host
/// that has one passes it as [`RainbowConfig::ground`]; these two stand for the
/// raw/embedder callers that do not: the dark one is `ColorScheme::default`'s
/// own `#111318`, the light one is the built-in light scheme's `#FDF6E3`.
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
///
/// A PAIR, lerped on the caret's paint (the ribbon's display spine — the
/// momentum), since 2026-09-08. AT REST the rim is four rings across a tenth
/// of the wheel — a compact coloured edge. UNDER A FAST HAND it fans across a
/// third of it. Momentum reads as how much of the arc the caret is wearing.
const HALO_HUE_SPREAD_IDLE: f32 = 0.10;
const HALO_HUE_SPREAD_MAX: f32 = 0.34;
/// THE MOMENTUM SPIN (turns per second at full paint). The rim's spectrum
/// rotates at a rate proportional to the display spine, on top of the family
/// phase — the eye reads SPEED as speed and brightness as merely "on", so a
/// caret leaning into a run must visibly turn, not only swell. It is a rate,
/// integrated into [`CursorRainbow::spin`] only across a PAINTED interval and
/// exactly zero at rest by construction: `Spine::at_rest` requires `disp ==
/// 0.0` EXACTLY, which `DISP_SNAP_ZERO` makes reachable, so a resting rim's
/// offset is a CONSTANT, its fingerprint settles, and [`CursorRainbow::is_active`]'s
/// fingerprint law releases the tick on the same law it used before. No idle
/// spin, no idle breath: the ruling against anything that reads as a blink
/// was given twice.
const RIM_SPIN_TURNS_PER_S: f32 = 0.45;

/// **THE RIM'S LIGHT IS HUE-INVARIANT.** Every ring is laid at the light a
/// GREY ring of this relative luminance (linear, `0..1`) would add to the page
/// at the same coverage — solved on the premultiplied bytes over the page by
/// [`ring_light_on_page`]: the arc colour is pulled toward white where it is
/// dark and toward black where it is bright, hue intact either way, until the
/// light it adds matches the grey's.
///
/// **Why it is a law and not a tuning.** The rim SPINS on the momentum
/// ([`RIM_SPIN_TURNS_PER_S`]) and the arc it wears is not one light: measured
/// on the shipped spectrum at full chroma (`shade(spectrum(t), 1, 1)`), relative
/// luminance runs `0.031` (indigo) to `0.924` (yellow) — a THIRTY-FOLD span,
/// mean `0.429` (at the idle shade, `SAT_IDLE`/`VAL_IDLE`, mean `0.418`). A
/// spinning rim whose stops differ thirty-fold in light is a rim that dims and
/// brightens with wherever the spin parked it, and on glass (review of the
/// first cut, 2026-09-08: caret's own ten columns, eight physical px above the
/// cell, no ribbon and no pet in the window) that read as the rim's light
/// SWELLING `+69 %` between 0.3 s and 1.3 s after the hand stopped — while the
/// paint was falling the whole time. Momentum falling and glow rising is a
/// one-cycle pulse after every pause, and the ruling against anything that
/// reads as a blink was given twice.
///
/// **Why it is solved on the glass and not on the colour.** Normalising the
/// un-premultiplied colour to one luminance and then premultiplying is not
/// enough: additive light lands in sRGB bytes, whose transfer is linear below
/// byte `~10` and a `2.4` power above it, so a colour concentrated in one
/// channel (a blue at `255`) adds more light per byte than the same luminance
/// spread across three — measured on this engine's own rasterized rim as a
/// `×1.13–1.16` spread across the sweep at every paint. And the glass adds the
/// stream TWICE — the raw byte add, then the EDR aurora's linear re-emission
/// ([`light_on_glass`] names both) — so a law solved on the byte add alone
/// still left a `+6.5 %` swell on an EDR panel. Solving the light the glass
/// actually shows, over the page it shows it on, takes that to the
/// premultiply's own rounding (`the_rim_light_is_hue_invariant_and_monotone_in_paint`
/// prints the census).
///
/// With every ring at one light the rim's light is a function of `halo_energy`
/// — coverage — alone, and `halo_energy` is monotone in the paint: the glow's
/// decay IS the momentum read and nothing else. `0.42` is the arc's own mean,
/// so the rim's AVERAGE light over a turn is where it was; only its variance is
/// gone. Blue and violet arrive as their pastels, yellow as a gold: still a sky
/// rainbow, and bright rather than dim at every stop — which is also what the
/// 2026-09-01 ruling asked of the arc.
const RIM_LIGHT: f32 = 0.42;
/// The grey of relative luminance [`RIM_LIGHT`] — sRGB byte `173`, the encode
/// of `0.42`. `rim_light_grey_is_the_arcs_mean` pins both halves: this byte's
/// luminance against `RIM_LIGHT`, and `RIM_LIGHT` against the arc's own mean.
const RIM_LIGHT_GREY: u32 = 0x00AD_ADAD;

/// The energy below which the cursor is considered SETTLED — the animator reports
/// itself inactive so the host stops arming the 60 fps tick. A settled caret is a
/// SOLID block inside its resting rim, on no cadence at all (the blink is never
/// armed while the rainbow owns the caret — R4, 2026-09-08).
const SETTLED_ENERGY: f32 = 0.02;

// ── the twinkle (the "glitter star") ────────────────────────────────────────
/// The caret sparkles four times as you wind up to full speed and never at
/// rest. Edge-triggered on upward crossings of these MOMENTUM RUNGS by the
/// caret's paint, with hysteresis: no timer, no idle cost, and strictly fewer
/// flares than the retired blink-flip source produced (one per 530 ms half
/// period while charged). The blink is gone (R4); this is what keeps the star.
const MOMENTUM_FLARE_STEPS: [f32; 4] = [0.25, 0.50, 0.75, 1.00];
/// A rung re-arms only once the paint has fallen this far BELOW it, so a spine
/// hovering on a rung cannot re-fire on jitter.
const MOMENTUM_FLARE_HYST: f32 = 0.06;
/// Flare length (seconds) of one twinkle. Comfortably shorter than the gap
/// between two momentum rungs at any human typing rate (the spine takes ~0.6 s
/// from one rung to the next on the build curve), so every flare completes —
/// and the 60 fps tick disarms — before the next rung can fire one.
const TWINKLE_DUR: f32 = 0.16;
/// §7.1's white HOLD — the one frame a v2 meteor is born on. The engine
/// stamps `flare_at` with the tick's own `now`, so frame 0 sees age exactly 0;
/// the hold is under half a 120 Hz frame so frame 1 is already on the spring
/// at any refresh rate, and clock jitter on frame 0 still reads white.
const FLARE_WHITE_HOLD_S: f32 = 0.004;
/// §7.1: the four halo rings pop radius ×1.4 on the flare's edge …
const FLARE_RING_POP: f32 = 0.4;
/// … and relax on τ 120 ms.
const FLARE_RING_TAU_S: f32 = 0.120;
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
    /// to a steady block whenever the rainbow owns the caret (`fill.is_some()`
    /// — reduced motion and load shed leave `fill` `None`, and the plain blink
    /// is provably restored) and never arms its blink deadline for that
    /// window (R4, 2026-09-08). What the rainbow gives you IN PLACE of the
    /// blink is the twinkle, sourced from momentum rungs
    /// ([`MOMENTUM_FLARE_STEPS`]) rather than from a phase flip — the raw
    /// `blink_phase` argument is no longer read. `false` (a steady block)
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
    /// The rim's light law ([`RIM_LIGHT`]) solves every ring's premultiplied
    /// bytes for the light they ADD to this page, so that a spinning rainbow rim
    /// adds the same light at indigo as at yellow. (Until 2026-09-08 this was
    /// also the ground the rim's own cyan law composited against; that law — the
    /// caret's private copy of the pile-paling the 2026-09-01 ruling retired
    /// everywhere else — is gone, after it was measured on glass stripping the
    /// spun rim to a grey-white edge every time the spin parked it on the
    /// crossing.)
    ///
    /// `None` falls back to the shipped page for the polarity the caller names
    /// ([`GROUND_DARK_THEME`] / [`GROUND_LIGHT_THEME`]).
    pub ground: Option<u32>,
    /// **THE FLARE** (`RAINBOW-KITTY-V2.md` §7.1) — the instant a Rainbow
    /// Kitty v2 meteor's frame-0 flare fired, copied by the host from
    /// [`crate::cursor_glow::CursorGlow::caret_flare_at`]. While `Some`: on
    /// the frame it fired the block fills `#FFFFFF` (a light theme flashes
    /// the vivid live hue instead — white sinks into paper, the same fork the
    /// twinkle glint takes), then the fill mixes from that flash back toward
    /// its field stop on `spring-snap` (critically damped, response 0.18 s:
    /// no undershoot, because a caret that dips past its own colour reads
    /// "not arrived"), and the four halo rings pop radius ×1.4 on the same
    /// edge relaxing τ 120 ms. The engine clears it once the relax has
    /// settled, so a `Some` is always a live relax.
    ///
    /// `None` is the IDENTITY: every expression in the tick runs unchanged
    /// and the emitted bytes are the pre-flare bytes, which is what keeps the
    /// caret's own pins green with the field added.
    pub flare_at: Option<Instant>,
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
    /// The highest momentum rung ([`MOMENTUM_FLARE_STEPS`]) the paint has
    /// crossed UPWARD and not yet released — the twinkle's edge detector. It
    /// climbs on the tick the paint reaches the next rung (firing ONE flare,
    /// however many rungs one tick crossed) and steps back down only once the
    /// paint has fallen [`MOMENTUM_FLARE_HYST`] below the rung it holds, so a
    /// spine hovering on a rung cannot re-fire on jitter. `0` at rest.
    flare_rung: u8,
    /// Start of the in-flight twinkle flare (`None` between flares).
    twinkle_at: Option<Instant>,
    /// Flare counter — the deterministic per-flare variation seed (dot
    /// corners + scintillation phase), and the fingerprint's flare identity.
    twinkle_seq: u32,
    /// THE MOMENTUM SPIN's accumulated offset along the family sweep, in
    /// turns. Advanced by `dt · RIM_SPIN_TURNS_PER_S · paint` across a
    /// continuously PAINTED interval (see the constant), frozen — never
    /// integrated across an idle gap — otherwise, exactly like `phase`.
    spin: f32,
    /// Latched "a flare is mid-flight" at the last tick (the [`is_active`]
    /// clockless answer, like `energy`).
    twinkling: bool,
    /// The fingerprint the LAST tick emitted, and the one before it — so
    /// [`is_active`] can answer from the u8-level delta the host would present
    /// rather than from `paint > SETTLED_ENERGY`. The colour envelope cools on
    /// a τ 0.85 s follower under v2 (`spine.rs`), so `paint` stays above 0.02
    /// for 0.85·ln 50 ≈ 3.3 s while the u8 fill is IDENTICAL frame after frame
    /// — the live capture of 2026-09-05 measured the caret as the sole owner of
    /// the effect deadline at ~5.5 presents/s for ~6 s after every burst, and
    /// the lane's park moving from +0.4 s to +1.47 s. `RAINBOW-KITTY-V2.md`
    /// §7.1 names this exact host follow-up. A frame whose fingerprint equals
    /// the previous frame's cannot differ on glass; the caret asks for no tick.
    fp_last: u64,
    fp_prev: u64,
    /// The rim's pixel buffer for [`Self::rim_light_peak`] — `clear`-and-refill
    /// scratch retained across frames exactly like `RainbowLedger`'s, so laying
    /// the rim out to measure its light is not the only heap traffic on this
    /// path.
    rim_scratch: Vec<u32>,
}

impl CursorRainbow {
    /// Whether the host must keep arming the animation tick: while the cursor is
    /// still CHARGED (typing or cooling) or a twinkle flare is mid-flight.
    /// Once settled it returns false and the caret sits solid inside its
    /// resting rim on no cadence at all — no rainbow-kitty-specific wakeups on
    /// a focused idle window, and no blink either (the host never arms it
    /// while the rainbow owns the caret).
    #[must_use]
    pub fn is_active(&self) -> bool {
        // THE FINGERPRINT LAW (§7.1): charged (`energy`) or mid-flare keeps the
        // tick; a merely COOLING block keeps it only while its last two frames
        // differed — the moment the u8 output settles, identical fingerprints
        // release the lane, and the caret rests solid.
        self.energy > SETTLED_ENERGY
            || self.twinkling
            || (self.paint > SETTLED_ENERGY && self.fp_last != self.fp_prev)
    }

    /// Advance one frame at `now` with the current typing `energy` (`0..1`), the
    /// host's raw cursor `blink_phase` (NO LONGER READ — the twinkle was
    /// re-sourced from momentum rungs on 2026-09-08 and the blink is never armed
    /// while the rainbow owns the caret; the parameter stays on the seam so
    /// every host and the web binding keep their arity), the block cursor cell
    /// `cur` (`None` ⇒ hidden), the theme darkness, grid `geom`, and the
    /// resolved `cfg`. Appends the additive rainbow HALO (+ any twinkle star)
    /// to `out` and returns the block FILL colour + a fingerprint. Pure: no
    /// wall-clock, unit-testable by injecting `now`/`energy`/`cfg.paint`.
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
        _blink_phase: bool,
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
            // Twinkle state clears too: a re-enable starts from rung 0, so the
            // paint it comes back at fires at most one flare, never a stale one.
            self.flare_rung = 0;
            self.twinkle_at = None;
            self.twinkling = false;
            self.fp_prev = self.fp_last;
            self.fp_last = 0;
            return RainbowFrame { fill: None, fp: 0 };
        }
        // **WHERE THIS TICK'S OWN LIGHT STARTS IN THE SHARED STREAM.** `out` is
        // the window's one glow scratch and `CursorGlow::tick` has already filled
        // it by the time the host gets here, so the caret's quads are exactly
        // the tail this tick appends. Marked before the first push and closed
        // after the last: [`Self::rim_light_peak`] measures that tail as the
        // pile it is, because the rim's peak light belongs to no single quad.
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
        let was_painted = self.paint > SETTLED_ENERGY;
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
        // THE MOMENTUM SPIN integrates on the PAINT's own interval, which
        // outlives the cadence's by the ribbon's release: the rim keeps
        // turning — ever slower, in proportion to the spine — for exactly as
        // long as it keeps glowing, and a stop is a deceleration rather than a
        // switch. Same gate shape as `dt` above (a continuously painted
        // interval, never an idle gap), same `MAX_DT` clamp; the accumulated
        // offset is frozen the moment the paint settles, and folded into the
        // fingerprint below so a turning rim is never early-outed.
        let painted = paint > SETTLED_ENERGY;
        let dt_spin = if was_painted && painted {
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
        if painted {
            // Wrapped on the family sweep's OWN period — the sweep is a
            // reflected (ping-pong) walk with period 2.0 (`rainbow_sweep_reflect`),
            // so a `fract()` here would teleport the rim half a period every
            // turn: measured on glass as the outer rings snapping blue → yellow
            // in one frame ~0.56 s into a decay, exactly where a 2.4 s run's
            // accumulator crossed 1.0. `spin_is_continuous_across_its_wrap` pins it.
            self.spin = (self.spin + dt_spin * RIM_SPIN_TURNS_PER_S * paint).rem_euclid(2.0);
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

        // MOMENTUM → TWINKLE (R4, 2026-09-08). The blink is gone, so the flare
        // no longer rides a phase flip: it fires on an UPWARD crossing of the
        // next momentum rung by the caret's paint — the caret sparkles as you
        // wind up to full speed, four times at most per wind-up, and never at
        // rest (a resting paint is exactly 0, below every rung). ONE flare per
        // tick however many rungs the tick crossed (a landing's re-light
        // floors the paint at 1.0 in one step; it earns one star, not four),
        // and a rung re-arms only once the paint has fallen `HYST` below it.
        // No timer, no idle cost: a monotone-decaying scalar crosses nothing
        // upward, so the idle-zero contract is kept by construction.
        // Still gated on `cfg.blinking`: the sparkle is what the rainbow gives
        // you IN PLACE of the blink the terminal asked for. A steady block
        // asked for nothing and gets nothing — there is no blink to replace,
        // so there is no star to stand in for it (`steady_block_never_twinkles`).
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
        let fill = lift_to_light_floor(fill, caret_floor);
        // **THE FLARE** (`RAINBOW-KITTY-V2.md` §7.1): on the frame a v2
        // meteor is born the block fills white (the vivid live hue on a light
        // theme), then mixes back toward the fill every line above resolved on
        // `spring-snap` — critically damped, so it never undershoots its own
        // colour on the way home. `None` is the identity: `fill` passes
        // through untouched and no expression above or below changes.
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

        // The additive HALO: concentric rings around the block. Brightness = a small
        // breathing idle floor + THE PAINT; radius grows with the paint. Purely
        // additive, so it only adds photons around the cell — never over the glyph.
        //
        // THE MOMENTUM GLOW (R4, 2026-09-08: *"I don't like the blinking cursor
        // but I do like some kind of glow indicating cursor momentum"*). This
        // rode `e` — the cadence's ignition heat, gone within ~0.35 s of the
        // last key — so the halo vanished like a switch. `paint` is the
        // ribbon's own display spine (`Engine::caret_paint` = `spine.disp()`
        // floored by a landing's re-light), which climbs the momentum arc under
        // a fast hand and bleeds off over about three seconds when the hand
        // stops. THAT DECAY IS THE MOMENTUM READ: the caret remembers how hard
        // you were going, and a delete run visibly un-earns it. The cadence is
        // still inside `paint` as a floor (`max`), so the attack is unchanged.
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
            // SEPARATE horizontal + vertical reach. The rings grow by a fraction of
            // the cell's OWN width sideways and its OWN height vertically — a single
            // radius scaled by `ch` (cell height) grew the horizontal bars by a full
            // cell WIDTH into the neighbour glyphs (cw ≪ ch on a normal font), which
            // is exactly the "reaches a full cell into neighbours" wash. Bound to
            // ≤ HALO_RADIUS_MAX of each axis so the light HUGS the block: ≤ half a
            // cell sideways (the comment's promise) and the differing x/y growth also
            // means no two layers land the SAME rect, so the thin rings blend into a
            // soft rim instead of double-adding a stacked pair.
            // §7.1: the rings pop ×1.4 on the flare's edge and relax τ 120 ms.
            // `1.0` with no flare — a multiply by one is the bit-exact
            // identity, so every pre-flare ring lands on its pre-flare pixel.
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
            // … and the fan TURNS with the momentum, on top of the family phase
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
                // Each ring samples its own point on the FAMILY's sweep — the
                // rim IS a rainbow, and the whole spectrum still spins with the
                // phase. The step is a distance ALONG the sweep now, not an
                // angle on a private wheel.
                let ring_arc = if layer == 0 {
                    // The innermost rim touches the ribbon nozzle and therefore
                    // wears its exact emitted hue. Outer rings fan through the
                    // family spectrum, preserving the authored rainbow halo.
                    shade(head_rgb, sat, val)
                } else {
                    shade(spectrum_at(ring_sweep, t * hue_spread), sat, val)
                };
                // …AND EVERY RING IS LAID AT ONE LIGHT (`RIM_LIGHT`): the hue
                // is the arc's, the light added to the page is a grey ring's,
                // so the spin can turn the rim through indigo and yellow
                // without the glow dimming and swelling thirty-fold on the
                // way. Coverage alone — `cov`, monotone in the paint — says
                // how bright the rim is.
                let ring_light = ring_light_on_page(ring_arc, cov, page, 1.0);
                rings[n_rings] = (out.len() - rings_from, cov, ring_arc);
                n_rings += 1;
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
                    ring_light,
                );
            }
            // …AND THE PILE AS A WHOLE, because the rings land on each other
            // and stacked light gains from the transfer's convexity by an
            // amount that depends on the colours: solved ring by ring the
            // rasterized rim still spread ×1.04–1.05 across the sweep.
            self.equalise_rim_light(&mut out[rings_from..], &rings[..n_rings], page);
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

        // **AND THE RIM'S OWN LIGHT, MEASURED ON THE PIXEL** — every ring, arm
        // and dot this tick emitted, composited exactly as `aterm_render` will.
        // (The cyan pile-paling that used to run here is retired — see
        // `rim_light_peak`; nothing below recolours a quad.)
        let rim_peak = self.rim_light_peak(&out[emitted_from..], dark_theme);
        // The rim is part of the cursor, but its pixels land OUTSIDE the opaque
        // block and after the ribbon has spent the field's light budget.  The
        // crossing roof made that ordering visible: a legal level-145 field plus
        // the ordinary rim reached L78, and the flare arm reached L109, while a
        // blue-end block sat at its L80 floor.
        //
        // `rim_light_peak` therefore measures the FINAL emitted pile over the
        // field's certified destination.  Keep the ornament intact and lift its
        // opaque centre above that measured peak by the SAME margin the family
        // already promises.  Weight by the live floor so a settled OSC-12 cursor
        // remains exactly its configured colour instead of jumping bright
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
            .wrapping_add(((self.spin * 1024.0) as u64).wrapping_mul(7_919))
            .wrapping_add(twinkle_fp);
        self.fp_prev = self.fp_last;
        self.fp_last = fp;

        RainbowFrame {
            fill: Some(fill),
            fp,
        }
    }

    /// **THE PILE'S LIGHT IS THE GREY PILE'S** — the second half of the rim's
    /// light law ([`RIM_LIGHT`]).
    ///
    /// [`ring_light_on_page`] makes each ring add, ALONE, the light a grey ring
    /// of the same coverage adds. But the rings are designed to land on each
    /// other (*"blend into a soft rim"*), and where two of them stack the
    /// transfer's convexity pays out a bonus that depends on how the light is
    /// spread across the channels — a pastel blue's `255` gains more than a
    /// gold's `179`. Rasterized, the ring-by-ring answer still spread
    /// `×1.04–1.05` across the sweep at every paint.
    ///
    /// So the pile is laid twice into the resident scratch over the page,
    /// through the family's own blend: once as emitted, once with every ring
    /// wearing [`RIM_LIGHT_GREY`] at its own coverage. If the two totals differ
    /// by more than [`Self::RIM_LIGHT_SLACK`], one scalar on every ring's target
    /// is bisected until they agree, and the rings are re-solved at it. The
    /// hues are untouched — each ring is still pulled along its own ray — only
    /// how far along it.
    ///
    /// Eight halvings, each a re-solve of at most four rings and one lay of a
    /// rim a cell-and-a-hem wide. Skipped whenever the ring-by-ring answer is
    /// already within the slack — which is most resting frames.
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
        // The pile's light over the page with ring `i` wearing `colour(i)` —
        // the same two terms as [`light_on_glass`], pile-wide: the byte
        // composite is laid into the scratch (its light is read once at the
        // end, because the bytes saturate and stack), and the EDR aurora's
        // linear re-emission is accumulated per quad-pixel as it is added,
        // because linear light is additive and the pass adds each quad's own.
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
            }
        }
        self.rim_scratch = scratch;
    }

    /// How far the ring-by-ring rim may sit from the grey pile's light before
    /// [`Self::equalise_rim_light`] re-solves it — half a percent, under the
    /// finest byte step the pile has.
    const RIM_LIGHT_SLACK: f32 = 0.005;

    /// **THE RIM'S PEAK LIGHT OVER THE FIELD** — what §8(d)'s lift of the opaque
    /// block is measured against, returned in the `0..255` luminance units
    /// [`lift_to_light_floor`] takes.
    ///
    /// # What used to run here, and why it does not any more
    ///
    /// Until 2026-09-08 this pass was `clear_caret_light_of_cyan`: it laid the
    /// rim out, asked [`crate::spectrum::light_is_over_the_glass_ceiling`] of
    /// every pixel, and bisected one pile-wide `keep` toward
    /// [`crate::spectrum::pale_light_at_constant_light`] until no pixel sat in
    /// the cyan window. It was the caret's private copy of the family's
    /// light-law — and the family's own copy ([`crate::spectrum::clear_light_of_cyan`])
    /// had already been retired by the owner's 2026-09-01 ruling (*"you can have
    /// cyan so long as it's a rainbow"*; *"the anti-cyan laws were what greyed the
    /// arc"*). This one survived the deletion because it did not call the
    /// retired seam; it re-stated the predicate.
    ///
    /// On glass it did exactly what the ruling said such laws do. With the rim
    /// spinning on the momentum (`RIM_SPIN_TURNS_PER_S`), every pause parked it
    /// somewhere on the arc; parked on the green→blue crossing, the pile went
    /// over the ceiling and the whole rim was paled to a grey-white edge
    /// (measured mean band colour `(7.5, 9.9, 8.9)` over the page at 0.3 s
    /// after the last key), then re-saturated as the spin carried it out. A
    /// rainbow rim that flashes grey once per pause is the greyed arc the
    /// ruling deleted, on the caret instead of the ribbon. Gone.
    ///
    /// # What remains
    ///
    /// The rasterization, because §8(d) — *"the caret is the brightest thing the
    /// effect draws"* — is a statement about LIGHT on a PIXEL, and the rim is a
    /// pile: `HALO_LAYERS` rings, two arms and two dots designed to land on each
    /// other, so its peak belongs to no single quad. The pile is composited over
    /// the exact grey level §4 derives for a fully spent field
    /// ([`RAINBOW_FIELD_LEVEL`]) through the family's own blend
    /// ([`crate::spectrum::compose_on_glass`] — `aterm_render`'s arithmetic, not
    /// a model), and the brightest pixel's relative luminance is the answer.
    ///
    /// It is composited with the quads' EMITTED colours. The earlier pass used a
    /// white envelope at each quad's peak channel, which was conservative
    /// against the paling it was about to do — and which made the block's lift
    /// depend on the rim's HUE (a pastel blue's peak channel is `255`, a gold's
    /// `179`). Every ring is now laid at one relative luminance
    /// ([`RIM_LIGHT`]), so the honest composite is hue-invariant to rounding,
    /// and the block's lift is a function of the rim's coverage alone.
    ///
    /// A light page returns `0.0`: §8(d)'s additive-brightest ordering is the
    /// dark-page law, and a light theme deliberately keeps the active block
    /// saturated and dark against white. [`Self::CARET_RASTER_MAX`] is the
    /// backstop for a geometry that ever made the rim large: past it the pass
    /// fails BRIGHT (`255.0`) so the caller lifts the centre to white rather than
    /// silently skipping §8(d).
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

    /// The largest rim, in pixels, [`Self::rim_light_peak`] lays out.
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

/// **THE LIGHT ONE ADDITIVE QUAD-PIXEL PUTS ON THE GLASS**, in relative
/// luminance over `page`'s own — the functional the rim's light law
/// ([`RIM_LIGHT`]) equalises, and it is TWO terms because the glass draws the
/// glow stream twice:
///
/// 1. `fs_glow` (aterm-gpu `renderer.rs`) emits the premultiplied bytes RAW,
///    One/One, over the offscreen's non-sRGB view — byte-exact
///    [`aterm_render::add_sat`], which is what [`crate::spectrum::compose_on_glass`]
///    computes. The blit then decodes that byte to linear for the swapchain, so
///    this term's light is `s2l(page + b) − s2l(page)`: page-dependent, and
///    near-linear in the bytes at the page's own slope.
/// 2. On an EDR panel the aurora pass (`fs_hdr_glow`, the WGSL twin of the
///    proven [`aterm_render::hdr::hdr_additive_encode`]) re-emits the same
///    quads as `s2l(b) · HDR_GLOW_BOOST` in linear light above reference
///    white: page-INDEPENDENT and convex in the bytes — [`crate::color_math::relative_luminance`]
///    of the premultiplied colour itself, times the boost.
///
/// Equalising the first term alone was measured on glass (2026-09-08, the
/// second cut of this law, EDR Mac panel, caret's own ten columns eight px
/// above the cell) as a rim still SWELLING `+6.5 %` in luminance over the
/// first 0.85 s of a pause while the paint sat pinned at its ceiling — the
/// second term is convex, so it pays a colour concentrated in one channel
/// (the green the spin carried the rim onto) more than the same first-term
/// light spread across three (the white the ribbon's head hands the inner
/// ring on the key). Both terms, or neither, is a law about the glass.
///
/// The headroom clamp is not modelled: it bounds emissions near the panel's
/// EDR ceiling, and a rim byte in the tens is nowhere near it. An SDR panel
/// (headroom `0`) skips the aurora pass, and the CPU renderer never runs it;
/// on those the first term alone is what lands, and its spread under this law
/// is printed by `the_rim_light_is_hue_invariant_and_monotone_in_paint` beside
/// the glass figure.
fn light_on_glass(page: u32, premul: u32) -> f32 {
    crate::color_math::relative_luminance(crate::spectrum::compose_on_glass(page, premul, 0))
        + aterm_render::hdr::HDR_GLOW_BOOST * crate::color_math::relative_luminance(premul)
}

/// **ONE RING'S PREMULTIPLIED LIGHT, SOLVED ON THE PAGE** — the rim's light
/// law ([`RIM_LIGHT`]).
///
/// Returns the premultiplied additive colour to push for a ring that wears the
/// hue of `arc` at coverage `cov` and adds to `page` exactly the relative
/// luminance a [`RIM_LIGHT_GREY`] ring at the same coverage would add. The
/// family of candidates is `arc` pulled toward black (`u < 0`) or toward white
/// (`u > 0`) — the two directions that move light without moving hue — and the
/// light each candidate adds is read off what the glass will actually show
/// ([`light_on_glass`]: the byte composite the renderer writes plus the EDR
/// aurora's linear re-emission), so the sRGB transfer's low-byte kink, the
/// aurora's convexity and the premultiply's rounding are all inside the
/// measurement rather than outside it. Monotone in `u` to rounding, so a
/// bisection finds it; of the two bracketing candidates the one nearer the
/// target is returned, so the quantised answer is the best available byte.
///
/// `scale` multiplies the target: `1.0` is the grey ring's own light, and
/// [`CursorRainbow::equalise_rim_light`] bisects it to make the PILE's light
/// the grey pile's once the rings have landed on each other.
///
/// Twenty-two halvings plus a 27-triple neighbourhood, four rings, a handful
/// of times per frame: a few thousand table lookups, small against the
/// frame-cost gate's budget (the retired cyan law rasterized the same rim up
/// to eleven times over two grounds on this path).
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
    // **THEN THE LAST BYTE.** The ray's candidates step three channels at once,
    // so their light is quantised at the coarsest channel's step — ~5 % of a
    // coverage-28 ring, ~10 % of a coverage-12 one. Among the byte triples one
    // level away in any channel, in the SAME hue sector (no channel ordering
    // flips), the one nearest the target is a better byte: the blue channel's
    // `0.0722` weight makes it a knob ten times finer than green's. A level
    // per channel is invisible as colour on a page; as light it is the
    // difference between a rim that is one light and one that is not.
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
            flare_at: None,
        }
    }

    /// §7.1, THE FLARE: `Some(now)` fills the block white on that frame; the
    /// relaxed flare (spring-snap settled) is the `None` frame to the byte,
    /// halo included — so the field's identity claim is measured, not stated.
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

    /// §7.1 + D4, THE LANDING'S CARET LAW, measured on the REAL seam: after a
    /// v2 meteor's one white frame the block mixes toward ITS OWN field stop
    /// — `spectrum(tri(field_t))`, the number the meteor phase-locked at the
    /// spawn — and stays on it for the whole dwell. A colour step with no
    /// keystroke behind it is the defect: on the live capture (2026-09-05,
    /// ctrl-e#1) the caret sat green (95,251,89) for 218 ms and snapped to
    /// salmon (248,117,117) in ONE frame, because the seam re-read the field
    /// every frame and the field's fallback — the newest cell of the band the
    /// jump had ABANDONED — dropped to `0.0` (red) the tick that band was
    /// retired, 0.64 s after the jump that abandoned it.
    ///
    /// The drive is the host's: `CursorGlow` with v2 engaged is ticked, then
    /// the caret block is ticked at the same `now` with the `RainbowConfig`
    /// `app_render.rs` builds from the seam (`rainbow_head_rgb`,
    /// `caret_paint`, `caret_flare_at`), at 120 Hz — nine typed cells, an
    /// idle long enough that the band's exit swoosh ends INSIDE the dwell,
    /// a 20-cell nav jump, and a 400 ms dwell.
    #[test]
    fn the_caret_settles_on_its_own_stop_without_a_pop() {
        use crate::cursor_glow::{CursorGlow, GlowConfig, GlowStyle};
        use crate::rainbow_kitty::meteor::tri;
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
        // The caret's config exactly as the host builds it (app_render.rs,
        // the live `rainbow_cfg`): an unpinned Default-theme cursor, the seam's
        // head colour, paint and flare, the page as the ground.
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
        // Idle 1.20 s at 120 Hz: the band is in its exit swoosh (retracting)
        // when the jump comes, and is retired at last-key + 1.54 s — 340 ms
        // INTO the dwell below.
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
        // The caret's own stop: the field the seam handed out ON THE LANDING
        // FRAME, which is also the meteor's `t_land` (D4, §6.4).
        let field_t = glow.rainbow_field();
        assert!(
            field_t > 0.25,
            "non-vacuous: the walk laid several stops before the jump (t = {field_t})"
        );
        let own_stop = spectrum(tri(field_t));
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

        // LAW 1 — no pop: after the one white frame every per-frame step of
        // the fill is under 40/255 on every channel (the spring-snap from
        // white peaks at ~27/255 per 120 Hz frame; the measured defect is a
        // 151/255 step).
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

        // LAW 2 — the settled fill IS the paint law applied to the caret's
        // own stop: a fresh block, handed the host's config with the head
        // colour replaced by `spectrum(tri(field_t))`, emits the same byte.
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
                flare_at: None,
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
                flare_at: None,
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
            "settled cursor idles (sits solid inside its resting rim)"
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

    // ───────────────────────── the twinkle (glitter star) ─────────────────────────

    /// An UPWARD crossing of a momentum rung while the cursor is charged fires
    /// a twinkle flare: star quads land in the scratch and the block fill
    /// GLINTS brighter than the unflared rainbow. (This fired on a blink-phase
    /// flip until 2026-09-08; the blink is gone — R4 — and the phase argument
    /// is inert, which the constant `true` below is.)
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
        // The glint is measured against a STEADY-block twin on the identical
        // clock and paint — the one caret that cannot flare — so the paint's
        // own mix toward the arc is not mistaken for (or against) the glint.
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

    /// IDLE-ZERO REGRESSION: recurring phase flips at settled energy never arm
    /// the rainbow kitty's effect timer. This was the exact permanent-wakeup
    /// bug under the blink-flip source; under the momentum source the phase is
    /// not even read, and a resting paint (exactly 0) crosses no rung upward.
    /// Twenty half-periods must leave the animator idle after every flip.
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

    /// Tier-1: project the genuine cursor animator's flare generation counter
    /// through a reachable charged-flare → cool → idle-tick trace. The idle
    /// tick deliberately lands while the earlier flare is still active, so a
    /// Boolean-only projection would see `twinkle == 1` both before and after.
    /// `twinkle_seq` makes a forbidden restart observable and rejectable.
    ///
    /// The model's `BlinkCharged` / `BlinkIdle` labels predate 2026-09-08: the
    /// event that fires a generation is now an upward MOMENTUM-RUNG crossing
    /// while charged, and the idle event is any tick at rest (a phase flip
    /// included — it is not read). The abstract law is unchanged: a charged
    /// event starts exactly one generation, an idle event never restarts one,
    /// and every armed flare can finish.
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
        // starts generation 1 — ONE generation, not three.
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

    /// The flare is BOUNDED: once `TWINKLE_DUR` passes with no further rung
    /// crossing the animator re-settles (the 60 fps tick disarms) and the
    /// emitted light is byte-identical to a twin that never flared — the flare
    /// leaves no residue.
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

    /// A STEADY block never twinkles: with `blinking: false` a full momentum
    /// wind-up crosses every rung and fires nothing (there is no blink to
    /// replace, so there is no star to stand in for it), and a flipping phase
    /// argument is not read at all.
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

    /// R4's MECHANISM (2026-09-08): the halo rides the PAINT — the ribbon's
    /// display spine — and not the cadence's ignition heat. With the cadence
    /// at exactly zero, a host paint of 0.8 must still swell and brighten the
    /// rim well past the resting ember; the same tick with no paint is the
    /// ember. And along a decaying paint the innermost ring's coverage falls
    /// MONOTONICALLY — a stop is a decay, never a switch.
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

    /// The momentum rungs are EDGE-TRIGGERED with hysteresis: one flare per
    /// upward crossing, none for hovering on a rung, none on the way down,
    /// one (not four) when a single tick — a landing's re-light — crosses
    /// every rung at once, and a rung re-arms only under its hysteresis band.
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

    /// THE MOMENTUM SPIN turns only while painted and is a CONSTANT at rest:
    /// across a painted run the rim's offset advances; once the paint is
    /// exactly zero, two further ticks emit the same fingerprint and the same
    /// bytes, and the caret reports itself settled — the idle law is intact.
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

    /// THE SPIN NEVER SNAPS. The accumulator wraps on the reflected sweep's
    /// period (2.0), so a full turn of the rim is continuous through the wrap:
    /// at constant full paint every ring quad's colour moves per frame by no
    /// more than the spectrum's own continuity ceiling for a step of
    /// `RIM_SPIN_TURNS_PER_S · 16 ms` — through 5 s of spin, which crosses
    /// both 1.0 (where a `fract()` wrap used to teleport it half a period)
    /// and 2.0 (the true wrap).
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
    /// coverage-capped, and hugging within ~half a cell of the block on BOTH
    /// sides — a twinkle must never wash the neighbour glyphs.
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

    /// The twinkle is a pure clock function: identical instants + identical
    /// paint trajectories ⇒ byte-identical quads and equal fingerprints (the
    /// CPU/GPU parity + repaint-key contract; no RNG anywhere in the flare).
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

    /// **THE RIM'S LIGHT IS A FUNCTION OF THE PAINT AND OF NOTHING ELSE** —
    /// the pin behind [`RIM_LIGHT`], rasterized, because the rim is a pile and
    /// its light belongs to no single quad.
    ///
    /// # The defect this refutes
    ///
    /// Measured on glass at the first cut of the momentum rim (2026-09-08): the
    /// caret's own ten columns, eight physical px above the cell (no ribbon, no
    /// pet in the window), 40 keys at 80 ms then hands off. The rim's light went
    /// `561 → 478 (0.27 s) → 547 → 723 → 698 → 809 (1.28 s) → 682 → 529 → 452 →
    /// 334 → 265 (3.5 s)`: a `+69 %` swell between 0.3 s and 1.3 s after the
    /// last key, with the paint falling the whole time. The spin had parked the
    /// rim on the arc's dark stops at release and carried it out onto the bright
    /// ones — the arc spans thirty-fold in relative luminance — so momentum fell
    /// while the glow rose: a one-cycle pulse, under a ruling given twice
    /// against anything that reads as a blink.
    ///
    /// # What it asserts
    ///
    /// For every shipped caret base, over both shipped dark pages, at each of
    /// five paints, the rim is laid at 129 positions along the family sweep
    /// (every hue the spin can park it on) with a fresh engine at one `now` (so
    /// the spin's own accumulator is zero and the sweep position IS the hue)
    /// and rasterized over the page through the family's own blend. The pile's
    /// light — the sum of relative-luminance excess over the page — must be
    /// the SAME at every position to within [`RIM_LIGHT_TOLERANCE`], and must
    /// rise STRICTLY with the paint at every position.
    ///
    /// **AND THE READING HAS TEETH**: the arc the rings wore before `RIM_LIGHT`
    /// (`shade(spectrum_at(sweep, 0), SAT_MAX, VAL_MAX)`) must itself spread by
    /// far more than the tolerance allows in light — an order of magnitude, on
    /// the arc's own numbers. If it did not, this gate would be measuring
    /// nothing.
    #[test]
    fn the_rim_light_is_hue_invariant_and_monotone_in_paint() {
        /// Ratio `max/min` of the pile's light across the sweep. The
        /// premultiply's rounding is the whole budget: the pile's light is a
        /// sum of BYTES, and `equalise_rim_light`'s two bracketing candidates
        /// are one byte flip apart on the inner ring (bytes `~30` at coverage
        /// `33`), which moved the pile by up to `4 %` on this fixture — so the
        /// nearest reachable pile sits within `±2 %` of the grey's, and two
        /// positions can differ by that twice over. Measured worst at pinning
        /// `×1.037` (paint `0.5`, the theme-polar base over `#111318`); the
        /// census lines this prints carry the live numbers, for the glass
        /// functional and for the byte-add term alone. Against the `×30` span
        /// of the raw arc and the `×1.69` swell measured on glass, `×1.05` is
        /// a law and not a shrug. (`RIM_LIGHT_TOLERANCE_IDLE` is the resting
        /// ember at coverage `5`, where one byte is a fifth of the light.)
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
                        // Energy 0: the paint is the only envelope, the pulse
                        // never advances (breath is the constant 0.5), and no
                        // flare can fire (`blinking: false`).
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

        // **THE READING HAS TEETH.** The arc the rings would have worn without
        // `at_light` — `shade(spectrum_at(sweep, off), sat, val)` at full paint —
        // spans far more than the tolerance in light on its own. This is the
        // pre-law rim's brightness law, and the census above would refute it.
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
        // **AND THE CROSSING IN PARTICULAR** — the green→blue handoff the spin
        // parked the rim on at release in the incident this law answers for
        // (the caret's old cyan law then paled that stop to grey, and the rim
        // re-brightened as the spin carried it onto the green flank). Asked of
        // the table rather than a transcribed position: the arc's own light
        // steps by more than ×2 between the crossing's two flanks, so a rim
        // that wore the raw arc would dim or brighten by that much crossing
        // it, with the paint unchanged.
        let mid = crate::spectrum::spectrum_crossing_position();
        let half = crate::spectrum::spectrum_crossing_width() * 0.5;
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

    /// [`RIM_LIGHT_GREY`] is the encode of [`RIM_LIGHT`], and [`RIM_LIGHT`] is
    /// the arc's own mean light at full chroma — derived here, not transcribed,
    /// so a re-paced arc or a re-typed byte is caught.
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
        use crate::typing_momentum::TypingMomentum;

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
        // THE CEILING IS DERIVED, not chosen — and since 2026-08-31 it is
        // derived against the spine's OWN step rather than against the fastest
        // step an exponential with `TYPING_MOMENTUM_TAU` could take.
        //
        // The colour terms the spine drives span `MIX_MAX - MIX_IDLE` (the mix),
        // `SAT_MAX - SAT_IDLE` and `VAL_MAX - VAL_IDLE`, each of which can move
        // a channel by at most 255. The four flat levels beside them are priced
        // below.
        //
        // **WHY THE `1/τ` FORM RETIRED (2026-08-31).** It bounded the spine's
        // rate by the decay's asymptote instead of by the step the spine
        // actually takes, and then leaned on the difference to cover a term it
        // never named: [`lift_to_light_floor`], which re-solves a bisection
        // toward white every frame and moves the caret's DIMMEST channel
        // fastest as the block cools through the floor. This form bounds each
        // step by the spine's OWN motion and carries that term openly.
        //
        // The four levels are `2` of `f32 -> u8` rounding (`shade` rounds, then
        // `mix_rgb` rounds again, at each end of a step) plus `2` for the
        // floor's re-solve, and that second pair is a REGRESSION PIN on the
        // shipped caret rather than a derivation — the lift's gain is the
        // inverse of the sRGB transfer at the blue channel's `0.0722` weight,
        // which is not a number this fixture can state honestly in one line.
        // Measured worst step on the shipped caret: `7` at frame `6`, where the
        // perceptual re-pace of the arc ([`crate::spectrum::SPECTRUM_ANCHOR_AT`])
        // put a colour `7` levels of blue from this fixture's base at the same
        // sweep the retired arc put one `6` levels away.
        //
        // It still refutes what it exists to catch: a caret that SNAPPED to its
        // base moves tens of levels across a step the spine barely moved at all.
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
            // Continuity is claimed where the caret's LIGHT FLOOR is constant
            // (`smoothstep01(paint / CARET_LIGHT_KNEE) == 1` for
            // `paint >= CARET_LIGHT_KNEE`); the floor's own knee is a
            // deliberately short ramp with its own contract, and folding it in
            // here would measure two laws with one bound.
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
                // …and it only ever COOLS: the distance back to the cursor's
                // own colour never grows during a release.
                // …and it only ever COOLS: the distance back to the cursor's
                // own colour never grows during a release — beyond the floor's
                // own re-solve. Since 2026-09-08 the rim FANS on the paint
                // (`HALO_HUE_SPREAD_IDLE..MAX`), so the outer rings' hues move
                // as the paint falls and `lift_to_light_floor`'s bisection
                // re-solves against a rim whose peak light is not monotone in
                // the paint; that is the same `2` of rounding the ceiling above
                // already carries for the floor. Measured on this fixture: ONE
                // `+1` uptick, at frame 14 (paint 0.894), across the 190-frame
                // release. A caret that snaps back toward its base moves tens
                // of levels in a frame and still refutes.
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

    /// Outer halo rings and the two glitter dots take offsets along the same
    /// spectrum. Every offset must interpolate continuously too; fixing only the
    /// block/head would leave coloured rings popping around an otherwise smooth
    /// cursor.
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
            classic_mono: false,
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
}
