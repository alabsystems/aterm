// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Located console attention within the resident brain.

use super::*;
use crate::pet_world::TaskbarProgress;
use crate::pet_world::{
    PetAnchor, PetFill, PetPane, PetReading, PetRect, PetReserve, PetWorld, PetWorldFacts,
    PetWorldStamp,
};
use aterm_core::terminal::BlockState;

const INPUT_HOLD: f32 = 0.65;
const _: () = assert!(TRICK_SETTLE > INPUT_HOLD);
const EDIT_HOLD: f32 = 1.10;
const CONTACT_HOLD: f32 = 0.40;
const RESULT_HOLD: f32 = 1.20;
const REACTION_TTL: f32 = 2.0;
const CLEARANCE: f32 = 0.10;
const CONTACT_MARGIN: f32 = 0.45;
const TYPED_RUN_MAX: usize = 4;
const PERCH_REACH: f32 = 24.0;
const CHEER_STAGE_COLS: f32 = 96.0;
const CHEER_STAGE_TENTHS_UP: u8 = 60;

/// A landing's stage ([`PetBrain::cheer_stage`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CheerStage {
    pub(super) at: Option<(f32, f32)>,
    pub(super) air: CheerAir,
}

const HOME_REACH: f32 = 6.0;
pub(super) const RIDE_ROWS_MAX: usize = 3;
const REST_REACH: f32 = 64.0;
/// How far (columns) the head law slides a stand taken on top of the host's line past the letters
/// over it ([`PetBrain::over_the_hosts_line`]): an escort's reach of the caret's column, the
/// reach the typing replays hold the stand beside the caret to. A cat that already stands past
/// the letters on that row keeps its place, however far the caret is.
const LINE_SLIDE_REACH: f32 = 12.0;
/// How far over its line a stand on top of the host's line keeps its soles: the body centred in
/// the two rows over the line, as [`PetWorld::nearest_clear_perch`] fits every perch, which clears
/// the line by more than [`CLEARANCE`].
const LINE_PERCH_GAP: f32 = (2.0 - ART_ROWS) * 0.5;
const _: () = assert!(LINE_PERCH_GAP > CLEARANCE);

const SEAT_KEPT: f32 = 0.5;
/// The last of the escort's [`SIT_AFTER`] (s) in which a cat off its stand on top of the host's
/// line no longer sets out for it ([`PetBrain::escort_reaches`]).
const ESCORT_TAIL: f32 = 0.5;
/// How far past the words that took its seat a cat walks to rest
/// ([`PetBrain::seat_lost_on_foot`]): room for a counter's next digit or two.
const LOST_SEAT_ROOM: f32 = 2.0;
/// How long the ground on top of the host's line must have held its words before a cat comes to
/// rest there ([`PetBrain::rest_where_typing_walks`]).
const WALK_SEAT_SETTLED: Duration = Duration::from_millis(1000);
/// How long (s) after the writing's last key, or the caret's last beat, a cat on top of the host's
/// line holds there rather than fly to a rest seat on another row ([`PetBrain::pause_hold`]): a
/// pause in the writing shorter than this costs no trip.
const PAUSE_HOLD: f32 = 3.0;
const PAUSE_HOLD_DUR: Duration = Duration::from_millis(3000);
/// How much further (cells, rows counted twice) from the stand than the nearest clean seat a
/// seat the cat holds or rests in may lie and still be kept ([`PetBrain::console_station_on`]).
const SEAT_KEEP_SLACK: f32 = 1.0;

const HIDDEN_HOLD_MAX: f32 = 0.25;

const PERCH_DWELL: f32 = 3.0;

const VETO_DWELL: f32 = 0.25;
const VETO_FADE: f32 = 0.20;
const INV_VETO_FADE: f32 = 1.0 / VETO_FADE;
const VETO_BLIND: f32 = 0.50;
const Z_DWELL: f32 = VETO_DWELL;

#[inline]
pub(super) const fn dur_from_secs_f32(secs: f32) -> Duration {
    Duration::from_micros(((secs as f64) * 1_000_000.0 + 0.5) as u64)
}

const INPUT_HOLD_DUR: Duration = dur_from_secs_f32(INPUT_HOLD);
const EDIT_HOLD_DUR: Duration = dur_from_secs_f32(EDIT_HOLD);
const CONTACT_HOLD_DUR: Duration = dur_from_secs_f32(CONTACT_HOLD);
const CONTACT_RECOVER_DUR: Duration = Duration::from_millis(140);
const RESULT_HOLD_DUR: Duration = dur_from_secs_f32(RESULT_HOLD);
const PROGRESS_HOLD_DUR: Duration = Duration::from_millis(350);
const REACTION_TTL_DUR: Duration = Duration::from_secs(REACTION_TTL as u64);
const PERCH_DWELL_DUR: Duration = dur_from_secs_f32(PERCH_DWELL);
const VETO_DWELL_DUR: Duration = dur_from_secs_f32(VETO_DWELL);
const VETO_BLIND_DUR: Duration = dur_from_secs_f32(VETO_BLIND);
const Z_DWELL_DUR: Duration = dur_from_secs_f32(Z_DWELL);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaretSeat {
    Hold,
    Escort,
    Yield,
}

#[derive(Clone, Copy, Debug)]
struct VisibilityHold {
    shown: bool,
    contrary_since: Option<Instant>,
    blind_since: Option<Instant>,
    cover: f32,
    at: Option<Instant>,
    by_fiat: bool,
}

impl Default for VisibilityHold {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl VisibilityHold {
    const DEFAULT: Self = Self {
        shown: true,
        contrary_since: None,
        blind_since: None,
        cover: 1.0,
        at: None,
        by_fiat: false,
    };

    #[inline]
    fn update(&mut self, now: Instant, verdict: Option<bool>) -> f32 {
        let dt = self.advance(now);
        if self.by_fiat {
            if verdict != Some(true) {
                return self.cover;
            }
            self.by_fiat = false;
            self.shown = true;
            self.cover = 1.0;
            self.contrary_since = None;
            self.blind_since = None;
            return self.cover;
        }
        let observed = match verdict {
            Some(clear) => {
                self.blind_since = None;
                Some(clear)
            }
            None => {
                let since = match self.blind_since {
                    Some(s) => s,
                    None => {
                        self.blind_since = Some(now);
                        now
                    }
                };
                if now.saturating_duration_since(since) >= VETO_BLIND_DUR {
                    Some(false)
                } else {
                    None
                }
            }
        };
        match observed {
            Some(observed) if observed == self.shown => self.contrary_since = None,
            Some(_) => {
                let since = match self.contrary_since {
                    Some(s) => s,
                    None => {
                        self.contrary_since = Some(now);
                        now
                    }
                };
                if now.saturating_duration_since(since) >= VETO_DWELL_DUR {
                    self.shown = !self.shown;
                    self.contrary_since = None;
                }
            }
            None => {}
        }
        self.ramp(dt)
    }

    #[inline]
    fn idle(&mut self, now: Instant) -> f32 {
        self.contrary_since = None;
        self.blind_since = None;
        let dt = self.advance(now);
        self.ramp(dt)
    }

    #[inline]
    fn advance(&mut self, now: Instant) -> f32 {
        let dt = match self.at {
            Some(at) => now
                .saturating_duration_since(at)
                .as_secs_f32()
                .clamp(0.0, 2.0),
            None => 0.0,
        };
        self.at = Some(now);
        dt
    }

    #[inline]
    const fn ramp(&mut self, dt: f32) -> f32 {
        let target = if self.shown { 1.0 } else { 0.0 };
        let step = dt * INV_VETO_FADE;
        self.cover = if self.cover < target {
            (self.cover + step).min(target)
        } else {
            (self.cover - step).max(target)
        };
        self.cover
    }

    #[inline]
    const fn hide_now(&mut self, now: Instant) {
        self.shown = false;
        self.cover = 0.0;
        self.contrary_since = None;
        self.blind_since = None;
        self.at = Some(now);
        self.by_fiat = true;
    }

    #[inline]
    const fn settled(&self) -> bool {
        let target = if self.shown { 1.0 } else { 0.0 };
        self.cover == target
            && self.contrary_since.is_none()
            && !(self.blind_since.is_some() && self.shown)
    }
}

#[derive(Clone, Copy, Debug)]
struct ZHold {
    under: bool,
    clear_since: Option<Instant>,
}

impl Default for ZHold {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

impl ZHold {
    const EMPTY: Self = Self {
        under: false,
        clear_since: None,
    };

    /// A leg dealt in front (`front`, [`PetBrain::front_pass`]) stays over the ink it reaches in
    /// front; one already under stays under it until it is clear for the dwell.
    #[inline]
    fn update(&mut self, now: Instant, ink: bool, front: bool) -> bool {
        if ink {
            self.under |= !front;
            self.clear_since = None;
        } else if self.under {
            let since = match self.clear_since {
                Some(s) => s,
                None => {
                    self.clear_since = Some(now);
                    now
                }
            };
            if now.saturating_duration_since(since) >= Z_DWELL_DUR {
                *self = Self::EMPTY;
            }
        }
        self.under
    }

    #[inline]
    fn deadline(&self) -> Option<Instant> {
        let since = self.clear_since?;
        Some(since + Z_DWELL_DUR)
    }
}

/// What the last [`PetBrain::finish_console_frame`] did to the motes the brain drew; the offer
/// reads it ([`PetBrain::hidden_mote_offer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MoteGate {
    /// The console drew every mote the brain drew.
    Open,
    /// The console dropped every mote, wherever it was: a resident, an incoherent surface, a body
    /// it does not draw.
    Shut,
    /// The console dropped each mote whose box does not clear the ink ([`mote_clears`]).
    Clearance,
}

#[inline]
const fn mote_cell_geom((cell_w, cell_h): (u16, u16)) -> (f32, f32, f32, f32, f32) {
    let min_ch = (if cell_h > 6 { cell_h } else { 6 }) as f32;
    let cw = (if cell_w > 1 { cell_w } else { 1 }) as f32;
    let ch = (if cell_h > 1 { cell_h } else { 1 }) as f32;
    (min_ch, cw, ch, 1.0 / cw, 1.0 / ch)
}

/// Whether the console draws mote `m`: a box a tile's radius around the pixel its centre draws at
/// ([`crate::kitty_pet::mote_box_px`]) clears the ink by [`CLEARANCE`]. Judged at that pixel, the
/// verdict turns only where the mote's drawn box does, at one of its own edges
/// ([`PetBrain::hidden_mote_offer`]).
#[inline]
const fn mote_clears(
    world: &PetWorld,
    m: &PetMoteSprite,
    (min_ch, cw, ch, inv_cw, inv_ch): (f32, f32, f32, f32, f32),
) -> bool {
    let (col, row) = ((m.col * cw).round() * inv_cw, (m.row * ch).round() * inv_ch);
    let radius = min_ch * m.scale + 1.0;
    let rw = radius * inv_cw;
    let rh = radius * inv_ch;
    world.clear(
        PetRect::new(row - rh, col - rw, 2.0 * rh, 2.0 * rw),
        CLEARANCE,
    )
}

/// The most edges [`PetBrain::hidden_mote_offer`] walks before it offers the one it reached.
const HIDDEN_MOTE_EDGES_MAX: u32 = 256;

#[inline]
const fn passing(frame: &PetFrame) -> bool {
    matches!(frame.action, PetAction::Leap)
}

#[inline]
const fn cover_alpha(alpha: u8, cover: f32) -> u8 {
    if alpha == 0 || cover <= 0.0 {
        return 0;
    }
    let scaled = ((alpha as f32) * cover.min(1.0)).round() as u8;
    if scaled > 1 { scaled } else { 1 }
}

/// An admitted user-input intent, never a claim that the PTY displayed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PetInputKind {
    Text,
    Delete,
    Navigate,
    Submit,
    Paste,
    /// A kill chord that moves the caret (`^U`, `^W`): reaches the cursor engines as a kill; the pet reads it as an edit like a delete.
    Kill,
    /// A kill chord that leaves the caret where it is (`^K`).
    KillForward,
}

impl PetInputKind {
    #[inline]
    #[must_use]
    pub const fn parse(kind: &str) -> Option<Self> {
        match kind.as_bytes() {
            b"text" => Some(Self::Text),
            b"delete" => Some(Self::Delete),
            b"navigate" => Some(Self::Navigate),
            b"submit" => Some(Self::Submit),
            b"paste" => Some(Self::Paste),
            b"kill" => Some(Self::Kill),
            b"kill-forward" => Some(Self::KillForward),
            _ => None,
        }
    }
}

/// The source of the resident's current attention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PetAttention {
    #[default]
    Rest,
    Typing,
    Contact,
    Editing,
    Output,
    Progress,
    Reading,
    Result,
    Exploring,
    Yielding,
}

impl PetAttention {
    #[inline]
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Typing => "typing",
            Self::Contact => "contact",
            Self::Editing => "editing",
            Self::Output => "output",
            Self::Progress => "progress",
            Self::Reading => "reading",
            Self::Result => "result",
            Self::Exploring => "exploring",
            Self::Yielding => "yielding",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct InputEpisode {
    seq: u64,
    kind: PetInputKind,
    at: Instant,
    target: Option<(f32, f32)>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PerchTrip {
    pub(super) from: (f32, f32),
    pub(super) to: (f32, f32),
    pub(super) elapsed: f32,
    pub(super) duration: f32,
    pub(super) hop: bool,
}

pub(super) struct ConsoleLife {
    pub(super) world: Option<Box<PetWorld>>,
    pub(super) spare_world: Option<Box<PetWorld>>,
    presentable: bool,
    seq: u64,
    input: Option<InputEpisode>,
    typed_at: Option<Instant>,
    typed_kind: Option<PetInputKind>,
    consumed: u64,
    attention: PetAttention,
    reason: &'static str,
    anchor: Option<PetAnchor>,
    pub(super) target: Option<PetRect>,
    gaze: Option<(f32, f32)>,
    pub(super) resident: bool,
    resident_handoff: bool,
    moving: bool,
    clipped: bool,
    contact_at: Option<Instant>,
    contact_armed: bool,
    completion: Option<(Instant, bool)>,
    result_at: Option<(Instant, bool)>,
    pose: Option<PetGlyphId>,
    still: bool,
    hidden_hold: f32,
    caret_held: bool,
    tick_stamp: Option<PetWorldStamp>,
    repair_until: Option<Instant>,
    pub(super) repair_target: Option<(f32, f32)>,
    progress_key: Option<u64>,
    progress: Option<TaskbarProgress>,
    progress_at: Option<Instant>,
    pub(super) trip: Option<PerchTrip>,
    lift: f32,
    source_seq: u64,
    completion_seq: u64,
    failure_quiet_until: Option<Instant>,
    last_body: Option<PetRect>,
    pub(super) last_seat: Option<(f32, f32)>,
    on_glass: bool,
    hold: VisibilityHold,
    z: ZHold,
    /// What the last frame's console pass did to the brain's motes.
    mote_gate: MoteGate,
    /// The mote slots the last frame's console pass dropped while the brain kept them.
    hidden_motes: [bool; PET_MOTES_MAX],
    /// A test's old schedule: the offer keeps every hidden mote's own edges
    /// ([`PetBrain::console_hid_mote`]).
    #[cfg(test)]
    pub(super) offer_hidden_motes: bool,
    /// A test's exact arms: the brain's deadlines without the host's [`ARM_MIN`] floor
    /// ([`PetBrain::arm_floor`]).
    #[cfg(test)]
    pub(super) exact_arms: bool,
    cursor_home: Option<(u16, u16)>,
    contact_changed: bool,
    observed_seq: u64,
    progress_seq: u64,
    perched_since: Option<Instant>,
    faux_caret: Option<(u16, u16)>,
    pub(super) rest_seat: std::cell::Cell<Option<(f32, f32)>>,
    /// The stand (`(col, foot row)`) [`Self::rest_seat`] was last taken or kept for
    /// ([`PetBrain::console_station_on`]): a stand the caret has since taken to another row, or
    /// a jump along its row, keeps the seat only while it is as near as the nearest clean one.
    pub(super) rest_stand: std::cell::Cell<Option<(f32, f32)>>,
    pub(super) rest_seated: std::cell::Cell<bool>,
    /// When the pause a cat on top of the host's line is holding through ends
    /// ([`PetBrain::pause_hold`]): an edge the console offers, where the rest takes the station.
    pause_hold_until: std::cell::Cell<Option<Instant>>,
    /// Whether the station last given is the escort's stand on top of the host's line
    /// ([`PetBrain::console_station`]): the letter veil a pause laid over the cat there lifts as
    /// the writing walks it on ([`PetBrain::escort_over_the_line`]).
    pub(super) line_escort: std::cell::Cell<bool>,
    row_sig: Vec<u64>,
    row_sig_prev: Vec<u64>,
    /// The host's chrome as this frame's rows carry it ([`rebase_host_chrome`]), when the rows it
    /// was painted on moved: kept between frames so a move allocates nothing.
    chrome_rebased: Vec<PetRect>,
    /// The glass the host blanks ([`PetReserve::blanked`]) as this frame's rows carry it, as
    /// [`Self::chrome_rebased`] carries its chrome: painted by the same pass, over the same rows.
    blanked_rebased: Vec<PetRect>,
    /// This frame's row signatures, read for [`rebase_host_chrome`] only on a frame whose rows
    /// under the host's chrome changed.
    chrome_rows: Vec<u64>,
    /// Per row of [`Self::row_sig`], when its cells last changed ([`PetBrain::rows_settled`]).
    row_changed: Vec<Option<Instant>>,
    /// The frame row [`Self::row_sig`] starts at.
    row_sig_first: usize,
    /// What [`Self::world`] and the row signatures were read from.
    reading: PetReading,
}

impl Default for ConsoleLife {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

/// THE HOST'S CHROME RIDES ITS ROWS. The host paints its chrome into the frame after the pet has
/// looked at it, so the chrome it reserves ([`PetReserve::protected`]: a harness's line over a
/// composer's upper rule, the chips on its lower rule) is where its last pass painted it, over the
/// frame the console read before this one (`prev`, the row signatures of that read). On the frame
/// a program moves the rows (a transcript growing over a composer pushes it down, a draft that
/// wraps grows it), the chrome still lies where the rows were: the console read a host's line
/// over the transcript's last line and none over the composer's moved rule, so a cat that rode
/// down with the composer was under the host's line, and hopped off it to a seat under the
/// footer, and back up once the host's next pass caught up (a window's harness turn: two leaps in
/// 0.65 s).
///
/// The glass that pass blanked ([`PetReserve::blanked`]: the tail of Claude Code's mode row past
/// the words the host laid there) lies where its rows were alike, and rides them the same way.
///
/// Each rect of the chrome goes where the rows it was painted over went: the smallest shift `k`
/// for which its rows, with one row beside them, read now `k` rows on as they read then (a
/// composer's two rules alike, their neighbours tell them apart). A rect whose rows read as they
/// did with a neighbour stays; one whose rows match nowhere stays too, as the host left it.
/// Writes the moved chrome into `out` and answers whether any rect moved; `rows` is scratch for
/// this frame's signatures, read only when the rows under the chrome changed.
#[inline]
fn rebase_host_chrome(
    prev: &[u64],
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
    protected: &[PetRect],
    rows: &mut Vec<u64>,
    out: &mut Vec<PetRect>,
) -> bool {
    let n = prev.len();
    let first = pane.row.min(input.cells.len());
    if n == 0 || protected.is_empty() || input.cells.len() - first < n {
        return false;
    }
    let col_end = pane.col + pane.cols;
    #[inline]
    fn sig_at_row(
        input: &aterm_core::render::RenderInput,
        first: usize,
        col_start: usize,
        col_end: usize,
        r: usize,
    ) -> u64 {
        let row = input.cells[first + r].as_slice();
        let start = col_start.min(row.len());
        let end = col_end.min(row.len());
        row_signature(&row[start..end])
    }
    // The span of map rows a rect covers, and whether its rows and their neighbours read now as
    // they did: the common frame (a spinner turning elsewhere) costs a few rows' signatures.
    #[inline]
    const fn rect_row_span(rect: &PetRect, n: usize) -> Option<(usize, usize)> {
        if !rect.valid() || rect.rows <= 0.0 {
            return None;
        }
        let a = rect.row.floor().max(0.0) as usize;
        let b_ceil = (rect.row + rect.rows).ceil() as usize;
        let b = if b_ceil < n { b_ceil } else { n };
        if a < b { Some((a, b)) } else { None }
    }
    #[inline]
    const fn rows_moved(cur: &[u64], prev: &[u64], a: usize, b: usize, k: isize) -> bool {
        #[inline]
        const fn shifted_row(r: usize, k: isize, n: usize) -> Option<usize> {
            match r.checked_add_signed(k) {
                Some(t) if t < n => Some(t),
                _ => None,
            }
        }
        let n = prev.len();
        let mut r = a;
        while r < b {
            let Some(t) = shifted_row(r, k, n) else {
                return false;
            };
            if cur[t] != prev[r] {
                return false;
            }
            r += 1;
        }
        let above = if a > 0 {
            match shifted_row(a - 1, k, n) {
                Some(t) => cur[t] == prev[a - 1],
                None => false,
            }
        } else {
            false
        };
        let below = b < n
            && match shifted_row(b, k, n) {
                Some(t) => cur[t] == prev[b],
                None => false,
            };
        above || below
    }
    let have_rows = rows.len() == n;
    let mut changed = false;
    let mut pi = 0;
    while pi < protected.len() {
        if let Some((a, b)) = rect_row_span(&protected[pi], n) {
            let r_end = (b + 1).min(n);
            let mut r = a.saturating_sub(1);
            while r < r_end {
                let sig = if have_rows {
                    rows[r]
                } else {
                    sig_at_row(input, first, pane.col, col_end, r)
                };
                if sig != prev[r] {
                    changed = true;
                    break;
                }
                r += 1;
            }
            if changed {
                break;
            }
        }
        pi += 1;
    }
    if !changed {
        return false;
    }
    if !have_rows {
        rows.clear();
        rows.reserve(n);
        let mut ri = 0;
        while ri < n {
            rows.push(sig_at_row(input, first, pane.col, col_end, ri));
            ri += 1;
        }
    }
    let cur = rows.as_slice();
    out.clear();
    let mut any = false;
    let mut pi = 0;
    while pi < protected.len() {
        let rect = &protected[pi];
        pi += 1;
        let shift = if let Some((a, b)) = rect_row_span(rect, n)
            && !rows_moved(cur, prev, a, b, 0)
        {
            let mut found = None;
            let mut k = 1isize;
            let max_k = n as isize;
            while k < max_k {
                if rows_moved(cur, prev, a, b, k) {
                    found = Some(k);
                    break;
                }
                if rows_moved(cur, prev, a, b, -k) {
                    found = Some(-k);
                    break;
                }
                k += 1;
            }
            found
        } else {
            None
        };
        match shift {
            Some(k) => {
                any = true;
                out.push(PetRect {
                    row: rect.row + k as f32,
                    ..*rect
                });
            }
            None => out.push(*rect),
        }
    }
    any
}

/// FNV-1a over the row's scalars, 0 for a blank row and at least 1 otherwise. The step stays
/// spelled out for the Trust lane, as in `aos_momentum::lane`'s `fnv1a`.
#[inline]
const fn row_signature(row: &[aterm_core::terminal::RenderCell]) -> u64 {
    let mut blank = true;
    let mut h = aterm_hash::FNV64_OFFSET;
    let mut i = 0;
    while i < row.len() {
        let ch = row[i].ch;
        blank &= matches!(ch, ' ' | '\0');
        h = (h ^ (ch as u32 as u64)).wrapping_mul(aterm_hash::FNV64_PRIME);
        i += 1;
    }
    if blank {
        0
    } else if h > 1 {
        h
    } else {
        1
    }
}

impl ConsoleLife {
    #[inline]
    #[must_use]
    pub(super) const fn new() -> Self {
        Self {
            world: None,
            spare_world: None,
            presentable: false,
            seq: 0,
            input: None,
            typed_at: None,
            typed_kind: None,
            consumed: 0,
            attention: PetAttention::Rest,
            reason: "",
            anchor: None,
            target: None,
            gaze: None,
            resident: false,
            resident_handoff: false,
            moving: false,
            clipped: false,
            contact_at: None,
            contact_armed: false,
            completion: None,
            result_at: None,
            pose: None,
            still: false,
            hidden_hold: 0.0,
            caret_held: false,
            tick_stamp: None,
            repair_until: None,
            repair_target: None,
            progress_key: None,
            progress: None,
            progress_at: None,
            trip: None,
            lift: 0.0,
            source_seq: 0,
            completion_seq: 0,
            failure_quiet_until: None,
            last_body: None,
            last_seat: None,
            on_glass: false,
            hold: VisibilityHold::DEFAULT,
            z: ZHold::EMPTY,
            mote_gate: MoteGate::Open,
            hidden_motes: [false; PET_MOTES_MAX],
            #[cfg(test)]
            offer_hidden_motes: false,
            #[cfg(test)]
            exact_arms: false,
            cursor_home: None,
            contact_changed: false,
            observed_seq: 0,
            progress_seq: 0,
            perched_since: None,
            faux_caret: None,
            rest_seat: std::cell::Cell::new(None),
            rest_stand: std::cell::Cell::new(None),
            rest_seated: std::cell::Cell::new(false),
            pause_hold_until: std::cell::Cell::new(None),
            line_escort: std::cell::Cell::new(false),
            row_sig: Vec::new(),
            row_sig_prev: Vec::new(),
            chrome_rebased: Vec::new(),
            blanked_rebased: Vec::new(),
            chrome_rows: Vec::new(),
            row_changed: Vec::new(),
            row_sig_first: 0,
            reading: PetReading::new(),
        }
    }

    /// A fresh console life for a retired coordinate space.
    #[inline]
    pub(super) fn retired(&mut self) -> Self {
        let spare_world = match self.world.take() {
            Some(mut world) => {
                world.retire();
                Some(world)
            }
            None => self.spare_world.take(),
        };
        let mut row_sig = core::mem::take(&mut self.row_sig);
        let mut row_sig_prev = core::mem::take(&mut self.row_sig_prev);
        let mut chrome_rebased = core::mem::take(&mut self.chrome_rebased);
        let mut blanked_rebased = core::mem::take(&mut self.blanked_rebased);
        let mut chrome_rows = core::mem::take(&mut self.chrome_rows);
        let mut row_changed = core::mem::take(&mut self.row_changed);
        row_sig.clear();
        row_sig_prev.clear();
        chrome_rebased.clear();
        blanked_rebased.clear();
        chrome_rows.clear();
        row_changed.clear();
        Self {
            spare_world,
            row_sig,
            row_sig_prev,
            chrome_rebased,
            blanked_rebased,
            chrome_rows,
            row_changed,
            ..Self::default()
        }
    }
}

impl PetBrain {
    /// Exactly one note per admitted gesture/bundle.
    #[inline]
    pub fn note_console_input(&mut self, now: Instant, kind: PetInputKind) {
        #[inline]
        const fn caret_f32(caret: Option<(u16, u16)>) -> Option<(f32, f32)> {
            match caret {
                Some((r, c)) => Some((r as f32, c as f32)),
                None => None,
            }
        }
        self.rearm_idle_ears();
        let repair_live = matches!(self.console.repair_until, Some(end) if now < end);
        let erases = matches!(
            kind,
            PetInputKind::Delete | PetInputKind::Kill | PetInputKind::KillForward
        );
        let caret_rc = caret_f32(self.last_caret);
        if erases || (kind == PetInputKind::Text && repair_live) {
            if !repair_live {
                self.console.repair_target = caret_rc;
            }
            self.console.repair_until = Some(now + EDIT_HOLD_DUR);
        } else {
            self.console.repair_until = None;
            self.console.repair_target = None;
        }
        let since_tick = match self.last_now {
            Some(tick) => now.saturating_duration_since(tick).as_secs_f64(),
            None => 0.0,
        };
        // A PASTE OPENS A BURST OF ITS OWN ([`OldPath::PasteOpensItsOwnBurst`]): its echo joins
        // no burst a program's output left behind, seconds settled. Joined to one (a command's
        // lines printed before the prompt), the paste's burst began seconds before it, past
        // [`SETTLE_CAP`]: no coil or eviction waited the paste and its Return out, and the cat
        // stepped off the pasted letters at once for the line's end, came down on the new
        // prompt's row there and flew back (windowed zsh, every delay, r29 K9-5).
        if kind == PetInputKind::Paste && !self.old_path(OldPath::PasteOpensItsOwnBurst) {
            self.burst_from = None;
            self.jump_gap = 0.0;
        }
        self.paste_hold = match kind {
            PetInputKind::Paste => Some(self.clock + since_tick + f64::from(self.paste_settle())),
            // The Return the paste held its burst open for has come: the hold runs on only for
            // its echo ([`HAND_ECHO`]), the Enter's repaint a burst of its own after it
            // ([`OldPath::PasteHoldsForItsReturn`]). Held on to its end, a paste entered 120 ms
            // after it sent the cat to the new prompt 0.9 s after the paste.
            PetInputKind::Submit => match self.paste_hold {
                Some(until) if !self.old_path(OldPath::PasteHoldsForItsReturn) => {
                    Some(until.min(self.clock + since_tick + f64::from(HAND_ECHO)))
                }
                held => held,
            },
            _ => None,
        };
        self.console.seq = self.console.seq.wrapping_add(1).max(1);
        self.console.typed_at = Some(now);
        self.console.typed_kind = Some(kind);
        self.console.input = Some(InputEpisode {
            seq: self.console.seq,
            kind,
            at: now,
            target: caret_rc,
        });
        if matches!(
            kind,
            PetInputKind::Text | PetInputKind::Delete | PetInputKind::Submit
        ) && let Some(trick) = self.pending_trick.as_mut()
        {
            trick.stamped = now;
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_input_seq(&self) -> u64 {
        self.console.seq
    }

    /// The console world the last observation read, as the pet judges a seat by it (`None` before any): what a host proves its exclusions against — a cell the host painted chrome on reads [`crate::pet_world::PetCell::Protected`] here or the pet may sit on it.
    #[inline]
    #[must_use]
    pub const fn console_world(&self) -> Option<&PetWorld> {
        match &self.console.world {
            Some(world) => Some(world),
            None => None,
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_input_consumed_seq(&self) -> u64 {
        self.console.consumed
    }

    #[inline]
    #[must_use]
    pub const fn console_input_kind(&self) -> Option<PetInputKind> {
        match self.console.input {
            Some(e) => Some(e.kind),
            None => None,
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_attention(&self) -> PetAttention {
        self.console.attention
    }

    #[inline]
    #[must_use]
    pub const fn console_reason(&self) -> &'static str {
        if self.console.reason.is_empty() {
            "none"
        } else {
            self.console.reason
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_anchor_id(&self) -> Option<u64> {
        match self.console.anchor {
            Some(a) => Some(a.block_id),
            None => None,
        }
    }

    #[inline]
    #[must_use]
    pub const fn console_event_seq(&self) -> u64 {
        self.console.source_seq
    }

    /// The console carried the body with the page this tick ([`Self::place_console_body_at_home`]).
    #[inline]
    pub(super) fn console_carried(&self) -> bool {
        self.console.reason == "content-ride"
    }

    /// A late frame's span carried the body with the page on one of its steps
    /// ([`PetBrain::live_late_span`]) and the frame found it carried: its reason says so.
    #[inline]
    pub(super) fn name_the_spans_carry(&mut self) {
        if self.console.reason == "quiet" {
            self.console.reason = "content-ride";
        }
    }

    /// The pose the last frame drew (`trail status`'s `pet_pose`): with the still pose a hold
    /// kept on glass, not the brain's own under it, so a sampler times what a person saw
    /// ([`OldPath::NamesTheDrawnPose`]). It named the brain's, and a landing the hold kept on
    /// glass read as on it for 10 ms.
    #[inline]
    #[must_use]
    pub const fn pose_name(&self) -> &'static str {
        if self.old_path(OldPath::NamesTheDrawnPose) {
            return PET_GLYPHS[self.species.skin(self.last_pose) as usize].id;
        }
        PET_GLYPHS[self.drawn_pose as usize].id
    }

    /// Clear occupancy is read from the exact cell plane being presented.
    #[inline]
    pub fn observe_console(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
    ) {
        self.observe_console_with_exclusions(input, facts, pane, &[]);
    }

    /// Cells the last world observation classified (at most `MAX_WORLD_ROWS` × `MAX_WORLD_COLS` per presented frame, and on a pane past that budget `MAX_BODY_ROWS` × `MAX_WORLD_COLS` more about the body, [`crate::pet_world::PetWorld::body_window`]; none when it kept the reading it held), so a running product can report the cost.
    #[inline]
    #[must_use]
    pub const fn observed_cells(&self) -> usize {
        if self.console.reading.kept() {
            return 0;
        }
        match &self.console.world {
            Some(world) => world.examined_cells(),
            None => 0,
        }
    }

    #[inline]
    pub fn observe_console_with_exclusions(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
    ) {
        self.observe_console_with_reserve(
            input,
            facts,
            pane,
            PetReserve::protected(exclusions),
            PetFill::Unconsumed,
        );
    }

    /// As [`Self::observe_console_with_exclusions`], with the host's transient overlays apart (`overlays`: a fact menu's card, the find bar; [`PetWorld::observe_with_overlays`]).
    #[inline]
    pub fn observe_console_with_overlays(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        exclusions: &[PetRect],
        overlays: &[PetRect],
    ) {
        self.observe_console_with_reserve(
            input,
            facts,
            pane,
            PetReserve {
                protected: exclusions,
                overlays,
                ink: &[],
                blanked: &[],
                painted_over: None,
            },
            PetFill::Unconsumed,
        );
    }

    /// The host's late overlays told apart ([`PetReserve`]): its chrome and its transient overlays protected, the person's typed glyphs (the predicted echo, the IME preedit) as ink. Of a [`PetFill::Consumed`] fill, the reading already held of the same cells is kept ([`PetReading`]).
    #[inline]
    pub fn observe_console_with_reserve(
        &mut self,
        input: &aterm_core::render::RenderInput,
        facts: &PetWorldFacts,
        pane: PetPane,
        reserve: PetReserve<'_>,
        fill: PetFill,
    ) {
        let held_center = match (self.console.cursor_home, self.console.last_body) {
            (Some(_), Some(body))
                if !input.cursor_visible
                    && self.console.presentable
                    && self.alpha > 0.0
                    && matches!(
                        self.console.tick_stamp,
                        Some(prior) if facts.stamp.preserves_cursor_home(prior)
                    ) =>
            {
                Some((body.row + body.rows * 0.5, body.col + body.cols * 0.5))
            }
            _ => None,
        };
        // A pane past the map's budget is read about its caret, and the rows about the body
        // (where it was last drawn, [`crate::pet_world::KEPT_ROWS`] over and under it) in a
        // second window about the body's centre column ([`OldPath::WorldReadsTheBodysRows`]).
        let keep = match self.console.last_body {
            Some(body)
                if (pane.rows > crate::pet_world::MAX_WORLD_ROWS
                    || pane.cols > crate::pet_world::MAX_WORLD_COLS)
                    && body.row.is_finite()
                    && body.rows.is_finite()
                    && body.col.is_finite()
                    && body.cols.is_finite()
                    && !self.old_path(OldPath::WorldReadsTheBodysRows) =>
            {
                let kept = crate::pet_world::KEPT_ROWS as f32;
                let first = (body.row - kept).max(0.0).floor() as usize;
                let end = (body.row + body.rows + kept).ceil().max(0.0) as usize;
                let mid = (body.col + body.cols * 0.5).max(0.0) as usize;
                Some((first.min(pane.rows), end.min(pane.rows), mid.min(pane.cols)))
            }
            _ => None,
        };
        // A flight to another row is judged where it lands ([`Self::head_span`]): the cells about
        // its landing (the box as last drawn, set down there, and `KEPT_ROWS` over and under it),
        // which that window reads about where the caret's does not read them all. A flight re-aimed
        // in the air lands where neither the caret nor the body is.
        let landing = match (&self.flight, self.console.last_body) {
            (Some(f), Some(body))
                if keep.is_some()
                    && (f.to_row - f.from_row).abs() >= 0.5
                    && f.to_row.is_finite()
                    && f.to_col.is_finite()
                    && self.col.is_finite() =>
            {
                let kept = crate::pet_world::KEPT_ROWS as f32;
                let (top, col) = (f.to_row + 1.0 - body.rows, f.to_col + (body.col - self.col));
                let first = ((top - kept).max(0.0).floor() as usize).min(pane.rows);
                let end = ((top + body.rows + kept).ceil().max(0.0) as usize).min(pane.rows);
                let c0 = (col.max(0.0).floor() as usize).min(pane.cols);
                let c1 = ((col + body.cols).ceil().max(0.0) as usize).min(pane.cols);
                (end > first && c1 > c0).then_some(PetPane {
                    row: first,
                    col: c0,
                    rows: end - first,
                    cols: c1 - c0,
                })
            }
            _ => None,
        };
        let centre = (held_center, keep, landing);
        // Taking the reading held again ([`PetReading`]) would leave the
        // world, the observation count, the row signatures, the faux caret and
        // the contact as they stand: the same cells give the same world, whose
        // stamp, selection, progress and ink by the body are those it holds.
        if matches!(self.console.world.as_ref(), Some(w) if w.stamp().is_some())
            && self
                .console
                .reading
                .holds((input, fill), facts, pane, reserve, centre)
        {
            self.console.reading.keep(input);
            return;
        }
        if self.console.world.is_none() {
            self.console.world = Some(self.console.spare_world.take().unwrap_or_default());
        }
        // The window about the body reads its cells' ground
        // ([`OldPath::BodysWindowReadsItsGround`]).
        let body_ground = !self.old_path(OldPath::BodysWindowReadsItsGround);
        let Some(world) = &mut self.console.world else {
            return;
        };
        let prior_stamp = world.stamp();
        let prior_selection = world.selection_target();
        let prior_progress = world.progress();
        let typed = typed_run(input, pane, self.last_caret);
        let old_ink = if self.console.contact_changed {
            None
        } else {
            match self.console.last_body {
                Some(r) => local_ink(world, r, typed),
                None => None,
            }
        };
        // THE HOST'S CHROME RIDES ITS ROWS ([`rebase_host_chrome`]): chrome painted over the
        // frame this console read last, which the frame now read has moved on from.
        let painted_over_last = matches!(
            (prior_stamp, reserve.painted_over),
            (Some(p), Some(seq))
                if p.surface == facts.stamp.surface
                    && p.content_seq == seq
                    && facts.stamp.content_seq != seq
        );
        self.console.chrome_rows.clear();
        let rebased = painted_over_last
            && rebase_host_chrome(
                &self.console.row_sig,
                input,
                pane,
                reserve.protected,
                &mut self.console.chrome_rows,
                &mut self.console.chrome_rebased,
            );
        let blanked_rebased = painted_over_last
            && rebase_host_chrome(
                &self.console.row_sig,
                input,
                pane,
                reserve.blanked,
                &mut self.console.chrome_rows,
                &mut self.console.blanked_rebased,
            );
        let seen = PetReserve {
            protected: if rebased {
                &self.console.chrome_rebased
            } else {
                reserve.protected
            },
            blanked: if blanked_rebased {
                &self.console.blanked_rebased
            } else {
                reserve.blanked
            },
            ..reserve
        };
        world.read_the_bodys_ground(body_ground);
        world.observe_with_exclusions_near(input, facts, pane, seen, centre);
        if world.stamp().is_some()
            && (prior_stamp != world.stamp()
                || prior_selection != world.selection_target()
                || prior_progress != world.progress())
        {
            self.console.observed_seq = self.console.observed_seq.wrapping_add(1).max(1);
        }
        let fixed_view = match (prior_stamp, world.stamp()) {
            (Some(a), Some(b)) => {
                a.surface == b.surface
                    && a.top_absolute_row == b.top_absolute_row
                    && a.display_offset == b.display_offset
                    && a.uniform_up_rows == b.uniform_up_rows
            }
            _ => false,
        };
        if fixed_view {
            // Multiple coherent reads may precede one tick. A duplicate
            // observation cannot erase new local ink already observed.
            if let Some(old) = old_ink
                && let Some(r) = self.console.last_body
                && let Some(new) = local_ink(world, r, typed)
            {
                let mut i = 0;
                while i < 8 {
                    if new[i] & !old[i] != 0 {
                        self.console.contact_changed = true;
                        break;
                    }
                    i += 1;
                }
            }
        } else {
            // Moving existing ink through a pane-local mask is not contact
            // from newly displayed text. Reflow/unknown snapshots revoke too.
            self.console.contact_changed = false;
        }
        if matches!(
            (prior_stamp, world.stamp()),
            (Some(a), Some(b)) if a.surface != b.surface
        ) {
            self.console.input = None;
            self.console.consumed = self.console.seq;
            self.console.repair_until = None;
            self.console.repair_target = None;
            self.console.source_seq = 0;
            self.console.completion = None;
            self.console.failure_quiet_until = None;
            self.console.faux_caret = None;
        }
        self.note_row_signatures(input, pane, prior_stamp);
        let prior_faux = if matches!(
            (prior_stamp, self.console.world.as_deref().and_then(PetWorld::stamp)),
            (Some(a), Some(b)) if a.surface == b.surface
        ) {
            self.console.faux_caret
        } else {
            None
        };
        self.console.faux_caret = if !input.cursor_visible
            && input.display_offset == 0
            && input.cursor_row + 2 >= pane.row + pane.rows
        {
            match detect_faux_composer_caret_with_prior(input, pane, prior_faux) {
                Some((r, c)) if pane.row + usize::from(r) < input.cursor_row => Some((r, c)),
                _ => None,
            }
        } else {
            None
        };
        match self.console.world.as_deref() {
            Some(w) if w.stamp().is_some() => {
                let read = (w.coverage(), w.body_window());
                self.console
                    .reading
                    .note((input, fill), facts, pane, reserve, held_center, read);
            }
            _ => self.console.reading.forget(),
        }
    }

    #[inline]
    fn note_row_signatures(
        &mut self,
        input: &aterm_core::render::RenderInput,
        pane: PetPane,
        prior: Option<PetWorldStamp>,
    ) {
        let now = self.last_now;
        let console = &mut self.console;
        let Some(stamp) = (match console.world.as_ref() {
            Some(w) => w.stamp(),
            None => None,
        }) else {
            console.row_sig.clear();
            console.row_sig_prev.clear();
            console.row_changed.clear();
            return;
        };
        let first = pane.row.min(input.cells.len());
        let rows = pane.rows.min(input.cells.len() - first);
        let col_end = pane.col + pane.cols;
        let same_grid = matches!(prior, Some(p) if p.surface == stamp.surface)
            && console.row_sig.len() == rows
            && console.row_sig_first == first;
        console.row_sig_first = first;
        let slice = &input.cells[first..first + rows];
        // A grid read afresh is all new at once; read before the first tick, its rows take the
        // first time known.
        if let Some(now) = now
            && matches!(console.row_changed.first(), Some(None))
        {
            console.row_changed.fill(Some(now));
        }
        if !same_grid {
            console.row_sig_prev.clear();
            console.row_sig.clear();
            console.row_changed.clear();
            console.row_changed.resize(rows, now);
            console.row_sig.reserve(rows);
            let mut i = 0;
            while i < slice.len() {
                let row_s = slice[i].as_slice();
                let start = pane.col.min(row_s.len());
                let end = col_end.min(row_s.len());
                console.row_sig.push(row_signature(&row_s[start..end]));
                i += 1;
            }
            return;
        }
        let have_chrome_rows = console.chrome_rows.len() == rows;
        let mut swapped = false;
        let mut i = 0;
        while i < slice.len() {
            let sig = if have_chrome_rows {
                console.chrome_rows[i]
            } else {
                let row_s = slice[i].as_slice();
                let start = pane.col.min(row_s.len());
                let end = col_end.min(row_s.len());
                row_signature(&row_s[start..end])
            };
            if !swapped {
                if console.row_sig[i] == sig {
                    i += 1;
                    continue;
                }
                std::mem::swap(&mut console.row_sig_prev, &mut console.row_sig);
                console.row_sig.resize(rows, 0);
                console.row_sig[..i].copy_from_slice(&console.row_sig_prev[..i]);
                swapped = true;
            }
            if console.row_sig_prev[i] != sig
                && let Some(changed) = console.row_changed.get_mut(i)
            {
                *changed = now;
            }
            console.row_sig[i] = sig;
            i += 1;
        }
    }

    /// Whether the map rows `rect` takes in `world` have held their cells for `settled` (a
    /// spinner's line, a reply streaming in, a screen just drawn have not).
    #[inline]
    fn rows_settled(&self, world: &PetWorld, rect: PetRect, settled: Duration) -> bool {
        let Some(now) = self.last_now else {
            return false;
        };
        let top = world.coverage().row as f32;
        let first = self.console.row_sig_first as f32;
        let (a, b) = (
            (rect.row + top - first).floor().max(0.0) as usize,
            (rect.row + rect.rows + top - first).ceil().max(0.0) as usize,
        );
        let Some(rows) = self.console.row_changed.get(a..b) else {
            return false;
        };
        let mut i = 0;
        while i < rows.len() {
            let Some(at) = rows[i] else {
                return false;
            };
            if now.saturating_duration_since(at) < settled {
                return false;
            }
            i += 1;
        }
        true
    }

    #[inline]
    pub(super) const fn content_ride(&self, rect: PetRect) -> Option<f32> {
        let (cur, prev) = (
            self.console.row_sig.as_slice(),
            self.console.row_sig_prev.as_slice(),
        );
        if prev.is_empty() || prev.len() != cur.len() || !rect.valid() {
            return None;
        }
        let top_i = rect.row.floor() as i64 - 1;
        let top = (if top_i > 0 { top_i } else { 0 }) as usize;
        let bot_i = (rect.row + rect.rows).ceil() as i64 - 1;
        let bottom = (if bot_i > 0 { bot_i } else { 0 }) as usize;
        if bottom + 1 >= prev.len() {
            return None;
        }
        let len = bottom - top + 1;
        let mut same = true;
        let mut has_nonzero = false;
        let mut i = 0;
        while i < len {
            let p = prev[top + i];
            if p != cur[top + i] {
                same = false;
            }
            if p != 0 {
                has_nonzero = true;
            }
            i += 1;
        }
        if same || !has_nonzero {
            return None;
        }
        let mut k = 1;
        while k <= RIDE_ROWS_MAX {
            if bottom + k < cur.len() {
                let mut matches = true;
                let mut j = 0;
                while j < len {
                    if prev[top + j] != cur[top + k + j] {
                        matches = false;
                        break;
                    }
                    j += 1;
                }
                if matches {
                    return Some(k as f32);
                }
            }
            k += 1;
        }
        None
    }

    /// THE HOST'S LINE CARRIES THE CAT UP. A body stood on top of the host's line
    /// ([`Self::over_the_hosts_line`]) that the line came up onto (a composer at the foot of the
    /// pane grown a row as its draft wraps, the rows over it scrolled up with it) rides up with
    /// it: `Some(-k)` where the rows from over the body down to the one under its feet all moved
    /// up `k` rows together and that one is still the host's line. A line that moved alone, or
    /// rows that came up without one under the feet, carry nothing: the cat steps aside as before.
    #[inline]
    const fn line_ride(&self, world: &PetWorld, rect: PetRect) -> Option<f32> {
        let (cur, prev) = (
            self.console.row_sig.as_slice(),
            self.console.row_sig_prev.as_slice(),
        );
        if prev.is_empty() || prev.len() != cur.len() || !rect.valid() {
            return None;
        }
        let top_i = rect.row.floor() as i64 - 1;
        let top = (if top_i > 0 { top_i } else { 0 }) as usize;
        let ground_i = (rect.row + rect.rows).ceil() as i64;
        let ground = (if ground_i > 1 { ground_i } else { 1 }) as usize;
        if top == 0 || ground >= prev.len() {
            return None;
        }
        let len = ground - top + 1;
        let mut same = true;
        let mut has_nonzero = false;
        let mut i = 0;
        while i < len {
            let p = prev[top + i];
            if p != cur[top + i] {
                same = false;
            }
            if p != 0 {
                has_nonzero = true;
            }
            i += 1;
        }
        if same || !has_nonzero {
            return None;
        }
        let mut k = 1;
        while k <= RIDE_ROWS_MAX {
            if top >= k && matches!(world.host_line(ground - k), Some(true)) {
                let mut matches = true;
                let mut j = 0;
                while j < len {
                    if prev[top + j] != cur[top - k + j] {
                        matches = false;
                        break;
                    }
                    j += 1;
                }
                if matches {
                    return Some(-(k as f32));
                }
            }
            k += 1;
        }
        None
    }

    /// Whether a command the console watches run (shell integration's executing block) has
    /// its last letters on `row`: a carriage return and line feed off that row is the
    /// command's own output, not an Enter's echo ([`PetBrain::line_returned`]). An Enter's
    /// echo leaves the command line, which no block's output holds.
    #[inline]
    pub(super) const fn console_output_ends_on(&self, row: u16) -> bool {
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        let anchors = world.anchors();
        let mut i = 0;
        while i < anchors.len() {
            if let Some(a) = &anchors[i]
                && matches!(a.state, BlockState::Executing)
                && a.col > 0.0
                && (a.row - row as f32).abs() < 0.5
            {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Whether the console's last observation holds a command still running
    /// (shell integration's executing block).
    #[inline]
    pub(super) const fn console_executing(&self) -> bool {
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        let anchors = world.anchors();
        let mut i = 0;
        while i < anchors.len() {
            if let Some(a) = &anchors[i]
                && matches!(a.state, BlockState::Executing)
            {
                return true;
            }
            i += 1;
        }
        false
    }

    /// The last observed letterform ([`crate::pet_world::PetWorld::letter_at`])
    /// among the cells `[a, b)` of `row`: `Some(Some(col))`, `Some(None)` where
    /// the map certifies none, and `None` where it covers none of them. An
    /// unobserved cell inside a covered run reads as no letter: the law that
    /// asks keeps its body off what it saw, never off what it guessed.
    #[inline]
    pub(super) const fn observed_last_letter(
        &self,
        row: f32,
        a: f32,
        b: f32,
    ) -> Option<Option<usize>> {
        if !self.console.presentable
            || !row.is_finite()
            || row < 0.0
            || !a.is_finite()
            || !b.is_finite()
            || b <= a
        {
            return None;
        }
        #[cfg(any(test, debug_assertions))]
        if self.letters_unseen {
            return None;
        }
        let Some(world) = self.console.world.as_ref() else {
            return None;
        };
        let (first, end) = (a.max(0.0).floor() as usize, b.ceil() as usize);
        if end <= first || end - first > crate::pet_world::MAX_WORLD_COLS {
            return None;
        }
        let r = row.round() as usize;
        world.last_letter_in_span(r, first, end)
    }

    /// Whether the console places and lifts the body itself: its resident, on a presentable
    /// console ([`Self::console_frame_pass`] takes the frame's lift), where the sink of the
    /// head law ([`PetBrain::head_target`]) is the console's own map of what the body may cover.
    #[inline]
    pub(super) const fn console_places_body(&self) -> bool {
        self.console.presentable && self.console.resident
    }

    /// Whether any cell `[a, b)` of `row` holds a glyph (`None` where the map covers none or
    /// misses one of them): the whole span, no [`INK_PAD`] off its ends.
    #[inline]
    pub(super) const fn observed_ink_in_span(&self, row: f32, a: f32, b: f32) -> Option<bool> {
        if !self.console.presentable
            || !row.is_finite()
            || row < 0.0
            || !a.is_finite()
            || !b.is_finite()
            || b <= a
        {
            return None;
        }
        let Some(world) = self.console.world.as_ref() else {
            return None;
        };
        let (first, end) = (a.max(0.0).floor() as usize, b.ceil() as usize);
        if end <= first || end - first > crate::pet_world::MAX_WORLD_COLS {
            return None;
        }
        world.locomotion_ink_in_span(row.round() as usize, first, end)
    }

    /// The columns `a..b` of `row` a read along the body's row takes
    /// ([`Self::observed_ink_in_span`], [`Self::observed_last_letter`]): `a..b` where the map
    /// reads them whole, else as far either way from the columns `core` (the body's, among
    /// them) as the window that reads those reaches
    /// ([`crate::pet_world::PetWorld::span_read_about`]). On a pane past the map's columns, a
    /// body setting off along its row, or bounding along it, is read over its way that far, not
    /// over none of it (ATERM-KI-1107-3). `a..b` as they are where neither window reads `core` on
    /// `row`, where they run past the pane's last column, and with
    /// [`OldPath::WorldReadsTheBodysRows`] reverted.
    #[inline]
    pub(super) const fn read_about(
        &self,
        row: f32,
        (a, b): (f32, f32),
        core: (f32, f32),
    ) -> (f32, f32) {
        if self.old_path(OldPath::WorldReadsTheBodysRows)
            || !self.console.presentable
            || !row.is_finite()
            || row < 0.0
            || !a.is_finite()
            || !b.is_finite()
            || b <= a
            || !core.0.is_finite()
            || !core.1.is_finite()
        {
            return (a, b);
        }
        let Some(world) = self.console.world.as_ref() else {
            return (a, b);
        };
        let (c0, c1) = (core.0.max(a), core.1.min(b));
        if c1 <= c0 {
            return (a, b);
        }
        let (first, end) = (a.max(0.0).floor() as usize, b.ceil() as usize);
        let core = (c0.max(0.0).floor() as usize, c1.ceil() as usize);
        match world.span_read_about(row.round() as usize, (first, end), core) {
            Some((n0, n1)) if n0 > first || n1 < end => (a.max(n0 as f32), b.min(n1 as f32)),
            _ => (a, b),
        }
    }

    /// Exact locomotion occupancy where the map covers the whole footprint.
    #[inline]
    pub(super) const fn observed_ink_overlaps(
        &self,
        col: f32,
        row: f32,
        width: f32,
    ) -> Option<bool> {
        let (a, b) = (col + INK_PAD, col + width - INK_PAD);
        if self.console.presentable
            && let Some(world) = self.console.world.as_ref()
            && row.is_finite()
            && row >= 0.0
            && a.is_finite()
            && a >= 0.0
            && b.is_finite()
            && b > a
        {
            let first = a.floor() as usize;
            let end = b.ceil() as usize;
            if end > first && end - first <= crate::pet_world::MAX_WORLD_COLS {
                let r = row.round() as usize;
                return world.locomotion_ink_in_span(r, first, end);
            }
        }
        None
    }

    /// An observed but incoherent surface cannot license new locomotion from the older ink spans.
    #[inline]
    pub(super) const fn console_observation_incoherent(&self) -> bool {
        self.console.presentable
            && matches!(&self.console.world, Some(world) if world.stamp().is_none())
    }

    /// Ownership/custody is independent of whether a live caret exists.
    #[inline]
    pub const fn set_console_presentable(&mut self, presentable: bool) {
        if self.console.presentable != presentable {
            self.console.hold = VisibilityHold::DEFAULT;
            self.console.z = ZHold::EMPTY;
            self.console.rest_seat = std::cell::Cell::new(None);
            self.console.rest_seated = std::cell::Cell::new(false);
        }
        self.console.presentable = presentable;
    }

    #[inline]
    #[must_use]
    pub const fn has_reading_interest(&self) -> bool {
        matches!(&self.console.world, Some(w) if w.selection_target().is_some())
    }

    #[inline]
    pub(super) const fn console_body(&self, sense: PetSense) -> PetRect {
        still_body_rect(
            (self.col, self.row),
            Stance::NEUTRAL,
            (sense.cell_w, sense.cell_h),
            (sense.cols, sense.rows),
        )
    }

    /// A LANDING'S STAGE (§11.3 of `docs/DESIGN-harness-autopilot-2026-10-15.md`): ground where the cat can cheer with its ♪/♥ legible — its body, and the air the three of them fill side by side in their lanes on the side it faces ([`PetBrain::cheer_air`]), clear of ink, the caret's ring and anything protected: [`PetWorld::clear`], the law every mote on glass is held to (`finish_console_frame`).
    #[inline]
    pub(super) const fn cheer_stage(
        &self,
        sense: PetSense,
        width: f32,
        prefer: Option<(f32, f32)>,
    ) -> Option<CheerStage> {
        #[derive(Clone, Copy)]
        struct CheerStageGeom {
            cw_f: f32,
            ch_f: f32,
            inv_cw: f32,
            inv_ch: f32,
            half_nat_w_minus_half_dest_w: i32,
            dest_h: i32,
            max_x: i32,
            max_y: i32,
            body_rows: f32,
            body_cols: f32,
            rh: f32,
            rw: f32,
            step: f32,
            width: f32,
        }

        impl CheerStageGeom {
            #[inline]
            const fn new(sense: PetSense, width: f32) -> Self {
                let cw = if sense.cell_w > 1 { sense.cell_w } else { 1 };
                let ch = if sense.cell_h > 1 { sense.cell_h } else { 1 };
                let cw_f = cw as f32;
                let ch_f = ch as f32;
                let inv_cw = 1.0 / cw_f;
                let inv_ch = 1.0 / ch_f;
                let nat_h = (ART_ROWS * ch_f).round();
                let nat_w = (nat_h * ART_ASPECT).round();
                let nat_h = clamp_i32(nat_h as i32, 1, u16::MAX as i32);
                let nat_w = clamp_i32(nat_w as i32, 1, u16::MAX as i32);
                let dest_w = nat_w;
                let dest_h = nat_h;
                let grid_w = (sense.cols as i32).saturating_mul(cw as i32);
                let grid_h = (sense.rows as i32).saturating_mul(ch as i32);
                let max_x = if grid_w > dest_w { grid_w - dest_w } else { 0 };
                let max_y = if grid_h > dest_h { grid_h - dest_h } else { 0 };
                let ch6 = if sense.cell_h > 6 { sense.cell_h } else { 6 };
                let reach_px = (ch6 as f32) * NOTE_SCALE_MAX + 1.0;
                Self {
                    cw_f,
                    ch_f,
                    inv_cw,
                    inv_ch,
                    half_nat_w_minus_half_dest_w: nat_w / 2 - dest_w / 2,
                    dest_h,
                    max_x,
                    max_y,
                    body_rows: dest_h as f32 * inv_ch,
                    body_cols: dest_w as f32 * inv_cw,
                    rh: reach_px * inv_ch,
                    rw: reach_px * inv_cw,
                    step: note_lane_step(sense.cell_w, sense.cell_h),
                    width,
                }
            }

            #[inline]
            const fn stage_at(
                &self,
                brain: &PetBrain,
                air: CheerAir,
                (col, row): (f32, f32),
            ) -> PetRect {
                let x0 = clamp_i32(
                    (col * self.cw_f).round() as i32 + self.half_nat_w_minus_half_dest_w,
                    0,
                    self.max_x,
                );
                let y0 = clamp_i32(
                    ((row + 1.0) * self.ch_f).round() as i32 - self.dest_h,
                    0,
                    self.max_y,
                );
                let body = PetRect::new(
                    y0 as f32 * self.inv_ch,
                    x0 as f32 * self.inv_cw,
                    self.body_rows,
                    self.body_cols,
                );
                let (left, top, right, bottom) =
                    brain.cheer_air((col, row), (self.rh, self.rw, self.step), self.width, air);
                let (row0, col0) = (top.min(body.row), left.min(body.col));
                PetRect::new(
                    row0,
                    col0,
                    bottom.max(body.row + body.rows) - row0,
                    right.max(body.col + body.cols) - col0,
                )
            }
        }

        #[inline]
        const fn reachable(
            brain: &PetBrain,
            world: &PetWorld,
            geom: &CheerStageGeom,
            from: Option<PetRect>,
            air: CheerAir,
            (dc, dr): (f32, f32),
        ) -> bool {
            let stage = geom.stage_at(brain, air, (brain.col + dc, brain.row + dr));
            world.clear(stage, CLEARANCE)
                && match from {
                    None => true,
                    Some(from) => world.under_text_corridor_clear_past_caret(
                        from,
                        PetRect::new(from.row + dr, from.col + dc, from.rows, from.cols),
                        CLEARANCE,
                    ),
                }
        }

        if !self.console.presentable {
            return None;
        }
        let Some(world) = self.console.world.as_ref() else {
            return None;
        };
        if world.stamp().is_none() {
            return None;
        }
        let body = self.console_body(sense);
        let from = if !matches!(
            world.under_text_clearance_past_caret(body, CLEARANCE),
            Some(false)
        ) {
            Some(body)
        } else {
            None
        };
        let geom = CheerStageGeom::new(sense, width);
        let half_px = 0.5 * geom.inv_cw;
        let tiers = [
            [
                Some(CheerAir::Lanes(self.facing_left)),
                Some(CheerAir::Lanes(!self.facing_left)),
            ],
            [Some(CheerAir::Compact), None],
        ];
        let mut ti = 0;
        while ti < tiers.len() {
            let tier = tiers[ti];
            ti += 1;
            let mut stages = [None; 2];
            let mut si = 0;
            while si < 2 {
                if let Some(air) = tier[si] {
                    let stage = geom.stage_at(self, air, (self.col, self.row));
                    let center = stage.col - CLEARANCE - half_px;
                    let c_lo_raw = (center - CHEER_STAGE_COLS).floor().max(0.0) as u16;
                    let c_lo = if c_lo_raw < sense.cols {
                        c_lo_raw
                    } else {
                        sense.cols
                    };
                    let c_hi_raw =
                        ((center + CHEER_STAGE_COLS).ceil().max(0.0) as u16).saturating_add(1);
                    let c_hi = if c_hi_raw < sense.cols {
                        c_hi_raw
                    } else {
                        sense.cols
                    };
                    stages[si] = Some((air, stage, c_lo, c_hi));
                }
                si += 1;
            }
            let mut si = 0;
            while si < 2 {
                if let Some((air, stage, _, _)) = stages[si]
                    && world.clear(stage, CLEARANCE)
                {
                    return Some(CheerStage { at: None, air });
                }
                si += 1;
            }
            if let Some((col, row)) = prefer {
                let mut si = 0;
                while si < 2 {
                    if let Some((air, _, _, _)) = stages[si]
                        && reachable(
                            self,
                            world,
                            &geom,
                            from,
                            air,
                            (col - self.col, row - self.row),
                        )
                    {
                        return Some(CheerStage {
                            at: Some((col, row)),
                            air,
                        });
                    }
                    si += 1;
                }
            }
            let mut best: Option<(f32, (f32, f32), CheerAir)> = None;
            let dirs = [-1.0f32, 1.0f32];
            let mut di = 0;
            while di < 2 {
                let dir = dirs[di];
                di += 1;
                let mut step = 0u8;
                while step <= CHEER_STAGE_TENTHS_UP {
                    if dir > 0.0 && step == 0 {
                        step += 1;
                        continue;
                    }
                    let dr = dir * 0.1 * (step as f32);
                    let row_dist = 2.0 * dr.abs();
                    if row_dist > CHEER_STAGE_COLS
                        || matches!(best, Some((d, _, _)) if row_dist >= d)
                    {
                        break;
                    }
                    let mut si = 0;
                    while si < 2 {
                        if let Some((air, stage, c_lo, c_hi)) = stages[si] {
                            let base_col = (CLEARANCE + half_px) - stage.col;
                            let mut c = c_lo;
                            while c < c_hi {
                                let dc = (c as f32) + base_col;
                                let distance = row_dist.max(dc.abs());
                                if distance > CHEER_STAGE_COLS
                                    || matches!(best, Some((d, _, _)) if distance >= d)
                                {
                                    if dc > 0.0 {
                                        break;
                                    }
                                    c += 1;
                                    continue;
                                }
                                if reachable(self, world, &geom, from, air, (dc, dr)) {
                                    best = Some((distance, (dc, dr), air));
                                }
                                c += 1;
                            }
                        }
                        si += 1;
                    }
                    step += 1;
                }
                if best.is_some() {
                    break;
                }
            }
            if let Some((_, (dc, dr), air)) = best {
                return Some(CheerStage {
                    at: Some((self.col + dc, self.row + dr)),
                    air,
                });
            }
        }
        None
    }

    /// A LANDING SEEN AT FIRST LOOK (§11.3): a cat off glass when its cheer comes due (its home under the host's chrome, where the console law hides it, or not yet faded in) has no flight anyone could watch, and a flight from a refused body is stepped aside on its first frame, the whole arc translated with it, so it lands off its stage.
    #[inline]
    pub(super) const fn appear_on_cheer_stage(&mut self, sense: PetSense, width: f32) -> bool {
        if self.console.on_glass || self.flight.is_some() {
            return false;
        }
        let stage = self.cheer_stage(sense, width, None);
        self.room_cheer_air = match stage {
            Some(s) => Some(s.air),
            None => None,
        };
        let Some(CheerStage {
            at: Some((col, row)),
            ..
        }) = stage
        else {
            self.room_cheer_trips = CHEER_STAGE_TRIPS;
            return false;
        };
        self.room_cheer_trips = 1;
        self.room_cheer_stage = Some((col, row));
        if let Some(CheerAir::Lanes(facing_left)) = self.room_cheer_air {
            self.facing_left = facing_left;
        }
        self.col = col;
        self.row = row;
        self.home = col;
        self.col_at_tick = col;
        self.alpha = 0.0;
        self.console.hold = VisibilityHold::DEFAULT;
        self.console.trip = None;
        self.console.target = None;
        true
    }

    /// Whether the re-sighting ghost may stay on its own glass, judged at the boundary ([`PetBrain::resight_coordinate_space`]) before the old map is retired: the body standing still at its seat in `last_stance` is admitted only where the map certifies nothing protected under it.
    #[inline]
    pub(super) const fn console_still_body_clear(&self) -> bool {
        matches!(
            self.console_still_rect_verdict(self.col, self.row, self.last_stance),
            Some(true)
        )
    }

    /// [`Self::console_still_body_clear`]'s verdict for a still body at `(col, row)` in `stance` (a ghost still on glass at the boundary, `Departure::own_clear`), over what the map sees of it (`PetWorld::seen_under_text_clearance_past_caret`): `None` where the covered part is clear and the rest unseen, `Some(false)` over a protected cell or with no certified map.
    #[inline]
    pub(super) const fn console_still_rect_verdict(
        &self,
        col: f32,
        row: f32,
        stance: Stance,
    ) -> Option<bool> {
        let Some(world) = self.console.world.as_ref() else {
            return Some(false);
        };
        if world.stamp().is_none() {
            return Some(false);
        }
        let rect = still_body_rect(
            (col, row),
            stance,
            (self.last_cell_w, self.last_cell_h),
            self.last_grid,
        );
        world.seen_under_text_clearance_past_caret(rect)
    }

    /// WHERE A SLEEPER MAY STRETCH: the body where the cat lies covers no ink and nothing protected the map sees ([`PetWorld::ink_clearance`], which counts as ink every cell anything protects: the host's chrome, an image, a selection, an overlay, a row it cannot see), so the stretch is drawn in front of blank ground and never under a word.
    #[inline]
    pub(super) const fn console_stretch_on_blank(&self) -> bool {
        self.console_stretch_on_blank_at((self.col, self.row))
    }

    /// [`Self::console_stretch_on_blank`] for a body at `at` (cells): the sleeper's fall judges where it falls from and where it lands.
    #[inline]
    pub(super) const fn console_stretch_on_blank_at(&self, at: (f32, f32)) -> bool {
        if !self.console.presentable {
            return true;
        }
        let Some(world) = self.console.world.as_ref() else {
            return true;
        };
        let rect = still_body_rect(
            at,
            Stance::NEUTRAL,
            (self.last_cell_w, self.last_cell_h),
            self.last_grid,
        );
        matches!(world.ink_clearance(rect, 0.0), Some(true))
    }

    /// An examined obstruction under the pet's natural body.
    #[inline]
    pub(super) fn console_obstructed(&self, sense: PetSense) -> bool {
        self.console.presentable
            && matches!(
                self.console.world.as_ref(),
                Some(world)
                    if matches!(
                        Self::stand_clearance(
                            world,
                            self.console_body(sense),
                            self.console_ground_passing(),
                        ),
                        Some(false)
                    )
            )
    }

    #[inline]
    const fn watching_stream(&self) -> bool {
        (self.watch_heat >= WATCH_GATE && !self.watch_spent) || self.vigil_armed || self.vigil
    }

    #[inline]
    pub(super) fn reseat_unshown_console_body(&mut self, sense: PetSense, width: f32) {
        // Nothing was on glass last emission, so a first appearance or a
        // fully faded resident may start its fade at a current safe station;
        // a body the user can still see must use locomotion.
        if self.console.on_glass
            || self.console.resident
            || self.flight.is_some()
            || !self.console_obstructed(sense)
        {
            return;
        }
        if let Some((r, c)) = sense.caret {
            let (col, row) = self.station_safe((r, c), sense.cols, sense.rows, width);
            if col != self.col || row != self.row {
                self.col = col;
                self.row = row;
                self.alpha = 0.0;
                self.last_caret = None;
                self.console.hold = VisibilityHold::DEFAULT;
            }
        }
    }

    /// Read once per brain tick, before choosing between caret work and a located reading/output interest.
    #[inline]
    pub(super) fn begin_console_tick(&mut self, sense: PetSense, width: f32) {
        let contact_changed = core::mem::take(&mut self.console.contact_changed);
        let perched_since = self.console.perched_since.take();
        let settled_since = if self.console.moving {
            sense.now
        } else {
            match perched_since {
                Some(t) => t,
                None => sense.now,
            }
        };
        self.console.pose = None;
        self.console.still = false;
        self.console.lift = 0.0;
        self.console.clipped = false;
        self.console.attention = PetAttention::Rest;
        self.console.reason = "quiet";
        let body = self.console_body(sense);
        let Some(stamp) = (match self.console.world.as_ref() {
            Some(w) => w.stamp(),
            None => None,
        }) else {
            self.console.cursor_home = None;
            self.console.rest_seat.set(None);
            self.console.rest_seated.set(false);
            self.console.resident = false;
            self.console.trip = None;
            return;
        };
        let prior = self.console.tick_stamp.replace(stamp);
        let same_surface = matches!(prior, Some(p) if p.surface == stamp.surface);
        let new_content =
            same_surface && matches!(prior, Some(p) if p.content_seq != stamp.content_seq);
        if !matches!(prior, Some(previous) if stamp.preserves_cursor_home(previous)) {
            self.console.cursor_home = None;
        }
        if !same_surface {
            self.console.anchor = None;
            self.console.target = None;
            self.console.rest_seat.set(None);
            self.console.rest_seated.set(false);
            self.console.trip = None;
            self.console.progress_key = None;
            self.console.progress_at = None;
            self.console.result_at = None;
            self.console.contact_at = None;
            self.console.contact_armed = true;
        }
        if !self.console.presentable {
            self.console.cursor_home = None;
            self.console.rest_seat.set(None);
            self.console.rest_seated.set(false);
            self.console.resident = false;
            self.console.trip = None;
            self.console.anchor = None;
            self.console.target = None;
            self.console.result_at = None;
            self.console.completion = None;
            self.console.contact_at = None;
            self.console.consumed = self.console.seq;
            self.console.last_body = None;
            self.console.last_seat = None;
            return;
        }
        if sense.caret_drawn
            && let Some(caret) = sense.caret
        {
            self.console.cursor_home = Some(caret);
        }

        let pending_input = match self.console.input {
            Some(e) if e.seq != self.console.consumed => Some(e),
            _ => None,
        };
        self.console.consumed = self.console.seq;
        let fresh_input = match pending_input {
            Some(e) if sense.now.saturating_duration_since(e.at) < INPUT_HOLD_DUR => Some(e),
            _ => None,
        };
        if let Some(e) = fresh_input {
            self.console.source_seq = e.seq;
            self.console.anchor = None;
            self.console.target = None;
            self.console.trip = None;
            self.console.resident = false;
            self.console.result_at = None;
            self.console.completion = None;
            self.clear_play();
            self.pending_pounce = false;
            self.pending_big_jump = false;
            self.pending_pointer_pounce = None;
            self.pending_cheer = None;
            self.drop_room_cheer();
            self.pending_sulk = false;
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
            self.quiet = 0.0;
            self.clear_trick_settle();
            self.drop_trick_go();
            self.song_left = 0;
            if self.alpha > 0.0 && self.action == PetAction::Sleep && sense.caret.is_some() {
                self.set_action(PetAction::Waking);
            }
        }
        let input_live = matches!(
            self.console.input,
            Some(e) if sense.now.saturating_duration_since(e.at) < INPUT_HOLD_DUR
        );
        if matches!(
            self.console.completion,
            Some((at, _)) if sense.now.saturating_duration_since(at) >= REACTION_TTL_DUR
        ) || input_live
        {
            self.console.completion = None;
            self.pending_sulk = false;
            self.pending_cheer = None;
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
        }
        let world = self.console.world.as_ref().expect("observed above");
        let home = self.console_caret_home(sense, width);
        let selection = world.selection_target();
        if let Some(gaze) = selection {
            self.console.attention = PetAttention::Reading;
            self.console.reason = "selection";
            self.console.source_seq = self.console.observed_seq;
            self.console.gaze = Some(gaze);
            self.console.anchor = None;
            self.console.target = self.console_seat(world, body, home);
            self.console.resident = true;
            self.console.result_at = None;
            self.console.completion = None;
            self.pending_sulk = false;
            self.pending_cheer = None;
            self.drop_room_cheer();
            self.drop_room_cue();
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
            self.console.pose = Some(if gaze.0 < self.row - 0.5 {
                PetGlyphId::PetEdgeLookUp
            } else if gaze.0 > self.row + 0.5 {
                PetGlyphId::PetEdgeLookDown
            } else {
                PetGlyphId::PetEdgePerch
            });
            self.console.still = true;
            return;
        }

        let repairing = matches!(self.console.repair_until, Some(end) if sense.now < end)
            && matches!(
                (self.console.repair_target, sense.caret),
                (Some((r, c)), Some((cr, cc)))
                    if (r - f32::from(cr)).abs() < 0.5 && (c - f32::from(cc)).abs() <= 6.0
            );
        if input_live || repairing {
            self.console.resident = false;
            self.console.trip = None;
            self.console.attention = if repairing {
                PetAttention::Editing
            } else {
                PetAttention::Typing
            };
            self.console.reason = if repairing {
                "committed-edit-intent"
            } else {
                "committed-input"
            };
            self.console.gaze = match self.console.repair_target {
                Some(t) => Some(t),
                None => match self.console.input {
                    Some(e) => e.target,
                    None => None,
                },
            };
            self.console.pose = Some(if repairing {
                PetGlyphId::PetInspectDown
            } else {
                PetGlyphId::PetStandEar
            });
            let caret_overlaps_body = match sense.caret {
                Some((cr, cc)) => {
                    let cr = f32::from(cr);
                    let cc = f32::from(cc);
                    cr + 1.0 > body.row
                        && cr < body.row + body.rows
                        && cc + 1.0 > body.col
                        && cc < body.col + body.cols
                }
                None => false,
            };
            let stand_pushed_past = match home {
                Some(stand) => {
                    (body.row - stand.row).abs() >= 0.25
                        || if self.wall_side {
                            body.col > stand.col + ARRIVED
                        } else {
                            body.col + ARRIVED < stand.col
                        }
                }
                None => false,
            };
            if repairing
                && !caret_overlaps_body
                && !stand_pushed_past
                && Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing())
                && self.flight.is_none()
            {
                self.console.target = Some(body);
                self.console.resident = true;
                self.console.still = true;
            }
            let roomy = world.clear_past_caret(body, CONTACT_MARGIN);
            if roomy
                && match self.console.contact_at {
                    Some(at) => sense.now.saturating_duration_since(at) >= CONTACT_HOLD_DUR,
                    None => true,
                }
            {
                self.console.contact_armed = true;
            }
            if new_content
                && contact_changed
                && !roomy
                && self.console.contact_armed
                && input_live
                && matches!(
                    self.console.input,
                    Some(e) if e.kind == PetInputKind::Text
                )
            {
                self.console.contact_at = Some(sense.now);
                self.console.contact_armed = false;
            }
            if let Some(at) = self.console.contact_at {
                let contact_age = sense.now.saturating_duration_since(at);
                if contact_age < CONTACT_HOLD_DUR {
                    self.console.attention = PetAttention::Contact;
                    self.console.reason = "advancing-ink";
                    self.console.pose = Some(if contact_age < CONTACT_RECOVER_DUR {
                        if self.facing_left && world.clear_past_caret(body, CLEARANCE) {
                            PetGlyphId::PetTailTuck
                        } else {
                            PetGlyphId::PetContactBrace
                        }
                    } else {
                        PetGlyphId::PetContactRecover
                    });
                }
            }
            return;
        }

        if self.room_cheer && !self.room_cheer_in_play() {
            if matches!(
                home,
                Some(stand)
                    if world.under_text_clearance_past_caret(stand, CLEARANCE) == Some(false)
            ) {
                self.room_cheer_leaving = true;
            } else {
                self.room_cheer = false;
            }
        }
        if self.room_cue_act && !self.room_cue_in_play() {
            self.room_cue_act = false;
        }
        if self.pending_pet > 0
            || self.pet_hold_t > 0.0
            || self.pending_bell.is_some()
            || self.flinch_t > 0.0
            || self.trick_engaged()
            || self.room_cheer
            || self.room_cue_act
            || (self.action.settled()
                && !matches!(self.action, PetAction::Sleep | PetAction::Waking)
                && ((self.pending_room.is_some()
                    && matches!(
                        self.room_at,
                        Some(at) if sense.now.saturating_duration_since(at) <= ROOM_CUE_TTL_DUR
                    ))
                    || (self.pending_bat.is_some()
                        && matches!(
                            self.bat_at,
                            Some(at) if sense.now.saturating_duration_since(at) <= BAT_TTL_DUR
                        ))
                    || (self.pending_look.is_some()
                        && matches!(
                            self.look_at,
                            Some(at) if sense.now.saturating_duration_since(at) <= LOOK_TTL_DUR
                        ))
                    || matches!(
                        self.pending_perk,
                        Some(at)
                            if sense.now >= at
                                && sense.now.saturating_duration_since(at) <= V2_PERK_LATE_DUR
                    )
                    || (self.catch_cool <= 0.0
                        && matches!(
                            self.pending_catch,
                            Some(star)
                                if sense.now.saturating_duration_since(star.born) <= V2_CATCH_TTL_DUR
                        )))
                && matches!(
                    home,
                    Some(stand) if self.caret_seat(world, body, stand) == CaretSeat::Hold
                ))
        {
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            self.console.pose = None;
            return;
        }

        let mut executing = None;
        let anchors = world.anchors();
        let mut ai = 0;
        while ai < anchors.len() {
            if let Some(a) = anchors[ai]
                && a.state == BlockState::Executing
            {
                executing = Some(a);
                break;
            }
            ai += 1;
        }
        let completion = match self.console.completion.take() {
            Some((at, failed)) if sense.now.saturating_duration_since(at) < REACTION_TTL_DUR => {
                Some((at, failed))
            }
            _ => None,
        };
        if let Some((_, failed)) = completion {
            self.console.result_at = Some((sense.now, failed));
            self.console.source_seq = self.console.completion_seq;
            if failed {
                self.console.failure_quiet_until = Some(sense.now + RESULT_HOLD_DUR);
            }
        }
        if completion.is_some() || self.console.result_at.is_some() {
            self.pending_sulk = false;
            self.pending_cheer = None;
            self.pending_vigil_pounce = false;
            self.vigil_cheer = None;
        }
        let result = match self.console.result_at {
            Some((at, failed)) if sense.now.saturating_duration_since(at) < RESULT_HOLD_DUR => {
                Some((at, failed))
            }
            _ => None,
        };
        if result.is_none() {
            self.console.result_at = None;
        }

        if let Some(subject) = executing {
            let next_progress = world.progress();
            if self.console.progress_key != Some(subject.block_id) {
                self.console.progress_key = Some(subject.block_id);
                self.console.progress = next_progress;
                self.console.progress_at = None;
            } else if next_progress != self.console.progress {
                self.console.progress = next_progress;
                if matches!(
                    next_progress,
                    Some(TaskbarProgress::Normal(_) | TaskbarProgress::Error(_))
                ) {
                    self.console.progress_seq = self.console.progress_seq.wrapping_add(1).max(1);
                    self.console.progress_at = Some(sense.now);
                } else {
                    self.console.progress_at = None;
                }
            }
            let progress = matches!(
                self.console.progress_at,
                Some(at) if sense.now.saturating_duration_since(at) < PROGRESS_HOLD_DUR
            );
            self.console.attention = if progress {
                PetAttention::Progress
            } else {
                PetAttention::Output
            };
            self.console.reason = if progress {
                "explicit-progress-change"
            } else {
                "command-output"
            };
            self.console.source_seq = if progress {
                self.console.progress_seq
            } else {
                stamp.content_seq
            };
            let interactive_caret = match sense.caret {
                Some((cr, cc))
                    if sense.caret_drawn
                        && (subject.row > f32::from(cr) + 0.5
                            || self.console.faux_caret.is_some()
                            || matches!(
                                self.console.input,
                                Some(e)
                                    if matches!(
                                        e.kind,
                                        PetInputKind::Text
                                            | PetInputKind::Delete
                                            | PetInputKind::Navigate
                                            | PetInputKind::Paste
                                    )
                            )) =>
                {
                    Some((cr, cc))
                }
                _ => None,
            };
            self.console.gaze = Some(if let Some((cr, cc)) = interactive_caret {
                (f32::from(cr), f32::from(cc))
            } else {
                (subject.row, subject.col)
            });
            self.console.pose = Some(if progress {
                PetGlyphId::PetPerkTurn
            } else if interactive_caret.is_some() {
                PetGlyphId::PetSit
            } else if subject.row < self.row - 0.5 {
                PetGlyphId::PetEdgeLookUp
            } else if subject.row > self.row + 0.5 {
                PetGlyphId::PetEdgeLookDown
            } else {
                PetGlyphId::PetEdgeLean
            });
            self.choose_console_perch(subject, body, home, stamp, prior);
            self.console.resident = true;
            self.console.still = true;
        } else if let Some((_, failed)) = result {
            self.console.attention = PetAttention::Result;
            self.console.reason = if failed {
                "command-exit-failed"
            } else {
                "command-exit-success"
            };
            self.console.pose = if failed {
                Some(PetGlyphId::PetInspectDown)
            } else if self.console_stretch_on_blank() {
                Some(PetGlyphId::PetStretchHind)
            } else {
                None
            };
            self.console.target =
                if Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing()) {
                    Some(body)
                } else {
                    None
                };
            self.console.resident = true;
            self.console.still = true;
        } else if let Some(anchor) = match self.console.anchor {
            Some(a) => match world.resolve(a) {
                Some(resolved) => Some(resolved),
                None => {
                    let mut found = None;
                    let mut j = 0;
                    while j < anchors.len() {
                        if let Some(current) = anchors[j]
                            && current.surface == a.surface
                            && current.block_id == a.block_id
                        {
                            found = Some(current);
                            break;
                        }
                        j += 1;
                    }
                    found
                }
            },
            None => None,
        } {
            // One content anchor may survive a completed command; no timer
            // grants another excursion. The perch is a visit, not a tenancy:
            // [`PERCH_DWELL`] bounds it and `console_deadline` offers its end.
            if sense.now.saturating_duration_since(settled_since) < PERCH_DWELL_DUR {
                self.console.perched_since = Some(settled_since);
                self.console.attention = PetAttention::Exploring;
                self.console.reason = "content-perch";
                self.console.gaze = Some((anchor.row, anchor.col));
                self.console.pose = Some(if anchor.row < self.row - 0.5 {
                    PetGlyphId::PetEdgeLookUp
                } else if anchor.row > self.row + 0.5 {
                    PetGlyphId::PetEdgeLookDown
                } else {
                    PetGlyphId::PetEdgePerch
                });
                self.choose_console_perch(anchor, body, home, stamp, prior);
                self.console.resident = true;
                self.console.still = true;
            } else {
                self.console.resident = false;
                self.console.anchor = None;
                self.console.target = None;
                self.console.trip = None;
                self.console.reason = "perch-spent";
            }
        } else {
            self.console.resident = false;
            self.console.anchor = None;
            self.console.target = None;
            self.console.trip = None;
            self.console.progress_at = None;
            self.console.progress_key = None;
        }
    }

    #[inline]
    fn choose_console_perch(
        &mut self,
        subject: PetAnchor,
        mut body: PetRect,
        home: Option<PetRect>,
        stamp: PetWorldStamp,
        prior: Option<PetWorldStamp>,
    ) {
        let world = self.console.world.as_ref().expect("observed");
        let retained = match self.console.anchor {
            Some(a) if a.block_id == subject.block_id => world.resolve(a),
            _ => None,
        };
        if let Some(anchor) = retained
            && let Some(mut target) = self.console.target
        {
            let mut leaving_view = false;
            if let Some(old) = prior {
                let exact_scroll = old.surface == stamp.surface
                    && old.display_offset == 0
                    && stamp.display_offset == 0
                    && stamp.top_absolute_row >= old.top_absolute_row
                    && stamp.top_absolute_row - old.top_absolute_row
                        == stamp.uniform_up_rows.saturating_sub(old.uniform_up_rows);
                if exact_scroll {
                    let delta = stamp.top_absolute_row - old.top_absolute_row;
                    if delta > 0 {
                        let dy = delta as f32;
                        target.row -= dy;
                        body.row -= dy;
                        let scrolled_off_top = self.row - dy < 0.0;
                        if !scrolled_off_top {
                            self.row -= dy;
                            if let Some(trip) = &mut self.console.trip {
                                trip.from.1 -= dy;
                                trip.to.1 -= dy;
                            }
                            if let Some(look) = &mut self.adopted_look {
                                look.seat.1 -= dy;
                            }
                            let mut i = 0;
                            while i < self.departures.len() {
                                if let Some(d) = &mut self.departures[i]
                                    && d.still
                                    && !d.resight
                                    && d.own_glass.is_none()
                                {
                                    d.row -= dy;
                                }
                                i += 1;
                            }
                        }
                        leaving_view = target.row < 2.0 || scrolled_off_top;
                    }
                } else if old.top_absolute_row != stamp.top_absolute_row {
                    self.console.anchor = None;
                    self.console.target = None;
                    self.console.trip = None;
                    return;
                }
            }
            if Self::near_console_home(target, home)
                && world.under_text_clear(target, CLEARANCE)
                && !leaving_view
            {
                self.console.anchor = Some(anchor);
                self.console.target = Some(target);
                return;
            }
        }
        let next = self.console_seat(world, body, home);
        self.console.anchor = Some(subject);
        self.console.target = next;
        self.console.trip = None;
    }

    /// Whether ordinary locomotion still owes a flight or landing step.
    #[inline]
    #[must_use]
    pub const fn console_legacy_motion_pending(&self) -> bool {
        self.flight.is_some()
            || self.land_t > 0.0
            || self.bound2.is_some()
            || self.retired_flight_lift.is_some()
            || self.deferred_hidden_landing.is_some()
    }

    /// Whether a deferred console owner still owes its first resident tick.
    #[inline]
    #[must_use]
    pub const fn console_resident_handoff_pending(&self) -> bool {
        self.console.resident_handoff
    }

    /// The repaint hold: for up to [`HIDDEN_HOLD_MAX`] an unpainted caret (a DECTCEM hide means "I am repainting") reads as the last caret this surface painted (`ConsoleLife::cursor_home`, while the observation vouches for it), so the brain sees a painted caret that has not moved: nothing chases, a flight lands where it was going, a sleeper sleeps.
    #[inline]
    pub(super) fn hold_unpainted_caret(&mut self, sense: PetSense, dt: f32) -> PetSense {
        if sense.caret_drawn && sense.caret.is_some() {
            self.console.hidden_hold = 0.0;
            self.console.caret_held = false;
            return sense;
        }
        if self.console.presentable
            && let Some(faux) = self.console.faux_caret
        {
            self.console.hidden_hold = 0.0;
            self.console.cursor_home = Some(faux);
            self.console.caret_held = true;
            return PetSense {
                caret: Some(faux),
                caret_drawn: true,
                ..sense
            };
        }
        self.console.hidden_hold += dt.max(0.0);
        let now_stamp = match &self.console.world {
            Some(w) => w.stamp(),
            None => None,
        };
        if let Some(home) = self.console.cursor_home
            && let Some((cr, cc)) = sense.caret
            && self.console.presentable
        {
            let vouched = self.console.hidden_hold <= HIDDEN_HOLD_MAX
                && matches!(
                    (now_stamp, self.console.tick_stamp),
                    (Some(now), Some(prior)) if now.preserves_cursor_home(prior)
                );
            let corner_park = matches!(
                (now_stamp, self.console.tick_stamp),
                (Some(now), Some(prior)) if now.top_absolute_row == prior.top_absolute_row
            ) && cc == 0
                && cr.saturating_add(1) >= sense.rows
                && (cr, cc) != home;
            if vouched || corner_park {
                self.console.caret_held = true;
                return PetSense {
                    caret: Some(home),
                    caret_drawn: true,
                    ..sense
                };
            }
        }
        if core::mem::take(&mut self.console.caret_held) && sense.caret.is_some() {
            self.last_caret = None;
        }
        sense
    }

    /// New interests use the resident's position, distance-driven gait and authored poses.
    /// `dt` is the tick's capped span, and `move_dt` the part of it the body and the clocks of
    /// what is drawn spend: none on a tick that ends a sleep ([`PetBrain::tick`]).
    ///
    /// A program's cursor hide holds the body at its last seat (`cursor-hidden-home`), but not a
    /// body the wall's cut takes off the line's letters ([`PetBrain::cut_on_the_letters`]): that
    /// seat is the one whose box met them, and the hold brought the body back there, lifting the
    /// cut over [`FADE_IN`] (on `console_cursor_home`'s dense page, its prompt on row 55 of 57,
    /// a cursor hide six frames into the wall's, the body off the glass from that hide's 4th,
    /// drew it back onto the same letters at 11 to 245 in the cursor hide's first 18 frames and
    /// at full from its 19th, Linux arm64, 2026-11-09). Its fade runs out under the hidden caret
    /// as under the wall's hide, and the body comes back as any faded-out one does.
    #[inline]
    pub(super) fn tick_console_resident(
        &mut self,
        sense: PetSense,
        width: f32,
        (dt, move_dt): (f32, f32),
    ) -> Option<PetFrame> {
        let holding = self.console.presentable
            && sense.caret.is_none()
            && self.console.cursor_home.is_some()
            && self.alpha > 0.0
            && !self.wall_cut_falls
            && !self.console_legacy_motion_pending();
        if holding {
            if let Some((col, row)) = self.console.last_seat {
                self.col = col;
                self.row = row;
            }
            self.console.resident = true;
            self.console.target = Some(self.console_body(sense));
            self.console.trip = None;
            if self.console.attention == PetAttention::Rest {
                self.console.reason = "cursor-hidden-home";
                if self.action != PetAction::Sleep {
                    self.console.pose = Some(PetGlyphId::PetLoaf);
                }
            }
            self.console.still = true;
            self.last_caret = None;
        } else if !sense.caret_drawn && sense.caret.is_some() && self.console.presentable {
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            self.console.still = false;
        }
        if !self.console.resident || !self.console.presentable {
            self.console.resident_handoff = false;
            return None;
        }
        if self.console_legacy_motion_pending() {
            self.console.resident_handoff = true;
            return None;
        }
        if matches!(
            self.console.attention,
            PetAttention::Output | PetAttention::Progress
        ) && self.watching_stream()
        {
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            return None;
        }
        self.console.resident_handoff = false;
        let stroke_point = if !holding
            && !sense.reduced_motion
            && self.alpha > 0.0
            && self.last_caret.is_some()
            && sense.caret == self.last_caret
            && self.console.trip.is_none()
            && !self.console.clipped
            && matches!(
                self.console.attention,
                PetAttention::Output
                    | PetAttention::Progress
                    | PetAttention::Result
                    | PetAttention::Exploring
            ) {
            self.stroke_point(&sense, width)
        } else {
            None
        };
        if self.stroke.sample(sense.now, stroke_point, width) {
            self.note_petted(sense.now);
            self.console.resident = false;
            self.console.target = None;
            self.console.trip = None;
            self.console.pose = None;
            return None;
        }
        self.pet_hold_t = 0.0;
        self.pending_pet = 0;
        self.pet_at = None;
        let repairing = matches!(self.console.repair_until, Some(end) if sense.now < end);
        let latched = if repairing { self.pending_trick } else { None };
        self.drop_tricks();
        self.pending_trick = latched;
        let body = self.console_body(sense);
        let world = self.console.world.as_ref()?;
        let home = self.console_caret_home(sense, width);
        let caret_overlaps_body = match sense.caret {
            Some((cr, cc)) => {
                let cr = f32::from(cr);
                let cc = f32::from(cc);
                cr + 1.0 > body.row
                    && cr < body.row + body.rows
                    && cc + 1.0 > body.col
                    && cc < body.col + body.cols
            }
            None => false,
        };
        let stand_ok = match home {
            Some(stand) => {
                (body.row - stand.row).abs() < 0.25
                    && if self.wall_side {
                        body.col <= stand.col + ARRIVED
                    } else {
                        body.col + ARRIVED >= stand.col
                    }
            }
            None => true,
        };
        let claimed = matches!(self.console.attention, PetAttention::Result)
            || (self.console.attention == PetAttention::Editing
                && !caret_overlaps_body
                && stand_ok);
        let target = match (home, sense.caret.is_some()) {
            (Some(stand), true)
                if claimed
                    && Self::near_console_home(body, Some(stand))
                    && Self::console_may_stand(
                        world,
                        body,
                        CLEARANCE,
                        self.console_ground_passing(),
                    ) =>
            {
                Some(body)
            }
            (Some(stand), true) => match self.caret_seat(world, body, stand) {
                CaretSeat::Hold => Some(body),
                CaretSeat::Escort if !sense.reduced_motion && self.alpha > 0.0 => {
                    self.console.resident = false;
                    self.console.target = None;
                    self.console.trip = None;
                    return None;
                }
                CaretSeat::Escort => Some(stand),
                CaretSeat::Yield => world.nearest_under_text_perch(stand, CLEARANCE, HOME_REACH),
            },
            _ => match self.console.target {
                Some(r) if world.under_text_clear(r, CLEARANCE) => Some(r),
                _ if Self::console_may_stand(
                    world,
                    body,
                    CLEARANCE,
                    self.console_ground_passing(),
                ) =>
                {
                    Some(body)
                }
                _ => world.home_perch(body, CLEARANCE, PERCH_REACH),
            },
        };
        self.console.target = target;
        let Some(target) = target else {
            self.console.clipped = true;
            self.console.moving = false;
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "protected-surface";
            self.console.trip = None;
            self.speed = 0.0;
            self.last_caret = sense.caret;
            self.note_burst_twitch(sense.output_burst, false);
            self.twitch_t = 0.0;
            self.stumble_t = 0.0;
            self.clear_play();
            self.pending_pounce = false;
            self.pending_big_jump = false;
            self.pending_bell = None;
            self.pending_flow = None;
            self.pending_perk = None;
            self.pending_catch = None;
            self.pending_look = None;
            self.watch_heat = 0.0;
            self.stream = false;
            self.motes = [None; PET_MOTES_MAX];
            self.land_t = 0.0;
            self.retired_flight_lift = None;
            self.drop_room_cue();
            self.lift_the_wall_cut(move_dt);
            return Some(self.emit(sense, width));
        };
        let destination = if target == body {
            (self.col, self.row)
        } else {
            (target.col, target.foot_row())
        };
        if self.alpha <= 0.0 {
            self.col = destination.0;
            self.row = destination.1;
        }
        self.alpha = if sense.reduced_motion {
            1.0
        } else {
            (self.alpha + move_dt * (1.0 / FADE_IN)).min(1.0)
        };
        self.last_caret = sense.caret;
        self.quiet += dt;
        self.twitch_t = if sense.reduced_motion {
            0.0
        } else {
            (self.twitch_t - move_dt).max(0.0)
        };
        self.stumble_t = 0.0;
        let prev_pointer = (
            self.last_pointer,
            self.pointer_vx,
            self.pointer_vy,
            self.pointer_speed,
            self.pointer_heat,
        );
        let prev_v2 = (self.pending_perk, self.pending_catch);
        self.clear_play();
        self.catch_cool = if sense.reduced_motion {
            0.0
        } else {
            (self.catch_cool - dt).max(0.0)
        };
        let v2_live = !holding
            && !sense.reduced_motion
            && !self.console.clipped
            && self.console.attention != PetAttention::Reading;
        self.pending_pounce = false;
        self.pending_big_jump = false;
        self.pending_bell = None;
        self.pending_flow = None;
        self.pending_perk = match prev_v2.0 {
            Some(at) if v2_live && sense.now.saturating_duration_since(at) <= V2_PERK_LATE_DUR => {
                Some(at)
            }
            _ => None,
        };
        self.pending_catch = match prev_v2.1 {
            Some(star)
                if v2_live
                    && sense.now.saturating_duration_since(star.born) <= V2_CATCH_TTL_DUR =>
            {
                Some(star)
            }
            _ => None,
        };
        if !v2_live
            || !matches!(
                self.room_at,
                Some(at) if sense.now.saturating_duration_since(at) <= ROOM_CUE_TTL_DUR
            )
        {
            self.drop_room_cue();
        }
        self.pending_bat = None;
        self.pending_look = None;
        self.watch_heat = 0.0;
        self.stream = false;
        self.motes = [None; PET_MOTES_MAX];
        self.land_t = 0.0;
        self.retired_flight_lift = None;
        self.deferred_hidden_landing = None;
        self.console.moving = false;

        let world = self.console.world.as_ref().expect("observed");
        if sense.reduced_motion {
            self.col = destination.0;
            self.row = destination.1;
            self.console.trip = None;
        } else if (self.col - destination.0)
            .abs()
            .max((self.row - destination.1).abs())
            > 0.02
        {
            if !world.under_text_corridor_clear_past_caret(body, target, CLEARANCE) {
                self.console.trip = None;
                if Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing()) {
                    self.console.target = Some(body);
                } else {
                    self.console.target = Some(target);
                    self.console.reason = "protected-displacement";
                }
            } else {
                let mut trip = match self.console.trip {
                    Some(trip) if trip.to == destination => trip,
                    _ => {
                        let dist = (self.col - destination.0)
                            .abs()
                            .max((self.row - destination.1).abs() * ROW_AS_COLS);
                        let hop = (self.row - destination.1).abs() > 0.25;
                        PerchTrip {
                            from: (self.col, self.row),
                            to: destination,
                            elapsed: 0.0,
                            duration: (dist * (1.0 / 22.0)).max(0.22),
                            hop,
                        }
                    }
                };
                trip.elapsed += move_dt;
                let u = (trip.elapsed / trip.duration).clamp(0.0, 1.0);
                let s = if trip.hop { u } else { u * u * (3.0 - 2.0 * u) };
                let x = trip.from.0 + (trip.to.0 - trip.from.0) * s;
                let y = trip.from.1 + (trip.to.1 - trip.from.1) * s;
                let travelled = (x - self.col).abs();
                self.speed = if move_dt > 0.0 && u < 1.0 {
                    (x - self.col) / move_dt
                } else {
                    0.0
                };
                let amble = !trip.hop && (trip.to.0 - trip.from.0).abs() <= 2.0;
                let inv_step = if amble {
                    1.0 / LAT_STRIDE_CELLS
                } else {
                    1.0 / STRIDE_CELLS
                };
                self.col = x;
                self.row = y;
                let next_stride = self.stride + travelled * inv_step;
                self.stride = if (0.0..1024.0).contains(&next_stride) {
                    next_stride
                } else {
                    next_stride - (next_stride * (1.0 / 1024.0)).floor() * 1024.0
                };
                self.console.moving = u < 1.0;
                let arc = PetRect::new(
                    body.row.min(target.row) - 0.25,
                    body.col.min(target.col),
                    body.rows + (body.row - target.row).abs() + 0.25,
                    body.cols + (body.col - target.col).abs(),
                );
                self.console.lift =
                    if trip.hop && Self::console_may_stand(world, arc, CLEARANCE, false) {
                        0.25 * crate::supernova::sin_cos_f32(core::f32::consts::PI * u).0
                    } else {
                        0.0
                    };
                if u == 1.0 {
                    self.console.trip = None;
                    self.console.lift = 0.0;
                } else {
                    self.console.trip = Some(trip);
                    self.console.pose = Some(if trip.hop {
                        if u < 0.25 {
                            PetGlyphId::PetLeapRise
                        } else if u < 0.65 {
                            PetGlyphId::PetHop
                        } else {
                            PetGlyphId::PetLeapDescend
                        }
                    } else if amble {
                        Self::CYCLE_LWALK[self.gait_index(Self::CYCLE_LWALK.len())]
                    } else {
                        Self::CYCLE_WALK[self.gait_index(Self::CYCLE_WALK.len())]
                    });
                }
            }
        } else {
            self.console.trip = None;
        }
        if !(holding && self.action == PetAction::Sleep) {
            self.set_action_keep(if self.console.moving {
                PetAction::Walk
            } else {
                PetAction::Sit
            });
        }
        if !self.console.moving {
            self.speed = 0.0;
            self.plant_stride(move_dt);
            if !holding
                && !sense.reduced_motion
                && !self.console.clipped
                && self.action != PetAction::Sleep
                && matches!(
                    self.console.attention,
                    PetAttention::Output | PetAttention::Exploring
                )
            {
                (
                    self.last_pointer,
                    self.pointer_vx,
                    self.pointer_vy,
                    self.pointer_speed,
                    self.pointer_heat,
                ) = prev_pointer;
                match sense.pointer {
                    Some((px, py)) if px.is_finite() && py.is_finite() => {
                        if let Some((lx, ly)) = self.last_pointer
                            && dt > 1e-4
                        {
                            let stationary = px == lx && py == ly;
                            if !stationary
                                || self.pointer_vx != 0.0
                                || self.pointer_vy != 0.0
                                || self.pointer_speed != 0.0
                            {
                                let k = 1.0 - crate::color_math::exp_f32(-dt * (1.0 / DASH_WINDOW));
                                let (vx_inst, vy_inst, inst) = if stationary {
                                    (0.0, 0.0, 0.0)
                                } else {
                                    let inv_dt = 1.0 / dt;
                                    let vx = (px - lx) * inv_dt;
                                    let vy = (py - ly) * inv_dt;
                                    (vx, vy, crate::color_math::hypot_f32(vx, vy))
                                };
                                self.pointer_vx += (vx_inst - self.pointer_vx) * k;
                                self.pointer_vy += (vy_inst - self.pointer_vy) * k;
                                self.pointer_speed += (inst - self.pointer_speed) * k;
                                if stationary && self.pointer_speed < 1e-5 {
                                    self.pointer_vx = 0.0;
                                    self.pointer_vy = 0.0;
                                    self.pointer_speed = 0.0;
                                }
                            }
                        }
                        self.last_pointer = Some((px, py));
                        self.pointer_heat = if self.pointer_speed >= GAZE_FOLLOW_SPEED_MIN {
                            self.frame_owed |= move_dt == 0.0 && self.pointer_heat < 1.0;
                            (self.pointer_heat + move_dt * POINTER_RISE).min(1.0)
                        } else {
                            (self.pointer_heat - move_dt * POINTER_FALL).max(0.0)
                        };
                    }
                    _ => {
                        self.last_pointer = None;
                        self.pointer_vx = 0.0;
                        self.pointer_vy = 0.0;
                        self.pointer_speed = 0.0;
                        self.pointer_heat = 0.0;
                    }
                }
            }
            let pointer_gaze = match (self.pointer_heat > 0.0, self.last_pointer) {
                (true, Some((px, py))) => Some((py, px)),
                _ => None,
            };
            if let Some((r, c)) = match pointer_gaze {
                Some(g) => Some(g),
                None => self.console.gaze,
            } {
                self.facing_left = c < self.col + width * 0.5;
                if matches!(
                    self.console.attention,
                    PetAttention::Reading | PetAttention::Output | PetAttention::Exploring
                ) && (self.console.pose != Some(PetGlyphId::PetSit)
                    || matches!(pointer_gaze, Some((pr, _)) if (pr - self.row).abs() > 0.5))
                {
                    self.console.pose = Some(if r < self.row - 0.5 {
                        PetGlyphId::PetEdgeLookUp
                    } else if r > self.row + 0.5 {
                        PetGlyphId::PetEdgeLookDown
                    } else if self.console.pose == Some(PetGlyphId::PetSit) {
                        PetGlyphId::PetSit
                    } else if matches!(
                        self.console.attention,
                        PetAttention::Reading | PetAttention::Exploring
                    ) {
                        PetGlyphId::PetEdgePerch
                    } else {
                        PetGlyphId::PetEdgeLean
                    });
                }
            }
            let can_twitch = !holding
                && !sense.reduced_motion
                && !self.console.clipped
                && self.action != PetAction::Sleep
                && self.console.pose == Some(PetGlyphId::PetSit);
            self.note_burst_twitch(sense.output_burst, can_twitch);
            if can_twitch {
                if self.twitch_t > 0.0 {
                    self.console.pose = Some(if self.twitch_up {
                        PetGlyphId::PetSitEar
                    } else {
                        PetGlyphId::PetSitEarFar
                    });
                }
            } else {
                self.twitch_t = 0.0;
            }
        } else {
            self.note_burst_twitch(sense.output_burst, false);
            self.twitch_t = 0.0;
            if self.speed.abs() > f32::EPSILON {
                self.facing_left = self.speed < 0.0;
            } else if let Some(trip) = self.console.trip
                && (trip.to.0 - trip.from.0).abs() > f32::EPSILON
            {
                self.facing_left = trip.to.0 < trip.from.0;
            }
        }
        self.lift_the_wall_cut(move_dt);
        Some(self.emit(sense, width))
    }

    #[inline]
    const fn console_may_stand(
        world: &PetWorld,
        rect: PetRect,
        margin: f32,
        passing: bool,
    ) -> bool {
        matches!(
            Self::ground_clearance(world, rect, margin, passing),
            Some(true)
        )
    }

    /// THE ESCORT PASSES UNDER THE HOST'S LABELS.
    #[inline]
    pub(super) fn console_escorting(&self) -> bool {
        self.escorted_within(SIT_AFTER, SIT_AFTER_DUR)
    }

    #[inline]
    fn escorted_within(&self, secs: f32, dur: Duration) -> bool {
        self.typed_within_dur(dur)
            || (self.watch_heat < WATCH_GATE
                && matches!(self.rhythm_at, Some(at) if ((self.clock - at) as f32) < secs))
    }

    #[inline]
    fn typed_within_dur(&self, dur: Duration) -> bool {
        matches!(
            (self.console.typed_at, self.last_now),
            (Some(at), Some(now)) if now.saturating_duration_since(at) < dur
        )
    }

    /// How long until the last key the app noted ([`Self::note_console_input`]) is `dur` old,
    /// while it is younger.
    #[inline]
    pub(super) fn typed_left_dur(&self, dur: Duration) -> Option<Duration> {
        match (self.console.typed_at, self.last_now) {
            (Some(at), Some(now)) => match dur.checked_sub(now.saturating_duration_since(at)) {
                Some(left) if !left.is_zero() => Some(left),
                _ => None,
            },
            _ => None,
        }
    }

    #[inline]
    fn typed_within(&self, secs: f32) -> bool {
        matches!(
            (self.console.typed_at, self.last_now),
            (Some(at), Some(now)) if now.saturating_duration_since(at).as_secs_f32() < secs
        )
    }

    /// The sequence number of the last key the app noted ([`Self::note_console_input`]), when it was an Enter: one Enter, told from the next.
    #[inline]
    pub(super) const fn last_submit_seq(&self) -> Option<u64> {
        if matches!(self.console.typed_kind, Some(PetInputKind::Submit)) {
            Some(self.console.seq)
        } else {
            None
        }
    }

    /// The kind of the last key the app noted ([`Self::note_console_input`]), when it was pressed within `secs`: a letter typed is writing that goes forward before its echo has moved the caret.
    #[inline]
    pub(super) fn key_within(&self, secs: f32) -> Option<PetInputKind> {
        if self.typed_within(secs) {
            self.console.typed_kind
        } else {
            None
        }
    }

    #[inline]
    pub(super) fn key_within_dur(&self, dur: Duration) -> Option<PetInputKind> {
        if self.typed_within_dur(dur) {
            self.console.typed_kind
        } else {
            None
        }
    }

    #[inline]
    fn console_ground_passing(&self) -> bool {
        matches!(self.action, PetAction::Walk | PetAction::Run)
            || (self.escorted_within(SIT_AFTER + SIT_FOLD, SIT_AFTER_FOLD_DUR)
                && matches!(self.last_caret, Some((cr, _)) if (self.row - cr as f32).abs() < 1.5))
    }

    #[inline]
    const fn ground_clearance(
        world: &PetWorld,
        rect: PetRect,
        margin: f32,
        passing: bool,
    ) -> Option<bool> {
        if passing {
            world.passing_under_text_clearance_past_caret(rect, margin)
        } else {
            world.under_text_clearance_past_caret(rect, margin)
        }
    }

    #[inline]
    const fn on_its_feet(body: PetRect) -> PetRect {
        PetRect::new(
            body.row,
            body.col,
            (body.rows - CLEARANCE).max(f32::EPSILON),
            body.cols,
        )
    }

    #[inline]
    const fn stand_clearance(world: &PetWorld, body: PetRect, passing: bool) -> Option<bool> {
        let (feet, margin) = Self::stand_rect(world, body);
        Self::ground_clearance(world, feet, margin, passing)
    }

    #[inline]
    const fn stand_feet_clearance(world: &PetWorld, feet: PetRect, passing: bool) -> Option<bool> {
        Self::ground_clearance(world, feet, world.margin_in_map(feet, CLEARANCE), passing)
    }

    #[inline]
    const fn stand_rect(world: &PetWorld, body: PetRect) -> (PetRect, f32) {
        let feet = Self::on_its_feet(body);
        (feet, world.margin_in_map(feet, CLEARANCE))
    }

    #[inline]
    const fn console_must_yield(world: &PetWorld, rect: PetRect, passing: bool) -> bool {
        matches!(
            Self::ground_clearance(world, rect, 0.0, passing),
            Some(false)
        )
    }

    /// The escort's own station, corrected only where the ladder is blind.
    #[inline]
    pub(super) fn console_station(
        &self,
        desired: (f32, f32),
        sense_dims: (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        self.console.rest_seated.set(false);
        self.console.pause_hold_until.set(None);
        self.console.line_escort.set(false);
        if !self.console.presentable {
            return None;
        }
        let world = self.console.world.as_ref()?;
        world.stamp()?;
        let escorting = self.console_escorting();
        let over = self.over_the_hosts_line(world, desired, sense_dims, width);
        if escorting
            && let Some(over) = over
            && self.escort_reaches(over)
        {
            self.console.line_escort.set(true);
            return Some(
                match self.console_station_on(world, over, sense_dims, width, true, None) {
                    Some(s) => s,
                    None => over,
                },
            );
        }
        // The escort's station where no host's line is over its stand; the rest's otherwise (the
        // writing paused, or the escort's tail, [`Self::escort_reaches`]), held on top of the line
        // through a short pause.
        let escort = escorting && over.is_none();
        let hold = match over {
            Some((_, row)) => self.pause_hold(row),
            None => None,
        };
        let kept = (self.console.rest_seat.get(), self.console.rest_stand.get());
        let station = self.console_station_on(
            world,
            desired,
            sense_dims,
            width,
            escort,
            over.map(|(_, row)| row),
        );
        if let (Some((row, until)), Some((_, seat_row))) = (hold, station)
            && (seat_row - row).abs() >= 0.5
        {
            self.console.rest_seat.set(kept.0);
            self.console.rest_stand.set(kept.1);
            self.console.rest_seated.set(false);
            self.console.pause_hold_until.set(Some(until));
            return Some((self.col, row));
        }
        station
    }

    /// Whether the cat escorts the writing on top of the host's line ([`ConsoleLife::line_escort`]):
    /// whole there, never dimmed for the turn's words it walks over (the owner, 2026-10-24: drawn
    /// under that line, "I can't see it"), though a pause held it veiled over them
    /// ([`Self::pause_hold`]).
    #[inline]
    pub(super) fn escort_over_the_line(&self) -> bool {
        self.console.line_escort.get() && self.console_escorting()
    }

    /// THE LINE HOLDS THROUGH A PAUSE. A cat standing on top of the host's line (the escort's
    /// ground over a harness composer's caret, [`Self::over_the_hosts_line`] on row `row`) when
    /// the writing pauses keeps its place there, for [`PAUSE_HOLD`] from the last key or the
    /// caret's last beat, rather than fly to a rest seat on another row: the words the top of the
    /// line holds (a turn's tip) left it no clean seat along that ground, and the writing that
    /// comes back finds the cat where it stood, its head veiled over the letters meanwhile. Flown
    /// under the footer as the escort ended, it flew back up at the next key: the harness's first
    /// turn leapt down and up again in 0.75 s as it paused between bursts of its next prompt. A
    /// pause longer than the hold still seats the cat clean; an Enter ends the writing at once.
    /// Answers the row held on and when the hold ends.
    #[inline]
    fn pause_hold(&self, row: f32) -> Option<(f32, Instant)> {
        let now = self.last_now?;
        if self.flight.is_some() || self.alpha <= 0.0 || (self.row - row).abs() >= 0.5 {
            return None;
        }
        let typed = match (self.console.typed_at, self.console.typed_kind) {
            (Some(at), Some(kind)) if kind != PetInputKind::Submit => {
                PAUSE_HOLD_DUR.checked_sub(now.saturating_duration_since(at))
            }
            _ => None,
        };
        let beat = match self.rhythm_at {
            Some(at) if self.watch_heat < WATCH_GATE => {
                let left = PAUSE_HOLD - (self.clock - at) as f32;
                if left > 0.0 {
                    Some(Duration::from_secs_f32(left))
                } else {
                    None
                }
            }
            _ => None,
        };
        let left = match (typed, beat) {
            (Some(a), Some(b)) => a.max(b),
            (a, b) => a.or(b)?,
        };
        if !left.is_zero() {
            Some((row, now + left))
        } else {
            None
        }
    }

    /// THE ESCORT'S TAIL SENDS NO ONE OUT. Once the writing stops, the escort holds the cat's
    /// station on top of the host's line for [`SIT_AFTER`] more, then the rest takes it. In the
    /// escort's tail ([`ESCORT_TAIL`]) a cat on that ground off its stand there (it sat watching a
    /// deletion walk the caret back) sets out for the stand only while the escort lasts long
    /// enough for it to get there at a walk ([`LAT_SPEED`]); otherwise its station is already the
    /// rest's. Sent to the stand three
    /// cells off with 0.3 s of the escort left, it walked there and turned back as the rest
    /// took over: a turn-back no caret move made (the harness's generating take, 38.2 s).
    #[inline]
    fn escort_reaches(&self, stand: (f32, f32)) -> bool {
        if self.flight.is_some() || (self.row - stand.1).abs() >= 0.5 {
            return true;
        }
        // Before the escort's tail ([`ESCORT_TAIL`]) it runs to its stand however far it is (the
        // writing is on, or pausing between keys); a cat already on its way there goes on.
        let left = self.escort_left();
        let gap = stand.0 - self.col;
        left > ESCORT_TAIL
            || gap.abs() <= ARRIVED
            || (matches!(self.action, PetAction::Walk | PetAction::Run) && self.speed * gap > 0.0)
            || gap.abs() * (1.0 / LAT_SPEED) <= left
    }

    /// How long (s) the escort lasts from now ([`Self::console_escorting`]): its [`SIT_AFTER`]
    /// from the last key, or from the last rhythm of the caret while the watch is down.
    #[inline]
    fn escort_left(&self) -> f32 {
        let typed = match (self.console.typed_at, self.last_now) {
            (Some(at), Some(now)) => SIT_AFTER - now.saturating_duration_since(at).as_secs_f32(),
            _ => 0.0,
        };
        let rhythm = match self.rhythm_at {
            Some(at) if self.watch_heat < WATCH_GATE => SIT_AFTER - (self.clock - at) as f32,
            _ => 0.0,
        };
        typed.max(rhythm)
    }

    /// THE HEAD LAW AT THE HOST'S LINE. A stand whose 1.7-row body reaches into a row the host
    /// paints its chrome on ([`PetWorld::host_line`]: a harness's lockup and switch on a
    /// composer's upper rule, which the host reserves whole, strokes and all) is drawn under that
    /// line, its head behind the strokes and the host's words, so the person sees the line and
    /// not the cat (2026-10-24, the owner: "the cursor kitty pet is drawn BELOW the top line and I
    /// can't see it"). The line's reserved cells keep every seat's head out of it, so no stand
    /// is taken under them; while the person types ([`Self::console_escorting`]) an escort's
    /// stand whose head they would cover goes on top of the line instead, at the same column,
    /// its feet on the row over it: the cat walks the top of the composer over the caret, whole,
    /// and comes down to the caret's line where the line has room for its head (the draft's
    /// second line). Its head keeps off the letters over that row as it does beside the caret
    /// ([`Self::past_letters_above`]), so the letter veil never dims it there. Only where that
    /// stand is clear behind text past the caret, none of the host's chrome over its head; with
    /// none (a composer under the card, a rule on the pane's first rows) the ladder below seats
    /// the cat clear of the line. At rest the ladder seats it too, on clean ground first. The
    /// line coming up onto the stand carries the cat up with it ([`Self::line_ride`]).
    ///
    /// The stand is a perch over the line ([`LINE_PERCH_GAP`]): its soles clear the line by the
    /// clearance every hold asks of a body (a repair's watch, a resident's seat). With its soles on
    /// the line the stand passed and the hold did not, so the cat watching a deletion from there
    /// stepped and sat by turns, a one-frame stride every few frames.
    ///
    /// Two limits keep that stand an escort's. Its slide past the letters stays within
    /// [`LINE_SLIDE_REACH`] of the caret's column: past a longer transcript line the cat keeps the
    /// caret's column, veiled over the letters as on the caret's own line when its slide finds no
    /// room. Slid 29 columns off the caret past the transcript line over Claude Code's composer at
    /// the first move of its turn end, the stand came to lie a hop from the cat, and the hop
    /// launched at once, short of where the caret settled: two flights where the coil held through
    /// the burst makes one. The reach does not call back a cat that already stands, on the ground
    /// on that row, where the slide goes: resting past the letters, the first key would have flown
    /// it back over them to the caret's column, to run out again as the caret came on. And the
    /// stand is never taken in a rule ([`PetWorld::rule_line`]): over a reserved line that is the
    /// composer's first (the rule above it the program's), the rule's strokes would cross the
    /// cat, so the ladder seats it as it did.
    #[inline]
    const fn over_the_hosts_line(
        &self,
        world: &PetWorld,
        desired: (f32, f32),
        (rows, cols): (u16, u16),
        width: f32,
    ) -> Option<(f32, f32)> {
        let body = Self::console_stand_rect(desired, rows, width);
        let feet = PetWorld::feet_row_of(body).row;
        let mut line = None;
        let mut r = body.row.floor().max(0.0);
        while r < feet {
            if matches!(world.host_line(r as usize), Some(true)) {
                line = Some(r);
                break;
            }
            r += 1.0;
        }
        let Some(line) = line else {
            return None;
        };
        // Only where the line's reserved cells are over the head: a host's line that leaves the
        // head room (a chip at its far end) leaves the stand to the ladder.
        let head = PetRect::new(body.row, body.col, feet - body.row, body.cols);
        if !matches!(
            world.under_text_clearance_past_caret(head, 0.0),
            Some(false)
        ) {
            return None;
        }
        // Seated as every perch is, in the two rows over the line with the clearance all round
        // ([`PetWorld::nearest_clear_perch`]): its soles a sliver over the line, so a body held
        // there (a repair's watch, a resident's seat) is judged clear as the stand was.
        let over = line - 1.0;
        let row = over - LINE_PERCH_GAP;
        let lifted = Self::console_stand_rect((desired.0, row), rows, width);
        if lifted.row < CLEARANCE {
            return None;
        }
        let mut r = lifted.row.floor().max(0.0);
        let r_end = (lifted.row + lifted.rows).ceil();
        while r < r_end {
            if !matches!(world.rule_line(r as usize), Some(false)) {
                return None;
            }
            r += 1.0;
        }
        if !Self::console_may_stand(world, lifted, CLEARANCE, false) {
            return None;
        }
        let stood = self.flight.is_none() && (self.row - row).abs() < 0.5;
        let col = match self.past_letters_above(desired.0, over, cols, width) {
            Some(col)
                if (col - desired.0 <= LINE_SLIDE_REACH
                    || (stood && (self.col - col).abs() <= ARRIVED))
                    && Self::console_may_stand(
                        world,
                        Self::console_stand_rect((col, row), rows, width),
                        CLEARANCE,
                        false,
                    ) =>
            {
                col
            }
            _ => desired.0,
        };
        Some((col, row))
    }

    /// [`Self::console_station`] for a stand the host's line leaves where it is.
    #[inline]
    fn console_station_on(
        &self,
        world: &PetWorld,
        desired: (f32, f32),
        sense_dims: (u16, u16),
        width: f32,
        escorting: bool,
        walk_row: Option<f32>,
    ) -> Option<(f32, f32)> {
        let body = Self::console_stand_rect(desired, sense_dims.0, width);
        let base_feet = Self::on_its_feet(body);
        let from = self.rest_reach_from(world, sense_dims.0, width);
        let walled = matches!(
            from,
            Some(from)
                if PetWorld::same_ground(from, body)
                    && Self::console_walled(world, from, body, escorting)
        );
        if escorting
            && !walled
            && self.flight.is_none()
            && !matches!(self.action, PetAction::Walk | PetAction::Run)
            && !self.escorted_within(RHYTHM_WINDOW, RHYTHM_WINDOW_DUR)
            && (self.row - desired.1).abs() < 0.5
            && (self.col - desired.0).abs() > ARRIVED
            && Self::stand_feet_clearance(world, base_feet, false) == Some(false)
        {
            let here_feet = PetRect {
                col: self.col,
                ..base_feet
            };
            if Self::stand_feet_clearance(world, here_feet, true) != Some(false) {
                return Some((self.col, desired.1));
            }
        }
        if !walled && Self::stand_feet_clearance(world, base_feet, escorting) != Some(false) {
            self.console.rest_seat.set(None);
            return None;
        }
        // THE REST BESIDE THE LINE: chrome over the head of the stand beside the caret (the
        // harness's lockup on a composer's top rule) seats the resting cat on the caret's own
        // row, as near as the chrome allows, wherever it comes from: the escort then waits
        // there for the writing ([`AWAIT_AHEAD`]) or walks to it, and the first key costs no
        // flight. Seated by the nearest perch to where it stood instead, it rested above the
        // composer, or under the session's launch card, and flew down to the line each time
        // typing began.
        let body_passing_clear =
            !escorting && Self::stand_feet_clearance(world, base_feet, true) != Some(false);
        if body_passing_clear {
            const STEP: f32 = 0.25;
            const INV_STEP: f32 = 1.0 / STEP;
            let limit = (f32::from(sense_dims.1) - width).max(0.0);
            let steps = (POUNCE_GAP * INV_STEP) as usize;
            // Off the caret's own cell: a seat over it would hide what is being typed.
            let caret = match self.last_caret {
                Some(rc) => Some(rc),
                None => self.console.cursor_home,
            };
            let caret_bounds = match caret {
                Some((cr, cc)) if (f32::from(cr) - desired.1).abs() < 0.5 => {
                    let cc = f32::from(cc);
                    Some((cc - STATION_LEAD, cc + STATION_LEAD))
                }
                _ => None,
            };
            let mut k = 1;
            while k <= steps {
                let offset = STEP * k as f32;
                let cands = [desired.0 - offset, desired.0 + offset];
                let mut ci = 0;
                while ci < 2 {
                    let c = cands[ci];
                    ci += 1;
                    if (0.0..=limit).contains(&c) {
                        let off_caret = match caret_bounds {
                            Some((lo, hi)) => c >= hi || c + width <= lo,
                            None => true,
                        };
                        if off_caret {
                            let rect = PetRect { col: c, ..body };
                            let feet = PetRect {
                                col: c,
                                ..base_feet
                            };
                            if Self::stand_feet_clearance(world, feet, false) == Some(true)
                                && !Self::console_walled(world, body, rect, true)
                            {
                                let seat = (c, desired.1);
                                self.console.rest_seat.set(Some(seat));
                                self.console.rest_seated.set(true);
                                self.console.rest_stand.set(Some(desired));
                                return Some(seat);
                            }
                        }
                    }
                }
                k += 1;
            }
        }
        if body_passing_clear && self.flight.is_none() && (self.row - desired.1).abs() < 0.5 {
            const STEP: f32 = 0.25;
            const INV_STEP: f32 = 1.0 / STEP;
            let limit = (f32::from(sense_dims.1) - width).max(0.0);
            let steps = (REST_REACH * INV_STEP) as usize;
            let here = Self::console_stand_rect((self.col, self.row), sense_dims.0, width);
            let mut k = 1;
            while k <= steps {
                let offset = STEP * k as f32;
                let cands = [desired.0 - offset, desired.0 + offset];
                let mut ci = 0;
                while ci < 2 {
                    let c = cands[ci];
                    ci += 1;
                    if c >= 0.0 && c <= limit {
                        let rect = PetRect { col: c, ..body };
                        let feet = PetRect {
                            col: c,
                            ..base_feet
                        };
                        if Self::stand_feet_clearance(world, feet, false) == Some(true)
                            && !Self::console_walled(world, here, rect, true)
                        {
                            let seat = (c, desired.1);
                            self.console.rest_seat.set(Some(seat));
                            self.console.rest_seated.set(true);
                            self.console.rest_stand.set(Some(desired));
                            return Some(seat);
                        }
                    }
                }
                k += 1;
            }
        }
        if !walled && {
            let margin = world.margin_in_map(base_feet, CLEARANCE);
            world.ground_under_text_clearance_past_caret(base_feet, margin) != Some(false)
        } {
            let next = world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH)?;
            return Some((next.col, next.foot_row()));
        }
        let resting = if self.flight.is_none() && self.alpha > 0.0 {
            Some((self.col, self.row))
        } else {
            None
        };
        if !escorting
            && let Some(walk_row) = walk_row
            && let Some(seat) = self.rest_where_typing_walks(
                world,
                desired,
                body,
                sense_dims.0,
                width,
                resting,
                walk_row,
            )
        {
            return Some(seat);
        }
        let held_seat = self.console.rest_seat.get();
        if let Some(seat) = self.seat_lost_on_foot(
            world,
            desired,
            body,
            sense_dims.0,
            width,
            resting,
            held_seat,
            from,
        ) {
            return Some(seat);
        }
        if let Some((col, row)) = resting
            && (row - desired.1).abs() < 0.5
            && !matches!(held_seat, Some((_, seat_row)) if (seat_row - desired.1).abs() >= 0.5)
            && (desired.0 - col).abs() <= REST_REACH
            && Self::console_may_wait(world, body, base_feet, col, from, escorting)
        {
            const STEP: f32 = 0.25;
            const INV_STEP: f32 = 1.0 / STEP;
            let toward = STEP * (col - desired.0).signum();
            let steps = ((desired.0 - col).abs() * INV_STEP).floor() as usize;
            let mut wait = col;
            let mut k = 1;
            while k <= steps {
                let c = desired.0 + toward * k as f32;
                if Self::console_may_wait(world, body, base_feet, c, from, escorting) {
                    wait = c;
                    break;
                }
                k += 1;
            }
            let seat = (wait, desired.1);
            self.console.rest_seat.set(Some(seat));
            self.console.rest_seated.set(true);
            self.console.rest_stand.set(Some(desired));
            return Some(seat);
        }
        #[inline]
        const fn pane_reach(body: PetRect, sense_dims: (u16, u16)) -> f32 {
            let across = body.col.abs().max((sense_dims.1 as f32 - body.col).abs());
            crate::color_math::hypot_f32(2.0 * (sense_dims.0 as f32), across)
        }
        // A SEAT GOES WITH ITS CARET. The seat the cat holds, or rests in, is kept while the
        // world changes round a caret that stays put (a command list shrinking under it), but
        // once the caret takes the stand to another row, or jumps along its row, it is kept only
        // while it is as near the new stand as the nearest clean seat ([`SEAT_KEEP_SLACK`]).
        // Kept anywhere in reach, the rest depended on where the cat came from: a seat taken
        // over the session's first composer, before Claude Code's first turn pushed it six rows
        // down, held the cat under the launch card until typing began, and a seat taken for a
        // caret's first frame held it 34 cells along the composer from the prompt.
        let stand_moved = held_seat.is_some()
            && matches!(
                self.console.rest_stand.get(),
                Some((c, r)) if (r - desired.1).abs() >= 0.5 || (c - desired.0).abs() >= POUNCE_JUMP
            );
        let resting = if resting == held_seat { None } else { resting };
        let mut nearest: Option<Option<PetRect>> = None;
        let mut clear_next = match held_seat {
            Some(s) => Self::console_keep_clear(world, s, sense_dims.0, width, body, from, true),
            None => None,
        };
        if stand_moved
            && let Some(k) = clear_next
            && !Self::seat_within_slack(k, body)
        {
            let n = *nearest.get_or_insert_with(|| {
                world.nearest_clear_perch(body, CLEARANCE, REST_REACH, from)
            });
            clear_next = Self::seat_as_near(clear_next, n, body);
        }
        if clear_next.is_none()
            && let Some(s) = resting
        {
            clear_next = Self::console_keep_clear(world, s, sense_dims.0, width, body, from, true);
            if stand_moved
                && let Some(k) = clear_next
                && !Self::seat_within_slack(k, body)
            {
                let n = *nearest.get_or_insert_with(|| {
                    world.nearest_clear_perch(body, CLEARANCE, REST_REACH, from)
                });
                clear_next = Self::seat_as_near(clear_next, n, body);
            }
        }
        if clear_next.is_none() {
            clear_next = *nearest.get_or_insert_with(|| {
                world.nearest_clear_perch(body, CLEARANCE, REST_REACH, from)
            });
        }
        if clear_next.is_none()
            && let Some(s) = held_seat
        {
            clear_next = Self::console_keep_clear(world, s, sense_dims.0, width, body, from, false);
        }
        if clear_next.is_none()
            && let Some(s) = resting
        {
            clear_next = Self::console_keep_clear(world, s, sense_dims.0, width, body, from, false);
        }
        if clear_next.is_none() {
            // The search requires a finite reach. Cover the whole pane,
            // including a stand whose column lies outside it.
            clear_next =
                world.nearest_clear_perch(body, CLEARANCE, pane_reach(body, sense_dims), from);
        }
        if let Some(next) = clear_next {
            let seat = (next.col, next.foot_row());
            self.console.rest_seat.set(Some(seat));
            self.console.rest_seated.set(true);
            self.console.rest_stand.set(Some(desired));
            return Some(seat);
        }
        let mut next = match held_seat {
            Some(s) => Self::console_keep_under(world, s, sense_dims.0, width, body, from, true),
            None => None,
        };
        if next.is_none()
            && let Some(s) = resting
        {
            next = Self::console_keep_under(world, s, sense_dims.0, width, body, from, true);
        }
        if next.is_none() {
            next = world.nearest_under_text_perch_from(body, CLEARANCE, HOME_REACH, from);
        }
        if next.is_none()
            && let Some(s) = held_seat
        {
            next = Self::console_keep_under(world, s, sense_dims.0, width, body, from, false);
        }
        if next.is_none()
            && let Some(s) = resting
        {
            next = Self::console_keep_under(world, s, sense_dims.0, width, body, from, false);
        }
        if next.is_none() {
            next = world.nearest_under_text_perch_from(
                body,
                CLEARANCE,
                pane_reach(body, sense_dims),
                from,
            );
        }
        if let Some(next) = next {
            let seat = (next.col, next.foot_row());
            self.console.rest_seat.set(Some(seat));
            self.console.rest_seated.set(true);
            self.console.rest_stand.set(Some(desired));
            return Some(seat);
        }
        self.console.rest_seat.set(None);
        None
    }

    /// THE REST WHERE THE TYPING WALKS. Beside a composer whose upper rule is the host's line
    /// ([`Self::over_the_hosts_line`]: the harness's lockup and switch), the escort walks the top
    /// of the composer over the caret, and the cat at rest is seated where that walk starts: on
    /// top of the line, at the nearest column whose head room is clear of words (past the end of
    /// the transcript's last line), as far along the composer as the ground over the line runs
    /// unwalled from the stand over the caret and the rest reach goes ([`REST_REACH`]). The first
    /// key then finds the cat on the escort's ground: it waits there for the writing, or walks to
    /// it. Seated on the nearest clean ground instead (under the footer, three rows from the
    /// caret), it flew up to the line at each start of typing and down again at each pause.
    ///
    /// Only ground that has held its words for [`WALK_SEAT_SETTLED`] is taken: the line a spinner
    /// turns on, or a screen still being drawn, would cover the seat as it counts on. A seat held
    /// there, or rested in, is kept while it stays clean ([`SEAT_KEPT`]); it goes with the
    /// composer, rows and all, and only a caret that jumps along its row takes the stand off it
    /// ([`SEAT_KEEP_SLACK`]). Where the top of the line holds no such seat the ladder seats the
    /// cat as before.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn rest_where_typing_walks(
        &self,
        world: &PetWorld,
        desired: (f32, f32),
        desired_body: PetRect,
        rows: u16,
        width: f32,
        resting: Option<(f32, f32)>,
        walk_row: f32,
    ) -> Option<(f32, f32)> {
        let stand = Self::console_stand_rect((desired.0, walk_row), rows, width);
        // A seat kept elsewhere stays kept: the cat is not sent across the composer by ground
        // that has only now settled round a caret that has not moved.
        let still = matches!(
            self.console.rest_stand.get(),
            Some((c, r)) if (r - desired.1).abs() < 0.5 && (c - desired.0).abs() < POUNCE_JUMP
        );
        if let Some(seat) = self.console.rest_seat.get()
            && still
            && (seat.1 - walk_row).abs() >= 0.5
            && Self::console_keep_clear(world, seat, rows, width, desired_body, None, true)
                .is_some()
        {
            return None;
        }
        #[inline]
        const fn on_walk(seat: (f32, f32), walk_row: f32) -> bool {
            (seat.1 - walk_row).abs() < 0.5
        }
        #[inline]
        const fn kept_seat(
            world: &PetWorld,
            seat: (f32, f32),
            rows: u16,
            width: f32,
            stand: PetRect,
        ) -> Option<PetRect> {
            let held = PetBrain::console_stand_rect(seat, rows, width);
            if (held.col - stand.col).abs() <= REST_REACH {
                world.nearest_clear_perch_along(held, CLEARANCE, SEAT_KEPT, Some(stand))
            } else {
                None
            }
        }
        // The seat went down (or up) with the composer the caret is in: held on top of its line,
        // it is the same seat on the line where it is now.
        let (dr, jumped) = match self.console.rest_stand.get() {
            Some((c, r)) => (desired.1 - r, (c - desired.0).abs() >= POUNCE_JUMP),
            None => (0.0, false),
        };
        let held = match self.console.rest_seat.get() {
            Some((c, r)) if on_walk((c, r + dr), walk_row) => {
                kept_seat(world, (c, r + dr), rows, width, stand)
            }
            _ => None,
        };
        let held = match held {
            Some(seat) => Some(seat),
            None => match resting {
                Some(pos) if on_walk(pos, walk_row) => kept_seat(world, pos, rows, width, stand),
                _ => None,
            },
        };
        // Where the ground over the line has not settled, the ladder chooses (and weighs a seat
        // held against the stand the caret jumped to).
        let next = match held {
            Some(seat) if jumped => {
                let near = if self.rows_settled(world, stand, WALK_SEAT_SETTLED) {
                    world.nearest_clear_perch_along(stand, CLEARANCE, REST_REACH, Some(stand))
                } else {
                    None
                }?;
                Self::seat_as_near(Some(seat), Some(near), stand)
            }
            Some(seat) => Some(seat),
            None => {
                if self.rows_settled(world, stand, WALK_SEAT_SETTLED) {
                    world.nearest_clear_perch_along(stand, CLEARANCE, REST_REACH, Some(stand))
                } else {
                    None
                }
            }
        }?;
        let seat = (next.col, next.foot_row());
        self.console.rest_seat.set(Some(seat));
        self.console.rest_seated.set(true);
        self.console.rest_stand.set(Some(desired));
        Some(seat)
    }

    /// A SEAT LOST TO THE WORDS IS LEFT ON FOOT. A cat at rest whose seat the screen covers round
    /// a caret that stays put (a status line counting on over it, a tip laid beside it) walks
    /// along its own ground to the nearest clean seat there, within the rest reach and the ground
    /// it can walk ([`PetWorld::nearest_clear_perch_along`]), before any seat on another row is
    /// flown to: seated beside a spinner's new line as a turn began, the cat flew a row up when
    /// the line counted its first second over it, the turn's second flight in under a second.
    /// A caret that moved the stand takes the seat with it instead ([`SEAT_KEEP_SLACK`]).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn seat_lost_on_foot(
        &self,
        world: &PetWorld,
        desired: (f32, f32),
        desired_body: PetRect,
        rows: u16,
        width: f32,
        resting: Option<(f32, f32)>,
        held_seat: Option<(f32, f32)>,
        from: Option<PetRect>,
    ) -> Option<(f32, f32)> {
        let here = resting?;
        let seat = held_seat?;
        let still = matches!(
            self.console.rest_stand.get(),
            Some((c, r)) if (r - desired.1).abs() < 0.5 && (c - desired.0).abs() < POUNCE_JUMP
        );
        if !still
            || (seat.0 - here.0).abs() > ARRIVED
            || (seat.1 - here.1).abs() >= 0.5
            || Self::console_keep_clear(world, seat, rows, width, desired_body, from, false)
                .is_some()
        {
            return None;
        }
        let body = Self::console_stand_rect(here, rows, width);
        let next = world.nearest_clear_perch_along(body, CLEARANCE, REST_REACH, Some(body))?;
        // Words that grew onto the seat grow on (a status line counting its seconds): the cat
        // goes a little past them where the ground allows, not to the cell their end leaves.
        let past = PetRect {
            col: next.col + LOST_SEAT_ROOM * (next.col - body.col).signum(),
            ..next
        };
        let next = if past.col >= 0.0
            && world
                .nearest_clear_perch_along(past, CLEARANCE, SEAT_KEPT, Some(body))
                .is_some()
        {
            past
        } else {
            next
        };
        let seat = (next.col, next.foot_row());
        self.console.rest_seat.set(Some(seat));
        self.console.rest_seated.set(true);
        self.console.rest_stand.set(Some(desired));
        Some(seat)
    }

    #[inline]
    const fn seat_span_sq(seat: PetRect, body: PetRect) -> f32 {
        let dr = 2.0 * (seat.row - body.row);
        let dc = seat.col - body.col;
        dr * dr + dc * dc
    }

    #[inline]
    const fn seat_within_slack(seat: PetRect, body: PetRect) -> bool {
        Self::seat_span_sq(seat, body) <= SEAT_KEEP_SLACK * SEAT_KEEP_SLACK
    }

    /// `kept`, a seat held or rested in, while it lies no further from the stand `body` than
    /// the nearest clean seat does by [`SEAT_KEEP_SLACK`] (cells, rows counted twice).
    #[inline]
    const fn seat_as_near(
        kept: Option<PetRect>,
        nearest: Option<PetRect>,
        body: PetRect,
    ) -> Option<PetRect> {
        match (kept, nearest) {
            (Some(k), Some(n)) => {
                let k_sq = Self::seat_span_sq(k, body);
                let n_sq = Self::seat_span_sq(n, body);
                if k_sq > n_sq
                    && k_sq > SEAT_KEEP_SLACK * SEAT_KEEP_SLACK
                    && crate::color_math::sqrt_f32(k_sq)
                        > crate::color_math::sqrt_f32(n_sq) + SEAT_KEEP_SLACK
                {
                    None
                } else {
                    kept
                }
            }
            _ => kept,
        }
    }

    #[inline]
    const fn console_may_wait(
        world: &PetWorld,
        body: PetRect,
        base_feet: PetRect,
        col: f32,
        from: Option<PetRect>,
        escorting: bool,
    ) -> bool {
        let rect = PetRect { col, ..body };
        let feet = PetRect { col, ..base_feet };
        matches!(
            Self::stand_feet_clearance(world, feet, escorting),
            Some(true)
        ) && !matches!(from, Some(from) if Self::console_walled(world, from, rect, escorting))
    }

    #[inline]
    const fn console_keep_clear(
        world: &PetWorld,
        seat: (f32, f32),
        rows: u16,
        width: f32,
        body: PetRect,
        from: Option<PetRect>,
        within_reach: bool,
    ) -> Option<PetRect> {
        const REST_REACH_SQ: f32 = REST_REACH * REST_REACH;
        let held = Self::console_stand_rect(seat, rows, width);
        if within_reach {
            let dr = 2.0 * (held.row - body.row);
            let dc = held.col - body.col;
            if dr * dr + dc * dc > REST_REACH_SQ {
                return None;
            }
        }
        world.nearest_clear_perch(held, CLEARANCE, SEAT_KEPT, from)
    }

    #[inline]
    const fn console_keep_under(
        world: &PetWorld,
        seat: (f32, f32),
        rows: u16,
        width: f32,
        body: PetRect,
        from: Option<PetRect>,
        within_reach: bool,
    ) -> Option<PetRect> {
        const REST_REACH_SQ: f32 = REST_REACH * REST_REACH;
        let held = Self::console_stand_rect(seat, rows, width);
        if within_reach {
            let dr = 2.0 * (held.row - body.row);
            let dc = held.col - body.col;
            if dr * dr + dc * dc > REST_REACH_SQ {
                return None;
            }
        }
        world.nearest_under_text_perch_from(held, CLEARANCE, SEAT_KEPT, from)
    }

    #[inline]
    fn rest_reach_from(&self, world: &PetWorld, rows: u16, width: f32) -> Option<PetRect> {
        if self.alpha <= 0.0 {
            return None;
        }
        let seat = match &self.flight {
            Some(f) => (f.to_col, f.to_row),
            None => (self.col, self.row),
        };
        let body = Self::console_stand_rect(seat, rows, width);
        let judged = if self.console_ground_passing() {
            PetWorld::feet_row_of(body)
        } else {
            body
        };
        match world.ground_under_text_clearance_past_caret(judged, 0.0) {
            Some(true) => Some(body),
            Some(false) => world.nearest_under_text_perch(body, CLEARANCE, HOME_REACH),
            None => None,
        }
    }

    /// Whether a flight's aim leads by the caret's drift: a rhythm is open and the station follows the caret.
    #[inline]
    pub(super) const fn aims_ahead(&self) -> bool {
        self.rhythm_open() && !self.console.rest_seated.get()
    }

    #[inline]
    const fn console_walled(world: &PetWorld, from: PetRect, to: PetRect, passing: bool) -> bool {
        if passing {
            // The way the feet go: chrome over the ground is walked under
            // ([`Self::console_escorting`]), chrome on it or across it is a
            // wall.
            world.ground_corridor_walled(PetWorld::feet_row_of(from), PetWorld::feet_row_of(to))
        } else {
            world.ground_corridor_walled(from, to)
        }
    }

    /// The tick's body moved with the rows round it ([`PetBrain::carry_with_regrid`]): the console's own content ride, by another road.
    #[inline]
    pub(super) const fn note_content_ride(&mut self) {
        self.console.reason = "content-ride";
    }

    /// Whether the console saw `row` drawn as a horizontal rule ([`PetWorld::rule_line`]), whose
    /// strokes would cross a body standing in it. False where it saw nothing.
    #[inline]
    pub(super) const fn console_rule_at(&self, row: f32) -> bool {
        if !self.console.presentable || !row.is_finite() || row < -0.5 {
            return false;
        }
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        matches!(world.rule_line(row.round() as usize), Some(true))
    }

    /// Whether `row` divides a composer's writing from the rows past it: drawn as a rule
    /// ([`Self::console_rule_at`]), or the host's line, its chrome laid on the rule (the footer
    /// chips on a composer's lower rule leave too few strokes for a rule's, [`PetWorld::host_line`]).
    #[inline]
    pub(super) const fn console_divides_at(&self, row: f32) -> bool {
        if self.console_rule_at(row) {
            return true;
        }
        if !self.console.presentable || !row.is_finite() || row < -0.5 {
            return false;
        }
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        matches!(world.host_line(row.round() as usize), Some(true))
    }

    /// Whether the console lets a cat rest at `desired` (`(col, foot row)`) and settle there: no lasting protected cell (the host's chrome; a selection or an overlay passes) under its whole body, as a rest is judged when no escort passes under the host's labels, nor [`ARRIVED`] to either side of it, where the walk that brings it there may stop. Unobserved ground and a console that is not presentable admit it.
    #[inline]
    pub(super) const fn console_admits_rest(
        &self,
        desired: (f32, f32),
        rows: u16,
        width: f32,
    ) -> bool {
        if !self.console.presentable {
            return true;
        }
        let Some(world) = self.console.world.as_ref() else {
            return true;
        };
        if world.stamp().is_none() {
            return true;
        }
        let body = Self::console_stand_rect(desired, rows, width);
        let base_feet = Self::on_its_feet(body);
        let dxs = [0.0, -ARRIVED, ARRIVED];
        let mut i = 0;
        while i < dxs.len() {
            let feet = PetRect {
                col: base_feet.col + dxs[i],
                ..base_feet
            };
            let margin = world.margin_in_map(feet, CLEARANCE);
            if matches!(
                world.ground_under_text_clearance_past_caret(feet, margin),
                Some(false)
            ) {
                return false;
            }
            i += 1;
        }
        true
    }

    /// Whether a body at rest at `seat` (`(col, foot row)`) would have to yield its ground
    /// ([`Self::place_console_body_at_home`]): chrome the host reserved over it since the cat
    /// chose it.
    #[inline]
    pub(super) fn console_yields_at(&self, seat: (f32, f32), sense: PetSense) -> bool {
        if !self.console.presentable {
            return false;
        }
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        if world.stamp().is_none() {
            return false;
        }
        let rect = still_body_rect(
            seat,
            Stance::NEUTRAL,
            (sense.cell_w, sense.cell_h),
            (sense.cols, sense.rows),
        );
        Self::console_must_yield(world, rect, self.console_ground_passing())
    }

    /// Whether the host's frame pass would turn back a walk to `seat` (`(col, foot row)`) or the
    /// rest it ends in there ([`Self::place_console_body_at_home`]): the body there must yield its
    /// ground as one passing (its feet's row, and the rows over them against every protection,
    /// the host's chrome included) or as one at rest. A cat on a cell size of `cell` in a grid
    /// of `grid` (`(cols, rows)`); false where the console has no coherent reading.
    #[inline]
    pub(super) const fn console_turns_back(
        &self,
        seat: (f32, f32),
        cell: (u16, u16),
        grid: (u16, u16),
    ) -> bool {
        if !self.console.presentable {
            return false;
        }
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        if world.stamp().is_none() {
            return false;
        }
        let rect = still_body_rect(seat, Stance::NEUTRAL, cell, grid);
        match world.under_text_clearance_past_caret(rect, 0.0) {
            Some(clear) => !clear,
            None => Self::console_must_yield(world, rect, true),
        }
    }

    /// Whether the host's frame pass carries a body at rest at `seat` with the rows it moved with
    /// ([`Self::content_ride`], [`Self::line_ride`]) where it must yield its ground there.
    #[inline]
    pub(super) const fn console_carries_at(&self, seat: (f32, f32), sense: PetSense) -> bool {
        let Some(world) = self.console.world.as_ref() else {
            return false;
        };
        if world.stamp().is_none() {
            return false;
        }
        let rect = still_body_rect(
            seat,
            Stance::NEUTRAL,
            (sense.cell_w, sense.cell_h),
            (sense.cols, sense.rows),
        );
        self.console_ride(world, rect).is_some()
            || self.old_path(OldPath::ChromeOverIsHoppedOff)
                && (self.content_ride(rect).is_some() || self.line_ride(world, rect).is_some())
    }

    /// The ride the host's frame pass carries a body at `rect` with
    /// ([`Self::place_console_body_at_home`]), `dy` rows: its rows moved with the page (a scroll, a
    /// composer that grew a row up under it, [`Self::content_ride`]) onto ground clear of text, or
    /// the line its feet stand on moved ([`Self::line_ride`]) onto a stand it may take; `None`
    /// for no ride, or one onto ground it may not take, which the host does not carry it by. The
    /// brain leaves a body the host carries to the host ([`Self::console_carries_at`]), so it is
    /// this ride and no other: a body whose rows moved onto ground it may not take (a composer
    /// that shrank under the cat on its second line, laying its top rule over the cat's head,
    /// as Claude Code clears its draft on exit) was left to a host that did not carry it, and the
    /// host put it on the nearest perch in one frame, a row down ([`OldPath::ChromeOverIsHoppedOff`]).
    #[inline]
    pub(super) const fn console_ride(&self, world: &PetWorld, rect: PetRect) -> Option<f32> {
        if let Some(dy) = self.content_ride(rect) {
            let ridden = PetRect {
                row: rect.row + dy,
                ..rect
            };
            return if world.under_text_clear(ridden, CLEARANCE) {
                Some(dy)
            } else {
                None
            };
        }
        // Its feet on the line: judged as a stand, whose margin stops at its soles.
        let Some(dy) = self.line_ride(world, rect) else {
            return None;
        };
        let ridden = PetRect {
            row: rect.row + dy,
            ..rect
        };
        if matches!(Self::stand_clearance(world, ridden, false), Some(true)) {
            Some(dy)
        } else {
            None
        }
    }

    /// Whether the console bars the escort from a stand at `desired` (`(col, foot row)`): protected ground under it, or between the body on glass and it ([`Self::console_walled`]).
    #[inline]
    pub(super) fn console_bars(&self, desired: (f32, f32), rows: u16, width: f32) -> bool {
        if !self.console.presentable {
            return false;
        }
        let Some(world) = &self.console.world else {
            return false;
        };
        if world.stamp().is_none() {
            return false;
        }
        let body = Self::console_stand_rect(desired, rows, width);
        let escorting = self.console_escorting();
        matches!(Self::stand_clearance(world, body, escorting), Some(false))
            || matches!(
                self.rest_reach_from(world, rows, width),
                Some(from) if Self::console_walled(world, from, body, escorting)
            )
    }

    #[inline]
    const fn console_stand_rect(desired: (f32, f32), rows: u16, width: f32) -> PetRect {
        PetRect::new(
            (desired.1 + 1.0 - ART_ROWS).clamp(
                CLEARANCE,
                (rows as f32 - ART_ROWS - CLEARANCE).max(CLEARANCE),
            ),
            desired.0,
            ART_ROWS,
            width,
        )
    }

    #[inline]
    fn console_caret_home(&self, sense: PetSense, width: f32) -> Option<PetRect> {
        let (r, c) = match sense.caret {
            Some(rc) => rc,
            None => self.console.cursor_home?,
        };
        let want = if self.wall_side && f32::from(sense.cols) - width > 0.0 {
            (f32::from(c) - width - STATION_LEAD - self.standoff()).max(0.0)
        } else {
            Self::station_with(c, sense.cols, width, self.standoff())
        };
        let stand = self.ink_stand_for(
            want,
            f32::from(r),
            width,
            sense.cols,
            sense.rows,
            Some((r, c)),
        );
        let stand = match self.console_station(stand, (sense.rows, sense.cols), width) {
            Some(s) => s,
            None => stand,
        };
        Some(Self::console_stand_rect(stand, sense.rows, width))
    }

    #[inline]
    fn caret_seat(&self, world: &PetWorld, body: PetRect, stand: PetRect) -> CaretSeat {
        if self.speed.abs() <= 0.01
            && self.carry_speed() < FLIP_SPEED
            && self.at_escort_stand(body, stand)
            && Self::console_may_stand(world, body, CLEARANCE, self.console_ground_passing())
        {
            CaretSeat::Hold
        } else if !Self::console_must_yield(world, stand, self.console_escorting()) {
            CaretSeat::Escort
        } else {
            CaretSeat::Yield
        }
    }

    #[inline]
    const fn at_escort_stand(&self, body: PetRect, stand: PetRect) -> bool {
        if Self::at_console_stand(body, stand) {
            return true;
        }
        let same_home = matches!(
            (self.held_for, self.console.cursor_home),
            (Some((r0, c0)), Some((r1, c1))) if r0 == r1 && c0 == c1
        );
        let lead = if !self.wall_side && !self.console.rest_seated.get() && same_home {
            self.lead()
        } else {
            0.0
        };
        lead > 0.0
            && (body.row - stand.row).abs() < 0.25
            && body.col >= stand.col - ARRIVED
            && body.col <= stand.col + lead + ARRIVED
    }

    #[inline]
    fn console_seat(
        &self,
        world: &PetWorld,
        body: PetRect,
        home: Option<PetRect>,
    ) -> Option<PetRect> {
        match home {
            Some(stand) => match self.caret_seat(world, body, stand) {
                CaretSeat::Hold => Some(body),
                CaretSeat::Escort => Some(stand),
                CaretSeat::Yield => world.nearest_under_text_perch(stand, CLEARANCE, HOME_REACH),
            },
            None if Self::console_may_stand(
                world,
                body,
                CLEARANCE,
                self.console_ground_passing(),
            ) =>
            {
                Some(body)
            }
            None => world.home_perch(body, CLEARANCE, HOME_REACH),
        }
    }

    #[inline]
    const fn at_console_stand(body: PetRect, stand: PetRect) -> bool {
        (body.col - stand.col).abs() <= ARRIVED && (body.row - stand.row).abs() < 0.25
    }

    #[inline]
    const fn near_console_home(rect: PetRect, home: Option<PetRect>) -> bool {
        match home {
            None => true,
            Some(home) => {
                (rect.col - home.col)
                    .abs()
                    .max((rect.row - home.row).abs() * 2.0)
                    <= HOME_REACH
            }
        }
    }

    #[inline]
    fn place_console_body_at_home(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if self.console.clipped || frame.alpha == 0 {
            return;
        }
        // A flight about to leave ([`Self::begin_arc`] holds a pose just put on glass its still
        // time first), or one that touched down this tick on ground the chrome took while it
        // flew and hops off it on the next, is passing still: moved off the chrome in that
        // frame, the cat that was to hop off it jumped on the spot and then hopped.
        if passing(frame) || self.deferred_arc.is_some() || self.touched_on_chrome {
            return;
        }
        let Some(world) = self.console.world.as_ref() else {
            return;
        };
        let Some(rect) = body_rect(frame, sense) else {
            return;
        };
        let passing = self.console_ground_passing();
        if !Self::console_must_yield(world, rect, passing) {
            return;
        }
        if (frame.scale_x != 1.0 || frame.scale_y != 1.0) && {
            let natural = still_body_rect(
                (frame.col, frame.row),
                Stance {
                    scale_x: 1.0,
                    scale_y: 1.0,
                    lift: frame.lift,
                    from: StretchFrom::Centre,
                },
                (sense.cell_w, sense.cell_h),
                (sense.cols, sense.rows),
            );
            !Self::console_must_yield(world, natural, passing)
        } {
            return;
        }
        if let Some(dy) = self.console_ride(world, rect) {
            let ridden = PetFrame {
                row: frame.row + dy,
                ..*frame
            };
            self.carry_console_body(frame, ridden, dy);
            self.console.reason = "content-ride";
            return;
        }
        let Some(safe) = world.nearest_under_text_perch(rect, CLEARANCE, HOME_REACH) else {
            self.console.clipped = true;
            self.console.reason = "protected-home-unavailable";
            return;
        };
        let cw = f32::from(sense.cell_w.max(1));
        let ch = f32::from(sense.cell_h.max(1));
        let inv_cw = 1.0 / cw;
        let inv_ch = 1.0 / ch;
        let natural_h = (ART_ROWS * ch).round();
        let natural_w = ((natural_h * ART_ASPECT).round() as i32).clamp(1, i32::from(u16::MAX));
        let dest_w = (rect.cols * cw).round() as i32;
        let dest_h = (rect.rows * ch).round();
        let safe_bottom_px = (safe.row * ch).round() + dest_h;
        let ground_row = safe_bottom_px * inv_ch - 1.0;
        let shifted = PetFrame {
            col: ((safe.col * cw).round() - (natural_w / 2) as f32 + (dest_w / 2) as f32) * inv_cw,
            row: (safe_bottom_px + (frame.lift * ch).round()) * inv_ch - 1.0,
            ..*frame
        };
        if !matches!(body_rect(&shifted, sense), Some(r) if world.under_text_clear(r, 0.0)) {
            self.console.clipped = true;
            self.console.reason = "protected-home-rounding";
            return;
        }
        let ground_dy = ground_row - frame.row;
        if matches!(self.action, PetAction::Walk | PetAction::Run)
            && (shifted.col - frame.col) * self.speed < 0.0
        {
            self.walk_turned_back();
        }
        self.carry_console_body(frame, shifted, ground_dy);
        self.console.reason = "cursor-home";
    }

    /// Move the drawn body to `to` and every motion coordinate with it, or the next tick's lerp would restore the body to where it was.
    #[inline]
    pub(super) const fn carry_console_body(
        &mut self,
        frame: &mut PetFrame,
        to: PetFrame,
        ground_dy: f32,
    ) {
        let (dx, dy) = (to.col - frame.col, to.row - frame.row);
        let old_seat = (self.col, self.row);
        *frame = to;
        self.col += dx;
        self.row += ground_dy;
        self.home += dx;
        self.col_at_tick += dx;
        self.skid_from += dx;
        if let Some(over) = &mut self.brake_over {
            *over += dx;
        }
        if let Some((tc, tr)) = &mut self.play_to
            && (*tc - old_seat.0).abs() <= 1e-4
            && (*tr - old_seat.1).abs() <= 1e-4
        {
            *tc += dx;
            *tr += ground_dy;
        }
        if let Some(look) = &mut self.adopted_look {
            look.seat.0 += dx;
            look.seat.1 += ground_dy;
        }
        let mut i = 0;
        while i < self.departures.len() {
            if let Some(record) = &mut self.departures[i]
                && record.still
                && !record.resight
            {
                record.from_col += dx;
                record.edge += dx;
                record.row += ground_dy;
                if let Some(drawn) = &mut frame.departures[i] {
                    drawn.col += dx;
                    drawn.row += dy;
                }
            }
            i += 1;
        }
        if let Some(flight) = &mut self.flight {
            flight.from_col += dx;
            flight.to_col += dx;
            flight.from_row += ground_dy;
            flight.to_row += ground_dy;
        }
        if let Some((col, row, _)) = &mut self.bound2 {
            *col += dx;
            *row += ground_dy;
        }
        if let Some((col, row)) = &mut self.deferred_hidden_landing {
            *col += dx;
            *row += ground_dy;
        }
        if let Some(target) = &mut self.console.target {
            target.col += dx;
            target.row += ground_dy;
        }
        if let Some(trip) = &mut self.console.trip {
            trip.from.0 += dx;
            trip.to.0 += dx;
            trip.from.1 += ground_dy;
            trip.to.1 += ground_dy;
        }
    }

    #[inline]
    pub(super) fn finish_console_frame(&mut self, frame: &mut PetFrame, sense: PetSense) {
        if !self.console.presentable {
            self.console_posed = false;
            self.console.mote_gate = MoteGate::Open;
            self.console.hidden_motes = [false; PET_MOTES_MAX];
            return;
        }
        let mut drawn_mask = 0u8;
        let mut i = 0;
        while i < PET_MOTES_MAX {
            if frame.motes[i].is_some() {
                drawn_mask |= 1 << i;
            }
            i += 1;
        }
        self.console.mote_gate = MoteGate::Open;
        self.console_frame_pass(frame, sense, drawn_mask != 0);
        if drawn_mask == 0 {
            self.console.hidden_motes = [false; PET_MOTES_MAX];
            return;
        }
        let mut i = 0;
        while i < PET_MOTES_MAX {
            self.console.hidden_motes[i] = (drawn_mask & (1 << i)) != 0 && frame.motes[i].is_none();
            i += 1;
        }
    }

    #[inline]
    fn console_frame_pass(&mut self, frame: &mut PetFrame, sense: PetSense, has_motes: bool) {
        self.console_posed = false;
        if !self.console.presentable {
            return;
        }
        if let Some(pose) = self.console.pose {
            let travelling = matches!(self.action, PetAction::Walk | PetAction::Run);
            if !self.console_legacy_motion_pending()
                && self.action != PetAction::Land
                && (self.console.resident
                    || (self.console.attention == PetAttention::Contact && !travelling))
            {
                frame.pose = self.species.skin(pose);
                self.last_pose = pose;
                frame.scale_x = 1.0;
                frame.scale_y = 1.0;
                frame.purr = 0.0;
                self.console_posed = true;
            }
        }
        if self.console.resident {
            if !self.console_legacy_motion_pending() && self.action != PetAction::Land {
                frame.lift = self.console.lift;
            }
            frame.motes = [None; PET_MOTES_MAX];
            self.console.mote_gate = MoteGate::Shut;
        }
        self.place_console_body_at_home(frame, sense);
        let Some(world) = self.console.world.as_ref() else {
            let mut i = 0;
            while i < PET_DEPARTURES_MAX {
                if matches!(self.departures[i], Some(d) if d.resight) {
                    frame.departures[i] = None;
                }
                i += 1;
            }
            return;
        };
        if world.stamp().is_none() {
            frame.motes = [None; PET_MOTES_MAX];
            self.console.mote_gate = MoteGate::Shut;
            self.console.attention = PetAttention::Yielding;
            self.console.reason = "incoherent-surface";
            if self.console.on_glass && frame.alpha > 0 {
                let cover = self.console.hold.update(sense.now, None);
                frame.alpha = cover_alpha(frame.alpha, cover);
                frame.lane_alpha = cover_alpha(frame.lane_alpha, cover);
                frame.under_ink |= self.console.z.under;
                let mut i = 0;
                while i < PET_DEPARTURES_MAX {
                    if matches!(self.departures[i], Some(d) if d.resight) {
                        frame.departures[i] = None;
                    }
                    i += 1;
                }
                if frame.alpha == 0 {
                    frame.departures = [None; PET_DEPARTURES_MAX];
                }
                self.console.on_glass = frame.alpha > 0;
                return;
            }
            frame.alpha = 0;
            frame.lane_alpha = 0;
            frame.departures = [None; PET_DEPARTURES_MAX];
            self.console.clipped = true;
            self.console.hold.idle(sense.now);
            self.console.z = ZHold::default();
            return;
        }
        let body = body_rect(&self.head_drawn(frame), sense);
        if let Some(rect) = body {
            self.console.last_body = Some(rect);
            self.console.last_seat = Some((self.col, self.row));
        }
        let selection = world.selection_rect();
        let selected = matches!(
            (body, selection),
            (Some(b), Some(s)) if b.overlaps(s)
        );
        if self.console.clipped || selected {
            frame.alpha = 0;
            frame.lane_alpha = 0;
            self.console.hold.hide_now(sense.now);
            self.console.z = ZHold::default();
            self.console.clipped = true;
            if !self.console.reason.starts_with("protected-") {
                self.console.reason = "protected-footprint";
            }
        } else {
            let ground_passing = self.console_ground_passing();
            let verdict = match body {
                Some(rect) if !passing(frame) => {
                    Some(Self::ground_clearance(world, rect, 0.0, ground_passing))
                }
                _ => None,
            };
            let cover = match verdict {
                Some(v) => self.console.hold.update(sense.now, v),
                None => self.console.hold.idle(sense.now),
            };
            if cover < 1.0 {
                frame.alpha = cover_alpha(frame.alpha, cover);
                frame.lane_alpha = cover_alpha(frame.lane_alpha, cover);
            }
            if verdict == Some(Some(false)) {
                if !self.console.reason.starts_with("protected-") {
                    self.console.reason = "protected-footprint";
                }
                self.console.clipped |= frame.alpha == 0;
            }
            match body {
                Some(rect) if frame.alpha > 0 => {
                    let ink = world.ink_clearance(rect, 0.0) != Some(true);
                    let front = self.front_pass();
                    frame.under_ink |= self.console.z.update(sense.now, ink, front);
                }
                _ => self.console.z = ZHold::default(),
            }
        }
        if frame.alpha == 0 {
            frame.motes = [None; PET_MOTES_MAX];
            frame.departures = [None; PET_DEPARTURES_MAX];
            self.console.mote_gate = MoteGate::Shut;
            self.console.on_glass = false;
            return;
        }
        self.console.on_glass = true;
        if has_motes && self.console.mote_gate != MoteGate::Shut {
            let geom = mote_cell_geom((sense.cell_w, sense.cell_h));
            let mut i = 0;
            while i < PET_MOTES_MAX {
                if let Some(m) = &frame.motes[i] {
                    self.console.mote_gate = MoteGate::Clearance;
                    if !mote_clears(world, m, geom) {
                        frame.motes[i] = None;
                    }
                }
                i += 1;
            }
        }
        let base_lane_alpha = frame.lane_alpha;
        let mut refused = 0u16;
        let mut handed = 0.0f32;
        let mut i = 0;
        while i < PET_DEPARTURES_MAX {
            let Some(d) = &mut frame.departures[i] else {
                i += 1;
                continue;
            };
            if matches!(self.departures[i], Some(l) if l.own_glass.is_some()) {
                d.under_ink = true;
                i += 1;
                continue;
            }
            let ghost_rect = if d.alpha != 0 && sense.cell_w != 0 && sense.cell_h != 0 {
                Some(still_body_rect(
                    (d.col, d.row),
                    Stance {
                        scale_x: d.scale_x,
                        scale_y: d.scale_y,
                        lift: d.lift,
                        from: d.stretch_from,
                    },
                    (sense.cell_w, sense.cell_h),
                    (sense.cols, sense.rows),
                ))
            } else {
                None
            };
            let keep = match ghost_rect {
                None => false,
                Some(rect) if matches!(selection, Some(s) if rect.overlaps(s)) => false,
                Some(rect) if matches!(self.departures[i], Some(l) if l.resight) => {
                    d.under_ink = world.ink_clearance(rect, 0.0) != Some(true);
                    match world.seen_under_text_clearance_past_caret(rect) {
                        Some(clear) => clear,
                        None => matches!(self.departures[i], Some(l) if l.own_clear),
                    }
                }
                Some(_) => true,
            };
            if !keep {
                if let Some(l) = self.departures[i]
                    && l.resight
                {
                    self.departures[i] = None;
                    refused += u16::from(d.alpha) * u16::from(base_lane_alpha) / 255;
                    handed += l.envelope((self.lane_clock - l.born) as f32);
                }
                frame.departures[i] = None;
            }
            i += 1;
        }
        if refused > 0 {
            frame.alpha = (u16::from(frame.alpha) + refused).min(u16::from(frame.lane_alpha)) as u8;
        }
        if handed > 0.0 && self.arrive_t > 0.0 && self.arrive_in > 0.0 {
            self.arrive_from =
                (self.arrive_from + handed * self.arrive_in / self.arrive_t).min(1.0);
        }
    }

    #[inline]
    pub(super) const fn console_hold_settled(&self) -> bool {
        self.console.hold.settled()
    }

    #[inline]
    pub(super) const fn console_frames(&self) -> Option<bool> {
        if !self.console.presentable || self.console_legacy_motion_pending() {
            return None;
        }
        if !self.console.hold.settled() {
            return Some(true);
        }
        if self.arrive_t > 0.0 {
            return Some(true);
        }
        let mut i = 0;
        while i < self.departures.len() {
            if self.departures[i].is_some() {
                return Some(true);
            }
            i += 1;
        }
        if !self.console.resident {
            return None;
        }
        Some(
            self.console.resident_handoff
                || self.console.completion.is_some()
                || self.console.seq != self.console.consumed
                || self.pending_pet > 0
                || self.pending_bell.is_some()
                || self.pending_bat.is_some()
                || self.pending_look.is_some()
                || (!self.console.clipped
                    && (self.console.moving
                        || self.alpha < 1.0
                        || self.pointer_heat > 0.0
                        || self.twitch_t > 0.0)),
        )
    }

    #[inline]
    fn offer_console_deadline(
        earliest: &mut Option<Instant>,
        floor: Instant,
        cand: Option<Instant>,
    ) {
        if let Some(at) = cand
            && at > floor
        {
            *earliest = Some(match *earliest {
                Some(prev) if prev <= at => prev,
                _ => at,
            });
        }
    }

    #[inline]
    pub(super) fn console_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.console.presentable {
            return None;
        }
        let floor = match self.last_now {
            Some(t) => t,
            None => now,
        };
        let mut earliest: Option<Instant> = None;
        if let Some(e) = self.console.input {
            Self::offer_console_deadline(&mut earliest, floor, Some(e.at + INPUT_HOLD_DUR));
        }
        if let Some(at) = self.console.contact_at {
            let recover = at + CONTACT_RECOVER_DUR;
            Self::offer_console_deadline(
                &mut earliest,
                floor,
                Some(if recover > floor {
                    recover
                } else {
                    at + CONTACT_HOLD_DUR
                }),
            );
        }
        if let Some((at, _)) = self.console.result_at {
            Self::offer_console_deadline(&mut earliest, floor, Some(at + RESULT_HOLD_DUR));
        }
        if let Some(at) = self.console.progress_at {
            Self::offer_console_deadline(&mut earliest, floor, Some(at + PROGRESS_HOLD_DUR));
        }
        if let Some(at) = self.console.perched_since {
            Self::offer_console_deadline(&mut earliest, floor, Some(at + PERCH_DWELL_DUR));
        }
        Self::offer_console_deadline(&mut earliest, floor, self.console.z.deadline());
        Self::offer_console_deadline(&mut earliest, floor, self.console.repair_until);
        Self::offer_console_deadline(&mut earliest, floor, self.console.failure_quiet_until);
        Self::offer_console_deadline(&mut earliest, floor, self.console.pause_hold_until.get());
        let at = earliest?;
        Some(at.max(now + self.arm_floor()))
    }

    /// The soonest after now a deadline may fall: the host's [`ARM_MIN`], or no floor under a
    /// test's exact arms.
    #[inline]
    pub(super) const fn arm_floor(&self) -> Duration {
        #[cfg(test)]
        if self.console.exact_arms {
            return Duration::ZERO;
        }
        ARM_MIN
    }

    /// Whether mote slot `i` is one the last frame's console pass dropped while the brain kept it.
    #[inline]
    pub(super) const fn console_hid_mote(&self, i: usize) -> bool {
        #[cfg(test)]
        if self.console.offer_hidden_motes {
            return false;
        }
        self.console.hidden_motes[i]
    }

    /// The offer's edge for the motes the console hid, in seconds after the last tick, when it
    /// comes before `horizon` (the brain's other edges).
    ///
    /// A hidden mote's own steps (a whole pixel, a look slice) draw nothing, so the offer does not
    /// wake for them. It walks them instead, every hidden mote's in time order, as the schedule
    /// that woke for each would have ticked, and offers the first of those ticks that would have
    /// changed anything ([`Self::console_motes_change_at`]): a hidden mote clear of the ink again,
    /// which the console then draws; a drawn mote the ink now covers; or a mote the brain drops
    /// (its life spent, or a z or a pop over ink), which frees its slot for the next.
    ///
    /// A tick this skips would have drawn the frame before it, and kept the brain's motes, for two
    /// reasons. The console judges a mote's clearance at the pixel it draws at ([`mote_clears`]),
    /// in a look its slices hold, so the verdict turns only at the mote's own edges, which the walk
    /// visits; a pop has no slices and swells every frame, but a swelling box only meets more ink,
    /// so a hidden pop clears it again only at a whole-pixel step of its centre. And the rest of
    /// the brain changes what it draws only at the edges [`Self::next_change_secs`] offers, which
    /// bound the walk (`horizon`), or on the frames it asks for ([`Self::needs_frames`]), when the
    /// host takes no offer: a beat counts as its offer does, a still pose's hold offers its end, a
    /// glide lands on its last step, and a rhythm run asks for frames. A skipped tick still changes
    /// how the brain's `f32` clocks sum, so an edge can fall a little apart from where a host that
    /// ticked at every step meets it; `a_dozing_pet_draws_what_a_dense_host_draws` holds a host at
    /// these offers to one that also ticks every 2⁻⁸ s.
    #[inline]
    pub(super) const fn hidden_mote_offer(&self, horizon: Option<f32>) -> Option<f32> {
        let limit = match horizon {
            Some(h) => h,
            None => f32::INFINITY,
        };
        let mut next = [None; PET_MOTES_MAX];
        let mut any_hidden = false;
        let cell = self.mote_cell_px();
        let mut i = 0;
        while i < PET_MOTES_MAX {
            if self.console_hid_mote(i)
                && let Some(m) = self.motes[i]
                && m.kind.offers_hidden()
            {
                let edge = self.mote_own_edge(&m, 0.0, cell);
                if edge.is_some() {
                    any_hidden = true;
                }
                next[i] = edge;
            }
            i += 1;
        }
        if !any_hidden {
            return None;
        }
        let mut walked = 0;
        loop {
            let mut soonest: Option<(usize, f32)> = None;
            let mut slot_idx = 0;
            while slot_idx < PET_MOTES_MAX {
                if let Some(t) = next[slot_idx]
                    && !matches!(soonest, Some((_, best)) if t >= best)
                {
                    soonest = Some((slot_idx, t));
                }
                slot_idx += 1;
            }
            let Some((slot, t)) = soonest else {
                return None;
            };
            if t >= limit {
                return None;
            }
            walked += 1;
            if walked >= HIDDEN_MOTE_EDGES_MAX || self.console_motes_change_at(t) {
                return Some(t);
            }
            next[slot] = match self.motes[slot] {
                Some(m) => self.mote_own_edge(&m, t, cell),
                None => None,
            };
        }
    }

    /// Whether a tick `t` seconds after the last would find the console's motes changed where no
    /// edge of their own says so: a hidden mote the brain drops, a hidden mote clear of the ink
    /// again, or a drawn mote the ink now covers. Judged at the pixel each mote draws at
    /// ([`mote_clears`]), clearance turns only at a mote's own edges, so a walk of those edges
    /// meets every turn.
    #[inline]
    const fn console_motes_change_at(&self, t: f32) -> bool {
        let clock = self.clock + (t as f64);
        let world = match &self.console.world {
            Some(w) => Some(&**w),
            None => None,
        };
        let cells = (self.last_cell_w, self.last_cell_h);
        let check_clearance = matches!(self.console.mote_gate, MoteGate::Clearance);
        let geom = if check_clearance && world.is_some() {
            mote_cell_geom(cells)
        } else {
            (0.0, 1.0, 1.0, 1.0, 1.0)
        };
        let mut i = 0;
        while i < PET_MOTES_MAX {
            if let Some(m) = self.motes[i]
                && m.kind.offers_hidden()
            {
                let Some(sprite) = self.mote_sprite(&m, m.age(clock)) else {
                    return true;
                };
                if check_clearance
                    && matches!(world, Some(w) if mote_clears(w, &sprite, geom))
                        == self.console_hid_mote(i)
                {
                    return true;
                }
            }
            i += 1;
        }
        false
    }

    #[inline]
    pub(super) fn note_console_completion(&mut self, now: Instant, failed: bool) {
        self.rearm_idle_ears();
        self.console.completion_seq = self.console.completion_seq.wrapping_add(1).max(1);
        self.console.completion = Some((now, failed));
        self.console.failure_quiet_until = if failed {
            Some(now + RESULT_HOLD_DUR)
        } else {
            None
        };
    }

    #[inline]
    pub(super) fn console_grieving(&self) -> bool {
        matches!(
            (self.console.failure_quiet_until, self.last_now),
            (Some(until), Some(now)) if now < until
        )
    }
}

#[inline]
const fn still_body_rect(
    seat: (f32, f32),
    stance: Stance,
    cell: (u16, u16),
    grid: (u16, u16),
) -> PetRect {
    let cw = if cell.0 > 1 { cell.0 } else { 1 };
    let ch = if cell.1 > 1 { cell.1 } else { 1 };
    let (x0, x1, y0, y1) = drawn_body_px(seat, stance, (cw, ch), grid);
    let (inv_cw, inv_ch) = (1.0 / (cw as f32), 1.0 / (ch as f32));
    PetRect::new(
        y0 as f32 * inv_ch,
        x0 as f32 * inv_cw,
        (y1 - y0) as f32 * inv_ch,
        (x1 - x0) as f32 * inv_cw,
    )
}

#[inline]
const fn body_rect(frame: &PetFrame, sense: PetSense) -> Option<PetRect> {
    let Some((x0, x1, y0, y1)) = frame.body_px(sense.cell_w, sense.cell_h, sense.cols, sense.rows)
    else {
        return None;
    };
    let cw = (if sense.cell_w > 1 { sense.cell_w } else { 1 }) as f32;
    let ch = (if sense.cell_h > 1 { sense.cell_h } else { 1 }) as f32;
    let (inv_cw, inv_ch) = (1.0 / cw, 1.0 / ch);
    Some(PetRect::new(
        y0 as f32 * inv_ch,
        x0 as f32 * inv_cw,
        (y1 - y0) as f32 * inv_ch,
        (x1 - x0) as f32 * inv_cw,
    ))
}

#[inline]
const fn typed_run(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
    last: Option<(u16, u16)>,
) -> Option<(usize, usize, usize)> {
    if input.display_offset != 0 {
        return None;
    }
    let Some(row) = input.cursor_row.checked_sub(pane.row) else {
        return None;
    };
    let Some(col) = input.cursor_col.checked_sub(pane.col) else {
        return None;
    };
    if !input.cursor_visible {
        let Some((r, c)) = last else {
            return None;
        };
        let r = r as usize;
        let c = c as usize;
        if r == row && c < col && col - c <= TYPED_RUN_MAX {
            return Some((row, c, col + 1));
        }
        return None;
    }
    let first = match last {
        Some((r, c)) if (r as usize) == row && (c as usize) < col => c as usize,
        _ => col.saturating_sub(1),
    };
    let floor = col.saturating_sub(TYPED_RUN_MAX);
    Some((row, if first > floor { first } else { floor }, col))
}

#[inline]
const fn local_ink(
    world: &PetWorld,
    body: PetRect,
    typed: Option<(usize, usize, usize)>,
) -> Option<[u64; 8]> {
    if world.stamp().is_none() {
        return None;
    }
    let r0 = (body.row - CONTACT_MARGIN).floor().max(0.0) as usize;
    let c0 = (body.col - CONTACT_MARGIN).floor().max(0.0) as usize;
    let r1 = (body.row + body.rows + CONTACT_MARGIN).ceil() as usize;
    let c1 = (body.col + body.cols + CONTACT_MARGIN).ceil() as usize;
    if r1.saturating_sub(r0) > 16 || c1.saturating_sub(c0) > 32 {
        return None;
    }
    let mut bits = [0; 8];
    let mut r = r0;
    while r < r1 {
        let dr = r - r0;
        let Some(raw_bits) = world.ink_row_bits(r, c0, c1) else {
            return None;
        };
        if raw_bits != 0 {
            let mut row_bits = raw_bits as u64;
            if let Some((row, first, end)) = typed
                && row == r
            {
                let t0 = if first > c0 { first } else { c0 };
                let t1 = if end < c1 { end } else { c1 };
                if t1 > t0 {
                    row_bits &= !(((1u64 << (t1 - t0)) - 1) << (t0 - c0));
                }
            }
            bits[dr >> 1] |= row_bits << ((dr & 1) << 5);
        }
        r += 1;
    }
    Some(bits)
}

#[inline]
const fn bg_eq(a: [u8; 3], b: [u8; 3]) -> bool {
    a[0] == b[0] && a[1] == b[1] && a[2] == b[2]
}

#[cfg(test)]
#[inline]
const fn detect_faux_composer_caret(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
) -> Option<(u16, u16)> {
    detect_faux_composer_caret_with_prior(input, pane, None)
}

#[inline]
const fn detect_faux_composer_caret_with_prior(
    input: &aterm_core::render::RenderInput,
    pane: PetPane,
    prior_faux: Option<(u16, u16)>,
) -> Option<(u16, u16)> {
    let start_r = pane.rows / 2;
    let max_end_r = pane.rows.saturating_sub(1);
    let sub_r = input.cursor_row.saturating_sub(pane.row);
    let end_r = if sub_r < max_end_r { sub_r } else { max_end_r };
    let cells = input.cells.as_slice();
    let mut local_r = end_r;
    while local_r > start_r {
        local_r -= 1;
        let frame_r = pane.row + local_r;
        if frame_r >= cells.len() {
            continue;
        }
        let row = cells[frame_r].as_slice();
        let mut prompt = None;
        let mut boxed = false;
        let scan_cols = if pane.cols < 6 { pane.cols } else { 6 };
        let mut local_c = 0;
        while local_c < scan_cols {
            let idx = pane.col + local_c;
            if idx >= row.len() {
                break;
            }
            match row[idx].ch {
                '│' | '┃' | '║' | '|' if local_c == 0 => boxed = true,
                ' ' => {}
                '>' | '❯' | '›' => {
                    prompt = Some((local_c, row[idx].bg));
                    break;
                }
                _ => break,
            }
            local_c += 1;
        }
        let Some((prompt_c, base_bg)) = prompt else {
            continue;
        };
        let mut last_text: Option<(usize, usize, usize)> = None;
        let mut end_comp_r = local_r;
        let mut comp_r = local_r;
        while comp_r < end_r {
            let comp_frame_r = pane.row + comp_r;
            if comp_frame_r >= cells.len() {
                break;
            }
            let comp_row = cells[comp_frame_r].as_slice();
            if pane.col >= comp_row.len() {
                break;
            }
            let avail_cols = comp_row.len() - pane.col;
            let first_cell = &comp_row[pane.col];
            if matches!(first_cell.ch, '╰' | '└' | '╭' | '┌' | '─' | '━' | '═' | '+')
            {
                break;
            }
            if comp_r > local_r {
                if boxed {
                    if !matches!(first_cell.ch, '│' | '┃' | '║' | '|') {
                        break;
                    }
                } else {
                    if prompt_c >= avail_cols {
                        break;
                    }
                    let mut indented = true;
                    let mut c = 0;
                    while c <= prompt_c {
                        if comp_row[pane.col + c].ch != ' ' {
                            indented = false;
                            break;
                        }
                        c += 1;
                    }
                    if !indented {
                        break;
                    }
                }
            }
            end_comp_r = comp_r;
            let mut right_limit = pane.cols.saturating_sub(1);
            if boxed {
                let mut c = if pane.cols < avail_cols {
                    pane.cols
                } else {
                    avail_cols
                };
                while c > prompt_c + 1 {
                    c -= 1;
                    let ch = comp_row[pane.col + c].ch;
                    if ch == ' ' {
                        continue;
                    }
                    if matches!(ch, '│' | '┃' | '║' | '|') {
                        right_limit = c;
                    }
                    break;
                }
            }
            let start_c = prompt_c + 1;
            let scan_end = if right_limit < avail_cols {
                right_limit
            } else {
                avail_cols
            };
            let mut row_has_ink = false;
            let mut scan_c = start_c;
            while scan_c < scan_end {
                let idx = pane.col + scan_c;
                let cell = &comp_row[idx];
                if cell.ch == '█'
                    || (!bg_eq(cell.bg, base_bg)
                        && (idx == 0 || !bg_eq(comp_row[idx - 1].bg, cell.bg))
                        && (idx + 1 >= comp_row.len() || !bg_eq(comp_row[idx + 1].bg, cell.bg)))
                {
                    return Some((comp_r as u16, scan_c as u16));
                }
                if cell.ch != ' ' && cell.ch != '\0' {
                    row_has_ink = true;
                    last_text = Some((comp_r, scan_c, right_limit));
                }
                scan_c += 1;
            }
            if !boxed && !row_has_ink {
                break;
            }
            comp_r += 1;
        }
        if boxed {
            if let Some((tr, tc, right_limit)) = last_text {
                let limit_m1 = right_limit.saturating_sub(1);
                let next_c = tc + 1;
                let col = if next_c < limit_m1 { next_c } else { limit_m1 };
                return Some((tr as u16, col as u16));
            }
            let limit_m1 = pane.cols.saturating_sub(1);
            let next_c = prompt_c + 2;
            let col = if next_c < limit_m1 { next_c } else { limit_m1 };
            return Some((local_r as u16, col as u16));
        }
        if let Some((pf_r, pf_c)) = prior_faux {
            let r = pf_r as usize;
            let c = pf_c as usize;
            let pane_limit = pane.cols.saturating_sub(1);
            if r >= local_r && r <= end_comp_r && c > prompt_c && c <= pane_limit {
                if let Some((tr, tc, right_limit)) = last_text {
                    let limit_m1 = right_limit.saturating_sub(1);
                    let next_c = tc + 1;
                    let max_c = if next_c < limit_m1 { next_c } else { limit_m1 };
                    if r < tr || (r == tr && c <= max_c) {
                        return Some((pf_r, pf_c));
                    }
                } else {
                    let next_c = prompt_c + 2;
                    let max_c = if next_c < pane_limit {
                        next_c
                    } else {
                        pane_limit
                    };
                    if r == local_r && c <= max_c {
                        return Some((pf_r, pf_c));
                    }
                }
            }
        }
    }
    None
}

#[cfg(any(test, debug_assertions))]
mod state_words;

#[cfg(test)]
#[path = "console_life_tests.rs"]
mod tests;
