// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet driver, [`CompanionOwner`](crate::companion::CompanionOwner): owns a resident pet brain per surface, senses for it, picks its coat, decides custody (pet vs flying head vs idle) and draws it.

use aterm_core::terminal::RenderCell;
use aterm_core::terminal::caret_path::CARET_PATH_LEN;
use aterm_render::sprites::FreeSprite;
use aterm_time::Instant;

use crate::cat_baker::CatColorKey;
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

/// Which rung of [`crate::companion_look::companion_precedence`] won a verdict, so the render sync sites can tell a program win (the only one that may carry an arrival ceremony) from a favourite or launch win.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CompanionRung {
    /// The pinned favourite won.
    Favourite,
    /// The tenured program cat won: the one rung whose arrival may be a ceremony, as ruled by the host's tenure gate.
    Program,
    /// The launch kitty floor won — "no stronger claim".
    Launch,
}

/// Which companion, at most one, puts a body in this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompanionDuty {
    /// No companion body this frame.
    Idle,
    /// The full-body resident pet ([`crate::kitty_pet`]).
    Pet,
    /// The earned flying head ([`crate::kitty_cursor`]), escorting the caret at `cell`.
    FlyingHead { cell: (u16, u16) },
}

/// Petting grace outside the pet's drawn body (frame px, each side): the target moves, so an honest aim must not slide off a paw mid-walk.
pub const PET_HIT_SLOP_PX: i32 = 4;

/// Whether a pointer at `(x, y)` (frame px) lands on the pet's drawn body `rect` (`(x0, x1, y0, y1)`, right/bottom exclusive), padded by `slop` on every side.
#[inline]
#[must_use]
pub const fn pet_rect_hit(rect: (i32, i32, i32, i32), x: f64, y: f64, slop: i32) -> bool {
    let (x0, x1, y0, y1) = rect;
    x >= x0.saturating_sub(slop) as f64
        && x < x1.saturating_add(slop) as f64
        && y >= y0.saturating_sub(slop) as f64
        && y < y1.saturating_add(slop) as f64
}

/// Resolve the companion's local terminal palette from the grid cells its sprite intersects, capped at 64 samples so this stays allocation-free and O(1).
#[inline]
#[must_use]
pub const fn cursor_cat_color_key(
    cells: &[Vec<RenderCell>],
    geom: EffectGeom,
    footprint: CatFootprint,
    fallback_bg: u32,
    fallback_fg: u32,
    fallback_accent: u32,
) -> CatColorKey {
    const MAX_SAMPLES: u32 = 64;
    let table = crate::color_math::relative_luminance_table();
    if geom.cell_w == 0 || geom.cell_h == 0 || cells.is_empty() {
        return CatColorKey::from_rgb_with(table, fallback_bg, fallback_fg, fallback_accent);
    };

    let cw = geom.cell_w as i64;
    let ch = geom.cell_h as i64;
    let fx = footprint.x as i64;
    let fy = footprint.y as i64;
    let x0 = if fx > 0 { fx } else { 0 };
    let y0 = if fy > 0 { fy } else { 0 };
    let x1_max = (geom.cols as i64) * cw;
    let x1_cand = fx + (footprint.w as i64);
    let x1 = if x1_cand < x1_max { x1_cand } else { x1_max };
    let y1_max = (geom.rows as i64) * ch;
    let y1_cand = fy + (footprint.h as i64);
    let y1 = if y1_cand < y1_max { y1_cand } else { y1_max };
    if x1 <= x0 || y1 <= y0 {
        return CatColorKey::from_rgb_with(table, fallback_bg, fallback_fg, fallback_accent);
    }
    let c0 = (x0 / cw) as usize;
    let c1 = ((x1 - 1) / cw) as usize;
    let r0 = (y0 / ch) as usize;
    let r1 = ((y1 - 1) / ch) as usize;

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
    let mut last_bg_rgb = [0u8; 3];
    let mut fallback_band_opt: Option<u8> = None;
    let cols_per_row = (c1 - c0 + 1) as u64;
    let mut row = r0;
    'rows: while row <= r1 {
        if row >= cells.len() {
            let rem_rows = (r1 - row + 1) as u64;
            let rem_raw = rem_rows * cols_per_row;
            let rem_cap = (MAX_SAMPLES - sampled) as u64;
            let rem_cells = (if rem_raw < rem_cap { rem_raw } else { rem_cap }) as u32;
            if rem_cells > 0 {
                let fallback_band = match fallback_band_opt {
                    Some(b) => b,
                    None => CatColorKey::background_band_rgb(table, fallback_bg_rgb),
                };
                if fallback_band < min_background_band {
                    min_background_band = fallback_band;
                }
                if fallback_band > max_background_band {
                    max_background_band = fallback_band;
                }
                bg_sum[0] += fallback_bg_rgb[0] as u32 * rem_cells;
                bg_sum[1] += fallback_bg_rgb[1] as u32 * rem_cells;
                bg_sum[2] += fallback_bg_rgb[2] as u32 * rem_cells;
                sampled += rem_cells;
            }
            break 'rows;
        }
        let line = cells[row].as_slice();
        let in_end = if c1 < line.len() { c1 + 1 } else { line.len() };
        let mut col = c0;
        while col < in_end {
            let cell = &line[col];
            let background_rgb = cell.bg;
            if sampled == 0
                || background_rgb[0] != last_bg_rgb[0]
                || background_rgb[1] != last_bg_rgb[1]
                || background_rgb[2] != last_bg_rgb[2]
            {
                let band = CatColorKey::background_band_rgb(table, background_rgb);
                last_bg_rgb = background_rgb;
                if band < min_background_band {
                    min_background_band = band;
                }
                if band > max_background_band {
                    max_background_band = band;
                }
            }
            bg_sum[0] += background_rgb[0] as u32;
            bg_sum[1] += background_rgb[1] as u32;
            bg_sum[2] += background_rgb[2] as u32;
            sampled += 1;
            if !cell.wide && crate::word_decorations::is_ink_char(cell.ch) {
                if principal_fg.is_none() {
                    principal_fg = Some(cell.fg);
                }
                fg_sum[0] += cell.fg[0] as u32;
                fg_sum[1] += cell.fg[1] as u32;
                fg_sum[2] += cell.fg[2] as u32;
                visible += 1;
            }
            if sampled == MAX_SAMPLES {
                break 'rows;
            }
            col += 1;
        }
        if col <= c1 {
            let oob_raw = (c1 - col + 1) as u32;
            let oob_cap = MAX_SAMPLES - sampled;
            let oob = if oob_raw < oob_cap { oob_raw } else { oob_cap };
            if oob > 0 {
                let fallback_band = match fallback_band_opt {
                    Some(b) => b,
                    None => {
                        let b = CatColorKey::background_band_rgb(table, fallback_bg_rgb);
                        fallback_band_opt = Some(b);
                        b
                    }
                };
                if fallback_band < min_background_band {
                    min_background_band = fallback_band;
                }
                if fallback_band > max_background_band {
                    max_background_band = fallback_band;
                }
                last_bg_rgb = fallback_bg_rgb;
                bg_sum[0] += fallback_bg_rgb[0] as u32 * oob;
                bg_sum[1] += fallback_bg_rgb[1] as u32 * oob;
                bg_sum[2] += fallback_bg_rgb[2] as u32 * oob;
                sampled += oob;
                if sampled == MAX_SAMPLES {
                    break 'rows;
                }
            }
        }
        if row == usize::MAX {
            break;
        }
        row += 1;
    }
    if sampled == 0 {
        return CatColorKey::from_rgb_with(table, fallback_bg, fallback_fg, fallback_accent);
    }
    let background =
        crate::word_decorations::rgb3_to_u32(crate::word_decorations::avg_rgb(bg_sum, sampled));
    let foreground = match principal_fg {
        Some(fg) => crate::word_decorations::rgb3_to_u32(fg),
        None => fallback_fg,
    };
    let surrounding = if visible == 0 {
        fallback_accent
    } else {
        crate::word_decorations::rgb3_to_u32(crate::word_decorations::avg_rgb(fg_sum, visible))
    };
    CatColorKey::from_rgb_span_with(
        table,
        background,
        foreground,
        surrounding,
        min_background_band,
        max_background_band,
    )
}

/// Describe the cursor companion actually drawn this frame, for the ambient word-cats' pixel yield.
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

/// Feed the resident pet the presentation context shared by live glass and capture, immediately before its tick.
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

/// Compute how loudly this frame's verdict lands on the pet across sync sites.
#[must_use]
#[inline]
pub const fn pet_arrival_for_sync(
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

/// The pet's petting hit box: the brain's drawn body (`PetFrame::body_px`) offset into frame px by `origin`.
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

/// Perk-and-watch: a frame is an output burst only when the pane visibly gained output (new scrollback rows, or the content clock moved within one session), the shell reports an executing command (OSC 133/633 C..D, so prompt echo never perks the pet), the viewport is at the live bottom, and the pane is not on the alternate screen: a full-screen program rewrites its own cells in place, so its composer would read as a build log.
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

/// Edge-detect the emulator's autowrap serial: true only when the same session's serial changed since this surface's last read; a session switch or first read stores the baseline.
#[inline]
pub const fn wrap_fact_edge(seen: &mut Option<(u64, u64)>, session: u64, serial: u64) -> bool {
    let wrapped = matches!(*seen, Some((sid, s)) if sid == session && s != serial);
    *seen = Some((session, serial));
    wrapped
}

/// Map the frame-px pointer onto the pet's pane as a fractional cell `(col, row)`; `None` outside the pane's grid.
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
    let dx = pointer_px.0 - origin.0 as f64;
    let dy = pointer_px.1 - origin.1 as f64;
    if !(dx >= 0.0 && dy >= 0.0) {
        return None;
    }
    let col = dx / cw as f64;
    let row = dy / ch as f64;
    if col < grid.0 as f64 && row < grid.1 as f64 {
        Some((col as f32, row as f32))
    } else {
        None
    }
}

/// Whether the open-mouth singing face is live; it does not move custody.
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
        Some(cell) if kitty_alpha > 0 => CompanionDuty::FlyingHead { cell },
        _ => CompanionDuty::Idle,
    }
}

/// Cursor companions belong to the live grid: the decoration lifecycle keeps running while history is visible, but no companion is drawn over retained rows.
#[inline]
#[must_use]
pub const fn cursor_companion_presentable(
    decoration_presentable: bool,
    live_viewport: bool,
) -> bool {
    decoration_presentable && live_viewport
}

/// The resident's cadence under the load-shed latch.
pub const RESIDENT_SHED_CADENCE: aterm_time::Duration = aterm_time::Duration::from_millis(50);

/// Real surface custody for the full resident.
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

/// The load-shed half of flying-head admission: a presentable head stays admitted while the envelope is finite and above zero.
#[inline]
#[must_use]
pub const fn shed_companion_presentable(base_presentable: bool, envelope: f32) -> bool {
    base_presentable && envelope.is_finite() && envelope > 0.0
}

/// Attenuate a companion's alpha by the load-shed envelope at the final presentation seam; the brains keep their unscaled state.
#[inline]
#[must_use]
pub const fn shed_companion_alpha(alpha: u8, envelope: f32) -> u8 {
    let envelope = if envelope.is_finite() {
        envelope.clamp(0.0, 1.0)
    } else {
        0.0
    };
    (alpha as f32 * envelope).round() as u8
}

/// Whether the soft-shed envelope still owes another presentation: fade-out stops at zero; fade-in starts from that exact zero and stops only at full amplitude.
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
#[inline]
pub const fn pin_pet_mode_exit(pet_mode: bool, frame: &mut crate::kitty_cursor::CatFrame) {
    if pet_mode {
        frame.exit = crate::kitty_cursor::CatExit::Plain;
    }
}

/// The `0x00RRGGBB` colours the palette sampler falls back to over blank cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContrastFallback {
    /// The terminal ground.
    pub bg: u32,
    /// The terminal's cursor colour.
    pub cursor: u32,
    /// The trail accent.
    pub accent: u32,
}

/// Whether the host's `cursor_pet` setting names a pet ([`crate::cursor_pet`]). The pet is
/// owned by that setting alone: no trail style, and no trail at all, is needed for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetOwnership {
    pub on: bool,
}

impl PetOwnership {
    /// No pet.
    pub const NONE: Self = Self { on: false };
    /// A pet.
    pub const PET: Self = Self { on: true };
}

impl Default for PetOwnership {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

/// What the resident is told about its session, restated by the host each frame (the brain diffs each fact).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoomFacts {
    /// The session's status phase is `Quiet`: a foreground job, nothing printing recently.
    pub quiet: bool,
    /// The caret of a sibling pane streaming this frame, in the pet's pane cells `(col, row)`; `None` in a single pane and on quiet frames.
    pub sibling: Option<(f32, f32)>,
    /// The session's own facts; `None` when the host no longer holds the session.
    pub session: Option<SessionRoom>,
}

impl RoomFacts {
    /// Empty room facts with no active session or sibling pane.
    pub const EMPTY: Self = Self {
        quiet: false,
        sibling: None,
        session: None,
    };
}

impl Default for RoomFacts {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// The facts only a live session has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionRoom {
    /// A turn lease is live: an agent is driving this session.
    pub lease: bool,
    /// The newest turn's id and whether it settled; `(0, false)` for none.
    pub turn: (u64, bool),
    /// The inbox as the pet sees it.
    pub inbox: RoomInbox,
    /// The serial of the session's task end (§11.2's end: Land's landing) that no pet has cheered yet; `None` when none stands or it was cheered (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.3).
    pub landed: Option<u32>,
}

impl SessionRoom {
    /// Empty session facts with no lease, turn, inbox items, or uncheered landing.
    pub const EMPTY: Self = Self {
        lease: false,
        turn: (0, false),
        inbox: RoomInbox::EMPTY,
        landed: None,
    };
}

impl Default for SessionRoom {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// One frame's inputs to [`CompanionOwner::prepare`].
#[derive(Clone, Copy, Debug)]
pub struct PetFacts<'a> {
    pub facts: &'a TerminalFacts,
    pub host: &'a HostFrameInput,
    /// Whether the host's pet setting names a pet.
    pub pet: PetOwnership,
    pub sing: SingFacts,
    /// The surface has focus (native: its cursor-effect focus).
    pub focused: bool,
    /// An app surface covers the grid (command palette, Settings, tab menu): the resident stays owned but is not presented.
    pub obscured: bool,
    /// A composed surface's focused pane: session and pixel origin in the window grid (see [`prepare_resident_pet_tick`]).
    pub pane: Option<(u64, (i32, i32))>,
    /// The room the pet lives in, `None` for a host that has none.
    pub room: Option<RoomFacts>,
    /// The caret's last moves on its screen, each at its output batch's time, oldest first
    /// (`Terminal::caret_path`, read with the facts): the brain lives a late frame's span on them
    /// ([`crate::kitty_pet::PetBrain::note_caret_path`]). None for a host that reads none.
    pub caret_path: [Option<(Instant, (u16, u16))>; CARET_PATH_LEN],
}

impl<'a> PetFacts<'a> {
    /// A focused, unobscured single-pane frame of `facts` and `host` under the pet setting
    /// `pet`, with no sing-along and no room; each `with_*` sets one field, and a field added
    /// later takes its default here, so no host or harness literal changes (§15.1 P0.1).
    #[inline]
    #[must_use]
    pub const fn new(
        facts: &'a TerminalFacts,
        host: &'a HostFrameInput,
        pet: PetOwnership,
    ) -> Self {
        Self {
            facts,
            host,
            pet,
            sing: SingFacts::EMPTY,
            focused: true,
            obscured: false,
            pane: None,
            room: None,
            caret_path: [None; CARET_PATH_LEN],
        }
    }

    /// The caret's last moves, read from the terminal with the facts ([`Self::caret_path`]).
    #[inline]
    #[must_use]
    pub const fn with_caret_path(
        mut self,
        caret_path: [Option<(Instant, (u16, u16))>; CARET_PATH_LEN],
    ) -> Self {
        self.caret_path = caret_path;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_sing(mut self, sing: SingFacts) -> Self {
        self.sing = sing;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_obscured(mut self, obscured: bool) -> Self {
        self.obscured = obscured;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_pane(mut self, pane: Option<(u64, (i32, i32))>) -> Self {
        self.pane = pane;
        self
    }

    #[inline]
    #[must_use]
    pub const fn with_room(mut self, room: Option<RoomFacts>) -> Self {
        self.room = room;
        self
    }
}

/// The half-sensed frame [`CompanionOwner::prepare`] hands back.
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
    /// `(exit code, exec duration ms)`, `Some` exactly once per completion, never on a session switch or a capture.
    pub completion: Option<(i32, Option<u64>)>,
    /// `(session, serial)`: the task end the room named ([`SessionRoom::landed`]) was on screen in this pane this frame — a present to a focused, unobscured, live surface — so the host marks it cheered and no frame, here or in another window, hands it again.
    pub landed_seen: Option<(u64, u32)>,
    /// The caret's moves the brain lives a late frame's span on ([`PetFacts::caret_path`]), when
    /// its caret is the terminal's.
    caret_path: [Option<(Instant, (u16, u16))>; CARET_PATH_LEN],
}

impl PetTick {
    /// The caret the cursor companions escort this frame, hidden or not: the terminal's, except on a frame the program has not finished, where a hidden cursor is its write position and this is the last finished present's caret (`None` before one, or off the live viewport).
    #[inline]
    #[must_use]
    pub const fn escort_caret(&self) -> Option<(u16, u16)> {
        self.escort_caret
    }
}

#[cfg(test)]
impl PetTick {
    /// The frame's burst verdict; the brain's own copy is not refreshed on a tick it returns from early.
    #[inline]
    pub(crate) const fn output_burst(&self) -> bool {
        self.sense.output_burst
    }
}

/// What one frame resolved — the frame the emitter draws, the custody verdict, and the yield box the word engine avoids.
#[derive(Clone, Copy, Debug)]
pub struct CompanionFrame {
    /// The brain's resident frame, retaining its alpha under load pressure.
    pub pet: PetFrame,
    pub duty: CompanionDuty,
    /// The pet is drawn this frame: owned, presentable, custody won, alpha > 0.
    pub on_glass: bool,
    /// The pet's live body in grid px (`PetFrame::body_px`), `None` when it is not on glass.
    pub body_px: Option<(i32, i32, i32, i32)>,
    /// The one-cat-per-caret yield handed to `WordDecorations::tick`.
    pub companion: Option<CompanionOnGlass>,
    /// `PetFrame::fp` — non-zero while anything rides the lane, byte-stable once nothing does.
    pub fp: u64,
    /// Slot for slot with `pet.departures`: the pane a ghost was drawn in when it is not this frame's surface, still on glass; `None` otherwise.
    pub glass: [Option<PetGlass>; PET_DEPARTURES_MAX],
}

/// One terminal surface's place on glass: its session, the window-grid pixel origin of its cell `(0, 0)` and its size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetGlass {
    pub session: u64,
    pub origin: (i32, i32),
    pub cols: u16,
    pub rows: u16,
}

/// The pet's per-session latches. Every key carries the session, so a tab or
/// pane switch re-baselines silently.
#[derive(Clone, Copy, Debug)]
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
    /// read as.
    finished_caret: Option<FinishedCaret>,
    /// `((session, alt_screen), Terminal::sync_end_seq)` as the last present
    /// read them: a frame whose count has moved on was closed in the stream.
    brackets: Option<((u64, bool), u64)>,
    /// A hidden cursor's move, not yet taken ([`HIDDEN_REST`]).
    hidden_rest: Option<HiddenRest>,
}

impl Latches {
    const EMPTY: Self = Self {
        cmd_seq: None,
        content_seq: None,
        wrap_serial: None,
        finished_caret: None,
        brackets: None,
        hidden_rest: None,
    };
}

impl Default for Latches {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

/// [`Latches::finished_caret`]: the caret reading of the last finished present
/// on one surface's grid.
#[derive(Clone, Copy, Debug)]
struct FinishedCaret {
    surface: (u64, bool),
    grid: (u16, u16),
    reading: CaretReading,
}

const HIDDEN_REST: std::time::Duration = std::time::Duration::from_millis(30);

/// [`Latches::hidden_rest`]: the caret taken, and the cell the hidden cursor
/// has stood at since when, on one surface's grid.
#[derive(Clone, Copy, Debug)]
struct HiddenRest {
    surface: (u64, bool),
    grid: (u16, u16),
    taken: Option<(u16, u16)>,
    seen: (Option<(u16, u16)>, Instant),
    /// When the last frame read the caret: a move this frame first sees came after it.
    read: Instant,
}

type CaretReading = (Option<(u16, u16)>, bool);

/// The pet driver.
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
    /// surface.
    own_glass: [Option<OwnGlass>; OWN_GLASS_MAX],
    /// The last own-glass key handed out (`OwnGlass::key`); never `0`.
    glass_key: u32,
    /// This frame's host vouched that the pending ghost's glass is on glass as
    /// drawn.
    resight_offered: bool,
    /// `(session, serial)`: the task end this pet last cheered
    /// ([`SessionRoom::landed`]), so a serial handed again (the host's mark
    /// not yet read back) latches nothing.
    landed_taken: Option<(u64, u32)>,
}

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
        Self::new()
    }
}

impl CompanionOwner {
    /// Construct a fresh pet driver in its disabled, byte-identical-off posture.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        let base = KittyLook::DEFAULT;
        Self {
            pet: PetBrain::new(),
            species: PetSpecies::Cat,
            enabled: false,
            pair: (base.coat, base.iris),
            arrival: PetArrival::Quiet,
            rung: CompanionRung::Launch,
            latches: Latches::EMPTY,
            pointer_px: None,
            hit_rect: None,
            last_alpha: 0,
            last_visible: false,
            last_static: false,
            last_shed_tick: None,
            geom: EffectGeom::EMPTY,
            last_sync: None,
            drawn_frame: None,
            resight_frame: None,
            own_glass: [None; OWN_GLASS_MAX],
            glass_key: 0,
            resight_offered: false,
            landed_taken: None,
        }
    }

    /// An owner the host dresses every frame through [`Self::set_look`] rather than the seed.
    #[must_use]
    #[inline]
    pub const fn dressed_by_host() -> Self {
        let mut owner = Self::new();
        owner.enabled = true;
        owner
    }

    /// Dress the pet in `KittyLook::for_launch(seed)` (rung `Launch`, quiet); `enabled = false` retires it outright, so off costs and draws nothing.
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

    /// Dress the pet from the native verdict ([`crate::companion_look`]); only a `Program` rung may carry a ceremony.
    #[inline]
    pub const fn set_look(&mut self, pair: (u8, u8), arrival: PetArrival, rung: CompanionRung) {
        self.pair = pair;
        self.arrival = arrival;
        self.rung = rung;
    }

    /// Which animal the pet is drawn as: kept for every `prepare` to re-apply (so a drain's fresh brain wears it) and written to the brain now.
    #[inline]
    pub const fn set_species(&mut self, s: PetSpecies) {
        self.species = s;
        self.pet.set_species(s);
    }

    /// The pointer in frame px between frames: `true` iff the stored value changed, so an idle hover costs no render.
    #[inline]
    pub const fn set_pointer(&mut self, px: Option<(f32, f32)>) -> bool {
        let next = match px {
            Some((x, y)) if x.is_finite() && y.is_finite() => Some((x, y)),
            _ => None,
        };
        if px.is_some() && next.is_none() {
            return false;
        }
        let changed = match (self.pointer_px, next) {
            (Some((x0, y0)), Some((x1, y1))) => x0 != x1 || y0 != y1,
            (None, None) => false,
            _ => true,
        };
        self.pointer_px = next;
        changed
    }

    /// Classify the cell plane being presented, minus the host's late overlays (`exclusions`, all protected).
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

    /// [`Self::observe_console`], with the host's chrome spliced over the grid (`exclusions`: the footer's rules and chips, the harness card's glass) apart from its transient overlays (`overlays`: the find bar, a fact menu's card).
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
                blanked: &[],
                painted_over: None,
            },
        )
    }

    /// [`Self::observe_console_with_overlays`], with the person's typed glyphs (the predicted echo, the IME preedit) reserved as ink ([`crate::pet_world::PetReserve`]): what the native host observes, read in full every time ([`Self::observe_console_filled`] keeps a redraw's reading).
    #[inline]
    pub fn observe_console_reserved(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &crate::pet_world::PetWorldFacts,
        pane: crate::pet_world::PetPane,
        reserve: crate::pet_world::PetReserve<'_>,
    ) -> usize {
        self.observe_console_filled(
            input,
            crate::pet_world::PetFill::Unconsumed,
            facts,
            pane,
            reserve,
        )
    }

    /// [`Self::observe_console_reserved`] of cells the host filled as `fill` says: a redraw's [`crate::pet_world::PetFill::Consumed`] or [`crate::pet_world::PetFill::Retained`] scratch keeps the reading the pet holds of the same cells, and the count is then 0.
    #[inline]
    pub fn observe_console_filled(
        &mut self,
        input: &aterm_core::render::RenderInput,
        fill: crate::pet_world::PetFill,
        facts: &crate::pet_world::PetWorldFacts,
        pane: crate::pet_world::PetPane,
        reserve: crate::pet_world::PetReserve<'_>,
    ) -> usize {
        self.pet
            .observe_console_with_reserve(input, facts, pane, reserve, fill);
        self.pet.observed_cells()
    }

    /// The caret of a finished frame, as the companions take it: the cursor
    /// itself, except a hidden one that moved with no bracket closing
    /// (`bracketed` false: no newly closed frame ended at that cell), which
    /// is taken at a new cell only once it has
    /// rested there [`HIDDEN_REST`] ([`Latches::hidden_rest`]); until then
    /// the caret taken before stands.
    #[inline]
    fn rested_caret(
        &mut self,
        facts: &TerminalFacts,
        grid: (u16, u16),
        (now, arrived): (Instant, Option<Instant>),
        present: bool,
        bracketed: bool,
    ) -> Option<(u16, u16)> {
        // A hidden cursor rests from when it came to its cell ([`PetFacts::caret_path`], the
        // output batch that left it there), not from the frame that first saw it there: a host
        // that drew late (a starved machine) took a caret that had long rested a whole frame
        // later, and the cat set off that much later than on a steady host. A move this frame
        // first sees came after the last frame read the caret elsewhere, whatever clock its batch
        // was stamped on.
        let came = |read: Instant| match arrived {
            Some(at) if self.pet.times_the_caret() => at.max(read).min(now),
            _ => now,
        };
        let surface = (facts.session, facts.alt_screen);
        let rest = match self.latches.hidden_rest {
            Some(mut rest)
                if rest.surface == surface
                    && rest.grid == grid
                    && !facts.cursor_visible
                    && !bracketed =>
            {
                if rest.taken == facts.caret {
                    rest.seen = (facts.caret, now);
                } else if rest.seen.0 != facts.caret {
                    rest.seen = (facts.caret, came(rest.read));
                }
                if rest.taken != facts.caret
                    && now.saturating_duration_since(rest.seen.1) >= HIDDEN_REST
                {
                    rest.taken = facts.caret;
                }
                rest.read = now;
                rest
            }
            _ => HiddenRest {
                surface,
                grid,
                taken: facts.caret,
                seen: (facts.caret, now),
                read: now,
            },
        };
        if present {
            self.latches.hidden_rest = Some(rest);
        }
        rest.taken
    }

    /// Prepare the frame's notes before the tick without advancing present-owned latches on a capture.
    #[inline]
    pub fn prepare(&mut self, f: PetFacts<'_>, decos: &mut WordDecorations) -> PetTick {
        let PetFacts {
            facts,
            host,
            pet: owner,
            sing,
            focused,
            obscured,
            pane,
            room,
            caret_path,
        } = f;
        let now = host.now;
        let geom = EffectGeom {
            cell_w: host.geometry.cell_w,
            cell_h: host.geometry.cell_h,
            rows: host.geometry.rows,
            cols: host.geometry.cols,
        };
        self.geom = geom;
        let pet_mode = owner.on;
        let present = host.capture == CaptureMode::Present;
        let write_cursor = facts.frame_unfinished && !facts.cursor_visible;
        let surface = (facts.session, facts.alt_screen);
        let grid = (host.geometry.rows, host.geometry.cols);
        let bracketed = facts.sync_end_caret.is_some()
            && facts.caret == facts.sync_end_caret
            && matches!(
                self.latches.brackets,
                Some((at_surface, seq)) if at_surface == surface && seq != facts.sync_end_seq
            );
        if present {
            self.latches.brackets = Some((surface, facts.sync_end_seq));
        }
        let (caret, caret_painted) = if write_cursor {
            match self.latches.finished_caret {
                Some(fc) if fc.surface == surface && fc.grid == grid && facts.live_viewport => {
                    fc.reading
                }
                _ => (None, false),
            }
        } else {
            let reading = (
                self.rested_caret(
                    facts,
                    grid,
                    (
                        now,
                        caret_path
                            .iter()
                            .flatten()
                            .next_back()
                            .filter(|&&(_, cell)| Some(cell) == facts.caret)
                            .map(|&(at, _)| at),
                    ),
                    present,
                    bracketed,
                ),
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
        let owned = self.enabled && !host.serious && pet_mode;
        let presentable = resident_pet_surface_presentable(
            focused,
            host.visibility != Visibility::Hidden && !obscured,
            facts.live_viewport,
            self.pet.has_reading_interest(),
        );
        let visible = owned && presentable;
        if !owned {
            self.pet.retire_unowned();
        }
        let mut completion = None;
        let mut landed_seen = None;
        let mut burst = false;
        let mut wrapped = false;
        let mut pointer = None;
        if present {
            let seq = match facts.cmd_done {
                Some((e, _, _)) => e,
                None => 0,
            };
            let key = (facts.session, seq);
            if self.latches.cmd_seq != Some(key) {
                let same_session =
                    matches!(self.latches.cmd_seq, Some((sid, _)) if sid == facts.session);
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
            self.pet.note_executing(now, facts.shell_executing);
            self.pet.note_full_screen(facts.alt_screen);
            let advanced = matches!(
                self.latches.content_seq,
                Some((sid, s)) if sid == facts.session && facts.content_seq > s
            );
            self.latches.content_seq = Some((facts.session, facts.content_seq));
            burst = pet_output_burst(
                facts.scrolled,
                advanced,
                facts.shell_executing,
                facts.display_offset == 0,
                facts.alt_screen,
            );
            wrapped = !write_cursor
                && wrap_fact_edge(
                    &mut self.latches.wrap_serial,
                    facts.session,
                    facts.wrap_serial,
                );
            if focused
                && visible
                && self.hit_rect.is_some()
                && let Some((x, y)) = match host.pointer_px {
                    Some(p) => Some(p),
                    None => self.pointer_px,
                }
            {
                pointer = pet_pointer_cell(
                    (f64::from(x), f64::from(y)),
                    host.geometry.origin_px,
                    (usize::from(geom.cell_w), usize::from(geom.cell_h)),
                    (usize::from(geom.cols), usize::from(geom.rows)),
                );
            }
        }
        if !focused {
            self.retire_coordinate_space();
        }
        self.pet.note_surface((facts.session, facts.alt_screen));
        self.pet.note_repainting(write_cursor);
        self.feed_ink(decos, pane);
        let reduced_motion = host.reduced_motion;
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
            painted_caret: if facts.cursor_visible {
                facts.caret
            } else {
                None
            },
            escort_caret: caret,
            origin: host.geometry.origin_px,
            glass: PetGlass {
                session: facts.session,
                origin: match pane {
                    Some((_, origin)) => origin,
                    None => (0, 0),
                },
                cols: geom.cols,
                rows: geom.rows,
            },
            completion,
            landed_seen,
            // The terminal's path is the brain's caret's only where that caret is the terminal's
            // live one: not a held write position's last finished caret, not a hidden cursor's
            // rest the frame has not taken yet.
            caret_path: if visible && !write_cursor && caret == facts.caret {
                caret_path
            } else {
                [None; CARET_PATH_LEN]
            },
        }
    }

    /// THE KITTY CHEERS THE LANDING, WHEN YOU ARE THERE TO SEE IT
    /// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §11.3): the room names
    /// a task end no pet has cheered (`landed`, its serial).
    #[inline]
    const fn take_landed(
        &mut self,
        session: u64,
        landed: Option<u32>,
        presentable: bool,
        owned: bool,
    ) -> Option<(u64, u32)> {
        let Some(landed) = landed else {
            return None;
        };
        let key = (session, landed);
        if !presentable {
            return None;
        }
        let same = matches!(self.landed_taken, Some((s, l)) if s == key.0 && l == key.1);
        if owned && !same {
            self.landed_taken = Some(key);
            self.pet.note_room_landed();
        }
        Some(key)
    }

    #[inline]
    const fn clear_own_glass_offers(&mut self) -> bool {
        let mut any = false;
        let mut i = 0;
        while i < self.own_glass.len() {
            if let Some(own) = &mut self.own_glass[i] {
                own.offered = false;
                any = true;
            }
            i += 1;
        }
        any
    }

    /// The host's terminal panes on glass this frame, offered before [`Self::tick`]: records whether each pending or own ghost glass is among them exactly as drawn.
    #[inline]
    pub fn offer_glass(&mut self, glass: impl IntoIterator<Item = PetGlass>) {
        let any_own = self.clear_own_glass_offers();
        let want_resight = self.resight_frame.as_ref().map(|f| f.glass);
        if want_resight.is_none() && !any_own {
            return;
        }
        self.resight_offered = false;
        for g in glass {
            if let Some(rg) = want_resight {
                self.resight_offered |= rg == g;
            }
            if any_own {
                let mut i = 0;
                while i < self.own_glass.len() {
                    if let Some(own) = &mut self.own_glass[i] {
                        own.offered |= own.frame.glass == g;
                    }
                    i += 1;
                }
            }
        }
    }

    /// The half of [`Self::prepare`] that reads the word engine: bind the composed pane, apply the species, and hand the brain the per-row ink.
    #[inline]
    pub fn feed_ink(&mut self, decos: &mut WordDecorations, pane: Option<(u64, (i32, i32))>) {
        prepare_resident_pet_tick(decos, &mut self.pet, self.species, pane);
    }

    /// The frame's tick, unconditional: `needs_frames()` reads state only the tick advances, so ticking inside the draw gate would freeze it.
    #[inline]
    pub fn tick(&mut self, t: PetTick) -> CompanionFrame {
        self.pet.note_caret_path(t.caret_path);
        let geom = self.geom;
        let frame_px = PixelFrame {
            origin: t.origin,
            cell: (geom.cell_w, geom.cell_h),
            glass: t.glass,
        };
        let offered = std::mem::take(&mut self.resight_offered);
        let mut any_own = false;
        let mut si = 0;
        while si < self.own_glass.len() {
            let slot = &mut self.own_glass[si];
            si += 1;
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
            } else {
                any_own = true;
            }
        }
        if let Some(drawn) = self.resight_frame.take() {
            if drawn.cell != frame_px.cell {
                self.pet.forget_resight();
            } else if drawn.origin == frame_px.origin {
                // The same glass, a new surface on it.
            } else if offered && drawn.glass.origin != frame_px.glass.origin {
                let mut free = None;
                let mut i = 0;
                while i < self.own_glass.len() {
                    if self.own_glass[i].is_none() {
                        free = Some(i);
                        break;
                    }
                    i += 1;
                }
                if let Some(free) = free {
                    self.glass_key = self.glass_key.wrapping_add(1).max(1);
                    self.pet.displace_resight(self.glass_key);
                    self.own_glass[free] = Some(OwnGlass {
                        key: self.glass_key,
                        frame: drawn,
                        offered: false,
                    });
                    any_own = true;
                } else {
                    self.pet.forget_resight();
                }
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
        self.last_shed_tick = if t.shed_active {
            Some(t.sense.now)
        } else {
            None
        };
        let on_glass = t.visible && pet_frame.alpha > 0;
        let kitty_alpha = if !t.pet_mode {
            shed_companion_alpha(t.sing.flying_alpha, t.shed_envelope)
        } else {
            0
        };
        let duty = cursor_companion_duty(on_glass, kitty_alpha, t.painted_caret);
        let body_px = if on_glass {
            pet_frame.body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows)
        } else {
            None
        };
        self.hit_rect = pet_hit_rect_win(body_px, t.origin);
        self.last_alpha = if on_glass { pet_frame.alpha } else { 0 };
        self.drawn_frame = if on_glass { Some(frame_px) } else { None };
        self.last_visible = t.visible;
        let mut glass = [None; crate::kitty_pet::PET_DEPARTURES_MAX];
        if any_own {
            let mut si = 0;
            while si < self.own_glass.len() {
                if let Some(own) = &self.own_glass[si]
                    && !self.pet.has_ghost_on_glass(own.key)
                {
                    self.own_glass[si] = None;
                }
                si += 1;
            }
            let mut di = 0;
            while di < crate::kitty_pet::PET_DEPARTURES_MAX {
                if let Some(d) = &pet_frame.departures[di]
                    && d.own_glass
                {
                    let key = d.glass;
                    let mut si = 0;
                    while si < self.own_glass.len() {
                        if let Some(own) = &self.own_glass[si]
                            && own.key == key
                        {
                            glass[di] = Some(own.frame.glass);
                            break;
                        }
                        si += 1;
                    }
                }
                di += 1;
            }
        }
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

    /// [`Self::prepare`] then [`Self::tick`], for a frame with nothing to do between them.
    #[cfg(test)]
    pub fn sense(&mut self, f: PetFacts<'_>, decos: &mut WordDecorations) -> CompanionFrame {
        let t = self.prepare(f, decos);
        self.tick(t)
    }

    /// Word-cat bat: forward the word engine's peek landings to the brain as the landed head's centre in fractional cells.
    #[inline]
    pub fn note_peeks(
        &mut self,
        now: Instant,
        cues: std::vec::Drain<'_, PeekCue>,
        cell: (u16, u16),
    ) {
        if !self.last_visible || cues.len() == 0 {
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

    /// Dress and draw the pet, committing the ceremony where it renders: dress through [`pet_arrival_for_sync`] + `sync_look`, then draw the pair actually worn into `free` with colours frozen per appearance (else sampled under the body by [`cursor_cat_color_key`] with [`ContrastFallback`]).
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

    /// [`Self::emit`] with the live lane drawn into `free` (the focused surface's channel) and each ghost on its own glass into `window` (`None`: `free` is the window's), in window-grid pixels at its pane, cropped to it, beneath its ink.
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
            self.last_sync = None;
            return (
                0,
                SyncLookOutcome {
                    worn: match self.pet.worn_pair() {
                        Some(w) => w,
                        None => pair,
                    },
                    parked: false,
                },
            );
        }
        let arrival = pet_arrival_for_sync(rung, self.arrival, self.pet.worn_pair(), pair.0);
        let outcome = self.pet.sync_look(pair, arrival);
        self.last_sync = Some((pair, arrival));
        let (coat, iris) = outcome.worn;
        let geom = self.geom;
        let colors = match self.pet.appearance_colors() {
            Some(c) => c,
            None => {
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
            }
        };
        let mut fp = 0;
        let frame = PetCursorFrame {
            geom,
            colors,
            coat,
            iris,
            pet: t.pet,
            page: if crate::color_math::relative_luminance_with(
                crate::color_math::relative_luminance_table(),
                fallback.bg,
            ) < 0.22
            {
                None
            } else {
                Some(fallback.bg)
            },
        };
        if let Some(pet_fp) = decos.pet_cursor(frame, free) {
            fp ^= pet_fp.rotate_left(29);
            let out = match window {
                Some(window) => window,
                None => free,
            };
            let mut slot = 0;
            while slot < t.glass.len() {
                if let Some(glass) = &t.glass[slot] {
                    let own = EffectGeom {
                        cols: glass.cols,
                        rows: glass.rows,
                        ..geom
                    };
                    if let Some(ghost_fp) = decos.pet_own_glass(frame, own, glass.origin, slot, out)
                    {
                        fp ^= ghost_fp.rotate_left(37);
                    }
                }
                slot += 1;
            }
        }
        (fp, outcome)
    }

    /// Whether a hello was spent, for the host's tenure bookkeeping: a drawn present (not a capture), the last `emit` synced a `Ceremony`, and the pair in hand differs from the pair on glass.
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
    pub const fn note_bell(&mut self, now: Instant) {
        self.pet.note_bell(now);
    }

    /// The hand found its flow (`CursorGlow::take_flow_entry`), forwarded between [`Self::prepare`] and [`Self::tick`].
    #[inline]
    pub const fn note_flow(&mut self, now: Instant) {
        self.pet.note_flow(now);
    }

    /// Rainbow kitty v2's offer to the resident, after [`Self::tick`]: at most one star comes back on the paw's landing frame for `CursorGlow::catch_star`.
    #[inline]
    pub fn note_v2_offer(
        &mut self,
        offer: &crate::rainbow_kitty::companion::PetOffer,
    ) -> Option<crate::rainbow_kitty::companion::StarCatch> {
        self.pet.note_v2_offer(offer)
    }

    /// A kitty command was typed at this pet (`sit`, `kitty jump`), as `crate::typed_tricks::TrickListener` reported.
    #[inline]
    pub fn note_trick(&mut self, now: Instant, trick: aterm_lexicon::Trick, confirmed: bool) {
        self.pet.note_trick(now, trick, confirmed);
    }

    /// The typed line turned into prose, or was aborted: take back a tentative kitty command not yet performed.
    #[inline]
    pub const fn revoke_trick(&mut self) {
        self.pet.revoke_trick();
    }

    /// The whole submitted line was pet talk (`sit` + Enter at a shell): the brain does not grieve the fast `command not found` that follows.
    #[inline]
    pub const fn note_trick_submit(&mut self, now: Instant) {
        self.pet.note_trick_submit(now);
    }

    /// One accepted console input intent.
    #[inline]
    pub fn note_console_input(&mut self, now: Instant, kind: PetInputKind) {
        self.pet.note_console_input(now, kind);
    }

    /// Accepted input intentions observed by this brain; reading does not tick.
    #[must_use]
    #[inline]
    pub const fn console_input_seq(&self) -> u64 {
        self.pet.console_input_seq()
    }

    /// Most recent classified input intention; no display-edit claim is implied.
    #[must_use]
    #[inline]
    pub const fn console_input_kind(&self) -> Option<PetInputKind> {
        self.pet.console_input_kind()
    }

    /// A left press at `px` (frame px) inside this frame's drawn body (padded by [`PET_HIT_SLOP_PX`]) strokes the cat and is consumed, so it never starts a selection or reaches a mouse-tracking app.
    #[inline]
    pub const fn press(&mut self, now: Instant, px: (f32, f32)) -> PressOutcome {
        let Some(rect) = self.hit_rect else {
            return PressOutcome::Pass;
        };
        if !pet_rect_hit(rect, px.0 as f64, px.1 as f64, PET_HIT_SLOP_PX) {
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

    /// Retire the resident's surface-relative state (the brain's coordinates and the hit rect) at a presentability boundary.
    #[inline]
    pub fn retire_coordinate_space(&mut self) {
        self.pet.retire_coordinate_space();
        self.clear_surface_state();
    }

    /// The same surface, re-gridded (a resize, a font step, a DPI change, a divider drag): the body keeps its place, alpha and pose; only the facts measured against the old grid and the hit rect go. The brain's next tick measures its station on the new grid: a body on a station only the art's width moved stays on it, its anchor where it was (`PetBrain::keep_place_on_restation`); a station the new grid moves otherwise (the wall's law's other side as columns come or go, a clamp at a moved grid edge, a reflowed caret) is the walk's or the leap's to reach.
    #[inline]
    pub fn rebase_geometry(&mut self) {
        self.pet.rebase_geometry();
        self.hit_rect = None;
        self.latches.finished_caret = None;
        self.latches.hidden_rest = None;
    }

    /// The surface was replaced under a body that may be on glass (another terminal, or the other screen): everything [`Self::retire_coordinate_space`] retires, but a body the last tick drew is kept for the next tick to crossfade from, in the pixel frame it was drawn in.
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
        self.resight_offered = false;
        self.clear_own_glass_offers();
        self.hit_rect = None;
        self.last_alpha = 0;
        self.last_visible = false;
        self.last_sync = None;
    }

    /// The host withdrew this frame's drawn body (a torn projection, a route that draws no pet), so the petting rect goes with it.
    #[inline]
    pub const fn withdraw_hit_rect(&mut self) {
        self.hit_rect = None;
    }

    /// The terminal under this surface was replaced: everything [`Self::retire_coordinate_space`] retires, and the stream latches re-baseline so the old stream's completion, wrap, and caret state are not replayed.
    #[inline]
    pub fn retire_owner(&mut self) {
        self.retire_coordinate_space();
        self.latches = Latches::default();
    }

    /// [`Self::retire_owner`] with the re-sighting crossfade: what the hosts' owner fences run
    /// for another terminal.
    #[inline]
    pub fn resight_owner(&mut self) {
        self.resight_coordinate_space();
        self.latches = Latches::default();
    }

    /// The same terminal switched between its main and its alternate screen (the hosts' owner
    /// fence): the stream latches re-baseline as for another terminal, so the old screen's
    /// completion, wrap and caret state are not replayed, and the hit rect goes; the body keeps
    /// its place on the glass and grid it is drawn on and travels to the new screen's caret
    /// ([`crate::kitty_pet::PetBrain::rescreen`]). Crossfaded as a surface replaced
    /// ([`Self::resight_owner`]), the ghost faded where the body was while the body came up at
    /// the new caret on the same frame: a person saw the cat jump across the screen.
    #[inline]
    pub fn rescreen_owner(&mut self) {
        if !self.pet.keeps_its_body() {
            self.resight_owner();
            return;
        }
        self.pet.rescreen();
        self.hit_rect = None;
        self.latches = Latches::default();
    }

    /// Drop everything the brain owns (body, motes, departures, cadence debt) because its owner went away.
    #[inline]
    pub fn retire(&mut self) {
        self.pet.retire_unowned();
        self.clear_surface_state();
        self.last_static = false;
        self.last_shed_tick = None;
    }

    /// Serious Mode's drain: a fresh brain.
    #[inline]
    pub fn drain(&mut self) {
        self.pet = self.pet.reborn();
        self.clear_surface_state();
        self.last_static = false;
        self.last_shed_tick = None;
    }

    /// Retire the pet's frozen contrast samples, its ghosts' included (a theme or palette change).
    #[inline]
    pub const fn invalidate_colors(&mut self) {
        self.pet.invalidate_colors();
    }

    /// The cadence gate: [`PetBrain::needs_frames`] (something is moving), never `is_active()` (a cat exists).
    #[inline]
    #[must_use]
    pub const fn needs_frames(&self) -> bool {
        !self.last_static && self.last_shed_tick.is_none() && self.pet.needs_frames()
    }

    /// The instant of a settled resident's next visible step (a breath pixel, a blink, a tail beat), [`PetBrain::next_change_deadline`].
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

    /// The brain's failure droop is on glass or owed ([`PetBrain::grieving`]); the host hushes the glow's caret-jump fanfare meanwhile, reading this between [`Self::prepare`] and [`Self::tick`].
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

    /// The animal the host chose, which every `prepare` applies and every retire keeps; the brain's identity is [`Self::brain`].
    #[inline]
    #[must_use]
    #[cfg(test)]
    pub const fn species(&self) -> PetSpecies {
        self.species
    }

    /// Negative-control test seam: make the pet's letter reader see none ([`PetBrain::set_letters_unseen`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_letters_unseen(&mut self, unseen: bool) {
        self.pet.set_letters_unseen(unseen);
    }

    /// A test's negative control: the pet's rest veil dwells and eases on
    /// ([`PetBrain::set_letter_arrival_dwells`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_letter_arrival_dwells(&mut self, dwells: bool) {
        self.pet.set_letter_arrival_dwells(dwells);
    }

    /// A test's negative control: the pet's rest veil steps on whenever the
    /// last key was an Enter ([`PetBrain::set_letter_step_rearms`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_letter_step_rearms(&mut self, rearms: bool) {
        self.pet.set_letter_step_rearms(rearms);
    }

    /// A test's negative control ([`PetBrain::set_old_paths`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_old_paths(&mut self, old: bool) {
        self.pet.set_old_paths(old);
    }

    /// A test's negative control ([`PetBrain::set_old_path`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_old_path(&mut self, law: crate::kitty_pet::OldPath, old: bool) {
        self.pet.set_old_path(law, old);
    }

    /// Read-only access to the pet brain for host projections.
    #[inline]
    #[must_use]
    pub const fn brain(&self) -> &PetBrain {
        &self.pet
    }
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "companion_law_tests.rs"]
mod law_tests;

#[cfg(test)]
#[path = "companion_owner_tests.rs"]
mod owner_tests;
