// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet driver, [`CompanionOwner`]: owns a resident pet brain per surface,
//! senses for it, decides which coat it wears, decides custody (pet vs flying
//! head vs idle) and draws it. The web pipeline and the native `WindowState`
//! run this same code (`docs/DESIGN-host-boundary-2026-08-30.md`).
//!
//! Scope cardinality: the owner borrows the frame's `WordDecorations` at
//! [`CompanionOwner::sense`] / [`CompanionOwner::emit`] and never owns one; the
//! flash-limiter root stays where it is declared.
//!
//! Three rules every host must keep (pinned by tests here):
//!
//! * The brain ticks unconditionally. A pet that cannot be drawn is fed
//!   `caret: None`; ticking inside a draw gate would freeze `needs_frames()`
//!   at its last answer.
//! * Cadence: the pet is a resident, so its cadence gate is
//!   [`crate::kitty_pet::PetBrain::needs_frames`] (something is moving), never
//!   `is_active()` (a cat exists).
//! * Seed: the look comes from
//!   [`crate::kitty_registry::KittyLook::for_launch`] over a host-minted seed,
//!   so the engine stays clockless and dieless.

use aterm_core::render::FreeSprite;
use aterm_core::terminal::RenderCell;
use aterm_time::Instant;

use crate::cat_baker::CatColorKey;
use crate::cursor_glow::GlowStyle;
use crate::host::{
    CaptureMode, HostFrameInput, PressOutcome, SingFacts, TerminalFacts, Visibility,
};
use crate::kitty_pet::{
    ART_ASPECT, ART_ROWS, PetArrival, PetBrain, PetFrame, PetInputKind, PetSense, PetSpecies,
    RoomInbox, SyncLookOutcome,
};
use crate::kitty_registry::KittyLook;
use crate::rainbow_kitty::ARM_MIN;
use crate::word_decorations::{
    CatFootprint, CompanionOnGlass, EffectGeom, PeekCue, PetCursorFrame, WordDecorations,
};

// ── custody and precedence enums ────────────────────────────────────────────

/// WHICH RUNG of the companion precedence law won a verdict — the winner
/// report the rate law reads at the render sync sites (kitty-motion §2.0.4,
/// Rungs: *"`companion_precedence` reports which arm won; the sync site maps
/// `Rung::Program => tenure.arrival()`, every other rung to Quiet"*). The
/// LOOK still travels alone (the native `App::companion_verdict` returns a
/// bare `KittyLook`); the rung rides beside it so the sync sites can tell a
/// program-rung win from a favourite or launch win — only a PROGRAM win may
/// ever carry the tenure gate's arrival ceremony.
///
/// Moved here from `aterm-gui/src/launch_kitty.rs` (Phase 1); the precedence
/// law itself — favourite > program (with tenure) > launch kitty — is
/// [`crate::companion_look::companion_precedence`] (Phase 5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompanionRung {
    /// The pinned favourite won. The user's explicit choice announces nothing:
    /// always quiet.
    Favourite,
    /// The tenured program cat won — the ONE rung whose arrival may be a
    /// ceremony, as ruled by the host's tenure gate.
    Program,
    /// The launch kitty floor won — "no stronger claim". The base cat is
    /// always home: always quiet.
    Launch,
}

/// Which companion, at most one, puts a body in this frame.
///
/// The host gates two alphas (the flying head only outside pet mode, the pet
/// whenever presentable), and an alpha is only a permission. This is custody
/// as a single value, so no emitter can draw two bodies. Every seam that must
/// agree about the companion reads it: [`CompanionOwner::emit`], the native
/// composed emitter, and [`cursor_companion_on_glass`] (so the ambient
/// word-cats' yield describes the sprite really drawn).
///
/// The pet wins a tie: it is the resident, and the flying head is never
/// admitted in pet mode, so a tie is already impossible upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompanionDuty {
    /// No companion body this frame.
    Idle,
    /// The full-body resident pet ([`crate::kitty_pet`]).
    Pet,
    /// The earned flying head ([`crate::kitty_cursor`]), escorting the caret
    /// at `cell`.
    FlyingHead { cell: (u16, u16) },
}

// ── the petting hit test (app_mouse.rs) ─────────────────────────────────────

/// How far outside the pet's drawn body (frame px, each side) a click still
/// counts as petting; the target moves, so a little grace keeps an honest aim
/// from sliding off a paw mid-walk.
pub const PET_HIT_SLOP_PX: i32 = 4;

/// Whether a pointer at `(x, y)` (frame px) lands on the pet's drawn body
/// `rect` (`(x0, x1, y0, y1)`, right/bottom exclusive), padded by `slop` on
/// every side. The petting hit test.
#[must_use]
pub fn pet_rect_hit(rect: (i32, i32, i32, i32), x: f64, y: f64, slop: i32) -> bool {
    let (x0, x1, y0, y1) = rect;
    x >= f64::from(x0.saturating_sub(slop))
        && x < f64::from(x1.saturating_add(slop))
        && y >= f64::from(y0.saturating_sub(slop))
        && y < f64::from(y1.saturating_add(slop))
}

// ── the palette sampler (app_render.rs) ─────────────────────────────────────

/// Resolve the companion's local terminal palette from every grid cell its
/// prospective sprite intersects. The explicit cap keeps this cold emission
/// path allocation-free and O(1), even under degenerate cell metrics.
#[must_use]
pub fn cursor_cat_color_key(
    cells: &[Vec<RenderCell>],
    geom: EffectGeom,
    footprint: CatFootprint,
    fallback_bg: u32,
    fallback_fg: u32,
    fallback_accent: u32,
) -> CatColorKey {
    const MAX_SAMPLES: u32 = 64;
    if geom.cell_w == 0 || geom.cell_h == 0 || cells.is_empty() {
        return CatColorKey::from_rgb(fallback_bg, fallback_fg, fallback_accent);
    };

    let cw = i64::from(geom.cell_w);
    let ch = i64::from(geom.cell_h);
    let x0 = i64::from(footprint.x).max(0);
    let y0 = i64::from(footprint.y).max(0);
    let x1 = (i64::from(footprint.x) + i64::from(footprint.w)).min(i64::from(geom.cols) * cw);
    let y1 = (i64::from(footprint.y) + i64::from(footprint.h)).min(i64::from(geom.rows) * ch);
    if x1 <= x0 || y1 <= y0 {
        return CatColorKey::from_rgb(fallback_bg, fallback_fg, fallback_accent);
    }
    let c0 = usize::try_from(x0 / cw).unwrap_or(0);
    let c1 = usize::try_from((x1 - 1) / cw).unwrap_or(usize::MAX);
    let r0 = usize::try_from(y0 / ch).unwrap_or(0);
    let r1 = usize::try_from((y1 - 1) / ch).unwrap_or(usize::MAX);

    let mut bg_sum = [0u32; 3];
    let mut fg_sum = [0u32; 3];
    let mut sampled = 0u32;
    let mut visible = 0u32;
    let mut min_background_band = 3u8;
    let mut max_background_band = 0u8;
    let mut principal_fg = None;
    let fallback_bg_rgb = [
        (fallback_bg >> 16) as u8,
        (fallback_bg >> 8) as u8,
        fallback_bg as u8,
    ];
    let pack =
        |rgb: [u8; 3]| (u32::from(rgb[0]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[2]);
    'rows: for line in (r0..=r1).map(|row| cells.get(row)) {
        for col in c0..=c1 {
            let cell = line.and_then(|line| line.get(col));
            let background_rgb = cell.map_or(fallback_bg_rgb, |cell| cell.bg);
            let background = pack(background_rgb);
            let band = CatColorKey::background_band(background);
            min_background_band = min_background_band.min(band);
            max_background_band = max_background_band.max(band);
            for (dst, src) in bg_sum.iter_mut().zip(background_rgb) {
                *dst += u32::from(src);
            }
            sampled += 1;
            if let Some(cell) = cell
                && !cell.wide
                && !cell.ch.is_whitespace()
                && cell.ch != '\0'
            {
                principal_fg.get_or_insert(cell.fg);
                for (dst, src) in fg_sum.iter_mut().zip(cell.fg) {
                    *dst += u32::from(src);
                }
                visible += 1;
            }
            if sampled == MAX_SAMPLES {
                break 'rows;
            }
        }
    }
    if sampled == 0 {
        return CatColorKey::from_rgb(fallback_bg, fallback_fg, fallback_accent);
    }
    let background = pack(bg_sum.map(|channel| (channel / sampled) as u8));
    let foreground = principal_fg.map_or(fallback_fg, pack);
    let surrounding = if visible == 0 {
        fallback_accent
    } else {
        pack(fg_sum.map(|channel| (channel / visible) as u8))
    };
    CatColorKey::from_rgb_span(
        background,
        foreground,
        surrounding,
        min_background_band,
        max_background_band,
    )
}

// ── ownership and presentation (app_render.rs) ──────────────────────────────

/// Whether this surface owns a resident pet: the config half of its
/// presentation rule, kept separate so ownership and presentation cannot drift.
///
/// The terms are the ones meaning "this surface should have a pet": the trail
/// master is on, the style is the pet style, and the resolved trail is in pet
/// mode. Excluded is everything that merely hides a pet that still exists
/// (focus, a scrolled viewport, the load-shed latch): those come back, and the
/// resident must return as the same cat. [`CompanionOwner::sense`] retires an
/// unowned pet outright before the tick.
#[inline]
#[must_use]
pub fn resident_pet_owner_present(
    pet_mode: bool,
    cursor_trail_enabled: bool,
    style: GlowStyle,
) -> bool {
    pet_mode && cursor_trail_enabled && matches!(style, GlowStyle::RainbowKitty)
}

/// Describe the one cursor companion actually drawn this frame, for the
/// ambient word-cats' pixel yield.
///
/// Driven by [`CompanionDuty`], the same value the emitters match on, so the
/// rect handed to the engine belongs to the sprite really drawn. The flying
/// head flies ~1.75 cells right of the caret column and ~0.4 rows above it, so
/// a caret band alone would let an ambient cat draw on top of it.
///
/// The resident pet's exact body is authoritative and may outlive a visible
/// caret for the bounded DECTCEM fade; then `cell` stays `None`, so the engine
/// gets real pixel ownership without a stale caret anchor.
///
/// `head_px` is the flying head's live footprint
/// ([`WordDecorations::kitty_cursor_footprint`], the rect
/// `kitty_cursor_at_placement` debug-asserts against). `None` degrades to the
/// caret band alone.
#[inline]
#[must_use]
pub fn cursor_companion_on_glass(
    duty: CompanionDuty,
    cursor: Option<(u16, u16)>,
    head_px: Option<(i32, i32, i32, i32)>,
    pet_body_px: Option<(i32, i32, i32, i32)>,
) -> Option<CompanionOnGlass> {
    match duty {
        CompanionDuty::Idle => None,
        CompanionDuty::Pet => pet_body_px.map(|body_px| CompanionOnGlass::at_body(cursor, body_px)),
        CompanionDuty::FlyingHead { cell } => Some(head_px.map_or_else(
            || CompanionOnGlass::at_cell(cell),
            |head_px| CompanionOnGlass::at_head(cell, head_px),
        )),
    }
}

/// Feed the resident pet the presentation context shared by live glass and
/// explicit capture, immediately before its frame tick.
///
/// `pane` is present only for a composed surface, and binding it must precede
/// the ink read: [`WordDecorations`] parks one scan per session, and the live
/// slot otherwise belongs to whichever pane the compose loop visited last.
/// Single-grid callers declare their scan session earlier (before
/// `needs_rescan`) and pass `None`.
pub fn prepare_resident_pet_tick(
    word_decos: &mut WordDecorations,
    pet: &mut PetBrain,
    species: PetSpecies,
    pane: Option<(u64, (i32, i32))>,
) {
    if let Some((session, px_origin)) = pane {
        word_decos.bind_pane(session, px_origin);
    }
    pet.set_species(species);
    let (spans, live) = word_decos.pet_ink();
    pet.sense_ink(0, spans, live);
}

/// The arrival mapping: how loudly this frame's verdict lands on the pet,
/// from the winning rung, the tenure gate's authorised arrival, and the pair
/// the pet is wearing. One pure function both sync sites call:
///
///   * a non-program rung (favourite, launch) is always Quiet;
///   * a program rung carries the gate's ruling (Quiet stays Quiet);
///   * an authorised Ceremony is demoted to Quiet when the incoming coat is
///     within [`crate::kitty_registry::SUFFICIENT_DIFFERENCE`] of the coat on
///     glass ([`crate::kitty_registry::coat_distance`]): a ceremony nobody can
///     see is noise. Because the commit rides the performed ceremony, the floor
///     is not stamped and the debt is kept. A pet never yet dressed (`worn`
///     `None`) keeps its ceremony.
///
/// `authorised` is the tenure gate's ruling
/// ([`crate::companion_look::KittyTenure::arrival`]).
#[must_use]
pub fn pet_arrival_for_sync(
    rung: CompanionRung,
    authorised: PetArrival,
    worn: Option<(u8, u8)>,
    incoming_coat: u8,
) -> PetArrival {
    use crate::kitty_registry::{SUFFICIENT_DIFFERENCE, coat_distance};
    if rung != CompanionRung::Program {
        return PetArrival::Quiet;
    }
    match authorised {
        PetArrival::Quiet => PetArrival::Quiet,
        PetArrival::Ceremony => match worn {
            Some(old) if coat_distance(old.0, incoming_coat) < SUFFICIENT_DIFFERENCE => {
                PetArrival::Quiet
            }
            _ => PetArrival::Ceremony,
        },
    }
}

// ── the per-frame pet facts (app_render.rs) ─────────────────────────────────

/// The pet's petting hit box for one frame: the brain's live drawn body
/// (`PetFrame::body_px`, grid-interior px) offset into frame px by the effects
/// origin (plus, on the composed path, the focused pane's pixel origin folded
/// into `origin` by the caller). `None` ("nothing drawn") clears the stash.
#[must_use]
pub fn pet_hit_rect_win(
    body: Option<(i32, i32, i32, i32)>,
    origin: (i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    body.map(|(x0, x1, y0, y1)| {
        (
            x0.saturating_add(origin.0),
            x1.saturating_add(origin.0),
            y0.saturating_add(origin.1),
            y1.saturating_add(origin.1),
        )
    })
}

/// PERK-AND-WATCH: the burst conjunction, in one pure function so
/// the law is testable without a terminal. A frame is a BURST only when the
/// pane visibly gained output (`scrolled` — new scrollback rows — or
/// `seq_advanced` — the content clock moved within one session) AND the
/// shell reports an executing command (OSC 133/633 C..D — this conjunct is
/// what keeps keystroke echo at a prompt, which also moves the content clock,
/// from ever perking the pet) AND the viewport is at the live bottom
/// (scrolled-back history is not a stream the pet can see) AND the pane is
/// NOT on the alternate screen.
///
/// A full-screen program's repaint is not output. It runs as the shell's
/// foreground command, so the Execute phase holds for its whole life, and it
/// rewrites its own cells in place — a spinner tick, a streamed token, the
/// user's own keystroke echoed into its composer all move the content clock.
/// Counted as a stream, a TUI's composer reads as a build log and the cat
/// perks at the footer. The alt screen has no scrollback for a stream to
/// grow, so nothing on it is a burst.
#[must_use]
pub fn pet_output_burst(
    scrolled: bool,
    seq_advanced: bool,
    shell_executing: bool,
    live_bottom: bool,
    alt_screen: bool,
) -> bool {
    (scrolled || seq_advanced) && shell_executing && live_bottom && !alt_screen
}

/// The wrap fact: one edge-detect over the emulator's autowrap serial, shared
/// by both production feeds. `wrapped` is true only when the same session's
/// serial changed since this surface's last read; a session switch or first
/// read only stores the baseline. `!=` rather than `>`, because main/alt
/// buffer swaps keep per-grid serials (`Terminal::wrap_serial`): inequality
/// costs at most one spurious wrap at a swap, where ordering would go blind.
pub fn wrap_fact_edge(seen: &mut Option<(u64, u64)>, session: u64, serial: u64) -> bool {
    let wrapped = matches!(*seen, Some((sid, s)) if sid == session && s != serial);
    *seen = Some((session, serial));
    wrapped
}

/// Pointer play: map the frame-px pointer onto the pet's pane as a fractional
/// cell `(col, row)` (pointer minus the pane's frame-space origin, as in
/// `pet_hit_rect_win`, over the cell metrics). `None` once the pointer leaves
/// the pane's grid.
#[must_use]
pub fn pet_pointer_cell(
    pointer_px: (f64, f64),
    origin: (i32, i32),
    cell: (usize, usize),
    grid: (usize, usize),
) -> Option<(f32, f32)> {
    let (cw, ch) = cell;
    if cw == 0 || ch == 0 {
        return None;
    }
    let col = (pointer_px.0 - f64::from(origin.0)) / cw as f64;
    let row = (pointer_px.1 - f64::from(origin.1)) / ch as f64;
    (col >= 0.0 && row >= 0.0 && col < grid.0 as f64 && row < grid.1 as f64)
        .then_some((col as f32, row as f32))
}

// ── the custody law (app_render.rs) ─────────────────────────────────────────

/// Whether the authored open-mouth singing face is live. This expression
/// threshold does not transfer custody away from the resident pet.
/// A non-finite drive reads as not live.
#[must_use]
pub fn sing_face_live(drive: f32) -> bool {
    drive >= 0.33
}

/// Resolve [`CompanionDuty`] from this frame's two already-gated alphas.
#[must_use]
pub fn cursor_companion_duty(
    pet_on_glass: bool,
    kitty_alpha: u8,
    cursor: Option<(u16, u16)>,
) -> CompanionDuty {
    if pet_on_glass {
        return CompanionDuty::Pet;
    }
    match cursor {
        // The head has no independent placement: it can claim glass only while
        // a visible cursor cell exists to escort.
        Some(cell) if kitty_alpha > 0 => CompanionDuty::FlyingHead { cell },
        _ => CompanionDuty::Idle,
    }
}

/// Cursor companions are active-grid decorations. Keep the broader decoration
/// lifecycle running while history is visible, but never project the flying
/// body or resident pet over retained rows.
#[must_use]
pub fn cursor_companion_presentable(decoration_presentable: bool, live_viewport: bool) -> bool {
    decoration_presentable && live_viewport
}

/// THE RESIDENT'S SHED CADENCE. The load-shed latch exists to cut per-present
/// cost (bloom, aurora, comet); the resident costs one sprite. So the shed
/// never reaches the resident's POSTURE — only the motion policy (OS Reduce
/// Motion, `motion = "reduced"`, an unfocused window) pins it at its station.
/// Folding the latch into that pin teleported a running cat to its station
/// (5.4-6.4 cells in one frame, Run→Sit) on every latch edge, and a flapping
/// latch repeated it: the owner's "glitching". Under the shed the resident
/// only leaves the frame train: no wake of its lands sooner than this after
/// its last tick — a moving body is offered exactly there, a settled one its
/// next visible step no sooner (unfloored, a breathing seat woke the shed
/// loop at 16-20 Hz, ~70 % of its offers under 50 ms apart). The brain's
/// motion is dt-based (dt clamps at 0.1 s), so ~20 Hz stays continuous.
pub const RESIDENT_SHED_CADENCE: aterm_time::Duration = aterm_time::Duration::from_millis(50);

/// Real surface custody for the full resident. Pressure affects its cadence,
/// never whether the body belongs to this surface.
#[inline]
#[must_use]
pub fn resident_pet_surface_presentable(
    focused: bool,
    unobscured: bool,
    live_viewport: bool,
    reading_interest: bool,
) -> bool {
    focused && unobscured && (live_viewport || reading_interest)
}

/// The load-shed half of flying-head admission: a presentable head stays
/// admitted while the envelope is finite and above zero.
#[inline]
#[must_use]
pub fn shed_companion_presentable(base_presentable: bool, envelope: f32) -> bool {
    base_presentable && envelope.is_finite() && envelope > 0.0
}

/// Apply the adaptive load-shed envelope at the final companion presentation
/// seam. The companion brains retain their unscaled state; only the copied
/// frame handed to the renderer is attenuated.
#[inline]
#[must_use]
pub fn shed_companion_alpha(alpha: u8, envelope: f32) -> u8 {
    let envelope = if envelope.is_finite() {
        envelope.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (f32::from(alpha) * envelope).round() as u8
}

/// Whether the soft-shed envelope still owes another presentation. The latch
/// direction matters at the endpoints: fade-out stops at zero, while fade-in
/// must start from that exact-zero edge and stops only at full amplitude.
#[inline]
#[must_use]
pub fn shed_envelope_transitioning(shed_active: bool, envelope: f32) -> bool {
    if !envelope.is_finite() {
        return false;
    }
    if shed_active {
        envelope > 0.0
    } else {
        envelope < 1.0
    }
}

/// Suppress the flying companion's exit flourish on a pet-mode frame copy.
/// The resident keeps custody even when the hidden flying episode winds down.
pub fn pin_pet_mode_exit(pet_mode: bool, frame: &mut crate::kitty_cursor::CatFrame) {
    if pet_mode {
        frame.exit = crate::kitty_cursor::CatExit::Plain;
    }
}

/// The resolved `0x00RRGGBB` colours the palette sampler falls back to when
/// the cells under the pet's prospective body carry no ink (the native
/// emitter's `cursor_cat_color_key` arguments: `default_bg, cursor_color,
/// accent`).
///
/// `cursor` is the terminal's cursor colour (native `cursor_color_u32`), not
/// the default foreground: over blank glass the pet keys off the caret it sits
/// beside, as the native app does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContrastFallback {
    /// The terminal ground.
    pub bg: u32,
    /// The terminal's cursor colour.
    pub cursor: u32,
    /// The trail accent.
    pub accent: u32,
}

// ── the owner ───────────────────────────────────────────────────────────────

/// The trail owner's verdict as the host resolved it: trail master on, parsed
/// style, and whether the resolved trail is in pet mode (native: from the trail
/// presentation; web: `GlowStyle::style_names_any_pet` over the raw spelling,
/// since the parsed enum cannot tell `rainbow kitty` from `rainbow kitty pet`).
#[derive(Clone, Copy, Debug, Default)]
pub struct GlowOwnership {
    pub enabled: bool,
    pub style: GlowStyle,
    pub style_raw_names_pet: bool,
}

/// The room's facts: what the resident is told about its session, as plain
/// data the host restates each frame (the brain diffs each fact, so a
/// restated level costs nothing). Native reads them from its session (status
/// phase, streaming sibling pane, turn lease, turn ledger, inbox); the web
/// passes none.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RoomFacts {
    /// The session's status phase is `Quiet`: a foreground job, nothing
    /// printing recently.
    pub quiet: bool,
    /// The caret of a sibling pane streaming this frame, in the pet's pane
    /// cells `(col, row)`; `None` in a single pane and on quiet frames.
    pub sibling: Option<(f32, f32)>,
    /// The session's own facts; `None` when the host no longer holds the
    /// session, which tells the pet nothing but "nobody is driving".
    pub session: Option<SessionRoom>,
}

/// The facts only a live session has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionRoom {
    /// A turn lease is live: an agent is driving this session.
    pub lease: bool,
    /// The newest turn's id and whether it settled; `(0, false)` for none.
    pub turn: (u64, bool),
    /// The inbox as the pet sees it.
    pub inbox: RoomInbox,
}

/// One frame's inputs to [`CompanionOwner::prepare`]: the emulator's facts,
/// the host's frame, the trail owner, the sing-along coupling, and the
/// surface's own presentability.
#[derive(Clone, Copy, Debug)]
pub struct PetFacts<'a> {
    pub facts: &'a TerminalFacts,
    pub host: &'a HostFrameInput,
    pub glow: GlowOwnership,
    pub sing: SingFacts,
    /// The surface has focus (the native host passes its cursor-effect focus,
    /// which the typed wake and a recording can pin).
    pub focused: bool,
    /// An app surface covers the grid (the native command palette, Settings,
    /// the tab menu): the resident stays owned but is not presented. The web
    /// has none.
    pub obscured: bool,
    /// A composed surface's focused pane: its session and pixel origin in the
    /// window grid. Binding it precedes the ink read (see
    /// [`prepare_resident_pet_tick`]). `None` for a single grid.
    pub pane: Option<(u64, (i32, i32))>,
    /// The room the pet lives in, `None` for a host that has none.
    pub room: Option<RoomFacts>,
}

/// The half-sensed frame [`CompanionOwner::prepare`] hands back: everything the
/// tick needs, plus the completion edge the host may act on.
///
/// Split so a host can act between the notes and the tick: the grief gate
/// reads [`CompanionOwner::grieving`] there, and the native host forwards the
/// glow's flow entry there (notes, grief, flow, tick).
#[derive(Clone, Copy, Debug)]
#[must_use = "a prepared frame is ticked by `CompanionOwner::tick`"]
pub struct PetTick {
    sense: PetSense,
    capture: CaptureMode,
    /// The base presentation gate (owned, focused, unobscured, live).
    visible: bool,
    pet_mode: bool,
    sing: SingFacts,
    shed_envelope: f32,
    /// The load-shed latch: it changes the resident's cadence, never its motion.
    shed_active: bool,
    /// The painted caret: the one a companion may claim a cell at.
    painted_caret: Option<(u16, u16)>,
    origin: (i32, i32),
    /// The focused surface's place on glass: the glass a body drawn this
    /// frame stands on.
    glass: PetGlass,
    /// THIS frame's completion edge in the surface's session:
    /// `(exit code, exec duration ms)`, `Some` exactly once per completion,
    /// never on a session switch, never on a capture.
    pub completion: Option<(i32, Option<u64>)>,
}

#[cfg(test)]
impl PetTick {
    /// The frame's burst verdict ([`pet_output_burst`]) as the owner resolved
    /// it — the brain's own copy is not refreshed on a tick it returns from
    /// early, so a replay that projects the law reads it here.
    pub(crate) fn output_burst(&self) -> bool {
        self.sense.output_burst
    }
}

/// What one frame resolved — the frame the emitter draws, the custody
/// verdict, and the yield box the word engine avoids.
#[derive(Clone, Copy, Debug)]
pub struct CompanionFrame {
    /// The brain's resident frame, retaining its alpha under load pressure.
    pub pet: PetFrame,
    pub duty: CompanionDuty,
    /// The pet is drawn this frame: owned, presentable, custody won, alpha > 0.
    pub on_glass: bool,
    /// The pet's live body in grid px (`PetFrame::body_px`), `None` when it
    /// is not on glass.
    pub body_px: Option<(i32, i32, i32, i32)>,
    /// The companion handed to `WordDecorations::tick` as the frame's
    /// one-cat-per-caret yield. The flying head's own rect is resolved by
    /// the route that draws it, which builds this with
    /// [`cursor_companion_on_glass`] itself.
    pub companion: Option<CompanionOnGlass>,
    /// `PetFrame::fp` — non-zero while anything rides the lane, byte-stable
    /// once nothing does.
    pub fp: u64,
    /// THE GLASS a ghost of this frame stands on when it is not this frame's
    /// surface (`PetDepartureSprite::own_glass`): the pane the focus left,
    /// still on glass where the body was drawn. `None` when no such ghost
    /// rides the lane. [`CompanionOwner::emit_on`] draws that ghost there.
    pub glass: Option<PetGlass>,
}

/// ONE TERMINAL SURFACE'S PLACE ON GLASS: its session, and where its cell
/// grid sits in the WINDOW grid — the pixel origin of its cell `(0, 0)`
/// (`PetFacts::pane`'s origin; `(0, 0)` for a single grid) and its size in
/// cells. The frame a resident drawn on it stands in, and what a host
/// offers ([`CompanionOwner::offer_glass`]) to say which panes are on glass
/// beside the focused one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetGlass {
    pub session: u64,
    pub origin: (i32, i32),
    pub cols: u16,
    pub rows: u16,
}

/// The pet's per-session latches. Every key carries the session, so a tab or
/// pane switch re-baselines silently.
#[derive(Clone, Copy, Debug, Default)]
struct Latches {
    /// `(session, completed_command_seq)`: the once-per-completion latch, the
    /// pet's own (separate from any rain latch).
    cmd_seq: Option<(u64, u64)>,
    /// `(session, content_seq)`: the perk-and-watch burst latch, so a frame
    /// can tell "the pane wrote" from "the pane repainted".
    content_seq: Option<(u64, u64)>,
    /// `(session, wrap_serial)`: the wrap-fact latch, compared with `!=` (see
    /// [`wrap_fact_edge`]).
    wrap_serial: Option<(u64, u64)>,
    /// `(session, (caret, painted))` as the last present the program had
    /// FINISHED showed them — what a torn frame's hidden write cursor is
    /// read as (see [`CompanionOwner::prepare`]).
    finished_caret: Option<(u64, CaretReading)>,
}

/// A caret as the pet senses it: the cell, and whether it is painted.
type CaretReading = (Option<(u16, u16)>, bool);

/// THE ONE PET DRIVER. Owns the brain, the identity verdict and the frame
/// latches; borrows the frame's [`WordDecorations`] at `prepare`/`emit`.
///
/// Per frame, in order: [`Self::observe_console`] (the world under the pet),
/// [`Self::prepare`] (ownership switch, notes, edges, focus, species and ink),
/// the host's work between notes and tick, [`Self::tick`] (unconditional tick,
/// custody, hit rect) before the word engine's tick so the yield box is this
/// frame's body, then [`Self::emit`] after it into the host's free-sprite
/// scratch.
pub struct CompanionOwner {
    pet: PetBrain,
    species: PetSpecies,
    /// The host's opt-in (`set_enabled_seed`); `false` at construction is the
    /// byte-identical-off posture.
    enabled: bool,
    /// The `(coat, iris)` verdict the pet is dressed from: the launch look at
    /// `set_enabled_seed`, the native verdict at `set_look`.
    pair: (u8, u8),
    arrival: PetArrival,
    rung: CompanionRung,
    latches: Latches,
    pointer_px: Option<(f32, f32)>,
    /// This frame's drawn body in FRAME px, cleared on every frame the pet is
    /// not drawn so a stale rect can never eat a click.
    hit_rect: Option<(i32, i32, i32, i32)>,
    /// The alpha the last tick put on glass (`0` = nothing).
    last_alpha: u8,
    /// The base presentation gate of the last tick — the word-cat bat reaches
    /// only a pet that could be seen.
    last_visible: bool,
    /// The resolved posture of the last frame. Static presentation does not
    /// inherit the brain's full-motion settle clocks or animation offers.
    last_static: bool,
    /// The instant of the last tick when it ran under the load-shed latch:
    /// the resident then owes no frame train, and no offer of its lands
    /// sooner than [`RESIDENT_SHED_CADENCE`] from here.
    last_shed_tick: Option<Instant>,
    /// The grid the last `prepare` resolved against — what `emit` bakes with.
    geom: EffectGeom,
    /// The pair and mapped arrival the last `emit` synced — the hello seam's
    /// inputs, kept so the host can ask `Self::commit_hello_due`.
    last_sync: Option<((u8, u8), PetArrival)>,
    /// The pixel frame — pane origin and cell metrics — the last tick DREW
    /// the resident in; `None` when it put nothing on glass.
    drawn_frame: Option<PixelFrame>,
    /// The pixel frame a pending re-sighting ghost was drawn in
    /// ([`Self::resight_coordinate_space`]). The ghost stands for pixels the
    /// user saw, so the next tick spends it only where those pixels still
    /// are: the same frame, or its own glass beside the new surface.
    resight_frame: Option<PixelFrame>,
    /// The palette the pending ghost's body was baked with, captured with
    /// [`Self::resight_frame`]: a ghost on its own glass is drawn in it
    /// ([`OwnGlass::colors`]), not in the one the arriver samples in the
    /// other pane.
    resight_colors: Option<CatColorKey>,
    /// The pixel frame of a ghost standing on its OWN glass — the pane the
    /// focus left ([`PetBrain::displace_resight`]) — for as long as it fades
    /// there. Every tick re-asks the host whether that glass is still on
    /// glass as drawn ([`Self::offer_glass`]).
    own_glass: Option<OwnGlass>,
    /// The own glass of a ghost that was still fading at the last boundary
    /// (`PetBrain::come_home`): if the next tick's frame is that glass
    /// exactly as drawn, the focus came back to it and the ghost keeps
    /// fading there — or, standing at the arriver's seat, is the arriver.
    home_glass: Option<PixelFrame>,
    /// This frame's host vouched that the glass the ghost wants
    /// ([`Self::resight_frame`] pending, else [`Self::own_glass`]) is on
    /// glass exactly as it was drawn. Consumed by every tick: a host that
    /// offers nothing spends no ghost off its own surface.
    glass_offered: bool,
}

/// A ghost's own glass ([`CompanionOwner::own_glass`]): the pixel frame it
/// was drawn in and the palette it was drawn with.
#[derive(Clone, Copy, Debug)]
struct OwnGlass {
    frame: PixelFrame,
    colors: Option<CatColorKey>,
}

/// Where a resident's cells land: the frame-pixel origin of its grid, the
/// cell metrics, and the surface's place on glass. Two frames whose origin
/// and metrics agree draw the same cell at the same pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelFrame {
    origin: (i32, i32),
    cell: (u16, u16),
    glass: PetGlass,
}

impl Default for CompanionOwner {
    fn default() -> Self {
        let base = KittyLook::default();
        Self {
            pet: PetBrain::default(),
            species: PetSpecies::Cat,
            enabled: false,
            pair: (base.coat, base.iris),
            arrival: PetArrival::Quiet,
            rung: CompanionRung::Launch,
            latches: Latches::default(),
            pointer_px: None,
            hit_rect: None,
            last_alpha: 0,
            last_visible: false,
            last_static: false,
            last_shed_tick: None,
            geom: EffectGeom::default(),
            last_sync: None,
            drawn_frame: None,
            resight_frame: None,
            resight_colors: None,
            own_glass: None,
            home_glass: None,
            glass_offered: false,
        }
    }
}

impl CompanionOwner {
    /// An owner the host dresses every frame through [`Self::set_look`] (the
    /// native verdict, which already carries the launch look) rather than
    /// through the seed. Enabled from birth; native ownership is resolved per
    /// frame in [`Self::prepare`].
    #[must_use]
    pub fn dressed_by_host() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Seed: `look = KittyLook::for_launch(seed)` dresses the pet (rung
    /// `Launch`, always quiet); `enabled = false` retires it outright
    /// ([`PetBrain::retire_unowned`]), so the off state costs and draws nothing.
    pub fn set_enabled_seed(&mut self, enabled: bool, seed: u64) {
        let look = KittyLook::for_launch(seed);
        self.pair = (look.coat, look.iris);
        self.arrival = PetArrival::Quiet;
        self.rung = CompanionRung::Launch;
        self.enabled = enabled;
        if !enabled {
            self.retire();
        }
    }

    /// The NATIVE verdict dresses the pet ([`crate::companion_look`]): the
    /// pair the precedence law chose, the tenure gate's authorised arrival,
    /// and the rung that won (only `Program` may carry a ceremony).
    pub fn set_look(&mut self, pair: (u8, u8), arrival: PetArrival, rung: CompanionRung) {
        self.pair = pair;
        self.arrival = arrival;
        self.rung = rung;
    }

    /// Which animal the pet is drawn as. Kept as the durable choice every
    /// `prepare` re-applies (so a drain's fresh brain wears it next frame) and
    /// written through to the brain now, so the brain's identity is the animal
    /// on glass from the first frame. Switching swaps the sprite without
    /// resetting the companion.
    pub fn set_species(&mut self, s: PetSpecies) {
        self.species = s;
        self.pet.set_species(s);
    }

    /// The pointer in frame px, value-shadowed: `true` iff the stored value
    /// changed, so an idle hover costs no render. A non-finite sample is
    /// dropped.
    ///
    /// The between-frames channel: a sample fed with the frame
    /// ([`HostFrameInput::pointer_px`]) wins over the stored one.
    pub fn set_pointer(&mut self, px: Option<(f32, f32)>) -> bool {
        let next = px.filter(|(x, y)| x.is_finite() && y.is_finite());
        if px.is_some() && next.is_none() {
            return false;
        }
        let changed = self.pointer_px != next;
        self.pointer_px = next;
        changed
    }

    /// Classify the cell plane being presented, minus the host's late overlays
    /// (`exclusions`: glyphs spliced after this observation, such as a preedit
    /// row or the find bar). Advances no animation or input seq. Returns the
    /// cells classified (the pet's per-frame walk size).
    pub fn observe_console(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &crate::pet_world::PetWorldFacts,
        pane: crate::pet_world::PetPane,
        exclusions: &[crate::pet_world::PetRect],
    ) -> usize {
        self.pet
            .observe_console_with_exclusions(input, facts, pane, exclusions);
        self.pet.observed_cells()
    }

    /// The frame's notes, everything before the tick, in native order:
    ///
    /// * the Hidden edge retires the surface (the pet returns as a fresh
    ///   sighting in the same coat);
    /// * the switch, before the tick: `!owned ⇒ retire_unowned`, motes and all;
    /// * on a present only: the completion edge (returned in
    ///   [`PetTick::completion`]), the room, the Execute level, the output burst
    ///   and the wrap fact through the session-keyed latches, and the pointer in
    ///   fractional cells;
    /// * `!focused ⇒` the surface retires;
    /// * the pane binding, species and ink from `decos.pet_ink()`.
    ///
    /// A capture reads and spends none of the present-owned diffs, so the next
    /// present keeps its baseline.
    pub fn prepare(&mut self, f: PetFacts<'_>, decos: &mut WordDecorations) -> PetTick {
        let PetFacts {
            facts,
            host,
            glow,
            sing,
            focused,
            obscured,
            pane,
            room,
        } = f;
        let now = host.now;
        let geom = EffectGeom {
            cell_w: host.geometry.cell_w,
            cell_h: host.geometry.cell_h,
            rows: host.geometry.rows,
            cols: host.geometry.cols,
        };
        self.geom = geom;
        let pet_mode = glow.style_raw_names_pet;
        let trail_master = glow.enabled;
        let present = host.capture == CaptureMode::Present;
        // A TORN FRAME'S HIDDEN CURSOR IS NOT A CARET SAMPLE. On a frame the
        // program has not finished, a hidden cursor is its write position
        // (e.g. a TUI's transcript repaint, presented torn once the host's
        // hold ran out). Read as the caret, the pet chases it into the
        // transcript and takes the caret's return as a line change. So such
        // a frame reads the caret, and its paint, of the last
        // present the program finished: exactly what a host that held the
        // frame would have shown, so the pet neither moves nor fades. `None`
        // would not do that, because a hidden caret the console layer cannot
        // hold (no painted home, or a flight in the air) fades the pet out.
        let write_cursor = facts.frame_unfinished && !facts.cursor_visible;
        let (caret, caret_painted) = if write_cursor {
            self.latches
                .finished_caret
                .filter(|&(session, _)| session == facts.session && facts.live_viewport)
                .map_or((None, false), |(_, reading)| reading)
        } else {
            let reading = (facts.caret, facts.cursor_visible);
            if present {
                self.latches.finished_caret = Some((facts.session, reading));
            }
            reading
        };

        if host.visibility == Visibility::Hidden {
            self.retire_coordinate_space();
        }
        // Ownership: the host's opt-in, serious mode (the glass belongs to
        // the work), and the trail owner's three terms.
        let owned = self.enabled
            && !host.serious
            && resident_pet_owner_present(pet_mode, trail_master, glow.style);
        // The full pet is a resident. Real focus/visibility gates decide
        // whether it is presentable; load pressure only coarsens its cadence.
        // Reading owns a certified content surface, never a fabricated caret.
        let visible = owned
            && resident_pet_surface_presentable(
                focused,
                host.visibility != Visibility::Hidden && !obscured,
                facts.live_viewport,
                self.pet.has_reading_interest(),
            );
        // The switch, before the tick: an unowned pet is retired outright
        // rather than left to fade, since a fading brain keeps `needs_frames()`
        // true through `FADE_OUT` and every mote and would keep the surface
        // presenting at 60 fps. `retire_unowned` no-ops on a retired brain.
        if !owned {
            self.pet.retire_unowned();
        }
        let mut completion = None;
        let mut burst = false;
        let mut wrapped = false;
        let mut pointer = None;
        if present {
            // Exit-code empathy: the pet consumes the completion probe keyed
            // (session, seq) with its own latch. A tab switch re-baselines
            // silently, a None→Some edge within a session is a real first
            // completion, and the note only latches (the tick acts). The edge
            // is handed back for the native verdict.
            let seq = facts.cmd_done.map_or(0, |(e, _, _)| e);
            let key = (facts.session, seq);
            if self.latches.cmd_seq != Some(key) {
                let same_session = self
                    .latches
                    .cmd_seq
                    .is_some_and(|(sid, _)| sid == facts.session);
                self.latches.cmd_seq = Some(key);
                if same_session && let Some((_, code, dur_ms)) = facts.cmd_done {
                    self.pet.note_command_done(now, code != 0, dur_ms);
                    completion = Some((code, dur_ms));
                }
            }
            // THE ROOM (panel #9): levels the brain diffs itself.
            if let Some(room) = room {
                self.pet.note_room_quiet(room.quiet);
                self.pet.note_room_sibling(now, room.sibling);
                match room.session {
                    None => self.pet.note_room_lease(false),
                    Some(session) => {
                        self.pet.note_room_lease(session.lease);
                        self.pet.note_room_turn(now, session.turn.0, session.turn.1);
                        self.pet.note_room_inbox(now, session.inbox);
                    }
                }
            }
            // The Execute phase (OSC 133/633), forwarded every frame as a level;
            // the pet starts no clock and strikes no pose here.
            self.pet.note_executing(now, facts.shell_executing);
            // …and the level that qualifies it: a full-screen program owns
            // the glass, so its Execute phase has no stream to watch and no
            // prompt row to wait at (`PetBrain::note_full_screen`).
            self.pet.note_full_screen(facts.alt_screen);
            // PERK-AND-WATCH: is the pane genuinely STREAMING this
            // frame? New scrollback rows, the content clock, the OSC 133/633
            // Execute phase, the live bottom, the main screen. The content-clock
            // diff rides the pet's own (session, seq) latch, re-baselined
            // SILENTLY on a tab switch (a session change is never a burst). The
            // AND with `shell_executing` keeps keystroke echo at a prompt from
            // perking the cat, and the alt-screen term keeps a TUI's repaints
            // from doing so (`pet_output_burst`).
            let advanced = self
                .latches
                .content_seq
                .is_some_and(|(sid, s)| sid == facts.session && facts.content_seq > s);
            self.latches.content_seq = Some((facts.session, facts.content_seq));
            burst = pet_output_burst(
                facts.scrolled,
                advanced,
                facts.shell_executing,
                facts.display_offset == 0,
                facts.alt_screen,
            );
            // THE WRAP FACT (kitty-motion §4.1): did the EMULATOR resolve an
            // autowrap since this surface's last read of this session? A torn
            // frame leaves it unread: the wrap belongs to the caret move the
            // next finished frame shows.
            wrapped = !write_cursor
                && wrap_fact_edge(
                    &mut self.latches.wrap_serial,
                    facts.session,
                    facts.wrap_serial,
                );
            // POINTER PLAY: the pointer in fractional grid cells —
            // frame px minus the grid origin, over the cell metrics; `None`
            // once it leaves the grid. Pixels-to-cells only: the brain is its
            // own motion sensor. THE PRECEDENCE (`HostFrameInput::pointer_px`):
            // a sample fed WITH the frame wins over the one stored through
            // `set_pointer`.
            let px = host.pointer_px.or(self.pointer_px);
            pointer = px.and_then(|(x, y)| {
                pet_pointer_cell(
                    (f64::from(x), f64::from(y)),
                    host.geometry.origin_px,
                    (usize::from(geom.cell_w), usize::from(geom.cell_h)),
                    (usize::from(geom.cols), usize::from(geom.rows)),
                )
            });
        }
        // Ink: live glass and capture share this pre-tick setup. One frame
        // stale by construction (the rescan runs later this frame).
        if !focused {
            self.retire_coordinate_space();
        }
        self.feed_ink(decos, pane);
        // Only the motion POLICY pins the resident; the shed latch and its
        // envelope change its cadence (`Self::tick`), never its motion.
        let reduced_motion = host.reduced_motion;
        // Singing never changes the resident's caret custody; the caret is
        // withheld only from a pet that cannot be drawn, which is the truth
        // (there is no caret it could be chasing on this surface): it fades
        // out, settles, and releases the lane on its own. A HIDDEN caret is
        // still chased: `caret_drawn` carries the hide.
        PetTick {
            sense: PetSense {
                caret_drawn: caret_painted && visible,
                now,
                caret: if visible { caret } else { None },
                wrapped,
                rows: geom.rows,
                cols: geom.cols,
                cell_w: geom.cell_w,
                cell_h: geom.cell_h,
                reduced_motion,
                output_burst: burst,
                // Pointer contact requires the last drawn body and current
                // pixel custody, including while the resident is static.
                pointer: if self.hit_rect.is_some() && visible {
                    pointer
                } else {
                    None
                },
            },
            capture: host.capture,
            visible,
            pet_mode,
            sing,
            shed_envelope: host.shed_envelope,
            shed_active: host.shed_active,
            painted_caret: facts.caret.filter(|_| facts.cursor_visible),
            origin: host.geometry.origin_px,
            glass: PetGlass {
                session: facts.session,
                origin: pane.map_or((0, 0), |(_, origin)| origin),
                cols: geom.cols,
                rows: geom.rows,
            },
            completion,
        }
    }

    /// THE GLASS BESIDE THE FOCUSED SURFACE: the host's terminal panes on
    /// glass this frame ([`PetGlass`]), offered before [`Self::tick`]. The
    /// owner keeps one answer — whether the glass its ghost wants (a pending
    /// re-sighting ghost's, else one fading on its own glass) is among them,
    /// exactly as it was drawn — and the next tick consumes it. Only a
    /// composed host has panes beside the focused one; a host that offers
    /// nothing never spends a ghost off its own surface.
    pub fn offer_glass(&mut self, glass: impl IntoIterator<Item = PetGlass>) {
        let Some(want) = self
            .resight_frame
            .or(self.own_glass.map(|own| own.frame))
            .map(|f| f.glass)
        else {
            return;
        };
        self.glass_offered = glass.into_iter().any(|g| g == want);
    }

    /// THE INK/SKIN SEAM (gauntlet F1/F3/F5/F8), the half of
    /// [`Self::prepare`] that reads the word engine: bind the composed pane
    /// (before the read, or the live slot belongs to whichever pane the
    /// compose loop visited last), apply the species, and hand the brain the
    /// per-row ink.
    pub fn feed_ink(&mut self, decos: &mut WordDecorations, pane: Option<(u64, (i32, i32))>) {
        prepare_resident_pet_tick(decos, &mut self.pet, self.species, pane);
    }

    /// The frame's tick, unconditional: `needs_frames()` is a pure read of
    /// state only the tick advances, so ticking inside the draw gate would
    /// freeze it when the pet stopped being drawable and pin a full frame rate
    /// on a surface with no cat.
    ///
    /// Then custody (`on_glass = admitted && alpha > 0`; the pet wins a tie with
    /// the flying head) and the hit rect (`body_px + origin`, cleared every
    /// frame the pet is not drawn).
    pub fn tick(&mut self, t: PetTick) -> CompanionFrame {
        let geom = self.geom;
        let frame_px = PixelFrame {
            origin: t.origin,
            cell: (geom.cell_w, geom.cell_h),
            glass: t.glass,
        };
        let offered = std::mem::take(&mut self.glass_offered);
        // A RE-SIGHTING GHOST IS PIXELS THE USER SAW, so it is spent only
        // where those pixels still are:
        //
        // * the SAME frame (origin and cell metrics) — a screen swap, a tab
        //   switch: its glass now belongs to the new surface, whose world
        //   judges it (`Departure::resight`);
        // * its OWN glass beside the new surface — a split-pane focus switch:
        //   the pane it was drawn in, which the host offers this frame
        //   exactly as drawn ([`Self::offer_glass`]), at the same cell
        //   metrics. It fades there, placed at that pane
        //   ([`Self::emit_on`]), while the arriver comes up at the new
        //   pane's station (`PetBrain::displace_resight`).
        //
        // Anywhere else — the cell metrics changed (a font step, a DPI
        // change), its pane left the glass (closed, zoomed away), or the
        // grid moved under it — it would stand where no old body was, so it
        // is dropped and the replacement fades in alone.
        if let Some(drawn) = self.resight_frame.take() {
            if drawn.cell != frame_px.cell {
                self.pet.forget_resight();
            } else if drawn.origin == frame_px.origin {
                // The same glass, a new surface on it.
            } else if offered && drawn.glass.origin != frame_px.glass.origin {
                self.pet.displace_resight();
                self.own_glass = Some(OwnGlass {
                    frame: drawn,
                    colors: self.resight_colors,
                });
            } else {
                self.pet.forget_resight();
            }
        } else if self
            .own_glass
            .is_some_and(|own| own.frame.cell != frame_px.cell || !offered)
        {
            // Mid-fade, the same law: its pane closed or zoomed away, or the
            // cell metrics changed, so its pixels are gone.
            self.pet.forget_own_glass();
            self.own_glass = None;
        }
        // A ghost that was still fading on its own glass at the boundary is
        // spent there only if the focus came BACK to that glass exactly as
        // it was drawn — a quick toggle between two panes. It is then this
        // cat on the focused surface, never cut mid-fade: the arriver adopts
        // it at its seat, or it keeps fading beside the arriver on the
        // arriver's ramp; anywhere else the brain's tick drops it.
        if self.home_glass.take().is_some_and(|home| home == frame_px) {
            self.pet.come_home();
        }
        self.pet.set_console_presentable(t.visible);
        let pet_frame = match t.capture {
            CaptureMode::Present | CaptureMode::LiveCapture => self.pet.tick(t.sense),
            CaptureMode::StaticCapture => self.pet.tick_static_capture(t.sense),
        };
        self.last_static = t.sense.reduced_motion;
        self.last_shed_tick = t.shed_active.then_some(t.sense.now);
        // The resident owns every pet-mode frame, including song and tail.
        let on_glass = t.visible && pet_frame.alpha > 0;
        // The flying head is admitted only outside pet mode.
        let kitty_alpha = if !t.pet_mode {
            shed_companion_alpha(t.sing.flying_alpha, t.shed_envelope)
        } else {
            0
        };
        // The frame's one companion ([`CompanionDuty`]), read by both the
        // ambient word-cats' yield and the emitter. A companion claims a cell
        // only at a painted caret: the resident may keep its body through a
        // DECTCEM hide, and a hidden caret would be a stale word claim.
        let duty = cursor_companion_duty(on_glass, kitty_alpha, t.painted_caret);
        let body_px = on_glass
            .then(|| pet_frame.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows))
            .flatten();
        // Petting: stash the body about to be drawn, in frame px, for the press
        // hit test, and clear it every frame the pet is not drawn.
        self.hit_rect = pet_hit_rect_win(body_px, t.origin);
        self.last_alpha = if on_glass { pet_frame.alpha } else { 0 };
        self.drawn_frame = on_glass.then_some(frame_px);
        self.last_visible = t.visible;
        // A ghost the brain did not spend (no caret sighted, reduced motion)
        // or that has faded leaves its glass with it.
        if !self.pet.has_own_glass_ghost() {
            self.own_glass = None;
        }
        let glass = self
            .own_glass
            .filter(|_| pet_frame.departures.iter().flatten().any(|d| d.own_glass))
            .map(|own| own.frame.glass);
        let companion = cursor_companion_on_glass(duty, t.painted_caret, None, body_px);
        CompanionFrame {
            pet: pet_frame,
            duty,
            on_glass,
            body_px,
            companion,
            fp: pet_frame.fp(),
            glass,
        }
    }

    /// [`Self::prepare`] then [`Self::tick`], for a frame with nothing to do
    /// between them.
    #[cfg(test)]
    pub fn sense(&mut self, f: PetFacts<'_>, decos: &mut WordDecorations) -> CompanionFrame {
        let t = self.prepare(f, decos);
        self.tick(t)
    }

    /// Word-cat bat: forward the word engine's positioned peek landings
    /// (drained right after its tick) to the brain as fractional cells of the
    /// landed head's centre (the head peeks rows away from its word). Only a pet
    /// visible this frame hears them. The note only latches (range-checked
    /// brain-side, retired by TTL); the brain acts on the ground next tick.
    pub fn note_peeks(
        &mut self,
        now: Instant,
        cues: std::vec::Drain<'_, PeekCue>,
        cell: (u16, u16),
    ) {
        let (cw, ch) = (f32::from(cell.0.max(1)), f32::from(cell.1.max(1)));
        for cue in cues {
            if !self.last_visible {
                continue;
            }
            let (x0, x1, y0, y1) = cue.head_px;
            self.pet.note_peek(
                now,
                (x0 + x1) as f32 * 0.5 / cw,
                (y0 + y1) as f32 * 0.5 / ch,
            );
        }
    }

    /// Dress and draw the pet. The ceremony is committed where it renders:
    /// dress through [`pet_arrival_for_sync`] + `sync_look` and draw the
    /// `(coat, iris)` actually worn, with colours frozen per appearance
    /// (`appearance_colors()` or the 64-sample footprint walk over the cells
    /// under the prospective body, [`cursor_cat_color_key`], with
    /// [`ContrastFallback`]'s ground / cursor / accent as blank-glass
    /// fallbacks), into `free` via `decos.pet_cursor`.
    ///
    /// Returns the fingerprint fold and the sync outcome; the host's tenure
    /// bookkeeping asks `Self::commit_hello_due` whether a hello was spent.
    /// Draws nothing and syncs nothing unless the frame put the pet on glass.
    ///
    /// `free` is the WINDOW's sprite channel: a ghost on its own glass
    /// ([`CompanionFrame::glass`]) is drawn into it beside the live lane. A
    /// host that draws the live lane into a pane's staging channel calls
    /// [`Self::emit_on`].
    pub fn emit(
        &mut self,
        t: &CompanionFrame,
        cells: &[Vec<RenderCell>],
        fallback: ContrastFallback,
        decos: &mut WordDecorations,
        free: &mut Vec<FreeSprite>,
    ) -> (u64, SyncLookOutcome) {
        self.emit_on(t, cells, fallback, decos, free, None)
    }

    /// [`Self::emit`] with the live lane drawn into `free` — the focused
    /// surface's own channel, which a composed host then places into its
    /// pane — and THE GHOST ON ITS OWN GLASS drawn into `window`, the
    /// window's channel (`None`: `free` is the window's): in window-grid
    /// pixels at the pane it was drawn in, cropped to that pane, beneath its
    /// ink. Placed by the focused pane's placement it would stand in the
    /// wrong pane, or be cropped away at the divider.
    pub fn emit_on(
        &mut self,
        t: &CompanionFrame,
        cells: &[Vec<RenderCell>],
        fallback: ContrastFallback,
        decos: &mut WordDecorations,
        free: &mut Vec<FreeSprite>,
        window: Option<&mut Vec<FreeSprite>>,
    ) -> (u64, SyncLookOutcome) {
        let (pair, rung) = (self.pair, self.rung);
        if !t.on_glass || t.duty != CompanionDuty::Pet {
            // Nothing is performed off glass, so nothing may be committed:
            // the previous frame's sync must not be read as this frame's.
            self.last_sync = None;
            return (
                0,
                SyncLookOutcome {
                    worn: self.pet.worn_pair().unwrap_or(pair),
                    parked: false,
                },
            );
        }
        let arrival = pet_arrival_for_sync(rung, self.arrival, self.pet.worn_pair(), pair.0);
        let outcome = self.pet.sync_look(pair, arrival);
        self.last_sync = Some((pair, arrival));
        let (coat, iris) = outcome.worn;
        let geom = self.geom;
        let colors = self.pet.appearance_colors().unwrap_or_else(|| {
            // Resolve contrast from the full body the pet actually covers:
            // ART_ROWS tall, bottom-aligned to its baseline.
            let pet_h = (ART_ROWS * f32::from(geom.cell_h)).round();
            let pet_w = (pet_h * ART_ASPECT).round();
            let sampled = cursor_cat_color_key(
                cells,
                geom,
                CatFootprint {
                    x: (t.pet.col * f32::from(geom.cell_w)) as i32,
                    y: ((t.pet.row + 1.0) * f32::from(geom.cell_h)) as i32 - pet_h as i32,
                    w: (pet_w as i32).clamp(1, i32::from(u16::MAX)) as u16,
                    h: (pet_h as i32).clamp(1, i32::from(u16::MAX)) as u16,
                },
                fallback.bg,
                fallback.cursor,
                fallback.accent,
            );
            self.pet.colors_for_appearance(sampled)
        });
        let mut fp = 0;
        let frame = PetCursorFrame {
            geom,
            colors,
            coat,
            iris,
            pet: t.pet,
        };
        if let Some(pet_fp) = decos.pet_cursor(frame, free) {
            fp ^= pet_fp.rotate_left(29);
            // Only beside a pet the emitter drew: the same baker frame, the
            // same one-body claim, the same presence envelope.
            if let Some(glass) = t.glass {
                let own = EffectGeom {
                    cols: glass.cols,
                    rows: glass.rows,
                    ..geom
                };
                // In the palette the body was drawn with in ITS pane, not
                // the one the arriver samples in this one.
                let frame = PetCursorFrame {
                    colors: self.own_glass.and_then(|own| own.colors).unwrap_or(colors),
                    ..frame
                };
                let out = match window {
                    Some(window) => window,
                    None => free,
                };
                if let Some(ghost_fp) = decos.pet_own_glass(frame, own, glass.origin, out) {
                    fp ^= ghost_fp.rotate_left(37);
                }
            }
        }
        (fp, outcome)
    }

    /// The commit half, for the host's tenure bookkeeping. A hello was spent
    /// only when all of:
    ///   * `present`: this was a drawn present, not a capture;
    ///   * the arrival the last `emit` synced was `Ceremony` (a demoted or
    ///     floor-spaced hello commits nothing);
    ///   * the pair in hand differs from the pair on glass (`pair != worn`).
    ///
    /// `pair != outcome.worn` rather than the `outcome.parked` edge: a capture
    /// syncing first would consume that edge. Committing on the standing
    /// difference is safe because the commit consumes the authorisation (the
    /// host's arrival latch falls to Quiet), so the next present cannot commit
    /// again.
    #[must_use]
    pub fn commit_hello_due(&self, present: bool, outcome: SyncLookOutcome) -> bool {
        present
            && self.last_sync.is_some_and(|(pair, arrival)| {
                arrival == PetArrival::Ceremony && pair != outcome.worn
            })
    }

    /// The terminal BEL rang in the pane this pet is chasing.
    pub fn note_bell(&mut self, now: Instant) {
        self.pet.note_bell(now);
    }

    /// The hand found its flow: one latch per flow entry
    /// (`CursorGlow::take_flow_entry`), forwarded between [`Self::prepare`] and
    /// [`Self::tick`]; it rides a tick already running.
    pub fn note_flow(&mut self, now: Instant) {
        self.pet.note_flow(now);
    }

    /// Rainbow kitty v2's offer to the resident: the brain takes what it wants
    /// and hands back at most one star on the paw's landing frame, for the host
    /// to spend with `CursorGlow::catch_star`. Called after [`Self::tick`].
    pub fn note_v2_offer(
        &mut self,
        offer: &crate::rainbow_kitty::companion::PetOffer,
    ) -> Option<crate::rainbow_kitty::companion::StarCatch> {
        self.pet.note_v2_offer(offer)
    }

    /// A kitty command was typed at this pet (`sit`, `kitty jump`, `good
    /// kitty`), as the host's `crate::typed_tricks::TrickListener` reported.
    /// `confirmed` is the listener's `addressed`; a tentative fire is followed
    /// by another call with `confirmed = true` or by [`Self::revoke_trick`].
    /// Latch only (`PetBrain::note_trick`): later ticks perform it, so the host
    /// should ask for a frame, as for [`Self::press`]. With no resident on glass
    /// the latch is dropped.
    pub fn note_trick(&mut self, now: Instant, trick: aterm_lexicon::Trick, confirmed: bool) {
        self.pet.note_trick(now, trick, confirmed);
    }

    /// The typed line turned into prose, or was aborted: take back a
    /// tentative kitty command that has not been performed. Idempotent — the
    /// listener may report a revoke when nothing tentative is pending here.
    pub fn revoke_trick(&mut self) {
        self.pet.revoke_trick();
    }

    /// The whole submitted line was pet talk (`sit` + Enter at a shell): the
    /// brain does not grieve the fast `command not found` that follows.
    pub fn note_trick_submit(&mut self, now: Instant) {
        self.pet.note_trick_submit(now);
    }

    /// One accepted console input intent. This supplies attention provenance,
    /// never evidence that the program changed a particular range of text.
    pub fn note_console_input(&mut self, now: Instant, kind: PetInputKind) {
        self.pet.note_console_input(now, kind);
    }

    /// Accepted input intentions observed by this brain; reading does not tick.
    #[must_use]
    pub fn console_input_seq(&self) -> u64 {
        self.pet.console_input_seq()
    }

    /// Most recent classified input intention; no display-edit claim is implied.
    #[must_use]
    pub fn console_input_kind(&self) -> Option<PetInputKind> {
        self.pet.console_input_kind()
    }

    /// Petting: a left press at `px` (frame px) inside this frame's drawn body
    /// (padded by [`PET_HIT_SLOP_PX`]) strokes the cat and is consumed, so it
    /// never starts a selection or reaches a mouse-tracking app. Latch only
    /// (`note_petted`); the latch re-arms `needs_frames`, but only a tick reads
    /// it, so the host should ask for a frame.
    pub fn press(&mut self, now: Instant, px: (f32, f32)) -> PressOutcome {
        let Some(rect) = self.hit_rect else {
            return PressOutcome::Pass;
        };
        if !pet_rect_hit(rect, f64::from(px.0), f64::from(px.1), PET_HIT_SLOP_PX) {
            return PressOutcome::Pass;
        }
        self.pet.note_petted(now);
        PressOutcome::Pet
    }

    /// Retire the resident's surface-relative state (the brain's coordinates
    /// and this frame's hit rect, together) at a presentability boundary (the
    /// Hidden edge, an unfocused frame, a capture splice). Session-keyed
    /// latches are kept, so commands finishing meanwhile are still felt, and
    /// durable identity (species, worn look, contentment, disposition)
    /// survives. Idempotent.
    pub fn retire_coordinate_space(&mut self) {
        self.pet.retire_coordinate_space();
        self.hit_rect = None;
        self.drawn_frame = None;
        self.resight_frame = None;
        self.own_glass = None;
        self.home_glass = None;
        self.glass_offered = false;
    }

    /// THE SAME SURFACE, RE-GRIDDED — a resize, a font step, a DPI change, a
    /// divider drag, a zoom: [`PetBrain::rebase_geometry`]. The body keeps
    /// its place, alpha and pose (its cells are the same terminal's cells);
    /// only the facts measured against the old grid go, and the hit rect,
    /// which is this frame's pixels. Every session latch and the drawn
    /// frame survive: nothing was replaced. Idempotent.
    pub fn rebase_geometry(&mut self) {
        self.pet.rebase_geometry();
        self.hit_rect = None;
    }

    /// THE SURFACE WAS REPLACED under a body that may be on glass — another
    /// terminal, or the same terminal's other screen:
    /// [`PetBrain::resight_coordinate_space`]. Everything
    /// [`Self::retire_coordinate_space`] retires is retired, and when the
    /// last tick drew the resident, its body is kept by value for the next
    /// tick to crossfade from, in the pixel frame it was drawn in. With
    /// nothing on glass this IS the hard retire. Idempotent: a second
    /// boundary before the next tick keeps the first one's ghost.
    pub fn resight_coordinate_space(&mut self) {
        if let Some(drawn) = self.drawn_frame.take() {
            self.resight_frame = Some(drawn);
            self.resight_colors = self.pet.appearance_colors();
        }
        if self.resight_frame.is_some() {
            self.pet.resight_coordinate_space();
        } else {
            self.pet.retire_coordinate_space();
        }
        // The brain carries every crossfade ghost still fading
        // (`PetBrain::carried`); one on its own glass is spent by the next
        // tick only if the focus came back to that glass.
        if let Some(own) = self.own_glass.take() {
            self.home_glass = Some(own.frame);
        }
        self.glass_offered = false;
        self.hit_rect = None;
    }

    /// The host withdrew this frame's drawn body (a torn projection, or a route
    /// that draws no pet), so the petting rect goes with it. The brain keeps its
    /// frame and coordinates.
    pub fn withdraw_hit_rect(&mut self) {
        self.hit_rect = None;
    }

    /// The terminal under this surface was replaced (front-terminal identity
    /// edge, or the fence that committed a new grid): everything
    /// [`Self::retire_coordinate_space`] retires, and the command/content
    /// probes re-baseline silently on the new owner's first tick so the old
    /// stream's completion is not replayed. Identity survives. Idempotent.
    pub fn retire_owner(&mut self) {
        self.retire_coordinate_space();
        self.latches.cmd_seq = None;
        self.latches.content_seq = None;
    }

    /// [`Self::retire_owner`] with the re-sighting crossfade
    /// ([`Self::resight_coordinate_space`]): what the hosts' owner fences
    /// run. The command/content probes re-baseline silently exactly as
    /// there; only the body's exit differs — a visible resident hands over to
    /// its replacement in a quarter-second crossfade instead of vanishing.
    pub fn resight_owner(&mut self) {
        self.resight_coordinate_space();
        self.latches.cmd_seq = None;
        self.latches.content_seq = None;
    }

    /// THE RESIDENT'S SWITCH: drop everything the brain owns — body, motes,
    /// departures, cadence debt — because its owner went away. Identity
    /// survives for the next appearance; the hit rect and the glass alpha go
    /// with the body.
    pub fn retire(&mut self) {
        self.pet.retire_unowned();
        self.hit_rect = None;
        self.drawn_frame = None;
        self.resight_frame = None;
        self.own_glass = None;
        self.home_glass = None;
        self.glass_offered = false;
        self.last_alpha = 0;
        self.last_static = false;
        self.last_shed_tick = None;
    }

    /// Serious Mode's drain: a fresh brain (body, motes, tricks, contentment and
    /// worn look). The session latches (the host's record of the streams), the
    /// opt-in and the verdict in hand survive. Idempotent.
    pub fn drain(&mut self) {
        self.pet = PetBrain::default();
        self.hit_rect = None;
        self.drawn_frame = None;
        self.resight_frame = None;
        self.own_glass = None;
        self.home_glass = None;
        self.glass_offered = false;
        self.last_alpha = 0;
        self.last_visible = false;
        self.last_static = false;
        self.last_shed_tick = None;
    }

    /// Retire only the pet's frozen contrast sample (a theme or palette
    /// authority change); behaviour, position and breed handoff continue.
    pub fn invalidate_colors(&mut self) {
        self.pet.invalidate_colors();
        if let Some(own) = self.own_glass.as_mut() {
            own.colors = None;
        }
    }

    /// THE CADENCE LAW: [`PetBrain::needs_frames`] — something is moving —
    /// never `is_active()` — a cat exists. Static presentation excludes the
    /// brain's motion clocks, matching the native scheduler's motion gate,
    /// and a tick under the load-shed latch leaves the frame train to
    /// [`Self::next_change_deadline`]'s coarse cadence.
    #[must_use]
    pub fn needs_frames(&self) -> bool {
        !self.last_static && self.last_shed_tick.is_none() && self.pet.needs_frames()
    }

    /// THE COARSE OFFER (Rainbow Kitty v2 A/B #19): a settled resident owes
    /// no frames but names the instant of its next VISIBLE step — a breath
    /// pixel, a blink, a tail beat — [`PetBrain::next_change_deadline`].
    /// Under the load-shed latch every offer is floored at
    /// [`RESIDENT_SHED_CADENCE`] after the last tick: a MOVING resident is
    /// offered exactly there, a settled one its next visible step no sooner.
    /// An overdue step means the host woke for it and drew nothing (a
    /// DEC-2026 hold, a pending acquire), so it is re-offered a whole cadence
    /// out: a past or `now` offer (the brain clamps its own to `now +
    /// ARM_MIN`) is re-armed every turn, a busy loop. `None` when nothing
    /// will change (static posture, deep asleep, hidden, gone).
    #[must_use]
    pub fn next_change_deadline(&self, now: Instant) -> Option<Instant> {
        if self.last_static {
            return None;
        }
        let Some(at) = self.last_shed_tick else {
            return self.pet.next_change_deadline(now);
        };
        let floor = at + RESIDENT_SHED_CADENCE;
        let due = if self.pet.needs_frames() {
            floor
        } else {
            self.pet.next_change_deadline(now)?.max(floor)
        };
        Some(if due > now + ARM_MIN {
            due
        } else {
            now + RESIDENT_SHED_CADENCE
        })
    }

    /// The grief gate: the brain's failure droop is on glass or owed
    /// ([`PetBrain::grieving`]). The host hushes the glow's caret-jump fanfare
    /// while this is true, reading it between [`Self::prepare`] and
    /// [`Self::tick`].
    #[must_use]
    pub fn grieving(&self) -> bool {
        self.pet.grieving()
    }

    /// The host's opt-in as last set.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The alpha the last tick put on glass (`0` = nothing drawn) — the
    /// observability twin of the hit rect.
    #[must_use]
    pub fn alpha(&self) -> u8 {
        self.last_alpha
    }

    /// This frame's drawn body in FRAME px, `None` when the pet is not drawn.
    #[must_use]
    pub fn hit_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.hit_rect
    }

    /// The animal the host chose, which every `prepare` applies and every
    /// retire keeps. A projection of the brain's identity reads
    /// [`Self::brain`] instead (a drain rebuilds the brain but keeps this).
    #[must_use]
    #[cfg(test)]
    pub fn species(&self) -> PetSpecies {
        self.species
    }

    /// Read-only access to the brain for the host's projections (`trail
    /// status`, the Tier-1 lifecycle bind, tests). A shared borrow cannot tick
    /// it.
    #[must_use]
    pub fn brain(&self) -> &PetBrain {
        &self.pet
    }
}

#[cfg(test)]
mod law_tests {
    //! The pure laws, pinned exactly as the native app pinned them (moved
    //! here with the laws; the GUI's copies were deleted in Phase 2).
    use super::*;
    use crate::kitty_cursor::{CatExit, CatFrame, CatPose, CatReaction};
    use aterm_core::terminal::UnderlineStyle;
    use aterm_time::Duration;

    /// Ownership is pet mode and the trail master and the pet style; nothing
    /// that merely hides a pet (focus, history, the shed) is a term.
    #[test]
    fn the_owner_is_present_only_with_pet_mode_the_master_and_the_pet_style() {
        assert!(resident_pet_owner_present(
            true,
            true,
            GlowStyle::RainbowKitty
        ));
        assert!(!resident_pet_owner_present(
            false,
            true,
            GlowStyle::RainbowKitty
        ));
        assert!(!resident_pet_owner_present(
            true,
            false,
            GlowStyle::RainbowKitty
        ));
        assert!(!resident_pet_owner_present(true, true, GlowStyle::Lumen));
        assert!(!resident_pet_owner_present(true, true, GlowStyle::Sparkle));
    }

    #[test]
    fn single_and_composed_companions_share_continuous_shed_admission_and_alpha() {
        for (envelope, expected) in [(1.0, 200), (0.75, 150), (0.5, 100), (0.25, 50)] {
            assert!(
                shed_companion_presentable(true, envelope),
                "both render paths retain companion custody at {envelope}"
            );
            assert_eq!(shed_companion_alpha(200, envelope), expected);
        }
        assert!(!shed_companion_presentable(true, 0.0));
        assert!(!shed_companion_presentable(false, 1.0));
        assert_eq!(shed_companion_alpha(200, 0.0), 0);
        assert_eq!(shed_companion_alpha(200, f32::NAN), 0);
        assert!(shed_envelope_transitioning(true, 0.5));
        assert!(!shed_envelope_transitioning(true, 0.0));
        assert!(shed_envelope_transitioning(false, 0.0));
        assert!(!shed_envelope_transitioning(false, 1.0));
        assert!(!shed_envelope_transitioning(false, f32::NAN));
    }

    #[test]
    fn hidden_caret_keeps_exact_resident_body_custody_without_a_stale_cell() {
        let body = (37, 66, 48, 82);
        let fading = cursor_companion_on_glass(CompanionDuty::Pet, None, None, Some(body))
            .expect("a visible fading pet still owns glass");
        assert_eq!(fading.cell, None, "DECTCEM supplies no honest caret cell");
        assert_eq!(
            fading.body_px,
            Some(body),
            "the emitted body is the yield box"
        );
        assert!(
            !fading.guards_caret,
            "the pet stands where its brain walked it and has no caret claim"
        );

        assert_eq!(
            cursor_companion_duty(false, 255, None),
            CompanionDuty::Idle,
            "the flying head may not guess a hidden cursor anchor"
        );
        assert!(
            cursor_companion_on_glass(CompanionDuty::Idle, None, None, None).is_none(),
            "an idle frame owns no glass"
        );

        // The head is not at the caret, so a caret band alone under-covers it;
        // the rect handed in must be the sprite's own.
        let head = (26, 85, 73, 123);
        let duty = cursor_companion_duty(false, 255, Some((4, 9)));
        assert_eq!(duty, CompanionDuty::FlyingHead { cell: (4, 9) });
        let flying = cursor_companion_on_glass(duty, Some((4, 9)), Some(head), None)
            .expect("a visible classic kitty owns its caret");
        assert_eq!(flying.cell, Some((4, 9)));
        assert_eq!(flying.body_px, Some(head), "the head's REAL drawn rect");
        assert!(
            flying.guards_caret,
            "the escorting head guards the caret cell as well as its sprite"
        );
        let degraded = cursor_companion_on_glass(duty, Some((4, 9)), None, None)
            .expect("an unresolved footprint still claims the caret band");
        assert_eq!(degraded.body_px, None);
        assert!(degraded.guards_caret);
    }

    #[test]
    fn exactly_one_companion_can_own_a_frame_even_if_both_alphas_arrive() {
        // Both alphas positive is unreachable through custody (pet mode never
        // admits the flying head); the shape still guarantees one body, and the
        // pet wins.
        assert_eq!(
            cursor_companion_duty(true, 255, Some((4, 9))),
            CompanionDuty::Pet,
        );
        // The yield box then describes the PET, never the head, so an ambient
        // cat is never told to avoid a sprite that is not there.
        let pet_body = (37, 66, 48, 82);
        let head = (26, 85, 73, 123);
        let on_glass = cursor_companion_on_glass(
            cursor_companion_duty(true, 255, Some((4, 9))),
            Some((4, 9)),
            Some(head),
            Some(pet_body),
        )
        .expect("the pet owns the frame");
        assert_eq!(on_glass.body_px, Some(pet_body));
        assert!(!on_glass.guards_caret);
        // Negative control: with the pet off glass the very same inputs hand
        // the frame to the head, so the assertion above is not vacuous.
        assert_eq!(
            cursor_companion_on_glass(
                cursor_companion_duty(false, 255, Some((4, 9))),
                Some((4, 9)),
                Some(head),
                Some(pet_body),
            )
            .expect("the head owns the frame")
            .body_px,
            Some(head),
        );
    }

    #[test]
    fn pre_tick_setup_binds_focused_ink_before_read_and_applies_species() {
        use crate::word_decorations::DecoConfig;
        use aterm_core::terminal::Terminal;
        use aterm_lexicon::Lexicon;

        let cfg = DecoConfig::default();
        let lexicon = Lexicon::with_languages(&["en"]);
        let now = Instant::now();
        let mut decorations = WordDecorations::default();

        let mut inked = Terminal::new(6, 40);
        inked.process(b"\x1b[3;1Hxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
        decorations.bind_pane(11, (0, 0));
        decorations.rescan(&inked, 6, 40, &lexicon, &cfg, 1, now);
        assert_eq!(decorations.pet_ink().1, Some(2));

        let blank = Terminal::new(6, 40);
        decorations.bind_pane(22, (400, 0));
        decorations.rescan(&blank, 6, 40, &lexicon, &cfg, 1, now);
        assert_eq!(
            decorations.pet_ink().1,
            None,
            "negative control: the live slot belongs to the final blank pane"
        );

        let sense = PetSense {
            caret_drawn: true,
            now,
            caret: Some((2, 10)),
            rows: 6,
            cols: 40,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        };
        let mut focused_pet = PetBrain::default();
        prepare_resident_pet_tick(
            &mut decorations,
            &mut focused_pet,
            PetSpecies::Dog,
            Some((11, (0, 0))),
        );
        let focused = focused_pet.tick_static_capture(sense);
        assert_eq!(focused_pet.species(), PetSpecies::Dog);
        assert_eq!(
            focused.row, 3.0,
            "focused-pane ink moves a cold pet onto the blank row below"
        );

        let mut sibling_pet = PetBrain::default();
        prepare_resident_pet_tick(
            &mut decorations,
            &mut sibling_pet,
            PetSpecies::Cat,
            Some((22, (400, 0))),
        );
        let sibling = sibling_pet.tick_static_capture(sense);
        assert_eq!(
            sibling.row, 2.0,
            "negative control: the sibling's blank map leaves the same pet on the caret row"
        );
    }

    fn cell(ch: char, fg: [u8; 3], bg: [u8; 3]) -> RenderCell {
        RenderCell {
            ch,
            fg,
            bg,
            wide: false,
            emoji_presentation: false,
            text_presentation: false,
            bold: false,
            italic: false,
            underline: UnderlineStyle::None,
            strikethrough: false,
            overline: false,
            underline_color: None,
            overline_color: None,
            script: aterm_core::terminal::Script::Normal,
        }
    }

    #[test]
    fn cursor_companion_samples_its_actual_multiline_footprint() {
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 2,
            cols: 3,
        };
        let footprint = CatFootprint {
            x: 0,
            y: 0,
            w: 30,
            h: 40,
        };
        let make = |neighbor, top_bg, bottom_bg| {
            vec![
                vec![cell('x', neighbor, top_bg); 3],
                vec![cell(' ', [240, 240, 240], bottom_bg); 3],
            ]
        };
        let red = cursor_cat_color_key(
            &make([255, 20, 20], [8, 8, 8], [8, 8, 8]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        let blue = cursor_cat_color_key(
            &make([20, 80, 255], [8, 8, 8], [8, 8, 8]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        let light = cursor_cat_color_key(
            &make([255, 20, 20], [248, 248, 248], [248, 248, 248]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        let mixed = cursor_cat_color_key(
            &make([255, 20, 20], [4, 4, 4], [248, 248, 248]),
            geom,
            footprint,
            0,
            0x00FF_FFFF,
            0,
        );
        assert_ne!(
            red.accent, blue.accent,
            "neighbor text changes the hue family"
        );
        assert_ne!(
            red.background, light.background,
            "backgrounds across the sprite footprint change contrast ink"
        );
        assert_eq!(
            mixed.background, 4,
            "a dark+light footprint must not collapse to its RGB-average band"
        );
    }

    #[test]
    fn cursor_companion_ignores_alternating_backgrounds_outside_its_footprint() {
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 10,
            rows: 2,
            cols: 4,
        };
        let footprint = CatFootprint {
            x: 20,
            y: 0,
            w: 20,
            h: 20,
        };
        let cells = vec![
            vec![
                cell('x', [255, 0, 0], [255, 255, 255]),
                cell('x', [255, 0, 0], [255, 255, 255]),
                cell('x', [0, 0, 255], [4, 4, 4]),
                cell('x', [0, 0, 255], [4, 4, 4]),
            ];
            2
        ];
        let sampled = cursor_cat_color_key(&cells, geom, footprint, 0, 0, 0);
        assert!(
            sampled.dark(),
            "only the dark right-hand footprint is sampled"
        );
    }

    /// The latch law: first sight and session switches baseline silently,
    /// a same-session serial change reads as exactly one wrap, and a swap
    /// back to an older serial still reads (`!=`, not `>`).
    #[test]
    fn same_session_change_is_a_wrap_and_a_session_switch_never_is() {
        let mut seen = None;
        // First read: baseline only, never a wrap.
        assert!(!wrap_fact_edge(&mut seen, 7, 41));
        // Unchanged serial: quiet.
        assert!(!wrap_fact_edge(&mut seen, 7, 41));
        // Same session, serial moved: the wrap fact — once.
        assert!(wrap_fact_edge(&mut seen, 7, 42));
        assert!(!wrap_fact_edge(&mut seen, 7, 42));
        // Session switch resets the baseline, even at a differing serial.
        assert!(!wrap_fact_edge(&mut seen, 8, 0));
        // A move BACKWARD (main/alt swap kept per-grid serials) still reads.
        assert!(wrap_fact_edge(&mut seen, 8, u64::MAX));
        assert!(wrap_fact_edge(&mut seen, 8, 3));
    }

    /// The px→cell map is the hit-rect's geometry inverted: origin off,
    /// cell metrics down, and anything off the grid is `None`.
    #[test]
    fn pointer_maps_into_pane_cells_and_dies_at_the_edge() {
        // Origin (20, 40), 10×20 cells, 80×24 grid.
        assert_eq!(
            pet_pointer_cell((125.0, 90.0), (20, 40), (10, 20), (80, 24)),
            Some((10.5, 2.5))
        );
        // Left/above the origin: outside.
        assert_eq!(
            pet_pointer_cell((19.0, 90.0), (20, 40), (10, 20), (80, 24)),
            None
        );
        // Past the last column: outside.
        assert_eq!(
            pet_pointer_cell((20.0 + 800.0, 90.0), (20, 40), (10, 20), (80, 24)),
            None
        );
        // Degenerate metrics never divide.
        assert_eq!(
            pet_pointer_cell((5.0, 5.0), (0, 0), (0, 20), (80, 24)),
            None
        );
    }

    /// The echo law: content movement alone is NEVER a burst — the shell
    /// must be executing, the viewport must be live, and the pane must be on
    /// the main screen.
    #[test]
    fn typing_echo_never_reads_as_a_burst() {
        // Echo: the content clock moves, the shell is NOT executing.
        assert!(!pet_output_burst(false, true, false, true, false));
        // A real stream: rows scrolled while the shell runs, live bottom.
        assert!(pet_output_burst(true, false, true, true, false));
        assert!(pet_output_burst(false, true, true, true, false));
        // Scrolled-back history is not a stream the pet can see.
        assert!(!pet_output_burst(true, true, true, false, false));
        // An executing shell that wrote nothing this frame is quiet.
        assert!(!pet_output_burst(false, false, true, true, false));
        // A full-screen program's repaint — its spinner, its streamed
        // tokens, the echo in its composer — is not output, whatever moved.
        assert!(!pet_output_burst(false, true, true, true, true));
        assert!(!pet_output_burst(true, true, true, true, true));
    }

    /// The offset math is exactly the emitter's: body px + effects origin
    /// (x on both x's, y on both y's), and `None` — pet not drawn — stays
    /// `None`, which is what clears the stash.
    #[test]
    fn pet_hit_rect_win_offsets_the_body_by_the_effects_origin() {
        assert_eq!(pet_hit_rect_win(None, (7, 9)), None);
        assert_eq!(
            pet_hit_rect_win(Some((10, 30, 40, 60)), (7, 9)),
            Some((17, 37, 49, 69))
        );
        // A row-0 pet's head rises above the grid top: the rect keeps the
        // negative overhang (the strip/modals still win the click by ORDER,
        // not by clamping the cat's face away).
        assert_eq!(
            pet_hit_rect_win(Some((0, 20, -12, 8)), (4, 30)),
            Some((4, 24, 18, 38))
        );
    }

    /// The press hit test: inside the body, inside the slop band, outside it —
    /// and the slop is symmetric on every side.
    #[test]
    fn a_press_lands_inside_the_body_plus_slop_and_nowhere_else() {
        let rect = (10, 30, 40, 60);
        assert!(pet_rect_hit(rect, 20.0, 50.0, PET_HIT_SLOP_PX));
        assert!(
            pet_rect_hit(rect, 6.0, 36.0, PET_HIT_SLOP_PX),
            "the top-left grace"
        );
        assert!(
            pet_rect_hit(rect, 33.9, 63.9, PET_HIT_SLOP_PX),
            "the bottom-right grace"
        );
        assert!(!pet_rect_hit(rect, 5.9, 50.0, PET_HIT_SLOP_PX));
        assert!(
            !pet_rect_hit(rect, 34.0, 50.0, PET_HIT_SLOP_PX),
            "right/bottom exclusive"
        );
        assert!(!pet_rect_hit(rect, 20.0, 64.0, PET_HIT_SLOP_PX));
        assert!(
            !pet_rect_hit(rect, 20.0, 50.0, -20),
            "a negative slop shrinks past nothing"
        );
    }

    /// The grid the hit-box cases below resolve against — the same 10x20 cell on
    /// an 80x30 surface their `PetSense` uses, so the rect and the brain agree.
    fn hit_geom() -> EffectGeom {
        EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: 30,
            cols: 80,
        }
    }

    /// The face expression changes at the authored threshold; this does not
    /// change the companion that owns the cursor.
    #[test]
    fn face_goes_live_at_the_swap_threshold() {
        assert!(!sing_face_live(0.0));
        assert!(!sing_face_live(0.3299));
        assert!(sing_face_live(0.33));
        assert!(sing_face_live(0.34));
        assert!(sing_face_live(1.0));
        assert!(!sing_face_live(f32::NAN));
    }

    #[test]
    fn reduced_song_keeps_full_pet_present_and_late_cutoffs_never_blank() {
        use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
        use crate::kitty_cursor::{CursorCat, SingSync};

        let t0 = Instant::now();
        let sense = |now, caret| PetSense {
            caret_drawn: true,
            now,
            caret,
            rows: 24,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: true,
            output_burst: false,
            pointer: None,
            wrapped: false,
        };
        let mut pet = PetBrain::default();
        let mut now = t0;

        // The ordinary resident is fully present before the song starts.
        for _ in 0..30 {
            now += Duration::from_millis(16);
            let _ = pet.tick(sense(now, Some((4, 12))));
        }
        let mut pet_frame = pet.tick(sense(now, Some((4, 12))));
        assert_eq!(pet_frame.alpha, 255);

        let mut singer = CursorCat::default();
        let singer_run = now;
        for i in 0..160u64 {
            singer.on_pet_mode_motion_pulse(CursorCatMotionPulse {
                at: singer_run + Duration::from_millis(i * 40),
                kind: CursorCatMotionKind::Advance,
            });
        }
        now = singer_run + Duration::from_millis(6_500);
        singer.set_singing(
            now,
            SingSync {
                drive: 1.0,
                beat: 0.0,
            },
        );
        let held = singer.static_frame(now);
        assert_eq!(held.alpha, 255);
        // A fully opaque singing head is present in the episode, and pet mode
        // never admits it: the resident's ownership is an admission decision.
        pet_frame = pet.tick(sense(now, Some((4, 12))));
        assert_eq!(pet_frame.alpha, 255);

        // Tier-1: project the real reduced-motion custody verdict onto the
        // derived state machine. Only the scalar phase bookkeeping comes from
        // the model successor; the pet's presentation facts come from runtime,
        // and the head is never admitted in pet mode (`singer_visible` false).
        let handoff_model = aterm_spec::derive::reduced_motion_companion_handoff_model();
        let bind_handoff = |before: &aterm_spec::interp::State,
                            action: &'static str,
                            singer_visible: bool,
                            pet_ready: bool,
                            pet_visible: bool| {
            let mut observed = handoff_model.successors(action, before)[0].clone();
            observed.insert("singer_visible", i64::from(singer_visible));
            observed.insert("pet_ready", i64::from(pet_ready));
            observed.insert("pet_visible", i64::from(pet_visible));
            let label = format!("reduced companion runtime {action}");
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &handoff_model,
                &[],
                before,
                &observed,
                Some(action),
                &label,
            );
            assert!(ok, "real reduced custody transition rejected: {why}");
            observed
        };
        let started = bind_handoff(
            &handoff_model.init_state(),
            "StartReducedSong",
            false,
            pet_frame.alpha == 255,
            pet_frame.alpha > 0,
        );

        // No intermediate wind-down tick: an occluded frame jumps straight
        // from the held drive to 0.49, below the 0.33 cutoff, or fully drained.
        // The runtime transition is validated against the corresponding model
        // action, then a forged transparent result must be rejected.
        for (drive, action) in [
            (0.49, "SampleLateBelowHalf"),
            (0.30, "SampleLateBelowFaceSwap"),
            (0.0, "SampleLateDrained"),
        ] {
            singer.set_singing(
                now,
                SingSync {
                    drive: 1.0,
                    beat: 0.0,
                },
            );
            assert_eq!(singer.static_frame(now).alpha, 255);
            now += Duration::from_millis(16);
            singer.set_singing(now, SingSync { drive, beat: 0.0 });
            let _ = singer.static_frame(now);
            pet_frame = pet.tick(sense(now, Some((4, 12))));
            assert_eq!(
                pet_frame.alpha, 255,
                "direct 1.0→{drive} reduced cutoff must reveal an opaque pet"
            );
            let observed = bind_handoff(
                &started,
                action,
                false,
                pet_frame.alpha == 255,
                pet_frame.alpha > 0,
            );
            let mut forged_blackout = observed;
            forged_blackout.insert("singer_visible", 0);
            forged_blackout.insert("pet_visible", 0);
            let (ok, _) = aterm_spec::verify::validate_transition_tiered(
                &handoff_model,
                &[],
                &started,
                &forged_blackout,
                Some(action),
                "forged reduced companion blackout",
            );
            assert!(!ok, "{action} must reject an all-transparent projection");
        }

        // Tier-1 for the ordinary cadence too: half cutoff -> sampled tail ->
        // below-face expression -> drain, with every action driven by the same
        // runtime gates as the direct-late branches above.
        let mut cadenced = started.clone();
        for (drive, action) in [
            (0.50, "SampleAtHalfCutoff"),
            (0.49, "SampleCadencedBelowHalf"),
            (0.329, "SampleBelowFaceSwap"),
            (0.0, "DrainSongTail"),
        ] {
            singer.set_singing(now, SingSync { drive, beat: 0.0 });
            let _ = singer.static_frame(now);
            assert_eq!(pet_frame.alpha, 255, "drive {drive}");
            cadenced = bind_handoff(
                &cadenced,
                action,
                false,
                pet_frame.alpha == 255,
                pet_frame.alpha > 0,
            );
        }
        assert_eq!(cadenced[&"phase"], 5);
        assert_eq!(cadenced[&"pet_visible"], 1);

        // A cold/new reduced-motion resident becomes an opaque still on its
        // first live-caret sample. Reduced motion owns no frame-cadence lane,
        // so leaving the ordinary 0.30 s appearance ramp here could strand a
        // transparent resident until an unrelated redraw.
        let mut cold_pet = PetBrain::default();
        let first = cold_pet.tick(sense(now, Some((4, 12))));
        assert_eq!(first.alpha, 255);
        for _ in 0..20 {
            now += Duration::from_millis(16);
            let frame = cold_pet.tick(sense(now, Some((4, 12))));
            assert_eq!(frame.alpha, 255, "the reduced still stays opaque");
            pet_frame = frame;
        }
        assert_eq!(pet_frame.alpha, 255, "the resident remains on glass");
    }

    #[test]
    fn history_suppresses_both_cursor_companions_without_suspending_decorations() {
        assert!(cursor_companion_presentable(true, true));
        assert!(
            !cursor_companion_presentable(true, false),
            "a presentable decoration surface still suppresses cursor-owned bodies in history"
        );
        assert!(!cursor_companion_presentable(false, true));
    }

    /// The pet half of the cursor-viewport lifecycle: the resident stays
    /// lifecycle-live in history (it gets hidden-caret ticks) and its scheduler
    /// settles. Its hit target disappears on the first suppressed frame because
    /// [`CompanionOwner::tick`] derives it from custody, not alpha.
    #[test]
    fn history_lets_the_fading_pet_brain_settle() {
        let t0 = Instant::now();
        let sense = |now, caret| PetSense {
            caret_drawn: true,
            now,
            caret,
            rows: 24,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        };
        let mut pet = PetBrain::default();
        let _ = pet.tick(sense(t0, Some((4, 12))));
        let live_pet = pet.tick(sense(t0 + Duration::from_millis(500), Some((4, 12))));
        assert!(live_pet.alpha > 0, "negative control owns a resident pet");
        let g = hit_geom();
        assert!(
            live_pet
                .body_px(g.cell_w, g.cell_h, g.cols, g.rows)
                .is_some(),
            "the live pet owns a clickable body"
        );
        let hidden_pet = pet.tick(sense(t0 + Duration::from_millis(600), None));
        assert!(
            hidden_pet.alpha > 0,
            "the brain is still fading on the first history frame"
        );
        for step in 1..=300 {
            let _ = pet.tick(sense(t0 + Duration::from_millis(600 + step * 100), None));
            if !pet.needs_frames() {
                break;
            }
        }
        assert!(
            !pet.needs_frames(),
            "hidden-caret ticks let the pet release the animation scheduler"
        );
    }

    /// A pet-mode summon exits plain whatever flourish was rolled; outside pet
    /// mode the roll is untouched.
    #[test]
    fn pet_mode_summons_exit_plain() {
        let frame = |exit| CatFrame {
            alpha: 128,
            exit,
            fade_out: 0.5,
            look: KittyLook::default(),
            reaction: CatReaction::Cruise,
            discovery: false,
            collection_hello: false,
            bob: 0.0,
            sing: 0.4,
            pose: CatPose::STILL,
        };
        for rolled in [CatExit::StarWink, CatExit::HeartMeow, CatExit::Plain] {
            let mut f = frame(rolled);
            pin_pet_mode_exit(true, &mut f);
            assert_eq!(f.exit, CatExit::Plain, "pet-mode episodes exit Plain");
            let mut f = frame(rolled);
            pin_pet_mode_exit(false, &mut f);
            assert_eq!(f.exit, rolled, "earned flights keep their roll");
        }
    }

    /// The arrival mapping, rung by rung: non-program rungs are quiet, a
    /// program rung carries the gate's ruling, an undressed pet keeps its
    /// ceremony, and a ceremony for a coat within sufficient difference of the
    /// one on glass is demoted.
    #[test]
    fn the_arrival_mapping_rations_the_theater_by_rung_and_difference() {
        use crate::kitty_registry::{SUFFICIENT_DIFFERENCE, coat_distance};
        for rung in [CompanionRung::Favourite, CompanionRung::Launch] {
            assert_eq!(
                pet_arrival_for_sync(rung, PetArrival::Ceremony, None, 3),
                PetArrival::Quiet,
                "{rung:?} never announces"
            );
        }
        assert_eq!(
            pet_arrival_for_sync(CompanionRung::Program, PetArrival::Quiet, None, 3),
            PetArrival::Quiet
        );
        assert_eq!(
            pet_arrival_for_sync(CompanionRung::Program, PetArrival::Ceremony, None, 3),
            PetArrival::Ceremony,
            "a pet never yet dressed has nothing to compare against"
        );
        // The same coat is within any difference of itself: demoted.
        assert_eq!(
            pet_arrival_for_sync(
                CompanionRung::Program,
                PetArrival::Ceremony,
                Some((3, 0)),
                3
            ),
            PetArrival::Quiet
        );
        // A coat far enough away keeps its ceremony — found, not assumed.
        let far = (0u8..16)
            .find(|&c| coat_distance(3, c) >= SUFFICIENT_DIFFERENCE)
            .expect("some coat is sufficiently different from coat 3");
        assert_eq!(
            pet_arrival_for_sync(
                CompanionRung::Program,
                PetArrival::Ceremony,
                Some((3, 0)),
                far
            ),
            PetArrival::Ceremony
        );
    }
}

#[cfg(test)]
mod owner_tests {
    //! The driver itself: the seed door, the latches, the unconditional tick,
    //! the press seam, the pointer shadow, the emitter.
    use super::*;
    use crate::host::FrameGeom;
    use aterm_time::Duration;
    use std::collections::BTreeMap;

    /// Every fallback black — the emitter tests that care about the sync
    /// outcome, not the palette.
    const BLACK_FALLBACK: ContrastFallback = ContrastFallback {
        bg: 0,
        cursor: 0,
        accent: 0,
    };

    const GRID: FrameGeom = FrameGeom {
        rows: 24,
        cols: 80,
        cell_w: 10,
        cell_h: 20,
        origin_px: (0, 0),
    };

    fn host_at(now: Instant) -> HostFrameInput {
        HostFrameInput {
            now,
            visibility: Visibility::Focused,
            reduced_motion: false,
            serious: false,
            shed_envelope: 1.0,
            shed_active: false,
            pointer_px: None,
            capture: CaptureMode::Present,
            geometry: GRID,
        }
    }

    fn facts_with(caret: Option<(u16, u16)>) -> TerminalFacts {
        TerminalFacts {
            session: 1,
            caret,
            cursor_visible: caret.is_some(),
            frame_unfinished: false,
            display_offset: 0,
            live_viewport: true,
            content_seq: 0,
            wrap_serial: 0,
            scrolled: false,
            shell_executing: false,
            cmd_done: None,
            alt_screen: false,
        }
    }

    fn pet_glow() -> GlowOwnership {
        GlowOwnership {
            enabled: true,
            style: GlowStyle::RainbowKitty,
            style_raw_names_pet: true,
        }
    }

    fn an_enabled_owner() -> CompanionOwner {
        let mut owner = CompanionOwner::default();
        owner.set_enabled_seed(true, 0x5EED);
        owner
    }

    /// One focused, owned, live-viewport frame at `now` with the caret at `caret`.
    fn frame(
        owner: &mut CompanionOwner,
        decos: &mut WordDecorations,
        now: Instant,
        caret: Option<(u16, u16)>,
    ) -> CompanionFrame {
        let facts = facts_with(caret);
        let host = host_at(now);
        owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            decos,
        )
    }

    /// Walk the caret until the resident is on glass and mid-motion. Returns
    /// the clock it stopped at.
    fn materialize(owner: &mut CompanionOwner, decos: &mut WordDecorations) -> Instant {
        let mut now = Instant::now();
        let mut on_glass = false;
        for step in 0u16..40 {
            now += Duration::from_millis(16);
            let t = frame(owner, decos, now, Some((4, 10 + step % 8)));
            on_glass |= t.on_glass;
        }
        assert!(on_glass, "fixture: the resident reached the glass");
        assert!(owner.alpha() > 0 && owner.hit_rect().is_some());
        assert!(owner.needs_frames(), "fixture: it is claiming the lane");
        now
    }

    /// Hold the caret still until the resident releases the lane
    /// (idle-to-zero) — bounded well past the sleep threshold. Returns the
    /// clock it settled at.
    fn settle(
        owner: &mut CompanionOwner,
        decos: &mut WordDecorations,
        mut now: Instant,
    ) -> Instant {
        for _ in 0..2400 {
            now += Duration::from_millis(50);
            let _ = frame(owner, decos, now, Some((4, 12)));
            if !owner.needs_frames() {
                return now;
            }
        }
        panic!("fixture: the resident never settled");
    }

    /// Seed: the same seed dresses the same pair on any instance
    /// (`KittyLook::for_launch`), a different seed a different cat, and
    /// `enabled = false` retires the body, rect and lane at once.
    #[test]
    fn the_seed_door_dresses_the_pet_and_disabling_retires_it() {
        let mut decos = WordDecorations::default();
        let mut a = an_enabled_owner();
        let now = materialize(&mut a, &mut decos);
        let t = frame(
            &mut a,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        let mut free = Vec::new();
        let (_, worn_a) = a.emit(
            &t,
            &[],
            ContrastFallback {
                bg: 0,
                cursor: 0x00FF_FFFF,
                accent: 0x0040_80FF,
            },
            &mut decos,
            &mut free,
        );
        let launch = KittyLook::for_launch(0x5EED);
        assert_eq!(
            worn_a.worn,
            (launch.coat, launch.iris),
            "the launch look is worn"
        );
        assert!(!free.is_empty(), "one body landed");

        let other = (0u64..64)
            .map(KittyLook::for_launch)
            .find(|l| (l.coat, l.iris) != (launch.coat, launch.iris))
            .expect("a different seed dresses a different cat");
        let mut b = CompanionOwner::default();
        assert!(
            !b.enabled(),
            "off at construction: the byte-identical posture"
        );
        b.set_enabled_seed(true, 0x5EED);
        assert!(b.enabled());
        let _ = other;

        a.set_enabled_seed(false, 0x5EED);
        assert!(!a.enabled());
        assert!(!a.needs_frames(), "disable releases the lane at once");
        assert_eq!(a.alpha(), 0);
        assert_eq!(a.hit_rect(), None);
        assert!(!a.brain().is_active());
    }

    /// The brain ticks unconditionally: an unfocused surface feeds `caret:
    /// None`, and the resident fades, settles and releases the lane. Negative
    /// control: an unticked brain's `needs_frames` latches true forever.
    #[test]
    fn the_brain_ticks_even_when_undrawable_so_the_lane_is_released() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let mut now = materialize(&mut owner, &mut decos);

        let mut released = false;
        for _ in 0..400 {
            now += Duration::from_millis(50);
            let facts = facts_with(Some((4, 12)));
            let host = host_at(now);
            let t = owner.sense(
                PetFacts {
                    facts: &facts,
                    host: &host,
                    glow: pet_glow(),
                    sing: SingFacts::default(),
                    focused: false,
                    obscured: false,
                    pane: None,
                    room: None,
                },
                &mut decos,
            );
            assert!(!t.on_glass, "an unfocused surface draws no pet");
            assert_eq!(owner.alpha(), 0);
            assert_eq!(owner.hit_rect(), None);
            if !owner.needs_frames() {
                released = true;
                break;
            }
        }
        assert!(
            released,
            "hidden-caret ticks let the resident release the lane"
        );

        // Negative control: the same live state, never ticked again.
        let mut frozen = an_enabled_owner();
        let _ = materialize(&mut frozen, &mut decos);
        assert!(
            frozen.needs_frames(),
            "a brain that is not ticked keeps claiming the lane — the latch the law prevents"
        );
    }

    /// The command-done latch: the first read of a session baselines silently,
    /// a None→Some edge within a session is a real first completion, a later
    /// same-session completion is felt again, and a session switch re-baselines
    /// without replay.
    #[test]
    fn the_command_latch_baselines_silently_then_feels_each_completion_once() {
        let mut decos = WordDecorations::default();
        let sense_cmd = |owner: &mut CompanionOwner,
                         decos: &mut WordDecorations,
                         now: Instant,
                         session: u64,
                         cmd_done: Option<(u64, i32, Option<u64>)>| {
            let mut facts = facts_with(Some((4, 12)));
            facts.session = session;
            facts.cmd_done = cmd_done;
            let host = host_at(now);
            let _ = owner.sense(
                PetFacts {
                    facts: &facts,
                    host: &host,
                    glow: pet_glow(),
                    sing: SingFacts::default(),
                    focused: true,
                    obscured: false,
                    pane: None,
                    room: None,
                },
                decos,
            );
        };

        // First read ever, with a failure already in the history: baseline.
        let mut owner = an_enabled_owner();
        let now = Instant::now();
        sense_cmd(&mut owner, &mut decos, now, 1, Some((9, 1, None)));
        assert!(
            !owner.brain().grieving(),
            "the first read of a session's history is a silent baseline"
        );
        // A new same-session completion: felt.
        sense_cmd(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            1,
            Some((10, 1, None)),
        );
        assert!(owner.brain().grieving(), "a same-session failure is felt");

        // The None→Some edge WITHIN one session is a real first completion.
        let mut first = an_enabled_owner();
        sense_cmd(&mut first, &mut decos, now, 1, None);
        assert!(!first.brain().grieving());
        sense_cmd(
            &mut first,
            &mut decos,
            now + Duration::from_millis(16),
            1,
            Some((1, 1, None)),
        );
        assert!(
            first.brain().grieving(),
            "the first completion this session saw"
        );

        // A session switch with a differing seq: baseline only, never a replay.
        let mut switched = an_enabled_owner();
        sense_cmd(&mut switched, &mut decos, now, 1, Some((9, 0, None)));
        sense_cmd(
            &mut switched,
            &mut decos,
            now + Duration::from_millis(16),
            2,
            Some((3, 1, None)),
        );
        assert!(
            !switched.brain().grieving(),
            "a session change is never a completion"
        );
    }

    /// Petting: a press inside the drawn body (plus slop) latches a pet and is
    /// consumed; outside passes; a retired pet has no body.
    #[test]
    fn a_press_inside_the_body_plus_slop_pets_and_outside_passes() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let (x0, x1, y0, y1) = owner.hit_rect().expect("a drawn body");

        let inside = ((x0 + x1) as f32 * 0.5, (y0 + y1) as f32 * 0.5);
        assert_eq!(owner.press(now, inside), PressOutcome::Pet);
        assert_eq!(owner.brain().pending_pets(), 1, "the latch moved");
        let grace = (x0 as f32 - PET_HIT_SLOP_PX as f32 + 0.5, y0 as f32);
        assert_eq!(
            owner.press(now, grace),
            PressOutcome::Pet,
            "the slop band pets too"
        );
        assert_eq!(owner.brain().pending_pets(), 2);

        let outside = (x1 as f32 + PET_HIT_SLOP_PX as f32 + 1.0, inside.1);
        assert_eq!(owner.press(now, outside), PressOutcome::Pass);
        assert_eq!(owner.brain().pending_pets(), 2, "a miss latches nothing");

        owner.retire();
        assert_eq!(
            owner.press(now, inside),
            PressOutcome::Pass,
            "no body, no pet"
        );
    }

    #[test]
    fn only_a_presented_resident_can_receive_pointer_strokes() {
        for (sing_drive, shed, focused, visible) in [
            (0.0, 1.0, true, true),
            (0.2, 1.0, true, true), // singing keeps the resident pettable
            (0.0, 1.0, false, false),
            (0.2, 0.0, false, false),
        ] {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let now = materialize(&mut owner, &mut decos);
            let mut now = settle(&mut owner, &mut decos, now);
            // Stay well inside the ordinary seated window and away from the
            // follower's final glide before introducing the pointer.
            for _ in 0..20 {
                now += Duration::from_millis(50);
                frame(&mut owner, &mut decos, now, Some((4, 12)));
            }
            let (x0, x1, y0, y1) = owner.hit_rect().expect("fixture: a drawn resident");
            let width = (x1 - x0) as f32;
            let before = owner.brain().content();
            let facts = facts_with(Some((4, 12)));
            let mut answered = false;
            for step in 0..=12 {
                now += Duration::from_millis(25);
                let mut host = host_at(now);
                host.shed_envelope = shed;
                host.pointer_px = Some((
                    x0 as f32 + width * (0.20 + 0.60 * step as f32 / 12.0),
                    (y0 + y1) as f32 * 0.5,
                ));
                let result = owner.sense(
                    PetFacts {
                        facts: &facts,
                        host: &host,
                        glow: pet_glow(),
                        sing: SingFacts {
                            drive: sing_drive,
                            ..SingFacts::default()
                        },
                        focused,
                        obscured: false,
                        pane: None,
                        room: None,
                    },
                    &mut decos,
                );
                assert_eq!(result.on_glass, visible);
                answered |= owner.brain().pending_pets() > 0
                    || owner.brain().action() == crate::kitty_pet::PetAction::Purr;
            }
            if visible {
                assert!(answered, "control: the same sweep strokes a drawn pet");
                assert!(owner.brain().content() > before + 0.05);
            } else {
                assert!(
                    !answered,
                    "a hidden pet cannot be touched: sing={sing_drive}, shed={shed}"
                );
                assert!(
                    owner.brain().content() <= before,
                    "a hidden pet earned affection: sing={sing_drive}, shed={shed}"
                );
            }
        }
    }

    /// The pointer is value-shadowed so the host bumps its frame gate only on
    /// a real change; a non-finite sample is dropped and changes nothing.
    #[test]
    fn the_pointer_is_value_shadowed() {
        let mut owner = CompanionOwner::default();
        assert!(!owner.set_pointer(None), "None over None is no change");
        assert!(owner.set_pointer(Some((12.0, 34.0))));
        assert!(
            !owner.set_pointer(Some((12.0, 34.0))),
            "a still pointer costs no render"
        );
        assert!(owner.set_pointer(Some((12.5, 34.0))));
        assert!(
            !owner.set_pointer(Some((f32::NAN, 1.0))),
            "poison is dropped"
        );
        assert!(
            !owner.set_pointer(Some((12.5, 34.0))),
            "…and left the value alone"
        );
        assert!(owner.set_pointer(None), "leaving the surface is a change");
        assert!(!owner.set_pointer(None));
    }

    /// The pointer reaches the brain as fractional cells: wandering over the
    /// grid builds attention and re-arms the lane; off the grid it does not
    /// exist for the pet, so a settled cat stays settled.
    #[test]
    fn a_pointer_over_the_grid_is_sensed_and_one_off_the_grid_is_not() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut now = settle(&mut owner, &mut decos, now);

        // Wander far outside the 800×480 grid: furniture.
        for k in 0..30 {
            now += Duration::from_millis(16);
            let dx = if k % 10 < 5 { 2.0 } else { -2.0 };
            let _ = owner.set_pointer(Some((-500.0 + dx * k as f32, -300.0)));
            let _ = frame(&mut owner, &mut decos, now, Some((4, 12)));
        }
        assert!(
            !owner.needs_frames(),
            "off the grid the pointer does not exist for the pet"
        );

        // Wander on the pet's row, ~12 cells/s of gentle circling (the
        // brain's own gaze fixture): attention builds and the lane re-arms.
        let mut px = 400.0f32;
        for k in 0..30 {
            now += Duration::from_millis(16);
            px += if k % 10 < 5 { 2.0 } else { -2.0 };
            let _ = owner.set_pointer(Some((px, 4.0 * 20.0 + 10.0)));
            let _ = frame(&mut owner, &mut decos, now, Some((4, 12)));
        }
        assert!(
            owner.needs_frames(),
            "a wandering pointer over the grid is sensed"
        );
    }

    /// A windowless still materialises the resident at full opacity on its
    /// first frame (`tick_static_capture`), where a live present's first
    /// tick is the transparent start of a fade-in.
    #[test]
    fn a_static_capture_materialises_the_first_still() {
        let mut decos = WordDecorations::default();
        let now = Instant::now();
        let facts = facts_with(Some((2, 2)));

        let mut capture = an_enabled_owner();
        let mut host = host_at(now);
        host.capture = CaptureMode::StaticCapture;
        let still = capture.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert_eq!(still.pet.alpha, 255, "the still shows the resident");
        assert!(still.on_glass && still.body_px.is_some());

        let mut present = an_enabled_owner();
        let host = host_at(now);
        let first = present.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert_eq!(first.pet.alpha, 0, "negative control: a present fades in");
        assert!(present.needs_frames(), "…and claims the lane to do so");
    }

    /// The emitter draws one body wearing the brain's latched pair, folds a
    /// non-zero fingerprint and syncs the verdict: a favourite parks a differing
    /// pair on the first emission, and the hello seam never commits a quiet rung.
    #[test]
    fn emit_draws_one_body_wearing_the_latched_pair_and_syncs_the_verdict() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        assert!(t.on_glass);
        assert_eq!(t.duty, CompanionDuty::Pet);
        assert!(
            t.companion
                .is_some_and(|c| c.body_px == t.body_px && !c.guards_caret),
            "the yield box is the pet's own body"
        );
        assert_ne!(t.fp, 0);

        let mut free = Vec::new();
        let theme = ContrastFallback {
            bg: 0x0010_1010,
            cursor: 0x00F0_F0F0,
            accent: 0x0040_80FF,
        };
        let (fp, outcome) = owner.emit(&t, &[], theme, &mut decos, &mut free);
        assert_ne!(fp, 0, "a drawn body folds into the fingerprint");
        assert!(!free.is_empty(), "the body landed as free sprites");
        let launch = KittyLook::for_launch(0x5EED);
        assert_eq!(outcome.worn, (launch.coat, launch.iris));
        assert!(!outcome.parked);
        assert!(
            !owner.commit_hello_due(true, outcome),
            "a launch rung spends no hello"
        );

        // A favourite-rung verdict with a different pair parks on the next
        // emission.
        let fav = KittyLook {
            coat: (launch.coat + 5) % 16,
            iris: (launch.iris + 3) % 8,
            ..KittyLook::default()
        };
        owner.set_look(
            (fav.coat, fav.iris),
            PetArrival::Quiet,
            CompanionRung::Favourite,
        );
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(32),
            Some((4, 12)),
        );
        let mut free = Vec::new();
        let (_, parked) = owner.emit(&t, &[], theme, &mut decos, &mut free);
        assert!(parked.parked, "the favourite is parked for the handoff");
        assert_eq!(
            parked.worn, outcome.worn,
            "…while the old coat is still worn"
        );
        assert!(
            !owner.commit_hello_due(true, parked),
            "a favourite is always quiet"
        );

        // Off glass: nothing is drawn, nothing is synced, the fold is zero.
        owner.retire();
        let idle = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(48),
            None,
        );
        assert!(!idle.on_glass);
        let mut free = Vec::new();
        let (fp, _) = owner.emit(&idle, &[], theme, &mut decos, &mut free);
        assert_eq!(fp, 0);
        assert!(free.is_empty());
    }

    /// The hello seam: a program ceremony for a sufficiently different coat
    /// commits on a drawn present (pair in hand ≠ pair on glass), never on a
    /// capture, and a re-sync under Quiet cannot commit again.
    #[test]
    fn the_hello_seam_commits_only_a_performed_ceremony_on_a_present() {
        use crate::kitty_registry::{SUFFICIENT_DIFFERENCE, coat_distance};
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        let theme = ContrastFallback {
            bg: 0,
            cursor: 0x00FF_FFFF,
            accent: 0,
        };
        let (_, first) = owner.emit(&t, &[], theme, &mut decos, &mut Vec::new());
        let worn = first.worn;
        let far = (0u8..16)
            .find(|&c| coat_distance(worn.0, c) >= SUFFICIENT_DIFFERENCE)
            .expect("a sufficiently different coat exists");

        owner.set_look((far, worn.1), PetArrival::Ceremony, CompanionRung::Program);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(32),
            Some((4, 12)),
        );
        let (_, outcome) = owner.emit(&t, &[], theme, &mut decos, &mut Vec::new());
        assert_eq!(outcome.worn, worn, "the ceremony is parked, not yet worn");
        assert!(
            owner.commit_hello_due(true, outcome),
            "a drawn present spends the hello"
        );
        assert!(
            !owner.commit_hello_due(false, outcome),
            "a capture never does"
        );

        // The host committed and its latch fell to Quiet: the agreeing re-sync
        // cannot commit again.
        owner.set_look((far, worn.1), PetArrival::Quiet, CompanionRung::Program);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(48),
            Some((4, 12)),
        );
        let (_, again) = owner.emit(&t, &[], theme, &mut decos, &mut Vec::new());
        assert!(!owner.commit_hello_due(true, again));
    }

    /// The species is the brain's identity from the moment the host chooses
    /// it, before any frame. A drain rebuilds the brain and drops it; the next
    /// prepare restores the host's choice.
    #[test]
    fn a_chosen_species_is_the_brains_at_once_and_a_drain_drops_it_until_the_next_prepare() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        owner.set_species(PetSpecies::Dog);
        assert_eq!(
            owner.brain().species(),
            PetSpecies::Dog,
            "a cold owner's brain already wears the chosen animal"
        );
        let now = materialize(&mut owner, &mut decos);
        owner.drain();
        assert_eq!(
            owner.brain().species(),
            PetSpecies::default(),
            "a drain is a fresh brain: the identity is gone until the next prepare"
        );
        assert_ne!(PetSpecies::default(), PetSpecies::Dog, "fixture");
        let _ = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        assert_eq!(owner.brain().species(), PetSpecies::Dog);
    }

    /// The owner retire keeps durable identity (species, worn look), drops
    /// every surface-relative fact (body, cadence debt, hit rect) and
    /// re-baselines the command probe for a new session.
    /// (`retire_coordinate_space_keeps_the_command_latch_and_retire_owner_rebaselines_it`
    /// covers the two retires within one session.)
    #[test]
    fn retiring_the_owner_keeps_identity_and_drops_the_surface() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        owner.set_species(PetSpecies::Dog);
        let now = materialize(&mut owner, &mut decos);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        let (_, before) = owner.emit(&t, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());

        owner.retire_owner();
        assert!(!owner.brain().is_active() && !owner.needs_frames());
        assert_eq!(owner.hit_rect(), None);
        assert_eq!(
            owner.brain().species(),
            PetSpecies::Dog,
            "durable identity survives the edge"
        );
        assert_eq!(
            owner.brain().worn_pair(),
            Some(before.worn),
            "…and so does the coat"
        );

        // The replacement owner's first completion is a baseline, not grief.
        let mut facts = facts_with(Some((4, 12)));
        facts.session = 2;
        facts.cmd_done = Some((1, 1, None));
        let host = host_at(now + Duration::from_millis(32));
        let _ = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(!owner.brain().grieving());
    }

    /// One owned, focused frame's facts and host at `now`, for the tests
    /// below that shape a single field.
    fn prepared(
        owner: &mut CompanionOwner,
        decos: &mut WordDecorations,
        facts: &TerminalFacts,
        host: &HostFrameInput,
        obscured: bool,
    ) -> PetTick {
        owner.prepare(
            PetFacts {
                facts,
                host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured,
                pane: None,
                room: None,
            },
            decos,
        )
    }

    /// A DECTCEM-hidden caret is still a caret: the pet keeps chasing it and is
    /// told it is unpainted, instead of losing it for every TUI repaint. The
    /// yield box claims only a painted caret.
    #[test]
    fn a_hidden_caret_is_still_chased_and_reported_unpainted() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut facts = facts_with(Some((4, 12)));
        facts.cursor_visible = false;
        let host = host_at(now + Duration::from_millis(16));
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert_eq!(t.sense.caret, Some((4, 12)), "the hidden caret travels");
        assert!(!t.sense.caret_drawn, "…and is reported unpainted");
        assert_eq!(
            t.painted_caret, None,
            "no stale word claim at a hidden caret"
        );
        let frame = owner.tick(t);
        assert!(
            frame.on_glass,
            "the resident keeps its body through the hide"
        );
        assert!(
            frame.companion.is_some_and(|c| c.cell.is_none()),
            "the yield box is the body alone"
        );

        facts.cursor_visible = true;
        let t = prepared(
            &mut owner,
            &mut decos,
            &facts,
            &host_at(now + Duration::from_millis(32)),
            false,
        );
        assert!(
            t.sense.caret_drawn,
            "negative control: a painted caret is drawn"
        );
    }

    /// A hidden caret on a frame the program has not finished is its write
    /// position: the sense carries the caret and paint of the last present
    /// the program finished, and the wrap fact waits for the next finished
    /// frame. A finished frame's hidden caret still travels (the control), and
    /// a torn frame with no finished caret of its session reads none.
    #[test]
    fn a_torn_frames_hidden_cursor_reads_as_the_last_finished_caret() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let at = |ms: u64| host_at(now + Duration::from_millis(ms));
        let finished = facts_with(Some((20, 7)));
        let t = prepared(&mut owner, &mut decos, &finished, &at(16), false);
        assert_eq!((t.sense.caret, t.sense.caret_drawn), (Some((20, 7)), true));
        let _ = owner.tick(t);

        let mut torn = facts_with(Some((2, 0)));
        torn.cursor_visible = false;
        torn.frame_unfinished = true;
        torn.wrap_serial = 1;
        let t = prepared(&mut owner, &mut decos, &torn, &at(32), false);
        assert_eq!(t.sense.caret, Some((20, 7)), "the write cursor is no caret");
        assert!(t.sense.caret_drawn, "…and the finished frame's paint rides");
        assert!(!t.sense.wrapped, "a torn frame leaves the wrap fact unread");
        assert_eq!(t.painted_caret, None, "nothing is painted on this frame");
        let _ = owner.tick(t);

        let mut closed = torn;
        closed.caret = Some((20, 8));
        closed.cursor_visible = true;
        closed.frame_unfinished = false;
        let t = prepared(&mut owner, &mut decos, &closed, &at(48), false);
        assert_eq!(t.sense.caret, Some((20, 8)));
        assert!(t.sense.wrapped, "the wrap is read with the finished frame");
        let _ = owner.tick(t);

        let mut hidden = closed;
        hidden.cursor_visible = false;
        let t = prepared(&mut owner, &mut decos, &hidden, &at(64), false);
        assert_eq!(
            (t.sense.caret, t.sense.caret_drawn),
            (Some((20, 8)), false),
            "control: a finished frame's hidden caret still travels, unpainted"
        );
        let _ = owner.tick(t);

        let mut other = torn;
        other.session = 2;
        let t = prepared(&mut owner, &mut decos, &other, &at(80), false);
        assert_eq!(t.sense.caret, None, "no finished caret in this session");
    }

    /// A CAPTURE is one isolated frame: it reads and spends none of the
    /// present-owned diffs — the completion, the burst, the wrap fact — and
    /// sees no pointer, so the next present still finds every edge. RED before
    /// the law: a static capture spent them all.
    #[test]
    fn a_capture_spends_no_present_latch() {
        for capture in [CaptureMode::LiveCapture, CaptureMode::StaticCapture] {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let now = materialize(&mut owner, &mut decos);
            // Baseline every latch on a present.
            let mut facts = facts_with(Some((4, 12)));
            facts.shell_executing = true;
            let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
            let _ = owner.tick(t);
            // The stream moves: a completion, new output, an autowrap.
            facts.cmd_done = Some((1, 1, Some(40)));
            facts.content_seq = 9;
            facts.wrap_serial = 3;
            let mut host = host_at(now + Duration::from_millis(16));
            host.capture = capture;
            host.pointer_px = Some((55.0, 85.0));
            let t = prepared(&mut owner, &mut decos, &facts, &host, false);
            assert_eq!(
                t.completion, None,
                "{capture:?}: a capture feels no completion"
            );
            assert!(!t.sense.output_burst && !t.sense.wrapped, "{capture:?}");
            assert_eq!(t.sense.pointer, None, "{capture:?}: a capture has no mouse");
            let _ = owner.tick(t);
            assert!(!owner.grieving(), "{capture:?}: nothing was noted");

            // …so the next PRESENT reads every edge the capture left.
            host.capture = CaptureMode::Present;
            host.now += Duration::from_millis(16);
            let t = prepared(&mut owner, &mut decos, &facts, &host, false);
            assert_eq!(t.completion, Some((1, Some(40))), "{capture:?}");
            assert!(t.sense.output_burst && t.sense.wrapped, "{capture:?}");
            assert!(owner.grieving(), "{capture:?}: the failure is felt now");
            let _ = owner.tick(t);
        }
    }

    /// The completion edge is handed back exactly once per completion in the
    /// pet's session, never on first sighting or a session switch.
    #[test]
    fn the_completion_edge_is_handed_back_once_and_never_on_a_session_switch() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let mut now = materialize(&mut owner, &mut decos);
        let mut step = |owner: &mut CompanionOwner, session, cmd_done| {
            now += Duration::from_millis(16);
            let mut facts = facts_with(Some((4, 12)));
            facts.session = session;
            facts.cmd_done = cmd_done;
            let t = prepared(owner, &mut decos, &facts, &host_at(now), false);
            let edge = t.completion;
            let _ = owner.tick(t);
            edge
        };
        assert_eq!(
            step(&mut owner, 7, Some((3, 0, None))),
            None,
            "first sighting"
        );
        assert_eq!(step(&mut owner, 7, Some((3, 0, None))), None, "no change");
        assert_eq!(
            step(&mut owner, 7, Some((4, 2, Some(900)))),
            Some((2, Some(900))),
            "a new completion in the same session"
        );
        assert_eq!(step(&mut owner, 7, Some((4, 2, Some(900)))), None, "once");
        assert_eq!(
            step(&mut owner, 8, Some((11, 1, None))),
            None,
            "a session switch re-baselines silently"
        );
    }

    /// An app surface over the grid keeps the resident OWNED — no retire, the
    /// identity and the lane untouched — but not presented.
    #[test]
    fn an_obscured_surface_keeps_the_pet_owned_but_off_glass() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let facts = facts_with(Some((4, 12)));
        let t = prepared(
            &mut owner,
            &mut decos,
            &facts,
            &host_at(now + Duration::from_millis(16)),
            true,
        );
        assert!(!t.visible && t.sense.caret.is_none());
        let frame = owner.tick(t);
        assert!(!frame.on_glass && owner.hit_rect().is_none());
        assert!(
            owner.brain().is_active(),
            "still the resident: only unpresented"
        );
        let t = prepared(
            &mut owner,
            &mut decos,
            &facts,
            &host_at(now + Duration::from_millis(32)),
            false,
        );
        assert!(
            t.visible,
            "negative control: the uncovered grid presents it"
        );
    }

    /// The load-shed LATCH takes the resident's frame train and nothing else:
    /// the sense it ticks is full motion, and a moving body is offered at the
    /// coarse cadence instead. Only the motion POLICY pins it.
    #[test]
    fn the_shed_latch_takes_the_train_never_the_motion() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let facts = facts_with(Some((4, 12)));
        let mut host = host_at(now + Duration::from_millis(16));
        host.shed_active = true;
        host.shed_envelope = 0.0;
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert!(!t.sense.reduced_motion, "the latch is not a posture");
        let _ = owner.tick(t);
        assert!(owner.brain().needs_frames(), "the body is still moving");
        assert!(!owner.needs_frames(), "…but it owes no frame train");
        assert_eq!(
            owner.next_change_deadline(host.now),
            Some(host.now + RESIDENT_SHED_CADENCE),
            "…it is offered at the coarse cadence"
        );
        // A host that woke for the step and drew nothing (a DEC-2026 hold)
        // is offered a whole cadence out, never a past instant it would
        // re-arm every turn.
        let late = host.now + RESIDENT_SHED_CADENCE * 2;
        assert_eq!(
            owner.next_change_deadline(late),
            Some(late + RESIDENT_SHED_CADENCE)
        );
        host.reduced_motion = true;
        host.now += Duration::from_millis(16);
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert!(t.sense.reduced_motion, "negative control: the policy pins");
        let _ = owner.tick(t);
        assert_eq!(owner.next_change_deadline(host.now), None);
    }

    /// Pointer contact needs the body the last frame DREW (its hit rect), so
    /// a body the host withdrew cannot be touched; the rect goes and the
    /// brain keeps its frame.
    #[test]
    fn a_withdrawn_body_cannot_be_touched_and_keeps_its_brain() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let facts = facts_with(Some((4, 12)));
        let rect = owner.hit_rect().expect("fixture: the resident is drawn");
        let over = (
            (rect.0 + rect.1) as f32 * 0.5,
            (rect.2 + rect.3) as f32 * 0.5,
        );
        let mut host = host_at(now + Duration::from_millis(16));
        host.pointer_px = Some(over);
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert!(t.sense.pointer.is_some(), "a drawn body senses the pointer");
        let _ = owner.tick(t);

        owner.withdraw_hit_rect();
        assert_eq!(owner.hit_rect(), None);
        assert!(owner.brain().is_active(), "the brain keeps its frame");
        assert_eq!(owner.press(now, over), PressOutcome::Pass, "nothing to pet");
        host.now += Duration::from_millis(16);
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert_eq!(t.sense.pointer, None, "a withdrawn body is untouchable");
    }

    /// Serious Mode's drain is a fresh brain (a latched trick and the body
    /// alike), while the session latches survive.
    #[test]
    fn the_drain_is_a_fresh_brain_that_keeps_the_session_latches() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let mut now = materialize(&mut owner, &mut decos);
        let mut facts = facts_with(Some((4, 12)));
        facts.cmd_done = Some((5, 0, None));
        let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
        let _ = owner.tick(t);
        owner.note_trick(now, aterm_lexicon::Trick::Sit, true);
        assert!(owner.brain().pending_trick().is_some());

        owner.drain();
        assert!(!owner.brain().is_active() && owner.brain().pending_trick().is_none());
        assert_eq!(owner.hit_rect(), None);
        // The completion latch still holds seq 5: the same completion is not
        // news, the next one is.
        now += Duration::from_millis(16);
        let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
        assert_eq!(t.completion, None, "seq 5 was already felt");
        let _ = owner.tick(t);
        facts.cmd_done = Some((6, 0, None));
        now += Duration::from_millis(16);
        let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
        assert_eq!(t.completion, Some((0, None)));
    }

    /// The hello commit reads the pair the last emit synced; an emit that put
    /// nothing on glass syncs nothing and leaves no stale ceremony to spend.
    #[test]
    fn an_off_glass_emit_leaves_no_hello_to_commit() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        // Dress the resident (the birth apply) so the next verdict parks.
        let t = frame(&mut owner, &mut decos, now, Some((4, 12)));
        let _ = owner.emit(&t, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());
        let worn = owner
            .brain()
            .worn_pair()
            .expect("fixture: the resident is dressed");
        let other = (0..=u8::MAX)
            .map(|coat| (coat, 0))
            .find(|pair| {
                crate::kitty_registry::coat_distance(worn.0, pair.0)
                    >= crate::kitty_registry::SUFFICIENT_DIFFERENCE
            })
            .expect("a visibly different coat");
        owner.set_look(other, PetArrival::Ceremony, CompanionRung::Program);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        let (_, outcome) = owner.emit(&t, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());
        assert!(
            owner.commit_hello_due(true, outcome),
            "fixture: the on-glass present performed the ceremony"
        );

        let off = CompanionFrame {
            on_glass: false,
            duty: CompanionDuty::Idle,
            body_px: None,
            companion: None,
            ..t
        };
        let (fp, outcome) = owner.emit(&off, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());
        assert_eq!(fp, 0, "nothing drawn");
        assert!(
            !owner.commit_hello_due(true, outcome),
            "an off-glass frame performed nothing, so it commits nothing"
        );
    }

    /// The level rules that hide or retire the pet: a style that stops naming
    /// it retires it outright; serious mode retires it; the Hidden edge and
    /// history hide it (identity kept); load pressure keeps the full resident.
    #[test]
    fn the_level_rules_retire_hide_and_preserve_the_resident_under_load() {
        let mut decos = WordDecorations::default();

        // 'lumen' — the style stopped naming the pet.
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let facts = facts_with(Some((4, 12)));
        let host = host_at(now + Duration::from_millis(16));
        let t = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: GlowOwnership {
                    enabled: true,
                    style: GlowStyle::Lumen,
                    style_raw_names_pet: false,
                },
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(
            !t.on_glass && !owner.needs_frames(),
            "retired at once, lane released"
        );

        // Serious mode takes the glass.
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut host = host_at(now + Duration::from_millis(16));
        host.serious = true;
        let t = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(!t.on_glass && !owner.needs_frames());

        // Hidden: the surface retires, the coat is kept.
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        let (_, worn) = owner.emit(&t, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());
        let mut host = host_at(now + Duration::from_millis(32));
        host.visibility = Visibility::Hidden;
        let t = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: false,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(!t.on_glass);
        assert_eq!(owner.hit_rect(), None);
        assert_eq!(
            owner.brain().worn_pair(),
            Some(worn.worn),
            "the same coat returns"
        );

        // History: hidden, brain still ticking (hit rect cleared on the first frame).
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut history = facts_with(None);
        history.display_offset = 3;
        history.live_viewport = false;
        let host = host_at(now + Duration::from_millis(16));
        let t = owner.sense(
            PetFacts {
                facts: &history,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(!t.on_glass);
        assert_eq!(
            owner.hit_rect(),
            None,
            "history clears the clickable body at once"
        );
        assert!(
            owner.brain().is_active(),
            "…while the brain is still fading"
        );

        // The shed envelope never reaches the resident's opacity.
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        for _ in 0..40 {
            let _ = frame(
                &mut owner,
                &mut decos,
                now + Duration::from_millis(16),
                Some((4, 12)),
            );
        }
        let mut host = host_at(now + Duration::from_millis(32));
        host.shed_envelope = 0.5;
        host.shed_active = true;
        let t = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(t.on_glass);
        assert_eq!(t.pet.alpha, 255, "the full resident remains visible");
        assert_eq!(owner.alpha(), 255);
        assert!(
            owner.brain().is_active(),
            "the resident remains active under the shed"
        );
    }

    /// THE SHED CONTRACT: load shedding takes the resident's 60 fps frame
    /// train and nothing else. `phase`: 0 moving at full motion, 1 the latch
    /// edge, 2 moving under the shed, 3 settled under the shed, 4 moving
    /// again after recovery. `smooth`: no frame stepped farther than the
    /// resident's ordinary motion. `gait`: the latch edge kept the brain's
    /// own pose. `train`: the resident claims the frame train. `paced`: a
    /// moving resident is owed a wake within [`RESIDENT_SHED_CADENCE`].
    /// `coarse`: no wake offered under the shed lands sooner than
    /// [`RESIDENT_SHED_CADENCE`] after the last tick.
    ///
    /// `Buggy` replays four defects: the shipped snap (the latch folded into
    /// the reduced-motion pin: a teleport, a forced seat, no wake), a pet kept
    /// on the 60 fps train under the shed, a settled pet offered at its
    /// full-motion grain under the shed, and a stale latch that never hands
    /// the train back.
    fn resident_shed_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            ResidentShed {
                const Buggy = 0;
                var phase = 0;
                var smooth = 1;
                var gait = 1;
                var train = 1;
                var paced = 1;
                var coarse = 1;
                action Shed when (phase == 0) {
                    phase = 1;
                    smooth = if Buggy == 0 { 1 } else { 0 };
                    gait = if Buggy == 0 { 1 } else { 0 };
                    train = 0;
                    paced = if Buggy == 0 { 1 } else { 0 };
                }
                action Move when (phase == 1) {
                    phase = 2;
                    train = if Buggy == 0 { 0 } else { 1 };
                }
                action Settle when (phase == 2) {
                    phase = 3;
                    train = 0;
                    coarse = if Buggy == 0 { 1 } else { 0 };
                }
                action Recover when (phase == 3) {
                    phase = 4;
                    train = if Buggy == 0 { 1 } else { 0 };
                }
                invariant Continuous: smooth == 1;
                invariant NoForcedSeat: gait == 1;
                invariant MovingStaysPaced: paced == 1;
                invariant ShedTakesTheTrain: phase == 0 || phase == 4 || train == 0;
                invariant RecoveryRestoresTheTrain: phase <= 3 || train == 1;
                invariant ShedWakesAreCoarse: coarse == 1;
                invariant Bounds: phase <= 4 && smooth <= 1 && gait <= 1 && train <= 1
                    && paced <= 1 && coarse <= 1;
            }
        }
    }

    #[test]
    fn resident_shed_model_proves_and_catches_the_snap() {
        let model = resident_shed_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        assert_eq!(
            aterm_spec::verify::uncaught_invariants(&model),
            ["Bounds"],
            "every design invariant has a Buggy branch that breaks it"
        );
    }

    /// The Tier-1 rig: the shipping owner over a REAL terminal, observed and
    /// ticked exactly as the native present does, with the host's two shed
    /// facts and its motion policy handed in per frame.
    struct ShedRig {
        term: aterm_core::terminal::Terminal,
        owner: CompanionOwner,
        decos: WordDecorations,
        now: Instant,
        prev: Option<PetFrame>,
        /// The largest step so far, in cells per 16 ms of clock.
        max_step: f32,
        /// Replay the shipped rule: the latch and its envelope folded into
        /// the reduced-motion pin.
        old_rule: bool,
        /// Every offer after a shed tick, moving or settled, lands no sooner
        /// than [`RESIDENT_SHED_CADENCE`]: `[the owner's, the unfloored
        /// rule's]` — the negative control asks the rule before the floor (a
        /// settled body at its brain's own grain) at the same ticks.
        coarse: [bool; 2],
    }

    /// The resident's ordinary motion bound: 1.2 cells per 16 ms of clock.
    /// Typing through a shed measures at most 0.41; the shipped snap, 5.4.
    const STEP_PER_FRAME: f32 = 1.2;

    impl ShedRig {
        fn new(old_rule: bool) -> Self {
            let mut term = aterm_core::terminal::Terminal::new(GRID.rows, GRID.cols);
            term.process(b"\x1b]11;#000000\x07$ ");
            Self {
                term,
                owner: an_enabled_owner(),
                decos: WordDecorations::default(),
                now: Instant::now(),
                prev: None,
                max_step: 0.0,
                old_rule,
                coarse: [true; 2],
            }
        }

        /// One present `dt` after the last — carrying a key's echo when
        /// `key` is given — returning the frame and whether its step stayed
        /// within the ordinary motion bound.
        fn present(
            &mut self,
            dt: Duration,
            key: Option<u8>,
            shed: bool,
            envelope: f32,
        ) -> (PetFrame, bool) {
            use crate::pet_world::{PetPane, PetWorldFacts};
            self.now += dt;
            if let Some(byte) = key {
                self.owner.note_console_input(self.now, PetInputKind::Text);
                self.term.process(&[byte]);
            }
            let input = self
                .term
                .cell_frame(usize::from(GRID.rows), usize::from(GRID.cols));
            let world = PetWorldFacts::read(&self.term, 1);
            self.owner
                .observe_console(&input, &world, PetPane::full(&input), &[]);
            let facts = TerminalFacts::read(&self.term, 1, false);
            let mut host = host_at(self.now);
            host.shed_active = shed;
            host.shed_envelope = envelope;
            host.reduced_motion = self.old_rule && (shed || envelope < 1.0);
            let t = prepared(&mut self.owner, &mut self.decos, &facts, &host, false);
            let pet = self.owner.tick(t).pet;
            if shed {
                let brain = self.owner.brain();
                let unfloored = if brain.needs_frames() {
                    self.owner.next_change_deadline(self.now)
                } else {
                    brain.next_change_deadline(self.now)
                };
                let late = |offer: Option<Instant>| {
                    offer.is_none_or(|d| d >= self.now + RESIDENT_SHED_CADENCE)
                };
                self.coarse[0] &= late(self.owner.next_change_deadline(self.now));
                self.coarse[1] &= late(unfloored);
            }
            let step = self.prev.map_or(0.0, |p| {
                ((pet.col - p.col).powi(2) + (pet.row - p.row).powi(2)).sqrt()
                    / (dt.as_secs_f32() / 0.016).max(1.0)
            });
            self.max_step = self.max_step.max(step);
            self.prev = Some(pet);
            (pet, step <= STEP_PER_FRAME)
        }

        /// A moving resident is on the train or owed a wake within the
        /// shed cadence of this tick; a settled one owes nothing.
        fn paced(&self) -> bool {
            !self.owner.brain().needs_frames()
                || self.owner.needs_frames()
                || self
                    .owner
                    .next_change_deadline(self.now)
                    .is_some_and(|d| d <= self.now + RESIDENT_SHED_CADENCE)
        }
    }

    /// One phase's observation, as a [`resident_shed_model`] state.
    fn shed_state(
        phase: i64,
        [smooth, gait, train, paced, coarse]: [bool; 5],
    ) -> BTreeMap<&'static str, i64> {
        BTreeMap::from([
            ("phase", phase),
            ("smooth", i64::from(smooth)),
            ("gait", i64::from(gait)),
            ("train", i64::from(train)),
            ("paced", i64::from(paced)),
            ("coarse", i64::from(coarse)),
        ])
    }

    /// THE SHED EDGE ON GLASS (the owner's "glitching of the cursor kitty
    /// pet"): typing at 80 ms/key, the load-shed latch engages mid-run, holds
    /// while the typing goes on and the resident settles, then clears. The
    /// shipping owner is driven like the native host: every 16 ms while it
    /// claims the train, and under the shed only at key echoes and at its own
    /// coarse offer. Each phase must be a transition of [`resident_shed_model`];
    /// the shipped rule, replayed on the same drive, must be rejected at the
    /// edge, and the unfloored rule's settled offers at the settle.
    #[test]
    fn a_shed_edge_never_snaps_or_seats_a_moving_resident() {
        use crate::kitty_pet::PetAction;
        const FRAME: Duration = Duration::from_millis(16);
        const KEY: Duration = Duration::from_millis(80);
        let model = resident_shed_model();
        let step = |state: &mut BTreeMap<&'static str, i64>,
                    action: &str,
                    observed: BTreeMap<&'static str, i64>|
         -> Result<(), String> {
            let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                &model,
                &[],
                state,
                &observed,
                Some(action),
                "real resident across a load-shed edge",
            );
            if !ok {
                return Err(format!("{action}: {observed:?}: {why}"));
            }
            *state = observed;
            Ok(())
        };
        for old_rule in [false, true] {
            let mut rig = ShedRig::new(old_rule);
            // The resident arrives and settles before the first key.
            for _ in 0..40 {
                let _ = rig.present(Duration::from_millis(100), None, false, 1.0);
            }
            let mut text = b"the quick brown fox jumps over the lazy dog and on and on".iter();
            let mut state = model.init_state();

            // Phase 0: full motion — the typing run, on the train.
            let (mut smooth, mut paced) = (true, true);
            let mut prev = rig.present(FRAME, None, false, 1.0).0;
            let mut frames = 0u32;
            while frames < 60 || prev.action != PetAction::Run {
                let key = frames
                    .is_multiple_of(5)
                    .then(|| *text.next().expect("fixture: text"));
                let (pet, ok) = rig.present(FRAME, key, false, 1.0);
                smooth &= ok;
                prev = pet;
                frames += 1;
            }
            assert!(
                smooth && rig.owner.needs_frames(),
                "fixture: a smooth run on the train"
            );

            // Phase 1: the latch edge, before the envelope has begun to fall.
            let (edge, ok) = rig.present(FRAME, None, true, 1.0);
            let gait = !matches!(edge.action, PetAction::Sit | PetAction::Sleep);
            let edge_state = shed_state(
                1,
                [
                    ok,
                    gait,
                    rig.owner.needs_frames(),
                    rig.paced(),
                    rig.coarse[0],
                ],
            );
            if let Err(why) = step(&mut state, "Shed", edge_state) {
                assert!(old_rule, "the edge {prev:?} -> {edge:?}: {why}");
                println!(
                    "shipped rule: max step {:.2} cells per 16 ms through the edge, {:?} -> {:?}; rejected: {why}",
                    rig.max_step, prev.action, edge.action
                );
                continue;
            }
            assert!(
                !old_rule,
                "negative control: the shipped snap passed the edge"
            );

            // Phase 2: typing goes on under the shed; the host presents at
            // the key echoes and at the resident's own coarse offer.
            let shed_at = rig.now;
            let mut next_key = rig.now + KEY;
            let mut train = false;
            for _ in 0..20 {
                loop {
                    let offer = rig.owner.next_change_deadline(rig.now);
                    let at = offer.map_or(next_key, |o| o.min(next_key));
                    let key = (at >= next_key).then(|| *text.next().expect("fixture: text"));
                    let dt = at
                        .saturating_duration_since(rig.now)
                        .max(Duration::from_millis(1));
                    let fall = 1.0 - (rig.now + dt - shed_at).as_secs_f32() / 0.45;
                    let (_, ok) = rig.present(dt, key, true, fall.max(0.0));
                    smooth &= ok;
                    paced &= rig.paced();
                    train |= rig.owner.needs_frames();
                    if key.is_some() {
                        next_key = rig.now + KEY;
                        break;
                    }
                }
            }
            let observed = shed_state(2, [smooth, gait, train, paced, rig.coarse[0]]);
            step(&mut state, "Move", observed).expect("moving under the shed");

            // Phase 3: the keys stop; the host wakes only on the offer.
            let mut settled = false;
            for _ in 0..400 {
                if !rig.owner.brain().needs_frames() {
                    settled = true;
                    break;
                }
                let at = rig.owner.next_change_deadline(rig.now);
                let dt = at.map_or(Duration::from_millis(100), |d| {
                    d.saturating_duration_since(rig.now)
                });
                let (_, ok) = rig.present(dt.max(Duration::from_millis(1)), None, true, 0.0);
                smooth &= ok;
                paced &= rig.paced();
                train |= rig.owner.needs_frames();
            }
            assert!(settled, "fixture: the resident settles under the shed");
            // Settled, it is woken at its own next visible step (a breath
            // pixel, a blink, a mote), never sooner than the shed cadence.
            let (settled_at, mut wakes) = (rig.now, 0u32);
            while rig.now < settled_at + Duration::from_secs(3) {
                let Some(at) = rig.owner.next_change_deadline(rig.now) else {
                    break;
                };
                let dt = at
                    .saturating_duration_since(rig.now)
                    .max(Duration::from_millis(1));
                let (_, ok) = rig.present(dt, None, true, 0.0);
                smooth &= ok;
                paced &= rig.paced();
                train |= rig.owner.needs_frames();
                wakes += 1;
            }
            assert!(
                wakes > 3,
                "fixture: a breathing seat is woken on its offers"
            );
            // Negative control: the rule before the floor, asked at the same
            // ticks, offered the seat's own sub-cadence grain (a breathing
            // cat woke the shed loop at 16-20 Hz); the model rejects it.
            let unfloored = shed_state(3, [smooth, gait, train, paced, rig.coarse[1]]);
            assert!(
                step(&mut state.clone(), "Settle", unfloored).is_err(),
                "negative control: the unfloored settled offers passed"
            );
            let observed = shed_state(3, [smooth, gait, train, paced, rig.coarse[0]]);
            step(&mut state, "Settle", observed).expect("settled under the shed");

            // Phase 4: the latch clears, the envelope ramps, and a key moves
            // the resident: it is back on the train.
            let clear_at = rig.now;
            let mut train = false;
            for i in 0..8 {
                let rise = (rig.now + FRAME - clear_at).as_secs_f32() / 0.25;
                let key = (i == 0).then_some(b'!');
                let (_, ok) = rig.present(FRAME, key, false, rise.min(1.0));
                smooth &= ok;
                train |= rig.owner.needs_frames();
            }
            let observed = shed_state(4, [smooth, gait, train, paced, rig.coarse[0]]);
            step(&mut state, "Recover", observed).expect("full motion resumes");
            println!("new rule: max step {:.2} cells per 16 ms", rig.max_step);
        }
    }

    /// Capture follows the caller's existing motion policy; it must not
    /// introduce a static scheduler posture merely because it is a capture.
    /// Disabling retires the remembered posture, and a fresh live frame
    /// resolves it again on re-enable.
    #[test]
    fn capture_and_reenable_do_not_leave_stale_static_scheduler_custody() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let mut now = materialize(&mut owner, &mut decos);
        let facts = facts_with(Some((4, 12)));
        let mut host = host_at(now);
        host.capture = CaptureMode::StaticCapture;
        owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(
            owner.brain().needs_frames(),
            "the captured owner still has live motion"
        );
        assert!(
            owner.needs_frames(),
            "capture alone must not cancel that motion"
        );
        assert!(!owner.last_static);
        // The rejected capture implementation remembers 'static' from the
        // capture mode, despite the full-motion sense it actually ticked.
        owner.last_static = true;
        assert!(
            !owner.needs_frames(),
            "control: the wrong cached fact loses real debt"
        );
        owner.last_static = false;

        now += Duration::from_millis(16);
        host.now = now;
        host.capture = CaptureMode::Present;
        host.shed_envelope = 0.0;
        host.shed_active = true;
        owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        // The shed moves the resident off the train without a static posture.
        assert!(!owner.last_static && owner.last_shed_tick == Some(now));
        assert!(!owner.needs_frames());
        owner.set_enabled_seed(false, 0x5EED);
        assert!(!owner.last_static && owner.last_shed_tick.is_none() && !owner.needs_frames());
        assert_eq!(owner.hit_rect(), None);
        owner.set_enabled_seed(true, 0x5EED);
        now += Duration::from_millis(16);
        host.now = now;
        host.shed_envelope = 1.0;
        host.shed_active = false;
        owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert!(!owner.last_static);
        assert!(
            owner.brain().needs_frames(),
            "the new appearance owes live frames"
        );
        assert!(
            owner.needs_frames(),
            "re-enable must expose the new motion debt"
        );
    }

    /// The shipping owner retains a full, pettable resident for the held
    /// song and every tail cutoff, even when a competing head has live alpha.
    #[test]
    fn a_song_and_its_tail_keep_full_resident_custody() {
        for reduced_motion in [false, true] {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let mut now = materialize(&mut owner, &mut decos);
            let facts = facts_with(Some((4, 12)));
            for drive in [1.0, 0.5, 0.4, 0.329, 0.1, 0.0] {
                now += Duration::from_millis(16);
                let mut host = host_at(now);
                host.reduced_motion = reduced_motion;
                let result = owner.sense(
                    PetFacts {
                        facts: &facts,
                        host: &host,
                        glow: pet_glow(),
                        sing: SingFacts {
                            drive,
                            flying_alpha: 200,
                        },
                        focused: true,
                        obscured: false,
                        pane: None,
                        room: None,
                    },
                    &mut decos,
                );
                assert_eq!(result.duty, CompanionDuty::Pet, "drive={drive}");
                assert!(result.on_glass);
                assert_eq!(result.pet.alpha, 255);
                assert!(result.body_px.is_some(), "the full body owns geometry");
                assert!(owner.hit_rect().is_some(), "the resident stays pettable");
                assert!(
                    result.companion.is_some_and(|c| !c.guards_caret),
                    "word companions yield to the resident body, not a head's caret band"
                );
            }
        }
    }

    /// Reduced motion pins the resident at its station: it is drawn, and a
    /// caret jump moves it without a walk (no lift, no cadence debt beyond
    /// the frame).
    #[test]
    fn reduced_motion_pins_the_pet_at_its_station() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = Instant::now();
        let mut host = host_at(now);
        host.reduced_motion = true;
        let facts = facts_with(Some((4, 12)));
        let first = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert_eq!(
            first.pet.alpha, 255,
            "the first reduced frame has real pixels"
        );
        let far = facts_with(Some((20, 70)));
        let mut host = host_at(now + Duration::from_millis(16));
        host.reduced_motion = true;
        let jumped = owner.sense(
            PetFacts {
                facts: &far,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
                obscured: false,
                pane: None,
                room: None,
            },
            &mut decos,
        );
        assert_eq!(jumped.pet.alpha, 255);
        assert_eq!(jumped.pet.lift, 0.0, "no arc");
        assert_eq!(
            jumped.pet.row, 20.0,
            "the station follows the caret row at once"
        );
    }

    /// Peek cues are forwarded as the landed head's centre in fractional
    /// cells; a far peek is scenery and a near one latches a bat — observed
    /// through the brain acting on it once it is on the ground.
    #[test]
    fn peek_cues_reach_the_brain_as_fractional_cells() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let now = settle(&mut owner, &mut decos, now);
        let settled = frame(&mut owner, &mut decos, now, Some((4, 12)));
        assert!(!owner.needs_frames(), "fixture: settled");
        // A head that landed one column right of the cat's feet, same row.
        let cx = ((settled.pet.col + 4.0) * 10.0) as i32;
        let cy = (settled.pet.row * 20.0) as i32;
        let mut cues = vec![PeekCue {
            row: 4,
            col: 16,
            head_px: (cx - 10, cx + 10, cy - 10, cy + 10),
        }];
        owner.note_peeks(now, cues.drain(..), (10, 20));
        assert!(cues.is_empty(), "the drain is consumed");
        assert!(owner.needs_frames(), "a latched near peek re-arms the lane");
    }

    /// A grid of `rows`×`cols` cells with no ink at all over one ground.
    fn blank_glass(rows: u16, cols: u16, bg: [u8; 3]) -> Vec<Vec<RenderCell>> {
        let cell = RenderCell {
            ch: ' ',
            bg,
            ..RenderCell::default()
        };
        vec![vec![cell; usize::from(cols)]; usize::from(rows)]
    }

    /// Colour-key fallback: over blank glass the sampler finds no principal
    /// ink, so the frozen palette keys off [`ContrastFallback`]'s cursor slot
    /// (native `cursor_color_u32`), not the default foreground. Checked both
    /// ways: the key is `from_rgb(bg, cursor, accent)`, and a neutral cursor on
    /// the same glass dresses a different cat.
    #[test]
    fn blank_glass_keys_the_pets_contrast_off_the_cursor_colour() {
        let bg = 0x0010_1010;
        let cells = blank_glass(GRID.rows, GRID.cols, [0x10, 0x10, 0x10]);
        let dress = |cursor: u32, accent: u32| {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let now = materialize(&mut owner, &mut decos);
            let t = frame(
                &mut owner,
                &mut decos,
                now + Duration::from_millis(16),
                Some((4, 12)),
            );
            assert!(t.on_glass, "fixture: the body is drawn");
            let _ = owner.emit(
                &t,
                &cells,
                ContrastFallback { bg, cursor, accent },
                &mut decos,
                &mut Vec::new(),
            );
            owner
                .brain()
                .appearance_colors()
                .expect("a drawn appearance freezes its palette")
        };

        let red = dress(0x00FF_0000, 0x00FF_2020);
        assert_eq!(
            red,
            CatColorKey::from_rgb(bg, 0x00FF_0000, 0x00FF_2020),
            "no ink under the body: the key is the cursor colour's"
        );
        let neutral = dress(0x0080_8080, 0x0080_8080);
        assert_eq!(neutral, CatColorKey::from_rgb(bg, 0x0080_8080, 0x0080_8080));
        assert_ne!(
            red.accent, neutral.accent,
            "the cursor slot decides the accent family on blank glass"
        );
    }

    /// The two retires: `retire_coordinate_space` (focus loss, torn space) is
    /// surface only, so a later completion is still felt; `retire_owner`
    /// (terminal replaced) also re-baselines the command probe, so the new
    /// owner's first completion is a silent baseline. Both drop body and hit
    /// rect.
    #[test]
    fn retire_coordinate_space_keeps_the_command_latch_and_retire_owner_rebaselines_it() {
        let sense_cmd = |owner: &mut CompanionOwner,
                         decos: &mut WordDecorations,
                         now: Instant,
                         cmd_done: Option<(u64, i32, Option<u64>)>| {
            let mut facts = facts_with(Some((4, 12)));
            facts.cmd_done = cmd_done;
            let host = host_at(now);
            let _ = owner.sense(
                PetFacts {
                    facts: &facts,
                    host: &host,
                    glow: pet_glow(),
                    sing: SingFacts::default(),
                    focused: true,
                    obscured: false,
                    pane: None,
                    room: None,
                },
                decos,
            );
        };

        for owner_edge in [false, true] {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let now = materialize(&mut owner, &mut decos);
            sense_cmd(&mut owner, &mut decos, now, Some((9, 0, None)));
            assert!(owner.hit_rect().is_some(), "fixture: a body to retire");

            if owner_edge {
                owner.retire_owner();
            } else {
                owner.retire_coordinate_space();
            }
            assert_eq!(
                owner.hit_rect(),
                None,
                "either retire drops the frame's coordinate artifact"
            );
            assert!(
                !owner.brain().is_active() && !owner.needs_frames(),
                "either retire drops the body and its lane claim ({owner_edge})"
            );

            sense_cmd(
                &mut owner,
                &mut decos,
                now + Duration::from_millis(16),
                Some((10, 1, None)),
            );
            if owner_edge {
                assert!(
                    !owner.grieving(),
                    "the replacement owner's first completion is a silent baseline"
                );
            } else {
                assert!(
                    owner.grieving(),
                    "a surface retire keeps feeling this session's completions"
                );
            }
        }
    }
}
