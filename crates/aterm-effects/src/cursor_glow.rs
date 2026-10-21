// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cursor aurora: pure host-side animation that turns cursor motion into
//! emitted light: a comet of premultiplied additive quads along the swept
//! path, a soft bloom crown around the head, and (per style) a particle
//! system of sparkles or rising embers. The renderer is a deterministic
//! compositor of [`GlowQuad`]s; the art lives here.
//!
//! Every emitted quad is premultiplied ([`aterm_render::premul_rgb`]),
//! single-cell-row and clamped to the grid interior, which keeps the additive
//! light byte-exact across the Metal and CPU backends and the row-scoped damage
//! gate exact. The clock is an injected [`Instant`], so the effect is testable
//! without sleeping, and it decays to exactly empty so the event loop idles.

use aterm_time::Instant;
use std::time::Duration;

use aterm_hash::{FNV64_OFFSET, fnv_mix_u64};
use aterm_render::{
    BEAM_LAYERS, BeamClip, BeamScratch, BeamVertex, COMET_LAYERS, CharFg, CometSample,
    FireHaloCell, FireMode, FirePatch, GlowQuad, HaloMode, LASER_LAYERS, PHASER_LAYERS, RainHalo,
    SPARKLE_LAYERS, comet_beam, comet_beam_over, layer_stack_lift, layered_beam_quads_with,
    premul_rgb,
};
// The profile is the rasterizer's, and the ribbon reaches it only through
// [`ribbon_beam`]: one law, evaluated in one place (the smoothness oracle
// derives its bound from the profile's Lipschitz constant).
//
// The jump streak uses it too. `ribbon_beam` is an x-major rasterizer (a
// [`RibbonVertex`] is an `x` and a `spine`) while a jump runs at any angle, so
// the streak builds its cross-section from abutting [`comet_beam`] tubes and
// takes each one's weight from this function.

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
    STAR_ARM_FINE, STAR_ARM_INK, STAR_ARM_STD, STAR_CORE, STAR_GLINT, STAR_GLINT_COV,
    TWINKLE_FLASH_BOUND_HZ, dust_r, fire_ramp, lerp_rgb, push_fx_quad, push_fx_rect as push_rect,
    push_twinkle_star, star_accent, star_arm, star_arm_px, twinkle_env, twinkle_peak, water_ramp,
};
// The one colour law (`docs/design/RAINBOW-KITTY-V2.md` §2.3, C1): every mark
// of the rainbow family resolves colour through `spectrum` and every point-mark
// snaps to a name through `spectrum_snap`.
use crate::spectrum::{SPECTRUM_STOPS, spectrum, spectrum_snap};
use crate::trail_sweep::line_cells_tail;
use crate::typing_momentum::TypingMomentum;

// The engine's parts. A part with public items is re-exported whole, so every
// `cursor_glow::X` path resolves at its visibility here; parts with none are
// `impl CursorGlow` blocks.
mod bed;
mod companion;
mod custody;
mod emit;
mod ink_ledger;
mod light_body;
mod probe;
mod raster;
mod scroll;
mod seam;
mod sound;
mod spawn;
// The `cursor_trail_style` vocabulary, public as a module: hosts read
// `cursor_glow::spellings::canonical` and its tables by path.
pub mod spellings;
mod status;
mod style;
mod thermal;
pub(crate) use bed::{
    BedScratch, BedShape, clip_quads_to, dark_bed_budget, dark_bed_ink, lay_dark_bed,
    split_air_runs,
};
pub use companion::*;
pub use custody::*;
use ink_ledger::BandLifts;
pub(crate) use ink_ledger::InkLedger;
pub use ink_ledger::OverInkTier;
pub(crate) use light_body::*;
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
    /// Fade lifetime in seconds: short for a lone single-cell typing advance
    /// (a tight wake that leads), chained to the inter-key cadence during
    /// sustained typing (so the streak never goes dark between keys, see
    /// `CHAIN_GAP_MAX`), and the full comet `duration` for a jump.
    life: f32,
    /// Typing spark (single-cell advance)? Typing sparks hold full brightness
    /// for 55% of life then cosine-fade; jump sparks fade linearly.
    typing: bool,
    /// The spectrum hue (turns) this cell's light was laid in. The Phaser band
    /// renders each cell in its laid hue, leaving a spatial rainbow; light never
    /// re-colours after leaving the emitter (re-sampling per frame collapses
    /// the band into one flickering bar). Other styles ignore it.
    hue: f32,
    born: Instant,
}

/// One deduplicated Water wake sample for the current frame. This is render
/// scratch, not animation state: the surface and its late falling bead are
/// derived entirely from the owning [`Spark`]'s age on every tick.
#[derive(Clone, Copy)]
struct WaterSample {
    surface: BeamVertex,
    /// The swell at this sample, so the curve between two samples is the
    /// same swell, not a straight chord: its phase (radians), its height
    /// (px), and the waterline it rides (window px).
    phase: f32,
    amp: f32,
    line_y: f32,
    row: u16,
    col: u16,
    drip_cov: u8,
    drip_x: f32,
    drip_y: f32,
    drip_h: f32,
}

/// One light particle (spark / ember), advanced analytically from `born`.
#[derive(Clone, Copy)]
struct Particle {
    x0: f32,
    y0: f32, // window-absolute px at birth (producers add `Geom::origin` once)
    vx: f32,
    vy: f32,   // px/sec
    gy: f32,   // gravity px/sec^2 (down +, up -)
    life: f32, // seconds
    hue: f32,  // 0..1 (sparkle), or warmth seed (fire)
    /// Birth-coverage scale (1.0 = full). Laser ablation showers divide their
    /// light across the burst (`1/√burst`) because every spark is born at the
    /// same impact point, where a saturating pile would clamp to a white blob.
    /// Styles with birth spread keep 1.0.
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

/// One fire meteor: the fire style's inter-line / long-jump streak, a straight
/// pixel-space line of fire along the true jump vector (never the swept cell
/// path) whose tail retracts into the landing. The landing flare slams at
/// arrival (`arrived`), not launch.
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

/// What a [`Vapor`] puff is: palette and growth-curve family. `Steam`/`Smoke`
/// are the fire style's thermal language and render only under that style;
/// `Poof` is the style-agnostic erase puff ([`CursorGlow::note_kill`]), so
/// [`CursorGlow::emit_vapor`] gates per particle rather than per style.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VaporKind {
    Steam,
    Smoke,
    Poof,
}

/// Which noise one erase poof makes: the key that licensed it, carried to
/// [`CursorGlow::spawn_poof`] so the cue is chosen where the poof is spawned
/// (see the sound note there).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PoofVoice {
    /// A line-scale kill chord (^U/^K): the
    /// [`crate::trail_sound::SoundKind::Kill`] swoosh, the erase's whole voice.
    Swoosh,
    /// A word-scale kill chord (^W, Alt-D, Alt/Ctrl-Backspace,
    /// [`CursorGlow::note_word_kill`]): the
    /// [`crate::trail_sound::SoundKind::KillWord`] word poof.
    WordPoof,
    /// A plain Backspace: the tiny [`crate::trail_sound::SoundKind::Poof`]
    /// breath of the erase poof's dispersal.
    Puff,
}

/// One vapor puff: Emberforge steam (quench) or smoke (a hot cursor), or the
/// erase poof. Analytic like [`Particle`] but rendered as a growing, thinning
/// radial halo. Steam expands fast, rises with a decelerating kick and dies
/// young; smoke drifts up on buoyancy with a waft and lingers for seconds; a
/// poof is a brief neutral-grey puff.
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

/// One lightning bolt: a jagged midpoint-displaced polyline (window pixels)
/// frozen at spawn so the channel holds its shape; only brightness animates,
/// one flash per bolt ([`bolt_flash`]: born at full strength, then a monotone
/// burn-out). `branch` forks are thinner and dimmer.
struct Bolt {
    /// Jagged channel points, origin → tip.
    pts: Vec<(f32, f32)>,
    born: Instant,
    life: f32,
    /// Branch fork: thinner, dimmer, no white-hot core.
    branch: bool,
    /// A typing crackle (a stray arc off the charged trail, over the newest
    /// letters), not a jump strike or one of its forks: on a dark ground it
    /// spends the streak's over-ink budget.
    crackle: bool,
}

/// One outgoing style crossfade: the complete animated state live when the
/// style switched, still ticking under its snapshotted old config
/// behind a cosine ramp-down (see the style-switch contract in
/// [`CursorGlow::tick`]). The residue keeps rendering under the config it was
/// forged under, so the whole animator moves here.
struct OutgoingFade {
    /// The moved-out animator: collections moved, thermal and phase scalars
    /// copied. Its own `fading` list is empty, so fade ticking is one level
    /// deep.
    engine: Box<CursorGlow>,
    /// The outgoing config (style/colours/length), frozen at the switch.
    cfg: GlowConfig,
    /// The cursor cell frozen at the switch, fed as `cur` to every fade tick:
    /// `cur == engine.last`, so no move is observed and the residue only decays
    /// and drifts, while positional emitters (a flame body, the crown) keep
    /// their anchor.
    anchor: Option<(u16, u16)>,
    /// Ramp-down envelope start ([`CursorGlow::FADE_OUT_S`]).
    started: Instant,
    /// The amplitude the outgoing style held at the switch (its own ramp-in
    /// level on a rapid re-switch), so chained switches never pop to full.
    level0: f32,
}

/// The two ping-pong buffers [`CursorGlow::spawn_bolt`]'s midpoint-displacement
/// jag swaps between: the polyline so far, and the one being built from it.
type BoltJag = (Vec<(f32, f32)>, Vec<(f32, f32)>);

/// Per-window aurora animation state.
#[derive(Default)]
pub struct CursorGlow {
    /// The classic wake engine, reached by the one `GlowStyle::Classic` branch
    /// in [`Self::tick`]. Inline because it is a few Vecs and stays empty for
    /// every other style.
    classic: crate::classic_wake::ClassicWake,
    sparks: Vec<Spark>,
    /// Admission diagnosis ring: the last [`ADMISSION_LOG_CAP`] content-candidate
    /// lifecycle events, served by the `trail` control verb. Diagnostic only;
    /// never read by a decision.
    admission_log: AdmissionLog,
    /// Cumulative count of moves [`Self::spawn`] admitted (a witnessed move that
    /// laid light). A `confirmed`-heavy [`AdmissionTally`] with `spawns == 0`
    /// means admission and morphology disagree. Diagnostic only, and it
    /// survives every transient teardown.
    spawns: u64,
    /// Resident, bounded swept-cell scratch. Built backward from the landing
    /// point so an outlier cursor jump never walks or allocates the discarded prefix.
    path_scratch: Vec<(i32, i32)>,
    particles: Vec<Particle>,
    ring: Option<Ring>,
    /// The cell height (device px) of the last [`Self::tick`], so
    /// [`Self::note_scroll`] (called with a row count only) can translate the
    /// pixel-addressed light pools. Zero before the first tick, in which case
    /// the pools are dropped.
    last_ch: u16,
    /// The grid-interior top Y (window px) of the last [`Self::tick`]: a
    /// row-band move ([`Self::note_band_move`]) arrives as row numbers, and the
    /// band's pixel span is `origin_y + top·ch .. origin_y + (bottom+1)·ch`,
    /// so chrome above the grid must be accounted for. Zero before the first
    /// tick, in which case the pools are dropped.
    last_origin_y: u16,
    /// How many row-band moves the host reported ([`Self::note_band_move`]):
    /// `trail status`'s `band_moves=`, one per band replayed. A counter only.
    coord_band_moves: u64,
    /// The press-credit ring: the typed-echo coalescer's ledger, one slot per
    /// keyed glyph (`note_typed_glyph`), spent by `classify_move` when a batched
    /// same-row echo is proven typing (`rainbow_coalesce`). Style-agnostic.
    type_press_ring: PressCredits,
    last_committed_type: Option<Instant>,
    /// The family metric ([`crate::typing_momentum`]): the one integrator the
    /// host's companions, the sing-along and v2's spine read
    /// (`RAINBOW-KITTY-V2.md` §17.1).
    momentum: TypingMomentum,
    /// The correlated typing advance the host's cursor cat mirrors
    /// ([`Self::take_cursor_cat_motion_pulse`]); set in `spawn`, cleared at
    /// the top of every tick.
    momentum_pulse: Option<CursorCatMotionPulse>,
    /// Live fire meteors (≤ [`Self::METEOR_CAP`]) — the fire style's jump
    /// streaks; see [`Meteor`].
    fire_meteors: Vec<Meteor>,
    /// Live vapor (steam + smoke) puffs, rendered as radial halos. Bounded by
    /// [`Self::MAX_VAPOR`].
    vapor: Vec<Vapor>,
    /// When the hot cursor last shed a smoke wisp (rate limiter).
    last_smoke: Option<Instant>,
    /// This frame's radial light ([`RainHalo`]s: fire embers, crown, impact
    /// flash), rebuilt by every [`Self::tick`] beside `out` and consumed into
    /// `RenderInput.glow_halo`. Soft round light with integer elliptical
    /// falloff. Bounded by [`Self::MAX_HALOS`].
    halo_out: Vec<RainHalo>,
    /// This frame's per-pixel fire: [`FirePatch`]es evaluated at every device
    /// pixel by the shared integer field (CPU sampler and WGSL twin,
    /// byte-exact), the flame body. Drawn at the under-ink seam so the dark
    /// cores work. Dark themes only (Add); a light theme's fire is ink
    /// tongues in the under-ink stream ([`Self::emit_flames_light`]).
    patch_out: Vec<FirePatch>,
    /// This frame's under-ink light (the flame body), drawn between cell
    /// backgrounds and glyph ink (`RenderInput.glow_under`) so engulfed letters
    /// read as silhouettes inside the flame. Tips, embers, beam and crown stay
    /// in `out`/`halo_out`, above the ink.
    under_out: Vec<GlowQuad>,
    /// This frame's charred ink: per-cell final-fg overrides for engulfed glyphs
    /// (`RenderInput.char_fg`), darkest inside the flame, scaled by engulfment.
    char_out: Vec<CharFg>,
    /// This frame's contrast-halo strengths: per-cell engulfment weights
    /// (`RenderInput.fire_halo`) driving the dark dilation ring the renderers
    /// stamp under each engulfed glyph's ink (the ink itself never recolours).
    fire_halo_out: Vec<FireHaloCell>,
    /// Per-cell engulfment accumulator scratch (row, col, weight), rebuilt by
    /// the flame-field emitter each frame; tiny (≤ ~36 cells).
    engulf_scratch: Vec<(u16, u16, f32)>,
    /// Live lightning bolts (Laser style; ≤ [`Self::MAX_BOLTS`]).
    bolts: Vec<Bolt>,
    /// Host-fed reduced-motion arm for the fresh-ink pop: the pop renders as a
    /// brightness step-fade only (no scale spring, no birth halo). The GUI's
    /// motion policy zeroes the whole aurora under Reduced, so this is the
    /// engine-level contract for hosts running reduced dynamics at nonzero
    /// intensity, like the sparkle/rain reduced twins.
    reduced_motion: bool,
    /// Last observed cursor cell (terminal coords), to detect a move.
    last: Option<(u16, u16)>,
    /// The last visible cursor cell and when, for the ConPTY hide bridge
    /// (mirroring [`crate::cursor_trail::CursorTrail`]): conhost hides the
    /// cursor briefly during every keystroke-echo repaint, which would drop
    /// the glow's spark for most keys. See
    /// [`crate::cursor_trail::HIDE_BRIDGE_MS`].
    last_visible: Option<((u16, u16), Instant)>,
    /// The hide-bridge estimate's witness: the cell the caret was last
    /// actually observed at, kept only while [`Self::last_visible`] holds the
    /// anchored lane's relocation estimate (`echo_anchor_pass`, Rainbow Kitty)
    /// and `None` when it is an observation. A show exactly here, left of the
    /// estimate on its row, is no move (`tick`). Rides scroll and band moves
    /// with `last_visible`.
    hide_bridge_shown: Option<(u16, u16)>,
    /// The echo anchor (host-fed, [`Self::observe_print_anchor`]): where the
    /// terminal's latest PTY print run ended, `(row, col, seq)` in `tick`'s
    /// grid coordinates, with `seq` advancing on every print so an unchanged
    /// position still reads as new output. The only truthful sensor for a TUI
    /// whose repaint leaves the cursor hidden or parked away from the caret
    /// (CUP 1;1 before show). See [`Self::echo_anchor_pass`].
    print_anchor: Option<(u16, u16, u64)>,
    /// The echo anchor's glyph (host-fed, [`Self::observe_print_anchor_glyph`]):
    /// the terminal's content at the print run's last cell, in the row probe's
    /// per-column convention, sampled under the anchor's lock. `None` when the
    /// host gave no glyph; the visible-parked glyph gate in
    /// [`Self::echo_anchor_pass`] then reads only the caret row's probes.
    print_anchor_glyph: Option<char>,
    /// The caret handed over this frame is not drawn (host-fed,
    /// [`Self::observe_caret_drawn`]): DECTCEM hides it. Only the single-pane
    /// window hands a hidden caret to [`Self::tick`] (for the pet); other hosts
    /// hand `None`, so the default is drawn. Read only by the minibuffer gate
    /// in [`Self::echo_anchor_pass`].
    caret_hidden: bool,
    /// The print-anchor `seq` already consumed by [`Self::echo_anchor_pass`],
    /// so one host sample is judged exactly once.
    print_anchor_seen: u64,
    /// Per-row memory of where the last print run on that row ended (the launch
    /// cell for an anchored echo sweep: a whole-row TUI rewrite's end advances
    /// by exactly the typed cells) plus the program-row taint brand
    /// ([`AnchorRow`]). A small LRU, since TUIs interleave the input row with
    /// status rows.
    anchor_rows: [Option<AnchorRow>; ANCHOR_ROWS],
    /// The touch clock [`Self::anchor_rows`] evicts by, stamped into each
    /// written entry ([`AnchorRow::touched`]), so a new row takes the least
    /// recently printed row's slot.
    anchor_rows_clock: u64,
    /// The last licensed anchored echo sweep, `(row, at)`: the spoken-for arm
    /// of row discrimination. While younger than [`Self::TYPE_HINT_FRESH`], no
    /// other row's advance may consume a stamp. The taint brand cannot refuse a
    /// program row's first advance (a counter going `(9)` → `(10)`), but the
    /// input row re-lights on every echo during a burst. Cleared with the
    /// anchor-row memory on reset or scroll.
    last_anchor_sweep: Option<(u16, Instant)>,
    /// The paid pending-wrap cell: the row whose pane-edge glyph the anchored
    /// lane licensed as its own print (`wrap_parked`: print anchor one past a
    /// caret parked on the pane's last column), with no caret move since. The
    /// following fold carries only the landing row's head, so the fold shapes
    /// and spend take `cc_local`, not `cc_local + 1`. One-shot: the next
    /// observed caret move consumes it; it rides scroll and band moves.
    wrap_paid_row: Option<u16>,
    /// When the cursor last moved (drives the bloom-crown fade around the head).
    last_move: Option<Instant>,
    /// Deadline until which the bloom crown is still emitted (cached so
    /// [`is_active`] needs no clock); keeps the timer armed past comet decay.
    crown_until: Option<Instant>,
    /// The crown window of the last move: [`Self::CROWN_TYPING_MS`] for a
    /// single-cell typing advance, [`Self::CROWN_MS`] for a jump. Read by
    /// `emit_crown`.
    crown_window_ms: u64,
    /// The [`GlowStyle`] of the last tick (`None` before the first). In-flight
    /// light was forged under the old style's geometry and colours, so when the
    /// live style changes `tick` moves it into an [`OutgoingFade`], which keeps
    /// rendering it under its old config behind a ramp-down, while coordinate
    /// tracking and typing warmth stay live.
    last_style: Option<GlowStyle>,
    /// Whether the last tick's classic wake wore its mono face, whose bed and
    /// over-ink account read the rows flanking the caret's
    /// ([`Self::reads_neighbor_rows`]).
    last_classic_mono: bool,
    /// Rolling hue phase (turns) for rainbow/sparkle, advanced per spawn.
    hue: f32,
    /// Typing-cadence heat 0..1: sustained fast typing brightens and lengthens
    /// the wake; a pause cools it. Decayed lazily from `heat_at`, so idle costs
    /// nothing.
    heat: f32,
    /// When `heat` was last decayed (the lazy-decay stamp).
    heat_at: Option<Instant>,
    /// Jump flare 0..1: slammed to full by a real jump, cooling fast
    /// ([`Self::FLARE_DECAY_TAU`]). Fire's blaze is `heat.max(flare)`, so a long
    /// leap ignites white-hot from a cold keyboard and cools through orange to
    /// red.
    flare: f32,
    /// Emberforge coal bed 0..1: the slow thermal integrator under the fast
    /// heat. Forward keystrokes at any human cadence charge it and it cools over
    /// seconds ([`Self::COAL_TAU`]), so relaxed typing keeps an ember floor
    /// across pauses and full blaze takes sustained forward momentum.
    coal: f32,
    /// Backspace quench meter 0..1: each delete adds [`Self::QUENCH_GAIN`]
    /// (decaying on [`Self::QUENCH_TAU`]) and damps the display temperature, so
    /// a backspace run reads as a quench. Fed at the key edge by
    /// [`Self::note_backspace`].
    quench: f32,
    /// A fresh Backspace classifier ([`Self::note_backspace`]). It can classify
    /// an already confirmed one-cell candidate as a deletion but never admits a
    /// move by itself. Expires after [`Self::QUENCH_HINT_FRESH`].
    quench_hint: Option<Instant>,
    /// The last Backspace and what its echo still owes ([`EraseWitness`]).
    last_erase: Option<EraseWitness>,
    /// A fresh navigation classifier ([`Self::note_navigation`]). It never
    /// admits a cursor delta; it suppresses an older ambiguous class so
    /// navigation cannot turn a later child relocation into heat or light.
    /// Expires after [`Self::NAV_HINT_FRESH`]; never cools heat or coal.
    nav_hint: Option<Instant>,
    /// Fresh typed-glyph key hints ([`Self::note_typed`]): armed only for plain
    /// Character/Space echo keys, never Enter, Tab, navigation or modified
    /// chords. With a confirmed exact candidate
    /// ([`Self::note_typed_expected`]), a hint classifies a one-row delta beyond
    /// the typed advance as a typed re-anchor (see `spawn`); the timestamp alone
    /// admits nothing. Each stamp expires after [`Self::TYPE_HINT_FRESH`] and is
    /// consumed by exactly one echo sweep. Banked ([`TypedStamps`]), so K
    /// presses in one frame gap license K sweeps.
    type_hint: TypedStamps,
    /// A fresh Return hint ([`Self::note_return`], main-screen Enter): a bounded
    /// morphology signal only, never movement provenance.
    return_hint: Option<Instant>,
    /// A fresh user-gesture classifier for cursor-moving input whose echo is
    /// not a typed glyph: a Tab's cross-row completion, a scripted preview, the
    /// classic trail's twin of an insert. v2 receives it as
    /// `Licence::Synthetic` (wake and meteor, not a walk); a delivered insert
    /// has its own class ([`InsertSeam`]). Controller, mouse and wheel reports
    /// cannot prove which PTY move they caused, so hosts cancel those.
    user_gesture_hint: Option<Instant>,
    /// The delivered-insert seam ([`InsertSeam`]): the licence, its undo, the
    /// span laid, the refused hop one delivery may retro-license, the presses
    /// an unknown width's hop echoed, and `trail status`'s insert rows.
    insert: InsertSeam,
    /// The in-flight rows of `trail status` ([`InFlightTally`]).
    in_flight_tally: InFlightTally,
    /// A same-row backward typed-paired move held for its return
    /// ([`HeldPark`]). Flushed by every edge that would have judged it.
    held_park: Option<HeldPark>,
    /// The last typed lay, until the caret rests ([`TornLay`]).
    torn_lay: Option<TornLay>,
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
    /// The row and clock of the last licensed move: the insert's row witness
    /// ([`Self::insert_row_identity`]). A row licensed within the chain window
    /// may spend a partial insert echo; any other row needs the whole width.
    last_licensed_row: Option<(u16, Instant)>,
    /// The hand's row: where the last licensed typed echo landed (stamp- or
    /// pool-paired). Line-editing presses and modifier edges keep it
    /// ([`Self::clear_typed_keeping_hand`]); every other press drops it
    /// ([`Self::clear_typed`], and [`Self::note_return`]'s Enter into a shell;
    /// an Enter into a composer keeps it, [`Self::note_return_keeping_hand`]),
    /// as do reset and teardown. A scroll or band move carries it with the row,
    /// or with the caret when the hand typed at it ([`HandRow::caret_row`]).
    /// The parked-caret arm of [`Self::echo_anchor_pass`] reads it: while the
    /// drawn caret stands on this row, advances on other rows are a program's
    /// and forget nothing, and presses kept by a refusal below the caret are
    /// held to this row ([`PressCredits::held_through`]).
    hand_row: Option<HandRow>,
    /// The caret row at a generic input boundary, and whether a later sample
    /// proved it parked away from the output. Cold program setup has no such
    /// boundary. A parked, unowned row cannot acquire typing ownership from
    /// a redisplay's matching glyph; an established hand keeps its lane.
    unowned_caret: Option<UnownedCaret>,
    /// A fresh composer-newline hint ([`Self::note_newline_break`]: an
    /// alt-screen Shift+Enter, bound by agent composers to a line break without
    /// submitting).
    ///
    /// Geometry cannot carry it. [`Self::retire_abandoned_light`] retires the
    /// previous row's ribbon on an unexplained typing-classified row change (a
    /// repaint moved the input line). A composer newline makes the identical
    /// move, one row down to the box's inset, but the row it leaves still holds
    /// the user's text.
    ///
    /// A licence term ([`Self::one_shot_licensed`], the park refusal) consumed
    /// once at the spawn seam, like the Return's: the composer's own row change
    /// takes it (`Licence::Return`). Expires after [`Self::RETURN_HINT_FRESH`].
    newline_hint: Option<Instant>,
    /// A fresh reflow classifier. Coordinate changes reset both engines, so
    /// reflow is dark and this never admits a movement birth.
    reflow_hint: Option<Instant>,
    /// A fresh kill key hint ([`Self::note_kill`]: Ctrl-K/U/W, Alt-D,
    /// word-delete Backspace, forward Delete), the licence for the erase poof.
    /// A poof needs both this hint within [`Self::KILL_HINT_FRESH`] and a
    /// same-row net shrink of the probed row (`poof_scan`); neither alone
    /// poofs.
    kill_hint: Option<Instant>,
    /// The fresh kill hint's scale: `true` iff [`Self::note_word_kill`] armed it
    /// (one word), `false` for line-scale chords (^K/^U). Read once, where the
    /// poof chooses its voice ([`PoofVoice`]); meaningless without a fresh
    /// `kill_hint`, and re-stamped by every `note_kill`.
    kill_hint_word: bool,
    /// The instant of a caret-moving kill whose own retreat is still owed:
    /// stamped by [`Self::note_kill`] with `moves_cursor`, spent by the first
    /// nav-shaped move `spawn` judges within [`Self::KILL_HINT_FRESH`], and
    /// superseded by a real navigation press ([`Self::note_motion`]). The
    /// retreat is one move; without this, a kill whose erase was never
    /// witnessed would swallow the next word move. It has its own clock
    /// because [`Self::kill_hint`] is also the poof's row proof, which
    /// `drop_row_probe` clears after every band move and scroll.
    kill_retreat_pending: Option<Instant>,
    /// The kill (by its key stamp) whose `rk::Event::Kill` has already gone to
    /// v2, from whichever witness came first: the retreat (so the ribbon takes
    /// the erase-retreat arm) or the poof that proves the span. One kill, one
    /// `Kill`: a witness for an already-reported kill stands down. A stationary
    /// ^K / Alt-D never moves the caret, so its poof is the first witness.
    kill_reported_to_v2: Option<Instant>,
    /// A fresh plain-Backspace poof licence ([`Self::note_backspace`]), separate
    /// from [`Self::kill_hint`] so it never borrows the kill's nav/quench
    /// semantics. `poof_scan` ORs it into the `fresh_kill` gate, so a Backspace
    /// poofs on the same full-trust proof surface as the kill chords (one that
    /// erased nothing stays silent, see `bs_only`; a ContentOnly probe licenses
    /// no poof, [`ProbeTrust`]). Independent of the cursor cat.
    bs_poof_hint: Option<Instant>,
    /// The cursor row's fill when the erase key arrived, `(row, fill)`, stamped
    /// by [`Self::note_backspace`] and [`Self::note_kill`] from the host's last
    /// probe.
    ///
    /// A stamp rather than the live diff: `poof_scan`'s span branch diffs the
    /// previous presented row against this frame's, but on a quiet screen the
    /// first probe after a lone Backspace is already post-erase, so every diff
    /// would compare post with post. Also the clock the caret fallback's grace
    /// can be released early against ([`Self::poof_erase_witnessed`]).
    bs_baseline: Option<(u16, u16)>,
    /// A fresh repaint-blink hint ([`Self::note_repaint_blink`]): the app's
    /// DECTCEM-hide-inside-DEC-2026 repaint bracket (the terminal's
    /// `repaint_blink_epoch` advanced; Claude Code wraps every keystroke's
    /// redraw in one). On the alt screen the typed/Backspace re-anchor also
    /// requires it fresh ([`Self::BLINK_HINT_FRESH`]). It only tunes admitted
    /// candidate morphology; the main screen ignores it.
    blink_hint: Option<Instant>,
    /// Per-frame alt-screen context ([`Self::note_context`], stamped beside the
    /// row probe). Gates the re-anchor's blink requirement; `false` (main
    /// screen or unwired host) leaves the classifier unchanged.
    ctx_alt: bool,
    /// Focused pane columns in the window-coordinate space consumed by this
    /// engine: `(first_col, width)`. The host refreshes it before every tick so
    /// a right-hand split's margin is classified against its pane, not against
    /// the whole window. `None` falls back to [`Geom::cols`] for embedders.
    pane_columns: Option<(u16, u16)>,
    /// When the last erase poof fired — the rate limiter ([`Self::POOF_MIN_GAP`])
    /// so a held kill-key repeat billows at a readable cadence, not per frame.
    last_poof: Option<Instant>,
    /// The fresh-typed glyph tint's palette, memoized on the
    /// `(theme_fg, theme_bg)` it was derived from. [`fresh_ink_glyph_palette`]
    /// runs a few hundred gamma evaluations that change only with the theme.
    /// Refreshed in [`Self::tick`]; the `&self` emitter reads it.
    /// `Some((key, None))` memoizes a refusal (a theme with no usable side).
    glyph_palette: Option<GlyphPaletteMemo>,
    /// Erase momentum: the mirror of the typing metric for deletion. The same
    /// law (a [`TypingMomentum`], the same rate-normalized accrual and 2 s
    /// decay), fed only by erase edges ([`Self::note_backspace`],
    /// [`Self::note_kill`]), so a held Backspace run builds like a typing run
    /// and a single correction stays a small puff (owner, 2026-08-06).
    ///
    /// Independent of [`RainbowState::momentum`]: forward momentum still builds
    /// only from forward typing and drains on a delete.
    erase_mom: TypingMomentum,
    /// Sound cues recorded at the spawn/poof edges this frame, drained by the
    /// host each tick ([`Self::drain_sound_cues`]) into the trail synth
    /// ([`crate::trail_sound::TrailSynth`]). Bounded
    /// ([`Self::MAX_SOUND_CUES`]) and cleared on every disable/reset path.
    sound_cues: Vec<SoundCue>,
    /// Key-time click credits: typing clicks already cued at the physical
    /// keypress ([`Self::cue_keystroke`]) whose echo is not yet observed. The
    /// echo path consumes one per typing-classified spawn and stays silent, so
    /// each character clicks once. Capped at [`Self::MAX_KEYED_CLICKS`] so
    /// swallowed keys (a password prompt, vim normal mode) cannot bank
    /// unbounded silence.
    keyed_clicks: u8,
    /// When the newest key-time click was cued: the credits' freshness anchor.
    /// Keys inside one [`Self::KEYED_CLICK_FRESH`] window keep all credits
    /// alive, so a batched echo run stays silent; past the window the ledger
    /// zeroes.
    keyed_click_at: Option<Instant>,
    /// When the previous key-time click was cued: the inter-key cadence clock
    /// for timbre prediction ([`Self::cue_keystroke`]). Separate from
    /// [`Self::keyed_click_at`], which zeroes whenever the ledger empties.
    /// Never cleared: a stale value only widens the gap, which earns no cadence
    /// credit, right for the first key of a burst.
    key_cue_at: Option<Instant>,
    /// The live style's per-keystroke heat gain and cooling τ, snapshotted
    /// ([`Self::store_key_timbre`]) by every [`Self::tick`] that may open the
    /// key seam (every tick past the master switch, including the classic wake
    /// and degenerate grids). Both are pure functions of the config, so a click
    /// minted during a load-shed frame reads this tick's timbre.
    /// [`Self::cue_keystroke`] runs from the host's key handler without a
    /// [`GlowConfig`], so it needs these to click at the heat the echo is about
    /// to reach. Read only behind [`Self::sound_live`], which is never set
    /// without storing these first.
    heat_gain_live: f32,
    heat_tau_live: f32,
    /// Whether the last [`Self::tick`] left the key seam open: it ran the live
    /// path, or went dark for any reason other than the master switch (motion
    /// policy, load shed, degenerate grid, classic wake's dark frame) while
    /// [`GlowConfig::audible`] said the key belongs to this window. One law,
    /// [`Self::settle_key_seam`], for every style. [`Self::cue_keystroke`]
    /// records nothing unless set.
    ///
    /// Sound is silenced only by gates about sound: the master switch and
    /// serious mode ([`GlowConfig::enabled`]) and unfocus (`audible = false`);
    /// the host's sound knobs gate before a cue is minted. False before the
    /// first tick.
    ///
    /// Read out of band by [`Self::sound_seam_open`] (`aterm ctl tone`'s
    /// `engine_sound=`, `aterm ctl trail status`'s `sound_seam=`), so changing
    /// either dark return changes a user-visible answer.
    sound_live: bool,
    /// This frame's cursor-row content probe ([`Self::observe_row`]), the newer
    /// half of the poof detector's double buffer. Per-column chars (lead char
    /// at its column, `'\0'` at wide continuations, `' '` for blanks) so the
    /// span math survives CJK and emoji. Reused capacity.
    row_cur: Vec<char>,
    /// The last-observed row probe (swapped from `row_cur` by `poof_scan`): the
    /// last-presented truth. [`Self::POOF_PROBE_STALE`] bounds staleness as a
    /// backstop.
    row_prev: Vec<char>,
    /// Metadata for `row_cur` (`None` ⇒ no fresh probe arrived this frame; the
    /// previous probe stays in place and the detector idles).
    row_cur_meta: Option<RowProbe>,
    /// Metadata for `row_prev`.
    row_prev_meta: Option<RowProbe>,
    /// The last caret-row probe that showed the caret, kept while the caret
    /// is withheld (a torn repaint probes the write cursor's row): a
    /// correction's repaint judged when the caret returns is compared against
    /// it ([`Self::merged_correction`]). Reused capacity.
    row_shown: Vec<char>,
    /// Metadata for `row_shown`; `None` while the caret shows.
    row_shown_meta: Option<RowProbe>,
    /// Exact glyphs of the last adjacent, licensed typed run. Kept outside
    /// the ribbon pool and deadline calculation after its visible cells
    /// retire; read only by the next typed echo on this row.
    recent_typed_run: Option<RecentTypedRun>,
    /// This frame's capture of the row above the probed cursor row
    /// ([`Self::observe_neighbor_rows`]), the content witnesses' newer half
    /// ([`NbrProbe`]), used by the soft-wrap and minibuffer witnesses. Same
    /// per-column convention as `row_cur`. Readable only in the
    /// [`NbrProbe::Probed`] state, so a skipped capture is never served stale.
    row_above_cur: Vec<char>,
    /// Last-presented capture of the row above (rotated from `row_above_cur`
    /// by `poof_scan`, in lockstep with `row_prev`).
    row_above_prev: Vec<char>,
    /// This frame's capture of the row below the probed cursor row.
    row_below_cur: Vec<char>,
    /// Last-presented capture of the row below.
    row_below_prev: Vec<char>,
    /// Eased display temperature 0..1, the one number every fire layer reads
    /// ([`Self::fire_t`]): it chases `max(heat, flare, coal floor)` damped by
    /// the quench, with a short attack and slower release, so the look ramps as
    /// one body. Fire-only (other styles use the raw [`Self::blaze`]).
    disp_t: f32,
    /// The flame field's integrated time phase (seconds, churn-weighted),
    /// advanced by dt·churn(T) in the lazy-decay block so the field speeds up
    /// smoothly as the fire builds and never resets mid-burn.
    flame_phase: f32,
    /// The cursor metal's temperature 0..1, hysteretic (heats over
    /// [`Self::TEMP_ATTACK_TAU`], cools over [`Self::TEMP_RELEASE_TAU`], 2×
    /// faster while quenched). Drives the block-cursor forge fill
    /// ([`Self::forge_fill`]). Fire-only; zero for other styles so its arm in
    /// [`Self::is_active`] is inert there.
    cursor_temp: f32,
    /// The forge fill's span of the ramp for the page the last tick saw
    /// ([`Self::refresh_forge_band`]); `None` before the first tick.
    forge_band: Option<thermal::ForgeBand>,
    /// Last typing advance (single-cell move), for the inter-key cadence.
    last_type: Option<Instant>,
    /// Deterministic spark/ember PRNG state.
    rng: u32,
    /// Swept-cell run builder reused each frame by [`Self::emit_comet`];
    /// `comet_run` is the run in progress.
    ///
    /// Inner buffers are pooled: each emitter clears entries in place and
    /// publishes a finished run by swapping it with the pooled entry at the
    /// live watermark, so `comet_run` always returns holding an allocated,
    /// empty buffer. Entries past `runs_len` are spares, so consumers read
    /// `&comet_runs[..runs_len]`. Bounded by runs ≤ sparks ≤
    /// [`Self::MAX_SPARKS`]. The emitter clears it on entry.
    comet_runs: Vec<Vec<CometSample>>,
    comet_run: Vec<CometSample>,
    /// Reused by [`Self::emit_water`] for the deduplicated curved wake spine
    /// and its analytically derived falling beads. Keeping this resident makes
    /// the fluid path allocation-free after its first growth.
    water_samples: Vec<WaterSample>,
    /// Fixed-size open-address owner table for [`Self::emit_water`], and for
    /// the light rails of the beam family ([`Self::emit_comet`]), which never
    /// run in the same frame; each emitter resets it on entry, and Water draws
    /// no beam. A
    /// repaint-heavy TUI can revisit the same cell hundreds of times before
    /// the old samples expire; newest-visible-wins here prevents those owners
    /// from compositing into an opaque reflection (or a veil past its cap).
    /// Fixed to twice the resident spark cap, so clearing it is
    /// O(MAX_SPARKS), never O(grid area).
    water_seen: Vec<u64>,
    /// Reused by [`Self::emit_water`]'s light arm for one run's polyline, so a
    /// light-ground water frame allocates nothing after the first growth.
    water_run: Vec<BeamVertex>,
    /// Reused by [`Self::emit_bolts`] (Laser strikes) for the per-bolt polyline,
    /// so a strike frame allocates nothing after the first growth.
    bolt_verts: Vec<BeamVertex>,
    /// Reused by [`Self::emit_bolts`] for each strike's channel resampled
    /// along its length (`(x, y, place, over text)`), so a strike frame
    /// allocates nothing after the first growth.
    bolt_channel: Vec<(f32, f32, f32, bool)>,
    /// Ping-pong scratch for [`Self::spawn_bolt`]'s midpoint-displacement
    /// rounds, so a strike allocates nothing. Taken out of `self` around the
    /// jag (the loop calls [`Self::frand`]) and handed back.
    bolt_jag: BoltJag,
    /// The layered beam rasterizer's working buffers (style colours, RDP
    /// work space and kept indices, per-layer vertices), reused by every
    /// [`Self::emit_comet`] arm so a streak frame allocates nothing after the
    /// first growth, and simplifies each run once for its whole layer stack.
    beam_scratch: BeamScratch,
    /// This frame's over-ink account over the probed text
    /// ([`InkLedger`]): Fire's lights on a dark ground spend one budget per
    /// text cell.
    ink_ledger: InkLedger,
    /// Test-only: draw the dark streak without its over-ink budget, and fire
    /// without its over-ink ledger and steam budget, the negative control of
    /// the contrast tests.
    #[cfg(test)]
    streak_unbudgeted: bool,
    /// Test-only: lay the dark bed in each stop at its full value instead of
    /// the luminance the bar allows, the negative control of the bed's bar.
    #[cfg(test)]
    bed_unsolved: bool,
    /// Test-only: keep the dark bed but no over-ink account, so every light
    /// over a letter lands whole: the negative control of the account.
    #[cfg(test)]
    over_ink_idle: bool,
    /// Test-only: spend every masked light of the over-ink account as one
    /// light, cut to its least-room cell's share everywhere (the account
    /// before it kept the light whole over blank cells), the negative
    /// control of the bloom-over-air test.
    #[cfg(test)]
    over_ink_uniform: bool,
    /// The last [`Self::BOLT_FLASHES_PER_S`] lightning onsets (strikes and
    /// crackles), a ring whose slot at [`Self::bolt_onset_next`] is the
    /// oldest: lightning's flash limiter ([`Self::admit_bolt`]).
    bolt_onsets: [Option<Instant>; Self::BOLT_FLASHES_PER_S],
    /// The [`Self::bolt_onsets`] slot the next onset overwrites.
    bolt_onset_next: usize,
    /// Test-only: forge every strike and crackle the dice ask for, the
    /// negative control of the lightning flash-rate test.
    #[cfg(test)]
    bolts_unlimited: bool,
    /// Test-only: stand the dark flame's tongues on one flat base (no curl
    /// at their feet), the negative control of the scalloped-feet test.
    #[cfg(test)]
    fire_flat_feet: bool,
    /// Test-only: straighten a one-row streak run into one chord, as every
    /// built-in but the phaser once did, the negative control of the typing
    /// streak's flash-rate test.
    #[cfg(test)]
    streak_chords_rows: bool,
    /// Reused by the light-ground body ([`Self::emit_comet_light`]) for its
    /// ribbon polyline, so a light frame allocates nothing after the first
    /// growth.
    light_verts: Vec<aterm_render::RibbonVertex>,
    /// The dark bed's working buffers ([`Self::emit_dark_bed`]), so a bed
    /// frame allocates nothing after the first growth.
    bed_scratch: BedScratch,
    /// The dark streak's beam runs over air ([`Self::emit_comet`]), split
    /// from the swept runs where they cross text; pooled as `comet_runs` is.
    air_runs: Vec<Vec<CometSample>>,
    /// A wedge bed's runs, laid by distance from each head
    /// ([`Self::emit_dark_bed`]); pooled as `comet_runs` is.
    wedge_runs: Vec<Vec<CometSample>>,
    /// The text of the rows this frame presents where the streak may run,
    /// from the frame's own cells ([`Self::observe_frame_text`]); read only
    /// by the frame it was handed for.
    frame_text: FrameText,
    /// Test-only: lay the light-ground bodies in their unsolved vivid ink
    /// ([`Self::light_ground`]) and lift the light fire's ceilings, the
    /// negative control of the light-body legibility tests.
    #[cfg(test)]
    light_body_unsolved: bool,
    /// Test-only: the centres of the ice glints the last light comet frame
    /// laid ([`Self::emit_comet_light`]).
    #[cfg(test)]
    light_glints: Vec<(f32, f32)>,
    /// Live outgoing style crossfades (≤ [`Self::FADE_CAP`], oldest dropped);
    /// see [`OutgoingFade`] and the style-switch contract in [`Self::tick`].
    /// Empty in steady state, where the tick pays one `is_empty` test.
    fading: Vec<OutgoingFade>,
    /// Resident quad scratch for the fade ticks (their `out` twin), so a
    /// crossfade frame allocates nothing after the first growth.
    fade_scratch: Vec<GlowQuad>,
    /// When the live style last arrived: drives the incoming
    /// [`Self::RAMP_IN_S`] ramp-in so the new style rises under the outgoing
    /// fade. `None` in steady state, where no scale is applied.
    ramp_in_at: Option<Instant>,
    /// The full config of the last tick, snapshotted into an [`OutgoingFade`]
    /// when the next tick detects a switch.
    last_cfg: Option<GlowConfig>,
    /// Latch: the `!cfg.enabled` teardown has run and nothing has dirtied wiped
    /// state since, so the dark tick skips re-issuing
    /// [`Self::clear_transient_state`] and [`Self::clear_thermals`] every frame
    /// (the pipeline gates on `enabled_any()`, not per engine). Dropped by every
    /// enabled tick, [`Self::reset`], and [`Self::unsettle`] at each entry that
    /// writes wiped state. Entries that only move state toward the wiped
    /// fixpoint (`clear_blink`, `clear_typed`, `drop_row_probe`, drains) keep
    /// it. `cue_keystroke` is inert here because the latch is set only by the
    /// master-off / degenerate-grid branch when it leaves the seam shut;
    /// `observe_neighbor_rows` is inert until an `observe_row` (which
    /// unsettles). Starts false, so the first dark tick runs one wipe.
    dark_settled: bool,
    /// The Rainbow Kitty engine (`RAINBOW-KITTY-V2.md` §17.2's seam object),
    /// which every `if self.v2.engaged()` delegates to (D14). Engaged at the
    /// top of [`Self::tick`] whenever the style is [`GlowStyle::RainbowKitty`];
    /// disengaged, each seam point is one cached-bool branch and the other
    /// styles run their own code.
    v2: rk::Engine,
    /// v2's polyline scratch ([`rk::Frame::beams`]): resident and reused, so
    /// the engine allocates nothing per frame (§18).
    v2_beams: Vec<BeamVertex>,
    /// The host row probe as v2's glyph-gate input (§5.4, seam point 13):
    /// resident, refilled from the same `cols` the v1 probe copies.
    v2_probe_scratch: Vec<rk::stardust::CellInk>,
    /// The content witness's rows for this frame ([`Self::observe_ribbon_row`],
    /// [`Self::capture_ribbon_row`], `rk::witness`): the grid rows the ribbon
    /// occupies, captured by the host under its terminal lock before the tick
    /// and read right after it. At most [`CURSOR_WITNESS_ROWS`] resident slots;
    /// `witness_rows_n` is zeroed by every tick so a stale sample is never read.
    witness_rows: Vec<WitnessRowBuf>,
    witness_rows_n: usize,
    /// The rows whole-grid scrolls carried the grid since the last tick
    /// ([`Self::note_scroll`]); `0` on a frame with no scroll. A scroll's
    /// frame feeds its witness rows, read after the translation
    /// (`ScrollChange::translated_only`), to the follow pass and the hand's
    /// seam, so a box redrawn in place under the scroll keeps its band on the
    /// very frame it is drawn, and a caret left on its screen row is judged in
    /// its line's own frame: no move, and a key in flight keeps its press for
    /// its echo. The walk waits for the next frame, as it did when a scroll's
    /// frame sampled nothing: a copy on a row the scroll brought in is dated
    /// from the walk before the scroll ([`rk::witness::Witness::translate`]),
    /// and a walk now would take a torn relocation for a twin. A held
    /// foreign-row park's source check does not wait: the park rode the
    /// scroll with its row ([`Self::carry_held_park`]), so the carried row is
    /// read where the scroll put it, and an echo that returns on the scroll's
    /// own frame to a composer the scroll carried with it is paid. A box
    /// redrawn in place on its screen row (Claude Code under a full screen)
    /// leaves transcript text on the carried row: the check fails there, the
    /// park is flushed and that echo stays dark, as it did when the check
    /// waited for the next frame.
    scroll_rows: u16,
    /// The rows the host was asked for this frame (what [`Self::ribbon_rows`]
    /// last handed out, and how many), so the follow pass counts as withheld
    /// only rows asked for and not delivered ([`rk::Engine::follow_rows`]).
    /// Recomputing at the tick could differ, since the waiting key that named
    /// arming rows ([`rk::Engine::ribbon_rows_for`]) may be spent. A `Cell`
    /// because a `&self` query writes it; taken by every tick.
    witness_asked: std::cell::Cell<([u16; RIBBON_LIST_ROWS], usize)>,
    /// The caret seam v2 handed back on the last engaged tick (§7.1); the
    /// host reads it through [`Self::caret_flare_at`] / [`Self::caret_paint`]
    /// when it builds the caret's `RainbowConfig`.
    v2_caret: CaretSeam,
    /// The caret's seated stop (§7.1, D4): the field `t` the caret block mixes
    /// toward, and the caret cell it was seated at. Re-seated on an engaged tick
    /// only when the caret cell changed or its own cell owns field light, and
    /// held otherwise, so a cell dying under a parked caret cannot move its
    /// colour ([`Self::rainbow_head_rgb`]). `None` until the first engaged tick
    /// and whenever v2 lets go of the frame.
    v2_stop: Option<(f32, Option<(u16, u16)>)>,
    /// The flow entry edge: whether the last [`Self::take_flow_entry`] drain
    /// found the theme open ([`rk::Flow::open`]). The run itself is the
    /// engine's ([`rk::spine::Spine::flow`]); this only remembers which side of
    /// the bar the last drain saw, so the pet is told once per entry.
    flow_open: bool,
    /// v2's cue sink ([`rk::Frame::cues`]): resident, drained into
    /// [`Self::sound_cues`] under `MAX_SOUND_CUES` after every engaged tick, so
    /// a non-draining host's backlog stays bounded and the frame allocates
    /// nothing (§18).
    v2_cues: Vec<SoundCue>,
}

/// Window-space geometry the host passes in (pixels). Effect pixel emissions
/// (fire patches, glow/halo/nova quads, beams) are window-absolute: producers
/// add `origin` once and the renderer adds nothing. `origin` is the grid
/// interior's top-left in window px (`pad`, `pad + head + strip_px`); `win` is
/// the full frame extent, so effects may draw above the grid (the titlebar
/// band). With `origin == (0,0)` and `win` equal to the grid extents, every
/// emission is pad-relative (the identity law tests rely on this).
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
    /// Rise allowance above the grid in px: the chrome band the host opened for
    /// effects (the macOS titlebar under `fullSizeContentView`;
    /// `ATERM_HEADROOM_PX` headless). Distinct from `origin_y`, which also folds
    /// in `pad` and the tab strip; the fire-root/rise clamps relax by exactly
    /// this, so `head == 0` reproduces grid-relative clamps (the identity law).
    pub head: u16,
}

impl Geom {
    /// The effects box, where effect pixels may land (window px): the grid box
    /// plus the `head` band above it, `x ∈ [fx_left, fx_right)`,
    /// `y ∈ [fx_top, fx_bot)`. Every pixel clamp uses these bounds; see
    /// [`Geom::head`].
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

    /// Window-px centre of a grid cell — `origin + (index + 0.5) · cell` on
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

/// Exact chronological keys whose content was observed in a repainted input
/// run. The source span is optional when every new glyph is on the landing
/// row. `cells` also prices a proved line-break Space omitted by word wrap;
/// only the observed spans are painted. This is one move's proof, not retained
/// input authority.
#[derive(Clone, Copy)]
struct ExactTypedRun {
    origin_row: u16,
    slot: usize,
    source: Option<(u16, u16, u16)>,
    landing_start: u16,
    cells: usize,
    rewrap_start: Option<(u16, u16)>,
}

/// One observed cursor move as [`CursorGlow::spawn`]'s classifier read it: the
/// raw endpoints, clock, config and geometry plus the typing-vs-jump verdicts
/// every later spawn phase keys on. Built once per move by `classify_move`,
/// which owns hint consumption, so the phases read pure data.
struct MoveCtx<'a> {
    pr: u16,
    pc: u16,
    cr: u16,
    cc: u16,
    now: Instant,
    cfg: &'a GlowConfig,
    geom: Geom,
    /// |Δrow|, |Δcol| and Chebyshev distance of the raw observed move.
    dr_abs: i32,
    dc_abs: i32,
    /// Focused pane's horizontal bounds in this move's window-coordinate
    /// space. They equal `(0, geom.cols)` for a single pane.
    pane_col0: usize,
    pane_cols: usize,
    /// The classifier verdicts; see `classify_move`. `dist`/`typing` below
    /// carry the wrap collapse for every later phase.
    shape_wrap: bool,
    /// The cells a `shape_wrap` fold paid for (origin tail from `pc` to the
    /// pane's edge plus landing head before `cc`, bounded by the credits at the
    /// spend): what the seam's fold sweep lays, as the key's `Typed` replay
    /// would. `0` off the fold or with nothing paid.
    fold_laid: usize,
    echo_run: bool,
    re_anchor: bool,
    rainbow_coalesce: bool,
    /// The coalesce shape matched in every respect except the press-credit
    /// budget (the anti-stray law). The ring's `no-credits` reason and a forget
    /// edge; no emitter reads it.
    credit_starved: bool,
    /// A typed-paired same-row forward hop wider than the sweep cap
    /// (`RAINBOW_TYPED_SWEEP_MAX`): a landing-only re-anchor and a forget edge.
    typed_over_cap: bool,
    /// The typed re-anchor lays its landing: the classifier's spend predicate
    /// (one cell, the landing), carried to the seam so the sweep and the spend
    /// agree. `false` for non-re-anchors and styles with no ribbon.
    re_anchor_lays_landing: bool,
    /// The exact, already-paid repaint spans, replacing a re-anchor's generic
    /// one-cell landing. Both content witnesses use the same payment and emit.
    exact_typed_run: Option<ExactTypedRun>,
    /// The visible lane's typed re-anchor would have laid its one cell under a
    /// glyph no live press typed ([`CursorGlow::probe_glyph_before`], or for a
    /// flushed park [`HeldPark::landing_cell`], against
    /// [`PressCredits::foreign_to_live`]). The classifier forgot the presses and
    /// [`CursorGlow::spawn_judged`] declines the move `program-row` before v2
    /// sees it. Never set with `soft_wrap`, `lift` or `re_anchor_lays_landing`.
    landing_foreign: bool,
    /// The soft-wrapped caret ([`CursorGlow::soft_wrapped_caret`]): the key's
    /// glyph stands at the origin `(pr, pc)` while the caret alone wrapped to
    /// the continuation indent. `Some(word_col0)` is the first column of the
    /// word the key ends. The move lays and spends one cell, at the origin.
    soft_wrap: Option<u16>,
    /// The lifted word ([`CursorGlow::lifted_word`]): the same-row retreat is
    /// the composer moving the word `cc..pc` up onto the end of the row above;
    /// `Some(col)` is where it starts there. The landing sweep stands down.
    lift: Option<u16>,
    /// Licensed by the in-flight pool alone (no stamp fresh; unpaid presses
    /// paid for the echo). The ring row reads `licence=inflight`.
    in_flight_licence: bool,
    /// A fold, re-anchor or exact repainted run: the classifier's wrap verdict,
    /// read by the momentum advance and the fold-shaped cat pulse.
    wrap: bool,
    /// The press-hint half of typed pairing: a fresh key-time `typed_at` stamp.
    /// Equal to `typed_pair` under the licence law; kept separate because the
    /// momentum spine and classifier read it for different reasons.
    typed_hinted: bool,
    /// The fresh typed stamp the classifier consumed: the key's clock, handed to
    /// v2's sweep as the birth of a late-echoed cell.
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
    const GLOW_OUT: u64 = 0x676c_6f77_5f6f_7574;
    const FIRE_PATCH: u64 = 0x6669_7265_7061_7463;
    const GLOW_UNDER: u64 = 0x676c_6f77_756e_6472;
    const CHAR_FG: u64 = 0x6368_6172_5f66_6700;
    const FIRE_HALO: u64 = 0x6669_7265_6861_6c6f;
    const RAIN_HALO: u64 = 0x7261_696e_6861_6c6f;
    const FORGE_FILL: u64 = 0x666f_7267_655f_6669;

    #[inline]
    fn write(&mut self, word: u64) {
        self.0 = Some(fnv_mix_u64(self.0.unwrap_or(FNV64_OFFSET), word));
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
        // One local fold rather than a `write` per word: the same
        // `fnv_mix_u64` chain over the same words in the same order. A ribbon
        // frame streams thousands of quads through here, so the loops are
        // counted: an unoptimized build calls an iterator's `next` out of line.
        let mut hash = self.0.unwrap_or(FNV64_OFFSET);
        let mut qi = 0;
        while qi < quads.len() {
            let words = quads[qi].damage_words();
            let words = words.as_slice();
            let mut wi = 0;
            while wi < words.len() {
                hash = fnv_mix_u64(hash, words[wi]);
                wi += 1;
            }
            qi += 1;
        }
        self.0 = Some(hash);
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
    /// Hard cap on emitted quads (protects the renderer and per-frame upload);
    /// the snapshot is truncated so both backends see identical geometry. Sized
    /// for the laser's six bloom layers over a full-length jump beam plus its
    /// bolts (~12 B per instance).
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
    /// Longest same-row typed coalesce the ribbon sweeps as continued typing
    /// (cells); beyond it the move keeps the single landing spark.
    ///
    /// Equal to [`TYPED_STAMP_DEPTH`] by construction: one observed move spends
    /// at most this many cells. The press ledger ([`PressCredits`]), not the
    /// length, separates a batched echo from a re-anchor.
    const RAINBOW_TYPED_SWEEP_MAX: usize = TYPED_STAMP_DEPTH;
    /// The anchored lane's bound (cells) for a row whose identity no licensed
    /// anchored echo has proven yet (`echo_anchor_pass`): a program row's
    /// brandless first advance of 33..128 cells beside a fresh stamp returns
    /// silently instead of reaching `spawn`. The established echo row and a
    /// delivered insert's row take the full cap.
    const ANCHOR_UNPROVEN_ROW_MAX: usize = 32;

    // ── The zoom streak's shape (see `emit_rainbow_jumps`) ─────────────────
    //
    // Four numbers, each an endpoint of a continuous ramp rather than a level
    // some stretch of the mark is held at, so the streak has no seams.

    /// Most simultaneous live lightning bolts, main channels + branch forks
    /// (oldest evicted first). A jump strike spawns 1 main + up to 4 branches,
    /// so this comfortably holds several overlapping strikes plus crackle.
    const MAX_BOLTS: usize = 24;
    /// Lightning's flash law (WCAG 2.3.1: no more than three flashes in any
    /// one second; the owner asked for "a lightning bolt or flashing yellow",
    /// 2026-07-14, so lightning keeps its flash and counts it). Every bolt
    /// flashes once ([`bolt_flash`]) and a strike's forks are born with it, so
    /// a pixel can flash no more often than bolts are born: at most this many
    /// onsets, strikes and crackles together, in any one second
    /// ([`Self::admit_bolt`]).
    const BOLT_FLASHES_PER_S: usize = 3;
    /// A typing crackle's refractory: the least time after the previous
    /// lightning onset (strike or crackle) before a crackle may fire. Over half
    /// a second, so crackles alone fill at most two of a second's
    /// [`Self::BOLT_FLASHES_PER_S`] and a jump strike always finds a slot.
    const CRACKLE_REFRACTORY_S: f32 = 0.55;
    /// Bloom-crown fade window (the crown follows the head for this long post-move).
    const CROWN_MS: u64 = 200;
    /// Typing crown window: a single-cell advance keeps the crown lit this long,
    /// so at human cadence the glow stream stays non-empty across inter-key gaps
    /// (and the renderer's SDR attack envelope does not re-bloom per key). Jumps
    /// keep [`CROWN_MS`].
    const CROWN_TYPING_MS: u64 = 350;
    /// Heat earned by one keystroke at full cadence (≈7 fast keys to full heat).
    const HEAT_GAIN: f32 = 0.16;
    /// Exponential heat cool-down time constant (seconds to ~37%).
    const HEAT_DECAY_TAU: f32 = 0.9;
    /// Jump-flare cool-down τ: fast enough that the white-hot burst reads as an
    /// event, slow enough that the cooling arc is visible.
    const FLARE_DECAY_TAU: f32 = 0.45;
    /// Inter-key gap (seconds) at or under which a keystroke earns full heat…
    const HEAT_GAP_FULL: f32 = 0.09;
    /// …and the gap beyond which it earns none (relaxed typing stays cool).
    const HEAT_GAP_ZERO: f32 = 0.40;
    /// Typing continuity: an inter-key gap at or under this
    /// (== [`Self::HEAT_GAP_ZERO`]) chains the new spark's life to the observed
    /// cadence, so the streak is continuous at any human rhythm instead of
    /// pulsing per key.
    const CHAIN_GAP_MAX: f32 = 0.40;
    /// Chained life = observed gap + this margin (the previous spark comfortably
    /// outlives the gap)…
    const CHAIN_MARGIN: f32 = 0.10;
    /// …capped here so the post-stop tail stays crisp (≤500ms after the last key).
    const CHAIN_LIFE_MAX: f32 = 0.50;
    // ---- EMBERFORGE thermal model (fire only) --------------------------------
    /// Fire's fast-heat gain per forward keystroke at full cadence, well below
    /// [`Self::HEAT_GAIN`]: full blaze takes ~30-40 sustained keys (fast heat
    /// plus the coal floor).
    const FIRE_HEAT_GAIN: f32 = 0.045;
    /// Fire's heat cool-down τ — slower than the shared 0.9 s: momentum
    /// survives a short thought instead of resetting between phrases.
    const FIRE_HEAT_TAU: f32 = 2.8;
    /// Coal-bed cool-down τ (seconds to ~37%) — the slow persistence floor.
    const COAL_TAU: f32 = 6.0;
    /// Coal charged by one forward keystroke at full cadence credit: with the
    /// τ above, ~60+ sustained keys reach a strong bed — the long momentum arc.
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
    /// Each delete also cools the standing heat and the coal bed by this factor.
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
    /// How long the caret must stand still before a typed lay is judged
    /// against where it rests ([`TornLay`]): two 60 Hz frames, past the
    /// continuation of a read that tore inside one redisplay.
    pub(crate) const TORN_REST_S: f32 = 0.032;
    /// Freshness window for the RETURN classifier ([`Self::return_hint`]).
    const RETURN_HINT_FRESH: f32 = 0.25;
    /// Freshness window for the Tab/scripted gesture stamp
    /// ([`Self::user_gesture_hint`]), in the 0.25 s class of the other press
    /// hints.
    const USER_GESTURE_HINT_FRESH: f32 = 0.25;
    /// Freshness window for the delivered-insert licence ([`InsertSeam`]). A
    /// debounced TUI (Claude Code's Ink prompt under load) can echo over a
    /// second after the bytes land, so 0.25 s would refuse most inserts. It is
    /// shorter than the typed press's in-flight patience
    /// (`IN_FLIGHT_PATIENCE_S`). Safe at this length because the shape bounds
    /// what it can buy: one same-row forward echo, no wider than the insert plus
    /// the unpaid typed credits behind it, on a non-program row, once.
    pub const INSERT_HINT_FRESH: f32 = 2.0;
    /// The same window for the unknown-width class (a bare Tab or ⌃V, whose
    /// echo is the local program's).
    ///
    /// Shorter than [`Self::INSERT_HINT_FRESH`]: the unknown class has no width
    /// to check (only the row witness and the 32-cell bound), so a long window
    /// would light any same-row forward advance on the hand's row (a background
    /// job printing on the prompt row) as the gesture's echo. The floor is
    /// `a_spinner_row_advancing_after_a_tab_stays_dark`, which needs a Tab's
    /// completion to still be the Tab's 800 ms later; one second keeps that
    /// with margin.
    ///
    /// A policy, not a bound: a slower completion goes unlit rather than
    /// leaving the window open for program output. A column witness cannot
    /// replace the time bound: the first same-row forward move after a Tab
    /// always starts at the hand's column, so the licence is spent before any
    /// later hop could be tested.
    pub const INSERT_GESTURE_HINT_FRESH: f32 = 1.0;
    /// How long after a delivered insert was laid a keyless same-row retreat
    /// inside its span reads as the program's rewrite of it (Claude Code swaps a
    /// dropped path for `[Image #1] ` a few hundred ms after echoing it). Past
    /// this the retreat is ordinary program output.
    pub const INSERT_REWRITE_FRESH: f32 = 2.0;
    /// How long after a refused same-row forward hop a delivery receipt may
    /// still claim it ([`PendingHop`]): the writer publishes microseconds after
    /// the write, and the observing frame is at most one present behind.
    pub const INSERT_RETRO_FRESH: f32 = 0.25;
    /// The unknown-width bound for an insert the host cannot price (a Tab
    /// completion, a raw ⌃V placeholder): a same-row forward echo of at most
    /// this many cells may spend it. Covers Claude Code's widest placeholder
    /// (`[Pasted text #1 +N lines] `, ~28 cells) plus a completion, and is
    /// independent of the press bank's depth. Counted once per licence, so an
    /// auto-repeated Tab does not widen it.
    pub const INSERT_GESTURE_CELLS: u16 = 32;
    /// Freshness window for the reflow classifier ([`Self::reflow_hint`]). It is
    /// retained for bounded lifecycle observation, not movement admission.
    const REFLOW_HINT_FRESH: f32 = 0.40;
    /// How long a kill key hint stays armed for its row-shrink echo (seconds;
    /// see [`Self::note_kill`]). Wider than the typed hint: a kill's erase is
    /// often a full-box TUI repaint that lands a frame or two later.
    const KILL_HINT_FRESH: f32 = 0.35;
    /// How long a repaint-blink hint ([`Self::note_repaint_blink`]) stays fresh
    /// (seconds). The blink is noted at the previous burst's present, so it
    /// must outlive one inter-key gap for the next keystroke's re-anchor, while
    /// staying well under the pause that means the TUI stopped repainting per
    /// key.
    const BLINK_HINT_FRESH: f32 = 0.5;
    /// Coarse poll cadence while only a pending kill hint keeps the engine live
    /// ([`Self::next_change_deadline`]): the caret fallback needs a few frames
    /// after [`Self::POOF_FALLBACK_GRACE`], not 60 fps for the whole hint
    /// window.
    const KILL_HINT_POLL_INTERVAL: Duration = Duration::from_millis(40);
    /// Minimum gap between erase poofs (seconds), so a held kill-key repeat
    /// billows at a readable rate. The caret-anchored fallback waits this long
    /// after the key so the precise span branch gets the first shot at the echo.
    ///
    /// A cap, not a floor: [`Self::poof_erase_witnessed`] releases it as soon
    /// as the probe shows the erase on glass (the span branch has then had its
    /// shot). The timer governs cases with no witness: a reflow that re-rowed
    /// the caret, an echo that never shows, a host with no probe at the key.
    const POOF_FALLBACK_GRACE: f32 = 0.06;
    const POOF_MIN_GAP: f32 = 0.14;
    /// A stored row probe older than this cannot witness a kill (seconds). A
    /// backstop against a host that stops probing mid-session.
    const POOF_PROBE_STALE: f32 = 5.0;
    /// Quench added by one kill key ([`Self::note_kill`]): about two
    /// backspaces' worth, so two kills fully quench a standing blaze.
    const KILL_QUENCH_GAIN: f32 = 0.55;
    /// Display-temperature attack ease (seconds to ~63% of a rise): every
    /// brightening is a swell, and the blaze builds over about a second of
    /// sustained typing.
    const DISP_ATTACK_S: f32 = 0.34;
    /// …except a slam (a jump flare opening a gap > 0.5): a fast whoosh so the
    /// eruption still lands within a couple of frames.
    const DISP_SLAM_S: f32 = 0.05;
    /// Display-temperature release τ — the fire dies down slower than it leaps.
    const DISP_RELEASE_TAU: f32 = 0.45;
    /// Bounded ring for `flame_phase` (the fire's churn clock), like
    /// `rainbow.phase`'s 1024 wrap: unbounded, f32 precision would freeze the
    /// phase over a long session and the `* 1024.0 as u32` cast would saturate.
    /// `TAU · 640` keeps both forge sines (`× 1.7`, `× 2.3`) seamless across the
    /// wrap (1088 and 1472 whole turns); the field's own u32 ring reseats only
    /// after ~40 min of continuous fire.
    const FLAME_PHASE_RING: f32 = std::f32::consts::TAU * 640.0;
    /// Cursor-metal attack τ (heats over ~1.5 s of standing blaze)…
    const TEMP_ATTACK_TAU: f32 = 1.5;
    /// Release τ: cools much slower than it heats (forged metal holds heat);
    /// the quench meter divides it, so deleting cools faster.
    const TEMP_RELEASE_TAU: f32 = 1.4;
    /// Below this metal temperature the forge is at rest: the fill holds its
    /// resting ember ([`thermal::ForgeBand`]'s `lo`), the dressing is dark and
    /// the fill is not folded into the fingerprint.
    const FORGE_MIN_TEMP: f32 = 0.12;
    /// Coarse wake cadence for the forge-ember-only cooling tail: the ember's
    /// u8 colour barely changes between these, and the present gate dedups
    /// anyway ([`Self::next_change_deadline`]).
    const EMBER_POLL_INTERVAL: Duration = Duration::from_millis(90);
    /// Same-row fire moves at/past this many columns become a meteor (any row
    /// change already does — matching the rainbow kitty's jump classification).
    const METEOR_MIN_COLS: i32 = 8;
    /// Most simultaneous live fire meteors (oldest evicted first).
    const METEOR_CAP: usize = 4;
    /// A fresh meteor whose origin equals a live meteor's landing within this
    /// window retargets that meteor instead of spawning another, so shell
    /// repaint choreography (CR → prompt reprint over a few frames) becomes one
    /// streak. Must stay under the strike fraction of the shortest meteor life
    /// so a retarget always beats arrival.
    const METEOR_RETARGET_S: f32 = 0.12;
    /// The head strikes (arrival flare) at this fraction of the meteor's life
    /// — the tail then finishes retracting into the eruption.
    const METEOR_STRIKE_FRAC: f32 = 0.62;
    /// Hard cap on per-frame radial halos (embers dominate; well above any
    /// real frame — both backends see the identical truncated snapshot). The
    /// classic engine fills the same stream on a light ground and caps it here
    /// too.
    pub(crate) const MAX_HALOS: usize = 512;
    /// Hard cap on live vapor puffs (steam + smoke).
    const MAX_VAPOR: usize = 128;
    /// Steam's stacking budget, in fresh puffs: the whole live steam cloud
    /// lays at most this much light, so a backspace run widens the cloud
    /// rather than brightening it (`emit_vapor`). Two fresh puffs overlapping
    /// on a letter of the line above leave it 3:1 on the default dark theme.
    const STEAM_STACK: f32 = 2.0;
    /// How much of a steam puff's light is gone once it has climbed 0.6 cell
    /// past the top of the row it rose from: the cloud stays over the doused
    /// text instead of settling on the line above.
    const STEAM_ABOVE_DIM: f32 = 0.65;
    /// How many cells either side of the typed run the over-ink ledger holds
    /// on a neighbouring row the host did not probe
    /// ([`Self::begin_ink_ledger`]): about as far as the ember shower and the
    /// steam drift sideways.
    const INK_LEDGER_REACH: i32 = 3;
    /// The most steam puffs one deletion echo releases at full quench (two at
    /// the first delete, one more as the quench builds).
    const STEAM_ECHO_PUFFS_MAX: usize = 3;
    /// The cursor metal smokes above this temperature (the brief's "heats up
    /// over use and starts smoking").
    const SMOKE_TEMP: f32 = 0.35;

    /// Phaser constant-distance streak: within a rhythm the band's life is this
    /// many observed inter-key gaps, so it spans the same distance at any
    /// cadence: three letters solidly lit behind the cursor, the fourth fading.
    const PHASER_CHAIN_KEYS: f32 = 4.5;
    /// The Phaser chain window, a real typing rhythm only and much tighter than
    /// Rainbow Kitty's, so a key after a pause does not inherit a long life and
    /// park the band over old text.
    const PHASER_CHAIN_GAP_MAX: f32 = 0.75;
    /// …capped just above `PHASER_CHAIN_KEYS × PHASER_CHAIN_GAP_MAX` so the
    /// slowest chained rhythm still earns its full three-letter span.
    const PHASER_CHAIN_LIFE_MAX: f32 = 3.5;
    /// Lightning trail residual: the fraction of beam power an aged trail cell
    /// holds after its discharge bleed, the dim static charge before the fade.
    const LASER_RESIDUAL: f32 = 0.30;
    /// The most opacity a light-ground bolt paints with (`emit_bolts`'s light
    /// arm, beneath the glyph ink): where a fork crosses the channel the two
    /// stack, and a dark foreground drawn over that pixel still keeps 4.5:1
    /// on the default violet (`laser_on_a_light_ground_is_ink_never_light`).
    const LIGHT_BOLT_ALPHA_CAP: f32 = 144.0;
    /// The laser streak's scintillation, rad/s: one swell per cycle, so its
    /// flash rate is `ω / 2π` (2.86 Hz), held under the crate's flash bound at
    /// build time.
    const LASER_RIPPLE_OMEGA: f32 = 18.0;
    /// Fire streak caps. [`Self::FIRE_STREAK_COV_CAP`] bounds every swept
    /// sample; over a text cell on a dark ground a sample also spends the
    /// fire's over-ink account ([`InkLedger`]) with the crown, forge, embers
    /// and steam that stack on the same letters, which holds the freshest
    /// glyphs' contrast. The bridge onto the empty cursor cell keeps
    /// [`Self::FIRE_HEAD_COV_CAP`].
    const FIRE_STREAK_COV_CAP: f32 = 108.0;
    const FIRE_HEAD_COV_CAP: f32 = 168.0;
    /// The dark flame field's columns per cell ([`Self::emit_flames`]): the
    /// heat smoothed along the line is sampled this finely (two pixels or
    /// so on a shipped cell).
    const FIRE_SUBCOLS: i32 = 6;
    /// The dark flame field's slice height, px: its ceiling thins toward the
    /// tips, and eases from the air's to the text's, in steps this fine.
    const FIRE_SLICE_PX: i32 = 2;
    /// How many rows over the topmost live row the frame's text is read
    /// ([`Self::observe_frame_text`]): a dark fire burns brighter over a
    /// blank row only where it knows the row is blank.
    const FRAME_TEXT_RISE: i32 = 3;
    /// The dark flame field's ceiling over a blank row, cold
    /// ([`Self::emit_flames`]): light with no letter under it.
    const FIRE_DARK_AIR_CAP_COLD: f32 = 96.0;
    /// How far the dark air ceiling climbs at full blaze.
    const FIRE_DARK_AIR_CAP_HEAT: f32 = 64.0;
    /// The dark flame over a blank row ([`Self::emit_flames`]): the newest
    /// burning cells' share of the head's strength, newest first, so the last
    /// few letters each carry a tongue falling away behind the caret however
    /// cool the fire still is.
    const FIRE_DARK_AIR_RANKS: [f32; 6] = [1.0, 0.92, 0.82, 0.68, 0.5, 0.32];
    /// The least tongue envelope over a blank row, in cell heights: a cool
    /// fire still stands about a row tall there.
    const FIRE_DARK_AIR_PEAK: f32 = 2.4;
    /// The least display temperature a burn over a blank row runs at: its
    /// tongues' cores reach orange and yellow, not only the deep red of a
    /// cool tip.
    const FIRE_DARK_AIR_TEMP: f32 = 0.72;
    /// The least ceiling a burn over a blank row runs at in the air: its
    /// tongues are lit flames, not a dim tint (no letter lies under them).
    const FIRE_DARK_AIR_TONGUE_CAP: f32 = 208.0;
    /// Where the dark flame roots, as a share of the cell height below the
    /// burning row's top: inside the row's ascender band, where the field's
    /// root lift fades it in over the letters' tint, so no unlit line stands
    /// between the letters and the flame.
    const FIRE_DARK_ROOT: f32 = 0.20;
    /// How far a dark flame over a blank row fades in from its root (the
    /// burning row's top edge), in cell heights: a soft foot, no edge.
    const FIRE_DARK_AIR_FOOT: f32 = 0.5;
    /// A dark tongue's half-width at its foot, in cells: a little over half
    /// a cell, so neighbours merge at their feet and part above them.
    const FIRE_DARK_TONGUE_HALF_W: f32 = 1.05;
    /// How far a dark tongue's foot curls up at its sides, in cell heights:
    /// the line of feet is scalloped, never one flat base.
    const FIRE_DARK_FOOT_CURL: f32 = 0.25;
    /// How much shorter a tongue over a blank row may stand than its
    /// tallest neighbour, by its cell's hash: the tongues are a fire's, not a
    /// comb's teeth.
    const FIRE_DARK_AIR_SIZES: f32 = 0.22;
    /// The tongue over the caret, ahead of the head, as a share of the
    /// head's strength over a blank row: the flames taper off past the
    /// newest letter rather than standing cut at its edge.
    const FIRE_DARK_AIR_LEAD: f32 = 0.45;
    /// How far along a leap off the line its grains start being born, in
    /// cells ([`Self::spawn_burst_particles`]).
    const LEAP_BIRTH_SKIP_CELLS: f32 = 1.5;
    /// Seconds an off-line flame gets to gutter out after the cursor leaves
    /// it behind ([`Self::snuff_off_line`]).
    const FLAME_SNUFF_S: f32 = 0.18;
    /// …and a spark, sooner.
    const SPARK_SNUFF_S: f32 = 0.08;
    /// How far back along a fire meteor's flight, as a share of it, its
    /// strike's debris is born ([`Self::meteor_strike_fountain`]).
    const STRIKE_DEBRIS_REACH: f32 = 0.35;
    /// A dark ember's coverage, as a share of its grain's
    /// ([`Self::emit_particles`]): a spark is small, so it burns near full
    /// strength without whiting anything out.
    const FIRE_EMBER_COV: f32 = 0.85;
    /// A dark ember's core radius in cell heights, coolest, and what the
    /// hottest adds: a point of light, not a blob.
    const FIRE_EMBER_R: f32 = 0.04;
    const FIRE_EMBER_R_HOT: f32 = 0.05;
    /// The least saturation a light landing veil's ink wears
    /// ([`Self::light_veil_ink`]).
    const LIGHT_VEIL_SAT: f32 = 0.75;
    /// The gap a light tongue keeps under a line of text above its own, in
    /// cell heights ([`Self::flame_light_headroom`]).
    const FIRE_LIGHT_HEADROOM: f32 = 0.08;
    /// The light ink-fire ([`Self::emit_flames_light`]): its peak opacity
    /// (over air; a letter it reaches holds its own ceiling).
    const FIRE_LIGHT_ALPHA: f32 = 240.0;
    /// The light ink-fire's root, as a share of the cell height below the
    /// burning row's top: at the foot of the letters' x-height, so the
    /// tongues stand up out of the line, most of each one above the
    /// letters' tops (rooted under the baseline they barely cleared them,
    /// and behind the letters a flame is held to the letters' ceiling).
    const FIRE_LIGHT_ROOT: f32 = 0.70;
    /// The light ink-fire's height at full strength, in cell heights: cold,
    /// and how much a full blaze adds.
    const FIRE_LIGHT_RISE_COLD: f32 = 2.3;
    /// See [`Self::FIRE_LIGHT_RISE_COLD`].
    const FIRE_LIGHT_RISE_HOT: f32 = 0.85;
    /// The light ink-fire's tongue spacing, in cells: far enough apart that
    /// each tongue reads as its own flame, not a stripe of a fence.
    const FIRE_LIGHT_SPACING: f32 = 0.85;
    /// A light tongue's half-width at its widest, in cells.
    const FIRE_LIGHT_HALF_W: f32 = 0.62;
    /// The light ink-fire's sampling: knots across, px, and slab height, px.
    const FIRE_LIGHT_KNOT_PX: f32 = 2.0;
    /// See [`Self::FIRE_LIGHT_KNOT_PX`].
    const FIRE_LIGHT_SLAB_PX: f32 = 2.0;
    /// The coloured letter a row nobody read is held for under the light
    /// ink-fire, at 3:1 ([`Self::flame_light_cap`]): GitHub Light's red.
    const FIRE_LIGHT_REF_RED: u32 = 0x00CF_222E;
    /// The most opacity a light-ground fire meteor paints with
    /// ([`Self::emit_fire_meteors`]'s light arm, beneath the glyph ink): over
    /// its deepest colour a default-foreground letter keeps 4.5:1.
    const LIGHT_METEOR_ALPHA_CAP: f32 = 144.0;
    /// The ramp span a light-ground meteor is painted in: the dark ground's
    /// tail-to-head walk folded into deep red through burnt orange.
    const LIGHT_METEOR_RAMP: (f32, f32) = (0.25, 0.50);
    /// Fire's landing veil on paper: a soft warm orange (the grey a
    /// white-hot flash inks to read as a stain).
    const FIRE_LIGHT_LANDING: u32 = 0x00F2_9A4A;
    /// A light-ground ember's deep rim (an ember's dull red) and hot core
    /// (burnt orange): a round bead of ink, never a square fleck (and never
    /// over the letters: a light ember is not drawn on a line of text).
    const LIGHT_EMBER_INKS: (u32, u32) = (0x00B4_3A0C, 0x00F2_8420);
    /// The over-ink contrast floor of a light laid straight across a letter:
    /// the least contrast a glyph stroke keeps on the reference pair
    /// ([`Self::STREAK_REF_INK`] on [`Self::STREAK_REF_GROUND`]) where Fire's
    /// streak or a laser crackle crosses it, WCAG's text minimum.
    ///
    /// Such a light composites over the glyphs it crosses (`cursor_glow_add`,
    /// saturating add), lifting ink and ground together until the ink clips
    /// and only the ground keeps climbing, so its coverage ceiling is the most
    /// at which its whole stack, in its own colour, still leaves the stroke
    /// this much. Fire spends it through its over-ink account ([`InkLedger`]),
    /// shared with its other lights over the letters; a crackle solves it
    /// alone. The rest of the beam family lays no light on a letter: its
    /// streak lies under the text as its bed ([`Self::emit_dark_bed`]).
    const STREAK_INK_FLOOR: f32 = 4.5;
    /// The reference ink [`Self::STREAK_INK_FLOOR`] is solved against: the
    /// shipping default dark theme's foreground (`aterm_render::Theme::default`).
    /// A fixed pair rather than the live theme's, so fire looks the same on
    /// every dark theme and a theme whose own contrast sits near the floor
    /// still gets a streak over its text.
    const STREAK_REF_INK: u32 = 0x00D0_D0D0;
    /// The reference ground: the shipping default dark theme's background.
    const STREAK_REF_GROUND: u32 = 0x0011_1318;
    /// Water's per-cell wake ceiling: saturated ocean stops carry less
    /// luminance than pale cyan, and newest-owner dedup keeps claims from
    /// stacking past it.
    const WATER_WAKE_COV_CAP: f32 = 88.0;
    /// The most opacity water's light-ground crest paints with
    /// ([`Self::emit_water`]'s light arm), reached by a fresh crest. The wake
    /// lies beneath the glyph ink, so the bar is the letters on it: crest over
    /// undertow at this opacity leaves a dark foreground about 6.5:1 on white
    /// (`water_on_light_is_ink_under_the_letters` measures it).
    const LIGHT_WATER_ALPHA_CAP: f32 = 160.0;
    /// The particle law's dark-ground dimming of a grain over a probed line of
    /// text: the ember's occupied-cell factor, so a grain crossing a letter
    /// stays a faint glint and the letter stays readable.
    const GRAIN_OVER_GLYPH_DIM: f32 = 0.30;
    /// A light-ground grain's opacity per unit of its dark coverage request:
    /// a fresh grain (coverage 230 at the default 0.7 intensity, 161)
    /// reaches the may-touch-text ceiling, and a fading one thins with it.
    const LIGHT_GRAIN_GAIN: f32 = 1.2;
    /// Sparkle's crescent moon radius as a multiple of the grain's stardust
    /// radius (`dust_r`): a small moon among the dust, well under a glyph.
    const MOON_DUST_RATIO: f32 = 2.0;
    /// A dark sparkle grain's coverage over the family's, and its size
    /// ([`Self::emit_particles`]): the gold comet's shower reads as sparks.
    const SPARKLE_DARK_LIFT: f32 = 1.35;
    /// See [`Self::SPARKLE_DARK_LIFT`].
    const SPARKLE_DARK_GROW: f32 = 1.3;
    /// The comet's ice glints on a light ground ([`Self::emit_comet_light`]):
    /// each one's distance behind the head in cells (clear of the letter just
    /// typed, which reads as it would with no glint), and whether it hangs in
    /// the leading above the letters (else under the baseline, where the
    /// letter over it has no descender, and above it where it has).
    const COMET_LIGHT_GLINTS: [(f32, bool); 3] = [(1.5, true), (2.3, false), (3.1, true)];
    /// How far a light band (beam, phaser, sparkle) rises from nothing over
    /// its oldest cells, in cells: a leap's tail (the Settings preview's
    /// demo leaps back along its line) fades out instead of ending in a cut.
    const LIGHT_BAND_TAIL_CELLS: f32 = 1.5;
    /// A light comet glint's horizontal arm, from its centre, in cell
    /// heights.
    const COMET_LIGHT_GLINT_ARM: f32 = 0.19;
    /// A light comet's coma ([`push_ink_ring`]): its radius and its rim's
    /// width, in cell heights.
    const COMET_LIGHT_COMA_R: f32 = 0.5;
    const COMET_LIGHT_COMA_RIM: f32 = 0.11;
    /// A light comet glint's vertical arm, from its centre, in cell heights,
    /// above the letters and under them: short, so the star stays in the
    /// leading it hangs in.
    const COMET_LIGHT_GLINT_RISE: (f32, f32) = (0.072, 0.10);
    /// Where a high glint hangs, from the row's top, in cell heights: in the
    /// leading above the ascenders' tops (about 0.15 of the row down at the
    /// gallery's 22 px), its vertical arm reaching no lower than they start.
    const COMET_LIGHT_GLINT_HIGH: f32 = 0.075;
    /// A text row's glyph band, as shares of the cell height from its top:
    /// from above its ascenders' tops to its descenders' feet
    /// ([`Self::emit_particles`]).
    const GLYPH_BAND: (f32, f32) = (0.12, 0.86);
    /// Where a dark comet's high dust hangs, from the row's top, in cell
    /// heights: at the row's very top, clear of the ascenders (a grain at
    /// their height reads as an apostrophe).
    const COMET_DARK_DUST_HIGH: f32 = 0.02;
    /// Where a dark comet's low dust hangs, from the row's top, in cell
    /// heights: along the tail under the letters.
    const COMET_DARK_DUST_LOW: f32 = 0.95;
    /// The share of a dark comet's dust that hangs in a text row's leading
    /// (the rest waits off the row, unseen), and the spread of its heights
    /// there, in cell heights.
    const COMET_DARK_DUST_SHARE: f32 = 0.4;
    const COMET_DARK_DUST_JITTER: f32 = 0.04;
    /// Where a low glint hangs, from the row's top, in cell heights: under
    /// the baseline (about 0.77 down), where only a descender reaches.
    const COMET_LIGHT_GLINT_LOW: f32 = 0.885;
    /// The water's line below its row's middle, in cell heights: the swell
    /// rides the lower half of the letters, a waterline rather than a line
    /// struck through them.
    const WATER_LINE_DY: f32 = 0.10;
    /// The swell's height at rest and how much a hot run adds, in cell
    /// heights.
    const WATER_SWELL: (f32, f32) = (0.08, 0.12);
    /// The knots each cell of a water run is drawn through: the swell's
    /// curve, not a polyline of chords whose corners read as carets.
    const WATER_KNOTS: usize = 4;
    /// A light water drop's ink: a clear deep blue rim round a clear sky
    /// blue core (a slate mote read as grey grit, not water).
    const LIGHT_WATER_DROP_INKS: (u32, u32) = (0x0010_6EB4, 0x0036_A8E2);
    /// A dark water droplet's radial falloff reaches this many times its
    /// body's half-size, so its soft rim carries about the light the hard
    /// chip it replaced did.
    const WATER_DROP_SOFT: f32 = 1.6;
    /// Sparkle's moon tint on a dark ground: a saturated warm gold, the
    /// comet's own family (a silver moon read as a grey smudge beside it, and
    /// a pale gold one fading to ash-brown).
    const MOON_GOLD: u32 = 0x00FF_BE3C;
    /// How slowly a dark sparkle moon fades: its coverage follows its life's
    /// remaining share to this power, so a moon reads gold, not ash, until
    /// it is nearly gone.
    const MOON_FADE_POW: f32 = 0.4;
    /// Sparkle's shower on paper: a bead's (and a star's and a dash's) deep
    /// gold rim, the mark's definition on white (4:1).
    const SPARKLE_PAPER_RIM: u32 = 0x00B8_6E00;
    /// The bright gold core of a sparkle bead on paper, inside its rim.
    const SPARKLE_PAPER_CORE: u32 = 0x00F5_B618;
    /// A sparkle star on paper: the rim's gold a shade deeper, since the
    /// star twin's thin arms are laid at a may-touch-text opacity.
    const SPARKLE_PAPER_STAR: u32 = 0x00A6_6000;
    /// A sparkle moon on paper: one gold between the rim and the core.
    const SPARKLE_PAPER_MOON: u32 = 0x00C4_7C00;
    /// A sparkle bead's radius on paper over its dark grain's: a solid gold
    /// bead needs less area than a halo of light to read.
    const SPARKLE_PAPER_BEAD: f32 = 0.85;
    /// The laser aura's peak as a fraction of its sample's coverage.
    const LASER_AURA_COV: f32 = 0.42;
    /// How far back along the charged trail (live typing sparks, newest first)
    /// a crackle arc may root — the stray arcs dance over the whole lingering
    /// trail, not just off the write head.
    const CRACKLE_TRAIL_CELLS: usize = 12;
    /// Beam typing continuity: the tube's spark life chains to this many
    /// inter-key gaps, so the rod spans about the last three and a half letters
    /// at any cadence (the newest one or two at full power).
    const BEAM_CHAIN_KEYS: f32 = 3.5;
    /// The beam's chain window (generous, mirroring the phaser's: a steady
    /// tube must survive leisurely rhythms too)…
    const BEAM_CHAIN_GAP_MAX: f32 = 0.75;
    /// …and its chained-life ceiling — well under the phaser's chained
    /// ceiling, so a slow rhythm never parks light on the line for seconds.
    const BEAM_CHAIN_LIFE_MAX: f32 = 1.2;
    /// Water typing continuity: inside a rhythm ([`Self::CHAIN_GAP_MAX`]) the
    /// wake's life is this many inter-key gaps, so about three cells of water
    /// trail the drop at any human cadence; heat still stretches it further
    /// into the torpedo at speed.
    const WATER_CHAIN_KEYS: f32 = 4.0;
    /// …and its chained-life ceiling, the beam's: a slow rhythm never leaves
    /// water standing on the line for seconds.
    const WATER_CHAIN_LIFE_MAX: f32 = 1.2;

    /// Last honest visible source owned by this engine. Native and pipeline
    /// hosts require the classic trail to report the same anchor before they
    /// arm any candidate; a reset/unseeded pair cannot reconstruct a path.
    #[must_use]
    pub fn cursor_anchor(&self) -> Option<(u16, u16)> {
        self.last
            .or_else(|| self.last_visible.map(|(cell, _)| cell))
    }

    /// Reduced-motion arm for the fresh-ink pop (see the `reduced_motion`
    /// field): a pop is a brightness step-fade only. Idempotent; the host sets
    /// it beside the tick, like the sparkle/rain reduced twins.
    pub fn set_reduced_motion(&mut self, reduced: bool) {
        self.reduced_motion = reduced;
        // Seam point 2 (§17.2, §6.11): the posture change goes to v2, dated to
        // the last drawn tick (the engine also reads it from `Config` each
        // tick).
        if self.v2.engaged()
            && let Some(at) = self.heat_at
        {
            self.v2.on_event(rk::Event::ReducedMotion(reduced), at);
        }
    }

    /// The focus seam (law G1): the window's OS focus changed, forwarded from
    /// `App::on_focus` on the event's clock. Rainbow Kitty v2 embers its ribbon
    /// out over `ribbon::FOCUS_EMBER_S` and puts every star on the same ember
    /// (`rk::Event::Focus(false)`); a regain inside the ember re-arms from the
    /// current level through `edge-in` (`Focus(true)`) (`RAINBOW-KITTY-V2.md`
    /// §8.2).
    ///
    /// Raw focus, not the host's presentability fold (which ORs in the typed
    /// wake so an agent-driven window keeps drawing). The ribbon keeps the
    /// wake's promise instead: a key typed during the ember clears it and
    /// re-lights, so only a window nobody types into goes dark on blur. Sound
    /// gates on raw focus too (`app_render::trail_sound_gain`).
    ///
    /// Inert for other styles and while v2 is not engaged.
    pub fn note_focus(&mut self, focused: bool, now: Instant) {
        if self.v2.engaged() {
            self.v2.on_event(rk::Event::Focus(focused), now);
        }
    }

    /// Host repaint blink: the focused terminal's `repaint_blink_epoch`
    /// advanced (the app hid the cursor inside a DEC-2026 synchronized update,
    /// Claude Code's per-keystroke redraw). Arms a short classifier
    /// ([`Self::BLINK_HINT_FRESH`]) selecting re-anchor morphology for an
    /// admitted typed/delete candidate on the alt screen. Never admits movement
    /// or gates bytes.
    pub fn note_repaint_blink(&mut self, now: Instant) {
        self.unsettle();
        self.blink_hint = Some(now);
    }

    /// Drop the blink hint — a tab/pane switch re-pointed this window at a
    /// different terminal, and a blink carried from the old one must not
    /// classify a candidate (or, host-side, the probe) against the new one.
    pub fn clear_blink(&mut self) {
        self.blink_hint = None;
    }

    /// Host per-frame context: whether the probed terminal is on the alt screen
    /// this frame (read beside the row probe). Only the re-anchor classifier
    /// consults it; `false` (main screen, or a host that never calls this)
    /// leaves the classifier unchanged.
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

    /// Any light still alive keeps the animation timer armed. The cursor metal
    /// is the one non-quad member: the forge fill is visible
    /// (`cursor_fill_override`), so the cursor animates until it has cooled to
    /// the theme fill, then snaps to 0. A pending kill hint also arms: a
    /// reflowing TUI's kill is answered by the caret fallback, which runs only
    /// on a ticked frame after its grace, after the app's redraw burst has gone
    /// idle. Bounded by [`Self::KILL_HINT_FRESH`] and paced coarsely
    /// ([`Self::next_change_deadline`]).
    pub fn is_active(&self) -> bool {
        // The classic wake's light is moving light (`has_live_motion`).
        self.has_live_motion()
            || self.ember_live()
            || self.poof_hint_pending().is_some()
            // §17.2: `is_active` follows the v2 fingerprint while it owns
            // the frame — `0` exactly when nothing of v2's is on glass (T6).
            || (self.v2.engaged() && self.v2.fingerprint() != 0)
            // A held park is pending work: its verdict is due ≤ 0.25 s after
            // the park, and only a tick can deliver it ([`HeldPark`]).
            || (self.v2.engaged() && self.held_park.is_some())
            // …and so is a typed lay awaiting the caret's rest ([`TornLay`]).
            || (self.v2.engaged() && self.torn_lay.is_some())
    }

    /// The forge ember is live while the metal is visibly hot or while the
    /// display temperature it chases is still above the visibility threshold
    /// (the heating arc is a pending visible change). Read by both wake
    /// predicates so they cannot drift. Fire-gated: every style integrates
    /// `disp_t`, but only Fire has a metal; `last_style` is the style of the
    /// tick that produced this state.
    fn ember_live(&self) -> bool {
        self.cursor_temp > Self::FORGE_MIN_TEMP
            || (matches!(self.last_style, Some(GlowStyle::Fire))
                && self.disp_t > Self::FORGE_MIN_TEMP)
    }

    /// The pending poof wake: the earlier of the kill licence or the
    /// plain-Backspace classifier that still needs detector service. Both need
    /// a detector tick (a kill via its row-diff/fallback licence, a Backspace
    /// via an exact confirmed span), so scheduling seams read this one
    /// accessor.
    fn poof_hint_pending(&self) -> Option<Instant> {
        match (self.kill_hint, self.bs_poof_hint) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The erase is already on glass: this frame's probe shows the caret's row
    /// shorter than the row stamped at the erase key ([`Self::bs_baseline`]).
    /// The kill fallback's early release from [`Self::POOF_FALLBACK_GRACE`];
    /// a plain Backspace shares it. A `None` baseline (no probe at the key) is
    /// no witness either way, so the fallback keeps its timed grace.
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

    /// Whether any moving light is alive (everything is_active tracks except
    /// the standing forge ember and a pending kill hint).
    fn has_live_motion(&self) -> bool {
        self.needs_frame_cadence() || !self.vapor.is_empty()
    }

    /// Whether live geometry changes fast enough to need phase-locked frame
    /// cadence. Vapor, the forge ember and kill-hint polling are excluded:
    /// their coarse deadlines are part of the latency budget.
    ///
    /// The GUI scheduler uses this exact predicate to choose between its
    /// phase-locked frame train and [`Self::next_change_deadline`]'s coarse
    /// path. A live outgoing style crossfade counts, which also keeps the train
    /// armed across a style switch (via `is_active` → `has_live_motion`).
    #[must_use]
    pub fn needs_frame_cadence(&self) -> bool {
        // The classic wake's comet, crown and ring all move every frame. Each
        // engine is live only while it owns the frame: any other style resets
        // the classic one, and the classic branch settles this one.
        self.classic.is_active() || self.modern_needs_frame_cadence()
    }

    /// [`Self::needs_frame_cadence`] for this engine's own light, without the
    /// classic wake's.
    fn modern_needs_frame_cadence(&self) -> bool {
        !self.sparks.is_empty()
            || !self.particles.is_empty()
            || !self.fire_meteors.is_empty()
            || !self.bolts.is_empty()
            || self.ring.is_some()
            // The crown's window is armed by the move path for every style, so
            // it is the one v1 term that could hold the frame train while v2
            // owns the frame (every other Rainbow Kitty pool is laid by the v1
            // spawn arm seam point 1 returns before). Under v2 the crown neither
            // paces (here) nor paints (the emit gate)
            // (`under_v2_the_seam_never_paces_the_frame_train_for_v1s_crown`).
            || (self.crown_until.is_some() && !self.v2.engaged())
            || !self.fading.is_empty()
            // A live fresh-ink pop is brisk light (spring and ease-out move
            // every frame), so it keeps the train armed until the ring drains.
            // Seam point 8 (§17.2, D14): OR in v2's pools (stars, meteors, a
            // live ribbon transient), `false` whenever v2 is not engaged.
            || self.v2.needs_frame_cadence()
    }

    /// When the next visible change is due, so the host paces the animation
    /// wake instead of pinning 60 fps for the effect's lifetime. `None` when
    /// nothing is live. Moving light gets the full `frame_interval`. A pending
    /// kill hint with no moving light paces at
    /// [`Self::KILL_HINT_POLL_INTERVAL`] (the fallback needs only a few frames
    /// after its grace, and full recomposes would contend the term mutex during
    /// a repaint burst). The slowly cooling forge ember, whose u8 colour barely
    /// changes, also polls coarsely, so a keypress never waits on a recompose.
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant, frame_interval: Duration) -> Option<Instant> {
        // Seam point 9 (§17.2, D14): v2's next change (earliest star death,
        // flight end or ribbon transient), `None` when nothing of v2's is live,
        // so it can only pull a wake earlier.
        let v2_due = if self.v2.engaged() {
            self.v2.next_change_deadline(now)
        } else {
            None
        };
        // Brisk light needs the full frame cadence; the shared predicate keeps
        // this identical to the host scheduler's phase-lock gate.
        //
        // What v1 may still pace under v2: the crown is excluded by the
        // predicate; the ember (`cursor_temp`) is Fire's and zero elsewhere;
        // the ring and particles come only from the v1 spawn arm seam point 1
        // returns before. What remains is the poof hint (the erase detector's
        // licence, whose tick mints v2's `Kill`) and the kill smoke (§5.6),
        // both coarse polls.
        let v1_due = if self.needs_frame_cadence() {
            Some(now + frame_interval)
        } else if let Some(t) = self.poof_hint_pending() {
            // The first wake lands as the fallback's grace opens (`>=` admits a
            // tick at hint+grace), then the coarse poll takes over, so the poof
            // fires at the caret's grace-open position. Either licence paces
            // this ([`Self::poof_hint_pending`]).
            //
            // Never `now` itself: a deadline at `now` is a busy re-arm (the
            // engine's `ARM_MIN` law), which would spin at render cost for the
            // rest of the hint.
            let grace_open = t + Duration::from_secs_f32(Self::POOF_FALLBACK_GRACE);
            let poll = now + Self::KILL_HINT_POLL_INTERVAL;
            Some(if grace_open > now {
                grace_open.min(poll)
            } else {
                poll
            })
        } else if !self.vapor.is_empty() || self.ember_live() {
            // The smoke-only tail joins the forge ember on the coarse poll: a
            // hot cursor sheds wisps for seconds after typing stops, each moving
            // well under a pixel per poll. `is_active` still tracks vapor, so
            // the window keeps ticking coarsely.
            Some(now + Self::EMBER_POLL_INTERVAL)
        } else {
            None
        };
        // A held park schedules the tick that judges it. The flush is strict
        // (`> p.patience()`), so the arm lands one `ARM_MIN` past the park's
        // own window: 0.25 s for a same-row park, 10 s custody for a proved
        // foreign-row park (arming the latter at 0.25 s would re-arm a 1 ms
        // timer every tick). The last unpaid press may expire sooner and end
        // custody too. Never earlier than `now + ARM_MIN`. v2-gated: the park is
        // v2 state and the Classic branch returns before the flush, so a park
        // stranded by a style switch would spin.
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
        // A typed lay is judged when the caret has rested ([`TornLay`]).
        let torn_due = self.torn_lay.filter(|_| self.v2.engaged()).map(|l| {
            (l.moved_at + Duration::from_secs_f32(Self::TORN_REST_S) + rk::ARM_MIN)
                .max(now + rk::ARM_MIN)
        });
        [v1_due, v2_due, park_due, torn_due]
            .into_iter()
            .flatten()
            .min()
    }

    /// Wipe every piece of transient state: in-flight light (sparks, particles,
    /// streaks, bursts, glide stars, meteors, vapor, bolts, ring, crossfades),
    /// all input hints, the row probes, and the Rainbow Kitty tail/pop/wake
    /// machinery. The one teardown all three dark/reset paths compose from, so
    /// a disabled engine reports idle and no stale hint survives to fire on
    /// refocus.
    ///
    /// Left to the callers, the four things the paths differ on: `sound_live`,
    /// the sound ledger ([`Self::clear_sound_ledger`]; a load-shed frame is
    /// dark but heard and keeps its credits), cursor-position tracking
    /// (`last` / `last_visible` / `last_move`), and `ctx_alt`. The classic
    /// engine is the exception: its tracking is cleared with its light.
    fn clear_transient_state(&mut self) {
        self.clear_modern_transient_state();
        // The classic engine keeps its own light and its own cursor memory, and
        // they go together: with no memory, the first classic tick after the
        // lights come back (or in a new coordinate space) only re-observes,
        // so no comet spawns from where the caret stood when they went out.
        self.classic.reset();
    }

    /// [`Self::clear_transient_state`] for this engine alone, leaving the
    /// classic wake's light and memory: the teardown the classic branch runs
    /// when it takes the frame from a style whose light is still live.
    fn clear_modern_transient_state(&mut self) {
        self.recent_typed_run = None;
        self.park_source_intact = None;
        self.carried_key_move = None;
        // A held park is judged before the teardown takes its stamp and
        // its pool: today's verdict, then the wipe.
        self.flush_held_park();
        self.torn_lay = None;
        self.clear_visual_geometry();
        self.type_press_ring.forget();
        self.insert.orphans = None;
        self.last_committed_type = None;
        // Every licence term goes: a surviving hint would license the first
        // program move after the lights come back.
        self.quench_hint = None;
        self.last_erase = None;
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
        self.hand_row = None;
        self.unowned_caret = None;
        self.bs_poof_hint = None;
        self.bs_baseline = None;
        self.last_poof = None;
        self.row_cur.clear();
        self.row_prev.clear();
        self.row_cur_meta = None;
        self.row_prev_meta = None;
        self.row_shown_meta = None;
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
        // Cached per-frame accessor planes: a reset can land between a tick
        // and the host projecting them, so clear them to avoid copying an
        // invalidated frame over newer content.
        self.halo_out.clear();
        self.patch_out.clear();
        self.under_out.clear();
        self.char_out.clear();
        self.fire_halo_out.clear();
    }

    /// The key seam's one law for every tick past the master switch: open when
    /// this tick drew, or when the host marked it [`GlowConfig::audible`]. A
    /// tick dark only from motion policy, load shed, a degenerate grid or the
    /// classic engine's dark frame is still heard. The sound ledger clears only
    /// when the seam closes, so a heard dark frame keeps its credits and the
    /// echoes of keys typed during it stay silent.
    ///
    /// Four call sites (degenerate-grid return, classic wake, zero-amplitude
    /// return, live path), one law, driven for every style by the
    /// `TrailSoundSeam` model's Tier-1 conformance.
    fn settle_key_seam(&mut self, cfg: &GlowConfig, drew: bool) {
        let open = drew || cfg.audible;
        if !open {
            self.clear_sound_ledger();
        }
        self.sound_live = open;
    }

    /// Un-latch the dark-settled fast path ([`Self::dark_settled`]): the caller
    /// is about to write state the `!cfg.enabled` teardown owns (a hint, a
    /// thermal kick, a row probe, new light), so the next disabled tick runs
    /// the full wipe again. Called from every external mutation of wiped state.
    #[inline]
    fn unsettle(&mut self) {
        self.dark_settled = false;
    }

    /// The curtain at a coordinate-space seam (laws A2/A4, D-2, D-3):
    /// [`Self::reset`] for every engine and hint of the ended space, except
    /// that Rainbow Kitty's ribbon is drawn into the caret it last stood under
    /// over `rk::ribbon::CURTAIN_S` and spent to zero, with nothing laid until
    /// it ends ([`rk::Engine::curtain`]). Other v2 marks (flights, stars, in
    /// old-space px) go with the reset, and the caret is learned from the next
    /// observed move. Hosts call this for alternate-screen entry, tab or
    /// terminal switches, resizes and content invalidation; a style switch,
    /// teardown and serious mode still `reset()`. For other styles it is
    /// `reset()`.
    pub fn curtain(&mut self, now: Instant) {
        self.reset_space(Some(now));
    }

    /// Hide Rainbow Kitty's next frame (D-4, A4): the viewport shows history.
    /// Returns `true` when v2 owns the frame and will run its clocks but write
    /// nothing next tick, keeping the band for when the live viewport returns;
    /// `false` for other styles, whose caller resets instead.
    pub fn hide_v2(&mut self) -> bool {
        if !self.v2.engaged() {
            return false;
        }
        self.v2.hide_next_frame();
        true
    }

    /// Whether v2 owns the frame: for a host path deciding between keeping
    /// Rainbow Kitty's band and resetting other styles' marks without arming a
    /// hide it has no tick for ([`Self::hide_v2`]; `TornBandLaw::Keep`).
    #[must_use]
    pub fn v2_owns_frame(&self) -> bool {
        self.v2.engaged()
    }

    /// Whether this engine reads the rows above and below the caret's, so a
    /// host must capture them with its row probe: v2's occupancy and content
    /// witnesses while it owns the frame, and every other lit style's
    /// over-text rules ([`Self::probed_text_spans`]: the over-ink ledger, the
    /// dark bed's split between bed and beam, Fire's light flame cap, the
    /// particles that keep off the text). An unread
    /// row counts as text to the ledger and the bed and as air to the light
    /// rules, so a style that reads them but is handed none either dims over
    /// blank rows or brightens over old output. The classic wake's mono face
    /// reads them too (its bed's rail and its over-ink account); only its
    /// spectrum face, which reads neither, lets a host skip the two row
    /// reads.
    #[must_use]
    pub fn reads_neighbor_rows(&self) -> bool {
        self.v2_owns_frame()
            || self.last_classic_mono
            || self
                .last_style
                .is_some_and(|s| !matches!(s, GlowStyle::Classic))
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
        // A space transition invalidates coordinates wholesale: fade residue,
        // live light, pops, row probes (a stale probe could witness a phantom
        // shrink), hints and thermals. Keyed-click credits go too, or the first
        // echo click in the new space would be muted. `sound_live` stays: a
        // reset is a coordinate event, not darkness.
        self.unsettle();
        self.clear_transient_state();
        self.clear_sound_ledger();
        self.clear_thermals();
        // The classic engine's own memory went with `clear_transient_state`.
        self.last = None;
        self.last_visible = None;
        self.hide_bridge_shown = None;
        // The echo-anchor row memory is coordinate-space state like `last`; a
        // stale launch column would synthesize a false sweep.
        // `print_anchor`/`print_anchor_seen` survive: they mirror the
        // terminal's monotonic sample, so the pass idles until new output.
        self.forget_anchor_rows();
        self.hand_row = None;
        self.wrap_paid_row = None;
        self.last_move = None;
        self.crown_window_ms = Self::CROWN_MS;
        self.ctx_alt = false;
        self.pane_columns = None;
        // Seam point 12 (§17.2, D14): a coordinate-space event drops every v2
        // mark, its spine, its buffered events and the caret's seated stop. v2
        // lets go of the frame: the next drawing tick engages a fresh engine,
        // and a key struck before then lays nowhere. Under a curtain
        // (`Self::curtain`) the engine keeps the frame and its ribbon eases
        // out; everything else resets the same way.
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

    /// Advance one frame: observe the cursor at `cur` (`Some(row, col)`
    /// visible, `None` hidden), spawn on a move, decay, and emit the current
    /// light quads into `out`. Returns a fingerprint that changes on every
    /// visible change (0 when empty).
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
        // The over-ink account is this frame's: closed until the emit phase
        // opens it, so a dark tick leaves none for the host's bodies to spend.
        self.ink_ledger.close();
        // The frame's cells are this frame's: a tick the host handed none
        // reads every row the probe missed as unknown.
        if self.frame_text.at != Some(now) {
            self.frame_text.rows.clear();
            self.frame_text.pairs.clear();
        }
        // The momentum pulse is a per-tick signal: clear it up front so the
        // host reads only this tick's correlated advance (set in `spawn`).
        self.momentum_pulse = None;
        // The witness's samples are this frame's and no other's: taken here,
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
        let scroll_rows = std::mem::take(&mut self.scroll_rows);
        let scroll_frame = scroll_rows > 0;
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
        // The seam's gate (`RAINBOW-KITTY-V2.md` §17.2, D14): v2 owns the frame
        // whenever the resolved style is Rainbow Kitty and this tick draws
        // (master switch on, non-degenerate grid, live amplitude), so a dark
        // tick disengages it. `set_engaged` resets only on a transition; for
        // other styles the `&&` chain ends at its first operand.
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
        // Master switch off or degenerate geometry: full zero. A momentary
        // unfocus arrives as `intensity <= 0` instead and is handled after the
        // lazy cooling below, so earned forge momentum merely cools and resumes
        // on refocus. A grid with no rows or columns is degenerate too (an
        // empty effects box would give `clamp_to_glass` `min > max`).
        if !cfg.enabled || geom.cw == 0 || geom.ch == 0 || geom.rows == 0 || geom.cols == 0 {
            // Full zero includes any in-flight crossfade: off means dark now.
            //
            // The key seam here depends on why the return is taken. The master
            // switch (and serious mode, via `enabled`) closes it: keypresses
            // record no cue and banked credits die. A degenerate grid is a fact
            // about the light's canvas, not sound, so it takes the common law
            // ([`Self::settle_key_seam`]): dark, and heard when `audible`.
            if cfg.enabled {
                self.store_key_timbre(cfg);
                self.settle_key_seam(cfg, false);
            } else {
                self.sound_live = false;
            }
            // Latched: after one full teardown every store below is already
            // cleared, and only an enabled tick, [`Self::reset`] or an
            // [`Self::unsettle`] caller can dirty them (each drops the latch),
            // so the wipe runs once per darkness. Cursor tracking stays outside
            // the latch so re-enabling sees the live position and spawns no
            // spurious comet.
            //
            // The latch is set only while the seam is shut: with the seam open
            // `cue_keystroke` writes the sound ledger without unsettling, so a
            // degenerate grid runs the teardown every frame instead. The sound
            // ledger survives it so keys typed meanwhile still find their
            // credits.
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
        // Any enabled tick may spawn light, bank heat, or arm hints: the next
        // dark frame must run one full teardown before it may latch again.
        self.dark_settled = false;

        // The classic wake takes the frame whole. It is a separate engine that
        // spawns on observed cursor motion, so it runs before the licensed
        // spawn/admission/emit machinery (whose shape gates would draw nothing
        // for a screen-crossing jump, `DECLINE_OFF_SHAPE`) and returns in its
        // place. It draws none of this engine's light, and settles whatever
        // light another style left live (below).
        if matches!(cfg.style, GlowStyle::Classic) {
            // The mono face reads the rows flanking the caret's (its rail and
            // its over-ink account), so its host captures them.
            self.last_classic_mono = cfg.classic_mono;
            // The erase licences expire only in `poof_scan`, which this branch
            // never reaches, and the classic engine never consumes them; left
            // armed they would keep `is_active` and the poof poll waking for the
            // rest of the session. Dropped, since this engine cannot draw a poof.
            self.kill_hint = None;
            self.bs_poof_hint = None;
            self.bs_baseline = None;
            // The mirror of the reset below: this engine's light, warmth and
            // crossfades, forged under the style that held the frame before
            // classic, decay only past this branch, so left here they would
            // hold `is_active`, the frame train and `trail status`'s
            // `sparks=` for as long as classic draws. A switch to classic
            // snaps them off (a style switch is a reset); once settled,
            // nothing on this branch lights them again, so the check is all
            // a classic frame pays. (The poof licences just went; v2 let go
            // of the frame above.)
            if self.modern_needs_frame_cadence() || !self.vapor.is_empty() || self.ember_live() {
                self.clear_modern_transient_state();
                self.clear_thermals();
            }
            // Cursor tracking follows the caret here as on the dark path, so
            // a return to another style spawns no comet from where the caret
            // stood before classic took the frame.
            self.last = cur;
            self.last_visible = cur.map(|c| (c, now));
            self.hide_bridge_shown = None;
            // The classic wake sounds (the phaser's `pew`) and its key seam
            // obeys the common law. It mints no echo cues, so the key-time click
            // is its only sound; the timbre snapshot uses the same config.
            self.store_key_timbre(cfg);
            self.settle_key_seam(cfg, cfg.intensity > 0.0);
            // A light ground's wake is ink under the letters and a veil
            // ring, so the classic engine fills the under-ink and halo
            // streams the host splices for every style (already cleared
            // above).
            // The mono face's dark bed spends one over-ink account with the
            // bodies and the glow the host lays after this tick, over the
            // text this frame's probe shows (the poof scan, which rotates it
            // for the other styles, is not the classic wake's).
            self.rotate_row_probe();
            self.begin_ink_ledger(cfg, geom, cur);
            let spans = self.probed_text_spans();
            let fp = self.classic.tick(
                cur,
                now,
                cfg,
                geom,
                out,
                &mut self.halo_out,
                &mut self.under_out,
                crate::classic_wake::WakeInk {
                    text: TextView::new(spans, Some(&self.frame_text)),
                    ledger: &mut self.ink_ledger,
                },
            );
            // Its spawns count and log like every style's, so `trail` and
            // `trail status` show what it drew.
            if let Some((from, to)) = self.classic.take_spawn() {
                self.spawns += 1;
                self.log_licensed_as(now, from, to, AdmissionRecord::LICENCE_MOTION);
            }
            return fp;
        }
        // Any other style owns this frame, so the classic engine's light and
        // its cursor memory go: its decay runs only in its own tick, so light
        // left here would hold `is_active` (and the frame train) for good, and
        // a stale `last` would draw a comet from the old caret when classic
        // returns. A style switch away from classic snaps it off rather than
        // crossfading it (a style switch is a reset). `last_style` never
        // records Classic, so the `style_changed` edge below cannot see this.
        if self.classic.is_active() || self.classic.has_last() {
            self.classic.reset();
        }

        if self.rng == 0 {
            self.rng = 0x9E37_79B9;
        }
        // The classic field is geometry-derived. There is no phase seed or
        // colour latch to initialize here, so navigation before the first key
        // cannot make the visible rainbow stale.

        // Style switch mid-animation. In-flight sparks, particles, bolts and
        // meteors were forged under the old style; re-rendering them through
        // the new emit path would pop and reshape them, and dropping them would
        // darken the screen and disarm the animation train. So the live state
        // moves into an [`OutgoingFade`] that ticks under its snapshotted old
        // config (decay and drift only) behind a ~250 ms cosine ramp-down, while
        // the new style ramps in over ~120 ms (`ramp_in_at`).
        //
        // Typing warmth (heat, flare, coal, quench, eased temperatures) carries
        // into the new style; coordinate tracking and input hints survive, so no
        // phantom comet spawns and a pending key→echo pairing still lands.
        // Pending sound cues are dropped at the edge. Chained switches hand off
        // at the level reached (`OutgoingFade::level0`), capped at
        // [`Self::FADE_CAP`].
        if self.last_style.is_some_and(|s| s != cfg.style) {
            self.begin_style_fade(now);
        }
        self.last_style = Some(cfg.style);
        self.last_classic_mono = false;
        // Compare-on-write: `last_cfg` backs one cold read (the fade snapshot
        // in `begin_style_fade`). After every enabled tick
        // `last_cfg == Some(*cfg)` holds; the steady state pays a compare
        // instead of a copy. A NaN field compares unequal and re-stores.
        if self.last_cfg.as_ref() != Some(cfg) {
            self.last_cfg = Some(*cfg);
        }
        // Incoming ramp-in (~120 ms): scale the live amplitude so the new style
        // rises under the outgoing fade. Without a recent switch
        // `ramp_in_scale` is exactly 1.0 and `cfg` is used untouched.
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
                // Fire cools on its own slower τ; other styles use 0.9 s. Shared
                // with the key-time click's timbre prediction via `heat_tau`.
                let heat_tau = Self::heat_tau(cfg);
                self.heat *= (-dt / heat_tau).exp();
                if self.heat < 0.005 {
                    self.heat = 0.0;
                }
                self.flare *= (-dt / Self::FLARE_DECAY_TAU).exp();
                if self.flare < 0.005 {
                    self.flare = 0.0;
                }
                // Emberforge thermal evolution (fire only; others' values are
                // zero). Integrators decay first, then the eased display
                // temperature chases the new target, so one tick's consumers
                // read one coherent temperature.
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
                    // An active quench accelerates the die-down (up to ~3×); a
                    // natural pause keeps the slower release.
                    Self::DISP_RELEASE_TAU / (1.0 + 2.0 * self.quench)
                };
                self.disp_t += rise * (1.0 - (-dt / ease).exp());
                if self.disp_t < 0.005 {
                    self.disp_t = 0.0;
                }
                // The flame field's clock integrates churn-weighted time so its
                // motion accelerates smoothly with temperature, wrapped on
                // `FLAME_PHASE_RING`.
                self.flame_phase += dt * (1.6 + 2.2 * self.disp_t);
                if self.flame_phase >= Self::FLAME_PHASE_RING {
                    self.flame_phase -= Self::FLAME_PHASE_RING;
                }
                if matches!(cfg.style, GlowStyle::Fire) {
                    // Cursor metal chases the display temperature with
                    // hysteresis; a full quench halves the cooling τ.
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
        // The forge fill reads its band of the ramp for this page.
        self.refresh_forge_band(cfg);
        // The key-time click's timbre inputs, written by every tick past the
        // master switch (the degenerate-grid return and the classic wake store
        // them through [`Self::store_key_timbre`] too), including ticks about to
        // return dark for motion or performance reasons. Both
        // [`Self::settle_key_seam`] calls below sit under this store, so the
        // seam never opens on a stale snapshot. `heat_gain` and `heat_tau` are
        // pure functions of `cfg` that ignore `intensity`, so the ramp shadow
        // cannot move them; sited right after the lazy decay so a cue reads a
        // coherent thermal snapshot.
        self.store_key_timbre(cfg);

        // Zero amplitude (unfocused, motion-reduced, or a real 0 intensity):
        // emit no light and spawn nothing, but keep the long-lived thermal
        // integrators (coal, cursor_temp, heat, flame_phase) that the lazy
        // cooling already aged. A focus blip costs only its own duration of
        // cooling; a long unfocus cools to exactly zero, so `is_active` disarms.
        // Only `reset()` wipes momentum. Moving light and the transient
        // candidate/classifier cohort are dropped; cursor position is tracked so
        // refocus spawns no phantom comet.
        if cfg.intensity <= 0.0 {
            // Crossfade residue and pops are moving light, dropped while dark
            // (the ramp-in floor keeps a genuine switch off this path).
            //
            // The key seam does not close with the light: it asks
            // [`GlowConfig::audible`], so dark for a motion or performance
            // reason is still heard, while dark because the key belongs to
            // another window is not. No `clear_thermals` here: the lazy decay
            // already cooled the integrators. The sound ledger goes only when
            // the seam goes ([`Self::settle_key_seam`]); wiping credits on every
            // shed frame would double-click the burst when the frame relit.
            self.settle_key_seam(cfg, false);
            self.clear_transient_state();
            self.track_shown_caret(cur, now);
            return 0;
        }
        // Past both dark returns this tick draws, so the key seam is open. Only
        // the master-off return shuts it unconditionally; the degenerate grid,
        // classic wake and zero amplitude ask `cfg.audible` through
        // [`Self::settle_key_seam`]. The timbre constants were stored above.
        self.settle_key_seam(cfg, true);

        self.expire_seam_holds(now);
        // A scroll that left the hand's row in doubt is settled by the caret
        // before anything this frame reads the row ([`HandRow::settle`]).
        if let (Some(hand), Some((caret_row, _))) = (self.hand_row.as_mut(), cur) {
            hand.settle(caret_row);
        }
        // The hand rides its line ([`Self::lift_hand_row`],
        // [`rk::Engine::lift_hand`]): with a key in flight, a caret shown on a
        // neighbouring row asks the follow pass first whether the hand's own
        // line was repainted there with it (a composer's box grown or shrunk a
        // row with no scroll, under the key's echo), judged on the hand's typed
        // glyphs with the caret as a second witness. If so the move is judged
        // in the text's own frame: the key's same-row echo. A relocation with
        // no key behind it is the program's, and keeps its own judgment.
        // On a scroll's frame the neighbouring row is the caret's own screen
        // row, the scroll's rows below where the scroll carried the hand: a
        // box redrawn in place under a tool result. Its line returned with
        // it, key or no key, and the caret did not move: nothing is judged, a
        // key between press and echo keeps its press for the echo instead of
        // buying the scroll's carry, and the hand's run returns with the hand
        // however short (one glyph, the caret its second witness), where the
        // follow pass alone wants two. A line that scrolled away with the grid
        // still holds the hand's glyphs where the scroll put them, so the
        // witness names no return for it.
        // A caret moved past the follow offsets with no scroll to say so (the
        // caret's carry, [`rk::Engine::carry_caret`]: a box pushed down by a
        // transcript that has not filled the screen, or pushed to the bottom
        // and scrolled the rest) is asked the same, key or no key: a key
        // between press and echo keeps its press for the echo instead of
        // buying the push, and a one-glyph line rides the box.
        // The move starts at the last caret seen, or, after a repaint's hidden
        // frames, at the last one shown (the hide bridge's own anchor).
        let key_in_flight = self.key_in_flight(now);
        let shown = self
            .last
            .or_else(|| self.last_visible.map(|(cell, _)| cell));
        // THE CARET'S CARRY: the rows the caret moved from where it last
        // stood, which the follow pass searches when that is past its offsets
        // and not the scroll's return. Set before the hand's line is asked
        // here, and again for the follow pass below, which ends it.
        let caret_carry = match (shown, cur) {
            (Some((pr, _)), Some((cr, _))) => {
                i16::try_from(i32::from(cr) - i32::from(pr)).unwrap_or(0)
            }
            _ => 0,
        };
        let carried = caret_carry.unsigned_abs() > 2
            && (!scroll_frame || i32::from(caret_carry) != i32::from(scroll_rows));
        if witness_n > 0 {
            self.v2.carry_caret(caret_carry);
        }
        let mut lifted = false;
        if self.v2.engaged()
            && witness_n > 0
            && let (Some((pr, _)), Some((cr, _))) = (shown, cur)
            && (key_in_flight || scroll_frame || carried)
            && if carried {
                true
            } else if scroll_frame {
                cr.checked_sub(pr) == Some(scroll_rows)
            } else {
                pr.abs_diff(cr) == 1
            }
        {
            let (samples, n) = witness_samples(&self.witness_rows, witness_n);
            if let Some(run) = self.v2.hand_line_follow(&samples[..n], Some(cr))
                && run.row == pr
                && pr.checked_add_signed(run.dr) == Some(cr)
            {
                self.lift_hand_row(pr, cr);
                self.v2.lift_hand(run);
                lifted = true;
            }
        }
        // A delivered insert echoed in a scroll's frame (a paste drawn in the
        // same update as a tool result) returns on its own evidence, which
        // holds in an empty composer too, with no typed glyph to follow: the
        // caret back on its screen row exactly the priced insert's width right
        // of where the scroll carried it from, on the row the insert was armed
        // on. The echo is then judged in its line's own frame, the same-row
        // sweep the insert pays for.
        if !lifted
            && self.v2.engaged()
            && let (Some((pr, pc)), Some((cr, cc))) = (shown, cur)
            && scroll_frame
            && cr.checked_sub(pr) == Some(scroll_rows)
            && let Some(ins) = self.insert.fresh(now)
            && ins.known
            && ins.row == Some(pr)
            && cc.checked_sub(pc) == Some(ins.cells)
        {
            self.lift_hand_row(pr, cr);
            self.v2.lift_caret(pr, cr);
        }
        if !key_in_flight
            && let (Some((pr, pc)), Some((cr, cc))) = (shown, cur)
            && pc == cc
            && (1..=2).contains(&pr.abs_diff(cr))
            && self
                .row_cur_meta
                .is_some_and(|m| m.row == cr && m.at == now)
            && let Some(run) = self.recent_typed_run.as_mut()
            && run.row == pr
            && run.end() == cc
            && (0..usize::from(run.len)).all(|i| {
                self.row_cur
                    .get(usize::from(run.col0) + i)
                    .copied()
                    .unwrap_or(' ')
                    == run.glyphs[i]
            })
        {
            run.row = cr;
        }
        // The caret goes hidden: keep the last probe that showed it.
        if cur.is_none()
            && let Some(meta) = self
                .row_prev_meta
                .filter(|m| self.last == Some((m.row, m.caret)))
        {
            self.row_shown.clone_from(&self.row_prev);
            self.row_shown_meta = Some(meta);
        }
        // Spawn on a real move between two visible positions — where "visible"
        // bridges ConPTY's per-echo hide window ([`Self::hidden_bridge_source`]).
        let spawn_from = self
            .last
            .or_else(|| self.hidden_bridge_source(cur, now, cfg, geom));
        self.judge_observed_caret(spawn_from, cur, now, cfg, geom);
        self.carried_key_move = None;
        // The hidden/parked-caret echo lane: when the cursor cannot witness the
        // echo (hidden across frames, or parked on another row), the host-fed
        // print anchor is the licensed move's mutation site. Runs after the
        // move lane so a visible move owns its own tick.
        let cursor_move_observed = spawn_from
            .zip(cur)
            .is_some_and(|((pr, pc), (cr, cc))| pr != cr || pc != cc);
        // The paid pending-wrap witness is read by the fold judged above and
        // consumed by any observed caret move.
        if cur.is_some() && cur != self.last_visible.map(|(cell, _)| cell) {
            self.wrap_paid_row = None;
        }
        let content_echo = self.same_caret_typed_echo(cur, cursor_move_observed, now, cfg, geom);
        self.echo_anchor_pass(cur, cursor_move_observed || content_echo, now, cfg, geom);
        self.track_shown_caret(cur, now);
        self.settle_torn_lay(cur, now);

        // Erase poof detection: a fresh kill hint paired with a same-row net
        // shrink of the probed row (stable prefix and suffix) puffs smoke off
        // the vanished span, before decay/emit so the puffs join this frame.
        // Runs off the host row probe ([`Self::observe_row`]); hosts that never
        // probe or arm the hint pay two `Option` reads.
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
        // A hot cursor smokes: above `SMOKE_TEMP`, wisps shed off its top edge
        // at a temperature-scaled rate, each with its own drift and life. Stops
        // as it cools, so vapor drains to idle.
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
        // Arrival-time strike: flare, landing ring and ember fountain fire when
        // the meteor head lands. The fountain follows the final vector, so a
        // retargeted meteor strews debris along the coalesced path, never an
        // intermediate repaint hop.
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
        // `under_out` (the glow_under stream, composited beneath glyph ink)
        // carries the rainbow ribbon body, jump zooms and Water's fluid wake, so
        // letters draw over those broad marks at full contrast.
        let mut under = std::mem::take(&mut self.under_out);
        let mut charred = std::mem::take(&mut self.char_out);
        let mut halo_cells = std::mem::take(&mut self.fire_halo_out);
        let mut engulf = std::mem::take(&mut self.engulf_scratch);
        // Bolts first: on a huge jump the beam alone can fill the quad budget,
        // and truncation must shed beam haze, never a strike. Lightning (Laser
        // only) reuses the resident bolt scratch via `mem::take`.
        let mut bolt_verts = std::mem::take(&mut self.bolt_verts);
        let mut bolt_channel = std::mem::take(&mut self.bolt_channel);
        // Lightning composites over the ink on a dark ground and paints
        // beneath it on a light one (`emit_bolts`).
        let bolt_out = if cfg.dark_theme {
            &mut *out
        } else {
            &mut under
        };
        self.emit_bolts(now, cfg, geom, bolt_out, &mut bolt_verts, &mut bolt_channel);
        self.bolt_channel = bolt_channel;
        // Fire's over-ink account opens before its first light (the
        // streak) and every later fire emitter spends from it.
        self.begin_ink_ledger(cfg, geom, cur);
        // No-op for Water/rainbow kitty (beam=false); on light themes the
        // beam family draws its body as ink under the letters
        // (`emit_comet_light`) instead of additive quads.
        self.emit_comet(now, cfg, geom, cur, out, &mut halos, &mut under);
        out.truncate(Self::MAX_QUADS);
        // Per-pixel fire (Fire only): FirePatches render at the under-ink
        // seam through the shared byte-exact integer field. Not gated on
        // the GlowQuad budget: the flame writes the separate patch/halo
        // streams, each self-capped in `emit_flames`.
        self.emit_flames(
            now,
            cfg,
            geom,
            &mut patches,
            &mut charred,
            &mut halo_cells,
            &mut engulf,
            &mut under,
        );
        // Jump streaks (Fire only): over the ink on a dark ground,
        // beneath it on a light one, like the lightning.
        if cfg.dark_theme {
            if out.len() < Self::MAX_QUADS {
                self.emit_fire_meteors(now, cfg, geom, out);
            }
        } else if under.len() < Self::MAX_QUADS {
            self.emit_fire_meteors(now, cfg, geom, &mut under);
        }
        if under.len() < Self::MAX_QUADS {
            // Water's broad reflection belongs below glyph ink. Its crown,
            // landing ripple and free droplets remain over-ink accents.
            self.emit_water(now, cfg, geom, cur, &mut under);
        }
        if self.v2.engaged() {
            // Seam point 3 (§17.2, D14): v2 owns the rainbow's whole emit
            // phase (zooms, starburst, ribbon and starfield, fresh-ink light
            // and glyph tint, the wake) in one call at the same position
            // (after fire/water, before crown/forge/ring/particles/vapor),
            // into the same `under` / `out` / `halos` scratch.
            // `Config::from_glow` is a field copy; the beams scratch and cue
            // sink are resident. `rainbow_exit_swoosh` stays v1-only.
            //
            // §18's zero allocation per frame for the lent streams: their
            // peaks are state-dependent, so an on-demand scratch could still
            // grow long after warm-up. They are sized to their caps once, on
            // the first engaged frame; later frames pay a capacity compare.
            Self::reserve_v2_scratch(&mut under, out, &mut halos);
            if self.v2_beams.capacity() < Self::V2_BEAM_SCRATCH {
                self.v2_beams.reserve(Self::V2_BEAM_SCRATCH);
            }
            if self.v2_cues.capacity() < Self::V2_CUE_SCRATCH {
                self.v2_cues.reserve(Self::V2_CUE_SCRATCH);
            }
            let v2_cfg = rk::Config::from_glow(cfg, self.reduced_motion);
            // The follow pass: before the tick replays this frame's events,
            // the engine reads the rows the host sampled under its lock and
            // carries every run whose text moved a row (a bottom-anchored
            // composer growing without a scroll) to its glyphs' new row, with
            // clocks intact (`rk::Engine::follow_rows`). The same samples
            // feed the witness after the tick.
            if witness_n > 0 {
                let (samples, n) = witness_samples(&self.witness_rows, witness_n);
                self.v2.carry_caret(caret_carry);
                let park = self.held_park.filter(|p| !p.cross_row && p.fresh(now));
                let pair = park.map(|p| ((p.row, p.origin), (p.landing_row, p.landing)));
                // A key in flight under a withheld caret (a torn present, a
                // repaint's bracket): the follow pass carries the hand with
                // its line. A caret shown a row away was judged above.
                let (_, proved) = self.v2.follow_rows_with_park(
                    &samples[..n],
                    &asked[..asked_n],
                    now,
                    pair,
                    key_in_flight && cur.is_none(),
                );
                if proved && let Some(p) = self.held_park.as_mut() {
                    p.content_followed = true;
                }
                if let Some((from, to)) = self.v2.take_hand_lift() {
                    self.lift_hand_row(from, to);
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
            // Read the walk of the typed run this frame began off the cells
            // just laid (§46), for a revive after a rest.
            self.read_fresh_run_origin(now);
            // Seam point 11 (§17.2): the frame's cues join the host's one
            // backlog under the same refusal as `cue_sound` (a full backlog
            // drops the newest), so a non-draining host sees a bound and the
            // steady frame allocates nothing (§18).
            for cue in self.v2_cues.drain(..) {
                if self.sound_cues.len() < Self::MAX_SOUND_CUES {
                    self.sound_cues.push(cue);
                }
            }
            // The caret's stop is seated here, only on an event the caret
            // can see: its cell moved (the frame's `Move` is already
            // ingested, so `field_t` is this frame's answer at the new cell),
            // or the cell under it owns field light. Otherwise the seat is
            // held ([`Self::rainbow_head_rgb`]). `field_at` is O(1) in
            // columns.
            let owns_light = cur.is_some_and(|(r, c)| self.v2.field_at(r, c).is_some());
            if owns_light || self.v2_stop.is_none_or(|(_, at)| at != cur) {
                self.v2_stop = Some((self.v2_caret.field_t, cur));
            }
            // The content witness: after the tick that laid this frame's
            // cells, read every resident cell against the rows the host
            // sampled under its lock (with the batch applied, so a redraw
            // restoring the same text is the same text, D2) and retire on
            // the fast melt the cells whose glyph changed or went
            // (`rk::witness`). This is where the absolute-cell ribbon
            // learns its text has moved. A scroll's frame fed the follow
            // pass and the hand's seam alone ([`Self::scroll_rows`]).
            if witness_n > 0 && !scroll_frame {
                let (samples, n) = witness_samples(&self.witness_rows, witness_n);
                self.v2.witness_rows(&samples[..n], now);
            }
        } else {
            // The crown is the other styles' head light: radial light
            // (RainHalo) capped by MAX_HALOS. v2 owns Rainbow Kitty's head
            // (§7.1's caret seam, the ribbon's nozzle), so while v2 owns the
            // frame the crown neither paints nor paces
            // ([`Self::needs_frame_cadence`]).
            self.emit_crown(now, cfg, geom, cur, &mut halos, &mut under);
        }
        // The forge dressing rides `cursor_temp`, not the crown's window,
        // so it runs whether or not a crown is live.
        if out.len() < Self::MAX_QUADS {
            self.emit_forge_cursor(cfg, geom, cur, out, &mut halos);
        }
        // The landing ring and the flying grains spend the over-ink
        // account where they cross a letter (Fire's embers spend it in
        // their own emitter; an idle account admits everything): the
        // ring as one light, each grain on its own.
        let flying = !matches!(cfg.style, GlowStyle::Fire);
        if out.len() < Self::MAX_QUADS {
            let (q0, h0) = (out.len(), halos.len());
            self.emit_ring(now, cfg, geom, out, &mut halos);
            if flying {
                let to = out.len();
                self.ink_ledger
                    .admit_masked(out, q0..to, InkLedger::FLYING_FLOOR);
                self.ink_ledger
                    .admit_halos_masked(&mut halos, h0, InkLedger::FLYING_FLOOR);
            }
        }
        if out.len() < Self::MAX_QUADS {
            let (q0, h0) = (out.len(), halos.len());
            self.emit_particles(now, cfg, geom, out, &mut halos, &mut under);
            if flying {
                self.ink_ledger
                    .admit_each_quad(&mut out[q0..], InkLedger::FLYING_FLOOR);
                self.ink_ledger
                    .admit_halos_masked(&mut halos, h0, InkLedger::FLYING_FLOOR);
            }
        }
        self.emit_vapor(now, cfg, geom, &mut halos);
        self.bolt_verts = bolt_verts;
        // The halo cap (§18): every style's streams are self-capped at the
        // emitter, so the only shared pass left is the halo count.
        Self::cap_and_clear_v2_streams(&mut halos);
        self.halo_out = halos;
        self.patch_out = patches;
        self.under_out = under;
        self.char_out = charred;
        self.fire_halo_out = halo_cells;
        self.engulf_scratch = engulf;

        // Outgoing crossfades: tick each moved-out animator under its old config
        // behind the cosine ramp-down and merge its light into this frame's
        // streams (additive premultiplied light is order-independent). One
        // `is_empty` test when no switch happened. Runs before the shared
        // truncation and fp fold, so caps and fingerprints see one merged frame:
        // the glow stream never goes empty mid-switch (the SDR attack envelope
        // keeps its sustain) and the changing fade bytes keep the present train
        // fed.
        self.tick_fades(now, geom, out);

        // Bound the snapshot identically for both renderers.
        if out.len() > Self::MAX_QUADS {
            out.truncate(Self::MAX_QUADS);
        }
        self.under_out.truncate(Self::MAX_QUADS);

        // Radial halos are part of the visible frame like the quads: bound them
        // the same way for both backends before fingerprinting every plane.
        self.halo_out.truncate(Self::MAX_HALOS);
        let mut fp = FrameFingerprint::default();
        fp.glow_quads(FrameFingerprint::GLOW_OUT, out);
        fp.fire_patches(&self.patch_out);
        fp.glow_quads(FrameFingerprint::GLOW_UNDER, &self.under_out);
        fp.chars(&self.char_out);
        fp.fire_halos(&self.fire_halo_out);
        fp.rain_halos(&self.halo_out);

        // The forge cursor fill is visible state (`cursor_fill_override`), so
        // fold it in while the metal is changing, until its quantized colour
        // settles. At rest the fill is a constant ember that stays drawn,
        // so it is not folded and the idle fingerprint returns to 0.
        // Fire-gated.
        if matches!(cfg.style, GlowStyle::Fire)
            && self.cursor_temp > Self::FORGE_MIN_TEMP
            && let Some(fill) = self.forge_fill()
        {
            fp.forge_fill(fill);
        }
        fp.finish()
    }

    // ----- style crossfade (the style-switch contract in `tick`) -----

    /// Outgoing ramp-down length: long enough to read as a handoff and overlap
    /// the ramp-in without a trough, short enough that rapid switches stay
    /// legible under [`Self::FADE_CAP`]. Cosine-eased in `tick_fades`, landing
    /// at exactly 0, with a zero-slope start that blends into the old style's
    /// own decays.
    const FADE_OUT_S: f32 = 0.25;
    /// Incoming ramp-in length: the same attack family as the eased display
    /// temperatures (`DISP_ATTACK_S`), so the new style swells rather than pops.
    const RAMP_IN_S: f32 = 0.12;
    /// The ramp-in's starting amplitude. Nonzero because a zero first frame
    /// would take `tick`'s `intensity <= 0` path (reserved for a genuine zero),
    /// and a faint immediate presence makes the handoff read as a crossfade.
    const RAMP_IN_FLOOR: f32 = 0.15;
    /// Concurrent outgoing fades. With a 250 ms envelope a third overlapping
    /// switch finds the oldest fade nearly dark, so dropping it is invisible,
    /// and a scroll-wheel spin through the style menu stays O(1).
    const FADE_CAP: usize = 2;

    /// Begin an outgoing crossfade for the style live until this tick: move the
    /// in-flight collections into a boxed ghost animator, copy the thermal and
    /// phase scalars it needs, and leave the live engine's warmth in place so
    /// the incoming style does not start cold. Called from `tick` when the
    /// style guard disagrees, before `last_style`/`last_cfg` are
    /// overwritten.
    fn begin_style_fade(&mut self, now: Instant) {
        // The outgoing style hands off at the amplitude it held (a partial
        // ramp-in during a rapid switch chain). Read before re-arming the ramp.
        let level0 = self.ramp_in_scale(now);
        self.ramp_in_at = Some(now);
        // A cue spawned under the old style must not play the new style's
        // palette (the host maps cues by the current style), so drop the
        // backlog.
        self.sound_cues.clear();
        // The credits account for exactly those dropped cues; left standing
        // they would silence echo clicks whose key click never played.
        self.clear_keyed_clicks();
        let Some(cfg) = self.last_cfg else {
            // A switch requires a prior tick, which recorded its config; a
            // missing snapshot means no residue, never a fade under the wrong
            // config.
            return;
        };
        let mut ghost = Box::<CursorGlow>::default();
        // Move the forged light: the ghost owns it now; the live engine starts
        // the incoming style with empty collections (plus carried warmth).
        ghost.sparks = std::mem::take(&mut self.sparks);
        ghost.particles = std::mem::take(&mut self.particles);
        // The landing starburst is forged light like the zoom streak and
        // finishes its bloom in the ghost. A fast glide's streak is a
        // `rainbow.jumps` entry and moves with the other zooms; only the
        // velocity spine needs handling here.
        ghost.fire_meteors = std::mem::take(&mut self.fire_meteors);
        ghost.vapor = std::mem::take(&mut self.vapor);
        ghost.bolts = std::mem::take(&mut self.bolts);
        ghost.ring = self.ring.take();
        ghost.crown_until = self.crown_until.take();
        ghost.crown_window_ms = self.crown_window_ms;
        ghost.last_move = self.last_move;
        ghost.last_smoke = self.last_smoke.take();
        // Fresh-ink pops finish fading in the ghost under the outgoing config,
        // like the sparks.
        ghost.reduced_motion = self.reduced_motion;
        // Copy the thermal and phase scalars: the residue renders at the warmth
        // it was forged with, and the live engine keeps its own, so mid-typing
        // switches arrive already glowing.
        ghost.heat = self.heat;
        ghost.heat_at = self.heat_at;
        ghost.flare = self.flare;
        ghost.coal = self.coal;
        ghost.quench = self.quench;
        ghost.disp_t = self.disp_t;
        ghost.cursor_temp = self.cursor_temp;
        ghost.flame_phase = self.flame_phase;
        // The typing metric is carried warmth like `heat`: the ghost's copy
        // keeps the residue at its momentum, and the live engine keeps its own
        // so a switch into Rainbow Kitty re-attacks toward the earned value.
        ghost.momentum = self.momentum;
        // The living-flow surface carries across the fork and is not restarted
        // with the style phases: the ghost draws the same light as this frame,
        // so its swells must stay put. It is a constant-rate surface clock
        // ([`RainbowState::flow`]) the incoming style inherits.
        ghost.hue = self.hue;
        ghost.rng = self.rng;
        // The ghost's anchor: `tick_fades` feeds this cell as `cur`, so
        // `cur == last` and no move is ever observed, while positional
        // emitters keep their footing. Input hints are not copied; they belong
        // to the live style.
        ghost.last = self.last;
        // Pin the ghost's own switch guard to its config so its ticks never
        // re-detect a change (and never nest another fade).
        ghost.last_style = Some(cfg.style);
        ghost.last_cfg = Some(cfg);
        // Style-specific phases restart for the incoming style: a fresh flame
        // field or ribbon spine attacks from 0 (the built-in half of the ramp).
        self.flame_phase = 0.0;
        // Classifier state, not forged light: a stale run direction or landing
        // anchor carried across a switch would mint a landing burst at a
        // coordinate that no longer exists.
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

    /// The incoming style's ramp-in amplitude at `now`: exactly `1.0` in steady
    /// state (`ramp_in_at == None`), else a cosine ease from
    /// [`Self::RAMP_IN_FLOOR`] to 1.0 over [`Self::RAMP_IN_S`], clearing itself
    /// when complete.
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

    /// Tick every live [`OutgoingFade`] under its old config with the cosine
    /// ramp-down on its intensity, and merge the streams into this frame's
    /// output. A fade is dropped when its envelope is spent or it contributes
    /// nothing visible, so a live fade always means visible output. Ghost sound
    /// cues are always cleared (sound cannot cross styles).
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
            // Cosine ramp-down from the level the outgoing style actually held
            // (a chained switch hands off mid-ramp without popping to full).
            let env = f.level0 * (0.5 + 0.5 * (std::f32::consts::PI * u).cos());
            let mut c = f.cfg;
            c.intensity *= env;
            if c.intensity <= 0.0 {
                return false; // inert amplitude — ticking would only wipe
            }
            // `cur == engine.last` (the frozen anchor) ⇒ never a move ⇒ never a
            // spawn: the residue decays and drifts only, under its old config.
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
            // A fade that painted nothing is already invisible — retire it now
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

/// The first `n` host-sampled witness `rows` lent to the engine as
/// [`rk::witness::RowSample`]s — a stack array (the steady frame allocates
/// nothing) and how many of its slots are filled.
fn witness_samples(
    rows: &[WitnessRowBuf],
    n: usize,
) -> ([rk::witness::RowSample<'_>; CURSOR_WITNESS_ROWS], usize) {
    let mut samples = [rk::witness::RowSample {
        row: u16::MAX,
        cols: &[],
    }; CURSOR_WITNESS_ROWS];
    let n = n.min(samples.len()).min(rows.len());
    for (sample, slot) in samples.iter_mut().zip(&rows[..n]) {
        *sample = rk::witness::RowSample {
            row: slot.row,
            cols: &slot.cols,
        };
    }
    (samples, n)
}

// The family's spectrum at one cell is [`spectrum`], at every cell and every
// cadence: a cell has one position and every layer reads it (§2.1, §2.3).

#[cfg(test)]
mod tests;
