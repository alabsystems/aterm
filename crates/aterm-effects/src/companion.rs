// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ONE PET DRIVER — [`CompanionOwner`]: everything that OWNS a resident
//! pet brain per surface, SENSES for it, decides which coat it wears, decides
//! CUSTODY (pet vs flying head vs idle) and DRAWS it, in one place.
//!
//! Owner, 2026-08-30: *"first, you need to migrate the pet from the app and
//! move it into the engine"*. Until this module the brain, the art, the baker
//! and the emitter were engine-side while the DRIVER lived in `aterm-gui`
//! in four copies (single-pane present, composed present, two capture
//! splices) — and the website's `/terminal` could not show the kitty. The
//! laws below are that driver, ported VERBATIM from `app_render.rs` and
//! `app_mouse.rs` (each function keeps its doc block and its owner rulings)
//! so the pipeline (Phase 1) and, from Phase 2, the GUI's `WindowState`
//! run the same code instead of two.
//!
//! Scope cardinality: the owner BORROWS the frame's `WordDecorations` at
//! [`CompanionOwner::sense`] / [`CompanionOwner::emit`] and never owns one —
//! the flash-limiter root stays where it is declared.
//!
//! Three laws that every host must not undo (they are pinned by tests here):
//!
//! * THE BRAIN TICKS UNCONDITIONALLY. A pet that cannot be drawn is fed
//!   `caret: None`, which is the truth; ticking inside a draw gate would
//!   freeze `needs_frames()` at whatever it last said.
//! * THE CADENCE LAW. The pet is a resident, so its cadence gate is
//!   [`crate::kitty_pet::PetBrain::needs_frames`] (something is moving),
//!   never `is_active()` (a cat exists).
//! * THE SEED DOOR. The look comes from
//!   [`crate::kitty_registry::KittyLook::for_launch`] over a seed the HOST
//!   mints; the engine stays clockless and dieless.

use aterm_core::render::FreeSprite;
use aterm_core::terminal::RenderCell;
use aterm_time::Instant;

use crate::cat_baker::CatColorKey;
use crate::cursor_glow::GlowStyle;
use crate::host::{
    CaptureMode, HostFrameInput, PressOutcome, SingFacts, TerminalFacts, Visibility,
};
use crate::kitty_pet::{
    ART_ASPECT, ART_ROWS, PetArrival, PetBrain, PetFrame, PetSense, PetSpecies, SyncLookOutcome,
};
use crate::kitty_registry::KittyLook;
use crate::word_decorations::{
    CatFootprint, CompanionOnGlass, EffectGeom, PeekCue, PetCursorFrame, WordDecorations,
};

// ── the two enums the GUI used to own ───────────────────────────────────────

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
/// law itself — favourite > program (with tenure) > launch kitty — follows
/// in Phase 5.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompanionRung {
    /// The pinned favourite won. The user's explicit choice announces
    /// nothing (the USER-ACT-ONLY precedent): always quiet.
    Favourite,
    /// The tenured program cat won — the ONE rung whose arrival may be a
    /// ceremony, as ruled by the host's tenure gate.
    Program,
    /// The launch kitty floor won — "no stronger claim". The base cat is
    /// always home: always quiet.
    Launch,
}

/// WHICH companion — at most ONE, always — puts a body in this frame.
///
/// [`flying_kitty_admitted`] and [`pet_companion_admitted`] compute two
/// ALPHAS, and an alpha is only a permission. This is the custody law as a
/// single value, so the emitters cannot draw two bodies even if both alphas
/// somehow arrived positive: there is one variant, one match, one sprite.
/// Every seam that must agree about the companion reads it —
/// [`CompanionOwner::emit`], the native composed emitter, and
/// [`cursor_companion_on_glass`], which is how the ambient word-cats' pixel
/// yield is guaranteed to describe the sprite that is really drawn rather than
/// the other animal's.
///
/// THE PET WINS A TIE. It is the resident; the flying head is the earned
/// flypast, and in pet mode it is admitted only for the sing-along, exactly
/// when `pet_companion_admitted` is false. A tie is therefore already
/// impossible upstream — this makes it impossible downstream too.
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
/// counts as petting. A cat is not a checkbox: the target moves, so a few
/// pixels of grace keeps an honest aim from sliding off a paw mid-walk.
pub const PET_HIT_SLOP_PX: i32 = 4;

/// Whether a pointer at `(x, y)` (frame px) lands on the pet's drawn body
/// `rect` (`(x0, x1, y0, y1)`, right/bottom exclusive), padded by `slop` on
/// every side. Pure — the petting seam's hit test, unit-testable without a
/// window.
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

/// WHO OWNS THE RESIDENT PET — the config half of its presentation law, split out
/// so ownership and presentation can never drift apart.
///
/// The three terms here are the ones that mean "this surface is supposed to have
/// a pet at all": the trail master is on, the selected style IS the pet style, and
/// the resolved trail is in pet mode. Deliberately EXCLUDED is everything that
/// merely hides a pet that still exists — focus, a scrolled viewport, the load-shed
/// latch — because those come back, and a resident that forgot itself on every
/// blur would return as a different cat.
///
/// [`retire_pet_without_owner`] is the consumer that matters: it is the switch.
#[inline]
#[must_use]
pub fn resident_pet_owner_present(
    pet_mode: bool,
    cursor_trail_enabled: bool,
    style: GlowStyle,
) -> bool {
    pet_mode && cursor_trail_enabled && matches!(style, GlowStyle::RainbowKitty)
}

/// The resident pet's presentation law, shared by glass and explicit capture.
/// Unlike the earned flying kitty, the pet does not require animation or
/// typing momentum: it sleeps and watches at the caret whenever its surface is
/// presentable and the selected rainbow-kitty-pet style owns the trail.
#[inline]
#[must_use]
pub fn resident_pet_presentation_enabled(
    pet_mode: bool,
    cursor_companion_presentable: bool,
    cursor_trail_enabled: bool,
    style: GlowStyle,
) -> bool {
    cursor_companion_presentable
        && resident_pet_owner_present(pet_mode, cursor_trail_enabled, style)
}

/// NOTHING THE RESIDENT PET OWNS MAY OUTLIVE ITS SWITCH: drop it all the moment its
/// trail owner goes away.
///
/// The exact twin of the flying kitty's `retire_kitty_cursor_without_owner`, which
/// the earned flying kitty has always had and the resident pet never did. Without
/// it the only lever a host had was to stop feeding the brain a caret, and that is
/// a graceful EXIT rather than a switch — `PetBrain::tick`'s no-caret arm fades
/// over `FADE_OUT` and keeps the MOTE lane drifting on its own clock.
///
/// WHAT LEAKED, stated precisely (the pixel story is NOT the story). Every pet emitter
/// — single-pane, composed, and the three capture arms — already gates on
/// [`resident_pet_presentation_enabled`], the same predicate this switch moves, so the
/// very first frame after `cursor_trail = false` draws no pet and no mote: the glass is
/// clean. What survived was the BRAIN, and through it the frame train.
/// `PetBrain::needs_frames()` stays true for the whole `FADE_OUT` ramp and for every
/// mote left in the lane, and the native scheduler consumes that directly — it takes
/// `animate_cursor_cat && cursor_pet.needs_frames()` and does NOT take the trail
/// master as a term. So the switch the user threw to make the terminal quieter left
/// the window presenting at 60 fps, for a second or more, to animate a companion it
/// had already stopped drawing. On the owner's minimal-fast Windows directive that
/// is the whole point of the switch, undone.
///
/// Called every frame from every path that ticks the brain, live and capture, so
/// startup-with-the-trail-off, a hot config reload, a style change and a serious-mode
/// toggle all retire through this one line.
/// [`crate::kitty_pet::PetBrain::retire_unowned`] no-ops on an already-retired
/// brain, so "off" costs one predicate per frame.
#[inline]
pub fn retire_pet_without_owner(
    pet_mode: bool,
    cursor_trail_enabled: bool,
    style: GlowStyle,
    pet: &mut PetBrain,
) {
    if !resident_pet_owner_present(pet_mode, cursor_trail_enabled, style) {
        pet.retire_unowned();
    }
}

/// Describe the one cursor companion actually drawn on this frame, for the
/// ambient word-cats' pixel yield.
///
/// Driven by [`CompanionDuty`], the same value the emitters match on, so the
/// rect handed to the engine always belongs to the sprite that is really
/// drawn. That identity is the whole fix for the owner's "two overlapping
/// kitties": the yield used to model the FLYING HEAD as a 2-cell band on the
/// caret, while the head actually flies ~1.75 cells right of the caret column
/// and ~0.4 rows above its row. An ambient cat drawn squarely ON the singing
/// head intersected that band by under 5% of itself, sailed past the
/// one-third stacking threshold, and drew a second kitty on top of the first.
///
/// The resident pet's exact body is authoritative and may outlive a visible
/// caret for the bounded DECTCEM fade. In that case `cell` stays `None`: the
/// ambient-word engine receives the real pixel ownership without inventing a
/// stale caret anchor.
///
/// `head_px` is the flying head's live footprint
/// ([`WordDecorations::kitty_cursor_footprint`], the same rect
/// `kitty_cursor_at_placement` debug-asserts its placement against). `None`
/// degrades to the caret band alone — honest for a host that cannot resolve
/// it, never correct for one that can.
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
/// explicit capture immediately before its one frame tick.
///
/// `pane` is present only for a composed surface. Binding the focused pane must
/// precede the ink read: [`WordDecorations`] parks one independent scan per
/// session, and the live slot otherwise belongs to whichever pane the preceding
/// compose loop visited last. The single-grid callers declare their scan session
/// earlier, before `needs_rescan`, and pass `None` here so that declaration
/// cannot accidentally move after the scan gate.
pub fn prepare_resident_pet_tick(
    word_decos: &mut WordDecorations,
    cursor_pet: &mut PetBrain,
    species: PetSpecies,
    pane: Option<(u64, (i32, i32))>,
) {
    if let Some((session, px_origin)) = pane {
        word_decos.bind_pane(session, px_origin);
    }
    cursor_pet.set_species(species);
    let (spans, live) = word_decos.pet_ink();
    cursor_pet.sense_ink(0, spans, live);
}

/// THE ARRIVAL MAPPING (kitty-motion §2.0.4, Rungs + the sufficient-
/// difference gate): how loudly THIS frame's verdict lands on the pet, from
/// the winner report, the tenure gate's authorised arrival, and the pair the
/// pet is actually wearing. Pure — one law both sync sites call, so
/// single-pane and composed rendering can never rule differently:
///
///   * a NON-program rung (favourite, launch) is always Quiet — a pinned
///     favourite and the base cat announce nothing, ever;
///   * a program rung carries the gate's ruling — Quiet stays Quiet;
///   * an authorised Ceremony is DEMOTED to Quiet when the incoming coat
///     sits within [`crate::kitty_registry::SUFFICIENT_DIFFERENCE`]
///     of the coat on glass ([`crate::kitty_registry::coat_distance`],
///     the min-over-backgrounds metric): a ceremony that announces nothing a
///     viewer can see is noise, so the theater is withheld — and because the
///     commit rides the PERFORMED ceremony, the floor is not stamped and the
///     debt is kept (§2.0.8's identical-pair precedent, generalised to
///     insufficiently-different pairs). A pet never yet dressed (`worn`
///     `None`) has nothing on glass to compare against: the ceremony stands.
///
/// `authorised` is the tenure gate's ruling expressed as a [`PetArrival`]
/// (the gate's own two-variant `Arrival` is isomorphic and stays host-side
/// until Phase 5).
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

/// The pet's petting hit-box for one frame: the brain's LIVE drawn body
/// (`PetFrame::body_px`, grid-interior px) offset into FRAME px by the
/// effects origin — plus, on the composed path, the focused pane's own
/// pixel origin folded into `origin` by the caller. `None` propagates
/// "nothing drawn", which is what CLEARS the stash on undrawn frames.
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

/// Resolve the per-frame pet hit target from the exact presentation decision.
/// A still-fading brain frame can retain non-zero alpha after its caret was
/// hidden; presentation must win so history clears the old clickable body on
/// the very first suppressed frame.
///
/// The four grid metrics `body_px` needs ride in as one [`EffectGeom`] rather
/// than four scalars: they are one thing (this surface's grid), the emitter that
/// must agree with this rect already speaks that type, and four positional `u16`s
/// in a row are exactly the shape a `cols`/`rows` swap hides in.
#[must_use]
pub fn pet_hit_rect_for_frame(
    pet_visible: bool,
    sing: f32,
    frame: &PetFrame,
    geom: EffectGeom,
    origin: (i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    let body = (pet_companion_admitted(pet_visible, sing) && frame.alpha > 0)
        .then(|| frame.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows))
        .flatten();
    pet_hit_rect_win(body, origin)
}

/// PERK-AND-WATCH (wave 2): the burst conjunction, in one pure function so
/// the law is testable without a terminal. A frame is a BURST only when the
/// pane visibly gained output (`scrolled` — new scrollback rows — or
/// `seq_advanced` — the content clock moved within one session) AND the
/// shell reports an executing command (OSC 133/633 C..D — this conjunct is
/// what keeps keystroke echo, which also moves the content clock, from ever
/// perking the pet) AND the viewport is at the live bottom (scrolled-back
/// history is not a stream the pet can see).
#[must_use]
pub fn pet_output_burst(
    scrolled: bool,
    seq_advanced: bool,
    shell_executing: bool,
    live_bottom: bool,
) -> bool {
    (scrolled || seq_advanced) && shell_executing && live_bottom
}

/// THE WRAP FACT (kitty-motion §4.1): one edge-detect over the emulator's
/// autowrap serial, in one pure function so both production feeds (the
/// single-pane present and the composed focused pane) diff by the same law.
/// `wrapped` is true only when the SAME session's serial CHANGED since this
/// surface's last read; a session switch — or the very first read — only
/// stores the new baseline and NEVER reports a wrap, exactly like the
/// content-seq burst latch beside it. The comparison is `!=`, not `>`,
/// because main/alt buffer swaps keep per-grid serials (see
/// `Terminal::wrap_serial`): inequality costs at most one spurious wrap at a
/// swap or restore, where an ordering test would go blind instead.
pub fn wrap_fact_edge(seen: &mut Option<(u64, u64)>, session: u64, serial: u64) -> bool {
    let wrapped = matches!(*seen, Some((sid, s)) if sid == session && s != serial);
    *seen = Some((session, serial));
    wrapped
}

/// POINTER PLAY (wave 2): map the raw frame-pixel pointer onto the pet's
/// pane as a fractional cell `(col, row)` — pointer px minus the pane's
/// frame-space origin (the effects origin, plus the focused pane's own
/// pixel offset on the composed path — exactly `pet_hit_rect_win`'s
/// geometry), over the cell metrics. `None` once the pointer leaves the
/// pane's grid: outside the pane the pointer does not exist for the pet.
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

/// The SINGING FACE is LIVE: the sing drive at/above the S115 face-swap
/// threshold (0.33 — `CatFrame::render_look` swaps to the authored open-mouth
/// meow head there). ONE predicate for both FULL-MOTION render paths' pet
/// caret feeds: while the face is live the pet's caret is withheld, so the pet
/// fades out HOLDING POSITION and the singing face takes the caret; the moment
/// the drive drops back below the threshold the caret re-feeds, and the pet's
/// return is a fresh sighting at its keep-ahead station — never a flinch.
/// Reduced motion uses [`pet_caret_admitted`]'s always-fed hidden resident.
/// A non-finite drive reads as "not live" so a poisoned detector can never
/// starve the pet of its caret.
#[must_use]
pub fn sing_face_live(drive: f32) -> bool {
    drive >= 0.33
}

/// Whether the resident pet may track the caret while a song winds down.
///
/// Full-motion presentation starts the pet's return at the authored 0.33 face
/// swap. Reduced motion has a stepped singer, so it keeps the pet caret-fed for
/// the ENTIRE song while pixel custody remains exclusively with the singer.
/// An already-visible resident therefore stays opaque, and a new resident can
/// finish its 0.30 s fade-in behind the still. Most importantly, a late render
/// may sample drive 1.0 -> 0.0 directly without depending on intermediate
/// wind-down ticks: the cutoff reveals a ready pet instead of a blank frame.
#[must_use]
pub fn pet_caret_admitted(pet_visible: bool, drive: f32, reduced_motion: bool) -> bool {
    pet_visible
        && if reduced_motion {
            true
        } else {
            !sing_face_live(drive)
        }
}

/// THE SONG'S CUSTODY LAW: may the FLYING companion be drawn this frame?
/// Outside pet mode an earned flight always may. In pet mode the resident pet
/// owns the caret, and the flying kitty — the singing face — is admitted ONLY
/// while the sing-along holds the frame (`sing > 0`: the armed hold plus the
/// whole wind-down crossfade). Admission ends exactly when the drive drains
/// to 0. In full motion the pet is already padding back because its caret
/// re-fed at the 0.33 face swap ([`sing_face_live`]); reduced motion keeps the
/// hidden resident caret-fed for the entire song ([`pet_caret_admitted`]).
#[must_use]
pub fn flying_kitty_admitted(pet_mode: bool, sing: f32) -> bool {
    !pet_mode || sing > 0.0
}

/// The pet half of [`flying_kitty_admitted`]: exactly one companion owns a
/// pet-mode frame, including the song's wind-down.
#[must_use]
pub fn pet_companion_admitted(pet_visible: bool, sing: f32) -> bool {
    pet_visible && !flying_kitty_admitted(true, sing)
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

/// The load-shed half of companion admission: a presentable companion stays
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

/// Pin a PET-MODE episode's exit flourish to Plain, on the frame copy the
/// host is about to draw (the state machine's roll becomes presentation-dead;
/// no engine state changes). In pet mode the flying companion exists solely
/// for the sing-along, and its admission ([`flying_kitty_admitted`]) ends the
/// instant the drive drains — a rolled heart/star would either play over the
/// pet's return or be chopped mid-flourish when admission cuts `kitty_alpha`
/// to 0 (the exit emitter gates on it). The song's goodbye is the pet padding
/// back, not a firework.
pub fn pin_pet_mode_exit(pet_mode: bool, frame: &mut crate::kitty_cursor::CatFrame) {
    if pet_mode {
        frame.exit = crate::kitty_cursor::CatExit::Plain;
    }
}

/// The RESOLVED `0x00RRGGBB` colours the palette sampler falls back to when
/// the cells under the pet's prospective body carry no ink of their own —
/// the three arguments the native emitter hands `cursor_cat_color_key`
/// (`emit_single_cursor_companion`: `default_bg, cursor_color, accent`).
///
/// `cursor` is the terminal's CURSOR colour (native `cursor_color_u32`), NOT
/// the default foreground: the pet is the caret's companion, so over blank
/// glass its ink keys off the caret it sits beside. Handing the default
/// foreground here dresses a differently-keyed cat than the native app draws
/// on the same blank prompt line.
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

/// The trail owner's verdict as the host resolved it: whether the trail
/// master is on, which style it parsed to, and whether the RAW style string
/// names a pet at all (`GlowStyle::style_names_any_pet` — the parsed enum
/// cannot tell `rainbow kitty` from `rainbow kitty pet`).
#[derive(Clone, Copy, Debug, Default)]
pub struct GlowOwnership {
    pub enabled: bool,
    pub style: GlowStyle,
    pub style_raw_names_pet: bool,
}

/// One frame's inputs to [`CompanionOwner::sense`]: the emulator's facts, the
/// host's frame, the trail owner, the sing-along coupling, and whether this
/// surface is focused.
#[derive(Clone, Copy, Debug)]
pub struct PetFacts<'a> {
    pub facts: &'a TerminalFacts,
    pub host: &'a HostFrameInput,
    pub glow: GlowOwnership,
    pub sing: SingFacts,
    pub focused: bool,
}

/// What one [`CompanionOwner::sense`] resolved — the frame the emitter draws,
/// the custody verdict, and the yield box the word engine avoids.
#[derive(Clone, Copy, Debug)]
pub struct CompanionFrame {
    /// The brain's frame, its alpha already attenuated by the shed envelope.
    pub pet: PetFrame,
    pub duty: CompanionDuty,
    /// The pet is drawn this frame: owned, presentable, custody won, alpha > 0.
    pub on_glass: bool,
    /// The pet's live body in grid px (`PetFrame::body_px`), `None` when it
    /// is not on glass.
    pub body_px: Option<(i32, i32, i32, i32)>,
    /// The companion handed to `WordDecorations::tick` as the frame's
    /// one-cat-per-caret yield.
    pub companion: Option<CompanionOnGlass>,
    /// `PetFrame::fp` — non-zero while anything rides the lane, byte-stable
    /// once nothing does.
    pub fp: u64,
}

/// The pet's per-session latches. Every key carries the session so a tab or
/// pane switch re-baselines SILENTLY (no stale replay from another session's
/// history), exactly the native `WindowState` fields they replace.
#[derive(Clone, Copy, Debug, Default)]
struct Latches {
    /// `(session, completed_command_seq)` — the once-per-completion latch.
    /// Deliberately the PET's own, separate from any rain latch: the pet must
    /// feel a finished command even when no rain is falling.
    cmd_seq: Option<(u64, u64)>,
    /// `(session, content_seq)` — the PERK-AND-WATCH burst latch: the content
    /// clock's previous reading, so a frame can tell "the pane wrote" from
    /// "the pane repainted".
    content_seq: Option<(u64, u64)>,
    /// `(session, wrap_serial)` — THE WRAP FACT latch, compared with `!=`
    /// (see [`wrap_fact_edge`]).
    wrap_serial: Option<(u64, u64)>,
}

/// THE ONE PET DRIVER. Owns the brain, the identity verdict and the frame
/// latches; borrows the frame's [`WordDecorations`] at `sense`/`emit`.
///
/// Per frame, in order: [`Self::sense`] (ownership switch, focus, species +
/// ink, the emulator's edges, the UNCONDITIONAL tick, custody, the hit rect)
/// BEFORE the word engine's tick — so the companion's yield box is this
/// frame's body — then [`Self::emit`] AFTER it, into the same free-sprite
/// scratch the host publishes.
pub struct CompanionOwner {
    pet: PetBrain,
    species: PetSpecies,
    /// The RAW style string named a pet at the last `sense` (observability).
    style_named: bool,
    /// The host's opt-in (`set_cursor_pet`); `false` at construction is the
    /// byte-identical-off posture.
    enabled: bool,
    /// The `(coat, iris)` verdict the pet is dressed from: the launch look at
    /// `set_enabled_seed`, the native verdict at `set_look`.
    pair: (u8, u8),
    arrival: PetArrival,
    rung: CompanionRung,
    /// A pinned favourite outranks the verdict above (the precedence law's
    /// top rung); `None` on the web.
    favourite: Option<KittyLook>,
    latches: Latches,
    pointer_px: Option<(f32, f32)>,
    /// This frame's drawn body in FRAME px, cleared on every frame the pet is
    /// not drawn so a stale rect can never eat a click.
    hit_rect: Option<(i32, i32, i32, i32)>,
    /// The alpha the last `sense` put on glass (`0` = nothing).
    last_alpha: u8,
    /// The grid the last `sense` resolved against — what `emit` bakes with.
    geom: EffectGeom,
    /// The pair and mapped arrival the last `emit` synced — the hello seam's
    /// inputs, kept so the host can ask [`Self::commit_hello_due`].
    last_sync: Option<((u8, u8), PetArrival)>,
}

impl Default for CompanionOwner {
    fn default() -> Self {
        let base = KittyLook::default();
        Self {
            pet: PetBrain::default(),
            species: PetSpecies::Cat,
            style_named: false,
            enabled: false,
            pair: (base.coat, base.iris),
            arrival: PetArrival::Quiet,
            rung: CompanionRung::Launch,
            favourite: None,
            latches: Latches::default(),
            pointer_px: None,
            hit_rect: None,
            last_alpha: 0,
            geom: EffectGeom::default(),
            last_sync: None,
        }
    }
}

impl CompanionOwner {
    /// [`resident_pet_owner_present`] — the ownership law, one copy.
    #[must_use]
    pub fn owner_present(pet_mode: bool, trail_master: bool, style: GlowStyle) -> bool {
        resident_pet_owner_present(pet_mode, trail_master, style)
    }

    /// THE SEED DOOR. `look = KittyLook::for_launch(seed)` dresses the pet
    /// (rung `Launch`, always quiet); `enabled = false` retires it outright
    /// ([`PetBrain::retire_unowned`]) so the off state costs nothing and
    /// draws nothing.
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

    /// The NATIVE verdict dresses the pet until Phase 5: the pair the
    /// precedence law chose, the tenure gate's authorised arrival, and the
    /// rung that won (only `Program` may carry a ceremony).
    pub fn set_look(&mut self, pair: (u8, u8), arrival: PetArrival, rung: CompanionRung) {
        self.pair = pair;
        self.arrival = arrival;
        self.rung = rung;
    }

    /// The pinned favourite — the precedence law's top rung, always quiet.
    /// `None` unpins and the pet falls back to the verdict in hand.
    pub fn set_favourite(&mut self, look: Option<KittyLook>) {
        self.favourite = look.map(KittyLook::normalized);
    }

    /// Which animal the pet is drawn as. Applied at the next `sense`, so a
    /// species switch swaps the sprite without resetting the companion.
    pub fn set_species(&mut self, s: PetSpecies) {
        self.species = s;
    }

    /// The pointer in FRAME px, value-shadowed: `true` iff the stored value
    /// changed (the host bumps its frame gate only then, so an idle hover
    /// costs no render). A non-finite sample is dropped and changes nothing.
    ///
    /// This is the BETWEEN-FRAMES channel: a sample fed WITH the frame
    /// ([`HostFrameInput::pointer_px`]) wins over what is stored here, and
    /// the stored value is what a frame carrying `None` reads.
    pub fn set_pointer(&mut self, px: Option<(f32, f32)>) -> bool {
        let next = px.filter(|(x, y)| x.is_finite() && y.is_finite());
        if px.is_some() && next.is_none() {
            return false;
        }
        let changed = self.pointer_px != next;
        self.pointer_px = next;
        changed
    }

    /// THE FRAME'S SENSE — the level rules, then the unconditional tick.
    ///
    /// `!owned ⇒ retire_unowned` (the switch, before the tick); `!focused ⇒`
    /// the surface retires (a fresh sighting on return); species + ink from
    /// `decos.pet_ink()`; the wrap / burst / command-done edges from the
    /// facts through the session-keyed latches; then `tick` — or
    /// `tick_static_capture` under [`CaptureMode::StaticCapture`] —
    /// UNCONDITIONALLY, with `caret: None` when the pet cannot be drawn.
    /// `on_glass = owned && live_viewport && focused && alpha > 0` (through
    /// the custody law); the hit rect is `body_px + origin`.
    pub fn sense(&mut self, f: PetFacts<'_>, decos: &mut WordDecorations) -> CompanionFrame {
        let PetFacts {
            facts,
            host,
            glow,
            sing,
            focused,
        } = f;
        let now = host.now;
        let geom = EffectGeom {
            cell_w: host.geometry.cell_w,
            cell_h: host.geometry.cell_h,
            rows: host.geometry.rows,
            cols: host.geometry.cols,
        };
        self.geom = geom;
        self.style_named = glow.style_raw_names_pet;
        let pet_mode = glow.style_raw_names_pet;
        let trail_master = glow.enabled;

        // The Hidden edge is a hard cursor-coordinate boundary (the native
        // presentability edge): the surface retires and the pet returns as a
        // fresh sighting wearing the same coat.
        if host.visibility == Visibility::Hidden {
            self.retire_surface();
        }
        let decoration_presentable = focused && host.visibility != Visibility::Hidden;
        let cursor_companion_presentable = shed_companion_presentable(
            cursor_companion_presentable(decoration_presentable, facts.live_viewport),
            host.shed_envelope,
        );
        // Ownership: the host's opt-in, serious mode (the glass belongs to
        // the work), and the trail owner's three terms.
        let owned = self.enabled
            && !host.serious
            && resident_pet_owner_present(pet_mode, trail_master, glow.style);
        // THE PET IS NOT EARNED. The flying kitty is a reward for a sustained
        // typing run and fades out when the run ends; the pet is a resident
        // — that is the whole point of a creature that *sleeps*, and an
        // earned companion can never be seen doing it. So the pet takes the
        // PRESENTATION gate (focused, master on, the trail style actually
        // selected, and the shared soft-shed envelope) and none of the
        // momentum gate. Its own fade envelope, driven by whether the caret
        // is visible at all, is the only thing that turns it off.
        let pet_visible = owned
            && resident_pet_presentation_enabled(
                pet_mode,
                cursor_companion_presentable,
                trail_master,
                glow.style,
            );
        // THE SWITCH, BEFORE THE TICK. An unowned pet is retired outright here
        // (motes and all) rather than left to fade, because the tick below is
        // the last one the scheduler owes it — see `retire_pet_without_owner`.
        if !owned {
            self.pet.retire_unowned();
        }
        // EXIT-CODE EMPATHY (wave 1): the pet is a consumer of the completion
        // probe, keyed (session, seq) with its OWN latch. A tab switch
        // re-baselines SILENTLY (no stale replay from another session's
        // history), the None→Some edge within one session is a real first
        // completion, and the note itself only latches — the tick below is
        // what acts, under the brain's precedence ladder.
        {
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
                }
            }
        }
        // PERK-AND-WATCH (wave 2): is the pane genuinely STREAMING this
        // frame? New scrollback rows, the content clock, the OSC 133/633
        // Execute phase, the live bottom. The content-clock diff rides the
        // pet's own (session, seq) latch, re-baselined SILENTLY on a tab
        // switch (a session change is never a burst). The AND with
        // `shell_executing` is what keeps keystroke echo from ever perking
        // the cat (`pet_output_burst`).
        let pet_burst = {
            let advanced = self
                .latches
                .content_seq
                .is_some_and(|(sid, s)| sid == facts.session && facts.content_seq > s);
            self.latches.content_seq = Some((facts.session, facts.content_seq));
            pet_output_burst(
                facts.scrolled,
                advanced,
                facts.shell_executing,
                facts.display_offset == 0,
            )
        };
        // THE WRAP FACT (kitty-motion §4.1): did the EMULATOR resolve an
        // autowrap since this surface's last read of this session?
        let pet_wrapped = wrap_fact_edge(
            &mut self.latches.wrap_serial,
            facts.session,
            facts.wrap_serial,
        );
        // POINTER PLAY (wave 2): the pointer in fractional grid cells —
        // frame px minus the grid origin, over the cell metrics; `None` once
        // it leaves the grid. Pixels-to-cells only: the brain is its own
        // motion sensor (the own-sensor doctrine). THE PRECEDENCE
        // (`HostFrameInput::pointer_px`): a sample fed WITH the frame wins
        // over the one stored through `set_pointer`; a host that feeds the
        // frame `None` reads the stored one — so the native host (which
        // tracks `last_cursor_px` per window and hands it in per frame) and
        // the web host (which pushes pointer events through `set_pointer`
        // between frames) resolve through one line.
        let origin = host.geometry.origin_px;
        let px = host.pointer_px.or(self.pointer_px);
        let pet_pointer = px.and_then(|(x, y)| {
            pet_pointer_cell(
                (f64::from(x), f64::from(y)),
                origin,
                (usize::from(geom.cell_w), usize::from(geom.cell_h)),
                (usize::from(geom.cols), usize::from(geom.rows)),
            )
        });
        // THE INK/SKIN SEAM (gauntlet F1/F3/F5/F8): live glass and capture
        // share this exact pre-tick setup. One frame stale by construction
        // (the rescan runs later in this frame) — content that has not
        // changed has not moved its ink.
        if !focused {
            self.retire_surface();
        }
        prepare_resident_pet_tick(decos, &mut self.pet, self.species, None);
        // THE MOTION POLICY ONLY — never the performance shed. The host's
        // stable preference × focus (an unfocused surface resolves Reduced,
        // the native `MotionPolicy::resolve`); the shed is applied to the
        // presented alpha below and never rewrites the resident's position
        // model mid-walk.
        let reduced_motion = host.reduced_motion || !focused;
        // The FLYING companion's animation gate, the native `animate_cat =
        // cursor_motion.animate(CursorGlow) && shed_envelope > 0.0`: the
        // motion policy AND the shed. It is the caret law's third argument
        // below (`!animate_cat`, exactly as native feeds it) and NOTHING
        // else — the brain's `reduced_motion` above stays the policy alone,
        // because a shed that flipped the resident's motion model would weld
        // a walking body to the caret (`a_performance_shed_never_puts_the_
        // pet_into_reduced_motion` in aterm-gui pins that half).
        let animate_cat = !reduced_motion && host.shed_envelope > 0.0;
        // THE BRAIN TICKS UNCONDITIONALLY: the scheduler asks `needs_frames()`
        // whether to keep the frame lane armed, and that is a pure read of
        // brain state which only `tick` advances. Ticking inside the draw
        // gate would freeze the brain the instant the pet stopped being
        // drawable — an alt-screen app, an unfocused surface, the trail
        // switched off — and the predicate would latch at whatever it last
        // said, pinning a full frame rate on a surface with no cat on it.
        //
        // A pet that cannot be drawn is fed `caret: None`, which is the
        // truth (there is no caret it could be chasing on this surface):
        // it fades out, settles, and releases the lane on its own.
        //
        // THE SONG'S CARET LAW ([`pet_caret_admitted`]): full motion
        // withholds the caret through the 0.33 face swap, so the pet fades
        // out holding position and returns as a fresh sighting. Reduced
        // motion keeps the resident caret-fed behind the opaque static
        // singer, so a stepped or late cutoff always reveals an opaque pet.
        // The third argument is the SINGER's stillness (`!animate_cat`, the
        // shed included): a shed frame freezes the singing face, and a frozen
        // singer is the stepped one the always-fed arm exists for.
        let sense = PetSense {
            now,
            caret: if pet_caret_admitted(pet_visible, sing.drive, !animate_cat) {
                facts.caret
            } else {
                None
            },
            wrapped: pet_wrapped,
            rows: geom.rows,
            cols: geom.cols,
            cell_w: geom.cell_w,
            cell_h: geom.cell_h,
            reduced_motion,
            output_burst: pet_burst,
            pointer: pet_pointer,
        };
        let mut pet_frame = match host.capture {
            CaptureMode::Present => self.pet.tick(sense),
            CaptureMode::StaticCapture => self.pet.tick_static_capture(sense),
        };
        pet_frame.alpha = shed_companion_alpha(pet_frame.alpha, host.shed_envelope);
        // The brain can begin returning below the face-swap threshold, but
        // only one companion is put on glass.
        let pet_on_glass = pet_companion_admitted(pet_visible, sing.drive) && pet_frame.alpha > 0;
        // The flying head's admission: outside pet mode an earned flight
        // always may; in pet mode only while the sing-along holds the frame.
        let kitty_alpha = if flying_kitty_admitted(pet_mode, sing.drive) {
            shed_companion_alpha(sing.flying_alpha, host.shed_envelope)
        } else {
            0
        };
        // THE FRAME'S ONE COMPANION ([`CompanionDuty`]), resolved once and
        // read by both the ambient word-cats' yield box and the emitter — so
        // the rect the word engine avoids is always the sprite really drawn.
        let duty = cursor_companion_duty(pet_on_glass, kitty_alpha, facts.caret);
        let body_px = pet_on_glass
            .then(|| pet_frame.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows))
            .flatten();
        // PETTING (wave 1): stash the body the emitter is about to draw, in
        // FRAME px, for the press seam's hit test — and CLEAR it on every
        // frame the pet is not drawn, so a stale rect can never eat a click
        // after a style switch or a fade-out. Post-tick by construction.
        self.hit_rect = pet_hit_rect_for_frame(pet_visible, sing.drive, &pet_frame, geom, origin);
        self.last_alpha = if pet_on_glass { pet_frame.alpha } else { 0 };
        // The head's own rect is the host's to resolve until Phase 5; `None`
        // degrades the flying duty to the caret band, never the pet.
        let companion = cursor_companion_on_glass(duty, facts.caret, None, body_px);
        CompanionFrame {
            pet: pet_frame,
            duty,
            on_glass: pet_on_glass,
            body_px,
            companion,
            fp: pet_frame.fp(),
        }
    }

    /// WORD-CAT BAT (wave 2): forward the word engine's positioned peek
    /// landings — drained PROMPTLY after its tick, the clear-at-tick-start
    /// law — to the brain as fractional cells of this grid: the landed
    /// HEAD's centre, because the head peeks rows away from its word and the
    /// bat swipes at the head. The note only latches (range-checked
    /// brain-side, retired by its TTL); the brain consumes it on the ground
    /// next tick — the latch law's one frame of latency.
    pub fn note_peeks(
        &mut self,
        now: Instant,
        cues: std::vec::Drain<'_, PeekCue>,
        cell: (u16, u16),
    ) {
        let (cw, ch) = (f32::from(cell.0.max(1)), f32::from(cell.1.max(1)));
        for cue in cues {
            let (x0, x1, y0, y1) = cue.head_px;
            self.pet.note_peek(
                now,
                (x0 + x1) as f32 * 0.5 / cw,
                (y0 + y1) as f32 * 0.5 / ch,
            );
        }
    }

    /// THE PERFORMANCE SEAM (kitty-motion §2.0.4's correction: *"the ceremony
    /// is committed where it RENDERS, not where it is authorised"*): dress
    /// the pet through [`pet_arrival_for_sync`] + `sync_look` and draw the
    /// `(coat, iris)` actually WORN (the latch, never the verdict), with the
    /// colours frozen per appearance — `appearance_colors()` or the
    /// 64-sample footprint walk over the cells under the prospective body
    /// ([`cursor_cat_color_key`], with [`ContrastFallback`]'s resolved
    /// ground / CURSOR colour / accent as the blank-glass fallbacks) — into
    /// `free` via `decos.pet_cursor`.
    ///
    /// Returns the fingerprint fold and the sync outcome; the host's tenure
    /// bookkeeping asks [`Self::commit_hello_due`] whether a hello was spent.
    /// Draws nothing and syncs nothing unless the frame put the pet on glass.
    pub fn emit(
        &mut self,
        t: &CompanionFrame,
        cells: &[Vec<RenderCell>],
        fallback: ContrastFallback,
        decos: &mut WordDecorations,
        free: &mut Vec<FreeSprite>,
    ) -> (u64, SyncLookOutcome) {
        let (pair, rung) = self.dress_pair();
        if !t.on_glass || t.duty != CompanionDuty::Pet {
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
        if let Some(pet_fp) = decos.pet_cursor(
            PetCursorFrame {
                geom,
                colors,
                coat,
                iris,
                pet: t.pet,
            },
            free,
        ) {
            fp ^= pet_fp.rotate_left(29);
        }
        (fp, outcome)
    }

    /// THE COMMIT half of the performance seam, for the host's tenure
    /// bookkeeping. A hello was SPENT only when ALL of:
    ///   * `present` — this emission was a DRAWN present, not a capture: a
    ///     capture splice reaches the same emitter, and a hello nobody saw
    ///     must not be spent;
    ///   * the arrival the last `emit` synced was `Ceremony` — a demoted or
    ///     floor-spaced hello performs nothing and commits nothing (debt kept);
    ///   * the pair in hand DIFFERS from the pair on glass (`pair != worn`) —
    ///     a ceremony that parks nothing was not performed (§2.0.8), and the
    ///     silent alpha-0 apply (worn comes back equal) stays free.
    ///
    /// Deliberately `pair != outcome.worn`, NOT the narrower `outcome.parked`
    /// edge: a capture splice syncing first would consume that edge, and the
    /// present's agreeing re-sync would then never commit. Committing on the
    /// standing difference is restamp-safe because the commit itself consumes
    /// the authorisation (the host's arrival latch falls to Quiet, and the
    /// next `set_look` carries it), so the next present frame maps to Quiet
    /// and cannot commit again.
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

    /// PETTING: a left press at `px` (FRAME px) inside this frame's drawn body
    /// (padded by [`PET_HIT_SLOP_PX`]) strokes the cat and is CONSUMED —
    /// chrome wins, like the tab strip: a press that pets never starts a
    /// selection and is never encoded for a mouse-tracking app. Latch only
    /// (`note_petted` — note, never act); the next tick consumes it on the
    /// ground. The latch re-arms `needs_frames`, but only a tick reads it, so
    /// the host should ask for the frame that runs one.
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

    /// The native `retire_cursor_pet_coordinate_space` (aterm-gui `lib.rs`),
    /// VERBATIM: retire the resident's surface-relative state — the brain's
    /// coordinates and this frame's hit rect, which is the same frame's
    /// coordinate artifact and must disappear atomically with the body — at
    /// a true presentability boundary (the Hidden edge, an unfocused frame,
    /// a capture splice). The session-keyed latches are NOT touched: an
    /// unfocused or hidden surface keeps feeling the commands that finish
    /// under it, and the pet is still grieving or cheering when it returns.
    /// The pet's durable identity survives — species, worn look, contentment,
    /// disposition. Idempotent.
    pub fn retire_coordinate_space(&mut self) {
        self.retire_surface();
    }

    /// THE OWNER EDGE: the terminal under this surface was REPLACED — the
    /// front-terminal identity edge (native `lib.rs` `sync_window`: *"these
    /// probes describe the old terminal's command/output stream"*) or the
    /// cursor-coordinate fence that committed a new grid (native
    /// `app_render.rs` `sync_cursor_effect_coordinate_space`, which nulls
    /// `pet_last_cmd` / `pet_content_seq` beside the retire). Everything
    /// [`Self::retire_coordinate_space`] retires, AND the command/content
    /// probes re-baseline SILENTLY on the replacement owner's first tick —
    /// the old stream's last completion must not replay as the new one's
    /// news. Identity survives here too. Idempotent.
    pub fn retire_owner(&mut self) {
        self.retire_surface();
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
        self.last_alpha = 0;
    }

    /// Retire only the pet's frozen contrast sample (a theme or palette
    /// authority change); behaviour, position and breed handoff continue.
    pub fn invalidate_colors(&mut self) {
        self.pet.invalidate_colors();
    }

    /// THE CADENCE LAW: [`PetBrain::needs_frames`] — something is moving —
    /// never `is_active()` — a cat exists.
    #[must_use]
    pub fn needs_frames(&self) -> bool {
        self.pet.needs_frames()
    }

    /// THE GRIEF GATE's read (gauntlet F4a): the brain's failure droop is on
    /// glass or owed ([`PetBrain::grieving`]). The host hushes the glow's
    /// caret-jump fanfare every frame this is `true` — the pet cannot reach
    /// that emitter; it can say when to be quiet.
    #[must_use]
    pub fn grieving(&self) -> bool {
        self.pet.grieving()
    }

    /// The host's opt-in as last set.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The alpha the last `sense` put on glass (`0` = nothing drawn) — the
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

    /// The animal currently being drawn.
    #[must_use]
    pub fn species(&self) -> PetSpecies {
        self.species
    }

    /// The RAW style string named a pet at the last `sense`.
    #[must_use]
    pub fn style_named(&self) -> bool {
        self.style_named
    }

    /// Read-only access to the brain, for the host's projections (the Tier-1
    /// lifecycle bind reads `is_active`/`needs_frames`/`species` here).
    #[must_use]
    pub fn brain(&self) -> &PetBrain {
        &self.pet
    }

    /// The precedence law's top two rungs as the owner holds them: a pinned
    /// favourite wins (always quiet), else the verdict in hand.
    fn dress_pair(&self) -> ((u8, u8), CompanionRung) {
        match self.favourite {
            Some(fav) => ((fav.coat, fav.iris), CompanionRung::Favourite),
            None => (self.pair, self.rung),
        }
    }

    /// The per-frame surface retire (the native
    /// `retire_cursor_pet_coordinate_space`): the brain's coordinates and the
    /// hit rect, which is the same frame's coordinate artifact and must
    /// disappear atomically with the body. The session-keyed latches are
    /// NOT touched here — an unfocused frame keeps feeling finished commands.
    fn retire_surface(&mut self) {
        self.pet.retire_coordinate_space();
        self.hit_rect = None;
    }
}

#[cfg(test)]
mod law_tests {
    //! The pure laws, pinned exactly as the native app pinned them (moved
    //! here with the laws; the GUI's copies retire in Phase 2).
    use super::*;
    use crate::kitty_cursor::{CatExit, CatFrame, CatPose, CatReaction};
    use aterm_core::terminal::UnderlineStyle;
    use aterm_time::Duration;

    #[test]
    fn resident_pet_does_not_depend_on_flying_kitty_animation() {
        assert!(resident_pet_presentation_enabled(
            true,
            true,
            true,
            GlowStyle::RainbowKitty,
        ));
        for denied in [
            resident_pet_presentation_enabled(false, true, true, GlowStyle::RainbowKitty),
            resident_pet_presentation_enabled(true, false, true, GlowStyle::RainbowKitty),
            resident_pet_presentation_enabled(true, true, false, GlowStyle::RainbowKitty),
            resident_pet_presentation_enabled(true, true, true, GlowStyle::Lumen),
        ] {
            assert!(!denied, "every resident-pet owner gate is necessary");
        }
    }

    /// The ownership law, one copy: pet mode AND the trail master AND the pet
    /// style — and nothing that merely hides a pet (focus, history, the shed)
    /// is a term.
    #[test]
    fn the_owner_is_present_only_with_pet_mode_the_master_and_the_pet_style() {
        assert!(CompanionOwner::owner_present(
            true,
            true,
            GlowStyle::RainbowKitty
        ));
        assert!(!CompanionOwner::owner_present(
            false,
            true,
            GlowStyle::RainbowKitty
        ));
        assert!(!CompanionOwner::owner_present(
            true,
            false,
            GlowStyle::RainbowKitty
        ));
        assert!(!CompanionOwner::owner_present(true, true, GlowStyle::Lumen));
        assert!(!CompanionOwner::owner_present(
            true,
            true,
            GlowStyle::Sparkle
        ));
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

        // THE OWNER'S BUG: the head is NOT at the caret, so a caret band alone
        // under-covers it and an ambient cat drawn on the head sails through
        // the yield. The rect handed in must be the sprite's own.
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
        // Both alphas positive at once is not reachable through the custody
        // law ([`pet_companion_admitted`] is the exact complement of
        // [`flying_kitty_admitted`] in pet mode) — which is why the SHAPE
        // matters: a future seam that broke that complement must still not be
        // able to draw two bodies. The pet, as the resident, wins.
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

    /// A pet that is awake, visible and owes frames — the state a switch has to be
    /// able to interrupt. Returns the brain and the clock it stopped at.
    fn a_live_pet() -> (PetBrain, Instant) {
        let sense = |now, caret| PetSense {
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
        let mut t = Instant::now();
        // Walk the caret so the resident is genuinely mid-motion, not merely faded in.
        for step in 0u16..40 {
            t += Duration::from_millis(16);
            let _ = pet.tick(sense(t, Some((4, 10 + step % 8))));
        }
        assert!(pet.is_active(), "fixture: a visible resident");
        assert!(pet.needs_frames(), "fixture: it is claiming the lane");
        (pet, t)
    }

    /// THE HOST HALF OF THE RESIDENT'S SWITCH — [`retire_pet_without_owner`],
    /// verdict by verdict. Ownership is `pet_mode && trail && the pet style`;
    /// anything less retires, and the owned case must not be disturbed.
    #[test]
    fn every_missing_owner_retires_the_pet_and_a_present_one_never_does() {
        // OWNED — the negative control. Nothing is taken away.
        let (mut owned, _) = a_live_pet();
        retire_pet_without_owner(true, true, GlowStyle::RainbowKitty, &mut owned);
        assert!(
            owned.is_active() && owned.needs_frames(),
            "an owned resident must survive the level call it takes every frame"
        );

        // …and each way of losing the owner, one at a time.
        for (what, pet_mode, trail, style) in [
            (
                "the trail master went off",
                true,
                false,
                GlowStyle::RainbowKitty,
            ),
            (
                "the style stopped being the pet",
                true,
                true,
                GlowStyle::Lumen,
            ),
            (
                "pet mode resolved off",
                false,
                true,
                GlowStyle::RainbowKitty,
            ),
        ] {
            let (mut pet, _) = a_live_pet();
            retire_pet_without_owner(pet_mode, trail, style, &mut pet);
            assert!(!pet.is_active(), "{what}: the pet must paint nothing");
            assert!(
                !pet.needs_frames(),
                "{what}: …and must release the host's frame lane at once — \
                 the scheduler reads exactly this"
            );
        }
    }

    /// LEVEL, NOT EDGE: hosts call this on every frame the pet has no owner, so the
    /// steady "off" state must be a no-op that keeps owing nothing. (A switch that
    /// only fired on an edge would miss a surface that STARTED with the trail off.)
    #[test]
    fn retiring_an_already_retired_pet_is_a_no_op() {
        let (mut pet, _) = a_live_pet();
        for _ in 0..4 {
            retire_pet_without_owner(true, false, GlowStyle::RainbowKitty, &mut pet);
            assert!(!pet.is_active() && !pet.needs_frames());
        }
        // A fresh brain — the startup-with-the-trail-already-off case — is
        // untouched and still owes nothing.
        let mut fresh = PetBrain::default();
        retire_pet_without_owner(true, false, GlowStyle::RainbowKitty, &mut fresh);
        assert!(!fresh.is_active() && !fresh.needs_frames());
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
    /// must be executing, and the viewport must be live.
    #[test]
    fn typing_echo_never_reads_as_a_burst() {
        // Echo: the content clock moves, the shell is NOT executing.
        assert!(!pet_output_burst(false, true, false, true));
        // A real stream: rows scrolled while the shell runs, live bottom.
        assert!(pet_output_burst(true, false, true, true));
        assert!(pet_output_burst(false, true, true, true));
        // Scrolled-back history is not a stream the pet can see.
        assert!(!pet_output_burst(true, true, true, false));
        // An executing shell that wrote nothing this frame is quiet.
        assert!(!pet_output_burst(false, false, true, true));
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

    /// The pet yields exactly at the S115 face-swap threshold — below it the
    /// pet keeps the caret, at/above it the singing face owns it — and a
    /// poisoned (NaN) drive must read "not live" so the pet never starves.
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
    fn reduced_song_keeps_pet_ready_under_singer_and_late_cutoffs_never_blank() {
        use crate::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
        use crate::kitty_cursor::{CursorCat, SingSync};

        let t0 = Instant::now();
        let sense = |now, caret| PetSense {
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

        // The ordinary resident is fully present before the song earns its
        // opaque still singer.
        for _ in 0..30 {
            now += Duration::from_millis(16);
            let _ = pet.tick(sense(now, Some((4, 12))));
        }
        let mut pet_frame = pet.tick(sense(now, Some((4, 12))));
        assert_eq!(pet_frame.alpha, 255);
        assert!(pet_caret_admitted(true, 1.0, true));

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
        assert!(flying_kitty_admitted(true, held.sing));
        assert!(!pet_companion_admitted(true, held.sing));
        // Caret custody and pixel custody are deliberately separate: keeping
        // the resident fed under the still never puts two bodies on glass.
        pet_frame = pet.tick(sense(now, Some((4, 12))));
        assert_eq!(pet_frame.alpha, 255);

        // Tier-1: project the real reduced-motion custody verdict onto the
        // derived state machine. Only the scalar phase bookkeeping comes from
        // the model successor; all three presentation facts come from runtime.
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
            held.alpha > 0,
            pet_frame.alpha == 255 && pet_caret_admitted(true, 1.0, true),
            pet_companion_admitted(true, held.sing) && pet_frame.alpha > 0,
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
            let cat = singer.static_frame(now);
            pet_frame = pet.tick(sense(now, Some((4, 12))));
            let kitty_alpha = if flying_kitty_admitted(true, cat.sing) {
                cat.alpha
            } else {
                0
            };
            let pet_alpha = if pet_companion_admitted(true, cat.sing) {
                pet_frame.alpha
            } else {
                0
            };
            assert_eq!(
                kitty_alpha.max(pet_alpha),
                255,
                "direct 1.0→{drive} reduced cutoff must reveal an opaque pet"
            );
            let observed = bind_handoff(
                &started,
                action,
                kitty_alpha > 0,
                pet_frame.alpha == 255 && pet_caret_admitted(true, drive, true),
                pet_alpha > 0,
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
        // below-face handoff -> drain, with every action driven by the same
        // runtime gates as the direct-late branches above.
        let mut cadenced = started.clone();
        for (drive, action) in [
            (0.50, "SampleAtHalfCutoff"),
            (0.49, "SampleCadencedBelowHalf"),
            (0.329, "SampleBelowFaceSwap"),
            (0.0, "DrainSongTail"),
        ] {
            singer.set_singing(now, SingSync { drive, beat: 0.0 });
            let cat = singer.static_frame(now);
            let kitty_alpha = if flying_kitty_admitted(true, cat.sing) {
                cat.alpha
            } else {
                0
            };
            let pet_alpha = if pet_companion_admitted(true, cat.sing) {
                pet_frame.alpha
            } else {
                0
            };
            assert_eq!(kitty_alpha.max(pet_alpha), 255, "drive {drive}");
            cadenced = bind_handoff(
                &cadenced,
                action,
                kitty_alpha > 0,
                pet_frame.alpha == 255 && pet_caret_admitted(true, drive, true),
                pet_alpha > 0,
            );
        }
        assert_eq!(cadenced[&"phase"], 5);
        assert_eq!(cadenced[&"pet_visible"], 1);

        // A cold/new reduced-motion resident becomes an opaque still on its
        // first live-caret sample. Reduced motion owns no frame-cadence lane,
        // so leaving the ordinary 0.30 s appearance ramp here could strand a
        // transparent pet behind the singer until an unrelated redraw.
        let mut cold_pet = PetBrain::default();
        let first = cold_pet.tick(sense(now, Some((4, 12))));
        assert_eq!(first.alpha, 255);
        for _ in 0..20 {
            now += Duration::from_millis(16);
            let frame = cold_pet.tick(sense(now, Some((4, 12))));
            assert!(pet_caret_admitted(true, 1.0, true));
            assert!(!pet_companion_admitted(true, 1.0));
            assert_eq!(frame.alpha, 255, "the reduced still stays opaque");
            pet_frame = frame;
        }
        assert_eq!(pet_frame.alpha, 255, "pet remains ready under singer");

        // Full motion deliberately keeps the authored 0.33 caret swap.
        assert!(!pet_caret_admitted(true, 0.4, false));
        assert!(pet_caret_admitted(true, 0.329, false));
        assert!(pet_caret_admitted(true, f32::NAN, true));
    }

    /// The swap, end to end at the gate level: an armed song in pet mode
    /// admits the flying kitty's alpha and withholds the pet's caret; a
    /// drained song (drive 0) cuts admission and restores the caret in the
    /// same frame; outside pet mode nothing changes.
    #[test]
    fn armed_song_swaps_the_companions_and_the_drain_swaps_back() {
        let (pet_mode, kitty_enabled, cat_alpha) = (true, true, 200u8);
        // Armed (drive 1): the singing face is the companion.
        let kitty_alpha = if kitty_enabled && flying_kitty_admitted(pet_mode, 1.0) {
            cat_alpha
        } else {
            0
        };
        assert!(kitty_alpha > 0, "armed drive must admit the singing face");
        assert!(
            sing_face_live(1.0),
            "armed drive must withhold the pet caret (fed None)"
        );
        // Drained (drive 0): admission ends, the pet's caret is restored.
        let kitty_alpha = if kitty_enabled && flying_kitty_admitted(pet_mode, 0.0) {
            cat_alpha
        } else {
            0
        };
        assert_eq!(kitty_alpha, 0, "drained drive must cut admission");
        assert!(
            !sing_face_live(0.0),
            "drained drive must re-feed the pet caret"
        );
        // Outside pet mode the earned flight is untouched by the song.
        assert!(flying_kitty_admitted(false, 0.0));
        assert!(flying_kitty_admitted(false, 1.0));
    }

    /// Single-pane rendering used to admit both companions during wind-down.
    #[test]
    fn pet_and_flying_face_are_never_admitted_together() {
        for sing in [0.0, 0.1, 0.3299, 0.33, 1.0, f32::NAN] {
            let flying = flying_kitty_admitted(true, sing);
            let pet = pet_companion_admitted(true, sing);
            assert_ne!(
                pet, flying,
                "pet mode must choose exactly one companion at sing={sing:?}"
            );
        }
        assert!(!pet_companion_admitted(false, 0.0));
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

    /// The pet half of the cursor-viewport lifecycle: the resident remains
    /// lifecycle-live in history (it receives hidden-caret ticks), but its
    /// hit target disappears on the very first suppressed frame and its
    /// scheduler eventually settles instead of sticking at animation cadence.
    #[test]
    fn history_clears_the_pet_hit_target_while_the_brain_settles() {
        let t0 = Instant::now();
        let sense = |now, caret| PetSense {
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
        assert!(
            pet_hit_rect_for_frame(true, 0.0, &live_pet, hit_geom(), (0, 0)).is_some(),
            "the live pet owns a clickable body"
        );
        let hidden_pet = pet.tick(sense(t0 + Duration::from_millis(600), None));
        assert!(
            hidden_pet.alpha > 0,
            "the brain is still fading on the first history frame"
        );
        assert_eq!(
            pet_hit_rect_for_frame(false, 0.0, &hidden_pet, hit_geom(), (0, 0)),
            None,
            "history clears the hit target before the brain's fade completes"
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

    /// A pet-mode summon EXITS PLAIN: whatever flourish the machine rolled,
    /// the pinned frame keeps the plain fade — and outside pet mode the roll
    /// is untouched.
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

    /// THE ARRIVAL MAPPING, rung by rung: non-program rungs are always quiet,
    /// a program rung carries the gate's ruling, an undressed pet keeps its
    /// ceremony, and a ceremony for a coat within sufficient difference of
    /// the one on glass is demoted.
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
    use crate::host::{ChromeGeom, FrameGeom};
    use aterm_time::Duration;

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
            chrome: ChromeGeom::default(),
            pointer_px: None,
            capture: CaptureMode::Present,
            sound_allowed: false,
            geometry: GRID,
        }
    }

    fn facts_with(caret: Option<(u16, u16)>) -> TerminalFacts {
        TerminalFacts {
            session: 1,
            caret,
            cursor_visible: caret.is_some(),
            display_offset: 0,
            live_viewport: true,
            content_seq: 0,
            wrap_serial: 0,
            scrolled: false,
            shell_executing: false,
            cmd_done: None,
            block: None,
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

    /// THE SEED DOOR: the same seed dresses the same pair on any instance
    /// (`KittyLook::for_launch`), a different seed a different cat (found,
    /// not assumed), and `enabled = false` retires the body, the rect and the
    /// lane at once while `enabled()` reports the switch.
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

    /// THE BRAIN TICKS UNCONDITIONALLY. An unfocused surface feeds `caret:
    /// None`; the resident fades, settles and RELEASES the lane. The negative
    /// control is a brain nobody ticks: its `needs_frames` latches true
    /// forever — exactly the frame train ticking-inside-the-gate would pin.
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

    /// The command-done latch: the VERY FIRST read of a session BASELINES
    /// silently (a completion already in its history is not news), the
    /// None→Some edge within one session is a real first completion, a later
    /// same-session completion is felt again, and a session switch
    /// re-baselines without replaying the other session's history.
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

    /// PETTING: a press inside the drawn body (plus the slop band) latches a
    /// pet and is consumed; one outside passes; a retired pet has no body to
    /// press.
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

    /// The pointer reaches the brain as fractional cells (the own-sensor
    /// doctrine): a pointer wandering over the grid builds the brain's
    /// attention and re-arms the lane; one wandering OFF the grid does not
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
            },
            &mut decos,
        );
        assert_eq!(first.pet.alpha, 0, "negative control: a present fades in");
        assert!(present.needs_frames(), "…and claims the lane to do so");
    }

    /// The emitter draws ONE body wearing the pair the brain latched, folds
    /// a non-zero fingerprint, and syncs the verdict: a pinned favourite parks
    /// a differing pair on the first emission (worn stays until the handoff)
    /// and the hello seam never commits a quiet rung.
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

        // A pinned favourite with a different pair parks on the next emission.
        let fav = KittyLook {
            coat: (launch.coat + 5) % 16,
            iris: (launch.iris + 3) % 8,
            ..KittyLook::default()
        };
        owner.set_favourite(Some(fav));
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
        owner.set_favourite(None);
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

    /// The hello seam: a PROGRAM ceremony for a sufficiently different coat
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

    /// The OWNER retire keeps the durable identity (species, the worn look)
    /// and drops every surface-relative fact: body, cadence debt, hit rect —
    /// and re-baselines the command probe so the replacement owner's history
    /// is never replayed (`retire_coordinate_space_keeps_the_command_latch_
    /// and_retire_owner_rebaselines_it` pins the two retires against each
    /// other within ONE session; here the owner is a new session).
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
            owner.species(),
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
            },
            &mut decos,
        );
        assert!(!owner.brain().grieving());
    }

    /// The level rules that hide or retire the pet: a style that stops naming
    /// it retires it outright; serious mode retires it; the Hidden edge and
    /// history hide it (identity kept); the shed attenuates the presented
    /// alpha without touching the brain.
    #[test]
    fn the_level_rules_retire_hide_and_attenuate_as_ruled() {
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

        // The shed: half the presented alpha, the brain untouched.
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
        let t = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts::default(),
                focused: true,
            },
            &mut decos,
        );
        assert!(t.on_glass);
        assert_eq!(t.pet.alpha, 128, "the presented alpha is attenuated");
        assert_eq!(owner.alpha(), 128);
        assert!(
            owner.brain().is_active(),
            "the brain keeps its unscaled state"
        );
    }

    /// The custody law through the owner: while a song holds the frame in
    /// pet mode the flying head owns the duty and the pet is off glass; the
    /// drain hands the frame back to the pet.
    #[test]
    fn a_song_hands_custody_to_the_flying_head_and_the_drain_hands_it_back() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let facts = facts_with(Some((4, 12)));
        let host = host_at(now + Duration::from_millis(16));
        let sung = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts {
                    drive: 1.0,
                    flying_alpha: 200,
                },
                focused: true,
            },
            &mut decos,
        );
        assert_eq!(sung.duty, CompanionDuty::FlyingHead { cell: (4, 12) });
        assert!(!sung.on_glass);
        assert_eq!(
            owner.hit_rect(),
            None,
            "the head is not pettable through the pet's rect"
        );
        assert!(
            sung.companion.is_some_and(|c| c.guards_caret),
            "the yield box is the head's caret band"
        );

        let host = host_at(now + Duration::from_millis(32));
        let drained = owner.sense(
            PetFacts {
                facts: &facts,
                host: &host,
                glow: pet_glow(),
                sing: SingFacts {
                    drive: 0.0,
                    flying_alpha: 200,
                },
                focused: true,
            },
            &mut decos,
        );
        assert_eq!(
            drained.duty,
            CompanionDuty::Pet,
            "drive 0 cuts the head's admission; the pet is the resident"
        );
        assert!(drained.on_glass);
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

    /// THE COLOUR-KEY FALLBACK: over blank glass the sampler finds no
    /// principal ink, so the pet's frozen palette keys off the CURSOR slot
    /// of [`ContrastFallback`] (the native `cursor_color_u32`) — the caret's
    /// colour, not the default foreground. Pinned from both sides: the
    /// frozen key IS `from_rgb(bg, cursor, accent)`, and a neutral cursor on
    /// the same glass keys a differently-dressed cat, so the slot is
    /// load-bearing rather than a coincidence of the fixture.
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

    /// THE TWO RETIRES: `retire_coordinate_space` is the native
    /// `retire_cursor_pet_coordinate_space` verbatim — surface only, so a
    /// completion that lands after a presentability edge is still FELT;
    /// `retire_owner` is the terminal-replaced edge and also re-baselines
    /// the command probe, so the replacement owner's first completion is a
    /// silent baseline, never a replay. Both drop the body and the hit rect.
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
