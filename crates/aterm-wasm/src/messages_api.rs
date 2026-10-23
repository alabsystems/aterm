// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! Message band bindings on [`WebTerminal`].

use std::collections::{BTreeSet, VecDeque};

use aterm_core::render::{HostRowPixels, RenderInput};
use aterm_core::terminal::RenderCell;
use aterm_messages::drive::{self, Commit, Lay, LookIn, View};
use aterm_messages::ink::{AnsiHues, BandInks, BarBase, ForcedPalette, RowRaster, ThemeInks};
use aterm_messages::page::{self, MessagesFilter, MessagesState};
use aterm_messages::paint::{Geometry, Hover, HoverTarget};
use aterm_messages::wire::{self, NoticeRequest, Paint, ReadQuery, WireGate};
use aterm_messages::{
    ActionIndex, Hit, Intent, LogLine, Look, MessageCenter, MessageId, MessageLog, Shelf,
    UpgradeWord, WallStamp, LOG_CAP, MAX_ROWS,
};
use aterm_render::band::{self as band_frame, BandFrame};
use aterm_render::ChromeBleed;
use aterm_time::Instant;

use crate::WebTerminal;

/// How many drained prepend buffers the band keeps for the next frame (the
/// band's most rows).
const POOL_CAP: usize = MAX_ROWS as usize;

/// How many drained log lines the page can still take
/// ([`WebTerminal::messages_log_since`]): the ring's record count.
const JOURNAL_CAP: usize = LOG_CAP;

/// How many of the page's kept lines a restore reads — the newest: twice the
/// ring, room for a full ring of records each posted and retired.
const RESTORE_CAP: usize = 2 * LOG_CAP;

/// Percent-encode a string into one space-free reply token: every byte that is not
/// ASCII-graphic, and `%`, becomes `%XX`.
pub(crate) fn pct_encode(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// A packed `0x00RRGGBB` colour as sRGB bytes.
#[inline]
const fn rgb(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// A page's number (a message id or a log cursor, which JS holds as a
/// `number`) as a whole count: negative, fractional-below-one or not a
/// number is 0.
#[inline]
fn whole(n: f64) -> u64 {
    if n.is_finite() && n >= 1.0 {
        n as u64
    } else {
        0
    }
}

/// The band row that frame pixel row `y` falls in: rows of `cell_h` px (zero reads as
/// one) stack from `grid_top`; `None` above them.
#[inline]
const fn band_row(y: usize, grid_top: usize, cell_h: usize) -> Option<usize> {
    if y < grid_top {
        return None;
    }
    let cell_h = if cell_h == 0 { 1 } else { cell_h };
    Some((y - grid_top) / cell_h)
}

/// The tick's reply ([`WebTerminal::messages_tick`]): bit 0, the band
/// changed; bit 1, its row count did.
#[inline]
fn tick_bits(changed: bool, rows_moved: bool) -> u32 {
    u32::from(changed || rows_moved) | (u32::from(rows_moved) << 1)
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

/// The band's state on one terminal: the engine's center and wire gate, the inks the
/// page's theme derives, and the painted rows the next frame composes.
pub(crate) struct WebBand {
    /// The engine: rows, holds, the log ring, the motion grid.
    pub(crate) center: MessageCenter,
    /// The wire's budgets and its one paced paint.
    pub(crate) gate: WireGate,
    /// Motion allowed (the page's reduced-motion).
    motion_ok: bool,
    /// The page is visible: a hidden page's band draws the still look and arms no timer.
    visible: bool,
    /// The page has a Details view: every row ends in its link, and a press
    /// on a row opens it ([`aterm_messages::Links::Painted`]).
    details: bool,
    /// The chip the pointer lights, `None` off the band.
    hover: Option<Hover>,
    /// The frame pixel the pointer was last on ([`WebTerminal::band_hover`]), from
    /// which the lit chip is resolved again when the layout changes under it.
    pointer: Option<(f64, f64)>,
    /// CSS `forced-colors`' five system colours while the page is forced.
    forced: Option<ForcedPalette>,
    /// The page theme's background, foreground and cursor.
    theme: ThemeInks,
    /// The configured palette's blue and cyan (slots 4 and 6), which a near-grey
    /// cursor's meter borrows, and its red and yellow (slots 1 and 3), the error and warn
    /// inks' seeds: the configured palette, never the live OSC 4 one.
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
    /// The page's kept log was loaded ([`WebTerminal::messages_restore`]):
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
            red: hue(1),
            green: hue(2),
            yellow: hue(3),
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
            pointer: None,
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
    /// allows motion, graded unless forced colours own it.
    #[inline]
    const fn look(&self) -> Look {
        drive::look(LookIn {
            motion_allowed: self.motion_ok,
            on_screen: self.visible,
            frozen: false,
            forced: self.forced.is_some(),
        })
    }

    /// How a `cols`-wide page lays the band out ([`Lay::web`]): links withheld
    /// until the page has a Details view, and no `$HOME` to abbreviate.
    #[inline]
    fn lay(&self, cols: usize) -> Lay {
        Lay::web(cols, self.details)
    }

    /// The inks the rows are painted with (the field's doc).
    #[inline]
    fn derive_inks(&self) -> BandInks {
        match self.forced {
            Some(hc) => BandInks::forced(hc),
            None => BandInks::derive(self.theme, Some(self.ansi), BarBase::Blend),
        }
    }

    /// A new page theme (packed `0x00RRGGBB`).
    #[inline]
    pub(crate) fn set_theme(&mut self, fg: u32, bg: u32, cursor: u32) {
        self.theme = ThemeInks {
            bg: rgb(bg),
            fg: rgb(fg),
            cursor: rgb(cursor),
        };
        self.inks = self.derive_inks();
    }

    /// A configured palette slot changed: slots 4 and 6 are the meter's
    /// borrowed hues, slots 1 and 3 the seeds of the error and warn inks.
    #[inline]
    pub(crate) fn set_palette_color(&mut self, index: u8, c: [u8; 3]) {
        match index {
            1 => self.ansi.red = c,
            3 => self.ansi.yellow = c,
            4 => self.ansi.blue = c,
            6 => self.ansi.cyan = c,
            _ => return,
        }
        self.inks = self.derive_inks();
    }

    /// The committed row count — the rows the frame reserves.
    #[inline]
    pub(crate) fn committed(&self) -> usize {
        usize::from(self.center.committed_rows())
    }

    /// The geometry a `cols`-wide band is painted onto: the frame, its cells
    /// `pad` px in.
    #[inline]
    fn geometry(cols: usize, cell_w: usize, pad: usize) -> Geometry {
        Geometry {
            win_w: cols * cell_w + 2 * pad,
            cells_x: pad,
            cell_w: cell_w.max(1),
        }
    }

    /// Prepare the band's rows for a frame at `self.now` on a `cols`-wide grid of
    /// `cell_w`-px cells `pad` px in: the layout (re-laid only when the center or the
    /// width moved), one motion frame, and the paint only when its inputs moved.
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

    /// The instant the band next needs the page back: a hold's expiry, an echo's end, a
    /// queued row's patience, the wire's paced paint or the next motion frame.
    pub(crate) fn next_wake(&self, now: Instant, cols: usize) -> Option<Instant> {
        if !self.visible {
            return None;
        }
        let reserved = u16::try_from(self.committed()).unwrap_or(u16::MAX);
        let motion = drive::motion_deadline_over(
            &self.center,
            reserved,
            false,
            now,
            [(&self.view, self.lay(cols), self.look())],
        );
        [
            drive::state_deadline(&self.center, &self.gate, false),
            motion,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Compose the painted rows above the grid of `dst`, the frame `at`: padded or
    /// trimmed to the committed count, each raster placed, every window-space effect
    /// stream translated down with the grid (the pipeline's chrome origin excludes the
    /// band).
    pub(crate) fn compose(&mut self, dst: &mut RenderInput, at: BandFrame) -> usize {
        band_frame::compose_band(
            dst,
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

    /// A fresh engine whose motion grid starts at `t0` (the tests' injected clock; the
    /// native golden anchors its center the same way).
    #[cfg(test)]
    pub(crate) fn reset_at(&mut self, t0: Instant) {
        self.center = MessageCenter::new(MessageLog::default(), t0);
        self.gate = WireGate::default();
        self.now = t0;
        self.restored = false;
    }

    /// Whether the page's kept log may still be loaded: nothing restored, minted, queued
    /// or drained yet.
    fn restorable(&self) -> bool {
        let log = self.center.log();
        !self.restored
            && log.next_id() == MessageId::FIRST
            && log.is_empty()
            && log.pending_len() == 0
            && self.journal.is_empty()
            && self.journal_first == 0
    }

    /// Hand a drained prepend back to the pool, undoing the last frame's prepend on the
    /// kept scratch so an echo frame under a band keeps the damage-scoped refill.
    #[inline]
    pub(crate) fn undo(&mut self, dst: &mut RenderInput, engine_rows: usize) -> bool {
        dst.undo_host_row_prepend(engine_rows, &mut self.pool, POOL_CAP)
    }

    /// The band row and column under frame pixel `(x, y)`: the band's rows stack from
    /// `m.grid_top`, and the column is `Geometry::cell_at`.
    #[inline]
    fn cell_under(&self, x: f64, y: f64, cols: usize, m: BandMetrics) -> Option<(usize, usize)> {
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        let row = band_row(y as usize, m.grid_top, m.cell_h)?;
        (row < self.committed()).then(|| {
            (
                row,
                Self::geometry(cols, m.cell_w, m.pad).cell_at(x as usize),
            )
        })
    }

    /// What a press at band `(row, col)` lands on, through the layout the
    /// painter drew (the kept one while the view is prepared at `cols`).
    #[inline]
    fn hit(&self, row: usize, col: usize, cols: usize) -> Hit {
        match self.view.layout() {
            Some(p) if self.view.is_prepared(&self.center, cols) => p.hit(row, col),
            _ => self.lay(cols).presentation(&self.center).hit(row, col),
        }
    }

    /// The chip a pointer on `hit` lights: a capsule (the `Details ›`
    /// included); on the row body and the overflow row, with a Details view,
    /// the link a press there follows.
    #[inline]
    const fn hover_target(&self, hit: Hit) -> Option<HoverTarget> {
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

/// The page's host for [`WebTerminal::messages_page`] (`page::Host`): the wall clock
/// at the export and the page's UTC offset, and none of the facts only a native host
/// has.
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
    /// lines [`WebTerminal::messages_log_since`] hands it.
    fn log_folder(&self) -> Option<String> {
        None
    }

    /// Saved: the page keeps its log itself, and the engine's not-saved note speaks of a
    /// log file the web never has.
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

    /// No filesystem, so `Open log` is never pressable: a page cannot see a file, and a
    /// press on an authored intent is only logged (`performed=0`).
    fn is_regular_file(&self, _path: &str) -> bool {
        false
    }

    /// No harness: no agent upgrade runs under a page, so none takes a word.
    fn upgrade_takes(&self, _tab: &str, _to: &str, _word: UpgradeWord) -> bool {
        false
    }

    /// No harness: no tab's upgrade stands here, so upgrade words are not offered.
    fn upgrade_stands(&self, _tab: &str) -> bool {
        false
    }

    /// No navigation: the web performs no authored intent, and every capsule a page
    /// lists is a record's, which neither press route reaches (`notice act` and a band
    /// press take only live rows).
    fn performs_navigation(&self) -> bool {
        false
    }

    /// One: the native reporters give every record a one-line sentence except agent
    /// upgrade rows, which only the native harness writes.
    fn sentence_lines(&self, _key: Option<&str>, _detail: &[String]) -> usize {
        1
    }
}

/// A page's `Date.prototype.getTimezoneOffset()` (minutes, positive west of UTC) as
/// the engine's offset in seconds east of UTC.
#[inline]
fn utc_offset_s(timezone_offset_min: f64) -> i64 {
    if timezone_offset_min.is_finite() && timezone_offset_min.abs() < 1440.0 {
        // The sign flips in the float (exact, and `round` is symmetric), so
        // no integer negation is left that could overflow.
        (timezone_offset_min * -60.0).round() as i64
    } else {
        0
    }
}

/// The wall clock's reading as the engine's stamp.
#[inline]
fn wall_now() -> WallStamp {
    let unix_ms = aterm_time::SystemTime::now()
        .duration_since(aterm_time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    WallStamp { unix_ms }
}

impl WebTerminal {
    /// [`Self::notice`] at `now`, stamped `wall`.
    pub(crate) fn notice_at(&mut self, line: &str, now: Instant, wall: WallStamp) -> String {
        let req = match NoticeRequest::parse(line) {
            Ok(req) => req,
            Err(refusal) => return format!("ERR {refusal}"),
        };
        if let NoticeRequest::Act { id, press } = &req {
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
    /// hands back the intent, and the band settles.
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
    /// host, as the engine's lines (`page::wire_lines`).
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
                floor: None,
            },
        );
        let (lines, _) = drive::drain_and_pace(&mut band.center, &mut band.gate, now, true);
        band.journal_push(lines);
        let rows_moved = committed.regrid.is_some();
        let m = self.band_metrics();
        let mut changed = self.band.prepare(self.cols, m.cell_w, m.pad);
        if self.settle_band_hover() {
            changed |= self.band.prepare(self.cols, m.cell_w, m.pad);
        }
        if changed || rows_moved {
            self.note_band_change(rows_moved);
        }
        tick_bits(changed, rows_moved)
    }

    /// [`Self::messages_next_wake_ms`] at `now`.
    pub(crate) fn messages_next_wake_ms_at(&self, now: Instant) -> f64 {
        match self.band.next_wake(now, self.cols) {
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
        let mut changed = self.band.prepare(self.cols, m.cell_w, m.pad);
        if self.settle_band_hover() {
            changed |= self.band.prepare(self.cols, m.cell_w, m.pad);
        }
        if changed {
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
        self.band.pointer = None;
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
        self.band.pointer = Some((x, y));
        let hover = self.band_hover_under(x, y);
        if self.band.hover == hover {
            return false;
        }
        self.band.hover = hover;
        self.band_redraw_at(now);
        true
    }

    /// The chip a pointer at frame pixel `(x, y)` lights, through the layout
    /// the painter drew; `None` off the band and where a press does nothing.
    fn band_hover_under(&self, x: f64, y: f64) -> Option<Hover> {
        let m = self.band_metrics();
        self.band
            .cell_under(x, y, self.cols, m)
            .and_then(|(row, col)| {
                let target = self.band.hover_target(self.band.hit(row, col, self.cols))?;
                Some(Hover {
                    row: u8::try_from(row).ok()?,
                    target,
                })
            })
    }

    /// Resolve the hover again from the pointer's last pixel: a press, a hold's fold, a
    /// `done` or a new row moves the chips under a still pointer, and the page resolves
    /// hover only on `pointermove`.
    fn settle_band_hover(&mut self) -> bool {
        let hover = self
            .band
            .pointer
            .and_then(|(x, y)| self.band_hover_under(x, y));
        if self.band.hover == hover {
            return false;
        }
        self.band.hover = hover;
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
                return "OK none".to_string();
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

    /// [`Self::band_press_acts`]: the arms [`Self::band_press_at`] performs.
    pub(crate) fn band_press_acts_at(&self, x: f64, y: f64) -> bool {
        let m = self.band_metrics();
        self.band
            .cell_under(x, y, self.cols, m)
            .is_some_and(|(row, col)| match self.band.hit(row, col, self.cols) {
                Hit::Capsule(..) => true,
                Hit::Details(_) | Hit::Body(_) | Hit::Overflow => self.band.details,
                Hit::Nothing => false,
            })
    }

    /// [`Self::set_frame_period_ms`]: a positive, finite period only.
    pub(crate) fn set_frame_period(&mut self, ms: f64) {
        if ms.is_finite() && ms > 0.0 {
            let period = aterm_messages::Duration::from_secs_f64(ms.min(1000.0) / 1000.0);
            self.band.center.set_refresh(period);
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

    /// Hand the effects pipeline the band's height (the render hook before the effects).
    #[inline]
    pub(crate) fn hand_band_to_effects(&mut self) {
        let px = u16::try_from(self.band_height_px()).unwrap_or(u16::MAX);
        self.effects.set_host_rows_px(px);
    }

    /// [`Self::messages_restore`] at `now`: the kept lines replayed as the native launch
    /// replays its file (`MessageLog::replay_all`), then a fresh center over that log.
    pub(crate) fn messages_restore_at(&mut self, lines: &str, now: Instant) -> String {
        if !self.band.restorable() {
            return "ERR restore: only once, before the first notice".to_string();
        }
        let kept = read_kept(lines);
        let mut log = MessageLog::empty();
        log.replay_all(kept.lines);
        let replaced = self.band.center.revision();
        self.band.center = MessageCenter::new(log, now);
        self.band.center.follow_revision(replaced);
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

    /// Compose the band onto the frame scratch (the render hook after the effects and
    /// the scroll stamp): re-prepare (a resize, cell size, pad or theme change lands
    /// here), prepend the rows with every window-space stream translated, move the grid
    /// band below them and hand the renderer the bleed.
    pub(crate) fn compose_band_into_frame(&mut self) {
        let m = self.band_metrics();
        let _ = self.band.prepare(self.cols, m.cell_w, m.pad);
        if self.settle_band_hover() {
            let _ = self.band.prepare(self.cols, m.cell_w, m.pad);
        }
        let at = BandFrame {
            cols: self.cols,
            cell_w: m.cell_w,
            cell_h: m.cell_h,
            pad: m.pad,
            lo: 0,
            frame_w: self.cols * m.cell_w + 2 * m.pad,
            grid_top: m.grid_top,
            room: None,
        };
        let n = self.band.compose(&mut self.frame_scratch, at);
        self.frame_scratch.grid_top_row += n;
        self.frame_scratch.grid_bot_row += n;
        let bleed = self.band.bleed(n);
        self.set_band_bleed(bleed);
    }
}

/// The JS-visible surface, which each crate's `web_terminal_impl!` expands onto its
/// own terminal type under its own export attribute: the attribute names the JS class
/// after the impl's type, which this shared file cannot spell.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Handle one `notice` control command line.
            pub fn notice(&mut self, line: &str) -> String {
                self.notice_at(line, Instant::now(), wall_now())
            }

            /// Handle the `messages` read verb.
            pub fn messages_read(&self, args: &str) -> String {
                self.messages_read_at(args, wall_now().unix_ms)
            }

            /// Format one message record's `messages` row by `id`.
            pub fn messages_entry(&self, id: f64) -> String {
                self.messages_entry_at(id, wall_now().unix_ms)
            }

            /// Render the Settings ▸ Messages page.
            pub fn messages_page(
                &self,
                tag: &str,
                problems: bool,
                timezone_offset_min: f64,
            ) -> String {
                self.messages_page_at(
                    tag,
                    problems,
                    utc_offset_s(timezone_offset_min),
                    wall_now().unix_ms,
                )
            }

            /// Advance the message band to `now` and return action flags.
            pub fn messages_tick(&mut self) -> u32 {
                self.messages_tick_at(Instant::now())
            }

            /// Milliseconds until the band next needs [`Self::messages_tick`], or `-1` when idle.
            pub fn messages_next_wake_ms(&self) -> f64 {
                self.messages_next_wake_ms_at(Instant::now())
            }

            /// Set whether band motion animations are allowed.
            pub fn set_band_motion(&mut self, allowed: bool) {
                self.set_band_motion_at(allowed, Instant::now());
            }

            /// Set the display frame period in milliseconds.
            pub fn set_frame_period_ms(&mut self, ms: f64) {
                self.set_frame_period(ms);
            }

            /// Set whether the page is visible.
            pub fn set_band_visible(&mut self, visible: bool) {
                self.set_band_visible_at(visible, Instant::now());
            }

            /// Set whether the host page provides a Details view.
            pub fn set_details_view(&mut self, available: bool) {
                self.set_details_view_at(available, Instant::now());
            }

            /// Configure CSS `forced-colors` system palette overrides.
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

            /// Update band hover state at frame pixel `(x, y)`.
            pub fn band_hover(&mut self, x: f64, y: f64) -> bool {
                self.band_hover_at(x, y, Instant::now())
            }

            /// Whether a press at frame pixel `(x, y)` triggers a band action.
            pub fn band_press_acts(&self, x: f64, y: f64) -> bool {
                self.band_press_acts_at(x, y)
            }

            /// Handle a press at frame pixel `(x, y)`.
            pub fn band_press(&mut self, x: f64, y: f64) -> String {
                self.band_press_at(x, y, Instant::now())
            }

            /// Return `m1` log lines since `cursor`.
            pub fn messages_log_since(&mut self, cursor: f64) -> String {
                self.messages_log_since_at(cursor)
            }

            /// Record a page-unload quit event and return `m1` log lines since `cursor`.
            pub fn messages_quit(&mut self, cursor: f64) -> String {
                self.messages_quit_at(cursor, Instant::now())
            }

            /// Restore message band state from persisted `m1` log lines.
            pub fn messages_restore(&mut self, lines: &str) -> String {
                self.messages_restore_at(lines, Instant::now())
            }

            /// Committed message band rows (`0..=3`).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn band_rows(&self) -> u32 {
                u32::from(self.band.center.committed_rows())
            }

            /// Committed message band height in device pixels.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn band_height_px(&self) -> u32 {
                let m = self.band_metrics();
                u32::try_from(self.band.committed() * m.cell_h).unwrap_or(u32::MAX)
            }

            /// Y offset of the terminal grid's first row in device pixels.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn grid_top_px(&self) -> u32 {
                u32::try_from(self.band_metrics().grid_top)
                    .unwrap_or(u32::MAX)
                    .saturating_add(self.band_height_px())
            }
        }
    };
}
web_terminal_impl!(bind);

/// Each crate's own tests of this surface (`src/messages_api_tests.rs` of the crate
/// compiling this file): the CPU and GPU hosts draw differently.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messages_api_tests.rs"
    ));

    fn stamp() -> WallStamp {
        WallStamp {
            unix_ms: 1_790_000_000_000,
        }
    }

    fn ms(n: u64) -> aterm_messages::Duration {
        aterm_messages::Duration::from_millis(n)
    }

    /// Frame pixel `(x, y)` at the middle of band cell `(row, col)`.
    fn band_px(t: &Host, row: usize, col: usize) -> (f64, f64) {
        let m = t.band_metrics();
        (
            (m.pad + col * m.cell_w + m.cell_w / 2) as f64,
            (m.grid_top + row * m.cell_h + m.cell_h / 2) as f64,
        )
    }

    /// The `rev=` and `total=` fields of the page's head line.
    fn page_head(page: &str) -> (String, String) {
        let head = page.lines().nth(1).expect("the page's head line");
        let field = |name: &str| {
            head.split('\t')
                .find_map(|f| f.strip_prefix(name))
                .expect("a head field")
                .to_string()
        };
        (field("rev="), field("total="))
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
    fn wake_the_pet(t: &mut Host) -> bool {
        t.set_cursor_glow(true, "rainbow kitty", None, None, 260, 24, 0.7, 0.6, true);
        t.set_cursor_pet(true, 7);
        draw(t);
        for frame in 0..240u32 {
            if frame % 6 == 0 {
                let ch = char::from(b'a' + u8::try_from(frame / 6 % 26).unwrap_or(0));
                t.note_typed_char(ch);
                t.process(&[ch as u8]);
            }
            t.advance_effects(33.0);
            draw(t);
            if t.cursor_pet_alpha() > 0 {
                return true;
            }
        }
        false
    }

    /// The pet under a band is pressed where it is drawn: the page presses in
    /// canvas pixels, band included, so with a row committed a press on the drawn
    /// body's centre or feet pets the cat and a press on the text half a row
    /// above passes to the terminal.
    #[test]
    fn the_pet_is_pressed_where_the_band_draws_it() {
        let (mut t, t0) = clocked(12, 60);
        t.set_chrome(6, 0).expect("admitted geometry");
        t.process(b"\r\n\r\n\r\n$ ");
        assert!(wake_the_pet(&mut t), "the pet must reach the glass");
        let m = t.band_metrics();
        let half_row = m.cell_h as f32 / 2.0;
        for band_rows in [0, 1] {
            if band_rows == 1 {
                let reply = t.notice_at("post ci sev=warn Disk nearly full", t0, stamp());
                assert!(reply.starts_with("OK message="), "{reply}");
                draw(&mut t);
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

    /// A still pointer's lit chip follows the layout: when the progress row under
    /// the pointer ends `done … ok`, its echo is `Hit::Nothing`, so the band draws
    /// as a twin's that took the same lines with no pointer.
    #[test]
    fn a_still_pointers_chip_never_lights_the_echo() {
        let (mut t, t0) = clocked(12, 60);
        let (mut twin, _) = clocked(12, 60);
        twin.band.reset_at(t0);
        for x in [&mut t, &mut twin] {
            x.set_chrome(6, 0).expect("admitted geometry");
            let _ = x.notice_at("progress p pct=40 Downloading assets", t0, stamp());
            x.set_details_view_at(true, t0);
            let _ = x.messages_tick_at(t0 + ms(2100));
        }
        assert_eq!(t.band_rows(), 1, "premise: revealed");
        let (x, y) = band_px(&t, 0, 20);
        assert!(t.band_hover_at(x, y, t0 + ms(2101)), "the body lights");
        for b in [&mut t, &mut twin] {
            let _ = b.messages_tick_at(t0 + ms(2101));
            draw(b);
        }
        assert!(!same_picture(&t, &twin), "control: the lit chip shows");
        for b in [&mut t, &mut twin] {
            let reply = b.notice_at("done p ok Downloaded assets", t0 + ms(2102), stamp());
            assert!(reply.starts_with("OK"), "{reply}");
        }
        let at = t0 + ms(2200);
        for b in [&mut t, &mut twin] {
            let _ = b.messages_tick_at(at);
            draw(b);
        }
        let echo = t
            .band
            .view
            .layout()
            .and_then(|p| p.rows.first().map(|r| r.kind))
            .expect("a laid-out row");
        assert!(
            matches!(echo, aterm_messages::RowKind::Echo(_)),
            "premise: the echo is on the band: {echo:?}"
        );
        let differ = !same_picture(&t, &twin);
        let flips = t.band_hover_at(x, y, at);
        draw(&mut t);
        let resolved = same_picture(&t, &twin);
        assert!(
            !differ,
            "the echo draws a lit chip the still pointer left (a fresh hover at the same \
             pixel changes the lit state: {flips}; after it the frames agree: {resolved})"
        );
    }

    /// A restore moves the page's `rev=`, as any observable log change does; a
    /// post after it moves it again (control).
    #[test]
    fn the_restore_moves_the_pages_rev() {
        let (mut prev, p0) = clocked(20, 80);
        for line in [
            "post ci Build finished",
            "post ci sev=warn hold=100 Disk nearly full",
        ] {
            let reply = prev.notice_at(line, p0, stamp());
            assert!(reply.starts_with("OK"), "{line}: {reply}");
        }
        let _ = prev.messages_tick_at(p0 + ms(10));
        let kept: Vec<String> = prev
            .messages_log_since_at(0.0)
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect();
        let (mut t, t0) = clocked(20, 80);
        let wall = stamp().unix_ms + 90_000;
        let (rev0, total0) = page_head(&t.messages_page_at("", false, 0, wall));
        assert_eq!(total0, "0", "premise: nothing yet");
        let reply = t.messages_restore_at(&kept.join("\n"), t0 + ms(1));
        assert!(reply.starts_with("OK records=2 "), "{reply}");
        let _ = t.messages_tick_at(t0 + ms(2));
        let (rev1, total1) = page_head(&t.messages_page_at("", false, 0, wall));
        assert_eq!(total1, "2", "premise: the log moved");
        assert_ne!(
            rev1, rev0,
            "the restore moved the log from 0 to 2 records and `rev=` did not move"
        );
        let _ = t.notice_at("post ci sev=warn Fresh warning", t0 + ms(3), stamp());
        let (rev2, _) = page_head(&t.messages_page_at("", false, 0, wall));
        assert_ne!(rev2, rev1, "control: a post moves it");
    }
}
