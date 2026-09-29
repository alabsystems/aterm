// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! THE MESSAGE BAND on [`AtermGpuTerminal`] — the unified message engine
//! (`aterm_messages`, docs/DESIGN-unified-messages-2026-09-21.md) drawn by
//! the GPU module, through the SAME composition the native window and the
//! CPU twin run (`aterm_render::band`, ruling 331 of that design): a
//! script's `notice` lines post rows, `messages` reads the log, and the rows
//! RESERVE space above the grid (the frame — and the swapchain — grows by
//! [`band_height_px`]; nothing overlays a terminal cell). This file is the
//! CPU module's (`aterm-wasm`) line for line (`web_binding_parity` holds the
//! two together, ruling 340); only the three host hooks each crate's
//! `lib.rs` defines differ — the metrics, the bleed (here handed to the CPU
//! face and the live GPU renderer) and what a band change means (here also
//! the swapchain's size).
//!
//! # The host seams
//!
//! 1. [`notice`](AtermGpuTerminal::notice) — the `notice` verb's grammar
//!    verbatim (`post`, `progress`, `done`, `dismiss`, `act`), every verdict,
//!    cap, budget and reply word the engine's (`aterm_messages::wire`).
//!    A success or info `post` is a RECORD: it goes to the log, never to the
//!    glass (the owner's rule: FYI goes to the log).
//! 2. [`messages_read`](AtermGpuTerminal::messages_read) — the `messages` verb:
//!    the log's rows, every free field percent-encoded exactly as the control
//!    socket encodes them; [`messages_entry`](AtermGpuTerminal::messages_entry)
//!    is one record's row, what a page's Details view is built from.
//! 3. [`messages_next_wake_ms`](AtermGpuTerminal::messages_next_wake_ms) and
//!    [`messages_tick`](AtermGpuTerminal::messages_tick) — the band's ONE clock:
//!    the page arms one timer for the ms this returns (`-1`: nothing armed —
//!    arm NO timer, so an idle page costs 0 % CPU), then ticks and renders.
//! 4. [`set_band_motion`](AtermGpuTerminal::set_band_motion) — the page's
//!    `prefers-reduced-motion`: `false` draws the still look;
//!    [`set_band_visible`](AtermGpuTerminal::set_band_visible) — the page's
//!    visibility: a hidden page's band is off screen, and a hidden page arms
//!    no timer at all (ruling 337, F2).
//! 5. [`band_rows`](AtermGpuTerminal::band_rows),
//!    [`band_height_px`](AtermGpuTerminal::band_height_px),
//!    [`grid_top_px`](AtermGpuTerminal::grid_top_px) — where the grid starts
//!    now: the page offsets every row it hands the terminal
//!    (`selection_start`, `selection_extend`, `selection_word`,
//!    `selection_line`, `encode_mouse_*`, `link_at`) by the band and
//!    swallows presses on band rows. The pet's doors (`note_pointer_px`,
//!    `pet_press_px`) take the canvas pixels unchanged, band included: the
//!    engine folds the band into the pet's origin itself (ruling 347).
//! 6. [`band_hover`](AtermGpuTerminal::band_hover) and
//!    [`band_press`](AtermGpuTerminal::band_press) — the pointer over the band,
//!    in frame pixels: the chip under it lights, and a press does what the
//!    `notice act` verb does; [`set_details_view`](AtermGpuTerminal::set_details_view)
//!    — the page has a Details view, so every row ends in its link.
//! 7. [`set_forced_colors`](AtermGpuTerminal::set_forced_colors) — CSS
//!    `forced-colors`: the five system colours own the band, ungraded.
//! 8. [`messages_log_since`](AtermGpuTerminal::messages_log_since) and
//!    [`messages_quit`](AtermGpuTerminal::messages_quit) — the log as `m1`
//!    codec lines for the page to keep (IndexedDB), and the quit record a
//!    closing page writes; [`messages_restore`](AtermGpuTerminal::messages_restore)
//!    — the kept lines back on the next visit, before the first notice: the
//!    native launch's load of `messages.log` (ruling 345).
//! 9. [`messages_page`](AtermGpuTerminal::messages_page) — the Settings ▸ Messages
//!    page the native window shows, as the engine builds it
//!    (`aterm_messages::page`, rulings 387 and 396), its words said: the
//!    chips with their counts and labels, the count line, the day headers,
//!    each entry's words, its meta line, its sentence and technical lines,
//!    its Copy text, its capsules under their intents' own labels and their
//!    footer buttons' words (`Open Manual`), and the page's chrome (the
//!    heading, the subtitle, `All · N`, `Problems · N`, `All tags`, the tag
//!    pop-up's `Tag: All`, `Copy All`, `Technical details`, `Copy`, `Nothing
//!    yet.`) — under the page's tag chip and Problems switch, in the
//!    engine's page grammar. What a site words or decides itself (the
//!    engine's `wire_lines` doc lists it): Copy All's text, composed from
//!    the lines (its build information, then for each entry in order two
//!    line feeds and its `copy=`), and Copy All pressable only while
//!    `total=` is above 0; the confirmations of the copies it performs (the
//!    macOS page's `Copying N messages…`, `Copied`); its layout's words (the
//!    macOS page names its groups `Severity`, `Tags` and `Message actions`,
//!    and a cut card says `… (N more lines)`) and whether the tag filter
//!    takes the chips' form or the pop-up's. The macOS page's `Open Log
//!    Folder` and `Explain heavy load` switch have no web meaning and are
//!    not in it.
//!
//! Each export samples the clock ONCE at the boundary (the `predict_api`
//! posture) and calls a `pub(crate)` `*_at` twin the tests drive at injected
//! instants; the engine itself never reads a clock.
//!
//! # The driver is the engine's
//!
//! The band's sequencing — settle, commit the rows, drain, the wire's paced
//! paint, the layout, the motion frame, the paint key, the deadline fold —
//! is `aterm_messages::drive` (ruling 336), the SAME driver the native
//! window runs. This module passes it what only the page knows: one grid of
//! `rows` to spare rows from, no handoff freeze, the page's visibility and
//! `set_band_motion`, its forced colours, its links (withheld until the page
//! says it has a Details view) and no `$HOME`, the page theme's inks.
//!
//! # Not on the web (by design, ruling 332)
//!
//! No native reporters (update, toolchain, crash, harness/upgrade, config,
//! person-pass, presence row, strain/load rail, session waits); no Settings ▸
//! Messages window — the engine's page is exported ([`messages_page`]) for a
//! site to render, which no site does yet (ruling 388), and a site that
//! builds a Details view from [`messages_entry`] says so with
//! `set_details_view`; no `messages.log`
//! file (the page keeps the lines [`messages_log_since`] hands it); no
//! seamless-handoff carry. The web performs no authored intent: a press on a
//! capsule is logged and answered `performed=0`; only `Details` is performed,
//! and only by a page with a Details view.
//!
//! [`band_height_px`]: AtermGpuTerminal::band_height_px
//! [`messages_entry`]: AtermGpuTerminal::messages_entry
//! [`messages_page`]: AtermGpuTerminal::messages_page
//! [`messages_log_since`]: AtermGpuTerminal::messages_log_since

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

use std::collections::{BTreeSet, VecDeque};

use aterm_core::render::{HostRowPixels, RenderInput};
use aterm_core::terminal::RenderCell;
use aterm_messages::drive::{self, Commit, Lay, LookIn, View};
use aterm_messages::ink::{AnsiHues, BandInks, BarBase, ForcedPalette, RowRaster, ThemeInks};
use aterm_messages::page::{self, MessagesFilter, MessagesState};
use aterm_messages::paint::{Geometry, Hover, HoverTarget};
use aterm_messages::wire::{self, NoticeRequest, Paint, ReadQuery, WireGate};
use aterm_messages::{
    ActionIndex, Hit, Intent, Links, LogLine, Look, MessageCenter, MessageId, MessageLog, Shelf,
    UpgradeWord, WallStamp, LOG_CAP, MAX_ROWS,
};
use aterm_render::band::{self as band_frame, BandFrame};
use aterm_render::ChromeBleed;
use aterm_time::Instant;

use crate::AtermGpuTerminal;

/// How many drained prepend buffers the band keeps for the next frame (the
/// band's most rows).
const POOL_CAP: usize = MAX_ROWS as usize;

/// How many drained log lines the page can still take
/// ([`AtermGpuTerminal::messages_log_since`]): the ring's record count. A page
/// that falls further behind is told how many it missed (a `dropped` line).
const JOURNAL_CAP: usize = LOG_CAP;

/// How many of the page's kept lines a restore reads — the newest: twice the
/// ring, room for a full ring of records each posted and retired. The older
/// lines are counted, never decoded (the native load reads a bounded tail).
const RESTORE_CAP: usize = 2 * LOG_CAP;

/// Percent-encode a string so it occupies ONE space-free token in a reply
/// line: every byte that is not ASCII-graphic (and `%` itself) becomes `%XX`.
/// A byte copy of `aterm_control::wire::pct_encode` — the control socket's
/// encoder, which this module cannot link (`aterm-control` takes the engine's
/// disk tier, whose zstd C library cannot target wasm32) — held to it over
/// every byte and a UTF-8 corpus by `pct_encode_is_the_control_wires`.
pub(crate) fn pct_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[usize::from(b >> 4)] as char);
            out.push(HEX[usize::from(b & 0x0f)] as char);
        }
    }
    out
}

/// A packed `0x00RRGGBB` colour as sRGB bytes.
fn rgb(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// A page's number (a message id or a log cursor, which JS holds as a
/// `number`) as a whole count: negative, fractional-below-one or not a
/// number is 0.
fn whole(n: f64) -> u64 {
    if n.is_finite() && n >= 1.0 {
        n as u64
    } else {
        0
    }
}

/// The band's host metrics a lib hands this module: one cell's width and
/// height, the frame's pad, and the grid's first pixel row before the band
/// (the chrome's top pad and head) — all device px.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BandMetrics {
    pub(crate) cell_w: usize,
    pub(crate) cell_h: usize,
    pub(crate) pad: usize,
    pub(crate) grid_top: usize,
}

/// The band's state on one terminal: the engine's center and wire gate, the
/// inks the page's theme derives, and the painted rows the next frame
/// composes. `pub(crate)` for the lib's render hooks and the tests (the
/// effects-field posture).
pub(crate) struct WebBand {
    /// The engine: rows, holds, the log ring, the motion grid.
    pub(crate) center: MessageCenter,
    /// The wire's budgets and its one paced paint.
    pub(crate) gate: WireGate,
    /// Motion allowed (the page's reduced-motion).
    motion_ok: bool,
    /// The page is visible: a hidden page's band is off screen — the still
    /// look — and it arms no timer (ruling 337, F2).
    visible: bool,
    /// The page has a Details view: every row ends in its link, and a press
    /// on a row opens it ([`Links::Painted`]).
    details: bool,
    /// The chip the pointer lights, `None` off the band.
    hover: Option<Hover>,
    /// CSS `forced-colors`' five system colours while the page is forced.
    forced: Option<ForcedPalette>,
    /// The page theme's background, foreground and cursor.
    theme: ThemeInks,
    /// The configured palette's blue and cyan (slots 4 and 6), which a
    /// near-grey cursor's meter borrows (ruling 250) — `band_palette`'s rule
    /// on the native host: the CONFIGURED palette, never the live OSC 4 one.
    ansi: AnsiHues,
    /// The inks the rows are painted with: the forced palette's
    /// (`BandInks::forced`) while forced, else `BandInks::derive(theme, ansi,
    /// Blend)` — re-derived on a theme, palette or forced-colours change.
    pub(crate) inks: BandInks,
    /// The band's one view (`aterm_messages::drive::View`): the width law's
    /// layout, the motion frame the painted rows were drawn at, and the key
    /// they were painted for.
    view: View,
    /// The painted rows, their gutter edges and their pixel rasters.
    rows: Vec<Vec<RenderCell>>,
    edges: Vec<Option<([u8; 3], [u8; 3])>>,
    rasters: Vec<Option<RowRaster>>,
    /// The instant of the last tick, notice or motion change: a frame
    /// re-prepares at it, so a resize or a theme change paints the frame the
    /// clock last asked for.
    now: Instant,
    /// Reclaimed prepend buffers ([`RenderInput::undo_host_row_prepend`]).
    pub(crate) pool: Vec<Vec<RenderCell>>,
    /// The drained log lines (`m1` codec), oldest first, at most
    /// [`JOURNAL_CAP`].
    journal: VecDeque<String>,
    /// The cursor of `journal[0]`: how many lines left the journal before it.
    journal_first: u64,
    /// The page's kept log was loaded ([`AtermGpuTerminal::messages_restore`]):
    /// a second restore would load it twice.
    restored: bool,
}

impl WebBand {
    /// An empty band at `now` over the theme's `fg`/`bg`/`cursor` (packed).
    pub(crate) fn new(now: Instant, fg: u32, bg: u32, cursor: u32) -> Self {
        let palette = aterm_types::ColorPalette::new();
        let hue = |i: u8| {
            let c = palette.get(i);
            [c.r, c.g, c.b]
        };
        let theme = ThemeInks {
            bg: rgb(bg),
            fg: rgb(fg),
            cursor: rgb(cursor),
        };
        let ansi = AnsiHues {
            blue: hue(4),
            cyan: hue(6),
        };
        Self {
            center: MessageCenter::new(MessageLog::default(), now),
            gate: WireGate::default(),
            motion_ok: true,
            visible: true,
            details: false,
            hover: None,
            forced: None,
            theme,
            ansi,
            inks: BandInks::derive(theme, Some(ansi), BarBase::Blend),
            view: View::default(),
            rows: Vec::new(),
            edges: Vec::new(),
            rasters: Vec::new(),
            now,
            pool: Vec::new(),
            journal: VecDeque::new(),
            journal_first: 0,
            restored: false,
        }
    }

    /// The look the band draws in: moving only while the page is visible and
    /// allows motion, graded unless forced colours own it. Never frozen.
    fn look(&self) -> Look {
        drive::look(LookIn {
            motion_allowed: self.motion_ok,
            on_screen: self.visible,
            frozen: false,
            forced: self.forced.is_some(),
        })
    }

    /// How a `cols`-wide page lays the band out: links withheld until the page
    /// has a Details view, and no `$HOME` to abbreviate (ruling 332).
    fn lay(&self, cols: usize) -> Lay {
        Lay {
            cols,
            links: if self.details {
                Links::Painted
            } else {
                Links::Withheld
            },
            home: || None,
        }
    }

    /// The inks the rows are painted with (the field's doc).
    fn derive_inks(&self) -> BandInks {
        match self.forced {
            Some(hc) => BandInks::forced(hc),
            None => BandInks::derive(self.theme, Some(self.ansi), BarBase::Blend),
        }
    }

    /// A new page theme (packed `0x00RRGGBB`).
    pub(crate) fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32) {
        self.theme = ThemeInks {
            bg: rgb(bg),
            fg: rgb(fg),
            cursor: rgb(cursor),
        };
        self.inks = self.derive_inks();
    }

    /// A configured palette slot changed: slots 4 and 6 are the meter's
    /// borrowed hues.
    pub(crate) fn set_palette_color(&mut self, index: u8, c: [u8; 3]) {
        match index {
            4 => self.ansi.blue = c,
            6 => self.ansi.cyan = c,
            _ => return,
        }
        self.inks = self.derive_inks();
    }

    /// The committed row count — the rows the frame reserves.
    pub(crate) fn committed(&self) -> usize {
        usize::from(self.center.committed_rows())
    }

    /// The geometry a `cols`-wide band is painted onto: the frame, its cells
    /// `pad` px in.
    fn geometry(cols: usize, cell_w: usize, pad: usize) -> Geometry {
        Geometry {
            win_w: cols * cell_w + 2 * pad,
            cells_x: pad,
            cell_w: cell_w.max(1),
        }
    }

    /// Prepare the band's rows for a frame at `self.now` on a `cols`-wide
    /// grid of `cell_w`-px cells `pad` px in: the drive's layout (re-laid only
    /// when the center's fingerprint or the width moved), one motion frame,
    /// and — only when what the rows are built from moved (the hover and the
    /// forced palette among it) — the paint. `true` when the painted band
    /// changed.
    pub(crate) fn prepare(&mut self, cols: usize, cell_w: usize, pad: usize) -> bool {
        let geom = Self::geometry(cols, cell_w, pad);
        let look = self.look();
        let lay = self.lay(cols);
        let _ = self.view.prepare(&self.center, &lay, self.now, look);
        let inks = self.inks;
        let rows = self.committed();
        let hover = self.hover.filter(|h| usize::from(h.row) < rows);
        let forced = self.forced.is_some();
        match self
            .view
            .paint(&self.center, cols, hover, geom, forced, &|| inks)
        {
            None => false,
            Some(resolved) => {
                (self.rows, self.edges, self.rasters) = band_frame::rows(resolved);
                true
            }
        }
    }

    /// The instant the band next needs the page back: a hold's expiry, an
    /// echo's end, a queued row's patience, the wire's paced paint, or the
    /// next motion frame (a still look asks only for its time words' ticks).
    /// `None` with nothing armed — an idle band arms no timer — and ALWAYS
    /// on a hidden page: it arms no timer at all (the owner's 0 % CPU rule;
    /// ruling 337, F2), and whatever fell due while it was hidden is settled
    /// by the first tick once it is visible again.
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        if !self.visible {
            return None;
        }
        // The kept layout's next change from the last tick's instant — this
        // module's own rule, kept exact by the drive (ruling 337, F4).
        let motion = (self.committed() > 0)
            .then(|| self.view.deadline_from(&self.center, self.now, self.look()))
            .flatten();
        [
            drive::state_deadline(&self.center, &self.gate, false),
            motion,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Compose the painted rows above the grid of `dst`, the frame `at`:
    /// padded or trimmed to the committed count, sealed, each raster placed —
    /// with every window-space effect stream TRANSLATED down with the grid
    /// (the web pipeline's one chrome origin never included the band). The
    /// rows prepended; `0` leaves `dst` untouched.
    pub(crate) fn compose(&mut self, dst: &mut RenderInput, at: BandFrame) -> usize {
        band_frame::compose_band(
            dst,
            None,
            &self.rows,
            &self.rasters,
            self.committed(),
            &self.inks,
            self.forced.is_some(),
            at,
            &mut self.pool,
            HostRowPixels::Translate,
        )
    }

    /// The chrome bleed for `n` composed rows: the gutters reach the frame's
    /// edges in the band's tone, each metered row's in its own edge cells'.
    pub(crate) fn bleed(&self, n: usize) -> Option<ChromeBleed> {
        (n > 0).then(|| {
            band_frame::band_bleed(
                &self.inks,
                0,
                n,
                band_frame::band_row_edges(0, n, &self.edges),
            )
        })
    }

    /// A fresh engine whose motion grid starts at `t0` — the tests' and the
    /// parity reader's injected clock (the native golden anchors its center
    /// the same way). The view and its painted rows stay: the next prepare
    /// repaints exactly when what it would paint differs, as on any tick (the
    /// view re-lays on the new center's fingerprint, and forgets its layout
    /// the first time it prepares with no row).
    #[cfg(test)]
    pub(crate) fn reset_at(&mut self, t0: Instant) {
        self.center = MessageCenter::new(MessageLog::default(), t0);
        self.gate = WireGate::default();
        self.now = t0;
        self.restored = false;
    }

    /// Whether the page's kept log may still be loaded: nothing restored,
    /// minted, queued or drained on this band yet — the native launch loads
    /// `messages.log` before its first post, and a row posted first would
    /// hold an id the kept lines may use (ruling 345).
    fn restorable(&self) -> bool {
        let log = self.center.log();
        !self.restored
            && log.next_id() == MessageId::FIRST
            && log.is_empty()
            && log.pending_len() == 0
            && self.journal.is_empty()
            && self.journal_first == 0
    }

    /// Hand a drained prepend back to the pool: the inverse of the last
    /// frame's prepend on the kept scratch, so an echo frame under a band
    /// keeps the damage-scoped refill (DMG-1).
    pub(crate) fn undo(&mut self, dst: &mut RenderInput, engine_rows: usize) -> bool {
        dst.undo_host_row_prepend(engine_rows, &mut self.pool, POOL_CAP)
    }

    /// The band row and column under frame pixel `(x, y)` — the grid's
    /// first pixel row is `m.grid_top` before the band, and the band's rows
    /// stack from there; the column is the engine's arithmetic
    /// (`Geometry::cell_at`, ruling 328). `None` off the band.
    fn cell_under(&self, x: f64, y: f64, cols: usize, m: BandMetrics) -> Option<(usize, usize)> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let row = (y as usize).checked_sub(m.grid_top)? / m.cell_h.max(1);
        (row < self.committed()).then(|| {
            (
                row,
                Self::geometry(cols, m.cell_w, m.pad).cell_at(x as usize),
            )
        })
    }

    /// What a press at band `(row, col)` lands on, through the layout the
    /// painter drew (the kept one while the view is prepared at `cols`).
    fn hit(&self, row: usize, col: usize, cols: usize) -> Hit {
        match self.view.layout() {
            Some(p) if self.view.is_prepared(&self.center, cols) => p.hit(row, col),
            _ => self.lay(cols).presentation(&self.center).hit(row, col),
        }
    }

    /// The chip a pointer on `hit` lights: a capsule (the `Details ›`
    /// included); on the row body and the overflow row, with a Details view,
    /// the link a press there follows. Nothing where a press does nothing.
    fn hover_target(&self, hit: Hit) -> Option<HoverTarget> {
        match hit {
            Hit::Capsule(_, k) => Some(HoverTarget::Capsule(k)),
            Hit::Details(_) => Some(HoverTarget::Capsule(ActionIndex::DETAILS)),
            Hit::Body(_) | Hit::Overflow if self.details => Some(HoverTarget::Body),
            Hit::Body(_) | Hit::Overflow | Hit::Nothing => None,
        }
    }

    /// Move the drained lines into the journal, evicting the oldest past
    /// [`JOURNAL_CAP`].
    fn journal_push(&mut self, lines: Vec<(LogLine, Shelf)>) {
        for (line, _) in lines {
            self.journal.push_back(line.encode());
        }
        while self.journal.len() > JOURNAL_CAP {
            self.journal.pop_front();
            self.journal_first += 1;
        }
    }

    /// `OK <next> <n>` then the journal's lines from `cursor` on, a `dropped`
    /// line first when lines the page never took were evicted.
    fn log_since(&self, cursor: u64) -> String {
        let end = self.journal_first + self.journal.len() as u64;
        let from = cursor.min(end);
        let mut lines = Vec::new();
        if from < self.journal_first {
            let count = u32::try_from(self.journal_first - from).unwrap_or(u32::MAX);
            lines.push(LogLine::Dropped { count }.encode());
        }
        let skip = usize::try_from(from.saturating_sub(self.journal_first)).unwrap_or(usize::MAX);
        lines.extend(self.journal.iter().skip(skip).cloned());
        let mut out = format!("OK {end} {}", lines.len());
        for line in lines {
            out.push('\n');
            out.push_str(&line);
        }
        out
    }
}

/// What a restore read from the page's kept lines.
struct Kept {
    /// The decoded lines, ordered by id as the native loader orders its two
    /// files (a stable sort: a record's own lines keep their order).
    lines: Vec<LogLine>,
    /// Lines the codec refused (torn, foreign, over its size cap).
    skipped: usize,
    /// Lines the input repeated byte for byte (a page that kept a line
    /// twice): read once, or the replay would load a retired record twice.
    dup: usize,
    /// Lines older than the newest [`RESTORE_CAP`], never decoded.
    over: usize,
}

/// Read the page's kept lines (`\n`-separated, oldest first): the newest
/// [`RESTORE_CAP`] non-empty ones, each exact repeat once, through the
/// engine's codec (`LogLine::decode`, the native loader's).
fn read_kept(text: &str) -> Kept {
    let mut newest = Vec::new();
    let mut over = 0;
    for line in text.rsplit('\n').filter(|l| !l.is_empty()) {
        if newest.len() < RESTORE_CAP {
            newest.push(line);
        } else {
            over += 1;
        }
    }
    newest.reverse();
    let mut seen = BTreeSet::new();
    let (mut skipped, mut dup) = (0, 0);
    let mut lines = Vec::with_capacity(newest.len());
    for line in newest {
        if !seen.insert(line) {
            dup += 1;
            continue;
        }
        match LogLine::decode(line) {
            Ok(decoded) => lines.push(decoded),
            Err(_) => skipped += 1,
        }
    }
    lines.sort_by_key(LogLine::id);
    Kept {
        lines,
        skipped,
        dup,
        over,
    }
}

/// The page's host for [`AtermGpuTerminal::messages_page`] (the engine's
/// `page::Host`, ruling 387): the wall clock at the export and the reader's
/// offset the page passed — and, by design (ruling 332), none of the facts
/// only a native host has.
struct WebDesk {
    /// The wall clock at the export.
    now_unix_ms: u64,
    /// The page's local offset from UTC, in seconds.
    utc_offset_s: i64,
}

impl page::Host for WebDesk {
    fn now_unix_ms(&self) -> u64 {
        self.now_unix_ms
    }

    /// No log folder: the web writes no `messages.log`; the page keeps the
    /// lines [`AtermGpuTerminal::messages_log_since`] hands it.
    fn log_folder(&self) -> Option<String> {
        None
    }

    /// Saved, as far as the engine can say: the page keeps its log itself
    /// (rulings 342 and 345), and the engine's not-saved note speaks of a
    /// log FILE that could not be opened, which the web never has — so it
    /// is never said here.
    fn saved(&self) -> bool {
        true
    }

    fn utc_offset_s(&self) -> i64 {
        self.utc_offset_s
    }

    /// No updater: nothing is staged, so an `Install now` is never pressable.
    fn staged_build(&self) -> Option<u64> {
        None
    }

    /// NO FILESYSTEM, so an `Open log` is never pressable: a page cannot see
    /// a file (a restored record's path names the disk of the machine that
    /// wrote it, if any), and the web performs no authored intent — a press
    /// on one is logged and answered `performed=0` (ruling 341) — so a
    /// pressable `Open log` would promise a file the press never opens.
    fn is_regular_file(&self, _path: &str) -> bool {
        false
    }

    /// No harness: no agent upgrade runs under a page, so none takes a word.
    fn upgrade_takes(&self, _tab: &str, _to: &str, _word: UpgradeWord) -> bool {
        false
    }

    /// No harness: no tab's upgrade stands here, so a record's upgrade
    /// words are not offered at all (ruling 315).
    fn upgrade_stands(&self, _tab: &str) -> bool {
        false
    }

    /// NO NAVIGATION IS PERFORMED HERE (ruling 389): the web performs no
    /// authored intent (ruling 341), and every capsule a page lists is a
    /// record's — a wire row carries none (ruling 172) and a restored line
    /// comes back to the log only — which neither press route reaches:
    /// `notice act` asks [`wire::press_target`] for a LIVE row and answers
    /// `ERR notice: no live message <id>`, and a band press takes only live
    /// rows. A pressable `Manual` would promise a press that cannot land.
    fn performs_navigation(&self) -> bool {
        false
    }

    /// One: ruling 314's one-line sentence, which the macOS host's
    /// reporters (`message_reporters::sentence_lines`, host-side by ruling
    /// 384) give every record but a tab's agent upgrade rows — and those are
    /// the native harness reporter's, which no page has.
    fn sentence_lines(&self, _key: Option<&str>, _detail: &[String]) -> usize {
        1
    }
}

/// A page's `Date.prototype.getTimezoneOffset()` — minutes, positive WEST of
/// UTC — as the engine's offset: seconds EAST of UTC. Junk (not a number, or
/// a day or more either way) reads as UTC.
fn utc_offset_s(timezone_offset_min: f64) -> i64 {
    if timezone_offset_min.is_finite() && timezone_offset_min.abs() < 1440.0 {
        -((timezone_offset_min * 60.0).round() as i64)
    } else {
        0
    }
}

/// The wall clock's reading as the engine's stamp.
fn wall_now() -> WallStamp {
    let unix_ms = aterm_time::SystemTime::now()
        .duration_since(aterm_time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    WallStamp { unix_ms }
}

impl AtermGpuTerminal {
    /// [`Self::notice`] at `now`, stamped `wall`.
    pub(crate) fn notice_at(&mut self, line: &str, now: Instant, wall: WallStamp) -> String {
        let req = match NoticeRequest::parse(line) {
            Ok(req) => req,
            Err(refusal) => return format!("ERR {refusal}"),
        };
        if let NoticeRequest::Act { id, press } = &req {
            // The engine names the capsule and spends the press budget, then
            // the press is the page's own ([`Self::band_act_at`]).
            return match wire::press_target(
                &self.band.center,
                &mut self.band.gate,
                *id,
                press,
                &pct_encode,
                now,
            ) {
                Ok((index, label)) => match self.band_act_at(*id, index, now) {
                    Some((_, performed)) => wire::acted_reply(label, performed, &pct_encode),
                    None => wire::no_live_reply(*id),
                },
                Err(line) => line,
            };
        }
        let applied = wire::apply(&mut self.band.center, &mut self.band.gate, req, wall, now);
        if applied.paint == Paint::Now {
            let _ = self.messages_tick_at(now);
        }
        applied.reply
    }

    /// A press on capsule `index` of row `id` — native's `notice act` after
    /// its target is named: the engine's `act` logs the press (`Acted`) and
    /// hands back the intent, and the band settles. The web performs one
    /// intent, `Details`, and only with a Details view; without one a
    /// `Details` press changes nothing (nothing was shown, so nothing is
    /// marked seen). Returns the capsule's full label and whether the page
    /// performs it; `None` when the row is gone.
    pub(crate) fn band_act_at(
        &mut self,
        id: MessageId,
        index: ActionIndex,
        now: Instant,
    ) -> Option<(&'static str, bool)> {
        if index.is_details() && !self.band.details {
            return self
                .band
                .center
                .live(id)
                .map(|_| (Intent::Details.label(), false));
        }
        let intent = self.band.center.act(id, index, now)?;
        let _ = self.messages_tick_at(now);
        Some((intent.label(), intent == Intent::Details))
    }

    /// [`Self::messages_read`] at wall time `wall_ms`.
    pub(crate) fn messages_read_at(&self, args: &str, wall_ms: u64) -> String {
        match wire::parse_read_args(args) {
            Err(usage) => format!("ERR {usage}"),
            Ok(q) => {
                let rows = wire::message_rows(&self.band.center, &q, wall_ms, &pct_encode);
                if rows.is_empty() {
                    "OK 0".to_string()
                } else {
                    format!("OK {}\n{}", rows.len(), rows.join("\n"))
                }
            }
        }
    }

    /// [`Self::messages_entry`] at wall time `wall_ms`.
    pub(crate) fn messages_entry_at(&self, id: f64, wall_ms: u64) -> String {
        let raw = whole(id);
        let Some(id) = MessageId::from_raw(raw) else {
            return "OK 0".to_string();
        };
        if self.band.center.log().get(id).is_none() {
            return "OK 0".to_string();
        }
        // The one record above its predecessor's id; the first id has none,
        // so its query pages the whole ring from the oldest.
        let q = match MessageId::from_raw(raw - 1) {
            Some(before) => ReadQuery {
                n: Some(1),
                since: Some(before),
                ..ReadQuery::default()
            },
            None => ReadQuery {
                n: Some(LOG_CAP),
                ..ReadQuery::default()
            },
        };
        let head = format!("message {raw} ");
        wire::message_rows(&self.band.center, &q, wall_ms, &pct_encode)
            .into_iter()
            .find(|row| row.starts_with(&head))
            .map_or_else(|| "OK 0".to_string(), |row| format!("OK 1\n{row}"))
    }

    /// [`Self::messages_page`] at wall time `wall_ms`, on a clock `offset_s`
    /// seconds east of UTC: the engine's projection of this band's log
    /// ([`MessagesState::of`], the native page's builder) through the page's
    /// host, as the engine's lines (`page::wire_lines`). Nothing is worded
    /// here.
    pub(crate) fn messages_page_at(
        &self,
        tag: &str,
        problems: bool,
        offset_s: i64,
        wall_ms: u64,
    ) -> String {
        let desk = WebDesk {
            now_unix_ms: wall_ms,
            utc_offset_s: offset_s,
        };
        let filter = MessagesFilter {
            tag: (!tag.is_empty()).then(|| tag.to_string()),
            warn_only: problems,
        };
        let lines = page::wire_lines(&MessagesState::of(&self.band.center, &desk), &filter);
        format!("OK {}\n{}", lines.len(), lines.join("\n"))
    }

    /// [`Self::messages_tick`] at `now`.
    pub(crate) fn messages_tick_at(&mut self, now: Instant) -> u32 {
        let before = self.band.center.committed_rows();
        let band = &mut self.band;
        band.now = now;
        // One grid: the band never takes the terminal's last row (the
        // drive's afford, measured against what the grid affords now — its
        // rows plus the band's committed ones). No handoff freezes a page.
        let afford = drive::afford(
            std::iter::once(u16::try_from(self.rows).unwrap_or(u16::MAX)),
            before,
        );
        let committed = drive::settle_and_commit(
            &mut band.center,
            Commit {
                now,
                frozen: false,
                afford,
                reserved: before,
            },
        );
        // No log file on the web: the drained lines wait in the journal for
        // the page ([`Self::messages_log_since`]). The paced paint needs no
        // bit of its own: the paint key sees it.
        let (lines, _) = drive::drain_and_pace(&mut band.center, &mut band.gate, now, true);
        band.journal_push(lines);
        let rows_moved = committed.regrid.is_some();
        let m = self.band_metrics();
        let changed = self.band.prepare(self.cols, m.cell_w, m.pad);
        if changed || rows_moved {
            self.note_band_change(rows_moved);
        }
        u32::from(changed || rows_moved) | (u32::from(rows_moved) << 1)
    }

    /// [`Self::messages_next_wake_ms`] at `now`.
    pub(crate) fn messages_next_wake_ms_at(&self, now: Instant) -> f64 {
        match self.band.next_wake() {
            None => -1.0,
            Some(t) => (t.saturating_duration_since(now).as_secs_f64() * 1000.0).ceil(),
        }
    }

    /// Re-prepare the band at `now` after a change of how it is drawn (the
    /// look, the links, the hover, the inks): a band change reopens the frame
    /// gate.
    fn band_redraw_at(&mut self, now: Instant) {
        self.band.now = now;
        let m = self.band_metrics();
        if self.band.prepare(self.cols, m.cell_w, m.pad) {
            self.note_band_change(false);
        }
    }

    /// [`Self::set_band_motion`] at `now`.
    pub(crate) fn set_band_motion_at(&mut self, allowed: bool, now: Instant) {
        if self.band.motion_ok == allowed {
            return;
        }
        self.band.motion_ok = allowed;
        self.band_redraw_at(now);
    }

    /// [`Self::set_band_visible`] at `now`.
    pub(crate) fn set_band_visible_at(&mut self, visible: bool, now: Instant) {
        if self.band.visible == visible {
            return;
        }
        self.band.visible = visible;
        self.band_redraw_at(now);
    }

    /// [`Self::set_details_view`] at `now`.
    pub(crate) fn set_details_view_at(&mut self, available: bool, now: Instant) {
        if self.band.details == available {
            return;
        }
        self.band.details = available;
        self.band.hover = None;
        // The view's layout is keyed by the center and the width, not the
        // links: a fresh view re-lays, and its invalidation repaints even an
        // empty band (a painted one must not keep the old links).
        self.band.view = View::default();
        self.band.view.invalidate();
        self.band_redraw_at(now);
    }

    /// [`Self::set_forced_colors`] at `now`.
    #[allow(
        clippy::too_many_arguments,
        reason = "the five CSS system colours, the switch and the injected instant"
    )]
    pub(crate) fn set_forced_colors_at(
        &mut self,
        active: bool,
        canvas: u32,
        canvas_text: u32,
        highlight: u32,
        highlight_text: u32,
        button_face: u32,
        now: Instant,
    ) {
        let forced = active.then(|| ForcedPalette {
            window: rgb(canvas),
            window_text: rgb(canvas_text),
            highlight: rgb(highlight),
            highlight_text: rgb(highlight_text),
            btn_face: rgb(button_face),
        });
        if self.band.forced == forced {
            return;
        }
        self.band.forced = forced;
        self.band.inks = self.band.derive_inks();
        self.band_redraw_at(now);
    }

    /// [`Self::band_hover`] at `now`.
    pub(crate) fn band_hover_at(&mut self, x: f64, y: f64, now: Instant) -> bool {
        let m = self.band_metrics();
        let hover = self
            .band
            .cell_under(x, y, self.cols, m)
            .and_then(|(row, col)| {
                let target = self.band.hover_target(self.band.hit(row, col, self.cols))?;
                Some(Hover {
                    row: u8::try_from(row).ok()?,
                    target,
                })
            });
        if self.band.hover == hover {
            return false;
        }
        self.band.hover = hover;
        self.band_redraw_at(now);
        true
    }

    /// [`Self::band_press`] at `now`.
    pub(crate) fn band_press_at(&mut self, x: f64, y: f64, now: Instant) -> String {
        let m = self.band_metrics();
        let Some((row, col)) = self.band.cell_under(x, y, self.cols, m) else {
            return "OK none".to_string();
        };
        let (id, index) = match self.band.hit(row, col, self.cols) {
            Hit::Capsule(id, k) => (id, k),
            Hit::Details(id) | Hit::Body(id) if self.band.details => (id, ActionIndex::DETAILS),
            Hit::Overflow if self.band.details => return "OK messages".to_string(),
            Hit::Details(_) | Hit::Body(_) | Hit::Overflow | Hit::Nothing => {
                return "OK none".to_string()
            }
        };
        match self.band_act_at(id, index, now) {
            Some((label, performed)) => format!(
                "{} id={id}",
                wire::acted_reply(label, performed, &pct_encode)
            ),
            None => wire::no_live_reply(id),
        }
    }

    /// [`Self::messages_log_since`]: the center's shelved lines join the
    /// journal first, so a record posted since the last tick is in it.
    pub(crate) fn messages_log_since_at(&mut self, cursor: f64) -> String {
        let lines = self.band.center.drain_shelved_for_persist();
        self.band.journal_push(lines);
        self.band.log_since(whole(cursor))
    }

    /// [`Self::messages_quit`] at `now`.
    pub(crate) fn messages_quit_at(&mut self, cursor: f64, now: Instant) -> String {
        let _ = self.band.center.quit(now);
        let _ = self.messages_tick_at(now);
        self.messages_log_since_at(cursor)
    }

    /// Hand the effects pipeline the band's height (the render hook, BEFORE
    /// the effects): the rows [`Self::compose_band_into_frame`] prepends
    /// after them — the same committed count. The pipeline's emissions stay
    /// placed without the band (the compose translates them), but the page
    /// hands its pointer in canvas pixels, band included (ruling 347), so the
    /// pet's hit rect and the pointer it watches are read against the grid
    /// where this frame draws it: a press on the drawn cat pets it, and a
    /// press on the row above it reaches the terminal.
    pub(crate) fn hand_band_to_effects(&mut self) {
        let px = u16::try_from(self.band_height_px()).unwrap_or(u16::MAX);
        self.effects.set_host_rows_px(px);
    }

    /// [`Self::messages_restore`] at `now`: the kept lines replayed as the
    /// native launch replays its file (`MessageLog::replay_all` — every
    /// `Posted` loads retired `Stale` until a `Retired` line says how it
    /// ended, a colliding id is re-minted, and the next id is above them
    /// all), then a fresh center over that log. Nothing is queued: the page
    /// already has these lines, so the journal never hands them back.
    pub(crate) fn messages_restore_at(&mut self, lines: &str, now: Instant) -> String {
        if !self.band.restorable() {
            return "ERR restore: only once, before the first notice".to_string();
        }
        let kept = read_kept(lines);
        let mut log = MessageLog::empty();
        log.replay_all(kept.lines);
        self.band.center = MessageCenter::new(log, now);
        self.band.now = now;
        self.band.restored = true;
        format!(
            "OK records={} skipped={} dup={} over={}",
            self.band.center.log().len(),
            kept.skipped,
            kept.dup,
            kept.over
        )
    }

    /// Compose the band onto the frame scratch (the render hook, after the
    /// effects and the scroll stamp): re-prepare at the last instant — a
    /// resize, a new cell size, pad or theme lands here — prepend the rows
    /// with every window-space stream translated, move the grid band below
    /// them, and hand the renderer the band's bleed. With no committed row it
    /// changes nothing: no prepend, and the bleed stays `None`.
    pub(crate) fn compose_band_into_frame(&mut self) {
        let m = self.band_metrics();
        let _ = self.band.prepare(self.cols, m.cell_w, m.pad);
        let at = BandFrame {
            cols: self.cols,
            cell_w: m.cell_w,
            cell_h: m.cell_h,
            pad: m.pad,
            lo: 0,
            frame_w: self.cols * m.cell_w + 2 * m.pad,
            grid_top: m.grid_top,
        };
        let n = self.band.compose(&mut self.frame_scratch, at);
        self.frame_scratch.grid_top_row += n;
        self.frame_scratch.grid_bot_row += n;
        let bleed = self.band.bleed(n);
        self.set_band_bleed(bleed);
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AtermGpuTerminal {
    /// One `notice` line — the words after `notice`: `post <tag> [sev=…]
    /// [key=…] [hold=1..3600] <title>[ -- <detail>]`, `progress key=<key>
    /// [pct=…|done=n/total|busy] <title>`, `done <key> <ok|warn|withdraw>
    /// [words]`, `dismiss <id>` or `act <id> <index|label|details>`. Returns
    /// the reply line the control socket's `notice` verb answers, without its
    /// newline: `OK …`, or `ERR …` for every refusal (usage, caps — three
    /// live wire rows, 60 mints / 16 KiB / 10 presses a minute, a 120-char
    /// title, a 2 KiB / 24-line / 240-char detail, `hold` 1–3600 s, `wire.`
    /// keys up to 40 chars — and aterm's own lanes). A success or info `post`
    /// is a record: it lands in the log, never on the band. `act` answers
    /// `performed=1` only for `details` on a page with a Details view: the
    /// web performs no other intent. When
    /// [`needs_frame`](Self::needs_frame) turns true, render; then re-arm the
    /// timer from [`messages_next_wake_ms`](Self::messages_next_wake_ms).
    pub fn notice(&mut self, line: &str) -> String {
        self.notice_at(line, Instant::now(), wall_now())
    }

    /// The `messages` verb — the words after `messages`: `[<n>] [since=<id>]
    /// [tag=<tag>] [sev=<sev>] [live]`. Returns `OK <count>` then one
    /// `message <id> …` row per record, `\n`-separated (no trailing newline),
    /// every free field percent-encoded; or `ERR usage: …`. The log is the
    /// in-memory ring for the page's lifetime: this is where a row's details
    /// live on the web.
    pub fn messages_read(&self, args: &str) -> String {
        self.messages_read_at(args, wall_now().unix_ms)
    }

    /// One record's `messages` row — `OK 1` then the row, or `OK 0` when the
    /// ring has no record `id`: what a page's Details view is built from
    /// (every field `messages_read` carries, the full detail among them).
    pub fn messages_entry(&self, id: f64) -> String {
        self.messages_entry_at(id, wall_now().unix_ms)
    }

    /// The Settings ▸ Messages page as the native window builds it, its
    /// words already said (what a site still words itself: the module doc's
    /// item 9): `OK <n>` then `n` lines, `\n`-separated. The `page` line
    /// carries the page's chrome words too (`heading=`, `subtitle=`,
    /// `all_words=` and `problems_words=` — `All · N`, `Problems · N` —,
    /// `all_tags=`, `tag_menu=`, `copy_all_button=`, `copy_button=`,
    /// `empty=` and the rest), a `chip` its painted `label=` (`ALab tools ·
    /// 2`), an `entry` its `meta=` line and its `copy=` — what Copy puts on
    /// the clipboard — and an `action` both the intent's own `label=`
    /// (`Manual`) and the `button=` the native footer paints (`Open Manual`).
    /// Copy All's text is the page's build information, then for each entry
    /// in order two line feeds and its `copy=`; Copy All is pressable while
    /// `total=` is above 0. `tag` selects a chip (`""`: every tag; a chip's
    /// `tag=`, or any tag that shows under it), `problems` keeps the
    /// warnings and errors, and `timezone_offset_min` is
    /// `new Date().getTimezoneOffset()` verbatim, for the local times and the
    /// day headers. Each line is a kind word (`page`, `chip`, `day`, `entry`,
    /// `action`) then TAB-separated `key=value` fields, each value unescaped
    /// `\\` → `\`, `\t` → TAB, `\n` → LF, `\r` → CR and `\u` → U+001F, which
    /// joins the lines of `sentence=` and `technical=` (`copy=` is one text,
    /// its line breaks `\n`); a reader ignores a key or a kind it does not
    /// know, and a key added later follows every older one of its kind (a
    /// reader of round 36's grammar still parses). The grammar is the engine's
    /// (`aterm_messages::page::wire_lines`), whose doc names every field. A
    /// wire row carries no authored capsule (ruling 172), so the web's
    /// entries have no `action` lines unless a restored log brought some,
    /// and none of those is pressable here (`actionable=0`, ruling 389).
    pub fn messages_page(&self, tag: &str, problems: bool, timezone_offset_min: f64) -> String {
        self.messages_page_at(
            tag,
            problems,
            utc_offset_s(timezone_offset_min),
            wall_now().unix_ms,
        )
    }

    /// Advance the band to now — call when the timer armed from
    /// [`messages_next_wake_ms`](Self::messages_next_wake_ms) fires: retire
    /// what expired, commit the row count, take the wire's paced paint and
    /// prepare the frame. Bit 0: the band changed (render); bit 1: the row
    /// count changed (the frame's height and [`grid_top_px`](Self::grid_top_px)
    /// moved — a page with a fixed canvas re-grids now).
    pub fn messages_tick(&mut self) -> u32 {
        self.messages_tick_at(Instant::now())
    }

    /// Milliseconds until the band next needs [`messages_tick`](Self::messages_tick)
    /// (`0`: now), or `-1` when NOTHING is armed — arm no timer then. A moving
    /// row asks on the engine's 33 ms grid; a held row only at its expiry; a
    /// still look only for its time words' ticks; a hidden page never.
    pub fn messages_next_wake_ms(&self) -> f64 {
        self.messages_next_wake_ms_at(Instant::now())
    }

    /// Whether the band may move: `false` (the page's `prefers-reduced-motion`)
    /// draws the still look, which asks for no frame between its time words'
    /// ticks. Re-arm the timer after a change.
    pub fn set_band_motion(&mut self, allowed: bool) {
        self.set_band_motion_at(allowed, Instant::now());
    }

    /// Whether the page is visible (`!document.hidden`): a hidden page's band
    /// is off screen — the still look, and NO wake at all:
    /// [`messages_next_wake_ms`](Self::messages_next_wake_ms) answers `-1`
    /// until the page is visible again, when it answers what came due
    /// meanwhile (`0`: tick now). Re-arm the timer after a change.
    pub fn set_band_visible(&mut self, visible: bool) {
        self.set_band_visible_at(visible, Instant::now());
    }

    /// Whether the page has a Details view (built from
    /// [`messages_entry`](Self::messages_entry)): with one, every row ends in
    /// its link (`Details ›`, `+N ›`, the overflow row's) and a press on a row
    /// opens it; without, the links are withheld. Render when
    /// [`needs_frame`](Self::needs_frame) turns true.
    pub fn set_details_view(&mut self, available: bool) {
        self.set_details_view_at(available, Instant::now());
    }

    /// CSS `forced-colors`: while `active`, the five system colours (packed
    /// `0x00RRGGBB`: `Canvas`, `CanvasText`, `Highlight`, `HighlightText`,
    /// `ButtonFace`) own the band, ungraded — the High Contrast mapping the
    /// native window uses. `active = false` returns to the theme's inks (the
    /// colours are then ignored). Render when
    /// [`needs_frame`](Self::needs_frame) turns true.
    pub fn set_forced_colors(
        &mut self,
        active: bool,
        canvas: u32,
        canvas_text: u32,
        highlight: u32,
        highlight_text: u32,
        button_face: u32,
    ) {
        self.set_forced_colors_at(
            active,
            canvas,
            canvas_text,
            highlight,
            highlight_text,
            button_face,
            Instant::now(),
        );
    }

    /// The pointer at frame pixel `(x, y)` (device px from the frame's
    /// top-left; anything off the band — `(-1, -1)` on leave — clears it):
    /// lights the chip a press there would follow. `true` when the lit chip
    /// changed — render; moving along one chip changes nothing.
    pub fn band_hover(&mut self, x: f64, y: f64) -> bool {
        self.band_hover_at(x, y, Instant::now())
    }

    /// A press at frame pixel `(x, y)`: `OK none` off the band or where a
    /// press does nothing; `OK messages` on the overflow row (open the
    /// Details view at the top of the log); else what `notice act` answers,
    /// then the row: `OK acted=<label> performed=<0|1> id=<id>` —
    /// `performed=1` is `Details`: open the Details view at record `id`. A
    /// press above [`grid_top_px`](Self::grid_top_px) never reaches the
    /// terminal. Render when [`needs_frame`](Self::needs_frame) turns true.
    pub fn band_press(&mut self, x: f64, y: f64) -> String {
        self.band_press_at(x, y, Instant::now())
    }

    /// The log lines (`m1` codec, one record's post, retirement or press
    /// each) from `cursor` on: `OK <next> <n>` then the lines,
    /// `\n`-separated. Keep them (IndexedDB) and pass `<next>` back next
    /// time; start at 0. A page that fell more than the ring's 512 lines
    /// behind reads a `dropped` line first, saying how many it missed.
    pub fn messages_log_since(&mut self, cursor: f64) -> String {
        self.messages_log_since_at(cursor)
    }

    /// The page is going away (`pagehide`, not persisted): every row still
    /// open is recorded as cut off by the quit (ruling 267), and the lines
    /// from `cursor` on — the quit records among them — come back as
    /// [`messages_log_since`](Self::messages_log_since) answers.
    pub fn messages_quit(&mut self, cursor: f64) -> String {
        self.messages_quit_at(cursor, Instant::now())
    }

    /// Load the log the page kept on earlier visits: the `m1` lines it saved
    /// from [`messages_log_since`](Self::messages_log_since) and
    /// [`messages_quit`](Self::messages_quit), `\n`-separated, oldest first.
    /// Call ONCE, before the first [`notice`](Self::notice). The lines become
    /// the log's records, never rows: a row still open when its page went
    /// away comes back `stale` and stays off the band, a retired one keeps
    /// its outcome, and every later post's id is above theirs. The newest
    /// 1024 lines are read; a line the codec refuses is skipped, a repeated
    /// one read once. Answers `OK records=<n> skipped=<n> dup=<n> over=<n>`
    /// (the ring's records after the load, then the lines refused, repeated
    /// and left unread), or `ERR restore: …` after a restore or a post, when
    /// nothing changes. The restored lines never come back from
    /// `messages_log_since`: the page has them already.
    pub fn messages_restore(&mut self, lines: &str) -> String {
        self.messages_restore_at(lines, Instant::now())
    }

    /// The band's committed rows (0–3): the rows the frame reserves above the
    /// grid.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn band_rows(&self) -> u32 {
        u32::from(self.band.center.committed_rows())
    }

    /// The band's height in device px (`band_rows · cell_height`): the frame
    /// grows by this, and the page offsets every row it hands the terminal
    /// (`selection_start`, `selection_extend`, `selection_word`,
    /// `selection_line`, `encode_mouse_*`, `link_at`) by it. The pet's
    /// doors (`note_pointer_px`, `pet_press_px`) take the canvas pixels
    /// unchanged: the engine folds the band into the pet's origin itself
    /// (ruling 347).
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn band_height_px(&self) -> u32 {
        let m = self.band_metrics();
        u32::try_from(self.band.committed() * m.cell_h).unwrap_or(u32::MAX)
    }

    /// Where the grid's first row starts in the frame, device px (the chrome's
    /// top pad and head, then the band). A press above it is on the band:
    /// swallow it.
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(getter))]
    pub fn grid_top_px(&self) -> u32 {
        u32::try_from(self.band_metrics().grid_top)
            .unwrap_or(u32::MAX)
            .saturating_add(self.band_height_px())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// The bundled JetBrains Mono — the CPU twin's test face.
    const FONT: &[u8] =
        include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");
    const FG: u32 = 0x00d8_d8d8;
    const BG: u32 = 0x0018_1818;
    const CURSOR: u32 = 0x00a0_c0ff;
    const SEL: u32 = 0x0040_4050;

    fn stamp() -> WallStamp {
        WallStamp {
            unix_ms: 1_790_000_000_000,
        }
    }

    fn ms(n: u64) -> aterm_messages::Duration {
        aterm_messages::Duration::from_millis(n)
    }

    fn clocked(rows: u16, cols: u16) -> (AtermGpuTerminal, Instant) {
        let mut t = AtermGpuTerminal::new(rows, cols, FONT, 20.0, FG, BG, CURSOR, SEL)
            .expect("bundled font");
        let t0 = Instant::now();
        t.band.reset_at(t0);
        (t, t0)
    }

    /// The GPU module's band is the CPU twin's on the frame it hands its
    /// renderer: a warning reserves one row above the grid (the frame
    /// scratch grows by it, the grid band moves below it, both faces hold
    /// the band's bleed), the tick reports the row count's move, and when
    /// the row leaves the frame is the band-less one again. With no band row
    /// the frame build changes nothing (the negative control).
    #[test]
    fn the_band_composes_into_the_gpu_frame() {
        let (mut plain, _) = clocked(12, 60);
        let (mut t, t0) = clocked(12, 60);
        for x in [&mut plain, &mut t] {
            x.set_chrome(6, 0);
            x.process(b"$ ls");
        }
        let _ = t.notice_at("post ci Build finished", t0, stamp());
        let _ = t.render_headless();
        let _ = plain.render_headless();
        assert!(
            t.frame_scratch == plain.frame_scratch,
            "a record never reaches the frame"
        );
        assert_eq!(t.cpu.chrome_bleed(), None);
        let reply = t.notice_at(
            "post ci sev=warn hold=5 Disk nearly full",
            t0 + ms(1),
            stamp(),
        );
        assert!(reply.starts_with("OK message="), "{reply}");
        assert_eq!(t.band_rows(), 1);
        assert!(!t.render_headless(), "the band's frame is built");
        assert_eq!(t.frame_scratch.rows, 13, "one band row above twelve");
        assert_eq!(t.frame_scratch.grid_top_row, 1);
        assert_eq!(t.frame_scratch.grid_bot_row, 13);
        let words: String = t.frame_scratch.cells[0].iter().map(|c| c.ch).collect();
        assert!(words.contains("Disk nearly full"), "{words}");
        assert_eq!(
            t.frame_scratch.cells[1][0].ch, '$',
            "the grid, one row down"
        );
        assert!(
            t.cpu.chrome_bleed().is_some(),
            "the gutters in the band's tone"
        );
        assert!(t.render_headless(), "a settled band gates the next frame");
        assert_eq!(t.messages_next_wake_ms_at(t0 + ms(1)), 5000.0);
        // The hold ends, the shrink's quiet passes, the row count follows.
        let _ = t.messages_tick_at(t0 + ms(5001));
        let bits = t.messages_tick_at(t0 + ms(5001) + aterm_messages::SHRINK_QUIET);
        assert_eq!(bits & 2, 2, "the row count moved");
        assert_eq!(t.band_rows(), 0);
        assert!(!t.render_headless());
        let _ = plain.render_headless();
        assert!(
            t.frame_scratch == plain.frame_scratch,
            "the band-less frame again"
        );
        assert_eq!(t.cpu.chrome_bleed(), None);
        assert!(
            t.messages_next_wake_ms_at(t0 + ms(9000)) < 0.0,
            "nothing armed"
        );
    }

    /// The GPU module's copy carries the round's exports: a hidden page arms
    /// nothing, the forced colours are the engine's forced inks, and the
    /// log drains as codec lines — the CPU twin's tests, abridged, on this
    /// module's own frame.
    #[test]
    fn the_gpu_band_has_the_twins_exports() {
        let (mut t, t0) = clocked(12, 60);
        let _ = t.notice_at("post ci sev=warn hold=5 Disk nearly full", t0, stamp());
        assert_eq!(t.messages_next_wake_ms_at(t0), 5000.0);
        t.set_band_visible_at(false, t0);
        assert_eq!(t.messages_next_wake_ms_at(t0), -1.0);
        t.set_band_visible_at(true, t0);
        // Five distinct colours: a swap between any two slots fails.
        let hc = ForcedPalette {
            window: [0x00, 0x00, 0x10],
            window_text: [0xff, 0xff, 0xff],
            highlight: [0x1a, 0xeb, 0xff],
            highlight_text: [0x10, 0x00, 0x00],
            btn_face: [0x00, 0x10, 0x00],
        };
        t.set_forced_colors_at(
            true,
            0x0000_0010,
            0x00ff_ffff,
            0x001a_ebff,
            0x0010_0000,
            0x0000_1000,
            t0,
        );
        assert_eq!(t.band.inks, BandInks::forced(hc));
        let _ = t.render_headless();
        assert!(t.frame_scratch.cells[0].iter().all(|c| c.bg == hc.btn_face));
        let log = t.messages_log_since_at(0.0);
        assert!(log.starts_with("OK 1 1\nm1\tkind=posted"), "{log}");
        t.set_details_view_at(true, t0);
        let _ = t.render_headless();
        let words: String = t.frame_scratch.cells[0].iter().map(|c| c.ch).collect();
        assert!(words.contains("Details"), "{words}");
    }

    /// The pet's drawn body in FRAME px `(x0, x1, y0, y1)`, read off the frame
    /// this module hands its renderer: the free sprite whose atlas tile is
    /// the body's natural size (`ART_ROWS` cells tall at `ART_ASPECT` — motes
    /// and word-cats bake at other sizes), placed where the rasterizers stamp
    /// it, `pad` in and `grid_top` down (any band rows are already in its
    /// `y`).
    fn drawn_pet_body(frame: &RenderInput, m: BandMetrics) -> Option<(f32, f32, f32, f32)> {
        use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS};
        let nat_h = (ART_ROWS * m.cell_h as f32).round();
        let nat = ((nat_h * ART_ASPECT).round() as u16, nat_h as u16);
        let s = frame.free_sprites.iter().find(|s| (s.aw, s.ah) == nat)?;
        let x0 = m.pad as f32 + s.x as f32;
        let y0 = m.grid_top as f32 + s.y as f32;
        Some((x0, x0 + f32::from(s.w), y0, y0 + f32::from(s.h)))
    }

    /// The CPU twin's `the_pet_is_pressed_where_the_band_draws_it`, on this
    /// module's own frame build: with a band row committed, a press at the
    /// drawn cat's centre and one on its feet pet it, and a press on the text
    /// half a row above the drawn body reaches the terminal. Without the band
    /// the same holds (the control).
    #[test]
    fn the_pet_is_pressed_where_the_band_draws_it() {
        let (mut t, t0) = clocked(12, 60);
        t.set_chrome(6, 0);
        // The caret three rows down, so a text row lies above the cat.
        t.process(b"\r\n\r\n\r\n$ ");
        t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        t.set_cursor_pet(true, 7);
        let _ = t.render_headless();
        for frame in 0..240u32 {
            if frame % 6 == 0 {
                let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
                t.note_typed_char(ch);
                t.process(&[ch as u8]);
            }
            t.advance_effects(33.0);
            let _ = t.render_headless();
            if t.cursor_pet_alpha() > 0 {
                break;
            }
        }
        assert!(t.cursor_pet_alpha() > 0, "the pet must reach the glass");
        let m = t.band_metrics();
        let half_row = m.cell_h as f32 / 2.0;
        for band_rows in [0, 1] {
            if band_rows == 1 {
                let reply = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
                assert!(reply.starts_with("OK message="), "{reply}");
                assert!(!t.render_headless(), "the band's frame is built");
            }
            assert_eq!(t.band_rows(), band_rows);
            assert!(t.cursor_pet_alpha() > 0, "the pet is on the glass");
            let (x0, x1, y0, y1) =
                drawn_pet_body(&t.frame_scratch, m).expect("the pet's body is drawn");
            assert!(
                y0 - half_row >= t.grid_top_px() as f32,
                "fixture: the row above the cat is the terminal's, not the band's"
            );
            let cx = (x0 + x1) / 2.0;
            assert_eq!(
                t.pet_press_px(cx, y0 - half_row),
                0,
                "band rows {band_rows}: a press on the text above the drawn cat is the terminal's"
            );
            assert_eq!(
                t.pet_press_px(cx, (y0 + y1) / 2.0),
                1,
                "band rows {band_rows}: a press on the drawn cat's centre pets it"
            );
            assert_eq!(
                t.pet_press_px(cx, y1 - half_row / 2.0),
                1,
                "band rows {band_rows}: a press on the drawn cat's feet pets it"
            );
        }
    }

    /// The GPU module restores the page's kept log as the CPU twin does
    /// (ruling 345): a held warning its last visit left open comes back a
    /// `stale` record — no band row, no swapchain growth, no timer — the next
    /// post takes the next id and IS on the band (the negative control), and
    /// a second restore is refused.
    #[test]
    fn the_gpu_band_restores_the_kept_log() {
        let (mut a, t0) = clocked(12, 60);
        let _ = a.notice_at("post ci sev=warn hold=600 Disk nearly full", t0, stamp());
        let _ = a.messages_tick_at(t0 + ms(10));
        assert_eq!(a.band_rows(), 1);
        let kept: Vec<String> = a
            .messages_log_since_at(0.0)
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect();
        let (mut t, t1) = clocked(12, 60);
        let rows = t.surface_rows();
        assert_eq!(
            t.messages_restore_at(&kept.join("\n"), t1),
            "OK records=1 skipped=0 dup=0 over=0"
        );
        let _ = t.messages_tick_at(t1 + ms(10));
        assert_eq!(t.band_rows(), 0, "a restored row is a record");
        assert_eq!(t.surface_rows(), rows);
        assert_eq!(t.messages_next_wake_ms_at(t1 + ms(10)), -1.0);
        assert!(t.messages_read_at("8", 9).contains(" state=stale "));
        assert!(t
            .messages_restore_at(&kept.join("\n"), t1)
            .starts_with("ERR restore: "));
        let reply = t.notice_at("post ci sev=warn Fresh warning", t1 + ms(20), stamp());
        assert!(reply.starts_with("OK message=2"), "{reply}");
        assert_eq!(t.band_rows(), 1);
    }

    /// The three rows both web modules' page tests post — a record, a held
    /// warning and an error, each detail a sentence and a technical line —
    /// ticked a second later; and the page's lines for them read 90 s after
    /// they were stamped, on a UTC clock (the CPU and the GPU module pin the
    /// SAME lines: one grammar, the engine's).
    fn post_page_rows(t: &mut AtermGpuTerminal, t0: Instant) -> Vec<String> {
        for (i, line) in [
            "post ci Build finished -- 12 tests\\nall green",
            "post ci sev=warn hold=5 Disk nearly full -- 2 GB left",
            "post deploy sev=error Deploy failed -- exit 1\\nsee the job log",
        ]
        .into_iter()
        .enumerate()
        {
            let reply = t.notice_at(line, t0 + ms(i as u64), stamp());
            assert!(reply.starts_with("OK "), "{reply}");
        }
        let _ = t.messages_tick_at(t0 + ms(1000));
        let at = stamp().unix_ms;
        let wall = at + 90_000;
        let rev = t.band.center.revision();
        vec![
            format!(
                "page\trev={rev}\tnow={wall}\ttotal=3\tshown=3\tall=3\tproblems=2\tstatus=3 messages{}",
                page_chrome(3, 2, None)
            ),
            "chip\ttag=deploy\twords=Deploy\tcount=1\ton=0\tlabel=Deploy \u{00b7} 1".to_string(),
            "chip\ttag=ci\twords=CI\tcount=2\ton=0\tlabel=CI \u{00b7} 2".to_string(),
            format!(
                "entry\tid=3\tat={at}\twhen=1 min ago\tlocal=Today 2:13:20 PM\ttag=deploy\ttag_words=Deploy\tsev=error\tsev_words=Error\tmark=\u{2715}\ttitle=Deploy failed\tstate=held\tstate_words=showing now\trep=1\tdescription=Error, Deploy, 1 minute ago\tsentence=exit 1\ttechnical=see the job log\tmeta=Today 2:13:20 PM \u{00b7} showing now\tcopy=Deploy failed\\n2026-09-21 14:13:20 UTC \u{00b7} deploy \u{00b7} error\\nexit 1\\nsee the job log"
            ),
            format!(
                "entry\tid=2\tat={at}\twhen=1 min ago\tlocal=Today 2:13:20 PM\ttag=ci\ttag_words=CI\tsev=warn\tsev_words=Warning\tmark=\u{26a0}\ttitle=Disk nearly full\tstate=held\tstate_words=showing now\trep=1\tdescription=Warning, CI, 1 minute ago\tsentence=2 GB left\ttechnical=\tmeta=Today 2:13:20 PM \u{00b7} showing now\tcopy=Disk nearly full\\n2026-09-21 14:13:20 UTC \u{00b7} ci \u{00b7} warn\\n2 GB left"
            ),
            format!(
                "entry\tid=1\tat={at}\twhen=1 min ago\tlocal=Today 2:13:20 PM\ttag=ci\ttag_words=CI\tsev=info\tsev_words=Info\tmark=\u{2139}\ttitle=Build finished\tstate=recorded\tstate_words=recorded\trep=1\tdescription=Info, CI, 1 minute ago\tsentence=12 tests\ttechnical=all green\tmeta=Today 2:13:20 PM \u{00b7} recorded\tcopy=Build finished\\n2026-09-21 14:13:20 UTC \u{00b7} ci \u{00b7} info\\n12 tests\\nall green"
            ),
        ]
    }

    /// The `page` line's chrome keys (ruling 396), after every older key: a
    /// page whose tag filter admits `all` entries, `problems` of them
    /// warnings or errors, under the chip worded `chip` (`None`: every tag).
    /// The words are the engine's, byte for byte, as the native page paints
    /// them.
    fn page_chrome(all: usize, problems: usize, chip: Option<&str>) -> String {
        let on = u8::from(chip.is_none());
        let current = chip.unwrap_or("All");
        format!(
            "\theading=Messages\tsubtitle=What aterm reported, newest first.\tall_words=All \u{00b7} {all}\tproblems_words=Problems \u{00b7} {problems}\tall_tags=All tags\tall_tags_on={on}\ttag_menu=Tag: {current}\ttag_menu_name=Filter by tag: {current}\tcopy_all_button=Copy All\tcopy_all_name=Copy the shown messages\tempty=Nothing yet.\ttechnical_caption=Technical details\tcopy_button=Copy"
        )
    }

    /// The GPU module's Settings ▸ Messages page is the CPU twin's (ruling
    /// 387): the same rows posted through `notice` read back from
    /// `messages_page` as the SAME lines the CPU module's test pins — the
    /// engine's grammar with the page's chrome words, each entry's meta line
    /// and Copy text (ruling 396), framed `OK <n>` — the Problems switch
    /// leaves the record out, and a chip down is named by the chrome.
    #[test]
    fn the_gpu_page_is_the_twins_page() {
        let (mut t, t0) = clocked(12, 60);
        let want = post_page_rows(&mut t, t0);
        let wall = stamp().unix_ms + 90_000;
        assert_eq!(
            t.messages_page_at("", false, 0, wall),
            format!("OK {}\n{}", want.len(), want.join("\n"))
        );
        let problems = t.messages_page_at("", true, 0, wall);
        assert!(problems.starts_with("OK 5\n"), "{problems}");
        assert!(problems.contains(&format!(
            "\tshown=2\tall=3\tproblems=2\tstatus=2 of 3{}",
            page_chrome(3, 2, None)
        )));
        assert!(problems.contains("\nchip\ttag=ci\twords=CI\tcount=1\ton=0\tlabel=CI \u{00b7} 1\n"));
        assert!(!problems.contains("\ttitle=Build finished\t"), "{problems}");
        // A chip down: the chrome names it, `All tags` is up (ruling 396).
        let deploy = t.messages_page_at("deploy", false, 0, wall);
        assert!(
            deploy.contains(&format!(
                "\tstatus=1 of 3{}\n",
                page_chrome(1, 1, Some("Deploy"))
            )),
            "{deploy}"
        );
    }

    /// The GPU module's page offers what the CPU twin's offers (rulings 387,
    /// 389): a restored native log's crash record reads its `Open log` and
    /// its `Manual` NOT pressable, each with the words its footer button
    /// paints (`button=`, ruling 396: `Open Manual` for `Manual`), and no
    /// press route reaches the record; an agent's upgrade record offers no
    /// word on the page while the log still names it — the SAME action lines
    /// the CPU module's `the_web_page_offers_no_file_and_no_upgrade` pins.
    #[test]
    fn the_gpu_page_offers_what_the_twin_offers() {
        let (mut t, t0) = clocked(12, 60);
        let mut native = MessageCenter::new(MessageLog::default(), t0);
        for message in [
            aterm_messages::Message::new(
                aterm_messages::tags::CRASH,
                aterm_messages::Severity::Error,
                "aterm closed unexpectedly",
            )
            .line("crash log at /logs/crash-1.log")
            .action(Intent::OpenPath {
                path: "/logs/crash-1.log".into(),
            })
            .action(Intent::OpenSettings {
                route: "/manual".into(),
            })
            .hold(aterm_messages::Hold::LogOnly),
            aterm_messages::Message::new(
                aterm_messages::tags::HARNESS,
                aterm_messages::Severity::Warn,
                "Claude upgrade waits in tab 1",
            )
            .action(Intent::AgentUpgrade {
                tab: "s-1".into(),
                to: "2.1.282".into(),
                word: UpgradeWord::Now,
            })
            .hold(aterm_messages::Hold::LogOnly),
        ] {
            let _ = native.post(message, stamp(), t0);
        }
        let kept: Vec<String> = native
            .drain_shelved_for_persist()
            .into_iter()
            .map(|(line, _)| line.encode())
            .collect();
        assert_eq!(
            t.messages_restore_at(&kept.join("\n"), t0),
            "OK records=2 skipped=0 dup=0 over=0"
        );
        let page = t.messages_page_at("", false, 0, stamp().unix_ms + 60_000);
        let actions: Vec<&str> = page.lines().filter(|l| l.starts_with("action\t")).collect();
        assert_eq!(
            actions,
            [
                "action\tid=1\tindex=0\tlabel=Open log\tactionable=0\tbutton=Open log",
                "action\tid=1\tindex=1\tlabel=Manual\tactionable=0\tbutton=Open Manual",
            ],
            "{page}"
        );
        assert_eq!(
            t.notice_at("act 1 1", t0, stamp()),
            wire::no_live_reply(MessageId::from_raw(1).expect("id 1")),
            "a pressable Manual would answer this"
        );
        assert!(page.contains("\nentry\tid=2\t"), "{page}");
        assert!(
            t.messages_entry_at(2.0, stamp().unix_ms)
                .contains(" actions=Upgrade%20now"),
            "the record keeps its word"
        );
    }
}
