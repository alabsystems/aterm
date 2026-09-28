// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! "LUMEN WAKE" — the paragon cursor aurora. Pure host-side animation that turns
//! cursor motion into emitted LIGHT: a comet of premultiplied additive quads
//! streaking the swept path, a soft bloom crown around the head, and (per style)
//! a particle system — sparkles or rising fire embers. The renderer is a dumb,
//! deterministic compositor of [`GlowQuad`]s; ALL the art lives here.
//!
//! Every emitted quad is PREMULTIPLIED (colour already scaled by coverage via
//! [`aterm_render::premul_rgb`]), single-cell-row, and clamped to the grid
//! interior — the invariants that make the additive light BYTE-EXACT across the
//! Metal and CPU backends and keep the row-scoped damage gate exact. The clock is
//! injected as an [`Instant`], so the whole effect is unit-testable without
//! sleeping, and it decays to EXACTLY empty so the event loop returns to 0% idle.

use aterm_time::Instant;
use std::time::Duration;

use aterm_render::{
    BeamClip, BeamVertex, CharFg, CometSample, FireHaloCell, FireMode, FirePatch, GlowQuad,
    HaloMode, RainHalo, beam_glow_quads, comet_beam, comet_glow_quads, custom_beam_quads,
    phaser_streak_quads, premul_rgb,
};
// THE PROFILE IS THE RASTERIZER'S, and the ribbon reaches it only through
// [`ribbon_beam`] — one law, evaluated in one place. The proofs in this file
// also state it directly (the smoothness oracle derives its bound from the
// profile's own Lipschitz constant).
//
// The JUMP STREAK evaluates it too, and cannot do otherwise: `ribbon_beam` is
// an X-MAJOR rasterizer (a [`RibbonVertex`] is an `x` and a `spine`), and a
// jump runs at whatever angle the caret actually travelled — a wrap, an Enter,
// a screen-crossing fling. So the streak builds its cross-section out of
// abutting [`comet_beam`] tubes and takes each one's WEIGHT from this
// function. That is the same law at a second call site, which is the thing
// worth having; the alternative was a second falloff written out beside it,
// and the whole reason this file has one profile is that it used to have five.

use crate::rainbow_kitty::{self as rk, CaretSeam, CompanionImpulse};

/// Host row slots for the content witness: the bands' eight rows
/// ([`rk::witness::WITNESS_ROWS`]), a waiting key's source row and the rows
/// one above and below it ([`rk::witness::ARMING_ROWS`]), and the caret's
/// row when it is outside both sets. The latter two are only read when
/// needed; the usual typing frame samples the band's rows and nothing more.
/// [`CursorGlow::ribbon_rows`] never names more than [`RIBBON_LIST_ROWS`],
/// so the caret's row, captured first, always leaves a slot for every row of
/// the list.
pub const CURSOR_WITNESS_ROWS: usize = RIBBON_LIST_ROWS + 1;

/// The most rows [`CursorGlow::ribbon_rows`] names: the bands' and a waiting
/// key's arming rows ([`rk::Engine::ribbon_rows_for`]).
const RIBBON_LIST_ROWS: usize = rk::witness::WITNESS_ROWS + rk::witness::ARMING_ROWS;

use crate::effect_util::{
    STAR_ARM_FINE, STAR_ARM_STD, STAR_CORE, STAR_GLINT, STAR_GLINT_COV, STAR_STACK_ADD, dust_r,
    fire_ramp, lerp_rgb, push_fx_rect as push_rect, push_twinkle_star, star_accent, star_arm,
    star_arm_px, twinkle_env, twinkle_peak, water_ramp,
};
// THE ONE COLOUR LAW (`docs/design/RAINBOW-TRAIL-ONE-STORY.md` §2). Every mark
// of the rainbow family resolves its colour through `spectrum` and every
// point-mark snaps to a name through `spectrum_snap`; this file no longer owns
// an interpolation of its own.
use crate::spectrum::{SPECTRUM_STOPS, spectrum, spectrum_snap};
// The LUT's length and the named stops are only ever asked for by the
// proofs — the emit path carries indices and reads colours. Test-gated so
// the lint tells the truth about the shipping binary.
#[cfg(test)]
use crate::spectrum::spectrum_stop;
use crate::trail_sweep::line_cells_tail;
use crate::typing_momentum::TypingMomentum;
// Trail Packs — the user-generated custom trail params driven by `emit_custom`.
// Re-exported here so the resolved `TrailParams` rides `GlowConfig` inline.
pub use crate::trail_pack::TrailParams;
use crate::trail_pack::{Envelope, HaloChannel, RampParams, ThemeArm};

// The engine's parts. A part with public items is re-exported whole, so every
// path that named `cursor_glow::X` still resolves at the visibility it had
// here; the parts with none are `impl CursorGlow` blocks the hub never names.
mod companion;
mod custody;
mod emit;
mod probe;
mod raster;
mod scroll;
mod seam;
mod sound;
mod spawn;
mod status;
mod style;
mod thermal;
pub use companion::*;
pub use custody::*;
pub use probe::*;
pub use raster::*;
pub use scroll::*;
pub use sound::*;
pub use status::*;
pub use style::*;

/// One comet cell, fading from `born`.
#[derive(Clone, Copy)]
struct Spark {
    row: u16,
    col: u16,
    /// Coverage at birth (head bright, tail faint).
    born_cov: u8,
    /// Position along the comet 0.0 (tail) .. 1.0 (head), for the hue sweep.
    pos: f32,
    /// Fade lifetime in seconds. Short for a LONE single-cell TYPING advance (a
    /// tight wake that reads as the cursor LEADING, not dragging), CHAINED to the
    /// observed inter-key cadence during sustained typing (so the streak never
    /// goes dark between keys — see `CHAIN_GAP_MAX`), and the full comet
    /// `duration` for a real JUMP. Per-spark, so typing stays crisp without
    /// shortening the glorious jump comet.
    life: f32,
    /// TYPING spark (single-cell advance)? Typing sparks hold full brightness for
    /// 55% of life then cosine-fade (the chained streak stays luminous across the
    /// inter-key gap); jump sparks keep the classic linear fade.
    typing: bool,
    /// The spectrum hue (turns) this cell's light was LAID in. The PHASER band
    /// renders each cell in its laid hue — the rolling sweep leaves a real
    /// spatial rainbow behind the cursor. LIGHT NEVER RE-COLOURS AFTER IT LEAVES
    /// THE EMITTER: re-sampling the live hue per frame collapses the whole band
    /// into one flickering monochrome bar. Other styles ignore it.
    hue: f32,
    born: Instant,
}

/// One deduplicated Water wake sample for the current frame. This is render
/// scratch, not animation state: the surface and its late falling bead are
/// derived entirely from the owning [`Spark`]'s age on every tick.
#[derive(Clone, Copy)]
struct WaterSample {
    surface: BeamVertex,
    row: u16,
    col: u16,
    drip_cov: u8,
    drip_x: f32,
    drip_y: f32,
    drip_h: f32,
}

/// One light particle (spark / ember), advanced ANALYTICALLY from `born`.
#[derive(Clone, Copy)]
struct Particle {
    x0: f32,
    y0: f32, // window-absolute px at birth (producers add `Geom::origin` once)
    vx: f32,
    vy: f32,   // px/sec
    gy: f32,   // gravity px/sec^2 (down +, up -)
    life: f32, // seconds
    hue: f32,  // 0..1 (sparkle), or warmth seed (fire)
    /// Birth-coverage scale (1.0 = full punch). LASER ablation showers divide
    /// their light across the burst (`1/√burst`) because every spark is born
    /// at the SAME impact point — the cutting-torch read — and a 30-streak
    /// saturating-add pile there clamped the landing to a flat white blob.
    /// Styles with birth SPREAD keep 1.0 (bit-exact through the multiply).
    cov_scale: f32,
    born: Instant,
}

/// One expanding landing ring.
#[derive(Clone, Copy)]
struct Ring {
    cx: f32,
    cy: f32, // grid-interior pixel center
    born: Instant,
    life: f32,
    /// The jump's impact magnitude (`rainbow_kitty::timing::impact`, `1.0..=3.5`):
    /// every expansion radius below is scaled by [`classic_ring_radius_factor`]
    /// of it, and `life` was already graded by [`classic_ring_life`] at spawn.
    scale: f32,
}

// (How much larger a landing star is drawn on LIGHT than on dark used to live
// here as `RAINBOW_LAND_STAR_LIGHT_SCALE = 1.55`. It is the whole family's ink
// tax, not the landing's private one, so it is now `effect_util::STAR_ARM_INK`
// and every light star pays it — the light ribbon twinkle, the fresh-ink spark,
// the poof hero and the landing sparkles used to pay 1.86, 1.71, 1.06 and 1.0.)

/// One FIRE METEOR — the fire style's inter-line / long-jump streak: a
/// straight pixel-space line of fire drawn ALONG the true jump vector, whose
/// tail retracts into the landing over its short life. The vector — never the
/// swept CELL path — is what the streak follows: a cell-path sweep parks a
/// static fire bar on cells the cursor never visited. The landing FLARE slams at ARRIVAL (`arrived`),
/// not at launch, so the eruption happens where — and when — the meteor
/// strikes.
#[derive(Clone, Copy)]
struct Meteor {
    x0: f32,
    y0: f32, // origin (tail) center, grid-interior pixels
    x1: f32,
    y1: f32, // landing (head) center
    born: Instant,
    life: f32,
    /// Display temperature at launch — scales the streak's thickness, with a
    /// floor so even a cold Enter draws a visible (thin) meteor.
    mom: f32,
    /// Whether the head has struck (the arrival flare fired).
    arrived: bool,
}

/// What a [`Vapor`] puff IS — palette + growth curve family.
///
/// `Steam`/`Smoke` are the FIRE style's thermal language (quench flash /
/// hot-metal wisps) and render only while the fire style is active; `Poof` is
/// the style-agnostic ERASE puff (see [`CursorGlow::note_kill`]) — a neutral
/// grey "text went up in smoke" that any style may shed, so [`CursorGlow::emit_vapor`]
/// gates per-PARTICLE rather than per-style.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VaporKind {
    Steam,
    Smoke,
    Poof,
}

/// WHICH NOISE one erase poof makes — the key that licensed it, carried to
/// [`CursorGlow::spawn_poof`] so the cue is chosen where the poof is spawned
/// (one edge, one rate limit, no second source of truth).
///
/// The two erases are different gestures and get different voices: see the
/// SOUND note at the head of [`CursorGlow::spawn_poof`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PoofVoice {
    /// A LINE-scale kill chord (^U/^K): the downward
    /// [`crate::trail_sound::SoundKind::Kill`] swoosh — the erase's whole
    /// voice, since the chord makes no keystroke sound of its own.
    Swoosh,
    /// A WORD-scale kill chord (^W, Alt-D, Alt/Ctrl-Backspace —
    /// [`CursorGlow::note_word_kill`]): the
    /// [`crate::trail_sound::SoundKind::KillWord`] word poof, the erase
    /// poof's slightly larger, softer cousin — one word going, not a clause.
    WordPoof,
    /// A plain BACKSPACE: the tiny [`crate::trail_sound::SoundKind::Poof`]
    /// breath — the CLOUD's noise, the dispersal of the erase poof the
    /// backspace already spoke on its cursor retreat.
    Puff,
}

/// One VAPOR puff — EMBERFORGE steam (quench) or smoke (a hot cursor), or the
/// erase POOF: analytic like [`Particle`] but rendered as a growing, thinning
/// RADIAL halo (never a quad). Steam expands fast, rises with a decelerating
/// kick, and dies young — water flashing off hot steel; smoke drifts up slowly
/// on buoyancy with a sinusoidal waft, swelling and lingering for seconds; a
/// poof is a brief neutral-grey smoke puff — there and gone.
#[derive(Clone, Copy)]
struct Vapor {
    x0: f32,
    y0: f32, // grid-interior pixels at birth
    vx: f32,
    vy: f32, // px/sec
    gy: f32, // px/sec^2 (steam: +drag stalls the rise; smoke/poof: -buoyancy)
    life: f32,
    /// Per-puff seed (waft phase, size jitter).
    seed: f32,
    /// Steam, smoke, or an erase poof — palette + growth curve.
    kind: VaporKind,
    born: Instant,
}

/// One LIGHTNING BOLT — a jagged midpoint-displaced polyline (grid-interior
/// pixels) frozen at spawn so the channel holds its shape for its whole life
/// (per-frame jitter reads as noise, a stable channel reads as a STRIKE). Only
/// its brightness animates: a strobing double-strike envelope (attack → hard
/// dip → restrike → collapse). Main channels are thick; `branch` forks are
/// thinner and dimmer.
struct Bolt {
    /// Jagged channel points, origin → tip.
    pts: Vec<(f32, f32)>,
    born: Instant,
    life: f32,
    /// Per-bolt strobe phase so simultaneous bolts never pulse in lockstep.
    seed: f32,
    /// Branch fork: thinner, dimmer, no white-hot core.
    branch: bool,
}

/// One OUTGOING style CROSSFADE — the complete animated state that was live
/// when the style/pack switched, still ticking under its SNAPSHOTTED old
/// config behind a cosine ramp-down (see the STYLE SWITCH contract in
/// [`CursorGlow::tick`]). The residue must keep rendering under the config it
/// was FORGED under — re-rendering it through the new style's emit path pops
/// its brightness and shape-shifts it — so the whole animator moves here rather
/// than being interpreted anew.
struct OutgoingFade {
    /// The moved-out animator: collections MOVED (the forged light itself),
    /// thermal/phase scalars COPIED (the residue keeps its earned warmth).
    /// Its own `fading` list is empty by construction, so fade ticking is
    /// exactly one level deep — never recursive.
    engine: Box<CursorGlow>,
    /// The outgoing config (style/colours/length/pack), frozen at the switch.
    cfg: GlowConfig,
    /// The cursor cell frozen at the switch, fed as `cur` to every fade tick:
    /// `cur == engine.last` for the fade's whole life, so a move can NEVER be
    /// observed and the residue decays and drifts only (no typing-driven
    /// spawns) — while positional emitters (a standing flame body, the crown)
    /// keep their anchor instead of vanishing on frame one.
    anchor: Option<(u16, u16)>,
    /// Ramp-down envelope start ([`CursorGlow::FADE_OUT_S`]).
    started: Instant,
    /// The amplitude the outgoing style actually HELD at the switch (its own
    /// ramp-IN level when a rapid chain re-switched mid-arrival) — chained
    /// switches hand off at the level they reached, never popping to full.
    level0: f32,
}

/// The two ping-pong buffers [`CursorGlow::spawn_bolt`]'s midpoint-displacement
/// jag swaps between: the polyline so far, and the one being built from it.
type BoltJag = (Vec<(f32, f32)>, Vec<(f32, f32)>);

/// Per-window aurora animation state.
#[derive(Default)]
pub struct CursorGlow {
    /// THE SALVAGED v0.28 WAKE — its own engine, its own state, reached by the
    /// one `GlowStyle::Classic` branch in [`Self::tick`]. Kept inline (rather
    /// than behind a `Box`) because it is a handful of Vecs and an f32 or two,
    /// and it stays EMPTY for every other style: a session that never selects
    /// `classic` pays three unused Vec headers and never a heap byte.
    classic: crate::classic_wake::ClassicWake,
    sparks: Vec<Spark>,
    /// ADMISSION DIAGNOSIS RING: the last [`ADMISSION_LOG_CAP`] content-
    /// candidate lifecycle events (armed/confirmed/retired + reason +
    /// generation + endpoints), served by the `trail` control verb. Purely
    /// diagnostic — written beside decisions, never read by one.
    admission_log: AdmissionLog,
    /// CUMULATIVE count of moves [`Self::spawn`] actually admitted (it returned
    /// `true` — a witnessed move that laid light). The number that separates
    /// "the proof confirmed but the move was never paired at the spawn seam"
    /// from "light was laid and something downstream ate it": a
    /// `confirmed`-heavy [`AdmissionTally`] with `spawns == 0` means admission
    /// and morphology disagree. Diagnostic only, like the ring beside it —
    /// never read by an admission, morphology, or rendering path, and (also
    /// like the ring) deliberately outliving every transient teardown, because
    /// a ledger that resets on the next unowned batch answers nothing.
    spawns: u64,
    /// Resident, bounded swept-cell scratch. Built backward from the landing
    /// point so an outlier cursor jump never walks or allocates the discarded prefix.
    path_scratch: Vec<(i32, i32)>,
    particles: Vec<Particle>,
    ring: Option<Ring>,
    /// The cell height (device px) of the most recent [`Self::tick`], so
    /// [`Self::note_scroll`] — which the host calls with a ROW count and no
    /// geometry — can translate the PIXEL-addressed light pools. Zero until the
    /// first tick; that case fails closed (the pools are dropped rather than
    /// left stranded), because a scroll before any frame has been drawn cannot
    /// have light worth keeping.
    last_ch: u16,
    /// The grid-interior top Y (window px) of the most recent [`Self::tick`],
    /// beside [`Self::last_ch`] and for the same reason: a ROW-BAND move
    /// ([`Self::note_band_move`]) reaches the engine as three row numbers and
    /// no geometry, and the pixel pools it must carry are addressed in
    /// window-absolute px — the band's pixel span is `origin_y + top·ch ..
    /// origin_y + (bottom+1)·ch`, so without the origin a chrome band above
    /// the grid (a titlebar, a tab strip) would mis-place every band edge by
    /// its own height. Zero until the first tick, which the band path treats
    /// exactly as the scroll path treats an unknown cell height: the pools
    /// are dropped, never stranded.
    last_origin_y: u16,
    /// How many ROW-BAND MOVES the host has reported ([`Self::note_band_move`])
    /// — the `band_moves=` column of `trail status`. One per band the host
    /// replayed, not per batch: a Codex answer streaming forty lines reads
    /// `band_moves=40` and `resets=0`; a regression that fell back to the
    /// wholesale reset reads the reverse. A pure counter, never read by the
    /// effect itself.
    coord_band_moves: u64,
    /// THE PRESS-CREDIT RING — the typed-echo coalescer's ledger, one slot
    /// per keyed glyph (`note_typed_glyph`), spent by `classify_move` when a
    /// batched same-row echo is proven to be typing (`rainbow_coalesce`).
    /// Style-agnostic: it is what turns a late three-cell echo into
    /// `Licence::Typed` at the seam instead of a jump.
    type_press_ring: PressCredits,
    last_committed_type: Option<Instant>,
    /// THE FAMILY METRIC ([`crate::typing_momentum`]) — the ONE integrator
    /// the host's companions, the sing-along and v2's spine read
    /// (`RAINBOW-KITTY-V2.md` §17.1: "the FAMILY metric, untouched").
    momentum: TypingMomentum,
    /// The correlated typing advance the host's cursor cat mirrors
    /// ([`Self::take_cursor_cat_motion_pulse`]); set in `spawn`, cleared at
    /// the top of every tick.
    momentum_pulse: Option<CursorCatMotionPulse>,
    /// Live FIRE METEORS (≤ [`Self::METEOR_CAP`]) — the fire style's jump
    /// streaks; see [`Meteor`].
    fire_meteors: Vec<Meteor>,
    /// Live VAPOR (steam + smoke) puffs, rendered as radial halos. Bounded by
    /// [`Self::MAX_VAPOR`].
    vapor: Vec<Vapor>,
    /// When the hot cursor last shed a smoke wisp (rate limiter).
    last_smoke: Option<Instant>,
    /// This frame's RADIAL light ([`RainHalo`]s — fire embers, crown, impact
    /// flash), rebuilt by every [`Self::tick`] alongside `out` and consumed by
    /// the host into `RenderInput.glow_halo`. Soft round light with built-in
    /// integer elliptical falloff — the retirement of the audited square
    /// embers and concentric-box crown. Bounded by [`Self::MAX_HALOS`].
    halo_out: Vec<RainHalo>,
    /// This frame's PER-PIXEL FIRE (campaign 2): [`FirePatch`]es evaluated at
    /// every device pixel by the shared integer field (CPU sampler == WGSL
    /// twin, byte-exact) — the SOTA flame body. Replaces the texel-column
    /// rasterization entirely; drawn at the under-ink seam so the P6 dark
    /// cores keep working. Mode picks Add (dark themes) or Over (ink-fire on
    /// light themes).
    patch_out: Vec<FirePatch>,
    /// This frame's UNDER-INK light (the flame field's BODY, P6): drawn by the
    /// renderer between the cell backgrounds and the glyph ink
    /// (`RenderInput.glow_under`), so letters engulfed by the fire read as
    /// silhouettes INSIDE the flame volume. Tips, embers, beam, crown stay in
    /// `out`/`halo_out` (above the ink).
    under_out: Vec<GlowQuad>,
    /// This frame's CHARRED INK (P6): per-cell final-fg overrides for engulfed
    /// glyphs (`RenderInput.char_fg`) — the letterform itself becomes the
    /// darkest thing inside the flame, scaled by engulfment.
    char_out: Vec<CharFg>,
    /// This frame's CONTRAST-HALO strengths: per-cell fire engulfment weights
    /// (`RenderInput.fire_halo`) driving the dark dilation ring the renderers
    /// stamp UNDER each engulfed glyph's ink — the colour-free legibility
    /// stream (the ink itself never recolours; the no-recolor law).
    fire_halo_out: Vec<FireHaloCell>,
    /// Per-cell engulfment accumulator scratch (row, col, weight), rebuilt by
    /// the flame-field emitter each frame; tiny (≤ ~36 cells).
    engulf_scratch: Vec<(u16, u16, f32)>,
    /// Live LIGHTNING bolts (Laser style; ≤ [`Self::MAX_BOLTS`]).
    bolts: Vec<Bolt>,
    /// Host-fed REDUCED-MOTION arm for the fresh-ink pop: when set, the pop
    /// renders as a brightness STEP-FADE only (no scale spring, no birth
    /// halo). NOTE the GUI's motion policy currently zeroes the whole aurora
    /// amplitude under Reduced (reduced ⇒ `intensity == 0` ⇒ nothing draws,
    /// pops included — which is how load-shed drops pops with the other
    /// effects), so this arm is the engine-level contract for hosts that run
    /// the aurora with reduced DYNAMICS at nonzero intensity (embedders /
    /// future graded policies), mirroring the sparkle/rain reduced twins.
    reduced_motion: bool,
    /// Last observed cursor cell (terminal coords), to detect a move.
    last: Option<(u16, u16)>,
    /// The last VISIBLE cursor cell + when — the ConPTY hide-bridge, mirroring
    /// [`crate::cursor_trail::CursorTrail`]: conhost hides the cursor for
    /// ~20-35ms during every keystroke-echo repaint, and without bridging that
    /// window the glow drops its spark for nearly every key at human typing
    /// cadence (the Windows-only "gaps in the back of the trail"). See
    /// [`crate::cursor_trail::HIDE_BRIDGE_MS`].
    last_visible: Option<((u16, u16), Instant)>,
    /// THE HIDE-BRIDGE ESTIMATE'S WITNESS: the cell the caret was last
    /// actually OBSERVED at,
    /// kept only while [`Self::last_visible`] holds the anchored lane's
    /// RELOCATION — its estimate of where a hidden caret will be shown
    /// (`echo_anchor_pass`, Rainbow Kitty) — and `None` whenever
    /// `last_visible` is an observation. A show frame landing exactly
    /// here, left of the estimate on its row, is no move at all: the
    /// estimate was wrong and the caret never left (`tick`). Rides a
    /// scroll or band move with `last_visible`; dies with the space.
    hide_bridge_shown: Option<(u16, u16)>,
    /// THE ECHO ANCHOR (host-fed, [`Self::observe_print_anchor`]): where the
    /// terminal's most recent PTY print run ENDED — `(row, col, seq)` in the
    /// same active-grid coordinates as `tick`'s `cur`, with `seq` advancing on
    /// every print action so an unchanged position still reads as "output
    /// landed". This is the trail's only truthful sensor for a TUI whose
    /// repaint bracket leaves the DEC cursor hidden (DECTCEM off across
    /// frames) or PARKED away from the caret (CUP 1;1 before show): the
    /// keystroke's echo mutates cells at a row the sampled cursor never
    /// visits, so the cursor-move lane observes nothing — a total,
    /// ledger-silent suppression. See [`Self::echo_anchor_pass`].
    print_anchor: Option<(u16, u16, u64)>,
    /// THE ECHO ANCHOR'S GLYPH (host-fed,
    /// [`Self::observe_print_anchor_glyph`]): what the terminal holds at the
    /// print run's LAST cell — `(row, col - 1)` of [`Self::print_anchor`] —
    /// in the row probe's per-column convention, sampled under the same lock
    /// as the anchor it belongs to. `None` — unknown — whenever the host
    /// handed over an anchor without it ([`Self::observe_print_anchor`]):
    /// the visible-parked glyph gate in [`Self::echo_anchor_pass`] then reads
    /// only the caret row's own probes, and every other lane never reads it.
    print_anchor_glyph: Option<char>,
    /// THE CARET THIS FRAME HANDED OVER IS NOT DRAWN (host-fed,
    /// [`Self::observe_caret_drawn`]): DECTCEM hides it. Only the single-pane
    /// window hands a hidden caret to [`Self::tick`] at all (a hidden cursor
    /// is still a caret, for the pet); every other host hands over `None`
    /// instead and never calls the observer, so `false` — drawn — is the
    /// default. Read by the minibuffer gate in [`Self::echo_anchor_pass`]
    /// alone, which is a law about a VISIBLE caret.
    caret_hidden: bool,
    /// The print-anchor `seq` already consumed by [`Self::echo_anchor_pass`],
    /// so one host sample is judged exactly once.
    print_anchor_seen: u64,
    /// Per-row memory of where the last print run on that row ended — the
    /// launch cell for an anchored echo sweep (a whole-row TUI rewrite passes
    /// through col 0 every keystroke, but its END advances by exactly the
    /// typed cells, so end-to-end is the echo sweep) — plus the program-row
    /// taint brand (see [`AnchorRow`]). Tiny LRU: TUIs interleave
    /// the input row with a spinner/status row, and a single slot would reset
    /// the launch column on every interleave.
    anchor_rows: [Option<AnchorRow>; ANCHOR_ROWS],
    /// Round-robin write head for [`Self::anchor_rows`] insertions.
    anchor_rows_head: usize,
    /// The last LICENSED anchored echo sweep — `(row, at)`. The second
    /// row-discrimination arm: while this is younger than
    /// [`Self::TYPE_HINT_FRESH`], the echo lane is SPOKEN FOR — a different
    /// row's advance may not consume a stamp, however fresh. The taint brand
    /// alone cannot refuse a program row's FIRST-ever end-advance (fc_hidden's
    /// spinner counter holds a constant width until `(9)` becomes `(10)`, so
    /// its first advance can land mid-burst with no keyless history), but the
    /// input row re-lights every ~echo during any burst, so the holder is
    /// always fresher than the stamp window when it matters. Cleared with the
    /// anchor-row memory on reset/scroll (coordinate-space state).
    last_anchor_sweep: Option<(u16, Instant)>,
    /// THE PAID PENDING-WRAP CELL: the row whose
    /// pane-edge glyph the anchored lane licensed as its OWN print (the
    /// `wrap_parked` arm — the print anchor one past the caret parked on
    /// the pane's last column), with no caret move observed since. The
    /// fold that follows from that column carries only the landing row's
    /// head — the last column was paid and lit when it was printed — so
    /// the fold shapes price and the spend takes `cc_local`, not
    /// `cc_local + 1`. One-shot: the next observed caret move consumes it;
    /// it rides its row on a scroll or band move and dies with the
    /// coordinate space.
    wrap_paid_row: Option<u16>,
    /// When the cursor last moved (drives the bloom-crown fade around the head).
    last_move: Option<Instant>,
    /// Deadline until which the bloom crown is still emitted (cached so
    /// [`is_active`] needs no clock); keeps the timer armed past comet decay.
    crown_until: Option<Instant>,
    /// The crown window applied by the LAST move: [`Self::CROWN_TYPING_MS`] for a
    /// single-cell typing advance (chains across human inter-key gaps),
    /// [`Self::CROWN_MS`] for a jump. Consumed by `emit_crown`'s fade math.
    crown_window_ms: u64,
    /// The [`GlowStyle`] this animator ran under LAST tick. `None` until the first
    /// tick. When the live style CHANGES mid-animation, the still-in-flight sparks /
    /// particles / bolts were forged under the OLD style's geometry, colours, and
    /// heat envelope; re-rendering them through the NEW style's emit path pops their
    /// brightness and shape-shifts the residue. The instant this disagrees with
    /// the live `cfg.style`, `tick` MOVES that foreign light into an
    /// [`OutgoingFade`] — where it keeps rendering under its OLD config behind a
    /// ramp-down — keeping the coordinate tracking and the typing warmth live.
    last_style: Option<GlowStyle>,
    /// The Trail Pack fingerprint this animator ran under LAST tick (0 when the
    /// live style was a built-in). A pack→pack swap keeps `style == Custom`, so
    /// the `last_style` guard alone would let foreign in-flight light re-render
    /// under the new pack; comparing this fingerprint too drops that stale light
    /// on any pack change (`TrailParams::pack_fp` is a compile-time hash of the
    /// pack id + source bytes).
    last_pack_fp: u32,
    /// Rolling hue phase (turns) for rainbow/sparkle, advanced per spawn.
    hue: f32,
    /// Typing-cadence HEAT 0..1 — sustained fast typing "accelerates" the wake
    /// (brighter, longer), a pause lets it cool. Decayed lazily from `heat_at`,
    /// so idle gaps cost nothing and never keep the animation timer armed.
    heat: f32,
    /// When `heat` was last decayed (the lazy-decay stamp).
    heat_at: Option<Instant>,
    /// Jump FLARE 0..1 — slammed to full by a real cursor jump, cooling fast
    /// ([`Self::FLARE_DECAY_TAU`]). Fire's blaze level is `heat.max(flare)`, so a
    /// long leap ignites the comet white-hot even from a cold keyboard, then the
    /// streak visibly cools back through orange to deep red as it fades.
    flare: f32,
    /// EMBERFORGE COAL BED 0..1 — the slow thermal integrator under the fast
    /// heat: FORWARD keystrokes at any human cadence charge it (a much wider
    /// cadence window than the fast heat's), and it cools over seconds
    /// ([`Self::COAL_TAU`]), so relaxed typing keeps a visible ember floor
    /// alive across thinking pauses, while full blaze is EARNED over a sentence
    /// or two of sustained forward momentum, never seven keys.
    coal: f32,
    /// Backspace QUENCH meter 0..1: each delete adds [`Self::QUENCH_GAIN`]
    /// (decaying on [`Self::QUENCH_TAU`]), damps the display temperature —
    /// deletion DOUSES the fire instead of feeding it — and escalates, so a
    /// backspace RUN reads as a real quench. Fed at the host key edge by
    /// [`Self::note_backspace`]; only a later exact candidate may move visibly.
    quench: f32,
    /// A fresh backspace classifier ([`Self::note_backspace`]). It can classify
    /// an already content-confirmed one-cell candidate as a DELETION (no
    /// heat/coal gain), but never admits an observed move by itself. Expires
    /// after [`Self::QUENCH_HINT_FRESH`].
    quench_hint: Option<Instant>,
    /// A fresh NAVIGATION classifier ([`Self::note_navigation`]). The timestamp
    /// never admits a cursor delta; it only suppresses an older ambiguous class,
    /// so Ctrl-A/E, Home/End, arrows and word motions cannot turn a later child
    /// relocation into heat or light. Expires after [`Self::NAV_HINT_FRESH`];
    /// unlike the quench hint it never cools heat/coal.
    nav_hint: Option<Instant>,
    /// A fresh TYPED-GLYPH key-hint ([`Self::note_typed`]): armed by the host on
    /// a PLAIN Character/Space echo key only — never Enter, Tab, navigation, or
    /// modified chords, whose jumps keep the owner-mandated meteors/ZOOMs.
    /// Once [`Self::note_typed_expected`] has supplied and confirmed the exact
    /// content/target candidate, this hint classifies a one-row delta beyond the
    /// typed advance as a TYPED RE-ANCHOR (see `spawn`): a TUI that repaints its
    /// inset input box per keystroke can relocate the caret without sweeping the
    /// interpolated cells. The timestamp alone never admits that relocation.
    /// Each stamp expires after [`Self::TYPE_HINT_FRESH`] and is one-shot —
    /// consumed by exactly one observed echo sweep. BANKED, not 1-deep (see
    /// [`TypedStamps`]): K presses inside one frame gap bank K stamps, so an
    /// echo arriving as several sweeps licenses every one of them instead of
    /// declining all but the first into a background-black hole.
    type_hint: TypedStamps,
    /// A fresh RETURN key-hint ([`Self::note_return`] — a main-screen Enter).
    /// This is retained only as a bounded morphology/classifier signal; it is
    /// never movement provenance. A Return has no causal content witness, so
    /// its cursor relocation stays dark under the exact-candidate gate.
    return_hint: Option<Instant>,
    /// A fresh USER-GESTURE classifier for cursor-moving input whose echo is
    /// not a typed glyph: a Tab's cross-row completion, a scripted preview,
    /// the classic trail's lockstep twin of an insert. Under v2 it is handed
    /// over as `Licence::Synthetic` (a wake + meteor, not a walk), which is
    /// why a delivered insert has its own class ([`InsertSeam`]) and this
    /// one no longer carries a paste. Controller,
    /// mouse and wheel reports cannot prove which later PTY move they
    /// caused, so production hosts cancel those candidates fail-closed.
    user_gesture_hint: Option<Instant>,
    /// THE DELIVERED-INSERT SEAM ([`InsertSeam`]): the licence a delivered
    /// insert arms, its undo, the span it laid, the refused hop one delivery
    /// may retro-license, the presses an unknown width's hop echoed, and
    /// `trail status`'s insert rows.
    insert: InsertSeam,
    /// The in-flight rows of `trail status` ([`InFlightTally`]).
    in_flight_tally: InFlightTally,
    /// A same-row backward typed-paired move HELD for its return
    /// ([`HeldPark`]). Flushed by every edge that would have judged it.
    held_park: Option<HeldPark>,
    /// This tick's source prefix was sampled unchanged before the witness
    /// row count was consumed, and the key did not carry the source row's
    /// text down with the caret ([`CursorGlow::key_pushed_text_down`]).
    /// Recomputed on every tick; never carried as permission into another
    /// observed frame.
    park_source_intact: Option<(u16, u16)>,
    /// The source tail proved this tick's cross-row caret move belongs to
    /// the unpaid glyph. Captured before the witness samples are taken and
    /// cleared immediately after the visible move is judged.
    carried_key_move: Option<((u16, u16), (u16, u16))>,
    /// Original per-column prefix, plus one lookahead for a wide unit.
    /// One bounded resident buffer, populated only for a cross-row park.
    park_source_cells: Vec<char>,
    park_source_confirmed: bool,
    /// The row of the last LICENSED move and its clock — the insert's row
    /// identity witness ([`Self::insert_row_identity`]): a row the hand was
    /// licensed on within the ribbon's chain window may spend a partial
    /// insert echo; any other row must show the insert's whole width.
    last_licensed_row: Option<(u16, Instant)>,
    /// A fresh COMPOSER-NEWLINE hint ([`Self::note_newline_break`] — an
    /// alt-screen Shift+Enter, the chord agent composers bind to "insert a line
    /// break without submitting").
    ///
    /// THE ONE SIGNAL THE GEOMETRY CANNOT CARRY. The TUI-relocation retirement
    /// (see [`Self::retire_abandoned_light`]) retires the previous row's ribbon
    /// on a typing-classified row change that no fold, gesture or scroll
    /// explains, because such a change means a repaint MOVED the input line and
    /// the light above is decorating cells that no longer hold what laid it. A
    /// composer newline produces the identical observed move — one row down,
    /// back to the box's left inset — and is the exact opposite case: the user
    /// authored that line break, and the row it leaves still holds the text they
    /// just typed. Nothing in `(pr, pc) → (cr, cc)` separates the two, which is
    /// why this was carried as a known residual until the host started arming
    /// the distinction.
    ///
    /// Shift+Enter also carries typed morphology once an exact candidate exists,
    /// so it cannot be told from a glyph echo by movement shape either. A
    /// licence term ([`Self::one_shot_licensed`], the park refusal) that is
    /// CONSUMED ONCE at the spawn seam, exactly as the Return's is: the
    /// composer's own row change takes it (`Licence::Return`), and the
    /// peeks then see a spent hint (peeked and never taken, one chord would
    /// license every program move for 250 ms). Expires after
    /// [`Self::RETURN_HINT_FRESH`] — the same bounded classifier window as
    /// Enter.
    newline_hint: Option<Instant>,
    /// A fresh REFLOW classifier. Coordinate-space changes reset both engines;
    /// no trustworthy old path exists in the new geometry, so reflow is
    /// deliberately dark and this timestamp never admits a movement birth.
    reflow_hint: Option<Instant>,
    /// A fresh KILL key-hint ([`Self::note_kill`] — Ctrl-K/U/W, Alt-D,
    /// word-delete Backspaces, forward Delete): the license for the erase POOF.
    /// A poof can only fire within [`Self::KILL_HINT_FRESH`] of a kill key AND
    /// a same-row NET SHRINK of the probed row content (see `poof_scan`) — the
    /// key alone never poofs (a kill that erased nothing is silent), and a
    /// shrink alone never poofs (Ink repaints rewrite rows constantly).
    kill_hint: Option<Instant>,
    /// WHICH SCALE the fresh kill hint is: `true` iff [`Self::note_word_kill`]
    /// armed it (^W, Alt-D, Alt/Ctrl-Backspace — one word going), `false` for
    /// the line-scale chords (^K/^U — a clause). Read exactly once, where the
    /// poof chooses its voice ([`PoofVoice`]); carries no freshness of its own
    /// because it is meaningless without a fresh `kill_hint`, and every
    /// `note_kill` re-stamps it.
    kill_hint_word: bool,
    /// The instant of a caret-MOVING kill whose own caret retreat is still
    /// OWED: stamped by [`Self::note_kill`] with `moves_cursor`, spent by
    /// the first nav-shaped move `spawn` judges within
    /// [`Self::KILL_HINT_FRESH`] of it, and superseded by a real navigation
    /// press ([`Self::note_motion`]). The retreat is ONE move; everything
    /// after it is navigation — [`Self::kill_hint`] alone is cleared only
    /// by the poof that fires or by expiry, so a Ctrl-W whose erase was
    /// never witnessed (a second kill inside `POOF_MIN_GAP`, a scrolled or
    /// unwired probe, a ContentOnly alt-screen frame) would swallow the
    /// user's next real word move. It carries ITS OWN clock: the kill hint
    /// is also the poof's row-content proof, which the host's
    /// `drop_row_probe` after every band-move and scroll replay clears
    /// without the retreat changing class.
    kill_retreat_pending: Option<Instant>,
    /// The kill — by its key's stamp, the one [`Self::kill_hint`] and
    /// [`Self::kill_retreat_pending`] carry — whose `rk::Event::Kill` has
    /// already gone to v2, from whichever witness came first: the retreat
    /// (the move seam, so the ribbon takes the erase-retreat arm) or the
    /// poof that proves the span. ONE KILL, ONE `Kill`: keyed on the kill's
    /// own stamp, a new kill is simply a new key, and a witness for a kill
    /// already reported stands down (a slow link lands the retreat after
    /// `POOF_FALLBACK_GRACE`, when the caret fallback has already minted the
    /// kill; a second `note_kill` before the first's poof scan is a new
    /// key). A stationary ^K / Alt-D never moves the caret, so its poof's
    /// `Kill` is the first witness, as ever.
    kill_reported_to_v2: Option<Instant>,
    /// A fresh PLAIN-BACKSPACE poof license ([`Self::note_backspace`]), kept
    /// SEPARATE from [`Self::kill_hint`] so it never borrows the kill hint's
    /// nav/quench escalation semantics. `poof_scan` ORs it into the
    /// `fresh_kill` gate, so a plain Backspace licenses the erase POOF on the
    /// SAME Full-trust proof surface the kill chords use: the shared
    /// full-trust span-shrink diff / witnessed caret fallback covers the erase
    /// (a Backspace that provably erased nothing stays silent — see
    /// `bs_only` in `poof_scan` — and a ContentOnly probe licenses no poof
    /// branch at all, see [`ProbeTrust`]). Cat-INDEPENDENT:
    /// `poof_scan` never consults the
    /// cursor cat, so the delete burst fires whether or not the cat is flying
    /// (the cat's tongue-out "oops" layers on top when it happens to be present).
    bs_poof_hint: Option<Instant>,
    /// Legacy kill/fallback timing witness: the cursor row's FILL as it stood
    /// when the erase key arrived —
    /// `(row, fill)`, stamped by [`Self::note_backspace`] AND [`Self::note_kill`]
    /// from whatever probe the host had last handed over.
    ///
    /// WHY A STAMP AND NOT THE LIVE DIFF. `poof_scan`'s span branch proves an
    /// erasure by diffing the PREVIOUS presented row against this frame's. That
    /// works while frames are flowing, and only while frames are flowing: the
    /// host probes the row as part of composing, so on a QUIET screen the first
    /// probe a lone Backspace ever produces is already the POST-erase row, and
    /// every diff from then on is post-vs-post — provably equal, forever. The
    /// Plain Backspace shares this witness with the kill chords.
    ///
    /// It is ALSO the clock the caret fallback's grace can be released early
    /// against — see [`Self::poof_erase_witnessed`].
    bs_baseline: Option<(u16, u16)>,
    /// A fresh REPAINT-BLINK hint ([`Self::note_repaint_blink`]): the host saw
    /// the attached app's DECTCEM-hide-inside-DEC-2026 repaint bracket (the
    /// terminal's `repaint_blink_epoch` advanced — Claude Code wraps EVERY
    /// keystroke's redraw in one). On the ALT screen (`ctx_alt`) the typed/
    /// backspace RE-ANCHOR classifier additionally requires this hint to be
    /// fresh ([`Self::BLINK_HINT_FRESH`]): a full-redraw TUI re-anchors its
    /// caret per keystroke. This only tunes already-admitted candidate
    /// morphology; the blink itself never authorizes a vim/less relocation.
    /// Main screen is blink-blind (the conjunct is vacuous there).
    blink_hint: Option<Instant>,
    /// Per-frame ALT-SCREEN context ([`Self::note_context`], stamped by the
    /// host beside the row probe each frame). Gates the re-anchor's blink
    /// requirement: `false` (main screen / unwired host) keeps the shipped
    /// classifier byte-identical.
    ctx_alt: bool,
    /// Focused pane columns in the window-coordinate space consumed by this
    /// engine: `(first_col, width)`. The host refreshes it before every tick so
    /// a right-hand split's margin is classified against its pane, not against
    /// the whole window. `None` falls back to [`Geom::cols`] for embedders.
    pane_columns: Option<(u16, u16)>,
    /// When the last erase poof fired — the rate limiter ([`Self::POOF_MIN_GAP`])
    /// so a held kill-key repeat billows at a readable cadence, not per frame.
    last_poof: Option<Instant>,
    /// The fresh-typed GLYPH TINT's palette, memoized on the `(theme_fg,
    /// theme_bg)` it was derived from.
    ///
    /// [`fresh_ink_glyph_palette`] bisects up to [`FRESH_INK_GLYPH_BISECT`]
    /// times per band over seven bands, and each probe evaluates a `powf`-based
    /// luminance — a few hundred gamma evaluations. That is nothing once, and
    /// silly every frame for a value that only changes when the THEME changes.
    /// Refreshed in [`Self::tick`], where `&mut self` exists; the emitter is
    /// `&self` and reads it.
    ///
    /// `Some((key, None))` is a memoized REFUSAL — a theme with no usable side
    /// costs one evaluation too, and re-deciding it every frame would be the
    /// same waste.
    glyph_palette: Option<GlyphPaletteMemo>,
    /// ERASE MOMENTUM — the mirror of the canonical typing metric, for the
    /// gesture going the other way (owner, 2026-08-06: bring the delete poof
    /// back "with a similar momentum as forward typing").
    ///
    /// The SAME law, deliberately: a [`TypingMomentum`] instance, the same
    /// rate-normalized accrual, the same 2 s decay. Only the gesture that feeds
    /// it differs — every erase edge ([`Self::note_backspace`],
    /// [`Self::note_kill`]) credits it, and nothing else can. So a held
    /// backspace run builds exactly the way a typing run builds, one erase at a
    /// time, and a single correction mid-sentence stays a single small puff.
    ///
    /// It does NOT touch [`RainbowState::momentum`]: forward momentum still
    /// only builds from forward typing and still DRAINS on a delete (the
    /// erase-never-builds law is untouched). This is a second, independent
    /// metric that exists so the delete gesture has a speedometer of its own.
    erase_mom: TypingMomentum,
    /// SOUND CUES recorded at the spawn/poof edges this frame — the aural
    /// twin of the visual spawns, drained by the host each tick
    /// ([`Self::drain_sound_cues`]) and fed to the trail synth
    /// ([`crate::trail_sound::TrailSynth`]). Bounded ([`Self::MAX_SOUND_CUES`])
    /// and cleared on every disable/reset path, so the proven-inert contract
    /// holds for sound exactly as for light.
    sound_cues: Vec<SoundCue>,
    /// KEY-TIME CLICK CREDITS: typing clicks the host already cued at the
    /// PHYSICAL keypress ([`Self::cue_keystroke`]) whose echo has not been
    /// observed yet. The echo path consumes one per typing-classified spawn
    /// and stays SILENT for it — one character must click exactly once, and
    /// without the ledger a key-time click plus its echo click would double.
    /// Capped at [`Self::MAX_KEYED_CLICKS`] so a run of keys an app SWALLOWS
    /// (a password prompt, vim normal mode) cannot bank unbounded silence.
    keyed_clicks: u8,
    /// When the NEWEST key-time click was cued — the credits' freshness
    /// anchor. Whole-burst semantics deliberately: five keys inside one
    /// [`Self::KEYED_CLICK_FRESH`] window keep all five credits alive, so a
    /// batched five-cell echo run stays silent for all of them. Past the
    /// window the ledger zeroes, so a swallowed key can mute at most one
    /// window's worth of later output-driven clicks instead of leaking
    /// silence forever.
    keyed_click_at: Option<Instant>,
    /// When the PREVIOUS key-time click was cued — the inter-key cadence clock
    /// the timbre prediction reads ([`Self::cue_keystroke`]). Deliberately NOT
    /// [`Self::keyed_click_at`]: that one zeroes the instant the ledger empties,
    /// which at ordinary typing speed happens after every single echo, so it
    /// would report "no previous key" mid-burst and predict a cold click.
    /// Never cleared on the dark/reset paths either — a stale value only
    /// widens the measured gap, and a wide gap earns exactly zero cadence
    /// credit, which is the right answer for the first key of a new burst.
    key_cue_at: Option<Instant>,
    /// The live style/pack's per-keystroke heat GAIN and cooling τ,
    /// snapshotted ([`Self::store_key_timbre`]) by every [`Self::tick`] that
    /// may open the key seam — every tick past the MASTER SWITCH, the classic
    /// wake and a degenerate grid included — not only by a drawing one. Both are pure functions of the config
    /// (neither reads `intensity`), so the dark-but-audible path computes
    /// bit-identical values, and a click minted during a load-shed frame
    /// reads THIS tick's timbre rather than the last lit tick's. [`Self::cue_keystroke`] runs from the
    /// host's key handler, which holds no [`GlowConfig`] — without these it
    /// could not reconstruct the heat the echo is about to reach, and the
    /// key-time click would carry the PRE-keystroke timbre (one keystroke of
    /// lag in the sound, the exact "feels behind" the seam exists to remove).
    /// Read only behind [`Self::sound_live`], and no tick opens that seam
    /// without writing these first (every [`Self::settle_key_seam`] call site
    /// stores them above it), so the zeroed `Default` is unreachable in the
    /// cue path.
    heat_gain_live: f32,
    heat_tau_live: f32,
    /// Whether the LAST [`Self::tick`] left the key seam OPEN — it ran the
    /// live path (master on, real geometry, nonzero amplitude), OR it went
    /// dark for any reason but the master switch (a motion policy, the
    /// load-shed envelope, a degenerate grid, the classic wake's own dark
    /// frame) while [`GlowConfig::audible`] said the key still belongs to
    /// this window. One law, [`Self::settle_key_seam`], on every style.
    /// [`Self::cue_keystroke`] records nothing unless this is set.
    ///
    /// THE LAW IT CARRIES, restated: sound is silenced by exactly the gates
    /// that are ABOUT SOUND. It used to be "the gates that silence the
    /// light", which handed a load-shed latch and macOS `Reduce Motion` a
    /// mute switch over a click that costs no GPU. The master switch and
    /// serious mode still close it (they arrive as [`GlowConfig::enabled`]),
    /// unfocus still closes it (it arrives as `audible = false`), and the
    /// host's own sound knobs never reach here at all — they gate before a
    /// cue is minted. Also false before the first tick, so a host that never
    /// ticks (headless, tests) is byte-identical.
    ///
    /// It has an OUT-OF-BAND READER: [`Self::sound_seam_open`], which
    /// `aterm ctl tone` prints as `engine_sound=` and `aterm ctl trail
    /// status` as `sound_seam=`. A future edit to either dark return is
    /// changing a user-visible answer, not only a private gate.
    sound_live: bool,
    /// THIS frame's cursor-row content probe ([`Self::observe_row`]) — the
    /// newer half of the poof detector's double buffer. Per-COLUMN chars
    /// (resolved lead char at its column, `'\0'` at wide continuations, `' '`
    /// for blanks) so the vanished-span math survives CJK/emoji. Reused
    /// capacity: zero steady-state allocation on the probe path.
    row_cur: Vec<char>,
    /// The LAST-observed row probe (rotated from `row_cur` by `poof_scan` via
    /// `mem::swap`) — always the last-presented truth: content cannot change
    /// without damage → present → tick, so staleness is bounded by
    /// [`Self::POOF_PROBE_STALE`] only as belt-and-braces.
    row_prev: Vec<char>,
    /// Metadata for `row_cur` (`None` ⇒ no fresh probe arrived this frame; the
    /// previous probe stays in place and the detector idles).
    row_cur_meta: Option<RowProbe>,
    /// Metadata for `row_prev`.
    row_prev_meta: Option<RowProbe>,
    /// Exact glyphs of the last adjacent, licensed typed run. Kept outside
    /// the ribbon pool and deadline calculation after its visible cells
    /// retire; read only by the next typed echo on this row.
    recent_typed_run: Option<RecentTypedRun>,
    /// THIS frame's capture of the row ABOVE the probed cursor row
    /// ([`Self::observe_neighbor_rows`]) — the content witnesses' newer half
    /// (see [`NbrProbe`]; f207c75f5 dropped these copies when the stars moved
    /// to v2's bitsets, and the soft-wrap and minibuffer witnesses brought
    /// them back). Same per-column char convention as `row_cur`. Readable
    /// ONLY through the
    /// probe metadata's [`NbrProbe::Probed`] state, so a frame that skipped
    /// the neighbor capture can never serve these bytes stale.
    row_above_cur: Vec<char>,
    /// Last-presented capture of the row above (rotated from `row_above_cur`
    /// by `poof_scan`, in lockstep with `row_prev`).
    row_above_prev: Vec<char>,
    /// THIS frame's capture of the row BELOW the probed cursor row.
    row_below_cur: Vec<char>,
    /// Last-presented capture of the row below.
    row_below_prev: Vec<char>,
    /// EASED display temperature 0..1 — the ONE number every fire layer reads
    /// (see [`Self::fire_t`]): it chases `max(heat, flare, coal floor)` damped
    /// by the quench, with a short attack and a slower release, so the whole
    /// look ramps as one body and no layer ever pops. Fire-only (other styles
    /// keep the raw, instant [`Self::blaze`]).
    disp_t: f32,
    /// The flame field's integrated TIME PHASE (seconds, churn-weighted):
    /// advanced by dt·churn(T) in the lazy-decay block, so the field speeds up
    /// smoothly as the fire builds instead of rescaling (which would pop), and
    /// never resets mid-burn. Pure function of injected clocks.
    flame_phase: f32,
    /// The cursor METAL's temperature 0..1 — hysteretic (heats over
    /// [`Self::TEMP_ATTACK_TAU`], cools over the slower
    /// [`Self::TEMP_RELEASE_TAU`], 2× faster while quenched): the forge arc a
    /// real tool has. Drives the block-cursor forge fill
    /// ([`Self::forge_fill`]) and, later, the smoking threshold. Fire-only
    /// (zeroed for every other style so its visibility arm in
    /// [`Self::is_active`] is inert there).
    cursor_temp: f32,
    /// Last TYPING advance (single-cell move), for the inter-key cadence.
    last_type: Option<Instant>,
    /// Deterministic spark/ember PRNG state.
    rng: u32,
    /// Reused each frame by [`Self::emit_comet`] and [`Self::emit_custom`] as the
    /// swept-cell run builder, so the animated beam reuses one resident nested Vec
    /// instead of allocating a fresh `Vec<Vec<CometSample>>` every redraw.
    /// `comet_run` is the in-progress run.
    ///
    /// The INNER buffers are pooled too: each emitter `clear()`s the existing
    /// entries in place (never `comet_runs.clear()`, which would free them) and
    /// publishes a finished run by SWAPPING it with the pooled entry at the live
    /// watermark — so `comet_run` always comes back holding an already-allocated,
    /// already-emptied buffer instead of the capacity-0 `Vec` a `mem::take` would
    /// leave behind. Entries past the emitter's local `runs_len` watermark are
    /// spares from earlier frames, which is why every consumer reads
    /// `&comet_runs[..runs_len]` rather than the whole spine. Growth is bounded by
    /// runs ≤ sparks ≤ [`Self::MAX_SPARKS`].
    ///
    /// Shared between the two emitters on purpose: `tick` takes exactly ONE of the
    /// two arms per frame and both clear on entry, so a style switch cannot leak
    /// samples across frames.
    comet_runs: Vec<Vec<CometSample>>,
    comet_run: Vec<CometSample>,
    /// Reused by [`Self::emit_water`] for the deduplicated curved wake spine
    /// and its analytically derived falling beads. Keeping this resident makes
    /// the fluid path allocation-free after its first growth.
    water_samples: Vec<WaterSample>,
    /// Fixed-size open-address owner table for [`Self::emit_water`]. A
    /// repaint-heavy TUI can revisit the same cell hundreds of times before
    /// the old samples expire; newest-visible-wins here prevents those owners
    /// from compositing into an opaque reflection. Fixed to twice the resident
    /// spark cap, so clearing it is O(MAX_SPARKS), never O(grid area).
    water_seen: Vec<u64>,
    /// Reused by [`Self::emit_bolts`] (Laser strikes) for the per-bolt polyline,
    /// so a strike frame allocates nothing after the first growth.
    bolt_verts: Vec<BeamVertex>,
    /// Ping-pong scratch for [`Self::spawn_bolt`]'s midpoint-displacement
    /// rounds. The jag used to build a FRESH `Vec` per round — `vec![from, to]`
    /// plus one `Vec::with_capacity` per round, two rounds for a crackle and
    /// four for a jump — so every strike cost three heap buffers to describe a
    /// polyline five points long, and hot typing crackles several a second.
    /// Taken out of `self` around the jag because the loop calls
    /// [`Self::frand`], and handed straight back.
    bolt_jag: BoltJag,
    /// Reused by [`Self::emit_comet`]'s Laser arm for the per-layer filament
    /// polyline, so a laser frame allocates nothing after the first growth.
    comet_verts: Vec<BeamVertex>,
    /// Live OUTGOING style crossfades (≤ [`Self::FADE_CAP`], oldest dropped) —
    /// see [`OutgoingFade`] and the STYLE SWITCH contract in [`Self::tick`].
    /// Empty in steady state: the no-switch tick path pays ONE `is_empty` test,
    /// so every built-in style's output stays byte-identical (the golden proof).
    fading: Vec<OutgoingFade>,
    /// Resident quad scratch for the fade ticks (their `out` twin), so a
    /// crossfade frame allocates nothing after the first growth.
    fade_scratch: Vec<GlowQuad>,
    /// When the LIVE style last ARRIVED (a switch): drives the incoming
    /// ~[`Self::RAMP_IN_S`] intensity ramp-IN so the new style rises under the
    /// outgoing fade instead of popping to full. `None` in steady state — the
    /// no-switch path applies NO scale at all (byte-identity preserved).
    ramp_in_at: Option<Instant>,
    /// The full config of the LAST tick — the snapshot an [`OutgoingFade`] is
    /// forged from when the NEXT tick detects a switch (`last_style` alone
    /// cannot reproduce the outgoing colours/length/pack for the residue).
    last_cfg: Option<GlowConfig>,
    /// LATCH: the `!cfg.enabled` teardown has already run and nothing has
    /// dirtied wiped state since (driver-02). While set, the dark tick skips
    /// the ~60-80 idempotent stores of [`Self::clear_transient_state`] +
    /// [`Self::clear_thermals`] it used to re-issue every frame — and a
    /// wake-off session pays that dark tick on every presented frame, because
    /// the pipeline's gate is `enabled_any()`, never per-engine. Dropped by
    /// every enabled tick, by [`Self::reset`], and via [`Self::unsettle`] at
    /// each mutating entry that writes wiped state. Entries that only move
    /// state TOWARD the wiped fixpoint (`clear_blink`, `clear_typed`,
    /// `drop_row_probe`, the drains/takes/swaps) keep it, as do the two that
    /// are structurally inert while dark: `cue_keystroke` no-ops while
    /// `sound_live` is false — this latch is set by the master-off /
    /// degenerate-grid branch and by no other, and ONLY when that branch
    /// leaves the seam shut (a degenerate grid the key belongs to stays open
    /// and unlatched), so the argument rests on neither the zero-amplitude
    /// return nor the classic wake, which leave the seam open for a heard dark
    /// frame — and `observe_neighbor_rows`
    /// no-ops until an `observe_row` — which unsettles — lands first.
    /// `#[derive(Default)]` starts it false, so the first dark tick still
    /// runs one (no-op) wipe before latching.
    dark_settled: bool,
    /// **THE RAINBOW KITTY ENGINE** — `RAINBOW-KITTY-V2.md` §17.2's seam
    /// object, the thing every `if self.v2.engaged()` in this file delegates
    /// to (D14). Default: disengaged, nothing live. Engaged at the top of
    /// [`Self::tick`] whenever the resolved style is
    /// [`GlowStyle::RainbowKitty`] (unconditional since §17.3 phase 7 —
    /// there is no other rainbow kitty); disengaged, every seam point is
    /// one cached-bool branch and the other nine styles run their own code
    /// byte for byte.
    v2: rk::Engine,
    /// v2's polyline scratch ([`rk::Frame::beams`]): resident and reused, so
    /// the engine allocates nothing per frame (§18).
    v2_beams: Vec<BeamVertex>,
    /// The `&[bool]` view of the host's row probe for v2's glyph gate (§5.4,
    /// seam point 13): resident, cleared and refilled from the SAME `cols`
    /// the v1 probe copies — never a second grid scan.
    v2_probe_scratch: Vec<rk::stardust::CellInk>,
    /// **THE CONTENT WITNESS'S ROWS** for this frame
    /// ([`Self::observe_ribbon_row`], [`Self::capture_ribbon_row`],
    /// `rk::witness`): the live grid rows the
    /// resident ribbon occupies, captured by the host under its terminal
    /// lock before the tick and read by the engine's witness right after
    /// it. At most [`CURSOR_WITNESS_ROWS`] slots, resident and reused;
    /// `witness_rows_n` is how many are filled THIS frame, taken to zero by
    /// every tick so a stale sample can never be read against a later
    /// frame's cells.
    witness_rows: Vec<WitnessRowBuf>,
    witness_rows_n: usize,
    /// **THE ROWS THE HOST WAS ASKED FOR** this frame — exactly what
    /// [`Self::ribbon_rows`] last handed out, and how many — so the follow
    /// pass counts as WITHHELD only a row the host was asked for and did
    /// not deliver ([`rk::Engine::follow_rows`]). Recomputing the list at
    /// the tick would ask a different question: by then the waiting key
    /// whose arming rows the list named ([`rk::Engine::ribbon_rows_for`])
    /// may be spent, and a row asked for and not delivered would be
    /// forgotten. Written by a `&self` query, hence the `Cell`; taken by
    /// every tick, so it never outlives its frame.
    witness_asked: std::cell::Cell<([u16; RIBBON_LIST_ROWS], usize)>,
    /// The caret seam v2 handed back on the last engaged tick (§7.1); the
    /// host reads it through [`Self::caret_flare_at`] / [`Self::caret_paint`]
    /// when it builds the caret's `RainbowConfig`.
    v2_caret: CaretSeam,
    /// **THE CARET'S SEATED STOP** (§7.1, D4) — the field `t` the caret block
    /// mixes toward, and the caret cell it was seated at. Seated on an engaged
    /// tick ONLY when the caret CELL changed since the last seat, or when the
    /// caret's own cell owns field light; HELD on every other tick, so a cell
    /// death under a parked caret cannot move its colour — the measured
    /// landing pop [`Self::rainbow_head_rgb`] documents. `None` until the
    /// first engaged tick, and cleared whenever v2 lets go of the frame.
    v2_stop: Option<(f32, Option<(u16, u16)>)>,
    /// **THE FLOW ENTRY EDGE** — whether the last [`Self::take_flow_entry`]
    /// drain found the theme OPEN ([`rk::Flow::open`]).
    ///
    /// One `bool` of memo, and the seam's whole state: the RUN itself is the
    /// engine's ([`rk::spine::Spine::flow`] — one counter, per the flow law),
    /// and this only remembers which side of the open bar the last drain saw
    /// it on, so the pet can be told ONCE per entry and nothing at all on an
    /// exit.
    flow_open: bool,
    /// v2's cue sink ([`rk::Frame::cues`]): resident, drained into
    /// [`Self::sound_cues`] under v1's own `MAX_SOUND_CUES` refusal after every
    /// engaged tick, so the backlog a non-draining host accumulates is bounded
    /// exactly as v1 bounds it and the steady frame allocates nothing (§18).
    v2_cues: Vec<SoundCue>,
}

/// Window-space geometry the host passes in (pixels). Effect-stream pixel
/// emissions (fire patches, glow/halo/nova quads, beams) are WINDOW-ABSOLUTE:
/// producers add `origin` once and the renderer adds nothing. `origin` is the
/// grid-interior top-left in window px (`pad`, `pad + head + strip_px`); `win`
/// is the full frame extent, so clamps relax to the window edge and effects
/// may draw above the grid (the titlebar chrome band). With `origin == (0,0)`
/// and `win` equal to the grid extents, every emission is byte-identical to
/// the old pad-relative contract (the identity law tests rely on this).
#[derive(Clone, Copy)]
pub struct Geom {
    pub cw: usize,
    pub ch: usize,
    pub rows: usize,
    pub cols: usize,
    /// Grid-interior top-left X in window px (the host's `pad`).
    pub origin_x: u16,
    /// Grid-interior top-left Y in window px (`pad + head + strip_px`).
    pub origin_y: u16,
    /// Full frame width in px (effects may reach the window edges).
    pub win_w: u16,
    /// Full frame height in px.
    pub win_h: u16,
    /// RISE ALLOWANCE above the grid in px — the chrome band the host opened for
    /// effects (the macOS titlebar under `fullSizeContentView`; `ATERM_HEADROOM_PX`
    /// headless). Distinct from `origin_y`, which also folds in `pad` and the tab
    /// strip: the fire-root/rise clamps relax by exactly THIS component, so with
    /// `head == 0` every clamp reproduces the historical grid-relative behavior
    /// byte-for-byte (the identity law) — pad and strip never granted rise room
    /// before the effects layer and must not silently start to.
    pub head: u16,
}

impl Geom {
    /// The EFFECTS BOX — where effect pixels may land, in window px. The grid box
    /// plus the `head` band above it: `x ∈ [fx_left, fx_right)`,
    /// `y ∈ [fx_top, fx_bot)`. Every pixel-emission clamp uses THESE bounds, not
    /// the raw window — see the identity law on [`Geom::head`].
    #[inline]
    pub(crate) fn fx_left(&self) -> i32 {
        i32::from(self.origin_x)
    }
    #[inline]
    pub(crate) fn fx_right(&self) -> i32 {
        i32::from(self.origin_x) + (self.cols * self.cw) as i32
    }
    #[inline]
    pub(crate) fn fx_top(&self) -> i32 {
        i32::from(self.origin_y) - i32::from(self.head)
    }
    #[inline]
    pub(crate) fn fx_bot(&self) -> i32 {
        i32::from(self.origin_y) + (self.rows * self.ch) as i32
    }

    /// Window-px CENTRE of a grid cell — `origin + (index + 0.5) · cell` on
    /// each axis, the one cell→pixel anchor every emitter shares.
    #[inline]
    pub(crate) fn cell_center(&self, row: u16, col: u16) -> (f32, f32) {
        (
            self.origin_x as f32 + (col as f32 + 0.5) * self.cw as f32,
            self.origin_y as f32 + (row as f32 + 0.5) * self.ch as f32,
        )
    }

    /// The beam rasterizers' clip for this geometry — the effects box + grid anchor.
    #[inline]
    pub(crate) fn beam_clip(&self) -> BeamClip {
        BeamClip {
            x0: self.fx_left(),
            y0: self.fx_top(),
            x1: self.fx_right(),
            y1: self.fx_bot(),
            cell_h: self.ch as i32,
            origin_y: i32::from(self.origin_y),
        }
    }
}

/// One observed cursor move as [`CursorGlow::spawn`]'s classifier read it: the
/// raw endpoints / clock / config / geometry plus the typing-vs-jump
/// classification every later spawn phase keys on. Built once per move by
/// `classify_move` (which also owns the hint consumption), so the phases read
/// pure data.
struct MoveCtx<'a> {
    pr: u16,
    pc: u16,
    cr: u16,
    cc: u16,
    now: Instant,
    cfg: &'a GlowConfig,
    geom: Geom,
    /// |Δrow|, |Δcol| and Chebyshev distance of the RAW observed move.
    dr_abs: i32,
    dc_abs: i32,
    /// Focused pane's horizontal bounds in this move's window-coordinate
    /// space. They equal `(0, geom.cols)` for a single pane.
    pane_col0: usize,
    pane_cols: usize,
    /// The classifier verdicts — see the classifier comments in `classify_move`.
    /// (`wrap` — `shape_wrap || re_anchor` — is a classifier-local now: its
    /// last downstream reader was the momentum advance, which moved into
    /// `classify_move` with the press-hint restoration; `dist`/`typing` below
    /// already carry its collapse for every other phase.)
    shape_wrap: bool,
    /// The cells a `shape_wrap` fold actually PAID for (the fold's cells —
    /// the origin row's tail from `pc` to the pane's edge
    /// plus the landing row's head before `cc` — bounded by the credits in
    /// the ring at the spend) — what the seam's fold sweep lays, ending at
    /// the landing and folded onto the origin row exactly as the key's
    /// `Typed` replay would; `0` off the fold or with nothing paid.
    fold_laid: usize,
    echo_run: bool,
    re_anchor: bool,
    rainbow_coalesce: bool,
    /// The coalesce shape matched in every respect EXCEPT the press CREDIT
    /// budget — the anti-stray law that keeps one credit from painting a
    /// ribbon over a word the user only skimmed. The ring's `no-credits`
    /// reason, and a forget edge (the pool did not describe the hop); no
    /// emitter reads it.
    credit_starved: bool,
    /// A typed-paired same-row forward hop WIDER than the sweep cap
    /// (`RAINBOW_TYPED_SWEEP_MAX`): a re-anchor that lays only its landing,
    /// and a forget edge (the pool cannot describe a hop the cap refuses).
    typed_over_cap: bool,
    /// **THE TYPED RE-ANCHOR LAYS ITS LANDING** — the classifier's own
    /// predicate for the spend ("lays exactly ONE cell, the landing"),
    /// carried to the seam so the sweep that lays it is the same verdict
    /// the credit was spent on. `false` for every non-re-anchor and for the
    /// styles that have no ribbon.
    re_anchor_lays_landing: bool,
    /// The VISIBLE lane's typed re-anchor would have laid its one cell under
    /// a glyph NO live press typed ([`CursorGlow::probe_glyph_before`] — for
    /// a flushed park, the glyph it was held over, [`HeldPark::landing_cell`]
    /// — against [`PressCredits::foreign_to_live`]): the classifier forgot
    /// the presses instead of spending one, and [`CursorGlow::spawn_judged`]
    /// declines the move `program-row` before v2 sees it. The cell is the
    /// landing, or a `soft_wrap`'s origin; a `lift` lays none and is never
    /// refused here. Never set with `soft_wrap`, `lift` or
    /// `re_anchor_lays_landing`.
    landing_foreign: bool,
    /// **THE SOFT-WRAPPED CARET** ([`CursorGlow::soft_wrapped_caret`]): the
    /// typed fold or re-anchor is a key whose glyph stands at the ORIGIN
    /// `(pr, pc)` while the caret alone wrapped to the continuation row's
    /// indent. `Some(word_col0)` names the first column of the word the key
    /// ends on the origin row. The move then lays and spends exactly one
    /// cell, the key's own at the origin, and never a landing-row cell.
    soft_wrap: Option<u16>,
    /// **THE LIFTED WORD** ([`CursorGlow::lifted_word`]): the typed
    /// same-row retreat is the composer moving the word `cc..pc` UP onto
    /// the end of the row above; `Some(col)` names where it starts there.
    /// Its landing is the word's old first column, not a glyph the key
    /// wrote, so the re-anchor's landing sweep stands down.
    lift: Option<u16>,
    /// The move was licensed by the IN-FLIGHT POOL alone: no stamp was
    /// fresh, and the unpaid presses paid for the echo (a batch, or one
    /// press for its own +1). The ring row reads `licence=inflight`.
    in_flight_licence: bool,
    /// `shape_wrap || re_anchor` — the classifier's one wrap verdict, kept on
    /// the ctx because the momentum advance and the fold-shaped cat pulse
    /// both key on it.
    wrap: bool,
    /// The press-hint half of typed pairing: a fresh key-time `typed_at`
    /// stamp. Identical to `typed_pair` under the license law; kept as its own
    /// field because the momentum spine and the classifier read it for
    /// different reasons.
    typed_hinted: bool,
    /// The fresh typed stamp the classifier consumed — the KEY's clock, which
    /// the seam hands v2's sweep as the birth of a late-echoed glyph cell so
    /// the frame that echoes the key already shows it lit.
    typed_at: Option<Instant>,
    /// The observed move paired with a fresh Backspace quench hint — a
    /// backspace landing wearing a wrap shape; excluded from the advance.
    bs_pair: bool,
    /// `raw_dist` with wrap / re-anchor / coalesced-echo moves collapsed to 1.
    dist: f32,
    typing: bool,
    fire: bool,
    forward: bool,
    deletion: bool,
    navigation: bool,
    /// The instant of the nav one-shot `navigation` consumed — the forget
    /// edge spares the presses typed after it.
    nav_taken: Option<Instant>,
}

/// One step of the frame-fingerprint FOLD. FNV-1a is deliberately cheap here:
/// the input is a fixed-shape render stream, and every record is first
/// losslessly packed into non-overlapping words by [`FrameFingerprint`].
#[inline]
fn fp_fold(fp: u64, key: u64) -> u64 {
    (fp ^ key).wrapping_mul(0x0000_0100_0000_01b3)
}

/// Build a deterministic, field-complete fingerprint of the cursor-effect
/// payload handed to the renderer.
///
/// The packing intentionally matches `aterm-core`'s `EffectStreamDamage`
/// serializers for the three shared render records. Stream tags and lengths
/// keep equal words in different planes (or different record boundaries) from
/// sharing the same fold position. This remains a 64-bit fingerprint, not a
/// collision-free proof of payload equality. `None` preserves the public
/// `0 == idle` contract without hashing empty streams on every resting frame.
#[derive(Default)]
struct FrameFingerprint(Option<u64>);

impl FrameFingerprint {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const GLOW_OUT: u64 = 0x676c_6f77_5f6f_7574;
    const FIRE_PATCH: u64 = 0x6669_7265_7061_7463;
    const GLOW_UNDER: u64 = 0x676c_6f77_756e_6472;
    const CHAR_FG: u64 = 0x6368_6172_5f66_6700;
    const FIRE_HALO: u64 = 0x6669_7265_6861_6c6f;
    const RAIN_HALO: u64 = 0x7261_696e_6861_6c6f;
    const FORGE_FILL: u64 = 0x666f_7267_655f_6669;

    #[inline]
    fn write(&mut self, word: u64) {
        self.0 = Some(fp_fold(self.0.unwrap_or(Self::OFFSET), word));
    }

    #[inline]
    fn begin_stream(&mut self, tag: u64, len: usize) {
        self.write(tag);
        self.write(u64::try_from(len).unwrap_or(u64::MAX));
    }

    fn glow_quads(&mut self, tag: u64, quads: &[GlowQuad]) {
        if quads.is_empty() {
            return;
        }
        self.begin_stream(tag, quads.len());
        for q in quads {
            for word in q.damage_words() {
                self.write(word);
            }
        }
    }

    fn fire_patches(&mut self, patches: &[FirePatch]) {
        if patches.is_empty() {
            return;
        }
        self.begin_stream(Self::FIRE_PATCH, patches.len());
        for p in patches {
            for word in p.damage_words() {
                self.write(word);
            }
        }
    }

    fn chars(&mut self, chars: &[CharFg]) {
        if chars.is_empty() {
            return;
        }
        self.begin_stream(Self::CHAR_FG, chars.len());
        for c in chars {
            self.write(u64::from(c.row) | (u64::from(c.col) << 16) | (u64::from(c.fg) << 32));
        }
    }

    fn fire_halos(&mut self, cells: &[FireHaloCell]) {
        if cells.is_empty() {
            return;
        }
        self.begin_stream(Self::FIRE_HALO, cells.len());
        for c in cells {
            self.write(u64::from(c.row) | (u64::from(c.col) << 16) | (u64::from(c.strength) << 32));
        }
    }

    fn rain_halos(&mut self, halos: &[RainHalo]) {
        if halos.is_empty() {
            return;
        }
        self.begin_stream(Self::RAIN_HALO, halos.len());
        for h in halos {
            for word in h.damage_words() {
                self.write(word);
            }
        }
    }

    fn forge_fill(&mut self, fill: u32) {
        self.begin_stream(Self::FORGE_FILL, 1);
        self.write(u64::from(fill));
    }

    fn finish(self) -> u64 {
        match self.0 {
            None => 0,
            Some(0) => 1,
            Some(fp) => fp,
        }
    }
}

impl CursorGlow {
    /// Hard cap on emitted quads (defends the renderer + the per-frame upload); the
    /// snapshot is truncated so BOTH backends see identical geometry. Sized for the
    /// LASER's six fine-stride bloom layers over a FULL-length (uncapped) jump beam
    /// plus its lightning bolts — instances are tiny (~12 B), so even a saturated
    /// snapshot uploads ~200 KB, still trivial per frame.
    const MAX_QUADS: usize = 16384;
    /// Hard cap on live swept-path samples for every style. `cfg.length` bounds
    /// one move, but without a resident cap a flood of moves at the same instant
    /// could accumulate samples faster than lifetime pruning removed them.
    const MAX_SPARKS: usize = 512;
    /// Water's per-frame newest-owner table: power-of-two open addressing at a
    /// maximum 50% load, so duplicate suppression stays bounded and cheap even
    /// for hostile 65,535×65,535 logical geometries.
    const WATER_OWNER_SLOTS: usize = Self::MAX_SPARKS * 2;
    /// Hard cap on analytic particles. Water can emit seven droplets per hot
    /// keystroke and jump splashes can emit 26 at once, so the per-move burst is
    /// not itself a bound on resident work.
    const MAX_PARTICLES: usize = 512;
    /// Longest same-row typed-coalesce the ribbon sweeps as CONTINUED TYPING
    /// (cells): a repaint re-anchor (Claude Code's inset box growing) hops the
    /// caret across cells it never visited — beyond this cap the move keeps the
    /// single landing spark.
    ///
    /// It is [`TYPED_STAMP_DEPTH`] (see that constant for the arithmetic),
    /// and the two must stay equal: one observed move can spend at most
    /// this many cells, so banking more presses than that cannot license
    /// anything a move could ever sweep. What separates a real batched echo
    /// from a re-anchor at ANY length is the press ledger
    /// ([`PressCredits`]), not the length itself.
    const RAINBOW_TYPED_SWEEP_MAX: usize = TYPED_STAMP_DEPTH;
    /// The anchored lane's bound (cells) for a row whose identity no licensed
    /// anchored echo has proven yet (`echo_anchor_pass`): a program row's
    /// brandless first advance of 33..128 cells beside a fresh stamp returns
    /// silently instead of reaching `spawn`. The established echo row and a
    /// delivered insert's row take the full cap.
    const ANCHOR_UNPROVEN_ROW_MAX: usize = 32;

    // ── THE ZOOM STREAK'S SHAPE (see `emit_rainbow_jumps`) ─────────────────
    //
    // Four numbers, and between them they replace the three-row `SEG` table
    // that drew the streak as three chained flat bars. Every one of them is an
    // ENDPOINT of a continuous ramp rather than a value some stretch of the
    // mark is held at, which is the whole difference: the old table's rows met
    // at 0.34 vs 0.608 coverage and 0.45 vs 0.75 thickness, and those two
    // mismatches were the seams the owner read as "a not smooth gradient".

    /// Most simultaneous live lightning bolts, main channels + branch forks
    /// (oldest evicted first). A jump strike spawns 1 main + up to 4 branches,
    /// so this comfortably holds several overlapping strikes plus crackle.
    const MAX_BOLTS: usize = 24;
    /// Bloom-crown fade window (the crown follows the head for this long post-move).
    const CROWN_MS: u64 = 200;
    /// TYPING crown window: a single-cell advance keeps the crown lit this long, so
    /// at human typing cadence (≤~350ms between keys) the crown NEVER lapses
    /// mid-sentence — the glow stream stays non-empty across the inter-key gap
    /// (which also keeps the renderer's SDR attack envelope from resetting and
    /// re-blooming on every key — the "laggy pulse" report). Jumps keep [`CROWN_MS`].
    const CROWN_TYPING_MS: u64 = 350;
    /// Heat earned by one keystroke at full cadence (≈7 fast keys to full heat).
    const HEAT_GAIN: f32 = 0.16;
    /// Exponential heat cool-down time constant (seconds to ~37%).
    const HEAT_DECAY_TAU: f32 = 0.9;
    /// Jump-flare cool-down time constant: fast enough that the white-hot burst
    /// reads as an EVENT (blaze → orange → ember inside the comet's own fade),
    /// slow enough that the cooling arc is visible rather than a single flash.
    const FLARE_DECAY_TAU: f32 = 0.45;
    /// Inter-key gap (seconds) at or under which a keystroke earns full heat…
    const HEAT_GAP_FULL: f32 = 0.09;
    /// …and the gap beyond which it earns none (relaxed typing stays cool).
    const HEAT_GAP_ZERO: f32 = 0.40;
    /// TYPING CONTINUITY: an inter-key gap at or under this (== [`Self::HEAT_GAP_ZERO`]
    /// — beyond the heat window it isn't "typing") lets the new spark's life CHAIN to
    /// the observed cadence, so the streak is continuous at any human typing rhythm
    /// instead of pulsing per key with dark rests (the quiet-shell "gapping" report).
    const CHAIN_GAP_MAX: f32 = 0.40;
    /// Chained life = observed gap + this margin (the previous spark comfortably
    /// outlives the gap)…
    const CHAIN_MARGIN: f32 = 0.10;
    /// …capped here so the post-stop tail stays crisp (≤500ms after the last key).
    const CHAIN_LIFE_MAX: f32 = 0.50;
    // ---- EMBERFORGE thermal model (fire only) --------------------------------
    /// Fire's fast-heat gain per forward keystroke at full cadence — much lower
    /// than the shared [`Self::HEAT_GAIN`]: full blaze is EARNED over ~30-40
    /// sustained keys (fast heat + the coal floor together), not seven.
    const FIRE_HEAT_GAIN: f32 = 0.045;
    /// Fire's heat cool-down τ — slower than the shared 0.9 s: momentum
    /// survives a short thought instead of resetting between phrases.
    const FIRE_HEAT_TAU: f32 = 2.8;
    /// Coal-bed cool-down τ (seconds to ~37%) — the slow persistence floor.
    const COAL_TAU: f32 = 6.0;
    /// Coal charged by one FORWARD keystroke at full cadence credit: with the
    /// τ above, ~60+ sustained keys reach a strong bed — the LONG momentum arc.
    const COAL_GAIN: f32 = 0.022;
    /// Inter-key gap earning full coal credit — deliberately wider than the
    /// fast heat's window: HUMAN-cadence typing must charge the bed…
    const COAL_GAP_FULL: f32 = 0.35;
    /// …fading to zero credit here (a key every 1.5 s is browsing, not writing).
    const COAL_GAP_ZERO: f32 = 1.50;
    /// The coal bed floors the display temperature at this fraction of itself.
    const COAL_FLOOR: f32 = 0.75;
    /// Quench added per backspace (≈4 fast deletes to a full douse)…
    const QUENCH_GAIN: f32 = 0.28;
    /// …decaying on this τ once the deleting stops.
    const QUENCH_TAU: f32 = 1.2;
    /// Each delete also cools the standing heat AND the coal bed by this factor.
    const QUENCH_COOL: f32 = 0.8;
    /// A full quench suppresses this fraction of the display temperature.
    const QUENCH_DAMP: f32 = 0.6;
    /// A backspace classifier stays fresh this long after its exact candidate.
    const QUENCH_HINT_FRESH: f32 = 0.25;
    /// A navigation classifier stays fresh this long; it never admits a move.
    const NAV_HINT_FRESH: f32 = 0.25;
    /// A typed-glyph classifier and its exact candidate stay fresh this long.
    /// Echo latency is tens of milliseconds; a quarter second covers a loaded
    /// PTY without correlating the candidate to a later, unrelated move.
    pub(crate) const TYPE_HINT_FRESH: f32 = 0.25;
    /// Freshness window for the RETURN classifier ([`Self::return_hint`]).
    const RETURN_HINT_FRESH: f32 = 0.25;
    /// Freshness window for the Tab/paste gesture stamp
    /// ([`Self::user_gesture_hint`]). The stamp predates the LICENSE and was
    /// only ever consumed, never dated; the license needs a window, and this
    /// is the 0.25 s class every other press-hint already lives in.
    const USER_GESTURE_HINT_FRESH: f32 = 0.25;
    /// Freshness window for the DELIVERED-INSERT licence
    /// ([`InsertSeam`]): the insert IS an unpaid credit of `cells`
    /// cells — a debounced TUI (Claude Code's Ink prompt under load) echoes
    /// up to ~1.4 s after the bytes land, measured, and a 0.25 s window
    /// would refuse most of them. Two seconds is the insert's OWN measured
    /// bound, not the typed press's in-flight patience
    /// (`IN_FLIGHT_PATIENCE_S`, 10 s, from a typed-stall measurement the
    /// paste path has not had — stall.py has no paste run). A paste
    /// delivered into an app stall longer than this is a recorded residual:
    /// when it is measured, this moves to the in-flight patience — the
    /// shape bounds hold at any window. It is safe at this length only
    /// because the SHAPE bounds what
    /// it can buy: one same-row forward echo, no wider than the insert plus
    /// the unpaid typed credits behind it, on a row that is not a program
    /// row, once.
    pub const INSERT_HINT_FRESH: f32 = 2.0;
    /// The same window for the UNKNOWN-WIDTH class — a bare Tab or ⌃V,
    /// whose echo is the local program's and whose bytes were one keystroke
    /// already on the wire.
    ///
    /// **WHY IT IS NOT [`Self::INSERT_HINT_FRESH`]** (2026-09-21). Two
    /// seconds is a PASTE's number: the host's write of a large body can
    /// land slowly, and the width check bounds what the licence can buy
    /// anyway. The unknown class has no width to check — its admission is
    /// the row witness and the 32-cell bound — so for the whole two seconds
    /// the FIRST same-row forward advance on the hand's row was taken as
    /// the gesture's echo, and 1.75 s of that is past every other licence's
    /// freshness. A background job printing on the prompt row was lit as a
    /// 32-cell sweep: ink over bytes no key asked for.
    ///
    /// **THE NUMBER, AND WHAT SET IT.** A local bash completion's echo was
    /// measured on glass at `<= 115 ms` — the recorder's own sampling
    /// floor, which is to say the instrument could not resolve it
    /// (`index.json`'s `analysis` reads `AT CAPTURE FLOOR`), so that is a
    /// bound and not a reading. The FLOOR on this constant is not that
    /// measurement but this crate's own standing law
    /// (`a_spinner_row_advancing_after_a_tab_stays_dark`), which requires a
    /// Tab's completion to still be the Tab's **800 ms** later; 0.75 broke
    /// it, and weakening a shipped law to fit a guessed constant is not a
    /// trade this lane gets to make. One second keeps every existing law
    /// with 200 ms to spare and halves the window a program hop can walk
    /// into.
    ///
    /// It is a POLICY, not a bound — a completion that shells out over a
    /// slow network can take longer, and past this its echo goes unlit
    /// rather than the window staying open for program output. That is the
    /// trade the owner's report picks: an unexpected rainbow is the defect;
    /// a completion that paints no trail is not.
    ///
    /// **WHAT WAS TRIED INSTEAD AND DOES NOT WORK:** a COLUMN witness
    /// beside the row one, refusing a hop that does not BEGIN where the
    /// hand stood when the gesture was armed. It cannot bind. The caret can
    /// only reach another column by a move, and the first same-row forward
    /// move after the Tab therefore always starts at the hand's own column
    /// — it is admitted, and the licence is spent, before any later hop can
    /// be tested. Measured: both arms of the twin scored 1. Time is the
    /// only discriminator this seam has.
    pub const INSERT_GESTURE_HINT_FRESH: f32 = 1.0;
    /// How long after a delivered insert was laid a keyless same-row retreat
    /// landing inside its span is read as the program's REWRITE of that
    /// insert (Claude Code swaps a dropped path for `[Image #1] ` 300-400 ms
    /// after echoing it, measured). Past this the retreat is program output
    /// like any other and retracts nothing.
    pub const INSERT_REWRITE_FRESH: f32 = 2.0;
    /// How long after a refused same-row forward hop a delivery receipt may
    /// still claim it ([`PendingHop`]) — the writer thread publishes within
    /// microseconds of the write returning, and the frame that observed the
    /// echo is at most one present interval behind it; anything older is
    /// program output that happened to precede a paste.
    pub const INSERT_RETRO_FRESH: f32 = 0.25;
    /// The unknown-width bound for an insert the host cannot price: a Tab
    /// completion (one `\t` byte, the shell decides the width) and a raw ⌃V
    /// (the app's own placeholder). A same-row forward echo of at most this
    /// many cells may spend it. It is the widest PLACEHOLDER Claude Code
    /// prints (`[Pasted text #1 +N lines] ` ≈ 28 cells) plus a Tab
    /// completion — 32, decoupled from the press bank's depth so the bank
    /// can grow for a stall without widening the unknown class's one-shot
    /// exposure. Counted ONCE per licence, not per arm: a second unknown
    /// arm accumulating into an unspent one — a Tab auto-repeat — leaves
    /// the bound where it is.
    pub const INSERT_GESTURE_CELLS: u16 = 32;
    /// Freshness window for the REFLOW classifier ([`Self::reflow_hint`]). It is
    /// retained for bounded lifecycle observation, not movement admission.
    const REFLOW_HINT_FRESH: f32 = 0.40;
    /// A kill key-hint stays armed this long for its row-shrink echo (seconds)
    /// — see [`Self::note_kill`]. Wider than the typed hint: a kill's erase is
    /// often a full-box TUI repaint (Ink rewrites every prompt row), which can
    /// land a frame or two later than a single-glyph echo.
    const KILL_HINT_FRESH: f32 = 0.35;
    /// A repaint-blink hint ([`Self::note_repaint_blink`]) stays fresh this
    /// long (seconds). Wider than the per-keystroke hints: the blink is noted
    /// at the PREVIOUS burst's present, so it must comfortably outlive one
    /// human inter-key gap for the NEXT keystroke's re-anchor — while staying
    /// far under the seconds-scale pause that separates "the TUI is repainting
    /// per keystroke" from "the app stopped doing that".
    const BLINK_HINT_FRESH: f32 = 0.5;
    /// Coarse poll cadence while ONLY a pending kill hint keeps the engine
    /// live (see [`Self::next_change_deadline`]): the caret fallback needs a
    /// handful of ticked frames after [`Self::POOF_FALLBACK_GRACE`] (0.06 s),
    /// not a 60 fps re-tick for the whole 0.35 s hint window.
    const KILL_HINT_POLL_INTERVAL: Duration = Duration::from_millis(40);
    /// Minimum gap between erase poofs (seconds): a held kill-key repeat
    /// billows at a readable ~7 Hz instead of stacking a puff per frame.
    /// The caret-anchored fallback waits this long after the kill keypress so
    /// the PRECISE span branch always gets first shot at the echo (a shell's
    /// EL lands within a frame or two; Claude Code's reflow never span-matches
    /// at any delay, so the fallback costs it ~4 imperceptible frames).
    ///
    /// It is a CAP, not a floor: [`Self::poof_erase_witnessed`] releases it the
    /// moment the probe proves the erase is already on glass, because at that
    /// point the span branch has had its shot on that very probe and declined.
    /// The timer is what governs the cases with no witness — a reflow that
    /// re-rowed the caret, a kill whose echo never shows, a host that had no
    /// probe to stamp at the key.
    const POOF_FALLBACK_GRACE: f32 = 0.06;
    const POOF_MIN_GAP: f32 = 0.14;
    /// A stored row probe older than this cannot witness a kill (seconds).
    /// Belt-and-braces: content cannot change without damage → present → tick,
    /// so the previous probe is normally the last-presented truth — this cap
    /// only fences a pathological host that stops probing mid-session.
    const POOF_PROBE_STALE: f32 = 5.0;
    /// Quench added by one KILL key ([`Self::note_kill`]) — a kill is a BIG
    /// douse (a whole span of text gone at once), roughly two backspaces'
    /// worth, so ~2 kills fully quench a standing blaze.
    const KILL_QUENCH_GAIN: f32 = 0.55;
    /// Display-temperature ATTACK ease (seconds to ~63% of a rise): nothing
    /// pops in — every brightening is a swell, and the blaze is EARNED across a
    /// real run (roughly a second of sustained key-repeat to approach the wall)…
    const DISP_ATTACK_S: f32 = 0.34;
    /// …except a SLAM (a jump flare opening a gap > 0.5): a fast whoosh so the
    /// eruption still lands within a couple of frames.
    const DISP_SLAM_S: f32 = 0.05;
    /// Display-temperature RELEASE τ — the fire dies down slower than it leaps.
    const DISP_RELEASE_TAU: f32 = 0.45;
    /// Bounded ring for `flame_phase` (the fire's churn clock), exactly like
    /// `rainbow.phase`'s 1024 wrap: without it the phase integrates every dt forever,
    /// and f32 ULP overtakes the ~0.03/frame increment near ~5e5 (quantize →
    /// freeze over a multi-day session), while the `* 1024.0 as u32` field cast
    /// saturates at u32::MAX past ~4.19e6. `TAU · 640` keeps BOTH forge sines
    /// (`× 1.7`, `× 2.3`) seamless across the wrap — `640 · 1.7 = 1088` and
    /// `640 · 2.3 = 1472` are whole turns — and the field's own u32 ring reseats
    /// one frame only after ~40 min of CONTINUOUS active fire (never mid-burst).
    const FLAME_PHASE_RING: f32 = std::f32::consts::TAU * 640.0;
    /// Cursor-metal attack τ (heats over ~1.5 s of standing blaze)…
    const TEMP_ATTACK_TAU: f32 = 1.5;
    /// …and release τ (cools much slower — forged metal holds its heat; the
    /// quench meter divides this, so deleting cools the cursor visibly faster).
    /// ~1.4 s to 63% cooled, fully dull inside ~4.5 s.
    const TEMP_RELEASE_TAU: f32 = 1.4;
    /// Below this metal temperature the cursor fill is untouched (pure theme).
    const FORGE_MIN_TEMP: f32 = 0.12;
    /// Coarse wake cadence for the forge-ember-only cooling tail (~11 fps): the
    /// ember's u8-quantized colour changes imperceptibly between these, so the
    /// present gate dedups anyway — see [`Self::next_change_deadline`].
    const EMBER_POLL_INTERVAL: Duration = Duration::from_millis(90);
    /// Same-row fire moves at/past this many columns become a METEOR (any row
    /// change already does — matching the rainbow kitty's jump classification).
    const METEOR_MIN_COLS: i32 = 8;
    /// Most simultaneous live fire meteors (oldest evicted first).
    const METEOR_CAP: usize = 4;
    /// A fresh meteor whose ORIGIN equals a live meteor's LANDING within this
    /// window RETARGETS that meteor instead of spawning a second one — shell
    /// repaint choreography (CR → prompt reprint, observed as several hops
    /// over a frame or three; the reprint can lag ~100 ms on a long edit
    /// line) collapses into ONE clean streak instead of a multi-hop smear.
    /// Must stay under the strike fraction of the
    /// shortest meteor life so a retarget always wins the race with arrival.
    const METEOR_RETARGET_S: f32 = 0.12;
    /// The head strikes (arrival flare) at this fraction of the meteor's life
    /// — the tail then finishes retracting into the eruption.
    const METEOR_STRIKE_FRAC: f32 = 0.62;
    /// Hard cap on per-frame radial halos (embers dominate; well above any
    /// real frame — both backends see the identical truncated snapshot).
    const MAX_HALOS: usize = 512;
    /// Hard cap on live vapor puffs (steam + smoke).
    const MAX_VAPOR: usize = 128;
    /// The cursor metal SMOKES above this temperature (the brief's "heats up
    /// over use and starts smoking").
    const SMOKE_TEMP: f32 = 0.35;

    /// PHASER CONSTANT-DISTANCE STREAK: within a rhythm the fat band's life is
    /// this many observed inter-key gaps (+ margin), so it always spans the
    /// SAME distance at ANY cadence: THREE letters back solidly lit behind the
    /// cursor (the 55%-hold typing envelope keeps a cell bright through ~2.5
    /// gaps of age), the fourth ghosting out.
    const PHASER_CHAIN_KEYS: f32 = 4.5;
    /// The chain window — a REAL typing rhythm only, deliberately much
    /// tighter than the rainbow kitty's: a longer window lets a key typed after a thinking
    /// pause inherit a multi-second life and park the band over old text. Past
    /// this gap a keystroke is a new burst: base time, then a crisp fade.
    const PHASER_CHAIN_GAP_MAX: f32 = 0.75;
    /// …capped just above `PHASER_CHAIN_KEYS × PHASER_CHAIN_GAP_MAX` so the
    /// slowest chained rhythm still earns its full three-letter span.
    const PHASER_CHAIN_LIFE_MAX: f32 = 3.5;
    /// LIGHTNING TRAIL residual: the fraction of full beam power an aged trail
    /// cell holds after its discharge bleed — the dim static charge that
    /// lingers (and crackles) behind the cursor before the final fade.
    const LASER_RESIDUAL: f32 = 0.30;
    /// FIRE streak text-safety caps — the OCCUPIED-CELL coverage discipline the
    /// phaser shares (`cursor_phaser::WING_STACK_BUDGET`). The flame comet rides
    /// the just-typed glyph row, and its near-white head stacks with the crown +
    /// flame body + bloom, which saturates the 1-2 FRESHEST glyph cells to cream
    /// on dark AND light. [`Self::FIRE_STREAK_COV_CAP`] bounds the per-sample
    /// streak coverage over the OCCUPIED glyph cells so the letters keep contrast
    /// through the flame. The BRIDGE sample sits on the EMPTY cursor cell — no
    /// glyph to bury — so it keeps [`Self::FIRE_HEAD_COV_CAP`] full punch and the
    /// light still visibly leaves the cursor.
    const FIRE_STREAK_COV_CAP: f32 = 108.0;
    const FIRE_HEAD_COV_CAP: f32 = 168.0;
    /// The STRUCTURAL legibility ceiling for the Trail Pack (`Custom`) path — a
    /// NON-configurable per-sample coverage cap set to the proven streaming
    /// posture (the phaser/rainbow kitty text-safety ceiling). Applied inside the custom
    /// interpreter's sole emission funnels (the beam CometSample builder + the
    /// particle loop) AND to the custom birth coverage, so a pack cannot express
    /// or emit a value that would bury the glyphs beneath it — the "a pack cannot
    /// opt out of the occupied-cell coverage ceiling" law. Every built-in stays
    /// byte-identical because this only ever bounds `GlowStyle::Custom` samples.
    const CUSTOM_COV_CAP: f32 = 150.0;
    /// The STRUCTURAL life ceiling for the Trail Pack (`Custom`) wake — a
    /// NON-configurable cap on a custom spark's base heat life so no combination
    /// of `heat.life_base_mul`/`life_a`/`life_b` can park a wake past this bound
    /// (chaining may still reach `chain_life_max`, itself ≤4 s by the schema).
    /// Equal to the pack particle-life ceiling; Custom-only, so built-ins are
    /// byte-identical.
    const MAX_TRAIL_SPARK_LIFE: f32 = 2.0;
    /// How far back along the charged trail (live typing sparks, newest first)
    /// a crackle arc may root — the stray arcs dance over the whole lingering
    /// trail, not just off the write head.
    const CRACKLE_TRAIL_CELLS: usize = 12;
    /// BEAM typing continuity: the tube's spark life chains to this many
    /// inter-key gaps, so the rod STEADILY spans about the last three-and-a-half
    /// typed letters at ANY cadence (with `beam_power`'s 40% full-power hold,
    /// the newest letter or two ride at full brightness) — a solid rod of
    /// light, not a comet smear.
    const BEAM_CHAIN_KEYS: f32 = 3.5;
    /// The beam's chain window (generous, mirroring the phaser's: a steady
    /// tube must survive leisurely rhythms too)…
    const BEAM_CHAIN_GAP_MAX: f32 = 0.75;
    /// …and its chained-life ceiling — well under the phaser's chained
    /// ceiling, so a slow rhythm never parks light on the line for seconds.
    const BEAM_CHAIN_LIFE_MAX: f32 = 1.2;

    /// Last honest visible source owned by this engine. Native and pipeline
    /// hosts require the classic trail to report the same anchor before they
    /// arm any candidate; a reset/unseeded pair cannot reconstruct a path.
    #[must_use]
    pub fn cursor_anchor(&self) -> Option<(u16, u16)> {
        self.last
            .or_else(|| self.last_visible.map(|(cell, _)| cell))
    }

    /// REDUCED-MOTION arm for the FRESH-INK pop (see the `reduced_motion`
    /// field doc): when set, a pop is a brightness STEP-FADE only — no scale
    /// spring, no birth halo. State-only and idempotent; the host folds its
    /// motion policy here beside the tick (the sparkle/rain reduced twins'
    /// pattern — this engine cannot depend on the GUI's `motion` module).
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
        // SEAM POINT 2 (§17.2, §6.11): the posture change → v2, dated to the
        // last drawn tick (this setter carries no clock, and the engine also
        // reads the posture off its `Config` on every tick).
        if self.v2.engaged()
            && let Some(at) = self.heat_at
        {
            self.v2.on_event(rk::Event::ReducedMotion(reduced), at);
        }
    }

    /// **THE FOCUS SEAM** (2026-09-13, Rainbow Path v3 step 1, law G1):
    /// the window's OS focus changed — `App::on_focus`'s `WindowEvent::Focused`
    /// arm, forwarded on the event's own clock. Rainbow Kitty v2 embers its
    /// ribbon out over `ribbon::FOCUS_EMBER_S` (0.30 s) on `spend` and puts
    /// every star on the same ember (`rk::Event::Focus(false)`); a regain
    /// inside the ember re-arms the light from the level it had through the
    /// one sanctioned attack, `edge-in` (`Focus(true)`). The design of record
    /// (`RAINBOW-KITTY-V2.md` §8.2) has carried the ember since 2026-09-05
    /// and the ribbon has pinned it since, but no host ever sent the event —
    /// on glass a blur was a one-frame cut the moment the motion policy took
    /// the amplitude to zero (`Engine::set_engaged(false)` → `reset`), or no
    /// change at all while the typed wake kept the amplitude up.
    ///
    /// RAW focus, not the host's presentability fold: the fold ORs in the
    /// live typed wake so a control-socket agent's window keeps drawing, and
    /// under it a human's alt-tab away from a fresh ribbon would never ember
    /// (the wake outlives the swoosh). The wake's own promise is kept by the
    /// ribbon instead: a typed key laid while the ember burns clears it and
    /// re-lights from the level (`Ribbon::on_event`'s `Typed` arm), so an
    /// unfocused window that is being typed into still shows the light of
    /// the keys being typed, and only a window nobody is typing into goes
    /// dark on blur. The sound already gated on raw focus
    /// (`app_render::trail_sound_gain`); the light now does the same.
    ///
    /// Inert for every style but rainbow kitty (the v2 gate), and inert
    /// while v2 is not engaged: a window that was dark stays dark.
    pub fn note_focus(&mut self, focused: bool, now: Instant) {
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Focus(focused), now);
        }
    }

    /// HOST REPAINT-BLINK: the focused terminal's `repaint_blink_epoch`
    /// advanced — the attached app hid the cursor INSIDE a DEC-2026
    /// synchronized update, the per-keystroke full-redraw bracket (Claude
    /// Code). Arms a short classifier ([`Self::BLINK_HINT_FRESH`]) that selects
    /// RE-ANCHOR morphology for an independently admitted typed/delete candidate
    /// on the alt screen. The blink never admits movement and never gates bytes.
    pub fn note_repaint_blink(&mut self, now: Instant) {
        self.unsettle();
        self.blink_hint = Some(now);
    }

    /// Drop the blink hint — a tab/pane switch re-pointed this window at a
    /// DIFFERENT terminal, and a blink carried from the old one must not
    /// classify a candidate (or, host-side, the probe) against the new one.
    pub fn clear_blink(&mut self) {
        self.blink_hint = None;
    }

    /// HOST PER-FRAME CONTEXT: whether the probed terminal is on the ALT
    /// screen this frame (read under the host's frame hold beside the row
    /// probe).
    /// Only the re-anchor classifier consults it — `false` (main screen, or a
    /// host/test that never calls this) keeps the classifier byte-identical
    /// to the pre-context behavior.
    pub fn note_context(&mut self, alt: bool) {
        self.ctx_alt = alt;
    }

    /// Stamp the focused pane's exact columns in the coordinate space handed
    /// to [`Self::tick`]. This is morphology only: [`Self::move_licensed`]
    /// remains the sole authority to mint light.
    pub fn note_pane_rows(&mut self, first_row: u16, rows: usize) {
        let pane_rows = u16::try_from(rows)
            .ok()
            .filter(|rows| *rows > 0)
            .map(|rows| (first_row, rows));
        self.v2.set_pane_rows(pane_rows);
    }

    /// Stamp the focused pane's exact columns in the coordinate space handed
    /// to [`Self::tick`]. This is morphology only: [`Self::move_licensed`]
    /// remains the sole authority to mint light.
    pub fn note_pane_columns(&mut self, first_col: u16, cols: usize) {
        self.pane_columns = u16::try_from(cols)
            .ok()
            .filter(|cols| *cols > 0)
            .map(|cols| (first_col, cols));
        // …and v2 folds at the same edges (its ribbon wraps a typed fold
        // within the pane, never at the grid's margin).
        self.v2.set_pane(self.pane_columns);
    }

    /// Any light still alive → keep the animation timer armed. The cursor-
    /// metal arm is the one non-quad member: the forge fill is VISIBLE state
    /// (it rides `cursor_fill_override`), so the cursor must keep animating —
    /// and re-presenting — until it has visibly cooled back to the theme fill;
    /// it then snaps to exactly 0 and the timer disarms (idle stays zero-work).
    /// A PENDING kill hint also arms: a reflowing TUI (Claude Code) answers
    /// the kill via the caret fallback, which only evaluates on a ticked frame
    /// AFTER its grace — but the app's own redraw burst ends within ~50 ms and
    /// the screen goes idle; without this the fallback never runs and the poof
    /// is silently lost. Bounded by [`Self::KILL_HINT_FRESH`] (0.35 s) — the
    /// hint expires or is consumed, and the timer disarms with it. It paces
    /// COARSELY, not at frame cadence — see [`Self::next_change_deadline`].
    pub fn is_active(&self) -> bool {
        self.classic.is_active()
            || self.has_live_motion()
            || self.ember_live()
            || self.poof_hint_pending().is_some()
            // §17.2: `is_active` follows the v2 fingerprint while it owns
            // the frame — `0` exactly when nothing of v2's is on glass (T6).
            || (self.v2.engaged() && self.v2.fingerprint() != 0)
            // A held park is pending work: its verdict is due ≤ 0.25 s after
            // the park, and only a tick can deliver it ([`HeldPark`]).
            || (self.v2.engaged() && self.held_park.is_some())
    }

    /// THE FORGE EMBER IS LIVE while the metal is visibly hot OR while the
    /// display temperature it chases (`TEMP_ATTACK_TAU`, 1.5 s) is still
    /// above the visibility threshold — the heating arc is a pending visible
    /// change. Read by both wake predicates so they cannot drift: before it,
    /// a Fire strike whose moving light died first read `cursor_temp` below
    /// the threshold, told the host to sleep, and crossed the threshold
    /// upward ~60 ms later with nobody to draw it (the 2026-09-14 audit).
    /// Fire-gated: every style integrates `disp_t` (a non-Fire jump's flare
    /// tail lasts ~1 s) but only Fire has a metal to heat; `last_style` is
    /// the style of the tick that produced this state.
    fn ember_live(&self) -> bool {
        self.cursor_temp > Self::FORGE_MIN_TEMP
            || (matches!(self.last_style, Some(GlowStyle::Fire))
                && self.disp_t > Self::FORGE_MIN_TEMP)
    }

    /// THE PENDING POOF WAKE — the earlier of the kill license or plain-
    /// Backspace classifier that still needs detector service.
    ///
    /// WHY THIS EXISTS. Both paths need a detector tick, but they prove the
    /// erasure differently: a kill uses its legacy row-diff/fallback license;
    /// plain Backspace waits for an exact confirmed span. Naming only
    /// `kill_hint` at the scheduling seams would strand the latter proof on a
    /// quiet screen.
    ///
    /// One accessor, so a third licence cannot repeat the omission.
    fn poof_hint_pending(&self) -> Option<Instant> {
        match (self.kill_hint, self.bs_poof_hint) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// THE ERASE IS ALREADY ON GLASS: this frame's probe shows the caret's row
    /// SHORTER than the row stamped at the erase key ([`Self::bs_baseline`]).
    ///
    /// This is the kill fallback's EARLY RELEASE from
    /// [`Self::POOF_FALLBACK_GRACE`]; a plain Backspace shares it.
    ///
    /// `None` baseline (the host had no probe at the key) is deliberately NOT a
    /// witness in either direction: it is the "was not composing" case, so the
    /// fallback falls back to its timed grace.
    fn poof_erase_witnessed(&self) -> bool {
        match (self.bs_baseline, self.row_cur_meta) {
            (Some((brow, bfill)), Some(cur)) => {
                // A ContentOnly probe (plain alt screen) is no erase witness:
                // the shrink may be a region scroll (see [`ProbeTrust`]).
                cur.trust == ProbeTrust::Full && cur.row == brow && cur.fill < bfill
            }
            _ => false,
        }
    }

    /// Whether any MOVING light is alive (everything is_active tracks EXCEPT
    /// the standing forge ember and a pending kill hint).
    fn has_live_motion(&self) -> bool {
        self.needs_frame_cadence() || !self.vapor.is_empty()
    }

    /// Whether live geometry changes quickly enough to require phase-locked
    /// frame cadence. Vapor, forge ember, and kill-hint polling are deliberately
    /// excluded: their coarse deadlines are part of the latency budget.
    ///
    /// The GUI scheduler consumes this exact predicate when choosing between
    /// its phase-locked frame train and [`Self::next_change_deadline`]'s coarse
    /// path, so a visible rainbow kitty tail cannot silently fall into
    /// `frame cost + interval` cadence.
    ///
    /// A live OUTGOING style crossfade counts: its ~250 ms cosine ramp-down is
    /// exactly the brisk-light case, and — because `is_active` reaches here via
    /// `has_live_motion` — it also keeps the animation train ARMED across a
    /// style switch, so something draws before the next keystroke.
    #[must_use]
    pub fn needs_frame_cadence(&self) -> bool {
        !self.sparks.is_empty()
            || !self.particles.is_empty()
            || !self.fire_meteors.is_empty()
            || !self.bolts.is_empty()
            || self.ring.is_some()
            // THE CROWN'S WINDOW IS ARMED BY THE MOVE PATH FOR EVERY STYLE, so
            // it is the one v1 term that can still hold the frame train while
            // v2 owns the frame — every other rainbow-kitty pool (the ZOOMs,
            // the starburst, the pops, the ring, the particles) is laid by the
            // v1 spawn arm that SEAM POINT 1 returns before. Measured on the
            // confirmation schedule (`under_v2_the_seam_never_paces_the_frame_
            // train_for_v1s_crown`): 172 of 688 seam wakes were the crown's
            // 350 ms typing window outliving the engine's 83 ms brisk light —
            // wakes v2 never asked for. The crown is v1's head light; under v2
            // it neither paces (here) nor paints (the emit gate).
            || (self.crown_until.is_some() && !self.v2.engaged())
            || !self.fading.is_empty()
            // A live FRESH-INK pop is brisk light (its spring + ease-out move
            // every frame): keep the phase-locked train armed until the ring
            // drains — in practice the rainbow kitty sparks born beside every pop
            // outlive it, but the pop's cadence must not depend on that.
            // SEAM POINT 8 (§17.2, D14): OR in v2's three pools — stars,
            // meteors, a live ribbon transient — which its own gate reads as
            // `false` the instant it is not engaged (idle → zero, T6).
            || self.v2.needs_frame_cadence()
    }

    /// RESPONSIVENESS: when the NEXT visible change is due, so the host can pace
    /// the animation wake instead of pinning 60 fps for the whole effect
    /// lifetime. Returns `None` when nothing is live (idle → the host sleeps on
    /// `Wait`). While MOVING light animates, the full `frame_interval` (60 fps).
    /// A PENDING kill hint with no moving light paces at a COARSE
    /// [`Self::KILL_HINT_POLL_INTERVAL`]: the caret fallback needs only a few
    /// ticked frames after its 0.06 s grace, not ~21 full recomposes over the
    /// 0.35 s hint window — each of which re-takes the frame hold + the row
    /// probe + the RepaintKey fingerprint exactly while a repaint burst contends the
    /// term mutex. And once the only live term is the slowly-cooling FORGE
    /// EMBER — whose colour is u8-quantized and changes imperceptibly
    /// frame-to-frame, so the present gate already dedups the identical frames
    /// — a COARSE poll is enough: the multi-second tail costs a handful of
    /// cheap wakes, so a keypress never lands behind an in-flight recompose.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant, frame_interval: Duration) -> Option<Instant> {
        // SEAM POINT 9 (§17.2, D14): v2's next change — the earliest star
        // death, flight end or ribbon transient — `None` the instant nothing
        // of its is live, so it can only ever PULL a wake earlier, never hold
        // the loop awake (T6: idle → zero).
        let v2_due = if self.v2.engaged() {
            self.v2.next_change_deadline(now)
        } else {
            None
        };
        // BRISK light needs the full frame cadence. The shared predicate keeps
        // this engine decision identical to the host scheduler's phase-lock gate.
        //
        // WHAT v1 MAY STILL PACE UNDER v2, and why each arm below is not a
        // fall-through to a replaced mechanism: the crown is excluded by the
        // predicate itself; the ember (`cursor_temp`) is Fire's and reads
        // exactly `0.0` for every other style; the ring and the particles are
        // laid only by the v1 spawn arm SEAM POINT 1 returns before. What
        // remains is deliberately v1's: the poof hint (the erase detector's
        // licence — the very tick that mints v2's `Kill`) and the kill smoke
        // §5.6 keeps above stardust's erase stars. Both are coarse polls.
        let v1_due = if self.needs_frame_cadence() {
            Some(now + frame_interval)
        } else if let Some(t) = self.poof_hint_pending() {
            // The FIRST wake lands exactly as the fallback's grace opens (the
            // `>=` admits a tick at hint+grace), then the coarse poll takes
            // over — the poof fires at the caret's position AT grace-open,
            // narrowing the moved-caret race a flat 40 ms cadence would allow.
            // EITHER licence paces this: a plain Backspace from a quiet screen
            // needs the wake just as much as a kill chord does (see
            // [`Self::poof_hint_pending`]).
            //
            // NEVER `now` ITSELF. This read `grace_open.max(now).min(now +
            // poll)`, which is `now` for the whole of a hint whose grace has
            // already opened — a deadline the host arms and is woken by at
            // once, i.e. a hot loop at render cost for the rest of the hint
            // (≤ 0.29 s per Backspace). v1's 350 ms typing crown happened to
            // hold the frame train across it for a one-cell retreat, so only a
            // kill chord's 200 ms window ever exposed it; the moment v2's
            // crown-free seam released the train it spun on every Backspace
            // (the census loop caught it). The engine's `ARM_MIN` states the
            // law: a deadline at `now` is a busy re-arm, not a next change.
            let grace_open = t + Duration::from_secs_f32(Self::POOF_FALLBACK_GRACE);
            let poll = now + Self::KILL_HINT_POLL_INTERVAL;
            Some(if grace_open > now {
                grace_open.min(poll)
            } else {
                poll
            })
        } else if !self.vapor.is_empty() || self.ember_live() {
            // The smoke-only tail joins the forge ember on the COARSE poll: a hot
            // cursor keeps shedding 1-2 s wisps for seconds after typing stops, and
            // each wisp drifts/grows well under a pixel per 90 ms — so pinning the
            // full 60 fps present cadence over the whole cooling tail bought nothing
            // but recompose+present cost. `is_active` still tracks vapor, so the
            // window keeps ticking (and draining the smoke) — just coarsely.
            Some(now + Self::EMBER_POLL_INTERVAL)
        } else {
            None
        };
        // A held park schedules the tick that judges it: the flush in `tick`
        // is strict (`> p.patience()`), so the arm lands one `ARM_MIN` past
        // the park's OWN window rather than exactly on it. A same-row park
        // lives for 0.25 s; a proved foreign-row park keeps custody of an
        // in-flight press for 10 s. Scheduling the latter at 0.25 s leaves
        // it fresh and re-arms a 1 ms timer on every tick for the remaining
        // 9.75 s. The last unpaid press may expire sooner than the park if
        // it was already in flight when the foreign repaint arrived; that
        // expiry also ends custody. Never arm earlier than `now + ARM_MIN`.
        // v2-gated: the park is v2's state and the Classic branch returns
        // before the flush, so an ungated arm on a park stranded by a style
        // switch would spin. Without this arm a deadline-driven host sleeps
        // with the park held and its verdict comes at the next event, judged
        // at the park's frozen clock.
        let park_due = self.held_park.filter(|_| self.v2.engaged()).map(|p| {
            let park_expiry = p.at + p.patience() + rk::ARM_MIN;
            let credit_expiry = p.cross_row.then(|| {
                self.type_press_ring
                    .newest_unpaid(now)
                    .map_or(now, |at| at + Duration::from_secs_f32(IN_FLIGHT_PATIENCE_S))
                    + rk::ARM_MIN
            });
            park_expiry
                .min(credit_expiry.unwrap_or(park_expiry))
                .max(now + rk::ARM_MIN)
        });
        [v1_due, v2_due, park_due].into_iter().flatten().min()
    }

    /// Wipe every piece of TRANSIENT state: the in-flight light (sparks,
    /// particles, streaks, bursts, the whole glide-star family, meteors,
    /// vapor, bolts, ring, crossfades), ALL input hints, the row probes, and
    /// the rainbow kitty
    /// tail/pop/wake machinery. The ONE teardown all three dark/reset paths
    /// compose from — never hand-maintained lists at the call sites, which drift
    /// and leave a disabled engine reporting live light or a stale hint alive to
    /// fire on refocus. Everything transient clears here, unconditionally:
    /// nothing presents while dark, a stale hint must not outlive the wipe,
    /// and a wiped engine must report idle-zero so the host's frame train
    /// disarms. `sound_live`, the SOUND LEDGER
    /// ([`Self::clear_sound_ledger`]), the cursor-position tracking (`last` /
    /// `last_visible` / `last_move`), and `ctx_alt` stay with the callers —
    /// the four things the three paths deliberately differ on. The sound
    /// ledger is the newest of them: a load-shed frame is dark but still
    /// HEARD, so it must KEEP the credits an in-flight echo is about to
    /// spend, while a master-off, reset or unfocused tick must not.
    fn clear_transient_state(&mut self) {
        self.recent_typed_run = None;
        self.park_source_intact = None;
        self.carried_key_move = None;
        // A held park is judged before the teardown takes its stamp and
        // its pool: today's verdict, then the wipe.
        self.flush_held_park();
        self.clear_visual_geometry();
        self.type_press_ring.forget();
        self.insert.orphans = None;
        self.last_committed_type = None;
        // EVERY LICENSE TERM GOES. A teardown means the engine is dark or
        // reset; a hint that survived it would license the first program move
        // after the lights come back on.
        self.quench_hint = None;
        self.nav_hint = None;
        self.type_hint.clear();
        self.kill_hint = None;
        self.blink_hint = None;
        self.reflow_hint = None;
        self.return_hint = None;
        self.user_gesture_hint = None;
        self.newline_hint = None;
        self.insert.teardown();
        self.last_licensed_row = None;
        self.bs_poof_hint = None;
        self.bs_baseline = None;
        self.last_poof = None;
        self.row_cur.clear();
        self.row_prev.clear();
        self.row_cur_meta = None;
        self.row_prev_meta = None;
        self.clear_neighbor_rows();
    }

    /// Clear output-bearing geometry and the per-frame accessor caches, while
    /// leaving classifier/audio state untouched.
    fn clear_visual_geometry(&mut self) {
        self.fading.clear();
        self.ramp_in_at = None;
        self.sparks.clear();
        self.particles.clear();
        self.fire_meteors.clear();
        self.vapor.clear();
        self.last_smoke = None;
        self.bolts.clear();
        self.ring = None;
        self.crown_until = None;
        // These are cached per-frame accessor planes rather than resident
        // emitter state, but a reset can happen after a tick and before the
        // host projects them. Clear them here so a torn snapshot cannot copy
        // the just-invalidated frame over newer terminal content.
        self.halo_out.clear();
        self.patch_out.clear();
        self.under_out.clear();
        self.char_out.clear();
        self.fire_halo_out.clear();
    }

    /// THE KEY SEAM'S ONE LAW for every tick past the master switch: open
    /// when this tick DREW, or when the host marked it [`GlowConfig::audible`]
    /// (the key belongs to this window and the person asked for an aurora).
    /// A tick that is dark only because of a motion policy, the load-shed
    /// envelope, a degenerate grid, or the classic engine's own dark frame
    /// is still heard. When the seam closes, the sound ledger goes with it,
    /// and only then: a heard dark frame keeps its banked credits, so the
    /// echoes of keys typed during it stay silent when they land.
    ///
    /// Four call sites (the degenerate-grid return, the classic wake, the
    /// zero-amplitude return, the live path) and ONE law. That is what the
    /// `TrailSoundSeam` model's Tier-1 conformance drives on every style.
    /// Before 2026-09-22 the classic wake returned above every writer, so its
    /// seam was whatever the previous style left: a fresh `classic` session
    /// never clicked, and one switched from another style kept clicking in
    /// an unfocused window.
    fn settle_key_seam(&mut self, cfg: &GlowConfig, drew: bool) {
        let open = drew || cfg.audible;
        if !open {
            self.clear_sound_ledger();
        }
        self.sound_live = open;
    }

    /// UN-LATCH the dark-settled fast path (see [`Self::dark_settled`]): the
    /// caller is about to write state the `!cfg.enabled` teardown owns — a
    /// hint, a thermal kick, a row probe, freshly spawned light — so the next
    /// disabled tick must run the FULL wipe once more instead of trusting the
    /// latch. Idempotent and ~free; invoked from every externally reachable
    /// mutation of wiped state, so the latch cannot hold a fresh candidate or
    /// classifier cohort across a dark frame.
    #[inline]
    fn unsettle(&mut self) {
        self.dark_settled = false;
    }

    /// **THE CURTAIN AT A COORDINATE-SPACE SEAM** (2026-09-13, Rainbow Path
    /// v3 §2.8, step 7; laws A2/A4, D-2, D-3): [`Self::reset`] for every
    /// engine and hint of the space that just ended — EXCEPT that Rainbow
    /// Kitty's ribbon is not cut. Its cohorts are drawn into the caret they
    /// last stood under over `rk::ribbon::CURTAIN_S` (0.24 s) and spent to
    /// exactly zero, with nothing laid until it is over
    /// ([`rk::Engine::curtain`]); every other v2 mark — the flights and the
    /// stars, whose px are the old space's — goes with the reset, and the
    /// caret is learned from the next observed move. The host calls this
    /// where it called `reset()` for the alternate screen, a tab or terminal
    /// switch, a resize and a content invalidation; a style switch, a
    /// teardown and serious mode still `reset()`. On glass the old law was a
    /// one-frame cut: 15 lit cells → 0 inside 17 ms at alt-screen entry
    /// (audit s8, the refresh's s19), 21 → 0 in one capture on a resize (s7).
    /// For every style but rainbow kitty this IS `reset()`.
    pub fn curtain(&mut self, now: Instant) {
        self.reset_space(Some(now));
    }

    /// **HIDE RAINBOW KITTY'S NEXT FRAME** (Rainbow Path v3 §2.8, D-4, A4):
    /// the viewport is showing history. Returns `true` when v2 owns the
    /// frame and will run every clock and write nothing on the next tick —
    /// the band is kept and reappears where its clocks say when the live
    /// viewport returns; `false` for every other style, whose caller keeps
    /// its own law (a `reset()` — their marks are px of the live grid).
    pub fn hide_v2(&mut self) -> bool {
        if !self.v2.engaged() {
            return false;
        }
        self.v2.hide_next_frame();
        true
    }

    /// Whether v2 owns the frame — the question a host path asks when it
    /// must decide between KEEPING rainbow kitty's band and `reset()`-ing
    /// every other style's marks, WITHOUT arming a one-frame hide it has no
    /// tick to spend ([`Self::hide_v2`]'s contract; `TornBandLaw::Keep`).
    #[must_use]
    pub fn v2_owns_frame(&self) -> bool {
        self.v2.engaged()
    }

    /// Whether a one-frame hide is armed and not yet taken — the twins'
    /// witness that no non-ticking path left one standing.
    #[must_use]
    pub fn v2_hide_armed(&self) -> bool {
        self.v2.hide_armed()
    }

    /// Drop all in-flight light and forget the last cursor position. Used when the
    /// cursor's coordinate space changes out from under the animator (e.g. a single
    /// pane ⇄ split-pane layout transition), so the next tick can't spawn a comet
    /// from a stale cross-space position.
    pub fn reset(&mut self) {
        self.reset_space(None);
    }

    /// [`Self::reset`]'s body; with `curtain = Some(now)` v2's ribbon takes
    /// the curtain instead of the cut ([`Self::curtain`]).
    fn reset_space(&mut self, curtain: Option<Instant>) {
        // A layout/space transition invalidates coordinates WHOLESALE: the
        // fade residue, the live light and pops, the row probes (a stale probe
        // could witness a phantom cross-space "shrink"), and the hints all sit
        // in the old space, and the thermals go with them. The keyed-click
        // credits account for cues that just died with the layout — keeping
        // them would mute the first echo click in the NEW coordinate space.
        // `sound_live` is deliberately NOT cleared: the engine is still
        // enabled and the next tick re-states it; a reset is a
        // coordinate-space event, not a darkness one.
        self.unsettle();
        self.clear_transient_state();
        self.clear_sound_ledger();
        self.clear_thermals();
        // The salvaged engine holds its OWN cursor memory and its own in-flight
        // light, both in the coordinate space that just died — so it takes the
        // same reset, for the same reason.
        self.classic.reset();
        self.last = None;
        self.last_visible = None;
        self.hide_bridge_shown = None;
        // The echo-anchor row memory is coordinate-space state exactly like
        // `last`: a stale launch column in a new space would synthesize a
        // false anchored sweep. `print_anchor`/`print_anchor_seen` survive —
        // they mirror the terminal's own monotonic sample, and a kept `seen`
        // equal to the live seq means the pass idles until real new output.
        self.forget_anchor_rows();
        self.wrap_paid_row = None;
        self.last_move = None;
        self.crown_window_ms = Self::CROWN_MS;
        self.ctx_alt = false;
        self.pane_columns = None;
        // SEAM POINT 12 (§17.2, D14): a coordinate-space event drops every
        // v2 mark, its spine and its buffered events with the rest — and the
        // caret's seated stop, which was a reading of those marks.
        // v2 lets go of the frame entirely: the next drawing tick engages a
        // fresh engine, and a key struck before it lays nowhere (the engine
        // no longer knows the caret) instead of at a cell the reset forgot
        // the meaning of. Under a CURTAIN (`Self::curtain`) the engine keeps
        // the frame and its ribbon eases out instead; everything else in it
        // is reset the same way.
        match curtain {
            Some(now) if self.v2.engaged() => self.v2.curtain(now),
            _ => self.v2.set_engaged(false),
        }
        self.v2_stop = None;
        // The witness's samples were rows of the space that just died, and
        // so were the rows it asked for.
        self.witness_rows_n = 0;
        self.witness_asked.take();
    }

    /// Advance one frame: observe the cursor at `cur` (`Some(row,col)` visible,
    /// `None` hidden), spawn on a move, decay, and emit the CURRENT light quads
    /// (grid-interior pixels) into `out`, returning a fingerprint that changes on
    /// every visible change (0 when empty).
    pub fn tick(
        &mut self,
        cur: Option<(u16, u16)>,
        now: Instant,
        cfg: &GlowConfig,
        geom: Geom,
        out: &mut Vec<GlowQuad>,
    ) -> u64 {
        self.last_ch = geom.ch.min(u16::MAX as usize) as u16;
        self.last_origin_y = geom.origin_y;
        out.clear();
        self.halo_out.clear();
        self.patch_out.clear();
        self.under_out.clear();
        self.char_out.clear();
        self.fire_halo_out.clear();
        // The momentum pulse is a per-tick signal: clear it up front so the
        // host reads only THIS tick's correlated advance (set in `spawn`).
        self.momentum_pulse = None;
        // The witness's samples are THIS frame's and no other's: taken here,
        // so a tick that returns dark below cannot leave them for the next.
        let park_source = self.last.filter(|&(row, col)| {
            cur.is_some_and(|(next, _)| next != row) && self.park_source_unchanged(row, col)
        });
        self.carried_key_move = if matches!(cfg.style, GlowStyle::RainbowKitty)
            && cfg.enabled
            && cfg.intensity > 0.0
            && geom.cw != 0
            && geom.ch != 0
            && geom.rows != 0
            && geom.cols != 0
        {
            park_source
                .zip(cur)
                .filter(|&(from, to)| self.key_pushed_text_down(from, Some(to), now))
        } else {
            None
        };
        self.park_source_intact = park_source.filter(|_| self.carried_key_move.is_none());
        self.park_source_confirmed = false;
        if let Some(p) = self.held_park.filter(|p| p.cross_row)
            && let Some(sample) = self.witness_rows[..self.witness_rows_n]
                .iter()
                .find(|s| s.row == p.row)
        {
            let mut exact = true;
            let mut replaced = false;
            for col in 0..p.origin {
                let old = rk::witness::unit_at(&self.park_source_cells, col);
                let current = rk::witness::unit_at(&sample.cols, col);
                if old != current {
                    exact = false;
                    replaced |= !current.is_blank();
                }
            }
            self.park_source_confirmed = exact;
            if replaced {
                // A different nonblank prefix is a new input identity. A
                // blank split redraw can wait, but replacement revokes the
                // old press instead of letting it purchase unrelated text.
                self.flush_held_park();
                self.type_hint.clear();
                self.forget_typed_credits(now);
            }
        }
        let witness_n = std::mem::take(&mut self.witness_rows_n);
        let (asked, asked_n) = self.witness_asked.take();
        // THE SEAM'S GATE (`RAINBOW-KITTY-V2.md` §17.2, §17.3 phase 7, D14):
        // v2 IS the rainbow kitty — it owns the frame whenever the resolved
        // style is rainbow kitty, and only on a tick that DRAWS (the master
        // switch, a non-degenerate grid, a live amplitude), so a dark tick
        // disengages it exactly as the teardowns below wipe the shared
        // transient light. `set_engaged` resets on a TRANSITION only; the
        // steady state is one cached bool, and for the other nine styles the
        // `&&` chain ends at its first operand.
        self.v2.set_engaged(
            matches!(cfg.style, GlowStyle::RainbowKitty)
                && cfg.enabled
                && cfg.intensity > 0.0
                && geom.cw != 0
                && geom.ch != 0
                && geom.rows != 0
                && geom.cols != 0,
        );
        if !self.v2.engaged() {
            // The seated caret stop is v2's; a frame v2 does not own has none,
            // so a re-engage seats afresh instead of holding a stale hue.
            self.v2_stop = None;
        }
        // GENUINE OFF (the master switch) or a degenerate geometry: full zero —
        // the whole struct returns to rest (idle-zero). A momentary UNFOCUS is
        // NOT wiped here: it arrives as `intensity <= 0` (the motion amplitude)
        // and must not destroy minutes of earned forge momentum. That case is
        // handled AFTER the lazy cooling below, so the metal merely COOLS by the
        // elapsed gap and resumes on refocus.
        // A grid with no rows or no columns is as degenerate as a zero cell
        // size: the effects box is empty and the first star to settle would
        // hit `clamp_to_glass` with `min > max` (the 2026-09-14 audit).
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || geom.rows == 0 || geom.cols == 0 {
            // Full zero includes any in-flight style crossfade: OFF means dark
            // NOW, not a quarter-second of residue from the previous style.
            //
            // THE KEY SEAM on this return depends on WHY it is taken. The
            // MASTER SWITCH (and serious mode, which arrives as `enabled`)
            // closes it: while the master is off a keypress records no cue,
            // and any credit banked before the switch dies with the cues it
            // was accounting for. A DEGENERATE GRID does not close it. That
            // is a fact about the light's canvas, not a gate about sound, so
            // it takes the one law every tick past the master switch takes
            // ([`Self::settle_key_seam`]): dark, and heard exactly when the
            // host marked it `audible`. It used to close the seam like the
            // master switch, and `aterm ctl tone` had no name for that, so a
            // focused window with a momentarily empty grid read
            // `closed:engine-silent` — the alarm reserved for a regression.
            if cfg.enabled {
                self.store_key_timbre(cfg);
                self.settle_key_seam(cfg, false);
            } else {
                self.sound_live = false;
            }
            // LATCHED (driver-02): after one full teardown every store in the
            // wipe pair below is already at its cleared value, and only an
            // enabled tick, [`Self::reset`], or an [`Self::unsettle`] caller
            // can dirty them again — each of which drops the latch. So the
            // wipe runs ONCE per darkness, not once per frame, and a session
            // running e.g. only sparkle words pays two tracking stores here
            // instead of the ~60-80-store teardown. The cursor tracking stays
            // OUTSIDE the latch on purpose: re-enabling mid-session must see
            // the live position, or the first lit frame spawns one giant
            // spurious comet (the cursor_trail.rs disabled-branch rationale).
            //
            // The latch is set ONLY while the seam is shut. A degenerate grid
            // the key belongs to leaves the seam open, and `cue_keystroke`
            // writes the sound ledger without unsettling, so latching there
            // would hold a dirty ledger past a later master-off. It runs the
            // teardown every frame instead, which only a transient empty grid
            // pays. The sound ledger survives it for the reason it survives a
            // shed frame: the echoes of keys typed during it must still find
            // their credits.
            if !self.dark_settled {
                self.clear_transient_state();
                if !self.sound_live {
                    self.clear_sound_ledger();
                }
                self.clear_thermals();
            }
            self.dark_settled = !self.sound_live;
            self.last = cur;
            self.last_visible = cur.map(|c| (c, now));
            self.hide_bridge_shown = None;
            return 0;
        }
        // Any ENABLED tick may spawn light, bank heat, or arm hints: the next
        // dark frame must run one full teardown before it may latch again.
        self.dark_settled = false;

        // THE CLASSIC WAKE takes the frame whole. It is the v0.28 engine, not a
        // dressing of this one, so it runs BEFORE the modern spawn/admission/
        // emit machinery and returns in its place — which is also the point:
        // v0.28 spawned on observed cursor MOTION, while today's engine spawns
        // only on a move the host has licensed AND whose shape a style's gates
        // mint geometry from. That gate is why today's styles draw nothing at
        // all for a screen-crossing jump (`DECLINE_OFF_SHAPE`), and routing the
        // salvage through it would restore the old art with the old feel
        // amputated. Nothing here reads or writes the modern engine's animation
        // state, so no built-in style's output can move because this branch
        // exists.
        if matches!(cfg.style, GlowStyle::Classic) {
            // The erase licences expire only in `poof_scan`, which this
            // branch never reaches, and the classic engine never consumes
            // them — armed, they answered `is_active` / the 40 ms poof poll
            // for the rest of the focused session after one Backspace or
            // kill (a permanent wake loop; the 2026-09-14 audit). Dropped
            // here rather than expired: keeping them alive for the 0.35 s
            // window would still buy ~9 idle wakes per erase key for a poof
            // this engine cannot draw.
            self.kill_hint = None;
            self.bs_poof_hint = None;
            self.bs_baseline = None;
            // The classic wake SOUNDS (its voice is the phaser's `pew`), and
            // its key seam obeys the one law every style obeys. The v0.28
            // engine mints no echo cues of its own, so the key-time click is
            // its only sound; the timbre snapshot rides the same config the
            // modern path reads.
            self.store_key_timbre(cfg);
            self.settle_key_seam(cfg, cfg.intensity > 0.0);
            return self.classic.tick(cur, now, cfg, geom, out);
        }

        if self.rng == 0 {
            self.rng = 0x9E37_79B9;
        }
        // The classic field is geometry-derived. There is no phase seed or
        // colour latch to initialize here, so navigation before the first key
        // cannot make the visible rainbow stale.

        // STYLE SWITCH mid-animation: the still-in-flight sparks / particles /
        // bolts / meteors were forged under the OLD style's geometry, colours, and
        // heat envelope. Re-rendering them through the NEW style's emit path pops
        // their brightness and shape-shifts the residue, so the new style NEVER
        // inherits them — but DROPPING them outright darkens the screen until the
        // next keystroke, re-attacks the SDR envelope off an empty stream, and
        // disarms the animation train mid-switch. The contract is therefore a
        // CROSSFADE: the live animated state MOVES into an [`OutgoingFade`] that keeps
        // ticking under its SNAPSHOTTED old config — decay and drift only, never
        // a typing-driven spawn — behind a ~250 ms cosine ramp-down, while the
        // incoming style ramps IN over ~120 ms (`ramp_in_at` below). Typing
        // WARMTH (heat/flare/coal/quench + the eased display temperatures)
        // CARRIES into the incoming style so it doesn't start cold mid-typing;
        // coordinate tracking (last / last_visible / last_move) and the input
        // hints survive as before, so the switch spawns no phantom comet and a
        // pending key→echo pairing still lands. Pending SOUND cues are dropped at
        // the edge — a cue spawned under the old style must never play the new
        // style's palette. Rapid switch chains hand each outgoing style off at
        // the amplitude it actually reached (`OutgoingFade::level0`), capped at
        // [`Self::FADE_CAP`] concurrent fades (oldest dropped). A pack→pack swap
        // keeps `style == Custom`, so also fade when the live pack fingerprint
        // disagrees — otherwise light forged under the old pack would re-render
        // through the new pack's interpreter.
        let live_pack_fp = cfg.pack.as_ref().map_or(0, |p| p.pack_fp);
        let style_changed = self.last_style.is_some_and(|s| s != cfg.style);
        let pack_changed = self.last_style.is_some() && self.last_pack_fp != live_pack_fp;
        if style_changed || pack_changed {
            self.begin_style_fade(now);
        }
        self.last_style = Some(cfg.style);
        self.last_pack_fp = live_pack_fp;
        // COMPARE-ON-WRITE (driver-05): `last_cfg` backs exactly one cold
        // read — the outgoing-fade snapshot in `begin_style_fade` — yet the
        // unconditional store moved ~0.5 KiB per enabled frame. The invariant
        // is unchanged: after every enabled tick `last_cfg == Some(*cfg)`
        // still holds (equal ⇒ the stored bytes already ARE this tick's
        // values; the derive is structural), so the steady state pays a
        // short-circuiting field compare — the `Option<TrailParams>`
        // discriminant test skips the ~440-byte pack body whenever no pack is
        // armed — instead of the copy. A NaN field compares unequal and
        // simply re-stores: today's behaviour, made explicit.
        if self.last_cfg.as_ref() != Some(cfg) {
            self.last_cfg = Some(*cfg);
        }
        // Incoming ramp-IN (~120 ms): scale the live style's amplitude so it
        // RISES under the outgoing fade instead of popping to full brightness.
        // Strictly inert without a recent switch: `ramp_in_scale` returns
        // exactly 1.0 and the caller's config reference is used untouched, so
        // the no-switch path stays byte-identical (the golden proof).
        let ramped_cfg;
        let cfg = {
            let scale = self.ramp_in_scale(now);
            if scale < 1.0 {
                ramped_cfg = GlowConfig {
                    classic_mono: false,
                    intensity: cfg.intensity * scale,
                    ..*cfg
                };
                &ramped_cfg
            } else {
                cfg
            }
        };

        // Cool the typing heat lazily (correct across arbitrary idle gaps, and
        // costs nothing while the animator is disarmed).
        if let Some(t0) = self.heat_at {
            let elapsed = now.saturating_duration_since(t0);
            let dt = elapsed.as_secs_f32();
            if dt > 0.0 {
                // Fire cools on its own slower τ (momentum survives a short
                // thought); every other style keeps the shipped 0.9 s. A Trail
                // Pack may override the decay τ (custom only): `None` and every
                // built-in (no pack) keep HEAT_DECAY_TAU byte-for-byte. Shared
                // with the key-time click's timbre prediction via `heat_tau`,
                // so the two cannot drift apart.
                let heat_tau = Self::heat_tau(cfg);
                self.heat *= (-dt / heat_tau).exp();
                if self.heat < 0.005 {
                    self.heat = 0.0;
                }
                self.flare *= (-dt / Self::FLARE_DECAY_TAU).exp();
                if self.flare < 0.005 {
                    self.flare = 0.0;
                }
                // EMBERFORGE thermal evolution (fire only; ~5 flops for others'
                // zeros). Order matters: integrators decay first, then the eased
                // display temperature chases the fresh target, so one tick's
                // consumers all read one coherent temperature.
                self.coal *= (-dt / Self::COAL_TAU).exp();
                if self.coal < 0.005 {
                    self.coal = 0.0;
                }
                self.quench *= (-dt / Self::QUENCH_TAU).exp();
                if self.quench < 0.005 {
                    self.quench = 0.0;
                }
                let target = self
                    .heat
                    .max(self.flare)
                    .max(self.coal * Self::COAL_FLOOR)
                    .clamp(0.0, 1.0)
                    * (1.0 - Self::QUENCH_DAMP * self.quench);
                let rise = target - self.disp_t;
                let ease = if rise > 0.5 {
                    Self::DISP_SLAM_S // a jump slam: fast whoosh, still no hard pop
                } else if rise > 0.0 {
                    Self::DISP_ATTACK_S
                } else {
                    // An active QUENCH accelerates the die-down (up to ~3×):
                    // water on hot steel kills a fire fast, while a natural
                    // pause keeps the slower cinematic release.
                    Self::DISP_RELEASE_TAU / (1.0 + 2.0 * self.quench)
                };
                self.disp_t += rise * (1.0 - (-dt / ease).exp());
                if self.disp_t < 0.005 {
                    self.disp_t = 0.0;
                }
                // The flame field's clock: integrate churn-weighted time so
                // the fire's motion accelerates smoothly with its temperature.
                // Wrap on a bounded ring (like `rainbow.phase`) so a multi-day
                // session can't let f32 ULP overtake the increment (freeze) or
                // saturate the field's `* 1024.0 as u32` cast — see FLAME_PHASE_RING.
                self.flame_phase += dt * (1.6 + 2.2 * self.disp_t);
                if self.flame_phase >= Self::FLAME_PHASE_RING {
                    self.flame_phase -= Self::FLAME_PHASE_RING;
                }
                if matches!(cfg.style, GlowStyle::Fire) {
                    // Cursor metal chases the display temperature with
                    // hysteresis; the quench meter halves the cooling τ at full
                    // (water on hot steel).
                    let temp_tau = if self.disp_t > self.cursor_temp {
                        Self::TEMP_ATTACK_TAU
                    } else {
                        Self::TEMP_RELEASE_TAU / (1.0 + self.quench)
                    };
                    self.cursor_temp +=
                        (self.disp_t - self.cursor_temp) * (1.0 - (-dt / temp_tau).exp());
                    if self.cursor_temp < 0.005 {
                        self.cursor_temp = 0.0;
                    }
                } else {
                    self.cursor_temp = 0.0;
                }
            }
        }
        self.heat_at = Some(now);
        // THE KEY-TIME CLICK'S TIMBRE INPUTS, written by every tick that gets
        // past the master switch (the degenerate-grid return and the classic
        // wake above store them through the same [`Self::store_key_timbre`])
        // — including one that is about to return dark for a motion or
        // performance reason, which now still sounds its keys.
        // THE INVARIANT THIS SITE EXISTS FOR, stated exactly: both
        // [`Self::settle_key_seam`] calls below sit BELOW this store, so the
        // seam can never open on a stale thermal snapshot.
        // Both are pure functions of `cfg`: `heat_gain` reads fire-ness and
        // `pack.heat.gain`, `heat_tau` reads the style and `pack.heat.tau`,
        // and NEITHER reads `intensity` — so the ramp shadow above (which
        // scales `intensity` alone) cannot move them, and the value computed
        // here is bit-identical to the one the lit path used to compute ~40
        // lines further down. Sited directly under the lazy decay that just
        // aged the integrators they pair with, so a cue minted after any
        // non-master-off tick reads a COHERENT thermal snapshot rather than a
        // `Default` zero.
        self.store_key_timbre(cfg);

        // ZERO AMPLITUDE (unfocused / motion-reduced / a genuine 0 intensity):
        // emit NO light and spawn nothing, but KEEP the long-lived thermal
        // integrators (coal, cursor_temp, heat, flame_phase) that the lazy cooling
        // above already aged by the elapsed gap. A momentary focus blip then costs
        // only its own duration of cooling — the earned forge momentum survives
        // and resumes on refocus — while a long unfocus (or a real 0 intensity)
        // cools to EXACTLY zero over that same lazy decay, so `is_active` disarms
        // and no perpetual wake leaks (idle-zero holds). Only `reset()` — called
        // on a real layout/space change — wipes the momentum outright. The visible
        // MOVING light and the transient candidate/classifier cohort are dropped
        // (nothing presents while dark); the cursor
        // position is tracked so refocus spawns no phantom comet.
        if cfg.intensity <= 0.0 {
            // The crossfade residue and the pops are MOVING light exactly like
            // the sparks: nothing presents while dark, so an unfocus /
            // reduced-motion blip mid-switch drops them rather than resuming
            // them stale later. (The ramp-IN floor keeps a genuine switch off
            // this path: a live positive intensity is never scaled to 0 by the
            // ramp.)
            //
            // THE KEY SEAM DOES NOT CLOSE WITH THE LIGHT. Zero amplitude is
            // dark, and dark used to mean silent — which made a load-shed
            // frame and a `Reduce Motion` session mute every keystroke, a
            // MOTION/PERFORMANCE policy deleting an AUDIO feature for a click
            // that costs no GPU. So the seam now asks [`GlowConfig::audible`]
            // instead: dark for a motion or performance reason is still
            // HEARD, dark because the key belongs to another window is not.
            // The light's behaviour on this path is unchanged in every other
            // respect. NOTE: no `clear_thermals` here — the lazy decay above
            // already cooled the long-lived integrators by the elapsed gap,
            // which is the whole focus-blip contract this branch guards.
            //
            // The SOUND ledger is transient state the audio half owns, so it
            // goes only when the seam goes ([`Self::settle_key_seam`]): wiping
            // the banked key credits on every shed frame would double-click
            // the whole burst the moment the latch flapped back to lit (the
            // in-flight echoes would find no credit to spend). An unfocused
            // tick keeps today's wipe.
            self.settle_key_seam(cfg, false);
            self.clear_transient_state();
            self.track_shown_caret(cur, now);
            return 0;
        }
        // Past both dark returns: this tick DRAWS, so the key seam is open.
        // Only the MASTER-OFF return shuts it unconditionally (it is the
        // `cursor_trail = false` / serious-mode case the user asked for);
        // every other return — a degenerate grid, the classic wake, zero
        // amplitude — asks `cfg.audible` through the same
        // [`Self::settle_key_seam`], so a shed or motion-reduced frame is dark
        // but heard. The thermal constants this click reads are written
        // above, at the end of the lazy decay, so every tick past the master
        // switch leaves them fresh.
        self.settle_key_seam(cfg, true);

        self.expire_seam_holds(now);
        // Spawn on a real move between two visible positions — where "visible"
        // BRIDGES ConPTY's per-echo hide window ([`Self::hidden_bridge_source`]).
        let spawn_from = self
            .last
            .or_else(|| self.hidden_bridge_source(cur, now, cfg, geom));
        self.judge_observed_caret(spawn_from, cur, now, cfg, geom);
        self.carried_key_move = None;
        // The hidden/parked-caret echo lane: when the DEC cursor cannot
        // witness the keystroke's echo (hidden across frames, or parked on a
        // different row), the host-fed print anchor is the mutation site of
        // the licensed move. Runs AFTER the move lane so a visible cursor
        // move always owns its own tick.
        let cursor_move_observed = spawn_from
            .zip(cur)
            .is_some_and(|((pr, pc), (cr, cc))| pr != cr || pc != cc);
        // The paid pending-wrap witness is read by the fold judged above and
        // consumed by ANY observed caret move (the fold, a Backspace, a
        // relocation): it names the caret parked on the paid cell.
        if cur.is_some() && cur != self.last_visible.map(|(cell, _)| cell) {
            self.wrap_paid_row = None;
        }
        let content_echo = self.same_caret_typed_echo(cur, cursor_move_observed, now, cfg, geom);
        self.echo_anchor_pass(cur, cursor_move_observed || content_echo, now, cfg, geom);
        self.track_shown_caret(cur, now);

        // ERASE POOF detection: a fresh kill-key hint paired with a same-row NET
        // SHRINK of the probed row content (stable surviving prefix + suffix)
        // puffs smoke off the exact vanished span — before the decay/emit passes
        // below so the puffs join THIS frame's emit. Runs off the host-fed row
        // probe ([`Self::observe_row`]); hosts that never probe or never arm the
        // kill hint take two `Option` reads and are otherwise byte-identical.
        self.poof_scan(now, cfg, geom);

        self.sparks.retain(|s| Self::spark_resident_at(now, s));
        let spark_cap = Self::MAX_SPARKS;
        if self.sparks.len() > spark_cap {
            let drop = self.sparks.len() - spark_cap;
            self.sparks.drain(0..drop);
        }
        self.particles
            .retain(|p| now.saturating_duration_since(p.born).as_secs_f32() < p.life);
        self.fire_meteors
            .retain(|m| now.saturating_duration_since(m.born).as_secs_f32() < m.life);
        self.vapor
            .retain(|v| now.saturating_duration_since(v.born).as_secs_f32() < v.life);
        // A HOT cursor SMOKES: while the metal is above SMOKE_TEMP, wisps
        // shed off its top edge at a temperature-scaled rate (hotter = denser
        // smoke), each with its own drift/waft/life. Ceases as it cools —
        // vapor drains and the idle-zero law holds.
        if matches!(cfg.style, GlowStyle::Fire)
            && self.cursor_temp > Self::SMOKE_TEMP
            && let Some((cr, cc)) = cur
            && (cr as usize) < geom.rows
            && (cc as usize) < geom.cols
        {
            let interval = 0.28 - 0.18 * self.cursor_temp; // 0.22s warm → 0.10s hot
            let due = self
                .last_smoke
                .is_none_or(|t| now.saturating_duration_since(t).as_secs_f32() >= interval);
            if due && self.vapor.len() < Self::MAX_VAPOR {
                let (r0, r1, r2) = (self.frand(), self.frand(), self.frand());
                let cell = geom.ch as f32;
                self.vapor.push(Vapor {
                    x0: geom.origin_x as f32 + (cc as f32 + 0.3 + 0.4 * r0) * geom.cw as f32,
                    y0: geom.origin_y as f32 + cr as f32 * cell,
                    vx: (r1 - 0.5) * 0.25 * cell,
                    vy: -(0.28 + 0.22 * r2) * cell,
                    gy: -0.10 * cell, // buoyant
                    life: 1.2 + r0 * 1.0,
                    seed: r1,
                    kind: VaporKind::Smoke,
                    born: now,
                });
                self.last_smoke = Some(now);
            }
        }
        // ARRIVAL-TIME STRIKE: the eruption fires when the meteor head lands
        // (not at launch) — flare, landing ring, and the ember debris fountain
        // all happen where and when the streak's tail drains in. Deferring the
        // fountain to the strike (along the FINAL vector) also makes it immune
        // to shell repaint choreography: a retargeted meteor strews its debris
        // along the coalesced path, never along a parked intermediate hop.
        let mut struck: Option<Meteor> = None;
        for m in &mut self.fire_meteors {
            if !m.arrived
                && now.saturating_duration_since(m.born).as_secs_f32()
                    >= m.life * Self::METEOR_STRIKE_FRAC
            {
                m.arrived = true;
                struck = Some(*m);
            }
        }
        if let Some(m) = struck {
            self.flare = 1.0;
            if cfg.ring {
                // Graded by the flight the head just finished, in cells.
                let scale = crate::rainbow_kitty::timing::impact(
                    (m.x1 - m.x0).abs() / geom.cw.max(1) as f32,
                );
                self.ring = Some(Ring {
                    cx: m.x1,
                    cy: m.y1,
                    born: now,
                    life: classic_ring_life(0.18, scale),
                    scale,
                });
            }
            if matches!(cfg.style, GlowStyle::Fire) {
                self.meteor_strike_fountain(&m, now, geom);
            }
        }
        self.bolts
            .retain(|b| now.saturating_duration_since(b.born).as_secs_f32() < b.life);
        if let Some(r) = self.ring
            && now.saturating_duration_since(r.born).as_secs_f32() >= r.life
        {
            self.ring = None;
        }
        if self.crown_until.is_some_and(|t| now >= t) {
            self.crown_until = None;
        }

        // ---- emit the light layers ----
        // The radial-halo / under-ink / charred-ink streams ride beside `out`
        // (the emitters are `&self`; the vecs are taken and restored so no
        // borrow overlaps).
        // Refresh the tint palette if (and only if) the theme moved. See
        // [`Self::glyph_palette`].
        let glyph_key = (cfg.theme_fg, cfg.theme_bg);
        if self.glyph_palette.map(|(k, _)| k) != Some(glyph_key) {
            self.glyph_palette = Some((
                glyph_key,
                fresh_ink_glyph_palette(cfg.theme_fg, cfg.theme_bg),
            ));
        }
        let mut halos = std::mem::take(&mut self.halo_out);
        let mut patches = std::mem::take(&mut self.patch_out);
        // `under_out` (the glow_under stream, composited BENEATH the glyph ink)
        // carries the rainbow ribbon body + jump ZOOMs and Water's fluid wake,
        // so letters draw OVER those broad marks at full contrast. (The fire's
        // flame body left this stream for the per-pixel `patch_out` field; the
        // stream itself — pipeline splice, both renderers' under pass, fp fold
        // — stayed live.)
        let mut under = std::mem::take(&mut self.under_out);
        let mut charred = std::mem::take(&mut self.char_out);
        let mut halo_cells = std::mem::take(&mut self.fire_halo_out);
        let mut engulf = std::mem::take(&mut self.engulf_scratch);
        // Bolts FIRST: on a monster jump the fat beam alone can saturate the quad
        // budget, and the lightning is the star — truncation must shed beam haze,
        // never a strike.
        // Lightning strikes (Laser only): reuse the resident bolt scratch via the
        // same mem::take idiom as the halo/patch/under/char/engulf scratches above.
        let mut bolt_verts = std::mem::take(&mut self.bolt_verts);
        // ONE branch: a resolved Trail Pack drives the DATA interpreter; every
        // built-in falls to the UNCHANGED emit sequence below. Because the whole
        // sequence is wrapped, NO built-in emitter gains a `cfg.pack` read — the
        // additivity mandate ("no builtin tick path branches on pack state") holds
        // structurally, and the shared tail (MAX_QUADS/MAX_HALOS truncation + the
        // fingerprint fold) runs identically for both. `cfg.pack` is only ever
        // `Some` when `style == GlowStyle::Custom` (the resolver's contract).
        if let Some(p) = cfg.pack.as_ref() {
            self.emit_custom(now, cfg, geom, cur, out, &mut halos, p);
            out.truncate(Self::MAX_QUADS);
        } else {
            self.emit_bolts(now, cfg, geom, out, &mut bolt_verts);
            // No-op for Water/rainbow kitty (beam=false); on light themes the beam family
            // emits source-over veil halos instead of additive quads.
            self.emit_comet(now, cfg, geom, cur, out, &mut halos);
            out.truncate(Self::MAX_QUADS);
            // The per-pixel fire (Fire only): FirePatches render at the under-ink
            // seam (P6 — letters read as silhouettes inside the volume) through the
            // shared byte-exact integer field. NOT gated on the GlowQuad budget — the
            // flame writes the SEPARATE patch/halo streams (each self-capped inside
            // `emit_flames`), so the old `out.len() < MAX_QUADS` gate could only ever
            // drop the whole flame body for a frame if a foreign emitter had already
            // saturated `out`.
            self.emit_flames(
                now,
                cfg,
                geom,
                &mut patches,
                &mut charred,
                &mut halo_cells,
                &mut engulf,
                &mut halos,
            );
            if out.len() < Self::MAX_QUADS {
                self.emit_fire_meteors(now, cfg, geom, out); // jump streaks (Fire only)
            }
            if under.len() < Self::MAX_QUADS {
                // Water's broad reflection belongs below glyph ink. Its crown,
                // landing ripple and free droplets remain over-ink accents.
                self.emit_water(now, cfg, geom, cur, &mut under);
            }
            if self.v2.engaged() {
                // SEAM POINT 3 (§17.2, D14): v2 owns the rainbow's whole emit
                // phase — the ZOOMs, the starburst, the ribbon and its
                // starfield, the fresh-ink light and glyph tint, the wake — in
                // ONE call at the SAME position (after fire / water, before
                // crown / forge / ring / particles / vapor), writing into the
                // SAME `under` / `out` / `halos` scratch, so the §3.4 ledger
                // below prices its output exactly as it prices v1's.
                // `Config::from_glow` is a seven-field copy, not a resolution;
                // the beams scratch and the cue sink are resident (§18: zero
                // allocation per frame). `rainbow_exit_swoosh` above stays
                // v1-only — v2's ribbon owns its own ending.
                // §18's "zero allocation per frame", made true for the streams
                // the host lends v2. Its peaks are STATE-dependent — a hot
                // meteor's coma beside a full sky is a later frame than a
                // cold one's — so a scratch grown on demand can still grow on
                // a steady tick long after warm-up (measured: the 17th halo of
                // a burst, 768 bytes, on tick 314 of a settled session). The
                // streams are sized to their caps ONCE, on the first engaged
                // frame, exactly as the engine sizes its own pools; every
                // later frame is a capacity compare.
                Self::reserve_v2_scratch(&mut under, out, &mut halos);
                if self.v2_beams.capacity() < Self::V2_BEAM_SCRATCH {
                    self.v2_beams.reserve(Self::V2_BEAM_SCRATCH);
                }
                if self.v2_cues.capacity() < Self::V2_CUE_SCRATCH {
                    self.v2_cues.reserve(Self::V2_CUE_SCRATCH);
                }
                let v2_cfg = rk::Config::from_glow(cfg, self.reduced_motion);
                // THE FOLLOW PASS (2026-09-21, the band follows its text):
                // BEFORE the tick replays this frame's events, the engine
                // reads the rows the host sampled under its lock and carries
                // every run whose text moved a row up or down — a
                // bottom-anchored composer growing without a scroll — to
                // the row its glyphs now stand on, with every clock intact
                // (`rk::Engine::follow_rows`). The same samples feed the
                // witness after the tick, below.
                if witness_n > 0 {
                    let mut samples = [rk::witness::RowSample {
                        row: u16::MAX,
                        cols: &[],
                    }; CURSOR_WITNESS_ROWS];
                    let n = witness_n.min(samples.len()).min(self.witness_rows.len());
                    for (sample, slot) in samples.iter_mut().zip(&self.witness_rows[..n]) {
                        *sample = rk::witness::RowSample {
                            row: slot.row,
                            cols: &slot.cols,
                        };
                    }
                    let park = self.held_park.filter(|p| !p.cross_row && p.fresh(now));
                    let pair = park.map(|p| ((p.row, p.origin), (p.landing_row, p.landing)));
                    let (_, proved) =
                        self.v2
                            .follow_rows_with_park(&samples[..n], &asked[..asked_n], now, pair);
                    if proved && let Some(p) = self.held_park.as_mut() {
                        p.content_followed = true;
                    }
                }
                let mut frame = rk::Frame {
                    under: &mut under,
                    out: &mut *out,
                    halos: &mut halos,
                    beams: &mut self.v2_beams,
                    cues: &mut self.v2_cues,
                    caret: CaretSeam::default(),
                    companion: None,
                    fp: 0,
                };
                self.v2.tick(now, geom, &v2_cfg, &mut frame);
                self.v2_caret = frame.caret;
                // SEAM POINT 11 (§17.2): the frame's cues join the one backlog
                // the host drains, under v1's own refusal — a full backlog
                // drops the newest, exactly as `cue_sound` does — so a host
                // that never drains (a harness, a wedged worker) sees the
                // same bound v1 gives it, and the resident sink keeps the
                // steady frame allocation-free (§18).
                for cue in self.v2_cues.drain(..) {
                    if self.sound_cues.len() < Self::MAX_SOUND_CUES {
                        self.sound_cues.push(cue);
                    }
                }
                // THE CARET'S STOP IS SEATED HERE, on the seam's own tick and
                // only on an event the caret can see: its CELL moved (a key,
                // a jump, an Enter, a backspace — the frame's `Move` is
                // ingested above, so `field_t` is this frame's answer at the
                // new cell), or the cell under it owns field light (a lay at
                // the caret, an erase onto a lit cell). Every other tick
                // HOLDS the seat; see [`Self::rainbow_head_rgb`] for the
                // measured pop this closes. `field_at` is O(1) in columns
                // (§18), so a steady frame pays one lane lookup.
                let owns_light = cur.is_some_and(|(r, c)| self.v2.field_at(r, c).is_some());
                if owns_light || self.v2_stop.is_none_or(|(_, at)| at != cur) {
                    self.v2_stop = Some((self.v2_caret.field_t, cur));
                }
                // THE CONTENT WITNESS (2026-09-12, the abandoned band): after
                // the tick that laid this frame's cells, read every resident
                // cell against the grid rows the host sampled under its lock
                // — the batch already applied, so a prompt redraw that put
                // the same text back is seen as the same text (D2) — and
                // retire, on the fast melt, the cells whose glyph has changed
                // or gone (`rk::witness`). The ribbon is an absolute-cell
                // record of the keys; this is the one place it learns that
                // the text it was laid under has moved.
                if witness_n > 0 {
                    let mut samples = [rk::witness::RowSample {
                        row: u16::MAX,
                        cols: &[],
                    }; CURSOR_WITNESS_ROWS];
                    let n = witness_n.min(samples.len()).min(self.witness_rows.len());
                    for (sample, slot) in samples.iter_mut().zip(&self.witness_rows[..n]) {
                        *sample = rk::witness::RowSample {
                            row: slot.row,
                            cols: &slot.cols,
                        };
                    }
                    self.v2.witness_rows(&samples[..n], now);
                }
            } else {
                // THE CROWN IS THE OTHER NINE STYLES' HEAD LIGHT: pure RADIAL
                // light (RainHalo), no GlowQuad budget to guard, its own
                // MAX_HALOS cap bounds it. v2 owns the rainbow kitty's head
                // (§7.1's caret seam, the ribbon's nozzle) and lists the crown
                // among the layers its one call precedes, not among the ones
                // it keeps — so it neither paints nor paces
                // ([`Self::needs_frame_cadence`]) while v2 owns the frame.
                self.emit_crown(now, cfg, geom, cur, &mut halos);
            }
            // The forge dressing rides `cursor_temp`, NOT the crown's last-move
            // window, so it must run whether or not a crown is live (fixes the
            // hard-pop at window expiry / the per-keystroke blink at slow typing).
            if out.len() < Self::MAX_QUADS {
                self.emit_forge_cursor(cfg, geom, cur, out, &mut halos);
            }
            if out.len() < Self::MAX_QUADS {
                self.emit_ring(now, cfg, geom, out, &mut halos);
            }
            if out.len() < Self::MAX_QUADS {
                self.emit_particles(now, cfg, geom, out, &mut halos);
            }
            self.emit_vapor(now, cfg, geom, &mut halos);
            self.bolt_verts = bolt_verts;
            // THE HALO CAP (§18): every style's streams are self-capped at
            // the emitter (v2's by construction; §3.4's ledger died with v1),
            // so the only shared pass left is the halo count.
            Self::cap_and_clear_v2_streams(&mut halos);
        } // end built-in emit branch (the custom interpreter above is the other arm)
        self.halo_out = halos;
        self.patch_out = patches;
        self.under_out = under;
        self.char_out = charred;
        self.fire_halo_out = halo_cells;
        self.engulf_scratch = engulf;

        // OUTGOING style crossfades: tick each moved-out animator under its
        // snapshotted OLD config behind the cosine ramp-down and merge its
        // light into THIS frame's streams (additive premultiplied light is
        // order-independent, so merge order can't change the composite).
        // Provably inert when no switch happened — the list is empty and this
        // is one `is_empty` test. Runs BEFORE the shared truncation + fp fold
        // below, so caps and fingerprints see ONE merged frame: the glow
        // stream never goes empty mid-switch (the renderer's SDR attack
        // envelope keeps its sustain instead of re-attacking — the switch
        // strobe), and the changing fade bytes keep the present train fed.
        self.tick_fades(now, geom, out);

        // Bound the snapshot identically for both renderers.
        if out.len() > Self::MAX_QUADS {
            out.truncate(Self::MAX_QUADS);
        }
        self.under_out.truncate(Self::MAX_QUADS);

        // Radial halos are part of the visible frame exactly like the quads;
        // bound them identically for both backends before fingerprinting the
        // exact payload of every plane.
        self.halo_out.truncate(Self::MAX_HALOS);
        let mut fp = FrameFingerprint::default();
        fp.glow_quads(FrameFingerprint::GLOW_OUT, out);
        fp.fire_patches(&self.patch_out);
        fp.glow_quads(FrameFingerprint::GLOW_UNDER, &self.under_out);
        fp.chars(&self.char_out);
        fp.fire_halos(&self.fire_halo_out);
        fp.rain_halos(&self.halo_out);

        // The FORGE cursor fill is visible frame state (it rides the host's
        // `cursor_fill_override`), so fold it in WHILE THE METAL IS CHANGING —
        // a heating/cooling cursor keeps presenting until the fill's quantized
        // colour settles. At REST the fill is a CONSTANT dull ember (forge_fill
        // never returns None now), which needs no per-frame fp churn: it rides
        // cursor_fill_override and stays drawn, so we do NOT fold it and the
        // idle fingerprint returns to 0 (idle-zero preserved). Fire-gated so
        // other styles' fingerprints are byte-identical to before.
        if matches!(cfg.style, GlowStyle::Fire)
            && self.cursor_temp > Self::FORGE_MIN_TEMP
            && let Some(fill) = self.forge_fill()
        {
            fp.forge_fill(fill);
        }
        fp.finish()
    }

    // ----- style crossfade (the STYLE SWITCH contract in `tick`) -----

    /// Outgoing ramp-DOWN length. Long enough that the eye reads a handoff
    /// (and comfortably overlaps the ~120 ms ramp-IN, so the switch never
    /// shows a trough), short enough that three rapid switches in a row stay
    /// legible under the [`Self::FADE_CAP`]-bounded fade list. Cosine-eased in
    /// `tick_fades` — it lands at exactly 0 (no exp-tau tail to babysit) and
    /// its zero-slope start blends into the old style's own exp-tau decays
    /// without a corner.
    const FADE_OUT_S: f32 = 0.25;
    /// Incoming ramp-IN length: the same ~120 ms attack family as the eased
    /// display temperatures (`DISP_ATTACK_S`), so the new style swells the way
    /// everything else in this engine does — quickly, but never as a pop.
    const RAMP_IN_S: f32 = 0.12;
    /// The ramp-IN's starting amplitude. Nonzero for two reasons: a dead-zero
    /// first frame would trip `tick`'s `intensity <= 0` full-wipe path (which
    /// must stay reserved for a GENUINE unfocus/reduced-motion zero), and the
    /// incoming style showing a faint immediate presence is what makes the
    /// handoff read as a crossfade rather than fade-out-then-in.
    const RAMP_IN_FLOOR: f32 = 0.15;
    /// Concurrent outgoing fades. Two is enough for the product gesture
    /// ("quickly select multiple trails in a sequence"): with a 250 ms
    /// envelope, a third overlapping switch means the oldest fade is already
    /// nearly dark — dropping it is invisible, and the bound keeps a
    /// scroll-wheel spin through the style menu O(1).
    const FADE_CAP: usize = 2;

    /// Begin an outgoing crossfade for the style that was live UNTIL this tick:
    /// MOVE the in-flight animated collections (the forged light itself) into a
    /// boxed ghost animator, COPY the thermal/phase scalars it needs to keep
    /// rendering that light at its earned warmth, and leave the live engine's
    /// own warmth in place (the carry-over: the incoming style must not start
    /// cold mid-typing). Called from `tick` the instant the style/pack guard
    /// disagrees; the caller has NOT yet overwritten `last_style`/`last_cfg`,
    /// so both still describe the outgoing style.
    fn begin_style_fade(&mut self, now: Instant) {
        // The outgoing style hands off at the amplitude it actually held: if it
        // was itself still ramping in (a rapid switch chain), its fade starts
        // from that partial level instead of popping back up to full. Read
        // BEFORE re-arming the ramp for the incoming style below.
        let level0 = self.ramp_in_scale(now);
        self.ramp_in_at = Some(now);
        // A cue spawned under the old style must never play the new style's
        // palette (the host maps cues → sounds by the CURRENT style when it
        // drains, after this tick): drop the pending backlog at the edge.
        self.sound_cues.clear();
        // The credits are the ledger for exactly those dropped cues. Left
        // standing, they would silence the first echo clicks under the NEW
        // style for a key whose click was never actually played.
        self.clear_keyed_clicks();
        let Some(cfg) = self.last_cfg else {
            // Unreachable in practice (the switch guard requires a prior tick,
            // which recorded its config) — but a missing snapshot must mean "no
            // residue", never a fade under the WRONG config.
            return;
        };
        let mut ghost = Box::<CursorGlow>::default();
        // MOVE the forged light: the ghost owns it now; the live engine starts
        // the incoming style with empty collections (plus carried warmth).
        ghost.sparks = std::mem::take(&mut self.sparks);
        ghost.particles = std::mem::take(&mut self.particles);
        // The landing starburst is forged light like the ZOOM streak: it finishes
        // its bloom under the OUTGOING (rainbow) config in the ghost — the
        // incoming style never births one, so re-judging it there would hard-cut
        // it mid-bloom.
        // A fast glide's streak IS a `rainbow.jumps` entry since step 13, so it
        // is handed to the ghost by the line above with every other ZOOM — one
        // mark, one crossfade rule. Only the velocity spine still needs saying.
        ghost.fire_meteors = std::mem::take(&mut self.fire_meteors);
        ghost.vapor = std::mem::take(&mut self.vapor);
        ghost.bolts = std::mem::take(&mut self.bolts);
        ghost.ring = self.ring.take();
        ghost.crown_until = self.crown_until.take();
        ghost.crown_window_ms = self.crown_window_ms;
        ghost.last_move = self.last_move;
        ghost.last_smoke = self.last_smoke.take();
        // Fresh-ink pops are forged light exactly like the sparks: they finish
        // fading under the OUTGOING (rainbow) config in the ghost — re-judging
        // them under the incoming style would either drop them mid-pop (a hard
        // cut) or render them through a style that never births pops.
        ghost.reduced_motion = self.reduced_motion;
        // COPY the thermal/phase scalars: the residue keeps rendering at the
        // warmth it was forged with, while the live engine KEEPS heat / flare /
        // coal / quench and the eased display temperatures too — that carried
        // warmth is what the incoming style's emitters read on their first
        // frame, so mid-typing switches arrive already glowing.
        ghost.heat = self.heat;
        ghost.heat_at = self.heat_at;
        ghost.flare = self.flare;
        ghost.coal = self.coal;
        ghost.quench = self.quench;
        ghost.disp_t = self.disp_t;
        ghost.cursor_temp = self.cursor_temp;
        ghost.flame_phase = self.flame_phase;
        // The canonical metric is carried warmth exactly like `heat`: the
        // ghost's frozen copy keeps its residue rendering at the momentum it
        // was forged with, and the LIVE engine keeps its own copy so a
        // mid-typing switch into rainbow kitty re-attacks the spine toward the earned
        // value instead of restarting the earn from zero.
        ghost.momentum = self.momentum;
        // The living-flow surface carries across the fork and is NOT restarted
        // below with the style phases: the ghost's residue is the same light
        // this frame drew, so its swells have to be where they already were —
        // a fresh `0` would print the switch as a jump in the fading mark. It
        // is not a style phase at all (see [`RainbowState::flow`]): it is a
        // constant-rate surface clock the incoming style simply inherits.
        ghost.hue = self.hue;
        ghost.rng = self.rng;
        // The ghost's anchor: `tick_fades` feeds this SAME cell as `cur`
        // forever, so `cur == last` guarantees no move is ever observed (no
        // spawn), while positional emitters keep their footing. The input
        // hints deliberately do NOT copy — they classify FUTURE input, which
        // belongs to the live style.
        ghost.last = self.last;
        // Pin the ghost's own switch guard to its config so its ticks never
        // re-detect a change (and never nest another fade).
        ghost.last_style = Some(cfg.style);
        ghost.last_pack_fp = cfg.pack.as_ref().map_or(0, |p| p.pack_fp);
        ghost.last_cfg = Some(cfg);
        // Style-specific PHASES restart for the incoming style (the ghost took
        // the copies above): a fresh flame field / ribbon spine attacks from 0
        // over ~100 ms — the built-in half of the ramp-in.
        self.flame_phase = 0.0;
        // CLASSIFIER state, not forged light: the ghost already takes
        // `glide.stars`/`glide.vel`'s visible output, but a stale run direction
        // or landing anchor carried across a style switch would mint a landing
        // burst at a coordinate that no longer exists.
        if self.fading.len() >= Self::FADE_CAP {
            // Chained switches: the oldest fade is the darkest — shed it.
            self.fading.remove(0);
        }
        self.fading.push(OutgoingFade {
            anchor: self.last,
            engine: ghost,
            cfg,
            started: now,
            level0,
        });
    }

    /// The incoming style's ramp-IN amplitude at `now`: EXACTLY `1.0` in steady
    /// state (`ramp_in_at == None` — the no-switch path applies no scale at
    /// all, so built-ins stay byte-identical), else a cosine ease from
    /// [`Self::RAMP_IN_FLOOR`] to 1.0 over [`Self::RAMP_IN_S`], self-clearing
    /// once complete so steady state costs one `Option` read.
    fn ramp_in_scale(&mut self, now: Instant) -> f32 {
        let Some(t0) = self.ramp_in_at else {
            return 1.0;
        };
        let u = now.saturating_duration_since(t0).as_secs_f32() / Self::RAMP_IN_S;
        if u >= 1.0 {
            self.ramp_in_at = None;
            return 1.0;
        }
        let eased = 0.5 - 0.5 * (std::f32::consts::PI * u).cos();
        Self::RAMP_IN_FLOOR + (1.0 - Self::RAMP_IN_FLOOR) * eased
    }

    /// Tick every live [`OutgoingFade`] under its snapshotted OLD config with
    /// the cosine ramp-down applied to its intensity, and merge the emitted
    /// streams into this frame's output. A fade is dropped when its envelope
    /// is spent OR the moment it contributes nothing visible — so "a fade is
    /// live" always implies "the frame shows cursor-effect output", the no-gap
    /// invariant the switch strobe fix rests on. Ghost sound cues are cleared
    /// unconditionally (the residue never talks; sound cannot cross styles).
    fn tick_fades(&mut self, now: Instant, geom: Geom, out: &mut Vec<GlowQuad>) {
        if self.fading.is_empty() {
            return;
        }
        let mut fades = std::mem::take(&mut self.fading);
        let mut scratch = std::mem::take(&mut self.fade_scratch);
        fades.retain_mut(|f| {
            let u = now.saturating_duration_since(f.started).as_secs_f32() / Self::FADE_OUT_S;
            if u >= 1.0 {
                return false; // envelope spent
            }
            // Cosine ramp-down FROM the level the outgoing style actually held
            // (a chained switch hands off mid-ramp without popping to full).
            let env = f.level0 * (0.5 + 0.5 * (std::f32::consts::PI * u).cos());
            let mut c = f.cfg;
            c.intensity *= env;
            if c.intensity <= 0.0 {
                return false; // inert amplitude — ticking would only wipe
            }
            // `cur == engine.last` (the frozen anchor) ⇒ never a move ⇒ never a
            // spawn: the residue decays and drifts only, under its OLD config.
            f.engine.tick(f.anchor, now, &c, geom, &mut scratch);
            f.engine.sound_cues.clear();
            let contributed = !(scratch.is_empty()
                && f.engine.halo_out.is_empty()
                && f.engine.patch_out.is_empty()
                && f.engine.under_out.is_empty()
                && f.engine.char_out.is_empty()
                && f.engine.fire_halo_out.is_empty());
            out.extend_from_slice(&scratch);
            self.halo_out.extend_from_slice(&f.engine.halo_out);
            self.patch_out.extend_from_slice(&f.engine.patch_out);
            self.under_out.extend_from_slice(&f.engine.under_out);
            self.char_out.extend_from_slice(&f.engine.char_out);
            self.fire_halo_out
                .extend_from_slice(&f.engine.fire_halo_out);
            // A fade that painted NOTHING is already invisible — retire it now
            // rather than holding `is_active` armed for dark residue.
            contributed
        });
        self.fade_scratch = scratch;
        self.fading = fades;
    }

    /// Deterministic xorshift float in [0,1).
    fn frand(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

// THE EASED SEGMENT LOOKUP IS DELETED with the luma compensation it placed.
// `rainbow_gradient_segment` split `t` across six anchors and eased inside the
// pair — the shape of the OLD colour law, an sRGB lerp between adjacent anchors,
// whose green->blue leg is defect (b) of the design: `#1ACC80`, a hue that is not
// on the arc, at a value 20% under both of its own endpoints. Colour resolves
// through `spectrum` now, and the easing lives INSIDE the generated table (the
// anchors sit on exact indices with zero hue slope either side of them), so the
// arc is C¹ at every anchor and at both ends of the reflected sweep without any
// caller placing a companion scalar per band.

// THE FAMILY'S SPECTRUM AT ONE CELL is [`spectrum`] — full stop, at every cell,
// at every cadence. `rotate_hue` and `rainbow_momentum_bands` used to stand here
// and spin all six anchors by ±`RAINBOW_HUE_SWING · disp` around the wheel; both
// are deleted. See §2.3 of the design of record: rotation is the one mechanism
// that could move a named anchor into an unnamed window (blue 204° → 189.6°,
// cyan), it made the trail's colours a function of typing speed, and it charged
// six HSV round trips per swept cell per frame to do it. Every caller now
// resolves the ONE spectrum at a position, so "which rainbow is this?" has one
// answer everywhere in the file.

// THE MARK'S SHORT-AXIS COLOUR CROSSFADE IS DELETED (§2.1). It resolved a cell's
// colour as `spectrum(hue_t + (tv - hue_t) * bloom)` — the laid position blended,
// by an amount that grew with MOMENTUM and with nearness to the head, toward
// where the sample sat DOWN the cell. So a cell's position depended on which
// pixel of it you asked AND on how fast the user was typing: two of the five
// laws §2.1 collapses, inside one expression. The head, where `bloom` is
// largest, drifted furthest — its dominant band could sit two named stops from
// the cell beside it while both were drawing the same keystroke's light.
//
// A cell has ONE position now and every layer reads it, so there is nothing left
// to crossfade between.

#[cfg(test)]
mod tests;
