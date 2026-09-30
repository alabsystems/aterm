// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! THE MESSAGE BAND on [`AtermTerminal`] — the unified message engine
//! (`aterm_messages`, docs/DESIGN-unified-messages-2026-09-21.md) drawn by
//! the CPU module, through the SAME composition the native window runs
//! (`aterm_render::band`, ruling 331 of that design): a script's `notice`
//! lines post rows, `messages` reads the log, and the rows RESERVE space
//! above the grid (the frame grows by [`band_height_px`]; nothing overlays a
//! terminal cell). The GPU module (`aterm-gpu-web`) carries this file line
//! for line (`web_binding_parity` holds the two together, ruling 340); only
//! the three host hooks each crate's `lib.rs` defines differ — the metrics,
//! the bleed and what a band change means there.
//!
//! # The host seams
//!
//! 1. [`notice`](AtermTerminal::notice) — the `notice` verb's grammar
//!    verbatim (`post`, `progress`, `done`, `dismiss`, `act`), every verdict,
//!    cap, budget and reply word the engine's (`aterm_messages::wire`).
//!    A success or info `post` is a RECORD: it goes to the log, never to the
//!    glass (the owner's rule: FYI goes to the log).
//! 2. [`messages_read`](AtermTerminal::messages_read) — the `messages` verb:
//!    the log's rows, every free field percent-encoded exactly as the control
//!    socket encodes them; [`messages_entry`](AtermTerminal::messages_entry)
//!    is one record's row, what a page's Details view is built from.
//! 3. [`messages_next_wake_ms`](AtermTerminal::messages_next_wake_ms) and
//!    [`messages_tick`](AtermTerminal::messages_tick) — the band's ONE clock:
//!    the page arms one timer for the ms this returns (`-1`: nothing armed —
//!    arm NO timer, so an idle page costs 0 % CPU), then ticks and renders.
//! 4. [`set_band_motion`](AtermTerminal::set_band_motion) — the page's
//!    `prefers-reduced-motion`: `false` draws the still look;
//!    [`set_band_visible`](AtermTerminal::set_band_visible) — the page's
//!    visibility: a hidden page's band is off screen, and a hidden page arms
//!    no timer at all (ruling 337, F2).
//! 5. [`band_rows`](AtermTerminal::band_rows),
//!    [`band_height_px`](AtermTerminal::band_height_px),
//!    [`grid_top_px`](AtermTerminal::grid_top_px) — where the grid starts
//!    now: the page offsets every row it hands the terminal
//!    (`selection_start`, `selection_extend`, `selection_word`,
//!    `selection_line`, `encode_mouse_*`, `link_at`) by the band and
//!    swallows presses on band rows. The pet's doors (`note_pointer_px`,
//!    `pet_press_px`) take the canvas pixels unchanged, band included: the
//!    engine folds the band into the pet's origin itself (ruling 347).
//! 6. [`band_hover`](AtermTerminal::band_hover) and
//!    [`band_press`](AtermTerminal::band_press) — the pointer over the band,
//!    in frame pixels: the chip under it lights, and a press does what the
//!    `notice act` verb does; [`set_details_view`](AtermTerminal::set_details_view)
//!    — the page has a Details view, so every row ends in its link.
//! 7. [`set_forced_colors`](AtermTerminal::set_forced_colors) — CSS
//!    `forced-colors`: the five system colours own the band, ungraded.
//! 8. [`messages_log_since`](AtermTerminal::messages_log_since) and
//!    [`messages_quit`](AtermTerminal::messages_quit) — the log as `m1`
//!    codec lines for the page to keep (IndexedDB), and the quit record a
//!    closing page writes; [`messages_restore`](AtermTerminal::messages_restore)
//!    — the kept lines back on the next visit, before the first notice: the
//!    native launch's load of `messages.log` (ruling 345).
//! 9. [`messages_page`](AtermTerminal::messages_page) — the Settings ▸ Messages
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
//! [`band_height_px`]: AtermTerminal::band_height_px
//! [`messages_entry`]: AtermTerminal::messages_entry
//! [`messages_page`]: AtermTerminal::messages_page
//! [`messages_log_since`]: AtermTerminal::messages_log_since

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

use crate::AtermTerminal;

/// How many drained prepend buffers the band keeps for the next frame (the
/// band's most rows).
const POOL_CAP: usize = MAX_ROWS as usize;

/// How many drained log lines the page can still take
/// ([`AtermTerminal::messages_log_since`]): the ring's record count. A page
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
    /// The page's kept log was loaded ([`AtermTerminal::messages_restore`]):
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

/// The page's host for [`AtermTerminal::messages_page`] (the engine's
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
    /// lines [`AtermTerminal::messages_log_since`] hands it.
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

impl AtermTerminal {
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
impl AtermTerminal {
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
    /// (`Manual`) and the `button=` the native footer paints (`Open Manual`),
    /// then `primary=1` on the one the native footer draws in the accent and
    /// a bare Return presses (the engine's rule,
    /// `aterm_messages::page::primary_index`, ruling 407), `primary=0` on the
    /// rest.
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
    /// and none of those is pressable here (`actionable=0`, ruling 389), so
    /// none leads (`primary=0`).
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

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_core::render::FrameRefill;
    use aterm_messages::Duration;

    /// The bundled JetBrains Mono — the face the web page ships, so every
    /// test here draws with the page's own glyphs and needs no system font.
    pub(super) const FONT: &[u8] =
        include_bytes!("../../aterm-render/assets/bundled/JetBrainsMono-Regular.ttf");
    const FG: u32 = 0x00d8_d8d8;
    const BG: u32 = 0x0018_1818;
    const CURSOR: u32 = 0x00a0_c0ff;
    const SEL: u32 = 0x0040_4050;

    fn term(rows: u16, cols: u16) -> AtermTerminal {
        AtermTerminal::new(rows, cols, FONT, 20.0, FG, BG, CURSOR, SEL).expect("bundled font")
    }

    fn stamp() -> WallStamp {
        WallStamp {
            unix_ms: 1_790_000_000_000,
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A terminal whose band's clock starts at `t0`, and `t0`.
    fn clocked(rows: u16, cols: u16) -> (AtermTerminal, Instant) {
        let mut t = term(rows, cols);
        let t0 = Instant::now();
        t.band.reset_at(t0);
        (t, t0)
    }

    /// Every reply the binding gives is the engine's own reply to the same
    /// line at the same instant — usage, lane and title refusals, the live-row
    /// cap, a record, a row, a restate, a finish, a dismissal, a press — so the
    /// web's `notice` IS the control socket's grammar, not a second copy.
    #[test]
    fn wire_replies_and_caps_are_the_engines() {
        let (mut t, t0) = clocked(20, 80);
        let mut center = MessageCenter::new(MessageLog::default(), t0);
        let mut gate = WireGate::default();
        let lines = [
            "",
            "frob",
            "post",
            "post update Hello",
            "post ci sev=warn hold=0 Held too briefly",
            "post ci Build finished",
            "post ci sev=warn Disk nearly full -- 2 GB left",
            "post ci sev=error Deploy failed -- exit 1",
            "post ci sev=warn key=wire.w3 Third row",
            "post ci sev=warn Fourth row, over the cap",
            "progress p pct=40 Downloading assets",
            "progress p pct=41 Downloading assets",
            "done p ok Downloaded assets",
            "done nope ok",
            "dismiss 3",
            "dismiss 999",
            "act 4 details",
            "act 4 0",
            "act 999 details",
        ];
        for (i, line) in lines.iter().enumerate() {
            let now = t0 + ms(10 * i as u64);
            let want = match NoticeRequest::parse(line) {
                Err(refusal) => format!("ERR {refusal}"),
                Ok(NoticeRequest::Act { id, press }) => {
                    match wire::press_target(&center, &mut gate, id, &press, &pct_encode, now) {
                        Ok((_, label)) => wire::acted_reply(label, false, &pct_encode),
                        Err(line) => line,
                    }
                }
                Ok(req) => {
                    let applied = wire::apply(&mut center, &mut gate, req, stamp(), now);
                    if applied.paint == Paint::Now {
                        let _ = center.settle(now, true);
                        let _ = center.commit_rows(now, 22);
                        let _ = center.drain_shelved_for_persist();
                        let _ = gate.take_due(now);
                    }
                    applied.reply
                }
            };
            assert_eq!(t.notice_at(line, now, stamp()), want, "{line:?}");
        }
        // Non-vacuous: the refusals and the cap really answer, in the
        // engine's words.
        assert!(t
            .notice_at("frob", t0, stamp())
            .starts_with("ERR usage: notice"));
        let (mut t, t0) = clocked(20, 80);
        for (i, title) in ["One", "Two", "Three"].into_iter().enumerate() {
            let reply = t.notice_at(
                &format!("post ci sev=warn {title}"),
                t0 + ms(i as u64),
                stamp(),
            );
            assert!(reply.starts_with("OK message="), "{reply}");
        }
        assert_eq!(
            t.notice_at("post ci sev=warn Four", t0 + ms(5), stamp()),
            "ERR notice: 3 wire messages are live; end one with notice done",
            "three live wire rows at most"
        );
        assert_eq!(
            t.notice_at("act 1 details", t0 + ms(6), stamp()),
            format!("OK acted={} performed=0", pct_encode("Details \u{203a}")),
            "a press names the capsule and says the web performed nothing"
        );
    }

    /// AN IDLE BAND ARMS NO TIMER: nothing posted, and a record — a success or
    /// info `post`, which goes to the log and never to the glass — leave
    /// `messages_next_wake_ms` at -1 and the band empty.
    #[test]
    fn an_idle_band_arms_no_timer() {
        let (mut t, t0) = clocked(20, 80);
        assert!(t.messages_next_wake_ms_at(t0) < 0.0);
        assert_eq!(t.messages_tick_at(t0), 0);
        let reply = t.notice_at("post ci Build finished", t0, stamp());
        assert!(reply.starts_with("OK recorded message="), "{reply}");
        assert_eq!(t.messages_tick_at(t0 + ms(5)), 0);
        assert_eq!(t.band_rows(), 0, "a record never reaches the glass");
        assert!(t.messages_next_wake_ms_at(t0 + ms(5)) < 0.0);
        assert_eq!(
            t.messages_read_at("", 0).lines().count(),
            2,
            "it is in the log"
        );
    }

    /// A held warning arms exactly its hold's end, and nothing between.
    #[test]
    fn a_held_warn_arms_exactly_its_hold() {
        let (mut t, t0) = clocked(20, 80);
        let reply = t.notice_at("post ci sev=warn hold=5 Disk nearly full", t0, stamp());
        assert!(reply.starts_with("OK message="), "{reply}");
        assert_eq!(t.band_rows(), 1);
        assert_eq!(t.messages_next_wake_ms_at(t0), 5000.0);
        assert_eq!(t.messages_next_wake_ms_at(t0 + ms(1234)), 3766.0);
        assert_eq!(t.messages_tick_at(t0 + ms(5000)) & 1, 1, "the row folds");
        // The shrink's quiet, then nothing.
        let _ = t.messages_tick_at(t0 + ms(5000) + aterm_messages::SHRINK_QUIET);
        assert_eq!(t.band_rows(), 0);
        assert!(t.messages_next_wake_ms_at(t0 + ms(7000)) < 0.0);
    }

    /// A busy row's comet asks for the engine's 33 ms grid only while the band
    /// may move; the still look asks for no frame between its words' ticks.
    #[test]
    fn a_comet_wakes_on_the_frame_grid_only_while_moving() {
        let (mut t, t0) = clocked(20, 80);
        let _ = t.notice_at("progress b busy Indexing the tree", t0, stamp());
        let at = t0 + aterm_messages::PROGRESS_GRACE + ms(100);
        let _ = t.messages_tick_at(at);
        assert_eq!(t.band_rows(), 1);
        let moving = t.messages_next_wake_ms_at(at);
        assert!((0.0..=33.0).contains(&moving), "{moving}");
        t.set_band_motion_at(false, at);
        let still = t.messages_next_wake_ms_at(at);
        assert!(still < 0.0 || still > 33.0, "{still}");
    }

    /// WF-1: once the band's frame is rendered, nothing asks for another.
    #[test]
    fn a_settled_band_needs_no_frame() {
        let (mut t, t0) = clocked(20, 80);
        let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        assert!(t.needs_frame());
        t.render();
        assert!(!t.needs_frame(), "the band's frame is on the glass");
        t.render();
        assert!(t.last_render_skipped());
        assert_eq!(
            t.messages_tick_at(t0 + ms(40)),
            0,
            "a tick that moved nothing"
        );
        assert!(!t.needs_frame());
    }

    /// DMG-1 UNDER A BAND: an echo frame refills only its damaged rows, because
    /// the render takes last frame's band rows off the kept scratch first.
    #[test]
    fn an_echo_frame_under_a_band_takes_the_scoped_refill() {
        let (mut t, t0) = clocked(20, 80);
        let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        t.process(b"$ ");
        t.render();
        // The first frames are full refills of their own (a fresh grid is
        // full damage, and a prepend onto a full-damage fill is not blessed),
        // exactly as on the native window; the steady state is what counts.
        t.process(b"x");
        t.render();
        t.process(b"y");
        t.render();
        assert!(
            matches!(t.last_refill, Some(FrameRefill::Scoped { .. })),
            "{:?}",
            t.last_refill
        );
        assert_eq!(t.frame_scratch.rows, 21, "the band row is back on top");
        assert_eq!(t.frame_scratch.cells[1][3].ch, 'y');
        assert_eq!(
            t.frame_scratch.cells[0][0].ch, ' ',
            "the band's glyph margin"
        );
    }

    /// WITH NO BAND ROW NOTHING CHANGES: a terminal that only recorded a
    /// message renders byte for byte what one that never heard of the band
    /// does.
    #[test]
    fn no_band_row_leaves_the_frame_byte_identical() {
        let mut plain = term(12, 60);
        let (mut quiet, t0) = clocked(12, 60);
        let _ = quiet.notice_at("post ci Build finished", t0, stamp());
        let _ = quiet.messages_tick_at(t0 + ms(40));
        for t in [&mut plain, &mut quiet] {
            t.set_chrome(6, 0);
            t.process(b"$ ls\r\nCargo.toml  src\r\n$ ");
            t.render();
        }
        assert_eq!(
            (plain.width(), plain.height()),
            (quiet.width(), quiet.height())
        );
        assert!(plain.rgba() == quiet.rgba(), "the same pixels");
        assert_eq!(quiet.grid_top_px(), 6);
        assert_eq!(quiet.band_height_px(), 0);
    }

    /// The rows RESERVE space: the frame grows by exactly the band's rows, the
    /// grid starts below them, and the terminal's own rows are all still there.
    #[test]
    fn the_frame_grows_by_the_band_rows() {
        let mut plain = term(12, 60);
        let (mut banded, t0) = clocked(12, 60);
        let _ = banded.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        let _ = banded.notice_at("post ci sev=error Deploy failed", t0 + ms(40), stamp());
        for t in [&mut plain, &mut banded] {
            t.set_chrome(6, 0);
            t.process(b"$ ");
            t.render();
        }
        let ch = banded.cell_height();
        assert_eq!(banded.band_rows(), 2);
        assert_eq!(banded.band_height_px() as usize, 2 * ch);
        assert_eq!(banded.width(), plain.width());
        assert_eq!(banded.height(), plain.height() + 2 * ch);
        assert_eq!(banded.grid_top_px() as usize, 6 + 2 * ch);
        assert_eq!(banded.frame_scratch.rows, 14);
        assert_eq!(banded.frame_scratch.grid_top_row, 2);
        assert_eq!(banded.frame_scratch.grid_bot_row, 14);
        // The grid's pixels are the plain frame's, one band lower.
        let w = plain.width() * 4;
        let dy = 2 * ch * w;
        let top = 6 * w;
        assert!(
            plain.rgba()[top..] == banded.rgba()[top + dy..],
            "the grid is the plain grid, moved down by the band"
        );
    }

    /// The pet's drawn body in FRAME px `(x0, x1, y0, y1)`, read off the frame
    /// the renderer drew: the free sprite whose atlas tile is the body's
    /// natural size (`ART_ROWS` cells tall at `ART_ASPECT` — motes and
    /// word-cats bake at other sizes), placed where the rasterizer stamps it,
    /// `pad` in and `grid_top` down (any band rows are already in its `y`).
    fn drawn_pet_body(frame: &RenderInput, m: BandMetrics) -> Option<(f32, f32, f32, f32)> {
        use aterm_effects::kitty_pet::{ART_ASPECT, ART_ROWS};
        let nat_h = (ART_ROWS * m.cell_h as f32).round();
        let nat = ((nat_h * ART_ASPECT).round() as u16, nat_h as u16);
        let s = frame.free_sprites.iter().find(|s| (s.aw, s.ah) == nat)?;
        let x0 = m.pad as f32 + s.x as f32;
        let y0 = m.grid_top as f32 + s.y as f32;
        Some((x0, x0 + f32::from(s.w), y0, y0 + f32::from(s.h)))
    }

    /// Drive the pet onto the glass through the binding alone — the glow
    /// style that names it, the seed door, then witnessed keystrokes at 33 ms
    /// — until `cursor_pet_alpha` goes positive.
    fn wake_the_pet(t: &mut AtermTerminal) -> bool {
        t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        t.set_cursor_pet(true, 7);
        t.render();
        for frame in 0..240u32 {
            if frame % 6 == 0 {
                let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
                t.note_typed_char(ch);
                t.process(&[ch as u8]);
            }
            t.advance_effects(33.0);
            t.render();
            if t.cursor_pet_alpha() > 0 {
                return true;
            }
        }
        false
    }

    /// THE PET UNDER A BAND IS PRESSED WHERE IT IS DRAWN. The band's rows
    /// land above the grid after the effects ran, and the compose moves the
    /// pet's sprite down with the grid; the page presses in canvas pixels,
    /// band included (ruling 347). So with a row committed, a press at the
    /// drawn body's centre or its feet pets the cat, and a press on the text
    /// half a row above the drawn body passes to the terminal — where a hit
    /// rect left a band row too high swallowed that press and let a press on
    /// the cat's feet start a selection. Without the band the same holds (the
    /// control).
    #[test]
    fn the_pet_is_pressed_where_the_band_draws_it() {
        let (mut t, t0) = clocked(12, 60);
        t.set_chrome(6, 0);
        // The caret three rows down, so a text row lies above the cat.
        t.process(b"\r\n\r\n\r\n$ ");
        assert!(wake_the_pet(&mut t), "the pet must reach the glass");
        let m = t.band_metrics();
        let half_row = m.cell_h as f32 / 2.0;
        for band_rows in [0, 1] {
            if band_rows == 1 {
                let reply = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
                assert!(reply.starts_with("OK message="), "{reply}");
                t.render();
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

    /// When the band leaves, the frame is the band-less frame again — the
    /// cached renderer keeps no band pixel, no bleed and no stale gutter.
    #[test]
    fn the_band_leaving_restores_the_bandless_frame() {
        let mut plain = term(12, 60);
        let (mut banded, t0) = clocked(12, 60);
        for t in [&mut plain, &mut banded] {
            t.set_chrome(6, 4);
            t.process(b"$ ls\r\nCargo.toml  src\r\n$ ");
        }
        let _ = banded.notice_at("progress p pct=50 Downloading assets", t0, stamp());
        let _ = banded.messages_tick_at(t0 + ms(2500));
        banded.render();
        assert_eq!(banded.band_rows(), 1);
        let _ = banded.notice_at("done p withdraw", t0 + ms(3000), stamp());
        let mut at = t0 + ms(3000);
        for _ in 0..200 {
            let wake = banded.messages_next_wake_ms_at(at);
            if wake < 0.0 {
                break;
            }
            at += ms(wake as u64).max(ms(1));
            let _ = banded.messages_tick_at(at);
            banded.render();
        }
        assert_eq!(banded.band_rows(), 0, "the row left and the count followed");
        assert!(
            banded.messages_next_wake_ms_at(at) < 0.0,
            "and nothing is armed"
        );
        banded.render();
        plain.render();
        assert_eq!(
            (plain.width(), plain.height()),
            (banded.width(), banded.height())
        );
        assert!(
            plain.rgba() == banded.rgba(),
            "the band-less frame, byte for byte"
        );
    }

    /// The `messages` read is the engine's rows over the ring, paged as the
    /// control socket pages them.
    #[test]
    fn messages_read_pages_the_log() {
        let (mut t, t0) = clocked(20, 80);
        for i in 0..5u64 {
            let _ = t.notice_at(&format!("post ci Build {i} finished"), t0 + ms(i), stamp());
        }
        assert_eq!(
            t.messages_read_at("junk", 0),
            "ERR usage: messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]"
        );
        let page = t.messages_read_at("2", 7);
        let want = wire::message_rows(
            &t.band.center,
            &wire::parse_read_args("2").expect("args"),
            7,
            &pct_encode,
        );
        assert_eq!(page, format!("OK 2\n{}", want.join("\n")));
        assert!(page.contains("Build%204%20finished"), "{page}");
        let since = t.messages_read_at("since=3", 7);
        assert!(since.starts_with("OK 2\n"), "{since}");
        assert_eq!(
            t.messages_read_at("live", 7),
            "OK 0",
            "records are never live"
        );
    }

    /// F2, A HIDDEN PAGE ARMS NOTHING (ruling 337): a live row — a busy one
    /// past its grace, and a held warning — arms `-1` while the page is
    /// hidden, where the same rows on a visible page each arm a wake (a
    /// still look included, the negative control); hidden, the band draws
    /// the still look. Visible again, the page is told what came due while
    /// it slept (`0`: tick now), and the tick settles it.
    #[test]
    fn a_hidden_page_arms_no_timer_and_resumes() {
        let (mut t, t0) = clocked(20, 80);
        let _ = t.notice_at("progress b busy Indexing the tree", t0, stamp());
        let _ = t.notice_at("post ci sev=warn hold=5 Disk nearly full", t0, stamp());
        let at = t0 + aterm_messages::PROGRESS_GRACE + ms(100);
        let _ = t.messages_tick_at(at);
        assert_eq!(t.band_rows(), 2);
        let moving = t.messages_next_wake_ms_at(at);
        assert!((0.0..=33.0).contains(&moving), "{moving}");
        // The negative control: a VISIBLE still page still arms its state.
        t.set_band_motion_at(false, at);
        let still = t.messages_next_wake_ms_at(at);
        assert!(still >= 0.0, "a visible still page keeps its hold: {still}");
        let still_rows = t.band.rows.clone();
        t.set_band_motion_at(true, at);
        t.set_band_visible_at(false, at);
        assert_eq!(t.messages_next_wake_ms_at(at), -1.0, "hidden: no timer");
        assert_eq!(t.band.look().pace, aterm_messages::Pace::Still);
        assert!(t.band.rows == still_rows, "hidden paints the still look");
        // Long past the hold, still hidden: nothing is armed, nothing moved.
        let later = at + ms(9000);
        assert_eq!(t.messages_next_wake_ms_at(later), -1.0);
        assert_eq!(t.band_rows(), 2);
        // Visible again: what came due is due now, and the tick settles it.
        t.set_band_visible_at(true, later);
        assert_eq!(
            t.messages_next_wake_ms_at(later),
            0.0,
            "the hold is overdue"
        );
        assert_eq!(t.messages_tick_at(later) & 1, 1, "the warning folds");
        assert_eq!(t.band.look().pace, aterm_messages::Pace::Moving);
        let resumed = t.messages_next_wake_ms_at(later);
        assert!(resumed >= 0.0, "the comet asks again: {resumed}");
    }

    /// Frame pixel `(x, y)` at the middle of band cell `(row, col)`.
    fn band_px(t: &AtermTerminal, row: usize, col: usize) -> (f64, f64) {
        let m = t.band_metrics();
        (
            (m.pad + col * m.cell_w + m.cell_w / 2) as f64,
            (m.grid_top + row * m.cell_h + m.cell_h / 2) as f64,
        )
    }

    /// The `Details ›` capsule of band row `row`: its first column and width.
    fn details_chip(t: &AtermTerminal, row: usize) -> (usize, usize) {
        let p = t.band.view.layout().expect("a laid-out band");
        let c = p.rows[row]
            .capsules
            .iter()
            .find(|c| c.action.is_details())
            .expect("the row ends in its Details link");
        (c.col, c.width)
    }

    /// THE POINTER ON THE WEB (c). Without a Details view the links are
    /// withheld: the row body lights nothing and a press there does nothing.
    /// Once the page has one, every row ends in `Details ›`; the pointer on
    /// the row body lights that chip and ONLY that chip (every pixel that
    /// moves is inside its cells), moving along the body changes nothing,
    /// leaving restores the resting frame byte for byte; and a press is
    /// native's `notice act <id> details`: the same reply words, the same
    /// record afterwards (the row seen and folded), with the row's id for
    /// the page's Details view.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one walk through the pointer's states"
    )]
    fn the_pointer_lights_only_the_chip_and_a_press_is_notice_act() {
        let (mut t, t0) = clocked(12, 60);
        t.set_chrome(6, 0);
        let _ = t.notice_at(
            "post ci sev=warn Disk nearly full -- 2 GB left",
            t0,
            stamp(),
        );
        t.render();
        let (x, y) = band_px(&t, 0, 20);
        assert!(
            !t.band_hover_at(x, y, t0),
            "withheld links: nothing to light"
        );
        assert_eq!(t.band_press_at(x, y, t0), "OK none");
        assert_eq!(t.band_rows(), 1, "and nothing happened");
        // The page says it has a Details view: the rows gain their links.
        t.set_details_view_at(true, t0);
        assert!(t.needs_frame(), "the links are a new frame");
        t.render();
        let words: String = t.frame_scratch.cells[0].iter().map(|c| c.ch).collect();
        assert!(words.contains("Details"), "{words}");
        let rest = t.rgba();
        let (chip, width) = details_chip(&t, 0);
        // The body lights the chip, and only the chip.
        assert!(t.band_hover_at(x, y, t0 + ms(1)));
        assert!(
            !t.band_hover_at(x + 3.0, y, t0 + ms(2)),
            "same chip: no change"
        );
        t.render();
        let lit = t.rgba();
        let m = t.band_metrics();
        let w = t.width();
        let (y0, y1) = (m.grid_top, m.grid_top + m.cell_h);
        let (x0, x1) = (m.pad + chip * m.cell_w, m.pad + (chip + width) * m.cell_w);
        let mut moved = 0usize;
        for (i, (a, b)) in rest.chunks(4).zip(lit.chunks(4)).enumerate() {
            if a != b {
                let (px, py) = (i % w, i / w);
                assert!(
                    (y0..y1).contains(&py) && (x0..x1).contains(&px),
                    "pixel ({px}, {py}) moved outside the chip {x0}..{x1} x {y0}..{y1}"
                );
                moved += 1;
            }
        }
        assert!(moved > 0, "the chip lit");
        // Leaving restores the resting frame.
        assert!(t.band_hover_at(-1.0, -1.0, t0 + ms(3)));
        t.render();
        assert!(t.rgba() == rest, "the resting frame, byte for byte");
        // The press is native's `notice act 1 details`, on a twin.
        let (mut twin, _) = clocked(12, 60);
        twin.band.reset_at(t0);
        let _ = twin.notice_at(
            "post ci sev=warn Disk nearly full -- 2 GB left",
            t0,
            stamp(),
        );
        twin.set_details_view_at(true, t0);
        let native = twin.notice_at("act 1 details", t0 + ms(4), stamp());
        assert_eq!(
            native,
            format!("OK acted={} performed=1", pct_encode("Details \u{203a}"))
        );
        let pressed = t.band_press_at(x, y, t0 + ms(4));
        assert_eq!(pressed, format!("{native} id=1"));
        assert_eq!(
            t.messages_read_at("", 9),
            twin.messages_read_at("", 9),
            "the same record afterwards"
        );
        assert_eq!(t.band_rows(), 1, "the row folds after the shrink's quiet");
        assert!(
            t.messages_read_at("", 9).contains("state=folded"),
            "{}",
            t.messages_read_at("", 9)
        );
        // Off the band — above it, and on the grid — a press is nobody's.
        assert_eq!(t.band_press_at(x, 0.0, t0 + ms(5)), "OK none");
        assert_eq!(t.band_press_at(x, 400.0, t0 + ms(5)), "OK none");
    }

    /// Without a Details view, `notice act <id> details` performs nothing AND
    /// changes nothing — nothing was shown, so nothing is marked seen — while
    /// with one it marks the row seen, as native's press does.
    #[test]
    fn a_details_press_needs_the_pages_details_view() {
        let (mut t, t0) = clocked(12, 60);
        let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        let reply = t.notice_at("act 1 details", t0 + ms(1), stamp());
        assert!(reply.ends_with("performed=0"), "{reply}");
        assert!(t.messages_read_at("", 9).contains("state=held"));
        t.set_details_view_at(true, t0 + ms(2));
        let reply = t.notice_at("act 1 details", t0 + ms(3), stamp());
        assert!(reply.ends_with("performed=1"), "{reply}");
        assert!(t.messages_read_at("", 9).contains("state=folded"));
    }

    /// The overflow row: with a Details view a press anywhere on it opens the
    /// view at the top of the log; without one it is words alone.
    #[test]
    fn the_overflow_row_opens_the_details_view_only_with_one() {
        // A fixed three-row canvas: the page re-grids on every row the band
        // takes (bit 1), so the third warning finds room for two rows only.
        let (mut t, t0) = clocked(3, 60);
        for (i, title) in ["One", "Two", "Three"].into_iter().enumerate() {
            let _ = t.notice_at(
                &format!("post ci sev=warn {title}"),
                t0 + ms(i as u64),
                stamp(),
            );
            let fit = 3 - t.band_rows() as u16;
            t.resize(fit, 60);
        }
        let _ = t.messages_tick_at(t0 + ms(10));
        assert_eq!(t.band_rows(), 2);
        let last = 1;
        let p = t.band.lay(t.cols).presentation(&t.band.center);
        let overflow = matches!(p.rows[last].kind, aterm_messages::RowKind::Overflow { .. });
        assert!(
            overflow,
            "three rows on a two-row band: the last is the overflow"
        );
        let (x, y) = band_px(&t, last, 3);
        assert_eq!(t.band_press_at(x, y, t0 + ms(11)), "OK none");
        t.set_details_view_at(true, t0 + ms(12));
        assert_eq!(t.band_press_at(x, y, t0 + ms(13)), "OK messages");
    }

    /// D, CSS `forced-colors`: the five system colours become the engine's
    /// forced inks (`BandInks::forced`, the native High Contrast mapping),
    /// the look turns ungraded, the band is re-inked in them (its ground is
    /// `ButtonFace`), a theme change under them moves nothing, and switching
    /// them off returns the theme's frame byte for byte. The same palette
    /// twice is no change (the negative control).
    #[test]
    fn forced_colors_own_the_band_ungraded() {
        let (mut t, t0) = clocked(12, 60);
        t.set_chrome(6, 0);
        let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        t.render();
        let themed = t.rgba();
        let graded = t.band.inks;
        // Five distinct colours, so a swap between any two of the five
        // slots changes the inks or the ground (a palette with three
        // blacks let `canvas`, `highlight_text` and `button_face` trade
        // places unseen).
        let hc = ForcedPalette {
            window: [0x00, 0x00, 0x10],
            window_text: [0xff, 0xff, 0xff],
            highlight: [0x1a, 0xeb, 0xff],
            highlight_text: [0x10, 0x00, 0x00],
            btn_face: [0x00, 0x10, 0x00],
        };
        let pack = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        let set = |t: &mut AtermTerminal, on: bool, at: Instant| {
            t.set_forced_colors_at(
                on,
                pack(hc.window),
                pack(hc.window_text),
                pack(hc.highlight),
                pack(hc.highlight_text),
                pack(hc.btn_face),
                at,
            );
        };
        set(&mut t, true, t0 + ms(1));
        assert_eq!(
            t.band.inks,
            BandInks::forced(hc),
            "the engine's forced derivation"
        );
        assert!(!t.band.look().graded, "ungraded under forced colours");
        assert!(t.needs_frame(), "re-inked");
        t.render();
        assert!(t.frame_scratch.cells[0].iter().all(|c| c.bg == hc.btn_face));
        assert!(t.rgba() != themed);
        let forced_frame = t.rgba();
        set(&mut t, true, t0 + ms(2));
        assert!(!t.needs_frame(), "the same palette: no change");
        t.set_theme(0x0010_2030, 0x00f0_f0f0, 0x0000_00ff, SEL);
        assert_eq!(t.band.inks, BandInks::forced(hc), "the OS owns the colour");
        set(&mut t, false, t0 + ms(3));
        t.set_theme(FG, BG, CURSOR, SEL);
        assert_eq!(t.band.inks, graded, "the theme's inks again");
        assert!(t.band.look().graded);
        t.render();
        assert!(t.rgba() == themed, "the themed frame, byte for byte");
        assert!(forced_frame != themed);
    }

    /// E, THE LOG ACROSS RELOADS: the lines the page keeps are the engine's
    /// `m1` codec lines, each decoding to the record it was drained as —
    /// a record posted since the last tick included — paged by a cursor; a
    /// page that fell behind the journal reads a `dropped` line first.
    #[test]
    fn the_log_drains_as_codec_lines_since_a_cursor() {
        let (mut t, t0) = clocked(20, 80);
        assert_eq!(t.messages_log_since_at(0.0), "OK 0 0");
        let _ = t.notice_at("post ci Build finished", t0, stamp());
        let _ = t.notice_at("post ci sev=warn Disk nearly full", t0 + ms(1), stamp());
        let first = t.messages_log_since_at(0.0);
        let mut lines = first.lines();
        let head: Vec<&str> = lines.next().expect("head").split(' ').collect();
        assert_eq!(head[0], "OK");
        let next: u64 = head[1].parse().expect("next");
        let n: usize = head[2].parse().expect("n");
        // The record's post and its retirement (a record is on the log, never
        // the glass), then the warning's post.
        assert_eq!(n, 3, "{first}");
        assert_eq!(next, 3);
        let ids: Vec<Option<MessageId>> = lines
            .map(|l| LogLine::decode(l).expect("an m1 line").id())
            .collect();
        let (one, two) = (MessageId::from_raw(1), MessageId::from_raw(2));
        assert_eq!(ids, vec![one, one, two]);
        assert_eq!(t.messages_log_since_at(3.0), "OK 3 0", "nothing new");
        assert_eq!(
            t.messages_log_since_at(99.0),
            "OK 3 0",
            "a cursor past the end"
        );
        // Past the cap, the oldest go, and a reader behind is told how many.
        let filler: Vec<(LogLine, Shelf)> = (0..JOURNAL_CAP)
            .map(|_| (LogLine::Dropped { count: 7 }, Shelf::Wire))
            .collect();
        t.band.journal_push(filler);
        let behind = t.messages_log_since_at(0.0);
        let mut lines = behind.lines();
        assert_eq!(
            lines.next(),
            Some(format!("OK {} {}", JOURNAL_CAP + 3, JOURNAL_CAP + 1).as_str())
        );
        assert_eq!(
            LogLine::decode(lines.next().expect("dropped")).expect("m1"),
            LogLine::Dropped { count: 3 },
            "the three lines evicted before the reader took them"
        );
        assert_eq!(
            t.messages_log_since_at(3.0).lines().count(),
            JOURNAL_CAP + 1,
            "a reader in step misses nothing"
        );
    }

    /// E, THE QUIT RECORD: a closing page's rows are recorded as cut off by
    /// the quit (ruling 267) — a determinate row with its percent — and the
    /// lines come back from the page's cursor; the band empties.
    #[test]
    fn a_closing_page_writes_the_quit_record() {
        let (mut t, t0) = clocked(20, 80);
        let _ = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        let _ = t.notice_at("progress p pct=28 Downloading assets", t0, stamp());
        let _ = t.messages_tick_at(t0 + ms(2500));
        let cursor = t
            .messages_log_since_at(0.0)
            .lines()
            .next()
            .expect("head")
            .split(' ')
            .nth(1)
            .expect("next")
            .parse::<f64>()
            .expect("cursor");
        let out = t.messages_quit_at(cursor, t0 + ms(3000));
        let lines: Vec<LogLine> = out
            .lines()
            .skip(1)
            .map(|l| LogLine::decode(l).expect("m1"))
            .collect();
        assert_eq!(lines.len(), 2, "{out}");
        for line in &lines {
            assert!(
                matches!(
                    line,
                    LogLine::Retired {
                        how: aterm_messages::Retired::Quit,
                        ..
                    }
                ),
                "{line:?}"
            );
        }
        assert!(
            out.contains("28%25%20when%20aterm%20quit") || out.contains("28% when aterm quit"),
            "{out}"
        );
        let read = t.messages_read_at("", 9);
        assert_eq!(read.matches("state=quit").count(), 2, "{read}");
        assert_eq!(
            t.messages_quit_at(0.0, t0 + ms(3001)).lines().next(),
            Some("OK 4 4"),
            "idempotent"
        );
    }

    /// The lines a page instance has not handed its store yet, as the page
    /// keeps them: `messages_log_since`'s lines after its head, from `cursor`.
    fn kept_since(t: &mut AtermTerminal, cursor: f64) -> (f64, String) {
        let out = t.messages_log_since_at(cursor);
        let mut lines = out.lines();
        let next = lines
            .next()
            .and_then(|head| head.split(' ').nth(1))
            .and_then(|n| n.parse::<f64>().ok())
            .expect("the head's next cursor");
        (next, lines.collect::<Vec<_>>().join("\n"))
    }

    /// The `state=` word of each row `messages_read` answers, by id.
    fn states(t: &AtermTerminal) -> Vec<(u64, String)> {
        t.messages_read_at("512", 9)
            .lines()
            .skip(1)
            .map(|row| {
                let id = row.split(' ').nth(1).and_then(|n| n.parse().ok());
                let state = row
                    .split(' ')
                    .find_map(|w| w.strip_prefix("state="))
                    .map(str::to_string);
                (id.expect("an id"), state.expect("a state"))
            })
            .collect()
    }

    /// F, THE LOG ACROSS RELOADS (ruling 345): the lines one visit kept come
    /// back on the next as the LOG — a record keeps its outcome, a held
    /// warning and a progress row the page never closed come back `stale`,
    /// never on the band, and arm no timer — and the next post's id is above
    /// every kept one. The new visit's own drain hands back only its own lines.
    #[test]
    fn a_restored_log_is_records_and_never_the_glass() {
        let (mut a, t0) = clocked(20, 80);
        let _ = a.notice_at("post ci Build finished", t0, stamp());
        let _ = a.notice_at("post ci sev=warn hold=600 Disk nearly full", t0, stamp());
        let _ = a.notice_at("progress p pct=28 Downloading assets", t0, stamp());
        let _ = a.messages_tick_at(t0 + ms(2500));
        // The negative control: on the visit that posted them, the warning
        // and the bar ARE on the band, and arm a timer.
        assert_eq!(a.band_rows(), 2);
        assert!(a.messages_next_wake_ms_at(t0 + ms(2500)) >= 0.0);
        // The page is gone without its pagehide (a crash): no quit record.
        let (_, kept) = kept_since(&mut a, 0.0);
        let a_states = states(&a);

        let (mut b, t1) = clocked(20, 80);
        assert_eq!(
            b.messages_restore_at(&kept, t1),
            "OK records=3 skipped=0 dup=0 over=0"
        );
        assert!(b.messages_next_wake_ms_at(t1) < 0.0, "records arm nothing");
        assert_eq!(b.messages_tick_at(t1 + ms(10)) & 1, 0, "nothing to draw");
        assert_eq!(
            b.band_rows(),
            0,
            "a restored row never comes back on the glass"
        );
        assert!(b.messages_next_wake_ms_at(t1 + ms(10)) < 0.0);
        assert_eq!(b.messages_read_at("live", 9), "OK 0");
        let b_states = states(&b);
        assert_eq!(
            b_states.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "the records in order"
        );
        assert_eq!(
            b_states[0], a_states[0],
            "a retired record keeps its outcome"
        );
        assert_eq!(a_states[1].1, "held");
        assert_eq!(b_states[1].1, "stale");
        assert_eq!(b_states[2].1, "stale");
        assert_eq!(
            b.messages_entry_at(2.0, 9).lines().count(),
            2,
            "the Details view reads it"
        );
        // Nothing restored is handed back as new.
        assert_eq!(b.messages_log_since_at(0.0), "OK 0 0");
        // The next post is above every kept id, and it is on the band.
        let reply = b.notice_at("post ci sev=warn Fresh warning", t1 + ms(20), stamp());
        assert!(reply.starts_with("OK message=4"), "{reply}");
        assert_eq!(b.band_rows(), 1);
        let (_, own) = kept_since(&mut b, 0.0);
        let ids: Vec<Option<MessageId>> = own
            .lines()
            .map(|l| LogLine::decode(l).expect("m1").id())
            .collect();
        assert_eq!(ids, vec![MessageId::from_raw(4)], "only this visit's line");
    }

    /// A restore loads the kept log ONCE and before the first notice:
    /// restoring twice, or restoring lines this visit drained itself, is
    /// refused and changes nothing (the engine's replay alone WOULD load a
    /// retired record twice — the negative control); a line the page kept
    /// twice is read once; and three visits sharing one store (drain →
    /// restore → drain → restore) load every record once, under distinct ids.
    #[test]
    fn restoring_twice_or_after_a_post_never_duplicates() {
        let (mut a, t0) = clocked(20, 80);
        let _ = a.notice_at("post ci Build finished", t0, stamp());
        let _ = a.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        let (_, kept_a) = kept_since(&mut a, 0.0);
        // The negative control: the same lines replayed twice into one log
        // load the retired record twice.
        let decoded = || kept_a.lines().map(|l| LogLine::decode(l).expect("m1"));
        let mut twice = MessageLog::empty();
        twice.replay_all(decoded().chain(decoded()));
        assert_eq!(twice.len(), 3, "the replay alone duplicates");

        let (mut b, t1) = clocked(20, 80);
        assert!(b
            .messages_restore_at(&kept_a, t1)
            .starts_with("OK records=2 "));
        let before = b.messages_read_at("512", 9);
        assert_eq!(
            b.messages_restore_at(&kept_a, t1 + ms(1)),
            "ERR restore: only once, before the first notice"
        );
        assert_eq!(
            b.messages_read_at("512", 9),
            before,
            "twice changes nothing"
        );

        // Lines this visit drained itself: refused, nothing doubled.
        let (mut c, t2) = clocked(20, 80);
        let _ = c.notice_at("post ci Deployed site", t2, stamp());
        let (cursor, own) = kept_since(&mut c, 0.0);
        assert!(c
            .messages_restore_at(&own, t2 + ms(1))
            .starts_with("ERR restore: "));
        assert_eq!(c.messages_read_at("512", 9).lines().count(), 2);
        assert_eq!(
            kept_since(&mut c, cursor).1,
            "",
            "the drain returns nothing again"
        );

        // A line kept twice is read once.
        let (mut d, t3) = clocked(20, 80);
        let doubled = format!("{kept_a}\n{kept_a}\n");
        let lines = kept_a.lines().count();
        assert_eq!(
            d.messages_restore_at(&doubled, t3),
            format!("OK records=2 skipped=0 dup={lines} over=0")
        );

        // Three visits, one store: B's new post lands above A's ids, and the
        // third visit loads A's and B's records once each.
        let _ = b.notice_at("post ci Tests passed", t1 + ms(5), stamp());
        let (_, kept_b) = kept_since(&mut b, 0.0);
        assert!(!kept_b.contains("Build%20finished") && !kept_b.contains("Build finished"));
        let store = format!("{kept_a}\n{kept_b}");
        let (mut e, t4) = clocked(20, 80);
        assert!(e
            .messages_restore_at(&store, t4)
            .starts_with("OK records=3 "));
        let ids: Vec<u64> = states(&e).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        let reply = e.notice_at("post ci Shipped", t4 + ms(1), stamp());
        assert!(reply.contains("message=4"), "{reply}");
    }

    /// Two tabs of one site share the store and mint ids from their own
    /// counters: both records load, the second under a fresh id above both —
    /// the native loader's rule for two writers of one file (ruling 270).
    #[test]
    fn two_tabs_sharing_a_store_both_load() {
        let (mut a, t0) = clocked(20, 80);
        let (mut b, _) = clocked(20, 80);
        let _ = a.notice_at("post ci From the first tab", t0, stamp());
        let _ = b.notice_at("post ci From the second tab", t0 + ms(3), stamp());
        let store = format!(
            "{}\n{}",
            kept_since(&mut a, 0.0).1,
            kept_since(&mut b, 0.0).1
        );
        let (mut c, t1) = clocked(20, 80);
        assert!(c
            .messages_restore_at(&store, t1)
            .starts_with("OK records=2 "));
        let read = c.messages_read_at("512", 9);
        assert!(
            read.contains("first%20tab") && read.contains("second%20tab"),
            "{read}"
        );
        assert_eq!(
            states(&c).into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let reply = c.notice_at("post ci Third", t1 + ms(1), stamp());
        assert!(reply.contains("message=3"), "{reply}");
    }

    /// A restore's input is bounded and junk-proof: torn, foreign and
    /// over-long lines are skipped and counted, and only the newest
    /// [`RESTORE_CAP`] lines are read — the older ones counted, their records
    /// not loaded — while the next id is still above every line READ.
    #[test]
    fn a_restore_skips_junk_and_reads_only_the_newest_lines() {
        let (mut a, t0) = clocked(20, 80);
        let (mut cursor, mut store) = (0.0, Vec::new());
        let records = RESTORE_CAP / 2 + 3;
        for i in 0..records {
            // Past the wire's 60 mints a minute: one post every 1.1 s.
            let at = t0 + ms(1100 * i as u64);
            let reply = a.notice_at(&format!("post ci Build {i} finished"), at, stamp());
            assert!(reply.starts_with("OK recorded message="), "{reply}");
            let (next, lines) = kept_since(&mut a, cursor);
            cursor = next;
            store.extend(lines.lines().map(str::to_string));
        }
        assert_eq!(store.len(), 2 * records, "a record's post and retirement");
        let good = store[store.len() - 1].clone();
        let torn = good[..good.len() - 8].to_string();
        let long = format!(
            "m1\tkind=posted\ttitle={}",
            "x".repeat(aterm_messages::log::MAX_LINE_BYTES)
        );
        let junk = ["frob".to_string(), torn, long];
        let (mut b, t1) = clocked(20, 80);
        assert_eq!(
            b.messages_restore_at(&junk.join("\n"), t1),
            "OK records=0 skipped=3 dup=0 over=0"
        );
        let (mut c, t2) = clocked(20, 80);
        let reply = c.messages_restore_at(&store.join("\n"), t2);
        assert_eq!(
            reply,
            format!("OK records={LOG_CAP} skipped=0 dup=0 over=6")
        );
        assert_eq!(c.messages_entry_at(3.0, 9), "OK 0", "an unread record");
        assert!(
            c.messages_entry_at(4.0, 9).starts_with("OK 1\n"),
            "the oldest read"
        );
        let next = c.notice_at("post ci After the restore", t2 + ms(1), stamp());
        assert!(next.contains(&format!("message={}", records + 1)), "{next}");
    }

    /// The Details view's data: one record's `messages` row, the same bytes
    /// `messages_read` answers for it — the first id and a later one — and
    /// `OK 0` for an id the ring does not hold or a page's junk number.
    #[test]
    fn messages_entry_is_the_records_row() {
        let (mut t, t0) = clocked(20, 80);
        for i in 0..4u64 {
            let _ = t.notice_at(
                &format!("post ci Build {i} finished -- line {i}"),
                t0 + ms(i),
                stamp(),
            );
        }
        let all = t.messages_read_at("", 7);
        let rows: Vec<&str> = all.lines().skip(1).collect();
        assert_eq!(t.messages_entry_at(1.0, 7), format!("OK 1\n{}", rows[0]));
        assert_eq!(t.messages_entry_at(3.0, 7), format!("OK 1\n{}", rows[2]));
        for junk in [0.0, -1.0, 0.5, 99.0, f64::NAN, f64::INFINITY] {
            assert_eq!(t.messages_entry_at(junk, 7), "OK 0", "{junk}");
        }
    }

    /// The web's percent-encoder is the control socket's, byte for byte: every
    /// byte value a `str` can carry (each ASCII byte, and every two-byte scalar,
    /// which walks every lead byte `C2`–`DF` over every continuation byte
    /// `80`–`BF`; the three- and four-byte leads by a stride through the rest),
    /// and a UTF-8 corpus.
    #[test]
    fn pct_encode_is_the_control_wires() {
        for b in 0u8..=0x7f {
            let s = char::from(b).to_string();
            assert_eq!(
                pct_encode(&s),
                aterm_control::wire::pct_encode(&s),
                "{b:#x}"
            );
        }
        let two_byte = (0x80u32..0x800).filter_map(char::from_u32);
        let wider = (0x800u32..=0x10_ffff)
            .step_by(61)
            .filter_map(char::from_u32);
        for c in two_byte.chain(wider) {
            let s = c.to_string();
            assert_eq!(pct_encode(&s), aterm_control::wire::pct_encode(&s), "{c:?}");
        }
        for s in [
            "",
            "%",
            "a b",
            "Details ›",
            "日本語 ✓ ⚠ …",
            "\t\n\r\u{7f}",
            "~/src/aterm",
        ] {
            assert_eq!(pct_encode(s), aterm_control::wire::pct_encode(s), "{s:?}");
        }
    }

    /// The band-less frame `a` moved down by `n` band rows, as the banded
    /// frame `b` must be: every grid stream `n` rows lower, every pixel `n·ch`
    /// lower, every window-space quad re-tagged onto the row its moved pixel
    /// lies in (an in-grid quad on its old row + `n`). Returns the streams that
    /// carried anything, so the caller can hold the check to being non-vacuous.
    #[allow(
        clippy::too_many_lines,
        reason = "one assertion per RenderInput stream"
    )]
    fn assert_translated(
        a: &RenderInput,
        b: &RenderInput,
        n: usize,
        grid_top: usize,
        ch: usize,
    ) -> Vec<&'static str> {
        use aterm_core::render::{FirePatch, GlowQuad, RainHalo};
        let dy = u16::try_from(n * ch).expect("small");
        let rows = u16::try_from(n).expect("small");
        let tag =
            |y: u16| u16::try_from((usize::from(y)).saturating_sub(grid_top) / ch).expect("row");
        let in_grid = |y: u16| usize::from(y) >= grid_top;
        let mut lit = Vec::new();
        let mut note = |name: &'static str, any: bool| {
            if any {
                lit.push(name);
            }
        };
        assert_eq!(b.cursor_row, a.cursor_row + n);
        assert_eq!(b.cursor_col, a.cursor_col);
        assert_eq!(b.rows, a.rows + n);
        assert_eq!(b.cursor_trail.len(), a.cursor_trail.len(), "trail");
        for (x, y) in a.cursor_trail.iter().zip(&b.cursor_trail) {
            assert_eq!(
                (y.row, y.col, y.alpha),
                (x.row + n, x.col, x.alpha),
                "trail"
            );
        }
        note("trail", !a.cursor_trail.is_empty());
        let quad = |x: &GlowQuad, y: &GlowQuad, what: &str| {
            let want = GlowQuad {
                y: x.y + dy,
                row: tag(x.y + dy),
                ..*x
            };
            assert_eq!(*y, want, "{what}");
            if in_grid(x.y) {
                assert_eq!(y.row, x.row + rows, "{what}: an in-grid quad keeps its row");
            }
        };
        assert_eq!(a.cursor_glow_add.len(), b.cursor_glow_add.len(), "glow");
        for (x, y) in a.cursor_glow_add.iter().zip(&b.cursor_glow_add) {
            quad(x, y, "glow");
        }
        note("glow", !a.cursor_glow_add.is_empty());
        assert_eq!(a.glow_under.len(), b.glow_under.len(), "under");
        for (x, y) in a.glow_under.iter().zip(&b.glow_under) {
            quad(x, y, "under");
        }
        note("under", !a.glow_under.is_empty());
        assert_eq!(a.fire_patch.len(), b.fire_patch.len(), "fire");
        for (x, y) in a.fire_patch.iter().zip(&b.fire_patch) {
            let want = FirePatch {
                y: x.y + dy,
                base_y: x.base_y + dy,
                row: tag(x.y + dy),
                ..*x
            };
            assert_eq!(*y, want, "fire");
        }
        note("fire", !a.fire_patch.is_empty());
        assert_eq!(a.glow_halo.len(), b.glow_halo.len(), "halo");
        for (x, y) in a.glow_halo.iter().zip(&b.glow_halo) {
            let want = RainHalo {
                y: x.y + dy,
                cy: x.cy + dy,
                row: tag(x.y + dy),
                ..*x
            };
            assert_eq!(*y, want, "halo");
        }
        note("halo", !a.glow_halo.is_empty());
        assert_eq!(a.char_fg.len(), b.char_fg.len(), "char_fg");
        for (x, y) in a.char_fg.iter().zip(&b.char_fg) {
            let mut z = *x;
            z.row = x.row + rows;
            assert_eq!(z, *y, "char_fg");
        }
        note("char_fg", !a.char_fg.is_empty());
        assert_eq!(a.fire_halo.len(), b.fire_halo.len(), "fire_halo");
        for (x, y) in a.fire_halo.iter().zip(&b.fire_halo) {
            let mut z = *x;
            z.row = x.row + rows;
            assert_eq!(z, *y, "fire_halo");
        }
        note("fire_halo", !a.fire_halo.is_empty());
        assert_eq!(
            a.word_decorations.len(),
            b.word_decorations.len(),
            "sparkle"
        );
        for (x, y) in a.word_decorations.iter().zip(&b.word_decorations) {
            let mut z = *x;
            z.row = x.row + rows;
            assert_eq!(z, *y, "sparkle");
        }
        note("sparkle", !a.word_decorations.is_empty());
        assert_eq!(a.ink.len(), b.ink.len(), "ink");
        for (x, y) in a.ink.iter().zip(&b.ink) {
            let mut z = *x;
            z.row = x.row + rows;
            assert_eq!(z, *y, "ink");
        }
        note("ink", !a.ink.is_empty());
        for (what, xs, ys) in [
            ("cat", &a.cat_quads, &b.cat_quads),
            ("rain", &a.rain_quads, &b.rain_quads),
        ] {
            assert_eq!(xs.len(), ys.len(), "{what}");
            for (x, y) in xs.iter().zip(ys) {
                let mut z = *x;
                z.row = x.row + rows;
                z.y = x.y + dy;
                assert_eq!(z, *y, "{what}");
            }
            note(what, !xs.is_empty());
        }
        assert_eq!(a.nova_add.len(), b.nova_add.len(), "nova");
        for (x, y) in a.nova_add.iter().zip(&b.nova_add) {
            let want = GlowQuad {
                row: x.row + rows,
                y: x.y + dy,
                ..*x
            };
            assert_eq!(*y, want, "nova");
        }
        note("nova", !a.nova_add.is_empty());
        assert_eq!(a.rain_add.len(), b.rain_add.len(), "rain_add");
        for (x, y) in a.rain_add.iter().zip(&b.rain_add) {
            let want = RainHalo {
                row: x.row + rows,
                y: x.y + dy,
                cy: x.cy + dy,
                ..*x
            };
            assert_eq!(*y, want, "rain_add");
        }
        note("rain_add", !a.rain_add.is_empty());
        assert_eq!(a.free_sprites.len(), b.free_sprites.len(), "pet");
        for (x, y) in a.free_sprites.iter().zip(&b.free_sprites) {
            let mut z = *x;
            z.y = x.y + i32::from(dy);
            assert_eq!(z, *y, "pet");
        }
        note("pet", !a.free_sprites.is_empty());
        assert_eq!(a.fx_clip, b.fx_clip, "the web sets no effect clip");
        lit
    }

    /// EFFECTS UNDER A BAND ARE THE BAND-LESS FRAME, TRANSLATED (design ruling
    /// 331): the web effects pipeline places its window-space streams from ONE
    /// chrome origin that never counted the band, so the prepend moves them
    /// (`HostRowPixels::Translate`). Two terminals, one with a two-row band,
    /// fed the same bytes, keys and clock through ember fire, the lumen aurora
    /// with the trail, sparkle words and rain, and the pet: every frame's every
    /// stream on the banded one is the band-less one moved down by the band —
    /// and each of glow, trail, fire, halo, sparkle, rain and the pet lit at
    /// least once, so no stream passed by being empty.
    #[test]
    fn effects_under_a_band_are_the_bandless_frame_translated() {
        let mut plain = term(16, 60);
        let (mut banded, t0) = clocked(16, 60);
        let _ = banded.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
        let _ = banded.notice_at("post ci sev=error Deploy failed", t0 + ms(40), stamp());
        assert_eq!(banded.band_rows(), 2);
        let ch = banded.cell_height();
        let mut lit = std::collections::BTreeSet::new();
        let mut frames = 0;
        let step = |plain: &mut AtermTerminal,
                    banded: &mut AtermTerminal,
                    lit: &mut std::collections::BTreeSet<&'static str>| {
            for t in [&mut *plain, &mut *banded] {
                t.advance_effects(33.0);
                t.render();
            }
            let grid_top = banded.renderer.grid_top();
            lit.extend(assert_translated(
                &plain.frame_scratch,
                &banded.frame_scratch,
                2,
                grid_top,
                ch,
            ));
        };
        for t in [&mut plain, &mut banded] {
            t.set_chrome(6, 4);
            t.set_effects_focused(true);
            t.set_sparkle_words_enabled(true);
            t.process(b"a curious cat watched the build fail: fuck\r\n$ ");
        }
        // Ember: fire, its halo, the flame bed under the ink, charred ink.
        for t in [&mut plain, &mut banded] {
            t.set_cursor_glow(true, "ember", None, None, 400, 64, 1.0, 2.0, true);
        }
        for ch_ in "burning".chars() {
            for t in [&mut plain, &mut banded] {
                let _ = t.note_typed_char(ch_);
                t.process(ch_.to_string().as_bytes());
            }
            step(&mut plain, &mut banded, &mut lit);
            frames += 1;
        }
        // The lumen aurora with the trail, and rain.
        for t in [&mut plain, &mut banded] {
            t.set_cursor_glow(true, "lumen", None, None, 260, 24, 0.7, 0.6, true);
            t.set_cursor_trail(true, 260, 24, None);
            // The decorative rain (no output material), which falls into the
            // empty cells while keys keep it calm.
            t.set_matrix_rain(
                30, 6, 4, 6, None, None, "matrix", None, 320, 12, false, true, true, false, 7,
            );
            t.set_matrix_rain_enabled(true);
        }
        for ch_ in " glowing words fuck".chars() {
            for t in [&mut plain, &mut banded] {
                let _ = t.note_typed_char(ch_);
                t.process(ch_.to_string().as_bytes());
            }
            step(&mut plain, &mut banded, &mut lit);
            frames += 1;
        }
        for _ in 0..120 {
            for t in [&mut plain, &mut banded] {
                t.note_keystroke();
            }
            step(&mut plain, &mut banded, &mut lit);
            frames += 1;
            if lit.contains("rain") {
                break;
            }
        }
        // The pet.
        for t in [&mut plain, &mut banded] {
            t.set_cursor_trail(false, 260, 24, None);
            t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
            t.set_cursor_pet(true, 7);
            let _ = t.note_typed_char('p');
            t.process(b"p");
        }
        for _ in 0..240 {
            step(&mut plain, &mut banded, &mut lit);
            frames += 1;
            if lit.contains("pet") && frames > 120 {
                break;
            }
        }
        for want in ["glow", "trail", "fire", "halo", "sparkle", "rain", "pet"] {
            assert!(
                lit.contains(want),
                "{want} never lit in {frames} frames: lit {lit:?}"
            );
        }
    }

    /// One golden scene: its name and its events in time order — a `notice`
    /// line, or a shot (`None`), at ms from the center's birth.
    type Scene = (String, Vec<(u64, Option<String>)>);

    /// FNV-1a-64 — the golden's digest (the native writer's `web_fnv`).
    fn fnv(bytes: &[u8]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }

    /// One golden frame line from this module's composed frame — the native
    /// writer's `web_golden_line`, over the page's own RGBA.
    fn golden_line(key: &str, t: &AtermTerminal) -> String {
        use std::fmt::Write as _;
        let n = t.band.committed();
        let (w, grid_top, cell_h) = (t.width(), t.renderer.grid_top(), t.cell_height());
        let rgba = &t.rgba;
        let px = |y0: usize, y1: usize| fnv(&rgba[y0 * w * 4..y1 * w * 4]);
        let bleed = t.band.bleed(n);
        let mut line = format!(
            "frame\t{key}\tn={n}\tw={w}\ttop={:016x}\tbleed={:016x}",
            px(0, grid_top),
            fnv(format!("{:?}", bleed.as_ref()).as_bytes())
        );
        let input = &t.frame_scratch;
        let mut words = Vec::new();
        for r in 0..n {
            let cells = fnv(format!("{:?}", input.cells[r]).as_bytes());
            let rasters: Vec<_> = input
                .chrome_rasters
                .iter()
                .filter(|m| usize::from(m.row) == r)
                .collect();
            let raster = fnv(format!("{rasters:?}").as_bytes());
            let y0 = grid_top + r * cell_h;
            let _ = write!(
                line,
                "\tr{r}={cells:016x}:{raster:016x}:{:016x}",
                px(y0, y0 + cell_h)
            );
            words.push(
                input.cells[r]
                    .iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string(),
            );
        }
        let _ = write!(line, "\t|{}|", words.join("|"));
        line
    }

    /// THE WEB BAND IS THE NATIVE GOLDEN (design ruling 333): every scene of
    /// `tests/fixtures/band_native.tsv` — which aterm-gui writes from the
    /// native window's band — replayed through THIS module's `notice`, tick
    /// and `render`, lands on every digest: each band row's cells, raster and
    /// pixels, the bleed, and the pixels above the grid, in every theme, width
    /// and look the golden holds.
    #[test]
    #[allow(clippy::too_many_lines, reason = "one replay of the golden's matrix")]
    fn the_web_band_is_the_native_golden() {
        let golden = include_str!("../tests/fixtures/band_native.tsv");
        let hex = |s: &str| u32::from_str_radix(s, 16).expect("hex");
        let mut themes: Vec<(String, [u32; 6])> = Vec::new();
        let mut scenes: Vec<Scene> = Vec::new();
        let mut frames: Vec<&str> = Vec::new();
        let scene = |scenes: &mut Vec<Scene>, name: &str| {
            if scenes.last().is_none_or(|(n, _)| n != name) {
                scenes.push((name.to_string(), Vec::new()));
            }
            scenes.len() - 1
        };
        for line in golden.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f[0] {
                "theme" => themes.push((
                    f[1].to_string(),
                    [
                        hex(f[2]),
                        hex(f[3]),
                        hex(f[4]),
                        hex(f[5]),
                        hex(f[6]),
                        hex(f[7]),
                    ],
                )),
                "line" => {
                    let i = scene(&mut scenes, f[1]);
                    scenes[i]
                        .1
                        .push((f[2].parse().expect("ms"), Some(f[3..].join("\t"))));
                }
                "shot" => {
                    let i = scene(&mut scenes, f[1]);
                    scenes[i].1.push((f[2].parse().expect("ms"), None));
                }
                "frame" => frames.push(line),
                _ => {}
            }
        }
        for (_, events) in &mut scenes {
            events.sort_by_key(|(at, l)| (*at, l.is_none()));
        }
        assert_eq!(themes.len(), 4);
        assert_eq!(scenes.len(), 16);
        let mut got = Vec::new();
        let mut px = 0usize;
        for (tname, [fg, bg, cursor, sel, blue, cyan]) in &themes {
            for cols in [60u16, 80, 120] {
                let mut t = AtermTerminal::new(20, cols, FONT, 20.0, *fg, *bg, *cursor, *sel)
                    .expect("bundled font");
                // On wasm32 the portable rasterizer is the ONLY one; this
                // native test run would otherwise draw with the host's
                // (CoreText on macOS). The golden pins the same path.
                t.renderer.debug_force_fontdue();
                t.set_chrome(6, 0);
                for (i, c) in [(4u8, blue), (6, cyan)] {
                    t.set_palette_color(i, (*c >> 16) as u8, (*c >> 8) as u8, *c as u8);
                }
                for (lname, moving) in [("moving", true), ("still", false)] {
                    for (name, events) in &scenes {
                        let t0 = Instant::now();
                        t.band.reset_at(t0);
                        t.set_band_motion_at(moving, t0);
                        for (at, line) in events {
                            let now = t0 + ms(*at);
                            if let Some(line) = line {
                                let _ = t.notice_at(line, now, stamp());
                                continue;
                            }
                            let _ = t.messages_tick_at(now);
                            t.render();
                            let key = format!("{name}\t{tname}\t{cols}\t{lname}\t{at}");
                            px += t.width() * (t.renderer.grid_top() + t.band_height_px() as usize);
                            got.push(golden_line(&key, &t));
                        }
                    }
                }
            }
        }
        assert_eq!(got.len(), frames.len(), "one frame per golden frame");
        let (mut cells, mut rasters, mut pixels, mut other) = (0usize, 0usize, 0usize, 0usize);
        let mut first = None;
        for (g, w) in got.iter().zip(&frames) {
            if g == w {
                continue;
            }
            first.get_or_insert_with(|| format!("web:    {g}\nnative: {w}"));
            let (gf, wf): (Vec<&str>, Vec<&str>) =
                (g.split('\t').collect(), w.split('\t').collect());
            for (a, b) in gf.iter().zip(&wf) {
                if a == b {
                    continue;
                }
                match a.split_once('=') {
                    Some((k, _)) if k.starts_with('r') => {
                        let (x, y): (Vec<&str>, Vec<&str>) =
                            (a.split(':').collect(), b.split(':').collect());
                        cells += usize::from(x.first() != y.first());
                        rasters += usize::from(x.get(1) != y.get(1));
                        pixels += usize::from(x.get(2) != y.get(2));
                    }
                    _ => other += 1,
                }
            }
        }
        let rows: usize = frames
            .iter()
            .map(|f| {
                f.split('\t')
                    .filter(|x| x.starts_with('r') && x.contains('='))
                    .count()
            })
            .sum();
        eprintln!(
            "web band parity: {} frames, {rows} band rows compared (cells, raster, pixels each), \
             {px} RGBA pixels digested, {cells} cell / {rasters} raster / {pixels} pixel row \
             differences, {other} other",
            frames.len()
        );
        assert!(
            first.is_none(),
            "the web band left the native golden: {cells} cell, {rasters} raster, {pixels} pixel \
             row differences, {other} other\n{}",
            first.unwrap_or_default()
        );
    }

    /// The three rows both web modules' page tests post — a record, a held
    /// warning and an error, each detail a sentence and a technical line —
    /// ticked a second later; and the page's lines for them read 90 s after
    /// they were stamped, on a UTC clock (the CPU and the GPU module pin the
    /// SAME lines: one grammar, the engine's).
    fn post_page_rows(t: &mut AtermTerminal, t0: Instant) -> Vec<String> {
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

    /// THE SETTINGS ▸ MESSAGES PAGE ON THE WEB (ruling 387): rows posted
    /// through `notice` come back from `messages_page` as the engine's page
    /// grammar with every word said — newest first, a script's tags as their
    /// chips (`Deploy`, `CI`), the rows on the band `showing now` and the
    /// record `recorded`, each detail's sentence and technical lines, each
    /// entry's meta line and Copy text, each chip's label and the page's
    /// chrome words (ruling 396) — framed `OK <n>`. The tag chip and the
    /// Problems switch filter it, and the count line, the segments' words
    /// and the tag pop-up say so; the page's `getTimezoneOffset()` moves the
    /// local times, and junk reads as UTC.
    #[test]
    fn the_web_page_is_the_engines_page() {
        let (mut t, t0) = clocked(20, 80);
        let want = post_page_rows(&mut t, t0);
        let wall = stamp().unix_ms + 90_000;
        assert_eq!(
            t.messages_page_at("", false, 0, wall),
            format!("OK {}\n{}", want.len(), want.join("\n"))
        );
        let rev = t.band.center.revision();
        // A chip: `deploy` alone, its chip lit, the count line says so.
        let deploy = t.messages_page_at("deploy", false, 0, wall);
        let lines: Vec<&str> = deploy.lines().collect();
        assert_eq!(lines.len(), 5, "{deploy}");
        assert_eq!(lines[0], "OK 4");
        assert_eq!(
            lines[1],
            format!(
                "page\trev={rev}\tnow={wall}\ttotal=3\tshown=1\tall=1\tproblems=1\tstatus=1 of 3{}",
                page_chrome(1, 1, Some("Deploy"))
            )
        );
        assert_eq!(
            lines[2],
            "chip\ttag=deploy\twords=Deploy\tcount=1\ton=1\tlabel=Deploy \u{00b7} 1"
        );
        assert_eq!(
            lines[3],
            "chip\ttag=ci\twords=CI\tcount=2\ton=0\tlabel=CI \u{00b7} 2"
        );
        assert_eq!(lines[4], want[3]);
        // Problems: the warning and the error; each chip counts its own.
        let problems = t.messages_page_at("", true, 0, wall);
        let lines: Vec<&str> = problems.lines().collect();
        assert_eq!(
            lines[1],
            format!(
                "page\trev={rev}\tnow={wall}\ttotal=3\tshown=2\tall=3\tproblems=2\tstatus=2 of 3{}",
                page_chrome(3, 2, None)
            )
        );
        assert_eq!(
            lines[3],
            "chip\ttag=ci\twords=CI\tcount=1\ton=0\tlabel=CI \u{00b7} 1"
        );
        assert_eq!(lines[4..], [want[3].as_str(), want[4].as_str()]);
        // The page's clock: `getTimezoneOffset()` is minutes WEST of UTC.
        assert_eq!(utc_offset_s(-120.0), 7200);
        assert_eq!(utc_offset_s(420.0), -25_200);
        for junk in [f64::NAN, f64::INFINITY, 1440.0, -2000.0] {
            assert_eq!(utc_offset_s(junk), 0, "{junk}");
        }
        let east = t.messages_page_at("", false, utc_offset_s(-120.0), wall);
        assert!(east.contains("\tlocal=Today 4:13:20 PM\t"), "{east}");
        assert!(!east.contains("2:13:20 PM"), "{east}");
    }

    /// THE WEB'S HOST OFFERS NOTHING IT CANNOT DO (rulings 387, 389): a
    /// restored native log's crash record offers its `Open log` NOT
    /// pressable — a page has no filesystem — and its `Manual` (a footer
    /// button worded `Open Manual`, ruling 396) NOT pressable either: the
    /// web performs no navigation, and no press route reaches a record
    /// (`notice act` on it answers `no live message`, pinned here). So
    /// neither leads: both lines end `primary=0` (ruling 407). An agent's
    /// upgrade record offers no word at all: no harness runs
    /// under a page, so no tab's upgrade stands (ruling 315). The log itself
    /// still names the word (the negative control: the page dropped it, the
    /// record did not).
    #[test]
    fn the_web_page_offers_no_file_and_no_upgrade() {
        let (mut t, t0) = clocked(20, 80);
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
                "action\tid=1\tindex=0\tlabel=Open log\tactionable=0\tbutton=Open log\tprimary=0",
                "action\tid=1\tindex=1\tlabel=Manual\tactionable=0\tbutton=Open Manual\tprimary=0",
            ],
            "{page}"
        );
        // Nothing pressable, nothing leads (ruling 407): the engine's rule
        // gives no Primary, and the key is the last of its line (appended).
        assert!(!page.contains("\tprimary=1"), "{page}");
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
