// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet driver, [`CompanionOwner`]: owns a resident pet brain per surface,
//! senses for it, picks its coat, decides custody (pet vs flying head vs idle)
//! and draws it. The web pipeline and the native `WindowState` run this same code
//! (`docs/DESIGN-host-boundary-2026-08-30.md`). It borrows the frame's
//! `WordDecorations` at [`CompanionOwner::prepare`] / [`CompanionOwner::emit`].
//!
//! Rules every host keeps (pinned by tests here):
//!
//! * The brain ticks unconditionally; a pet that cannot be drawn is fed
//!   `caret: None`. Ticking inside a draw gate would freeze `needs_frames()`.
//! * The cadence gate is [`crate::kitty_pet::PetBrain::needs_frames`]
//!   (something is moving), never `is_active()` (a cat exists).
//! * The look comes from [`crate::kitty_registry::KittyLook::for_launch`] over a
//!   host-minted seed, so the engine stays clockless and dieless.

use aterm_core::render::FreeSprite;
use aterm_core::terminal::RenderCell;
use aterm_time::Instant;

use crate::cat_baker::CatColorKey;
use crate::cursor_glow::GlowStyle;
use crate::host::{
    CaptureMode, HostFrameInput, PressOutcome, SingFacts, TerminalFacts, Visibility,
};
use crate::kitty_pet::{
    ART_ASPECT, ART_ROWS, PET_DEPARTURES_MAX, PetArrival, PetBrain, PetFrame, PetInputKind,
    PetSense, PetSpecies, RoomInbox, SyncLookOutcome,
};
use crate::kitty_registry::KittyLook;
use crate::rainbow_kitty::ARM_MIN;
use crate::word_decorations::{
    CatFootprint, CompanionOnGlass, EffectGeom, PeekCue, PetCursorFrame, WordDecorations,
};

// ── custody and precedence enums ────────────────────────────────────────────

/// Which rung of [`crate::companion_look::companion_precedence`] won a verdict,
/// so the render sync sites can tell a program win (the only one that may carry
/// an arrival ceremony) from a favourite or launch win.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompanionRung {
    /// The pinned favourite won. The user's explicit choice announces nothing:
    /// always quiet.
    Favourite,
    /// The tenured program cat won: the one rung whose arrival may be a
    /// ceremony, as ruled by the host's tenure gate.
    Program,
    /// The launch kitty floor won — "no stronger claim". The base cat is
    /// always home: always quiet.
    Launch,
}

/// Which companion, at most one, puts a body in this frame. The host gates two
/// alphas, which are only permissions; this single value is custody, so no
/// emitter can draw two bodies. [`CompanionOwner::emit`], the native composed
/// emitter and [`cursor_companion_on_glass`] all read it. The pet wins a tie,
/// which upstream gating already makes impossible.
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

// ── the petting hit test ────────────────────────────────────────────────────

/// Petting grace outside the pet's drawn body (frame px, each side): the target
/// moves, so an honest aim must not slide off a paw mid-walk.
pub const PET_HIT_SLOP_PX: i32 = 4;

/// Whether a pointer at `(x, y)` (frame px) lands on the pet's drawn body `rect`
/// (`(x0, x1, y0, y1)`, right/bottom exclusive), padded by `slop` on every side.
#[inline]
#[must_use]
pub const fn pet_rect_hit(rect: (i32, i32, i32, i32), x: f64, y: f64, slop: i32) -> bool {
    let (x0, x1, y0, y1) = rect;
    x >= x0.saturating_sub(slop) as f64
        && x < x1.saturating_add(slop) as f64
        && y >= y0.saturating_sub(slop) as f64
        && y < y1.saturating_add(slop) as f64
}

// ── the palette sampler ─────────────────────────────────────────────────────

/// Resolve the companion's local terminal palette from the grid cells its sprite
/// intersects, capped at 64 samples so this stays allocation-free and O(1).
#[inline]
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
    let mut last_bg: Option<([u8; 3], u8)> = None;
    'rows: for row in r0..=r1 {
        let line = cells.get(row).map_or(&[][..], Vec::as_slice);
        for col in c0..=c1 {
            let cell = line.get(col);
            let background_rgb = cell.map_or(fallback_bg_rgb, |cell| cell.bg);
            let band = match last_bg {
                Some((prev_rgb, prev_band)) if prev_rgb == background_rgb => prev_band,
                _ => {
                    let b = CatColorKey::background_band(pack(background_rgb));
                    last_bg = Some((background_rgb, b));
                    b
                }
            };
            min_background_band = min_background_band.min(band);
            max_background_band = max_background_band.max(band);
            for (dst, src) in bg_sum.iter_mut().zip(background_rgb) {
                *dst += u32::from(src);
            }
            sampled += 1;
            if let Some(cell) = cell
                && !cell.wide
                && cell.ch != ' '
                && cell.ch != '\0'
                && !cell.ch.is_whitespace()
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

// ── ownership and presentation ──────────────────────────────────────────────

/// Whether this surface owns a resident pet: the trail master is on, the style is
/// the pet style, and the trail is in pet mode. Anything that merely hides a pet
/// (focus, a scrolled viewport, the load-shed latch) is excluded: the resident
/// must come back as the same cat.
#[inline]
#[must_use]
pub const fn resident_pet_owner_present(
    pet_mode: bool,
    cursor_trail_enabled: bool,
    style: GlowStyle,
) -> bool {
    pet_mode && cursor_trail_enabled && matches!(style, GlowStyle::RainbowKitty)
}

/// Describe the cursor companion actually drawn this frame, for the ambient
/// word-cats' pixel yield. Driven by [`CompanionDuty`], as the emitters are. The
/// flying head flies ~1.75 cells right of and ~0.4 rows above the caret, so a
/// caret band alone would let an ambient cat draw over it; `head_px` is its live
/// footprint ([`WordDecorations::kitty_cursor_footprint`]), `None` degrading to
/// the caret band. The pet's body is authoritative and may outlive a visible caret
/// for the DECTCEM fade; then `cursor` is `None`, so the yield carries no stale
/// caret anchor.
#[inline]
#[must_use]
pub const fn cursor_companion_on_glass(
    duty: CompanionDuty,
    cursor: Option<(u16, u16)>,
    head_px: Option<(i32, i32, i32, i32)>,
    pet_body_px: Option<(i32, i32, i32, i32)>,
) -> Option<CompanionOnGlass> {
    match duty {
        CompanionDuty::Idle => None,
        CompanionDuty::Pet => match pet_body_px {
            Some(body_px) => Some(CompanionOnGlass::at_body(cursor, body_px)),
            None => None,
        },
        CompanionDuty::FlyingHead { cell } => Some(match head_px {
            Some(head_px) => CompanionOnGlass::at_head(cell, head_px),
            None => CompanionOnGlass::at_cell(cell),
        }),
    }
}

/// Feed the resident pet the presentation context shared by live glass and
/// capture, immediately before its tick. A composed surface passes `pane`, which
/// must be bound before the ink read: [`WordDecorations`] parks one scan per
/// session. Single-grid callers bind their scan session earlier and pass `None`.
#[inline]
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

/// How loudly this frame's verdict lands on the pet, for both sync sites:
///   * a non-program rung (favourite, launch) is always Quiet;
///   * a program rung carries `authorised`, the tenure gate's ruling;
///   * a Ceremony is demoted to Quiet when the incoming coat is within
///     [`crate::kitty_registry::SUFFICIENT_DIFFERENCE`] of the coat on glass: a
///     ceremony nobody can see is noise. It is not committed, so the debt is kept.
///     A pet never yet dressed (`worn` `None`) keeps its ceremony.
#[must_use]
#[inline]
pub fn pet_arrival_for_sync(
    rung: CompanionRung,
    authorised: PetArrival,
    worn: Option<(u8, u8)>,
    incoming_coat: u8,
) -> PetArrival {
    use crate::kitty_registry::{SUFFICIENT_DIFFERENCE, coat_distance};
    if !matches!(rung, CompanionRung::Program) {
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

// ── the per-frame pet facts ─────────────────────────────────────────────────

/// The pet's petting hit box: the brain's drawn body (`PetFrame::body_px`) offset
/// into frame px by `origin`. `None` ("nothing drawn") clears the stash.
#[inline]
#[must_use]
pub const fn pet_hit_rect_win(
    body: Option<(i32, i32, i32, i32)>,
    origin: (i32, i32),
) -> Option<(i32, i32, i32, i32)> {
    match body {
        Some((x0, x1, y0, y1)) => Some((
            x0.saturating_add(origin.0),
            x1.saturating_add(origin.0),
            y0.saturating_add(origin.1),
            y1.saturating_add(origin.1),
        )),
        None => None,
    }
}

/// Perk-and-watch: a frame is an output burst only when the pane visibly gained
/// output (new scrollback rows, or the content clock moved within one session),
/// the shell reports an executing command (OSC 133/633 C..D, so prompt echo never
/// perks the pet), the viewport is at the live bottom, and the pane is not on the
/// alternate screen: a full-screen program rewrites its own cells in place, so
/// its composer would read as a build log.
#[inline]
#[must_use]
pub const fn pet_output_burst(
    scrolled: bool,
    seq_advanced: bool,
    shell_executing: bool,
    live_bottom: bool,
    alt_screen: bool,
) -> bool {
    (scrolled || seq_advanced) && shell_executing && live_bottom && !alt_screen
}

/// Edge-detect the emulator's autowrap serial: true only when the same session's
/// serial changed since this surface's last read; a session switch or first read
/// stores the baseline. `!=` rather than `>`, because main/alt buffer swaps keep
/// per-grid serials: inequality costs at most one spurious wrap at a swap.
#[inline]
pub const fn wrap_fact_edge(seen: &mut Option<(u64, u64)>, session: u64, serial: u64) -> bool {
    let wrapped = matches!(*seen, Some((sid, s)) if sid == session && s != serial);
    *seen = Some((session, serial));
    wrapped
}

/// Map the frame-px pointer onto the pet's pane as a fractional cell `(col, row)`;
/// `None` outside the pane's grid.
#[must_use]
#[inline]
pub const fn pet_pointer_cell(
    pointer_px: (f64, f64),
    origin: (i32, i32),
    cell: (usize, usize),
    grid: (usize, usize),
) -> Option<(f32, f32)> {
    let (cw, ch) = cell;
    if cw == 0 || ch == 0 {
        return None;
    }
    let col = (pointer_px.0 - origin.0 as f64) / cw as f64;
    let row = (pointer_px.1 - origin.1 as f64) / ch as f64;
    if col >= 0.0 && row >= 0.0 && col < grid.0 as f64 && row < grid.1 as f64 {
        Some((col as f32, row as f32))
    } else {
        None
    }
}

// ── the custody law ─────────────────────────────────────────────────────────

/// Whether the open-mouth singing face is live; it does not move custody. A
/// non-finite drive reads as not live.
#[inline]
#[must_use]
pub const fn sing_face_live(drive: f32) -> bool {
    drive >= 0.33
}

/// Resolve [`CompanionDuty`] from this frame's two already-gated alphas.
#[inline]
#[must_use]
pub const fn cursor_companion_duty(
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

/// Cursor companions belong to the live grid: the decoration lifecycle keeps
/// running while history is visible, but no companion is drawn over retained rows.
#[inline]
#[must_use]
pub const fn cursor_companion_presentable(
    decoration_presentable: bool,
    live_viewport: bool,
) -> bool {
    decoration_presentable && live_viewport
}

/// The resident's cadence under the load-shed latch. The shed cuts per-present
/// cost (bloom, aurora, comet), but the resident is one sprite, so the shed never
/// touches its posture (pinning on the latch would teleport a running cat at every
/// latch edge); only the motion policy pins it. Under the shed the resident leaves
/// the frame train and no wake of its lands sooner than this after its last tick
/// (unfloored, a breathing seat woke the shed loop at 16–20 Hz). Its motion is
/// dt-based (dt clamps at 0.1 s), so ~20 Hz stays continuous.
pub const RESIDENT_SHED_CADENCE: aterm_time::Duration = aterm_time::Duration::from_millis(50);

/// Real surface custody for the full resident. Pressure affects its cadence,
/// never whether the body belongs to this surface.
#[inline]
#[must_use]
pub const fn resident_pet_surface_presentable(
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
pub const fn shed_companion_presentable(base_presentable: bool, envelope: f32) -> bool {
    base_presentable && envelope.is_finite() && envelope > 0.0
}

/// Attenuate a companion's alpha by the load-shed envelope at the final
/// presentation seam; the brains keep their unscaled state.
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

/// Whether the soft-shed envelope still owes another presentation: fade-out stops
/// at zero; fade-in starts from that exact zero and stops only at full amplitude.
#[inline]
#[must_use]
pub const fn shed_envelope_transitioning(shed_active: bool, envelope: f32) -> bool {
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
#[inline]
pub const fn pin_pet_mode_exit(pet_mode: bool, frame: &mut crate::kitty_cursor::CatFrame) {
    if pet_mode {
        frame.exit = crate::kitty_cursor::CatExit::Plain;
    }
}

/// The `0x00RRGGBB` colours the palette sampler falls back to over blank cells.
/// `cursor` is the terminal's cursor colour, not the default foreground: over
/// blank glass the pet keys off the caret it sits beside.
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
/// presentation; web: `GlowSpelling::pet_of` over the page's spelling, since
/// the parsed enum cannot tell `rainbow kitty pet` from `rainbow kitty
/// flying`).
#[derive(Clone, Copy, Debug, Default)]
pub struct GlowOwnership {
    pub enabled: bool,
    pub style: GlowStyle,
    pub style_raw_names_pet: bool,
}

/// What the resident is told about its session, restated by the host each frame
/// (the brain diffs each fact). The web passes none.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RoomFacts {
    /// The session's status phase is `Quiet`: a foreground job, nothing
    /// printing recently.
    pub quiet: bool,
    /// The caret of a sibling pane streaming this frame, in the pet's pane
    /// cells `(col, row)`; `None` in a single pane and on quiet frames.
    pub sibling: Option<(f32, f32)>,
    /// The session's own facts; `None` when the host no longer holds the session.
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
    /// The serial of the session's task end (§11.2's end: Land's landing)
    /// that no pet has cheered yet; `None` when none stands or it was cheered
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.3). The host's
    /// glass keeps the once; the first pet that sees it takes it
    /// ([`PetTick::landed_seen`]).
    pub landed: Option<u32>,
}

/// One frame's inputs to [`CompanionOwner::prepare`].
#[derive(Clone, Copy, Debug)]
pub struct PetFacts<'a> {
    pub facts: &'a TerminalFacts,
    pub host: &'a HostFrameInput,
    pub glow: GlowOwnership,
    pub sing: SingFacts,
    /// The surface has focus (native: its cursor-effect focus).
    pub focused: bool,
    /// An app surface covers the grid (command palette, Settings, tab menu): the
    /// resident stays owned but is not presented.
    pub obscured: bool,
    /// A composed surface's focused pane: session and pixel origin in the window
    /// grid (see [`prepare_resident_pet_tick`]). `None` for a single grid.
    pub pane: Option<(u64, (i32, i32))>,
    /// The room the pet lives in, `None` for a host that has none.
    pub room: Option<RoomFacts>,
}

/// The half-sensed frame [`CompanionOwner::prepare`] hands back. Split so a host
/// can act between the notes and the tick (the grief gate, the glow's flow entry).
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
    /// The caret the cursor companions escort this frame ([`Self::escort_caret`]).
    escort_caret: Option<(u16, u16)>,
    origin: (i32, i32),
    /// The glass a body drawn this frame stands on.
    glass: PetGlass,
    /// `(exit code, exec duration ms)`, `Some` exactly once per completion, never
    /// on a session switch or a capture.
    pub completion: Option<(i32, Option<u64>)>,
    /// `(session, serial)`: the task end the room named
    /// ([`SessionRoom::landed`]) was on screen in this pane this frame — a
    /// present to a focused, unobscured, live surface — so the host marks it
    /// cheered and no frame, here or in another window, hands it again. The
    /// pet cheered it where it owns the glass; with no pet (another trail
    /// style, serious mode) it is spent unplayed, so turning the pet on later
    /// replays nothing. `None` on a capture and while nobody can see the pane.
    pub landed_seen: Option<(u64, u32)>,
}

impl PetTick {
    /// The caret the cursor companions escort this frame, hidden or not: the
    /// terminal's, except on a frame the program has not finished, where a
    /// hidden cursor is its write position and this is the last finished
    /// present's caret (`None` before one, or off the live viewport). The pet
    /// chases the same reading, so a host that draws the flying head (or any
    /// companion at the caret) from it puts both bodies on one caret; the raw
    /// cursor took the head up the transcript to wherever a torn repaint
    /// stopped writing.
    #[inline]
    #[must_use]
    pub const fn escort_caret(&self) -> Option<(u16, u16)> {
        self.escort_caret
    }
}

#[cfg(test)]
impl PetTick {
    /// The frame's burst verdict; the brain's own copy is not refreshed on a tick
    /// it returns from early.
    #[inline]
    pub(crate) const fn output_burst(&self) -> bool {
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
    /// The one-cat-per-caret yield handed to `WordDecorations::tick`. The flying
    /// head's rect is resolved by the route that draws it.
    pub companion: Option<CompanionOnGlass>,
    /// `PetFrame::fp` — non-zero while anything rides the lane, byte-stable
    /// once nothing does.
    pub fp: u64,
    /// Slot for slot with `pet.departures`: the pane a ghost was drawn in when it
    /// is not this frame's surface, still on glass; `None` otherwise.
    pub glass: [Option<PetGlass>; PET_DEPARTURES_MAX],
}

/// One terminal surface's place on glass: its session, the window-grid pixel
/// origin of its cell `(0, 0)` and its size in cells.
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
    /// `(session, completed_command_seq)`: the once-per-completion latch.
    cmd_seq: Option<(u64, u64)>,
    /// `(session, content_seq)`: the perk-and-watch burst latch, so a frame
    /// can tell "the pane wrote" from "the pane repainted".
    content_seq: Option<(u64, u64)>,
    /// `(session, wrap_serial)`: the wrap-fact latch, compared with `!=` (see
    /// [`wrap_fact_edge`]).
    wrap_serial: Option<(u64, u64)>,
    /// `((session, alt_screen), grid, (caret, painted))` as of the last present
    /// the program had finished: what a torn frame's hidden write cursor is
    /// read as. The surface (`(session, alt_screen)`) and grid (`(rows, cols)`)
    /// key it: a caret read on the other screen buffer or before a resize is a
    /// cell of another surface.
    finished_caret: Option<FinishedCaret>,
    /// `((session, alt_screen), Terminal::sync_end_seq)` as the last present
    /// read them: a frame whose count has moved on was closed in the stream.
    /// Only a current caret equal to that close's recorded cell is taken
    /// immediately (`HIDDEN_REST` is for later unbracketed moves).
    brackets: Option<((u64, bool), u64)>,
    /// A hidden cursor's move, not yet taken ([`HIDDEN_REST`]).
    hidden_rest: Option<HiddenRest>,
}

/// [`Latches::finished_caret`]: the caret reading of the last finished present
/// on one surface's grid.
#[derive(Clone, Copy, Debug)]
struct FinishedCaret {
    surface: (u64, bool),
    grid: (u16, u16),
    reading: CaretReading,
}

/// A program that does not bracket its frames moves its hidden cursor through
/// every repaint (the spinner's row, the status line) and back to the caret.
/// A present between can catch it anywhere, and the companions took that cell
/// as the caret: the flying head went seventeen rows up the transcript and
/// back, one present each way (2026-10-20). A hidden cursor's move that no
/// bracket closed is taken once the cursor has rested at the cell this long;
/// a repaint's write position never does. A move a bracket closed is the
/// program's finished frame and is taken at once: latched only by a torn
/// frame the companions caught, the program's bracketing went unseen in
/// Claude Code, whose presents all land between brackets, and every caret
/// move it made reached the cat two presents late. At a wrap the stale caret
/// sent it hopping down the line it had left before it bounded back to the
/// new one.
const HIDDEN_REST: std::time::Duration = std::time::Duration::from_millis(30);

/// [`Latches::hidden_rest`]: the caret taken, and the cell the hidden cursor
/// has stood at since when, on one surface's grid.
#[derive(Clone, Copy, Debug)]
struct HiddenRest {
    surface: (u64, bool),
    grid: (u16, u16),
    taken: Option<(u16, u16)>,
    seen: (Option<(u16, u16)>, Instant),
}

/// A caret as the pet senses it: the cell, and whether it is painted.
type CaretReading = (Option<(u16, u16)>, bool);

/// The pet driver. Owns the brain, the identity verdict and the frame latches.
///
/// Per frame, in order: [`Self::observe_console`], [`Self::prepare`], the host's
/// work between notes and tick, [`Self::tick`] (before the word engine's tick, so
/// the yield box is this frame's body), then [`Self::emit`] after it.
pub struct CompanionOwner {
    pet: PetBrain,
    species: PetSpecies,
    /// The host's opt-in; `false` at construction is the byte-identical-off posture.
    enabled: bool,
    /// The `(coat, iris)` the pet is dressed from.
    pair: (u8, u8),
    arrival: PetArrival,
    rung: CompanionRung,
    latches: Latches,
    pointer_px: Option<(f32, f32)>,
    /// This frame's drawn body in frame px, cleared whenever the pet is not drawn
    /// so a stale rect can never eat a click.
    hit_rect: Option<(i32, i32, i32, i32)>,
    /// The alpha the last tick put on glass (`0` = nothing).
    last_alpha: u8,
    /// The last tick's base presentation gate: the word-cat bat reaches only a
    /// pet that could be seen.
    last_visible: bool,
    /// The resolved posture of the last frame. Static presentation does not
    /// inherit the brain's full-motion settle clocks or animation offers.
    last_static: bool,
    /// The last tick under the load-shed latch: no offer lands sooner than
    /// [`RESIDENT_SHED_CADENCE`] after it.
    last_shed_tick: Option<Instant>,
    /// The grid the last `prepare` resolved against — what `emit` bakes with.
    geom: EffectGeom,
    /// The pair and mapped arrival the last `emit` synced, for
    /// [`Self::commit_hello_due`].
    last_sync: Option<((u8, u8), PetArrival)>,
    /// The pixel frame (pane origin and cell metrics) the last tick drew the
    /// resident in; `None` when it put nothing on glass.
    drawn_frame: Option<PixelFrame>,
    /// The pixel frame a pending re-sighting ghost was drawn in. The ghost stands
    /// for pixels the user saw, so the next tick spends it only where those pixels
    /// still are: the same frame, or its own glass beside the new surface.
    resight_frame: Option<PixelFrame>,
    /// The pixel frame of each pane a ghost still fades on beside the focused
    /// surface. Every tick re-asks the host whether each is still on glass as
    /// drawn ([`Self::offer_glass`]); each is kept, dropped with its ghosts, or
    /// home when the focus returns (`PetBrain::come_home`).
    own_glass: [Option<OwnGlass>; OWN_GLASS_MAX],
    /// The last own-glass key handed out (`OwnGlass::key`); never `0`.
    glass_key: u32,
    /// This frame's host vouched that the pending ghost's glass is on glass as
    /// drawn. Consumed by every tick, so a host that offers nothing spends no
    /// ghost off its own surface.
    resight_offered: bool,
    /// `(session, serial)`: the task end this pet last cheered
    /// ([`SessionRoom::landed`]), so a serial handed again (the host's mark
    /// not yet read back) latches nothing. Compared for equality only and
    /// never re-baselined at a switch, unlike [`Latches`]: a switch onto the
    /// landed tab IS the sighting (§11.3.3, item 3).
    landed_taken: Option<(u64, u32)>,
}

/// At most this many own glasses at once ([`CompanionOwner::own_glass`]):
/// one per ghost the lane can hold, plus the one a boundary displaces.
const OWN_GLASS_MAX: usize = PET_DEPARTURES_MAX + 1;

/// A ghost's own glass: the key its ghosts carry (`PetDepartureSprite::glass`)
/// and the pixel frame they were drawn in.
#[derive(Clone, Copy, Debug)]
struct OwnGlass {
    key: u32,
    frame: PixelFrame,
    /// This frame's host offered it, exactly as drawn; consumed by every tick.
    offered: bool,
}

/// Where a resident's cells land: grid origin in frame px, cell metrics, and the
/// surface's place on glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PixelFrame {
    origin: (i32, i32),
    cell: (u16, u16),
    glass: PetGlass,
}

impl Default for CompanionOwner {
    #[inline]
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
            own_glass: [None; OWN_GLASS_MAX],
            glass_key: 0,
            resight_offered: false,
            landed_taken: None,
        }
    }
}

impl CompanionOwner {
    /// An owner the host dresses every frame through [`Self::set_look`] rather
    /// than the seed. Enabled from birth; ownership is resolved per frame.
    #[must_use]
    #[inline]
    pub fn dressed_by_host() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// Dress the pet in `KittyLook::for_launch(seed)` (rung `Launch`, quiet);
    /// `enabled = false` retires it outright, so off costs and draws nothing.
    #[inline]
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

    /// Dress the pet from the native verdict ([`crate::companion_look`]); only a
    /// `Program` rung may carry a ceremony.
    #[inline]
    pub const fn set_look(&mut self, pair: (u8, u8), arrival: PetArrival, rung: CompanionRung) {
        self.pair = pair;
        self.arrival = arrival;
        self.rung = rung;
    }

    /// Which animal the pet is drawn as: kept for every `prepare` to re-apply (so
    /// a drain's fresh brain wears it) and written to the brain now. Switching
    /// swaps the sprite without resetting the companion.
    #[inline]
    pub fn set_species(&mut self, s: PetSpecies) {
        self.species = s;
        self.pet.set_species(s);
    }

    /// The pointer in frame px between frames: `true` iff the stored value changed,
    /// so an idle hover costs no render. A non-finite sample is dropped; a sample
    /// fed with the frame ([`HostFrameInput::pointer_px`]) wins.
    #[inline]
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
    /// (`exclusions`, all protected). Advances no animation or input seq;
    /// returns the cells classified.
    #[inline]
    pub fn observe_console(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &crate::pet_world::PetWorldFacts,
        pane: crate::pet_world::PetPane,
        exclusions: &[crate::pet_world::PetRect],
    ) -> usize {
        self.observe_console_with_overlays(input, facts, pane, exclusions, &[])
    }

    /// [`Self::observe_console`], with the host's chrome spliced over the
    /// grid (`exclusions`: the footer's rules and chips, the harness card's
    /// glass) apart from its transient overlays (`overlays`: the find bar, the
    /// Autopilot menu). Both protect their cells; an overlay, like a live
    /// selection, never moves where the pet rests
    /// ([`crate::kitty_pet::PetBrain::observe_console_with_overlays`]).
    #[inline]
    pub fn observe_console_with_overlays(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &crate::pet_world::PetWorldFacts,
        pane: crate::pet_world::PetPane,
        exclusions: &[crate::pet_world::PetRect],
        overlays: &[crate::pet_world::PetRect],
    ) -> usize {
        self.observe_console_reserved(
            input,
            facts,
            pane,
            crate::pet_world::PetReserve {
                protected: exclusions,
                overlays,
                ink: &[],
            },
        )
    }

    /// [`Self::observe_console_with_overlays`], with the person's typed
    /// glyphs (the predicted echo, the IME preedit) reserved as ink
    /// ([`crate::pet_world::PetReserve`]): what the native host observes.
    #[inline]
    pub fn observe_console_reserved(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &crate::pet_world::PetWorldFacts,
        pane: crate::pet_world::PetPane,
        reserve: crate::pet_world::PetReserve<'_>,
    ) -> usize {
        self.pet
            .observe_console_with_reserve(input, facts, pane, reserve);
        self.pet.observed_cells()
    }

    /// The caret of a finished frame, as the companions take it: the cursor
    /// itself, except a hidden one that moved with no bracket closing
    /// (`bracketed` false: no newly closed frame ended at that cell), which
    /// is taken at a new cell only once it has
    /// rested there [`HIDDEN_REST`] ([`Latches::hidden_rest`]); until then
    /// the caret taken before stands. The first reading on a session's grid
    /// is taken at once. A capture reads the state and moves none of it.
    #[inline]
    fn rested_caret(
        &mut self,
        facts: &TerminalFacts,
        grid: (u16, u16),
        now: Instant,
        present: bool,
        bracketed: bool,
    ) -> Option<(u16, u16)> {
        let surface = (facts.session, facts.alt_screen);
        let held = self
            .latches
            .hidden_rest
            .filter(|r| r.surface == surface && r.grid == grid);
        let rest = match held {
            Some(mut rest) if !facts.cursor_visible && !bracketed => {
                if rest.taken == facts.caret || rest.seen.0 != facts.caret {
                    rest.seen = (facts.caret, now);
                } else if now.saturating_duration_since(rest.seen.1) >= HIDDEN_REST {
                    rest.taken = facts.caret;
                }
                rest
            }
            _ => HiddenRest {
                surface,
                grid,
                taken: facts.caret,
                seen: (facts.caret, now),
            },
        };
        if present {
            self.latches.hidden_rest = Some(rest);
        }
        rest.taken
    }

    /// The frame's notes, everything before the tick, in native order:
    ///
    /// * the Hidden edge retires the surface (the pet returns as a fresh
    ///   sighting in the same coat);
    /// * `!owned ⇒ retire_unowned`, motes and all;
    /// * on a present only: the completion edge ([`PetTick::completion`]), the
    ///   room, the Execute level, the output burst, the wrap fact and the pointer;
    /// * `!focused ⇒` the surface retires;
    /// * the pane binding, species and ink.
    ///
    /// A capture reads and spends none of the present-owned diffs.
    #[inline]
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
        // On a frame the program has not finished, a hidden cursor is the write
        // position (a TUI's transcript repaint), not a caret the pet should chase:
        // read the caret of the last finished present instead, as a holding host
        // would have shown. `None` would not do: a hidden caret the console cannot
        // hold fades the pet.
        let write_cursor = facts.frame_unfinished && !facts.cursor_visible;
        let surface = (facts.session, facts.alt_screen);
        let grid = (host.geometry.rows, host.geometry.cols);
        // A close since the last present certifies its own end cell, not a
        // write position reached by later unbracketed output before this present.
        let bracketed = facts
            .sync_end_caret
            .is_some_and(|end| facts.caret == Some(end))
            && self.latches.brackets.is_some_and(|(at_surface, seq)| {
                at_surface == surface && seq != facts.sync_end_seq
            });
        if present {
            self.latches.brackets = Some((surface, facts.sync_end_seq));
        }
        let (caret, caret_painted) = if write_cursor {
            self.latches
                .finished_caret
                .filter(|fc| fc.surface == surface && fc.grid == grid && facts.live_viewport)
                .map_or((None, false), |fc| fc.reading)
        } else {
            let reading = (
                self.rested_caret(facts, grid, now, present, bracketed),
                facts.cursor_visible,
            );
            if present {
                self.latches.finished_caret = Some(FinishedCaret {
                    surface,
                    grid,
                    reading,
                });
            }
            reading
        };

        if host.visibility == Visibility::Hidden {
            self.retire_coordinate_space();
        }
        // Serious mode: the glass belongs to the work.
        let owned = self.enabled
            && !host.serious
            && resident_pet_owner_present(pet_mode, trail_master, glow.style);
        // Focus and visibility decide presentability; load only coarsens cadence.
        // Reading owns a certified content surface, never a fabricated caret.
        let presentable = resident_pet_surface_presentable(
            focused,
            host.visibility != Visibility::Hidden && !obscured,
            facts.live_viewport,
            self.pet.has_reading_interest(),
        );
        let visible = owned && presentable;
        // Retire an unowned pet outright rather than let it fade: a fading brain
        // keeps `needs_frames()` true through `FADE_OUT` and every mote, keeping
        // the surface presenting at 60 fps.
        if !owned {
            self.pet.retire_unowned();
        }
        let mut completion = None;
        let mut landed_seen = None;
        let mut burst = false;
        let mut wrapped = false;
        let mut pointer = None;
        if present {
            // Exit-code empathy, latched by (session, seq): a tab switch
            // re-baselines silently; a None→Some edge within a session is a real
            // first completion. The edge is handed back for the native verdict.
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
            if let Some(room) = room {
                self.pet.note_room_quiet(room.quiet);
                self.pet.note_room_sibling(now, room.sibling);
                match room.session {
                    None => self.pet.note_room_lease(false),
                    Some(session) => {
                        self.pet.note_room_lease(session.lease);
                        self.pet.note_room_turn(now, session.turn.0, session.turn.1);
                        self.pet.note_room_inbox(now, session.inbox);
                        landed_seen =
                            self.take_landed(facts.session, session.landed, presentable, owned);
                    }
                }
            }
            // Levels, forwarded every frame; the pet starts no clock here.
            self.pet.note_executing(now, facts.shell_executing);
            // A full-screen program owns the glass: no stream to watch, no prompt
            // row to wait at.
            self.pet.note_full_screen(facts.alt_screen);
            // The content-clock diff rides a session-keyed latch, so a session
            // change is never a burst.
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
            // A torn frame leaves the wrap serial unread: the wrap belongs to the
            // caret move the next finished frame shows.
            wrapped = !write_cursor
                && wrap_fact_edge(
                    &mut self.latches.wrap_serial,
                    facts.session,
                    facts.wrap_serial,
                );
            // Pixels to fractional cells only; the brain is its own motion sensor.
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
        // Live glass and capture share this setup. Ink is one frame stale by
        // construction (the rescan runs later this frame).
        if !focused {
            self.retire_coordinate_space();
        }
        // A ghost born from this body carries this surface key, so an arriver
        // adopting it resumes its palette only on its own ground.
        self.pet.note_surface((facts.session, facts.alt_screen));
        self.pet.note_repainting(write_cursor);
        self.feed_ink(decos, pane);
        // Only the motion policy pins the resident; the shed changes its cadence.
        let reduced_motion = host.reduced_motion;
        // The caret is withheld only from a pet that cannot be drawn (it then
        // fades and releases the lane); a hidden caret is still chased, with
        // `caret_drawn` carrying the hide.
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
                // Pointer contact needs the last drawn body and pixel custody.
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
            escort_caret: caret,
            origin: host.geometry.origin_px,
            glass: PetGlass {
                session: facts.session,
                origin: pane.map_or((0, 0), |(_, origin)| origin),
                cols: geom.cols,
                rows: geom.rows,
            },
            completion,
            landed_seen,
        }
    }

    /// THE KITTY CHEERS THE LANDING, WHEN YOU ARE THERE TO SEE IT
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.3): the room names
    /// a task end no pet has cheered (`landed`, its serial). While nobody can
    /// see this pane (`presentable` false: the window unfocused or hidden, an
    /// app surface over the grid, the viewport in history) it waits, and every
    /// frame of a background tab never names it at all, so across many tabs
    /// each cheers on its first look. Seen, it is handed back to be marked
    /// cheered; the pet, where it `owns` the glass, latches the big cheer
    /// ([`PetBrain::note_room_landed`]) once per `(session, serial)`.
    #[inline]
    fn take_landed(
        &mut self,
        session: u64,
        landed: Option<u32>,
        presentable: bool,
        owned: bool,
    ) -> Option<(u64, u32)> {
        let key = (session, landed?);
        if !presentable {
            return None;
        }
        if owned && self.landed_taken != Some(key) {
            self.landed_taken = Some(key);
            self.pet.note_room_landed();
        }
        Some(key)
    }

    /// The host's terminal panes on glass this frame, offered before [`Self::tick`]:
    /// records whether each pending or own ghost glass is among them exactly as
    /// drawn. A host that offers nothing never spends a ghost off its own surface.
    #[inline]
    pub fn offer_glass(&mut self, glass: impl IntoIterator<Item = PetGlass>) {
        if self.resight_frame.is_none() && self.own_glass.iter().all(Option::is_none) {
            return;
        }
        let pending = self.resight_frame.map(|f| f.glass);
        self.resight_offered = false;
        for own in self.own_glass.iter_mut().flatten() {
            own.offered = false;
        }
        for g in glass {
            self.resight_offered |= pending == Some(g);
            for own in self.own_glass.iter_mut().flatten() {
                own.offered |= own.frame.glass == g;
            }
        }
    }

    /// The half of [`Self::prepare`] that reads the word engine: bind the composed
    /// pane, apply the species, and hand the brain the per-row ink.
    #[inline]
    pub fn feed_ink(&mut self, decos: &mut WordDecorations, pane: Option<(u64, (i32, i32))>) {
        prepare_resident_pet_tick(decos, &mut self.pet, self.species, pane);
    }

    /// The frame's tick, unconditional: `needs_frames()` reads state only the
    /// tick advances, so ticking inside the draw gate would freeze it. Then custody
    /// (`on_glass = admitted && alpha > 0`) and the hit rect.
    #[inline]
    pub fn tick(&mut self, t: PetTick) -> CompanionFrame {
        let geom = self.geom;
        let frame_px = PixelFrame {
            origin: t.origin,
            cell: (geom.cell_w, geom.cell_h),
            glass: t.glass,
        };
        let offered = std::mem::take(&mut self.resight_offered);
        // Each own glass is judged by its own pane first: focus back on it as
        // drawn brings its ghosts home; still on glass as drawn keeps them fading;
        // otherwise its pixels, and its ghosts, are gone.
        for slot in &mut self.own_glass {
            let Some(own) = slot.as_mut() else {
                continue;
            };
            let on_glass = std::mem::take(&mut own.offered);
            if own.frame == frame_px {
                self.pet.come_home(own.key);
                *slot = None;
            } else if !on_glass
                || own.frame.cell != frame_px.cell
                || own.frame.glass.origin == frame_px.glass.origin
            {
                self.pet.forget_own_glass(own.key);
                *slot = None;
            }
        }
        // A re-sighting ghost is spent only where its pixels still are: the same
        // frame (a screen swap, a tab switch), or its own glass beside the new
        // surface, offered this frame as drawn at the same metrics (a split-pane
        // focus switch: it fades there while the arriver comes up at the new pane).
        // Anywhere else it is dropped and the replacement fades in alone.
        if let Some(drawn) = self.resight_frame.take() {
            if drawn.cell != frame_px.cell {
                self.pet.forget_resight();
            } else if drawn.origin == frame_px.origin {
                // The same glass, a new surface on it.
            } else if offered
                && drawn.glass.origin != frame_px.glass.origin
                && let Some(free) = self.own_glass.iter().position(Option::is_none)
            {
                self.glass_key = self.glass_key.wrapping_add(1).max(1);
                self.pet.displace_resight(self.glass_key);
                self.own_glass[free] = Some(OwnGlass {
                    key: self.glass_key,
                    frame: drawn,
                    offered: false,
                });
            } else {
                self.pet.forget_resight();
            }
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
        // A companion claims a cell only at a painted caret: the resident may keep
        // its body through a DECTCEM hide, where the caret would be a stale claim.
        let duty = cursor_companion_duty(on_glass, kitty_alpha, t.painted_caret);
        let body_px = on_glass
            .then(|| pet_frame.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows))
            .flatten();
        // Stash the drawn body for the press hit test.
        self.hit_rect = pet_hit_rect_win(body_px, t.origin);
        self.last_alpha = if on_glass { pet_frame.alpha } else { 0 };
        self.drawn_frame = on_glass.then_some(frame_px);
        self.last_visible = t.visible;
        // A ghost the brain did not spend (no caret sighted, reduced motion)
        // or that has faded leaves its glass with it.
        for slot in &mut self.own_glass {
            if slot.is_some_and(|own| !self.pet.has_ghost_on_glass(own.key)) {
                *slot = None;
            }
        }
        let glass = pet_frame.departures.map(|d| {
            let key = d.filter(|d| d.own_glass)?.glass;
            self.own_glass
                .iter()
                .flatten()
                .find(|own| own.key == key)
                .map(|own| own.frame.glass)
        });
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

    /// Word-cat bat: forward the word engine's peek landings to the brain as the
    /// landed head's centre in fractional cells. Only a pet visible this frame
    /// hears them; the brain acts on the ground next tick.
    #[inline]
    pub fn note_peeks(
        &mut self,
        now: Instant,
        cues: std::vec::Drain<'_, PeekCue>,
        cell: (u16, u16),
    ) {
        if !self.last_visible {
            return;
        }
        let inv_2cw = 0.5 / f32::from(cell.0.max(1));
        let inv_2ch = 0.5 / f32::from(cell.1.max(1));
        for cue in cues {
            let (x0, x1, y0, y1) = cue.head_px;
            self.pet
                .note_peek(now, (x0 + x1) as f32 * inv_2cw, (y0 + y1) as f32 * inv_2ch);
        }
    }

    /// Dress and draw the pet, committing the ceremony where it renders: dress
    /// through [`pet_arrival_for_sync`] + `sync_look`, then draw the pair actually
    /// worn into `free` with colours frozen per appearance (else sampled under the
    /// body by [`cursor_cat_color_key`] with [`ContrastFallback`]). Draws and syncs
    /// nothing unless the pet is on glass. Returns the fingerprint fold and the
    /// sync outcome for [`Self::commit_hello_due`].
    #[inline]
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

    /// [`Self::emit`] with the live lane drawn into `free` (the focused surface's
    /// channel) and each ghost on its own glass into `window` (`None`: `free` is
    /// the window's), in window-grid pixels at its pane, cropped to it, beneath its
    /// ink. Placed by the focused pane's placement it would land in the wrong pane.
    #[inline]
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
            page: (!aterm_render::theme_is_dark(fallback.bg)).then_some(fallback.bg),
        };
        if let Some(pet_fp) = decos.pet_cursor(frame, free) {
            fp ^= pet_fp.rotate_left(29);
            // Ghosts draw only beside a pet the emitter drew, each in its own pane.
            let out = match window {
                Some(window) => window,
                None => free,
            };
            for (slot, glass) in t.glass.iter().enumerate() {
                let Some(glass) = glass else {
                    continue;
                };
                let own = EffectGeom {
                    cols: glass.cols,
                    rows: glass.rows,
                    ..geom
                };
                // In the palette its body was drawn with in its pane
                // (`PetDepartureSprite::colors`), not the one the arriver
                // samples in this one.
                if let Some(ghost_fp) = decos.pet_own_glass(frame, own, glass.origin, slot, out) {
                    fp ^= ghost_fp.rotate_left(37);
                }
            }
        }
        (fp, outcome)
    }

    /// Whether a hello was spent, for the host's tenure bookkeeping: a drawn
    /// present (not a capture), the last `emit` synced a `Ceremony`, and the pair in
    /// hand differs from the pair on glass. The standing difference, not the
    /// `outcome.parked` edge, since a capture syncing first would consume that edge;
    /// the commit consumes the authorisation, so the next present cannot repeat it.
    #[must_use]
    #[inline]
    pub const fn commit_hello_due(&self, present: bool, outcome: SyncLookOutcome) -> bool {
        present
            && matches!(
                self.last_sync,
                Some((pair, PetArrival::Ceremony))
                    if pair.0 != outcome.worn.0 || pair.1 != outcome.worn.1
            )
    }

    /// The terminal BEL rang in the pane this pet is chasing.
    #[inline]
    pub fn note_bell(&mut self, now: Instant) {
        self.pet.note_bell(now);
    }

    /// The hand found its flow (`CursorGlow::take_flow_entry`), forwarded between
    /// [`Self::prepare`] and [`Self::tick`].
    #[inline]
    pub fn note_flow(&mut self, now: Instant) {
        self.pet.note_flow(now);
    }

    /// Rainbow kitty v2's offer to the resident, after [`Self::tick`]: at most one
    /// star comes back on the paw's landing frame for `CursorGlow::catch_star`.
    #[inline]
    pub fn note_v2_offer(
        &mut self,
        offer: &crate::rainbow_kitty::companion::PetOffer,
    ) -> Option<crate::rainbow_kitty::companion::StarCatch> {
        self.pet.note_v2_offer(offer)
    }

    /// A kitty command was typed at this pet (`sit`, `kitty jump`), as
    /// `crate::typed_tricks::TrickListener` reported. A tentative fire
    /// (`confirmed = false`) is followed by a confirmed one or [`Self::revoke_trick`].
    /// Latch only: later ticks perform it. Dropped with no resident on glass.
    #[inline]
    pub fn note_trick(&mut self, now: Instant, trick: aterm_lexicon::Trick, confirmed: bool) {
        self.pet.note_trick(now, trick, confirmed);
    }

    /// The typed line turned into prose, or was aborted: take back a tentative
    /// kitty command not yet performed. Idempotent.
    #[inline]
    pub fn revoke_trick(&mut self) {
        self.pet.revoke_trick();
    }

    /// The whole submitted line was pet talk (`sit` + Enter at a shell): the
    /// brain does not grieve the fast `command not found` that follows.
    #[inline]
    pub fn note_trick_submit(&mut self, now: Instant) {
        self.pet.note_trick_submit(now);
    }

    /// One accepted console input intent. This supplies attention provenance,
    /// never evidence that the program changed a particular range of text.
    #[inline]
    pub fn note_console_input(&mut self, now: Instant, kind: PetInputKind) {
        self.pet.note_console_input(now, kind);
    }

    /// Accepted input intentions observed by this brain; reading does not tick.
    #[must_use]
    #[inline]
    pub fn console_input_seq(&self) -> u64 {
        self.pet.console_input_seq()
    }

    /// Most recent classified input intention; no display-edit claim is implied.
    #[must_use]
    #[inline]
    pub fn console_input_kind(&self) -> Option<PetInputKind> {
        self.pet.console_input_kind()
    }

    /// A left press at `px` (frame px) inside this frame's drawn body (padded by
    /// [`PET_HIT_SLOP_PX`]) strokes the cat and is consumed, so it never starts a
    /// selection or reaches a mouse-tracking app. Latch only: the host should ask
    /// for a frame.
    #[inline]
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

    #[inline]
    const fn clear_surface_state(&mut self) {
        self.hit_rect = None;
        self.drawn_frame = None;
        self.resight_frame = None;
        self.own_glass = [None; OWN_GLASS_MAX];
        self.resight_offered = false;
        self.last_alpha = 0;
        self.last_visible = false;
        self.last_sync = None;
    }

    /// Retire the resident's surface-relative state (the brain's coordinates and
    /// the hit rect) at a presentability boundary. Session-keyed latches and
    /// durable identity (species, worn look, contentment) survive. Idempotent.
    #[inline]
    pub fn retire_coordinate_space(&mut self) {
        self.pet.retire_coordinate_space();
        self.clear_surface_state();
    }

    /// The same surface, re-gridded (a resize, a font step, a DPI change, a divider
    /// drag): the body keeps its place, alpha and pose; only the facts measured
    /// against the old grid and the hit rect go. Idempotent.
    #[inline]
    pub fn rebase_geometry(&mut self) {
        self.pet.rebase_geometry();
        self.hit_rect = None;
        self.latches.finished_caret = None;
        self.latches.hidden_rest = None;
    }

    /// The surface was replaced under a body that may be on glass (another
    /// terminal, or the other screen): everything [`Self::retire_coordinate_space`]
    /// retires, but a body the last tick drew is kept for the next tick to
    /// crossfade from, in the pixel frame it was drawn in. Idempotent: a second
    /// boundary before the next tick keeps the first one's ghost.
    #[inline]
    pub fn resight_coordinate_space(&mut self) {
        if let Some(drawn) = self.drawn_frame.take() {
            self.resight_frame = Some(drawn);
        }
        if self.resight_frame.is_some() {
            self.pet.resight_coordinate_space();
        } else {
            self.pet.retire_coordinate_space();
        }
        // A ghost on its own glass keeps its glass; the next tick judges it.
        self.resight_offered = false;
        for own in self.own_glass.iter_mut().flatten() {
            own.offered = false;
        }
        self.hit_rect = None;
        self.last_alpha = 0;
        self.last_visible = false;
        self.last_sync = None;
    }

    /// The host withdrew this frame's drawn body (a torn projection, a route that
    /// draws no pet), so the petting rect goes with it.
    #[inline]
    pub const fn withdraw_hit_rect(&mut self) {
        self.hit_rect = None;
    }

    /// The terminal under this surface was replaced: everything
    /// [`Self::retire_coordinate_space`] retires, and the stream latches
    /// re-baseline so the old stream's completion, wrap, and caret state are
    /// not replayed. Idempotent.
    #[inline]
    pub fn retire_owner(&mut self) {
        self.retire_coordinate_space();
        self.latches = Latches::default();
    }

    /// [`Self::retire_owner`] with the re-sighting crossfade: what the hosts'
    /// owner fences run. A visible resident hands over to its replacement in a
    /// quarter-second crossfade instead of vanishing.
    #[inline]
    pub fn resight_owner(&mut self) {
        self.resight_coordinate_space();
        self.latches = Latches::default();
    }

    /// Drop everything the brain owns (body, motes, departures, cadence debt)
    /// because its owner went away. Identity survives for the next appearance.
    #[inline]
    pub fn retire(&mut self) {
        self.pet.retire_unowned();
        self.clear_surface_state();
        self.last_static = false;
        self.last_shed_tick = None;
    }

    /// Serious Mode's drain: a fresh brain. The session latches, the opt-in and the
    /// verdict in hand survive. Idempotent.
    #[inline]
    pub fn drain(&mut self) {
        self.pet = self.pet.reborn();
        self.clear_surface_state();
        self.last_static = false;
        self.last_shed_tick = None;
    }

    /// Retire the pet's frozen contrast samples, its ghosts' included (a theme or
    /// palette change). Behaviour, position and breed handoff continue.
    #[inline]
    pub fn invalidate_colors(&mut self) {
        self.pet.invalidate_colors();
    }

    /// The cadence gate: [`PetBrain::needs_frames`] (something is moving), never
    /// `is_active()` (a cat exists). A static posture or a tick under the load-shed
    /// latch leaves the frame train to [`Self::next_change_deadline`].
    #[inline]
    #[must_use]
    pub fn needs_frames(&self) -> bool {
        !self.last_static && self.last_shed_tick.is_none() && self.pet.needs_frames()
    }

    /// The instant of a settled resident's next visible step (a breath pixel, a
    /// blink, a tail beat), [`PetBrain::next_change_deadline`]. Under the load-shed
    /// latch every offer is floored at [`RESIDENT_SHED_CADENCE`] after the last
    /// tick. An overdue step means the host woke for it and drew nothing (a
    /// DEC-2026 hold), so it is re-offered a whole cadence out: a past or `now`
    /// offer would be re-armed every turn, a busy loop. `None` when nothing will
    /// change.
    #[inline]
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

    /// The brain's failure droop is on glass or owed ([`PetBrain::grieving`]); the
    /// host hushes the glow's caret-jump fanfare meanwhile, reading this between
    /// [`Self::prepare`] and [`Self::tick`].
    #[inline]
    #[must_use]
    pub fn grieving(&self) -> bool {
        self.pet.grieving()
    }

    /// The host's opt-in as last set.
    #[inline]
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// The alpha the last tick put on glass (`0` = nothing drawn).
    #[inline]
    #[must_use]
    pub const fn alpha(&self) -> u8 {
        self.last_alpha
    }

    /// This frame's drawn body in frame px, `None` when the pet is not drawn.
    #[inline]
    #[must_use]
    pub const fn hit_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.hit_rect
    }

    /// The animal the host chose, which every `prepare` applies and every retire
    /// keeps; the brain's identity is [`Self::brain`].
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn species(&self) -> PetSpecies {
        self.species
    }

    /// Read-only access to the brain for the host's projections; a shared borrow
    /// cannot tick it.
    #[inline]
    #[must_use]
    pub const fn brain(&self) -> &PetBrain {
        &self.pet
    }
}

#[cfg(test)]
mod law_tests {
    //! Pins for the pure laws above.
    use super::*;
    use crate::kitty_cursor::{CatExit, CatFrame, CatPose, CatReaction};
    use aterm_core::terminal::UnderlineStyle;

    /// Ownership is pet mode, the trail master and the pet style, nothing else.
    #[test]
    fn the_owner_is_present_only_with_pet_mode_the_master_and_the_pet_style() {
        for (pet_mode, master, style, owned) in [
            (true, true, GlowStyle::RainbowKitty, true),
            (false, true, GlowStyle::RainbowKitty, false),
            (true, false, GlowStyle::RainbowKitty, false),
            (true, true, GlowStyle::Lumen, false),
        ] {
            assert_eq!(
                resident_pet_owner_present(pet_mode, master, style),
                owned,
                "pet_mode={pet_mode} master={master} {style:?}"
            );
        }
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

        // The head is off the caret, so the rect must be the sprite's own.
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

        // Unreachable through custody (pet mode never admits the head), but the
        // shape still guarantees one body, and the pet wins: the yield box
        // describes the pet, never the head.
        let duty = cursor_companion_duty(true, 255, Some((4, 9)));
        assert_eq!(duty, CompanionDuty::Pet);
        let on_glass = cursor_companion_on_glass(duty, Some((4, 9)), Some(head), Some(body))
            .expect("the pet owns the frame");
        assert_eq!(on_glass.body_px, Some(body));
        assert!(!on_glass.guards_caret);
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
        let key = |neighbor, top_bg, bottom_bg| {
            let cells = make(neighbor, top_bg, bottom_bg);
            cursor_cat_color_key(&cells, geom, footprint, 0, 0x00FF_FFFF, 0)
        };
        let red = key([255, 20, 20], [8, 8, 8], [8, 8, 8]);
        let blue = key([20, 80, 255], [8, 8, 8], [8, 8, 8]);
        let light = key([255, 20, 20], [248, 248, 248], [248, 248, 248]);
        let mixed = key([255, 20, 20], [4, 4, 4], [248, 248, 248]);
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

    /// First sight and session switches baseline silently; a same-session change
    /// is exactly one wrap, backward too (`!=`, not `>`).
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
        // A move backward (main/alt swap kept per-grid serials) still reads.
        assert!(wrap_fact_edge(&mut seen, 8, u64::MAX));
        assert!(wrap_fact_edge(&mut seen, 8, 3));
    }

    /// The hit-rect geometry inverted; anything off the grid is `None`.
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

    /// Content movement alone is never a burst: the shell must be executing, the
    /// viewport live, and the pane on the main screen.
    #[test]
    fn typing_echo_never_reads_as_a_burst() {
        // Echo: the content clock moves, the shell is not executing.
        assert!(!pet_output_burst(false, true, false, true, false));
        // A real stream: rows scrolled while the shell runs, live bottom.
        assert!(pet_output_burst(true, false, true, true, false));
        assert!(pet_output_burst(false, true, true, true, false));
        // Scrolled-back history is not a stream the pet can see.
        assert!(!pet_output_burst(true, true, true, false, false));
        // An executing shell that wrote nothing this frame is quiet.
        assert!(!pet_output_burst(false, false, true, true, false));
        // A full-screen program's repaint is not output, whatever moved.
        assert!(!pet_output_burst(false, true, true, true, true));
        assert!(!pet_output_burst(true, true, true, true, true));
    }

    /// Body px + effects origin, as the emitter places it; `None` stays `None`.
    #[test]
    fn pet_hit_rect_win_offsets_the_body_by_the_effects_origin() {
        assert_eq!(pet_hit_rect_win(None, (7, 9)), None);
        assert_eq!(
            pet_hit_rect_win(Some((10, 30, 40, 60)), (7, 9)),
            Some((17, 37, 49, 69))
        );
        // A row-0 pet's head rises above the grid top: the rect keeps the negative
        // overhang (overlays win the click by order, not by clamping).
        assert_eq!(
            pet_hit_rect_win(Some((0, 20, -12, 8)), (4, 30)),
            Some((4, 24, 18, 38))
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

    /// Non-program rungs are quiet whatever the gate ruled, a program rung carries
    /// the gate's ruling, an undressed pet keeps its ceremony, and a near-identical
    /// coat is demoted.
    #[test]
    fn the_arrival_mapping_rations_the_theater_by_rung_and_difference() {
        use crate::kitty_registry::{SUFFICIENT_DIFFERENCE, coat_distance};
        use CompanionRung::{Favourite, Launch, Program};
        use PetArrival::{Ceremony, Quiet};
        let far = (0u8..16)
            .find(|&c| coat_distance(3, c) >= SUFFICIENT_DIFFERENCE)
            .expect("some coat is sufficiently different from coat 3");
        for (rung, authorised, worn, incoming, expected) in [
            (Favourite, Ceremony, None, 3, Quiet),
            (Launch, Ceremony, None, 3, Quiet),
            (Program, Quiet, None, 3, Quiet),
            (Program, Ceremony, None, 3, Ceremony),
            (Program, Ceremony, Some((3, 0)), 3, Quiet),
            (Program, Ceremony, Some((3, 0)), far, Ceremony),
        ] {
            assert_eq!(
                pet_arrival_for_sync(rung, authorised, worn, incoming),
                expected,
                "{rung:?} {authorised:?} worn={worn:?} incoming={incoming}"
            );
        }
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

    /// Every fallback black, for tests about the sync outcome, not the palette.
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
            sync_end_seq: 0,
            sync_end_caret: None,
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

    /// One owned, focused, unobscured pet-mode frame's facts; a test shapes
    /// only the field it changes (`PetFacts { focused: false, ..pet_facts(..) }`).
    fn pet_facts<'a>(facts: &'a TerminalFacts, host: &'a HostFrameInput) -> PetFacts<'a> {
        PetFacts {
            facts,
            host,
            glow: pet_glow(),
            sing: SingFacts::default(),
            focused: true,
            obscured: false,
            pane: None,
            room: None,
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
        owner.sense(pet_facts(&facts, &host), decos)
    }

    /// Walk the caret until the resident is on glass and moving; returns when.
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

    /// Hold the caret still until the resident releases the lane; returns when.
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

    /// An unfocused surface feeds `caret: None` and the resident releases the
    /// lane. Negative control: an unticked brain's `needs_frames` stays true.
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
                    focused: false,
                    ..pet_facts(&facts, &host)
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

    /// A press inside the drawn body (plus slop) latches a pet and is consumed;
    /// outside passes. Contact needs the body the last frame drew (its hit rect),
    /// so a withdrawn body cannot be touched, by a press or the pointer, while the
    /// brain keeps its frame.
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

        owner.withdraw_hit_rect();
        assert_eq!(owner.hit_rect(), None);
        assert!(owner.brain().is_active(), "the brain keeps its frame");
        assert_eq!(
            owner.press(now, inside),
            PressOutcome::Pass,
            "no body, no pet"
        );
        let mut host = host_at(now + Duration::from_millis(16));
        host.pointer_px = Some(inside);
        let facts = facts_with(Some((4, 12)));
        let t = prepared(&mut owner, &mut decos, &facts, &host, false);
        assert_eq!(t.sense.pointer, None, "a withdrawn body is untouchable");
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
            // Stay inside the seated window, clear of the follower's final glide.
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
                        sing: SingFacts {
                            drive: sing_drive,
                            ..SingFacts::default()
                        },
                        focused,
                        ..pet_facts(&facts, &host)
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

    /// The pointer is value-shadowed; a non-finite sample changes nothing.
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

    /// Wandering over the grid builds attention and re-arms the lane; off the grid
    /// the pointer does not exist for the pet.
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

        // Gentle circling on the pet's row: attention builds and the lane re-arms.
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

    /// A windowless still is at full opacity on its first frame, where a live
    /// present's first tick starts a fade-in.
    #[test]
    fn a_static_capture_materialises_the_first_still() {
        let mut decos = WordDecorations::default();
        let now = Instant::now();
        let facts = facts_with(Some((2, 2)));

        let mut capture = an_enabled_owner();
        let mut host = host_at(now);
        host.capture = CaptureMode::StaticCapture;
        let still = capture.sense(pet_facts(&facts, &host), &mut decos);
        assert_eq!(still.pet.alpha, 255, "the still shows the resident");
        assert!(still.on_glass && still.body_px.is_some());

        let mut present = an_enabled_owner();
        let host = host_at(now);
        let first = present.sense(pet_facts(&facts, &host), &mut decos);
        assert_eq!(first.pet.alpha, 0, "negative control: a present fades in");
        assert!(present.needs_frames(), "…and claims the lane to do so");
    }

    /// The emitter draws one body in the brain's latched pair with a non-zero
    /// fingerprint; a favourite parks a differing pair, and never commits a hello.
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

        // A favourite verdict with a different pair parks on the next emission.
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

    /// A program ceremony for a different enough coat commits on a drawn present,
    /// never on a capture nor an emit that put nothing on glass, and a re-sync
    /// under Quiet cannot commit again.
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
        let off = CompanionFrame {
            on_glass: false,
            duty: CompanionDuty::Idle,
            body_px: None,
            companion: None,
            ..t
        };
        let (fp, off_outcome) = owner.emit(&off, &[], theme, &mut decos, &mut Vec::new());
        assert_eq!(fp, 0, "nothing drawn");
        assert!(
            !owner.commit_hello_due(true, off_outcome),
            "an off-glass frame performed nothing, so it commits nothing"
        );

        // Committed, the host latch is Quiet: the re-sync cannot commit again.
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

    /// One owned, focused frame's facts and host at `now`.
    fn prepared(
        owner: &mut CompanionOwner,
        decos: &mut WordDecorations,
        facts: &TerminalFacts,
        host: &HostFrameInput,
        obscured: bool,
    ) -> PetTick {
        owner.prepare(
            PetFacts {
                obscured,
                ..pet_facts(facts, host)
            },
            decos,
        )
    }

    /// On an unfinished frame a hidden caret is the write position: the sense
    /// carries the last finished present's caret and paint, and the wrap waits.
    /// A finished frame's DECTCEM-hidden caret is still chased, flagged unpainted,
    /// rather than lost for every TUI repaint, and the yield box is the body alone;
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
            "a finished frame's hidden caret still travels, unpainted"
        );
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

        let mut other = torn;
        other.session = 2;
        let t = prepared(&mut owner, &mut decos, &other, &at(80), false);
        assert_eq!(t.sense.caret, None, "no finished caret in this session");
    }

    /// The escort caret is the pet's caret reading for whichever companion the
    /// glass has, the flying head (no pet owned, so the sense carries none)
    /// included: a torn frame's hidden cursor reads as the last finished
    /// present's caret, a finished frame's hidden caret is itself, and a torn
    /// frame with no finished caret of its session reads none.
    #[test]
    fn the_escort_caret_is_the_finished_caret_for_every_companion() {
        for pet_mode in [true, false] {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let now = Instant::now();
            let glow = GlowOwnership {
                style_raw_names_pet: pet_mode,
                ..pet_glow()
            };
            let mut prep = |facts: &TerminalFacts, ms: u64| {
                let host = host_at(now + Duration::from_millis(ms));
                let t = owner.prepare(
                    PetFacts {
                        glow,
                        ..pet_facts(facts, &host)
                    },
                    &mut decos,
                );
                let escort = t.escort_caret();
                let sensed = t.sense.caret;
                let _ = owner.tick(t);
                (escort, sensed)
            };
            let finished = facts_with(Some((20, 7)));
            assert_eq!(prep(&finished, 16).0, Some((20, 7)), "pet {pet_mode}");
            let mut torn = facts_with(Some((2, 0)));
            torn.cursor_visible = false;
            torn.frame_unfinished = true;
            let (escort, sensed) = prep(&torn, 32);
            assert_eq!(escort, Some((20, 7)), "pet {pet_mode}: no write cursor");
            assert_eq!(sensed.is_some(), pet_mode, "only an owned pet senses it");
            // The bracket closes: the frame it finished shows its caret.
            let mut hidden = facts_with(Some((5, 9)));
            hidden.cursor_visible = false;
            hidden.sync_end_seq = 1;
            hidden.sync_end_caret = hidden.caret;
            assert_eq!(prep(&hidden, 48).0, Some((5, 9)), "pet {pet_mode}");
            let mut other = torn;
            other.session = 2;
            assert_eq!(prep(&other, 64).0, None, "pet {pet_mode}");
        }
    }

    /// A program that never brackets its frames moves its hidden cursor through
    /// a repaint and back. A present that caught it at the write position, or
    /// two 16 ms apart, does not move the escort caret; a hidden cursor that
    /// rests at its new cell [`HIDDEN_REST`] is taken (typing on), and a move
    /// a bracket closed (the terminal's DEC 2026 count moved on since the
    /// last present) is a finished frame's and is taken at once, the next
    /// unbracketed move waiting its rest again. Negative control: a visible
    /// cursor is taken at once wherever it goes.
    #[test]
    fn an_unbracketed_repaint_s_write_position_is_no_caret() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = Instant::now();
        let mut prep = |facts: &TerminalFacts, ms: u64| {
            let t = prepared(
                &mut owner,
                &mut decos,
                facts,
                &host_at(now + Duration::from_millis(ms)),
                false,
            );
            let reading = (t.escort_caret(), t.sense.caret);
            let _ = owner.tick(t);
            reading
        };
        let hidden = |caret| {
            let mut f = facts_with(Some(caret));
            f.cursor_visible = false;
            f
        };
        assert_eq!(prep(&hidden((20, 10)), 16).0, Some((20, 10)), "first sight");
        // The spinner's repaint caught mid-write, on one present and on two.
        assert_eq!(prep(&hidden((3, 60)), 32), (Some((20, 10)), Some((20, 10))));
        assert_eq!(prep(&hidden((20, 10)), 48).0, Some((20, 10)));
        assert_eq!(prep(&hidden((3, 60)), 64).0, Some((20, 10)));
        assert_eq!(
            prep(&hidden((3, 60)), 80).0,
            Some((20, 10)),
            "16 ms is no rest"
        );
        assert_eq!(prep(&hidden((20, 10)), 96).0, Some((20, 10)));
        // A key's echo: the hidden cursor steps on and stays.
        assert_eq!(prep(&hidden((20, 11)), 112).0, Some((20, 10)));
        assert_eq!(prep(&hidden((20, 11)), 128).0, Some((20, 10)));
        assert_eq!(
            prep(&hidden((20, 11)), 144).0,
            Some((20, 11)),
            "rested 32 ms"
        );
        // A visible cursor is the caret wherever it goes.
        let mut shown = facts_with(Some((3, 60)));
        shown.cursor_visible = true;
        assert_eq!(prep(&shown, 160).0, Some((3, 60)), "negative control");
        // A torn frame reads the last finished caret.
        let mut torn = hidden((3, 0));
        torn.frame_unfinished = true;
        assert_eq!(
            prep(&torn, 176).0,
            Some((3, 60)),
            "a torn frame reads the last finished"
        );
        // A frame its program closed: the hidden cursor it finished at is
        // taken on the present that shows it.
        let mut closed = hidden((9, 9));
        closed.sync_end_seq = 1;
        closed.sync_end_caret = closed.caret;
        assert_eq!(
            prep(&closed, 192).0,
            Some((9, 9)),
            "a finished frame's caret"
        );
        let mut again = hidden((9, 10));
        again.sync_end_seq = 2;
        again.sync_end_caret = again.caret;
        assert_eq!(prep(&again, 208).0, Some((9, 10)), "and the next");
        // A move no bracket closed waits its rest, whatever came before.
        let mut unclosed = hidden((3, 60));
        unclosed.sync_end_seq = 2;
        assert_eq!(
            prep(&unclosed, 224).0,
            Some((9, 10)),
            "an unbracketed write position after bracketed frames"
        );

        // Real parser provenance, including both actions in ONE process
        // batch before ONE present: a close ends at its own cursor cell,
        // not the later unbracketed write position.
        let mut terminal = aterm_core::terminal::Terminal::new(24, 80);
        let mut owner = an_enabled_owner();
        let mut decos = WordDecorations::default();
        let clock = |ms| aterm_core::terminal::ClockReading {
            monotonic: now + Duration::from_millis(ms),
            wall_ms: None,
        };
        let mut take = |term: &aterm_core::terminal::Terminal, ms| {
            let facts = TerminalFacts::read(term, 1, false);
            let tick = prepared(
                &mut owner,
                &mut decos,
                &facts,
                &host_at(now + Duration::from_millis(ms)),
                false,
            );
            let caret = tick.escort_caret();
            let _ = owner.tick(tick);
            caret
        };
        terminal.process_at(b"\x1b[?25l\x1b[20;11H", clock(0));
        assert_eq!(take(&terminal, 0), Some((19, 10)), "first sight");
        terminal.process_at(b"\x1b[?2026h\x1b[20;12H\x1b[?2026l", clock(16));
        assert_eq!(terminal.sync_end_caret(), Some((19, 11)));
        assert_eq!(
            take(&terminal, 16),
            Some((19, 11)),
            "closed move: immediate"
        );

        terminal.process_at(
            b"\x1b[?2026h\x1b[20;13H\x1b[?2026l\x1b[3;1Hwrite",
            clock(32),
        );
        assert_eq!(terminal.sync_end_seq(), 2, "a new close really occurred");
        assert!(
            !terminal.sync_frame_unfinished(),
            "later write is unbracketed"
        );
        assert_eq!(terminal.sync_end_caret(), Some((19, 12)));
        assert_eq!((terminal.cursor().row, terminal.cursor().col), (2, 5));
        assert_eq!(
            take(&terminal, 32),
            Some((19, 11)),
            "the close did not end here"
        );
        assert_eq!(take(&terminal, 48), Some((19, 11)), "16 ms is no rest");
        assert_eq!(
            take(&terminal, 64),
            Some((2, 5)),
            "a genuine rest still arrives"
        );

        terminal.process_at(b"\x1b[?2026h\x1b[20;14H\x1b[?2026l\x1b[?2026h", clock(80));
        assert!(!terminal.sync_frame_unfinished(), "empty reopened bracket");
        assert_eq!(
            take(&terminal, 80),
            Some((19, 13)),
            "closed move still immediate"
        );
        terminal.process_at(b"\x1b[3;1Hpartial", clock(96));
        assert_eq!(take(&terminal, 96), Some((19, 13)), "torn frame stays held");
        terminal.process_at(b"", clock(2100));
        assert!(!terminal.modes().synchronized_output(), "timeout control");
        assert!(
            terminal.sync_frame_unfinished(),
            "timeout is no program close"
        );
        assert_eq!(terminal.sync_end_caret(), None, "latest count is a timeout");
        assert_eq!(take(&terminal, 2100), Some((19, 13)));
        terminal.process_at(b"\x1b[?2026l", clock(2116));
        assert_eq!(
            take(&terminal, 2116),
            Some((2, 7)),
            "real close after timeout"
        );

        terminal.process_at(b"\x1b[!p", clock(2132));
        assert_eq!(terminal.sync_end_caret(), None, "soft reset invalidates");
        terminal.process_at(b"\x1b[?2026h\x1b[?2026l\x1bc", clock(2148));
        assert_eq!(terminal.sync_end_caret(), None, "full reset invalidates");
        terminal.process_at(b"\x1b[?2026h\x1b[?2026l\x1b[?1049h", clock(2164));
        assert_eq!(terminal.sync_end_caret(), None, "screen switch invalidates");
        terminal.process_at(b"\x1b[?1049l\x1b[?2026h\x1b[?2026l", clock(2180));
        assert!(
            terminal.sync_end_caret().is_some(),
            "fresh main-screen close"
        );
        terminal.resize(24, 81);
        terminal.resize(24, 80);
        assert_eq!(
            terminal.sync_end_caret(),
            None,
            "regrid back cannot revive it"
        );
        terminal.process_at(b"\x1b[?2026h\x1b[?2026l", clock(2196));
        assert!(terminal.sync_end_caret().is_some());
        let checkpoint = terminal.checkpoint();
        terminal.restore_checkpoint(&checkpoint);
        assert_eq!(
            terminal.sync_end_caret(),
            None,
            "in-place restore invalidates"
        );
        let restored = aterm_core::terminal::Terminal::from_checkpoint(&checkpoint);
        assert_eq!(
            restored.sync_end_caret(),
            None,
            "fresh restore starts unknown"
        );
        terminal.process_at(b"\x1b[?2026h\x1b[?2026l", clock(2212));
        assert!(terminal.sync_end_caret().is_some());
        let _ = terminal.cell_frame(24, 80);
        assert!(
            terminal.sync_end_caret().is_some(),
            "cell_frame snapshot preserves close cell for TerminalFacts::read"
        );
        let _ = terminal.grid_mut();
        assert_eq!(terminal.sync_end_caret(), None, "mutable grid boundary");
    }

    /// The finished caret is a cell of the grid it was read on: a torn frame
    /// after a resize reads none rather than the old grid's cell.
    #[test]
    fn a_torn_frame_after_a_resize_reads_no_old_caret() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let finished = facts_with(Some((20, 7)));
        let t = prepared(&mut owner, &mut decos, &finished, &host_at(now), false);
        let _ = owner.tick(t);
        let mut torn = facts_with(Some((2, 0)));
        torn.cursor_visible = false;
        torn.frame_unfinished = true;
        let mut host = host_at(now + Duration::from_millis(16));
        let t = prepared(&mut owner, &mut decos, &torn, &host, false);
        assert_eq!(
            t.escort_caret(),
            Some((20, 7)),
            "fixture: same grid, the latch"
        );
        let _ = owner.tick(t);
        host.now += Duration::from_millis(16);
        host.geometry.rows += 4;
        let t = prepared(&mut owner, &mut decos, &torn, &host, false);
        assert_eq!(
            t.escort_caret(),
            None,
            "a caret read before the resize is a cell of another grid"
        );
    }

    /// A capture reads and spends none of the present-owned diffs (completion,
    /// burst, wrap) and sees no pointer, so the next present finds every edge.
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
            // A capture of a torn repaint must not teach the owner that this
            // session brackets its live frames.
            facts.frame_unfinished = true;
            facts.cursor_visible = false;
            facts.caret = Some((2, 0));
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

            // …so the next present reads every edge the capture left.
            host.capture = CaptureMode::Present;
            host.now += Duration::from_millis(16);
            facts.frame_unfinished = false;
            facts.caret = Some((4, 13));
            let t = prepared(&mut owner, &mut decos, &facts, &host, false);
            assert_eq!(
                t.escort_caret(),
                Some((4, 12)),
                "{capture:?}: the new hidden caret still waits for a live rest"
            );
            assert_eq!(t.completion, Some((1, Some(40))), "{capture:?}");
            assert!(t.sense.output_burst && t.sense.wrapped, "{capture:?}");
            assert!(owner.grieving(), "{capture:?}: the failure is felt now");
            let _ = owner.tick(t);
        }
    }

    /// The completion edge comes back once per completion in the pet's session,
    /// never on first sighting or a session switch.
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
        assert_eq!(step(&mut owner, 9, None), None, "a session with no history");
        assert_eq!(
            step(&mut owner, 9, Some((1, 1, None))),
            Some((1, None)),
            "its None -> Some edge is a real first completion"
        );
    }

    /// One present in session 1 whose room names the task end `landed`
    /// (`SessionRoom::landed`), with the frame's fields shaped by `shape`; the
    /// end handed back, and the frame drawn.
    fn landed_frame(
        owner: &mut CompanionOwner,
        decos: &mut WordDecorations,
        now: Instant,
        landed: Option<u32>,
        shape: impl FnOnce(PetFacts<'_>) -> PetFacts<'_>,
    ) -> (Option<(u64, u32)>, CompanionFrame) {
        let facts = facts_with(Some((4, 12)));
        let host = host_at(now);
        let room = RoomFacts {
            session: Some(SessionRoom {
                landed,
                ..SessionRoom::default()
            }),
            ..RoomFacts::default()
        };
        let t = owner.prepare(
            shape(PetFacts {
                room: Some(room),
                ..pet_facts(&facts, &host)
            }),
            decos,
        );
        let seen = t.landed_seen;
        (seen, owner.tick(t))
    }

    /// THE KITTY CHEERS THE LANDING, WHEN YOU ARE THERE TO SEE IT
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.3.5): while the
    /// window is unfocused, or an app surface covers the grid, the end waits —
    /// no frame hands it back and the pet latches nothing; the first frame
    /// that shows the pane takes it (handed back for the glass to mark) and
    /// the pet cheers. A serial handed again — the host's mark not yet read
    /// back, frame after frame — does nothing: the one cheer plays (three ♪/♥
    /// at most on glass) and the pet settles, owing nothing. A second end is
    /// news again. NEGATIVE CONTROLS (run by hand, files restored): a pet
    /// that latches at every restatement (`take_landed` without its
    /// `landed_taken` check) keeps owing the cheer and claiming frames; an end
    /// taken whether or not anyone can see the pane (`take_landed` without its
    /// `presentable` gate) is handed back by the unfocused frames.
    #[test]
    fn a_landing_is_cheered_once_by_the_first_frame_that_shows_it() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut now = settle(&mut owner, &mut decos, now);
        let c0 = owner.brain().content();
        for (why, focused, obscured) in [("unfocused", false, false), ("covered", true, true)] {
            for _ in 0..30 {
                now += Duration::from_millis(16);
                let (seen, _) = landed_frame(&mut owner, &mut decos, now, Some(1), |f| PetFacts {
                    focused,
                    obscured,
                    ..f
                });
                assert_eq!(seen, None, "{why}: nobody can see the pane, the end waits");
                assert!(!owner.brain().cheer_owed(), "{why}: no cheer latched");
            }
        }
        assert!(
            owner.brain().content() < c0 + 0.1,
            "no ledger moved while it waited"
        );
        now += Duration::from_millis(16);
        let (seen, _) = landed_frame(&mut owner, &mut decos, now, Some(1), |f| f);
        assert_eq!(seen, Some((1, 1)), "the first frame that shows it takes it");
        assert!(owner.brain().cheer_owed(), "and the pet latched the cheer");
        let mut most = 0;
        for _ in 0..400 {
            now += Duration::from_millis(16);
            let (seen, frame) = landed_frame(&mut owner, &mut decos, now, Some(1), |f| f);
            assert_eq!(seen, Some((1, 1)), "handed back while the room names it");
            let cheer = frame
                .pet
                .motes
                .iter()
                .flatten()
                .filter(|m| {
                    matches!(
                        m.kind,
                        crate::kitty_pet::PetMoteKind::Note | crate::kitty_pet::PetMoteKind::Heart
                    )
                })
                .count();
            most = most.max(cheer);
        }
        assert!(
            (1..=3).contains(&most),
            "one cheer's ♪/♥ on glass, not a cheer per restatement: {most}"
        );
        assert!(
            !owner.brain().cheer_owed(),
            "nothing owed: the serial again is nothing"
        );
        assert!(!owner.needs_frames(), "and the pet settled");
        now += Duration::from_millis(16);
        let (seen, _) = landed_frame(&mut owner, &mut decos, now, Some(2), |f| f);
        assert_eq!(seen, Some((1, 2)));
        assert!(owner.brain().cheer_owed(), "a second end is news again");
    }

    /// NO CHEER WITHOUT THE PET (§11.3.1): under another trail style, or in
    /// serious mode, the first frame that shows the pane still takes the end
    /// — spent unplayed, so turning the pet on later replays nothing — and the
    /// brain latches nothing. NEGATIVE CONTROL (run by hand, files restored):
    /// `take_landed` handing back only an owned pet's end leaves it standing
    /// for a later pet.
    #[test]
    fn without_the_pet_a_landing_is_spent_unplayed() {
        for (why, glow, serious) in [
            (
                "another trail style",
                GlowOwnership {
                    style_raw_names_pet: false,
                    ..pet_glow()
                },
                false,
            ),
            ("serious mode", pet_glow(), true),
        ] {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            let now = materialize(&mut owner, &mut decos);
            let now = settle(&mut owner, &mut decos, now) + Duration::from_millis(16);
            let facts = facts_with(Some((4, 12)));
            let mut host = host_at(now);
            host.serious = serious;
            let room = RoomFacts {
                session: Some(SessionRoom {
                    landed: Some(1),
                    ..SessionRoom::default()
                }),
                ..RoomFacts::default()
            };
            let t = owner.prepare(
                PetFacts {
                    glow,
                    room: Some(room),
                    ..pet_facts(&facts, &host)
                },
                &mut decos,
            );
            assert_eq!(t.landed_seen, Some((1, 1)), "{why}: seen, so spent");
            assert!(!owner.brain().cheer_owed(), "{why}: no cheer");
            let _ = owner.tick(t);
        }
    }

    /// THE PAPER LAW, AS THE OWNER WIRES IT (§11.3.7 of
    /// `docs/DESIGN-harness-autopilot-2026-10-15.md`): `emit` hands the pet's
    /// emitter the page it draws on, the terminal ground where no dark theme
    /// is (`aterm_render::theme_is_dark`), so on white (Aterm Day) and cream
    /// (Linen) paper each ♪ and ♥ in the air takes the point mark's ink
    /// (`paper_mark_ink`: 4.5:1 against the page, its hue kept), and on a
    /// dark ground it keeps its own colour. NEGATIVE CONTROLS (as run, each
    /// failing this test): `page: None` in `emit_on`, the gold note reads
    /// 1.6:1 on white; the polarity flipped (`theme_is_dark(bg)`), the dark
    /// ground's motes are re-inked and the paper's are not.
    #[test]
    fn the_owner_inks_a_cheers_motes_for_the_page_it_draws_on() {
        use crate::kitty_pet::{PetMoteKind, PetMoteSprite};
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = materialize(&mut owner, &mut decos);
        let mut t = frame(
            &mut owner,
            &mut decos,
            now + Duration::from_millis(16),
            Some((4, 12)),
        );
        assert!(t.on_glass, "fixture: the pet is on glass");
        // The cheer's gold ♪ and pink ♥, stamped at birth as a lit ribbon's
        // offered colour would be, so the expected ink is exact.
        let air = [
            (PetMoteKind::Note, 0x00FF_C24Du32),
            (PetMoteKind::Heart, 0x00FF_6B9A),
            (PetMoteKind::Note, 0x00FF_C24D),
        ];
        for (k, (kind, rgb)) in air.into_iter().enumerate() {
            t.pet.motes[k] = Some(PetMoteSprite {
                kind,
                col: t.pet.col + 2.0 + k as f32,
                row: (t.pet.row - 2.0).max(0.5),
                scale: 1.0,
                rot: 0.4,
                alpha: 255,
                rgb: Some(rgb),
            });
        }
        let mut tints = |bg: u32| {
            let theme = ContrastFallback {
                bg,
                cursor: 0x0080_8080,
                accent: 0x0040_80FF,
            };
            let mut free = Vec::new();
            // A frame's bake budget takes one new tile family at a time: the
            // third frame draws every mote from warm tiles.
            for _ in 0..3 {
                free.clear();
                decos.begin_host_frame();
                let _ = owner.emit(&t, &[], theme, &mut decos, &mut free);
            }
            free.iter()
                .map(|s| s.tint)
                .filter(|&tint| {
                    air.iter().any(|&(_, rgb)| {
                        tint == rgb || tint == crate::rainbow_kitty::ribbon::paper_mark_ink(rgb, bg)
                    })
                })
                .collect::<Vec<_>>()
        };
        let contrast = |a: u32, b: u32| {
            let (ya, yb) = (
                crate::color_math::relative_luminance(a),
                crate::color_math::relative_luminance(b),
            );
            (ya.max(yb) + 0.05) / (ya.min(yb) + 0.05)
        };
        let dark = tints(0x0010_1010);
        assert_eq!(
            dark,
            air.map(|(_, rgb)| rgb).to_vec(),
            "a dark ground keeps each ♪/♥ its own colour"
        );
        for (name, bg) in [("white", 0x00F8_FAFDu32), ("cream", 0x00F9_F3E6)] {
            let paper = tints(bg);
            assert_eq!(
                paper,
                air.map(|(_, rgb)| crate::rainbow_kitty::ribbon::paper_mark_ink(rgb, bg))
                    .to_vec(),
                "{name}: each ♪/♥ takes the mark's ink for the page"
            );
            for &ink in &paper {
                assert!(
                    contrast(ink, bg) >= 4.5 - 1e-3,
                    "{name}: {ink:06x} on {bg:06x} reads at {:.2}:1",
                    contrast(ink, bg)
                );
            }
        }
    }

    /// An app surface over the grid keeps the resident owned but not presented.
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

    /// The load-shed latch takes the resident's frame train, not its motion: the
    /// sense is full motion and a moving body is offered at the coarse cadence.
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
        // A host that woke and drew nothing is offered a whole cadence out, never
        // a past instant it would re-arm every turn.
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

    /// Serious Mode's drain is a fresh brain (a latched trick, the body and the
    /// species alike); the session latches survive. The species is the brain's
    /// from the moment the host chooses it, and the next prepare restores it.
    #[test]
    fn the_drain_is_a_fresh_brain_that_keeps_the_session_latches() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        owner.set_species(PetSpecies::Dog);
        assert_eq!(
            owner.brain().species(),
            PetSpecies::Dog,
            "a cold owner's brain already wears the chosen animal"
        );
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
        // The latch still holds seq 5: that completion is not news, the next is.
        assert_ne!(PetSpecies::default(), PetSpecies::Dog, "fixture");
        assert_eq!(
            owner.brain().species(),
            PetSpecies::default(),
            "a drain is a fresh brain: the identity is gone until the next prepare"
        );
        now += Duration::from_millis(16);
        let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
        assert_eq!(owner.brain().species(), PetSpecies::Dog);
        assert_eq!(t.completion, None, "seq 5 was already felt");
        let _ = owner.tick(t);
        facts.cmd_done = Some((6, 0, None));
        now += Duration::from_millis(16);
        let t = prepared(&mut owner, &mut decos, &facts, &host_at(now), false);
        assert_eq!(t.completion, Some((0, None)));
    }

    /// A style that stops naming the pet and serious mode retire it; the Hidden
    /// edge and history hide it (identity kept); load keeps the full resident.
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
                glow: GlowOwnership {
                    enabled: true,
                    style: GlowStyle::Lumen,
                    style_raw_names_pet: false,
                },
                ..pet_facts(&facts, &host)
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
        let t = owner.sense(pet_facts(&facts, &host), &mut decos);
        assert!(!t.on_glass && !owner.needs_frames());

        // Hidden: the surface retires, the coat is kept (a coordinate-space
        // retire, never a drain).
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
                focused: false,
                ..pet_facts(&facts, &host)
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
        let t = owner.sense(pet_facts(&history, &host), &mut decos);
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
        let t = owner.sense(pet_facts(&facts, &host), &mut decos);
        assert!(t.on_glass);
        assert_eq!(t.pet.alpha, 255, "the full resident remains visible");
        assert_eq!(owner.alpha(), 255);
        assert!(
            owner.brain().is_active(),
            "the resident remains active under the shed"
        );
    }

    /// The shed contract: load shedding takes the resident's 60 fps frame train
    /// and nothing else. `phase`: 0 moving, 1 the latch edge, 2 moving under the
    /// shed, 3 settled under it, 4 moving after recovery. `smooth`: no frame
    /// stepped past ordinary motion. `gait`: the edge kept the brain's pose.
    /// `train`: the resident claims the frame train. `paced`: a moving resident is
    /// owed a wake within [`RESIDENT_SHED_CADENCE`]. `coarse`: no shed wake lands
    /// sooner than that after the last tick.
    ///
    /// `Buggy` replays four defects: the latch folded into the reduced-motion pin
    /// (a teleport, a forced seat, no wake), a pet kept on the 60 fps train under
    /// the shed, a settled pet offered at full-motion grain, and a stale latch
    /// that never hands the train back.
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

    /// The owner over a real terminal, observed and ticked as the native present
    /// does, with the host's shed facts and motion policy handed in per frame.
    struct ShedRig {
        term: aterm_core::terminal::Terminal,
        owner: CompanionOwner,
        decos: WordDecorations,
        now: Instant,
        prev: Option<PetFrame>,
        /// The largest step so far, in cells per 16 ms of clock.
        max_step: f32,
        /// Replay the snap rule: the latch folded into the reduced-motion pin.
        old_rule: bool,
        /// Every offer after a shed tick lands no sooner than
        /// [`RESIDENT_SHED_CADENCE`]: `[the owner's, the unfloored rule's]`.
        coarse: [bool; 2],
    }

    /// The resident's ordinary motion bound: 1.2 cells per 16 ms of clock (typing
    /// through a shed measures at most 0.41; the snap, 5.4).
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

        /// One present `dt` after the last, with a key's echo when `key` is given;
        /// returns the frame and whether its step stayed within the motion bound.
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

        /// A moving resident is on the train or owed a wake within the shed
        /// cadence; a settled one owes nothing.
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

    /// The shed edge on glass: typing at 80 ms/key, the load-shed latch engages
    /// mid-run, holds while typing goes on and the resident settles, then clears.
    /// The owner is driven like the native host (every 16 ms on the train, under
    /// the shed only at key echoes and its coarse offer). Each phase must be a
    /// [`resident_shed_model`] transition; the snap rule, replayed on the same
    /// drive, is rejected at the edge, and the unfloored rule at the settle.
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

            // Phase 2: typing under the shed; the host presents at key echoes and
            // the resident's coarse offer.
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
            // Settled, it is woken at its own next visible step, never sooner
            // than the shed cadence.
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
            // Negative control: the unfloored rule offers the seat's sub-cadence
            // grain at the same ticks; the model rejects it.
            let unfloored = shed_state(3, [smooth, gait, train, paced, rig.coarse[1]]);
            assert!(
                step(&mut state.clone(), "Settle", unfloored).is_err(),
                "negative control: the unfloored settled offers passed"
            );
            let observed = shed_state(3, [smooth, gait, train, paced, rig.coarse[0]]);
            step(&mut state, "Settle", observed).expect("settled under the shed");

            // Phase 4: the latch clears and keys put the resident back on the
            // train. The run left the cat out ahead, where it waits for the
            // writing (`PetBrain::lead` keeps the lead it took), so a burst
            // brings the caret up to it, and then it must move.
            let clear_at = rig.now;
            let mut train = false;
            for _ in 0..8 {
                let rise = (rig.now + FRAME - clear_at).as_secs_f32() / 0.25;
                let (_, ok) = rig.present(FRAME, Some(b'!'), false, rise.min(1.0));
                smooth &= ok;
                train |= rig.owner.needs_frames();
            }
            let observed = shed_state(4, [smooth, gait, train, paced, rig.coarse[0]]);
            step(&mut state, "Recover", observed).expect("full motion resumes");
            println!("new rule: max step {:.2} cells per 16 ms", rig.max_step);
        }
    }

    /// A capture takes the caller's motion policy and introduces no static
    /// scheduler posture of its own; disabling retires the remembered posture.
    #[test]
    fn capture_and_reenable_do_not_leave_stale_static_scheduler_custody() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let mut now = materialize(&mut owner, &mut decos);
        let facts = facts_with(Some((4, 12)));
        let mut host = host_at(now);
        host.capture = CaptureMode::StaticCapture;
        owner.sense(pet_facts(&facts, &host), &mut decos);
        assert!(
            owner.brain().needs_frames(),
            "the captured owner still has live motion"
        );
        assert!(
            owner.needs_frames(),
            "capture alone must not cancel that motion"
        );
        assert!(!owner.last_static);
        // Negative control: remembering 'static' from the capture mode itself.
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
        owner.sense(pet_facts(&facts, &host), &mut decos);
        // The shed moves the resident off the train without a static posture.
        assert!(!owner.last_static && owner.last_shed_tick == Some(now));
        assert!(!owner.needs_frames());
        owner.set_enabled_seed(false, 0x5EED);
        assert!(!owner.last_static && owner.last_shed_tick.is_none() && !owner.needs_frames());
        assert_eq!(owner.hit_rect(), None);
        assert_eq!(owner.alpha(), 0, "disable retires the body at once");
        assert!(!owner.brain().is_active());
        owner.set_enabled_seed(true, 0x5EED);
        now += Duration::from_millis(16);
        host.now = now;
        host.shed_envelope = 1.0;
        host.shed_active = false;
        owner.sense(pet_facts(&facts, &host), &mut decos);
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

    /// The resident stays full and pettable through the held song and every tail
    /// cutoff, sampled at its cadence or jumped to directly from the held song by an
    /// occluded frame, even when a competing head has live alpha. Each sample is a
    /// transition of `reduced_motion_companion_handoff_model` (its `Buggy` replays
    /// the former singing-head takeover and late-frame blackout), and a forged
    /// all-transparent projection of the same sample is rejected.
    #[test]
    fn a_song_and_its_tail_keep_full_resident_custody() {
        let model = aterm_models_effects::reduced_motion_companion_handoff_model();
        let held = (1.0, "StartReducedSong");
        let runs: [&[(f32, &str)]; 4] = [
            &[
                held,
                (0.5, "SampleAtHalfCutoff"),
                (0.4, "SampleCadencedBelowHalf"),
                (0.329, "SampleBelowFaceSwap"),
                (0.1, ""),
                (0.0, "DrainSongTail"),
            ],
            &[held, (0.49, "SampleLateBelowHalf")],
            &[held, (0.30, "SampleLateBelowFaceSwap")],
            &[held, (0.0, "SampleLateDrained")],
        ];
        for reduced_motion in [false, true] {
            for run in runs {
                let mut decos = WordDecorations::default();
                let mut owner = an_enabled_owner();
                let mut now = materialize(&mut owner, &mut decos);
                let facts = facts_with(Some((4, 12)));
                let mut state = model.init_state();
                for &(drive, action) in run {
                    now += Duration::from_millis(16);
                    let mut host = host_at(now);
                    host.reduced_motion = reduced_motion;
                    let result = owner.sense(
                        PetFacts {
                            sing: SingFacts {
                                drive,
                                flying_alpha: 200,
                            },
                            ..pet_facts(&facts, &host)
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
                    if action.is_empty() {
                        continue;
                    }
                    let mut observed = model.successors(action, &state)[0].clone();
                    observed.insert(
                        "singer_visible",
                        i64::from(result.duty != CompanionDuty::Pet),
                    );
                    observed.insert("pet_ready", i64::from(result.pet.alpha == 255));
                    observed.insert("pet_visible", i64::from(result.on_glass));
                    let label = format!("resident custody {action} (reduced={reduced_motion})");
                    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                        &model,
                        &[],
                        &state,
                        &observed,
                        Some(action),
                        &label,
                    );
                    assert!(ok, "{label}: the real owner's sample is rejected: {why}");
                    let mut blackout = observed.clone();
                    blackout.insert("singer_visible", 0);
                    blackout.insert("pet_visible", 0);
                    let (forged_ok, _) = aterm_spec::verify::validate_transition_tiered(
                        &model,
                        &[],
                        &state,
                        &blackout,
                        Some(action),
                        "forged blackout",
                    );
                    assert!(!forged_ok, "{label}: an all-transparent projection passed");
                    state = observed;
                }
            }
        }
    }

    /// Reduced motion pins the resident at its station: it is drawn, and a caret
    /// jump moves it without a walk.
    #[test]
    fn reduced_motion_pins_the_pet_at_its_station() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let now = Instant::now();
        let mut host = host_at(now);
        host.reduced_motion = true;
        let facts = facts_with(Some((4, 12)));
        let first = owner.sense(pet_facts(&facts, &host), &mut decos);
        assert_eq!(
            first.pet.alpha, 255,
            "the first reduced frame has real pixels"
        );
        let far = facts_with(Some((20, 70)));
        let mut host = host_at(now + Duration::from_millis(16));
        host.reduced_motion = true;
        let jumped = owner.sense(pet_facts(&far, &host), &mut decos);
        assert_eq!(jumped.pet.alpha, 255);
        assert_eq!(jumped.pet.lift, 0.0, "no arc");
        assert_eq!(
            jumped.pet.row, 20.0,
            "the station follows the caret row at once"
        );
    }

    /// Peek cues reach the brain as the landed head's centre in fractional cells:
    /// a far peek is scenery, a near one latches a bat.
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

    /// Over blank glass the frozen palette keys off [`ContrastFallback`]'s cursor
    /// slot, not the default foreground: the key is `from_rgb(bg, cursor, accent)`,
    /// and a neutral cursor on the same glass dresses a different cat.
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

    /// `retire_coordinate_space` is surface only, so a later completion is still
    /// felt; `retire_owner` also re-baselines the command probe. Both drop body and
    /// hit rect and keep the durable identity (species, worn coat).
    #[test]
    fn retire_coordinate_space_keeps_the_command_latch_and_retire_owner_rebaselines_it() {
        let sense_cmd = |owner: &mut CompanionOwner,
                         decos: &mut WordDecorations,
                         now: Instant,
                         cmd_done: Option<(u64, i32, Option<u64>)>| {
            let mut facts = facts_with(Some((4, 12)));
            facts.cmd_done = cmd_done;
            let host = host_at(now);
            let _ = owner.sense(pet_facts(&facts, &host), decos);
        };

        for owner_edge in [false, true] {
            let mut decos = WordDecorations::default();
            let mut owner = an_enabled_owner();
            owner.set_species(PetSpecies::Dog);
            let now = materialize(&mut owner, &mut decos);
            sense_cmd(&mut owner, &mut decos, now, Some((9, 0, None)));
            assert!(owner.hit_rect().is_some(), "fixture: a body to retire");
            let t = frame(&mut owner, &mut decos, now, Some((4, 12)));
            let (_, before) = owner.emit(&t, &[], BLACK_FALLBACK, &mut decos, &mut Vec::new());

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
            assert_eq!(
                owner.alpha(),
                0,
                "either retire clears the cached frame alpha ({owner_edge})"
            );
            assert!(
                !owner.brain().is_active() && !owner.needs_frames(),
                "either retire drops the body and its lane claim ({owner_edge})"
            );
            assert_eq!(
                (owner.brain().species(), owner.brain().worn_pair()),
                (PetSpecies::Dog, Some(before.worn)),
                "durable identity, species and coat, survives either retire ({owner_edge})"
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

    /// Switching between the main and alternate screen buffers, or replacing/
    /// re-gridding the owner, never leaks `finished_caret` or `hidden_rest`
    /// from the prior surface.
    #[test]
    fn alt_screen_switch_and_owner_retire_do_not_leak_finished_caret_or_hidden_rest() {
        let mut decos = WordDecorations::default();
        let mut owner = an_enabled_owner();
        let t0 = Instant::now();

        // Main screen finished frame with hidden cursor at (3, 15).
        let mut main_facts = facts_with(Some((3, 15)));
        main_facts.cursor_visible = false;
        let host0 = host_at(t0);
        let tick0 = owner.prepare(pet_facts(&main_facts, &host0), &mut decos);
        assert_eq!(tick0.escort_caret, Some((3, 15)));

        // Enter alt screen (`alt_screen = true`) on a torn frame (`frame_unfinished = true`)
        // with a hidden cursor at (20, 5): must NOT read the main screen's (3, 15).
        let t1 = t0 + Duration::from_millis(5);
        let mut alt_torn = facts_with(Some((20, 5)));
        alt_torn.alt_screen = true;
        alt_torn.cursor_visible = false;
        alt_torn.frame_unfinished = true;
        let host1 = host_at(t1);
        let tick1 = owner.prepare(pet_facts(&alt_torn, &host1), &mut decos);
        assert_eq!(
            tick1.escort_caret, None,
            "torn alt-screen frame must not inherit main-screen finished_caret"
        );

        // Finished alt-screen frame with unbracketed hidden cursor at (20, 5)
        // before HIDDEN_REST (5 ms < 30 ms): first reading on the alt surface is
        // taken at once, not held at the main screen's (3, 15).
        let mut alt_finished = alt_torn;
        alt_finished.frame_unfinished = false;
        let tick2 = owner.prepare(pet_facts(&alt_finished, &host1), &mut decos);
        assert_eq!(
            tick2.escort_caret,
            Some((20, 5)),
            "first hidden-cursor reading on alt screen is taken immediately"
        );

        // `rebase_geometry` clears `finished_caret` and `hidden_rest` even at the
        // same `(rows, cols)` (e.g. a font-step or DPI change).
        owner.rebase_geometry();
        let t2 = t1 + Duration::from_millis(5);
        let host2 = host_at(t2);
        let tick3 = owner.prepare(pet_facts(&alt_torn, &host2), &mut decos);
        assert_eq!(
            tick3.escort_caret, None,
            "rebase_geometry clears finished_caret"
        );

        // Re-seed `finished_caret`, `hidden_rest`, and `wrap_serial`, then verify
        // `retire_owner` and `resight_owner` clear all of them.
        for use_resight in [false, true] {
            let mut m = main_facts;
            m.wrap_serial = 7;
            let _ = owner.prepare(pet_facts(&m, &host2), &mut decos);
            owner.last_alpha = 200;
            owner.last_visible = true;
            if use_resight {
                owner.resight_owner();
            } else {
                owner.retire_owner();
            }
            assert_eq!(
                owner.alpha(),
                0,
                "use_resight={use_resight}: retired/resighted owner immediately reports alpha 0 before next tick"
            );
            assert!(
                !owner.last_visible,
                "use_resight={use_resight}: retired/resighted owner immediately clears last_visible"
            );
            let mut next = m;
            next.caret = Some((9, 2));
            next.wrap_serial = 99;
            let t3 = t2 + Duration::from_millis(5);
            let host3 = host_at(t3);
            let tick4 = owner.prepare(pet_facts(&next, &host3), &mut decos);
            assert_eq!(
                tick4.escort_caret,
                Some((9, 2)),
                "use_resight={use_resight}: replacement owner takes its first hidden caret immediately"
            );
            assert!(
                !tick4.sense.wrapped,
                "use_resight={use_resight}: replacement owner baselines wrap_serial silently"
            );
        }
    }
}
