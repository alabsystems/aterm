//! A pure VT/ECMA-48 screen: the deterministic fold of recorded output.
//!
//! [`fold`] turns an ordered slice of [`Record`]s into a [`Screen`] with no
//! clock, no randomness, and no I/O, so the same records always yield a
//! byte-identical screen ([`Screen::serialize`]). That purity is the spine of
//! the astream-term replay guarantee.
//!
//! ### Scope
//!
//! A deliberately bounded but real VT subset: C0 controls (CR/LF/TAB/BS, and
//! VT/FF as line feeds), index (IND/NEL/RI), CSI cursor motion
//! (CUU/CUD/CUF/CUB/CNL/CPL/CHA/VPA/CUP), erase (ED/EL/ECH), insert and delete
//! (ICH/DCH/IL/DL), SGR (the
//! 16- and 256-colour palettes in both the `;` and the `:` sub-parameter form,
//! plus bold/underline/reverse), the alternate screen (`?1049`/`?47`/`?1047`),
//! cursor visibility (`?25`), save/restore cursor (DECSC/DECRC and `CSI s`/`u`,
//! one slot per screen buffer as in xterm), the DEC scrolling region (`DECSTBM`),
//! RIS, and inline [`Record::Resize`]. The scroll region is cross-validated
//! against the production aterm engine (see `term.fold.scroll-region`).
//!
//! Everything else in the byte stream is **consumed, never painted**, following
//! the ECMA-48 parser: OSC/DCS/APC/PM/SOS string sequences (window titles,
//! OSC-133 shell marks, image payloads) are swallowed up to their terminator
//! (`ESC \`, or BEL for OSC); two-byte escapes (`ESC ( B` charset designations,
//! `ESC =`/`ESC >`) and CSI sequences outside the subset (any with an
//! intermediate byte, or a private marker other than `?`) are skipped whole;
//! truecolour SGR (`38;2;r;g;b`, `38:2::r:g:b`) is consumed *with* its
//! sub-parameters and leaves the pen unchanged; DEL, and C1 controls arriving
//! as UTF-8 (U+0080..U+009F), are ignored; malformed UTF-8 (overlong,
//! surrogate, truncated, a stray continuation or 8-bit byte) yields one U+FFFD
//! per maximal subpart (Unicode 3.9) — a control character never lands in a
//! cell. `ESC` anywhere begins a new escape, and a
//! C0 control executes inside an unfinished CSI.
//!
//! Simplifications, documented so they are never mistaken for fidelity — each is
//! deterministic:
//!
//! * **Deferred line wrap** is modelled with xterm's last-column semantics: after
//!   the last column is painted the cursor reports `col == cols`, and the next
//!   glyph wraps. CR/LF/BS/CUB/CUF/CUU/CUD/CUP resolve the pending wrap back
//!   onto the last column first — so BS after a full line lands on the
//!   second-to-last column (`abcde BS X` on 5 columns is `abcXe`) — and so do
//!   ED/EL 0-2, which then erase from that column: xterm's `ClearRight` clears
//!   from `cur_col` inclusive, the parked glyph, and every erase ends in
//!   `ResetWrap` (util.c). TAB leaves the wrap pending, `ED 3` touches neither
//!   the screen nor the wrap, and DECSC/DECRC round-trip it (a restored cursor
//!   wraps on its next glyph, as in aterm/xterm). Scrolling honours the
//!   `DECSTBM` region, and CUU/CUD (and CNL/CPL) stop at its margins as in
//!   xterm.
//! * **Every blank the fold makes honours background-colour-erase** (`bce`):
//!   `ED`/`EL`, a line scrolled in, and the alternate-screen clears fill with
//!   the current SGR background, as xterm's `ClearCells` / `ScrnClearLines`
//!   write the current colour pair, not a hardcoded default blank
//!   (reverse-video bce is out of scope). Cross-validated against aterm.
//! * **One alternate buffer, as in xterm**: it is allocated blank on first
//!   use and persists across switches and across RIS (xterm's `ReallyReset`
//!   clears it only if it is up), with its own DECSC slot, which RIS keeps too.
//!   `?47` only switches; `?1047` switches and clears it on the way out;
//!   `?1049` saves the cursor, switches and clears it on the way in (even when
//!   already up), and restores the cursor on the way out (even when not up).
//!   Each clear is xterm's `ClearScreen` (charproc.c `srm_ALTBUF`,
//!   `srm_OPT_ALTBUF`, `srm_OPT_ALTBUF_CURSOR`), which resets the pending
//!   wrap. The cursor is otherwise shared between the buffers and never moved
//!   by a switch.
//! * Character sets, origin mode, insert mode (IRM), left/right margins,
//!   SU/SD and truecolour are not modelled (consumed and ignored). ICH/DCH/IL/DL
//!   and ECH follow xterm's `InsertChar`/`DeleteChar`/`InsertLine`/
//!   `DeleteLine`/`ClearRight`: each resets a pending wrap, IL/DL act only on a
//!   cursor inside the scrolling region and move it to column 0, and every
//!   vacated cell takes the bce blank.
//! * **One codepoint per cell**: an East Asian wide character takes one cell,
//!   not two, and a combining (zero-width) mark takes a cell of its own.
//! * DECSC/DECRC save and restore the cursor position (with a pending wrap)
//!   and the SGR rendition, as xterm's `CursorSave`/`CursorRestore` do; a
//!   restore from a slot never saved homes the cursor and resets the
//!   rendition (xterm's zeroed `SavedCursor`). The charsets and origin mode
//!   xterm also saves are not modelled. There is no scrollback, so `ED 3` (xterm's
//!   "erase saved lines", `do_erase_display` case 3) changes nothing here: the
//!   visible screen is not saved lines.
//! * **Resize clips/extends**; it does not reflow wrapped lines.
//! * Grid dimensions are clamped to `1..=`[`MAX_DIM`] so hostile input cannot
//!   trigger an unbounded allocation (panic-free, like `astream-wire`).

use crate::ops::ScreenOp;
use crate::record::Record;

/// Where [`Screen::serialize_into`] writes its encoding: a byte buffer, or a
/// hasher that consumes it without buffering.
pub(crate) trait ByteSink {
    fn extend_from_slice(&mut self, bytes: &[u8]);
    fn push(&mut self, byte: u8) {
        self.extend_from_slice(&[byte]);
    }
}

impl ByteSink for Vec<u8> {
    fn extend_from_slice(&mut self, bytes: &[u8]) {
        Vec::extend_from_slice(self, bytes);
    }
    fn push(&mut self, byte: u8) {
        Vec::push(self, byte);
    }
}

/// Largest grid edge the fold will allocate, in cells. A guard against a hostile
/// [`Record::Resize`] requesting a multi-gigabyte grid.
pub const MAX_DIM: u16 = 1000;

/// An SGR colour: the terminal default, or a palette index (`0..=255`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// The terminal's default foreground/background.
    Default,
    /// A palette index: `0..=7` standard, `8..=15` bright, `16..=255` extended.
    Indexed(u8),
}

/// Cell attribute bits (a bitset in [`Cell::attrs`]).
pub mod attr {
    /// Bold / increased intensity.
    pub const BOLD: u8 = 1 << 0;
    /// Underline.
    pub const UNDERLINE: u8 = 1 << 1;
    /// Reverse video (swap fg/bg).
    pub const REVERSE: u8 = 1 << 2;
    /// Faint / dim — used to render an unconfirmed speculative echo.
    pub const FAINT: u8 = 1 << 3;
}

/// One character cell of the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The displayed character (one `char`; grapheme clustering is a later rung).
    pub ch: char,
    /// Foreground colour.
    pub fg: Color,
    /// Background colour.
    pub bg: Color,
    /// Attribute bits (see [`attr`]).
    pub attrs: u8,
}

impl Cell {
    /// An empty cell: a space with default colours and no attributes.
    pub const BLANK: Cell = Cell {
        ch: ' ',
        fg: Color::Default,
        bg: Color::Default,
        attrs: 0,
    };
}

/// The current drawing pen (SGR state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pen {
    fg: Color,
    bg: Color,
    attrs: u8,
}

/// What DECSC saves (xterm `CursorSave2`, within this fold's state): the
/// position, a pending wrap as `col == cols`, and the SGR pen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SavedCursor {
    row: u16,
    col: u16,
    pen: Pen,
}

impl Pen {
    const fn reset() -> Pen {
        Pen {
            fg: Color::Default,
            bg: Color::Default,
            attrs: 0,
        }
    }
}

/// A terminal screen: the foldable state. Build one with [`fold`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    cols: u16,
    rows: u16,
    grid: Vec<Cell>,
    row: u16,
    col: u16,
    pen: Pen,
    cursor_visible: bool,
    altscreen: bool,
    /// The buffer not on screen: the main grid while the alternate screen is
    /// up, and the alternate grid while the main one is (`None` until the
    /// alternate screen is first entered).
    inactive: Option<Vec<Cell>>,
    /// The main screen's DECSC slot (`ESC 7` / `CSI s`, and the `?1049` round
    /// trip). Never consumed by a restore.
    saved_cursor: Option<SavedCursor>,
    /// The alternate screen's own DECSC slot (xterm `sc[1]`): a DECSC issued
    /// there never clobbers the main slot, and it lasts as long as the
    /// alternate buffer does.
    alt_saved_cursor: Option<SavedCursor>,
    /// Top row of the DEC scrolling region (`DECSTBM`), 0-based inclusive.
    scroll_top: u16,
    /// Bottom row of the DEC scrolling region (`DECSTBM`), 0-based inclusive.
    /// `(scroll_top, scroll_bottom) == (0, rows-1)` means the full screen scrolls.
    scroll_bottom: u16,
}

/// Fold an ordered slice of records into the screen they produce.
///
/// `cols`/`rows` are the initial viewport size (clamped to `1..=`[`MAX_DIM`]).
/// [`Record::In`] and [`Record::Exit`] are ignored — input does not paint.
pub fn fold(cols: u16, rows: u16, records: &[Record]) -> Screen {
    let mut folder = Folder::new(cols, rows);
    for rec in records {
        folder.apply(rec);
    }
    folder.into_screen()
}

/// A resumable terminal fold: the [`Screen`] plus the live parser state, so it
/// can be advanced record-by-record and **cloned to snapshot the exact
/// mid-stream state** — including a parser paused mid-escape-sequence or
/// mid-string-sequence (when an escape, or an OSC/DCS payload, is split across
/// two `Out` records). Cloning a `Folder` at offset `K` and feeding it only the
/// records after `K` reproduces the same screen as folding the whole log: this
/// is what makes `/a/state` + log-tail resume exact rather than approximate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    screen: Screen,
    parser: Parser,
}

impl Folder {
    /// A fresh folder over a blank screen of the given size.
    pub fn new(cols: u16, rows: u16) -> Folder {
        Folder {
            screen: Screen::new(cols, rows),
            parser: Parser::new(),
        }
    }

    /// Advance by one record. `In`/`Exit` do not paint (only echoed `Out` does).
    pub fn apply(&mut self, record: &Record) {
        match record {
            Record::Out(bytes) => self.parser.feed(&mut self.screen, bytes),
            Record::Resize { cols, rows } => {
                self.screen.resize(*cols, *rows);
                self.parser.rec(ScreenOp::Resize(*cols, *rows));
            }
            Record::In { .. } | Record::Exit { .. } => {}
        }
    }

    /// The current screen.
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Consume the folder, returning its screen.
    pub fn into_screen(self) -> Screen {
        self.screen
    }
}

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
#[path = "screen_island.rs"]
mod screen_island;
#[cfg(not(clean_islands))]
pub use crate::twins::screen_clamp_dim;
#[cfg(clean_islands)]
pub use screen_island::screen_clamp_dim;

contract_fn! {
    /// Kernel-certified bounds check that a screen row/column coordinate `coord`
    /// lies strictly within dimension `dim` (`coord < dim`).
    ///
    /// Discharged in the Clean CIC kernel by definitional equality against
    /// `screen_coord_in_bounds_isl` (`clean-kernel-defeq`).
    #[must_use]
    pub const fn screen_coord_in_bounds(coord: u16, dim: u16) -> bool
    clauses [ensures result == (coord < dim)]
    {
        coord < dim
    }
}

/// Maximum total cells a single screen grid may allocate (`MAX_DIM * MAX_DIM`).
const MAX_GRID_CELLS: usize = (MAX_DIM as usize) * (MAX_DIM as usize);

fn blank_grid(cols: u16, rows: u16) -> Vec<Cell> {
    let n = (cols as usize)
        .saturating_mul(rows as usize)
        .min(MAX_GRID_CELLS);
    let cap = if n < MAX_GRID_CELLS {
        n
    } else {
        MAX_GRID_CELLS
    };
    let mut grid = Vec::with_capacity(cap);
    let mut i = 0usize;
    while i < n {
        grid.push(Cell::BLANK);
        i = i.saturating_add(1);
    }
    grid
}

impl Screen {
    /// A blank screen of the given size (each edge clamped to `1..=`[`MAX_DIM`]).
    pub fn new(cols: u16, rows: u16) -> Screen {
        let cols = screen_clamp_dim(cols.max(1) as u64, MAX_DIM as u64) as u16;
        let rows = screen_clamp_dim(rows.max(1) as u64, MAX_DIM as u64) as u16;
        Screen {
            cols,
            rows,
            grid: blank_grid(cols, rows),
            row: 0,
            col: 0,
            pen: Pen::reset(),
            cursor_visible: true,
            altscreen: false,
            inactive: None,
            saved_cursor: None,
            alt_saved_cursor: None,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
        }
    }

    /// Current `(cols, rows)`.
    pub const fn dims(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Cursor position as `(row, col)`, 0-based. `col` equals `cols` while a
    /// line-wrap is pending (deferred wrap: the cursor sits past the last
    /// column until the next glyph wraps or cursor motion resolves it).
    pub const fn cursor(&self) -> (u16, u16) {
        (self.row, self.col)
    }

    /// Whether the cursor is currently visible (`?25`).
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Whether the alternate screen is active.
    pub const fn is_altscreen(&self) -> bool {
        self.altscreen
    }

    /// The cell at `(row, col)`, or `None` if out of bounds.
    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if screen_coord_in_bounds(row, self.rows) && screen_coord_in_bounds(col, self.cols) {
            self.grid.get(self.idx(row, col))
        } else {
            None
        }
    }

    /// Overlay a single cell. Used by speculative echo to paint an unconfirmed
    /// prediction onto a copy of the authoritative screen. No-op if out of bounds.
    pub fn set_cell(&mut self, row: u16, col: u16, cell: Cell) {
        if screen_coord_in_bounds(row, self.rows) && screen_coord_in_bounds(col, self.cols) {
            let i = self.idx(row, col);
            if let Some(slot) = self.grid.get_mut(i) {
                *slot = cell;
            }
        }
    }

    /// The text of a row, with trailing blanks trimmed. Empty if out of bounds.
    pub fn line_text(&self, row: u16) -> String {
        if row >= self.rows {
            return String::new();
        }
        let start = self.idx(row, 0);
        let end = start.saturating_add(self.cols as usize);
        let mut line: String = self
            .grid
            .get(start..end)
            .unwrap_or(&[])
            .iter()
            .map(|c| c.ch)
            .collect();
        line.truncate(line.trim_end().len());
        line
    }

    /// A deterministic byte encoding of the **entire** screen state — the visible
    /// grid, cursor, modes, the current SGR pen, the saved cursors and the
    /// inactive buffer's grid. Two screens are equal under the derived `Eq` iff
    /// they serialize to the same bytes (the encoding is self-delimiting, hence
    /// injective); that is what the replay/fork/cut/echo oracles and
    /// [`crate::render::frame_hash`] rely on, and what the replay claim pins.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.serialize_into(&mut out);
        out
    }

    /// [`serialize`](Self::serialize), streamed into `out` instead of collected, so
    /// a consumer that only hashes the encoding never materializes it.
    pub(crate) fn serialize_into<S: ByteSink>(&self, out: &mut S) {
        out.extend_from_slice(&encode_screen_header(
            self.cols,
            self.rows,
            self.row,
            self.col,
            self.cursor_visible,
            self.altscreen,
        ));
        for cell in &self.grid {
            out.extend_from_slice(&encode_cell_bytes(cell));
        }
        // Internal state that derived `Eq` distinguishes but the grid alone does
        // not — without these, two unequal screens could serialize identically
        // (e.g. a pending SGR pen with nothing yet printed). The grid length is
        // fixed by cols*rows above, so what follows is self-delimiting.
        out.extend_from_slice(&encode_pen_bytes(self.pen.fg, self.pen.bg, self.pen.attrs));
        match self.saved_cursor {
            Some(saved) => out.extend_from_slice(&encode_saved_cursor_bytes(
                saved.row,
                saved.col,
                saved.pen.fg,
                saved.pen.bg,
                saved.pen.attrs,
            )),
            None => out.push(0),
        }
        match self.alt_saved_cursor {
            Some(saved) => out.extend_from_slice(&encode_saved_cursor_bytes(
                saved.row,
                saved.col,
                saved.pen.fg,
                saved.pen.bg,
                saved.pen.attrs,
            )),
            None => out.push(0),
        }
        match &self.inactive {
            Some(cells) => {
                out.push(1);
                out.extend_from_slice(&encode_u32_le_bytes(cells.len() as u32));
                for cell in cells {
                    out.extend_from_slice(&encode_cell_bytes(cell));
                }
            }
            None => out.push(0),
        }
        out.extend_from_slice(&encode_scroll_region_bytes(
            self.scroll_top,
            self.scroll_bottom,
        ));
    }

    // --- internal mutators ------------------------------------------------

    const fn idx(&self, row: u16, col: u16) -> usize {
        (row as usize)
            .saturating_mul(self.cols as usize)
            .saturating_add(col as usize)
    }

    /// Resolve a pending wrap (`col == cols`) back onto the last column: what
    /// xterm's cursor-motion primitives and erases do to the last-column flag
    /// (`ResetWrap`) before they move or clear (CR/LF/BS/CUB/CUU/CUD/CUP,
    /// ED/EL). TAB deliberately does not.
    const fn clear_pending_wrap(&mut self) {
        let last_col = self.cols.saturating_sub(1);
        if self.col > last_col {
            self.col = last_col;
        }
    }

    fn put(&mut self, ch: char) {
        // Deferred wrap: a pending wrap (col == cols) resolves on the next glyph.
        if self.col >= self.cols {
            self.col = 0;
            self.line_feed();
        }
        let i = self.idx(self.row, self.col);
        if let Some(slot) = self.grid.get_mut(i) {
            *slot = Cell {
                ch,
                fg: self.pen.fg,
                bg: self.pen.bg,
                attrs: self.pen.attrs,
            };
        }
        self.col = self.col.saturating_add(1); // may reach `cols`, leaving a wrap pending
    }

    const fn carriage_return(&mut self) {
        self.col = 0;
    }

    fn line_feed(&mut self) {
        // LF is a cursor-down: it resolves a pending wrap (xterm CursorDown),
        // so `abcd LF Z` on four columns puts Z on the last column of the next
        // row rather than wrapping a second time.
        self.clear_pending_wrap();
        if self.row == self.scroll_bottom {
            // At the bottom of the scrolling region: scroll the region up.
            self.scroll_up();
        } else if self.row.saturating_add(1) < self.rows {
            self.row = self.row.saturating_add(1);
        }
    }

    /// Scroll the active region `[scroll_top, scroll_bottom]` up one line: the top
    /// row of the region is discarded and a blank row appears at its bottom. Rows
    /// outside the region are untouched. With the default full-screen region this
    /// is the ordinary full-screen scroll. The new row takes the current SGR
    /// background (xterm screen.c `ScrnClearLines` fills with `xtermColorPair`).
    fn scroll_up(&mut self) {
        self.shift_up(self.scroll_top, 1);
    }

    /// Rows `[from, scroll_bottom]` move up `n` (`n <= scroll_bottom - from + 1`):
    /// the top `n` of them are discarded, `n` bce-blank rows appear at the bottom
    /// margin. The scroll (`from == scroll_top`) and DL (`from == row`).
    fn shift_up(&mut self, from: u16, n: u16) {
        let w = self.cols as usize;
        let start = (from as usize).saturating_mul(w);
        let end = (self.scroll_bottom as usize)
            .saturating_add(1)
            .saturating_mul(w)
            .min(self.grid.len());
        let gap = (n as usize).saturating_mul(w);
        let src_start = start.saturating_add(gap).min(end);
        let copy_len = end.saturating_sub(src_start);
        let mut i = 0usize;
        while i < copy_len {
            if let Some(&cell) = self.grid.get(src_start.saturating_add(i)) {
                if let Some(dst) = self.grid.get_mut(start.saturating_add(i)) {
                    *dst = cell;
                }
            }
            i = i.saturating_add(1);
        }
        let blank = self.bce_blank();
        let fill_start = end.saturating_sub(gap).max(start);
        if let Some(slice) = self.grid.get_mut(fill_start..end) {
            slice.fill(blank);
        }
    }

    /// Rows `[from, scroll_bottom]` move down `n` (`n <= scroll_bottom - from +
    /// 1`): the bottom `n` of them are discarded, `n` bce-blank rows open at
    /// `from`. RI's reverse scroll (`from == scroll_top`) and IL (`from == row`).
    fn shift_down(&mut self, from: u16, n: u16) {
        let w = self.cols as usize;
        let start = (from as usize).saturating_mul(w);
        let end = (self.scroll_bottom as usize)
            .saturating_add(1)
            .saturating_mul(w)
            .min(self.grid.len());
        let gap = (n as usize).saturating_mul(w);
        let src_end = end.saturating_sub(gap).max(start);
        let dst_start = start.saturating_add(gap).min(end);
        let copy_len = src_end.saturating_sub(start);
        let mut i = copy_len;
        while i > 0 {
            i = i.saturating_sub(1);
            if let Some(&cell) = self.grid.get(start.saturating_add(i)) {
                if let Some(dst) = self.grid.get_mut(dst_start.saturating_add(i)) {
                    *dst = cell;
                }
            }
        }
        let blank = self.bce_blank();
        if let Some(slice) = self.grid.get_mut(start..dst_start) {
            slice.fill(blank);
        }
    }

    /// RI (`ESC M`, xterm `RevIndex`): at the top margin the region scrolls down
    /// a line; anywhere else it is CUU 1 (which stops at the top margin from
    /// inside the region and at row 0 from above it). Either way the wrap
    /// resets.
    fn reverse_index(&mut self) {
        self.clear_pending_wrap();
        if self.row == self.scroll_top {
            self.shift_down(self.scroll_top, 1);
        } else {
            self.cursor_up(1);
        }
    }

    /// ICH (xterm `InsertChar`): `n` bce blanks open at the cursor and the rest
    /// of the line shifts right; what passes the last column is lost.
    fn insert_chars(&mut self, n: u16) {
        self.clear_pending_wrap();
        let n = n.min(self.cols.saturating_sub(self.col)) as usize;
        let at = self.idx(self.row, self.col);
        let end = self
            .idx(self.row, 0)
            .saturating_add(self.cols as usize)
            .min(self.grid.len());
        let src_end = end.saturating_sub(n).max(at);
        let dst_start = at.saturating_add(n).min(end);
        let copy_len = src_end.saturating_sub(at);
        let mut i = copy_len;
        while i > 0 {
            i = i.saturating_sub(1);
            if let Some(&cell) = self.grid.get(at.saturating_add(i)) {
                if let Some(dst) = self.grid.get_mut(dst_start.saturating_add(i)) {
                    *dst = cell;
                }
            }
        }
        let blank = self.bce_blank();
        if let Some(slice) = self.grid.get_mut(at..dst_start) {
            slice.fill(blank);
        }
    }

    /// DCH (xterm `DeleteChar`): `n` cells go at the cursor, the rest of the
    /// line shifts left, and `n` bce blanks fill in at the right edge.
    fn delete_chars(&mut self, n: u16) {
        self.clear_pending_wrap();
        let n = n.min(self.cols.saturating_sub(self.col)) as usize;
        let at = self.idx(self.row, self.col);
        let end = self
            .idx(self.row, 0)
            .saturating_add(self.cols as usize)
            .min(self.grid.len());
        let src_start = at.saturating_add(n).min(end);
        let copy_len = end.saturating_sub(src_start);
        let mut i = 0usize;
        while i < copy_len {
            if let Some(&cell) = self.grid.get(src_start.saturating_add(i)) {
                if let Some(dst) = self.grid.get_mut(at.saturating_add(i)) {
                    *dst = cell;
                }
            }
            i = i.saturating_add(1);
        }
        let blank = self.bce_blank();
        let fill_start = end.saturating_sub(n).max(at);
        if let Some(slice) = self.grid.get_mut(fill_start..end) {
            slice.fill(blank);
        }
    }

    /// ECH (xterm `do_erase_char` -> `ClearRight(n)`): `n` cells from the
    /// cursor (the parked glyph, from a pending wrap) take the bce blank.
    fn erase_chars(&mut self, n: u16) {
        self.clear_pending_wrap();
        let n = n.min(self.cols.saturating_sub(self.col)) as usize;
        let at = self.idx(self.row, self.col);
        self.fill(at, at.saturating_add(n));
    }

    /// IL / DL (xterm `InsertLine`/`DeleteLine`): nothing outside the
    /// scrolling region; inside it, the cursor goes to column 0 (resolving any
    /// wrap) and `n` lines open (IL) or close (DL) at its row, bounded by the
    /// bottom margin.
    fn insert_lines(&mut self, n: u16, insert: bool) {
        if self.row < self.scroll_top || self.row > self.scroll_bottom {
            return;
        }
        self.col = 0;
        let n = n.min(
            self.scroll_bottom
                .saturating_sub(self.row)
                .saturating_add(1),
        );
        if insert {
            self.shift_down(self.row, n);
        } else {
            self.shift_up(self.row, n);
        }
    }

    /// `DECSTBM`: set the scrolling region to 0-based inclusive `[top, bottom]`
    /// and home the cursor. As in xterm's `CASE_DECSTBM`, a `bottom` past the
    /// screen means the last row, and a region that is empty after that clamp
    /// (`top >= bottom`) is ignored entirely: neither the margins nor the cursor
    /// change.
    const fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        let last_row = self.rows.saturating_sub(1);
        let bottom = if bottom < last_row { bottom } else { last_row };
        if top < bottom {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.row = 0;
            self.col = 0;
        }
    }

    const fn tab(&mut self) {
        // A pending wrap survives a TAB (xterm's TabToNextStop never touches the
        // last-column flag): the next glyph still wraps.
        if self.col >= self.cols {
            return;
        }
        // Advance to the next 8-column tab stop, not past the last column.
        let next = (self.col / 8).saturating_add(1).saturating_mul(8);
        let last_col = self.cols.saturating_sub(1);
        self.col = if next < last_col { next } else { last_col };
    }

    const fn backspace(&mut self) {
        // From a pending wrap the cursor is ON the last column (xterm), so BS
        // lands on the second-to-last: `abcde BS X` is `abcXe`, never `abcdX`.
        self.clear_pending_wrap();
        self.col = self.col.saturating_sub(1);
    }

    const fn move_to(&mut self, row: u16, col: u16) {
        let last_row = self.rows.saturating_sub(1);
        let last_col = self.cols.saturating_sub(1);
        self.row = if row < last_row { row } else { last_row };
        self.col = if col < last_col { col } else { last_col };
    }

    /// CUU. As in xterm's `CursorUp`, a cursor at or below the top margin stops
    /// at it; one above the region may go to row 0.
    const fn cursor_up(&mut self, n: u16) {
        self.clear_pending_wrap();
        let min = if self.row >= self.scroll_top {
            self.scroll_top
        } else {
            0
        };
        let sub = self.row.saturating_sub(n);
        self.row = if sub > min { sub } else { min };
    }

    /// CUD. As in xterm's `CursorDown`, a cursor at or above the bottom margin
    /// stops at it; one below the region may go to the last row.
    const fn cursor_down(&mut self, n: u16) {
        self.clear_pending_wrap();
        let max = if self.row <= self.scroll_bottom {
            self.scroll_bottom
        } else {
            self.rows.saturating_sub(1)
        };
        // saturating_add: a CSI param up to 65535 must not overflow u16.
        let add = self.row.saturating_add(n);
        self.row = if add < max { add } else { max };
    }

    const fn cursor_forward(&mut self, n: u16) {
        let add = self.col.saturating_add(n);
        let last_col = self.cols.saturating_sub(1);
        self.col = if add < last_col { add } else { last_col };
    }

    const fn cursor_back(&mut self, n: u16) {
        // Like BS: resolve a pending wrap onto the last column, then move.
        self.clear_pending_wrap();
        self.col = self.col.saturating_sub(n);
    }

    /// The grid index erase-to-end starts at, and the one erase-to-cursor ends
    /// before: the cursor cell, inclusive both ways. Called after the pending
    /// wrap is resolved, so a parked glyph is the cursor cell and is erased
    /// either way (xterm `ClearRight` clears from `cur_col`, `ClearLeft`
    /// through it).
    const fn erase_bounds(&self) -> (usize, usize) {
        let at = self.idx(self.row, self.col);
        (at, at.saturating_add(1))
    }

    fn erase_display(&mut self, mode: u16) {
        if mode > 2 {
            // `ED 3` erases saved lines, and there are none; any other
            // parameter is ignored. Neither resets the wrap (xterm
            // `do_erase_display` has no `ResetWrap` outside cases 0-2).
            return;
        }
        self.clear_pending_wrap();
        let (from_cursor, through_cursor) = self.erase_bounds();
        let last = self.grid.len();
        match mode {
            0 => self.fill(from_cursor, last), // cursor to end
            1 => self.fill(0, through_cursor), // start to cursor (inclusive)
            _ => self.fill(0, last),           // entire display
        }
    }

    fn erase_line(&mut self, mode: u16) {
        if mode > 2 {
            return;
        }
        self.clear_pending_wrap();
        let start = self.idx(self.row, 0);
        let end = start.saturating_add(self.cols as usize);
        let (from_cursor, through_cursor) = self.erase_bounds();
        match mode {
            0 => self.fill(from_cursor, end),      // cursor to end of line
            1 => self.fill(start, through_cursor), // start of line to cursor
            _ => self.fill(start, end),            // whole line
        }
    }

    /// An erased cell under background-colour-erase (`bce`): a space carrying the
    /// **current SGR background**, so `ED`/`EL` after `\x1b[44m` paints blue, not a
    /// hardcoded default blank. (Reverse-video bce is out of scope.)
    const fn bce_blank(&self) -> Cell {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: self.pen.bg,
            attrs: 0,
        }
    }

    fn fill(&mut self, from: usize, to: usize) {
        let blank = self.bce_blank();
        let to = to.min(self.grid.len());
        if let Some(slice) = self.grid.get_mut(from..to) {
            slice.fill(blank);
        }
    }

    /// Apply an SGR parameter list. `sub[i]` marks `params[i]` as a `:`
    /// sub-parameter of the preceding parameter (`38:5:196`) rather than a
    /// `;`-separated parameter of its own; a colon group is handled as one unit so
    /// a sub-parameter is never re-dispatched as a top-level code.
    fn sgr(&mut self, params: &[u16], sub: &[bool]) {
        if params.is_empty() {
            self.pen = Pen::reset();
            return;
        }
        let mut i = 0;
        while let Some(&param) = params.get(i) {
            let mut end = i.saturating_add(1);
            while end < params.len() && matches!(sub.get(end), Some(&true)) {
                end = end.saturating_add(1);
            }
            if end > i.saturating_add(1) {
                if let Some(group) = params.get(i..end) {
                    self.sgr_colon(group);
                }
                i = end;
                continue;
            }
            match param {
                0 => self.pen = Pen::reset(),
                1 => self.pen.attrs |= attr::BOLD,
                4 => self.pen.attrs |= attr::UNDERLINE,
                7 => self.pen.attrs |= attr::REVERSE,
                22 => self.pen.attrs &= !attr::BOLD,
                24 => self.pen.attrs &= !attr::UNDERLINE,
                27 => self.pen.attrs &= !attr::REVERSE,
                30..=37 => self.pen.fg = sgr_fg_std(param),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = sgr_bg_std(param),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = sgr_fg_bright(param),
                100..=107 => self.pen.bg = sgr_bg_bright(param),
                38 => {
                    if let Some(c) = parse_extended(params, &mut i) {
                        self.pen.fg = c;
                    }
                }
                48 => {
                    if let Some(c) = parse_extended(params, &mut i) {
                        self.pen.bg = c;
                    }
                }
                _ => {}
            }
            i = i.saturating_add(1);
        }
    }

    /// One colon group: `g[0]` is the code, `g[1..]` its sub-parameters.
    const fn sgr_colon(&mut self, g: &[u16]) {
        let &[code, ref tail @ ..] = g else {
            return;
        };
        match code {
            // `4:0` is underline off; `4:n` a styled underline (modelled as plain).
            4 => {
                if matches!(tail, &[0, ..]) {
                    self.pen.attrs &= !attr::UNDERLINE;
                } else {
                    self.pen.attrs |= attr::UNDERLINE;
                }
            }
            38 => {
                if let Some(c) = parse_colon_colour(tail) {
                    self.pen.fg = c;
                }
            }
            48 => {
                if let Some(c) = parse_colon_colour(tail) {
                    self.pen.bg = c;
                }
            }
            _ => {} // 58 (underline colour) and any other sub-parameterised code
        }
    }

    fn set_mode(&mut self, params: &[u16], set: bool) {
        for &p in params {
            match p {
                25 => self.cursor_visible = set,
                // `?1049` is DECSC + switch + clear on set, switch + DECRC on reset
                // (xterm `srm_OPT_ALTBUF_CURSOR`), each half unconditional but the
                // switch: a second set clears again, a stray reset still restores.
                // The save goes to the SAME per-buffer slot DECSC uses, so a later
                // bare `ESC 8` on the main screen restores what `?1049h` saved.
                1049 => {
                    if set {
                        self.save_cursor();
                        self.enter_alt();
                        self.clear_screen();
                    } else {
                        self.leave_alt();
                        self.restore_cursor();
                    }
                }
                // `?1047` clears the alternate buffer on the way out, if it is up
                // (xterm `srm_OPT_ALTBUF`); `?47` only switches (`srm_ALTBUF`).
                // The cursor is shared and stays.
                1047 => {
                    if set {
                        self.enter_alt();
                    } else {
                        if self.altscreen {
                            self.clear_screen();
                        }
                        self.leave_alt();
                    }
                }
                47 => {
                    if set {
                        self.enter_alt();
                    } else {
                        self.leave_alt();
                    }
                }
                _ => {}
            }
        }
    }

    /// Swap the alternate buffer onto the screen (xterm `ToAlternate`): the one
    /// a previous visit left, or a blank one on the first visit.
    fn enter_alt(&mut self) {
        if self.altscreen {
            return;
        }
        let alt = match self.inactive.take() {
            Some(grid) => grid,
            None => blank_grid(self.cols, self.rows),
        };
        self.inactive = Some(std::mem::replace(&mut self.grid, alt));
        self.altscreen = true;
    }

    /// Swap the main buffer back (xterm `FromAlternate`), keeping the alternate
    /// one for the next visit.
    fn leave_alt(&mut self) {
        if !self.altscreen {
            return;
        }
        if let Some(main) = self.inactive.take() {
            self.inactive = Some(std::mem::replace(&mut self.grid, main));
        }
        self.altscreen = false;
    }

    /// xterm's `ClearScreen` (util.c): reset the pending wrap and fill the
    /// whole active grid with the bce blank. The cursor does not move.
    fn clear_screen(&mut self) {
        self.clear_pending_wrap();
        let last = self.grid.len();
        self.fill(0, last);
    }

    /// DECSC: save the cursor and the pen into the active buffer's slot (one
    /// per buffer, as xterm) — a save inside the alternate screen never
    /// clobbers the main slot.
    const fn save_cursor(&mut self) {
        let saved = SavedCursor {
            row: self.row,
            col: self.col,
            pen: self.pen,
        };
        let slot = if self.altscreen {
            &mut self.alt_saved_cursor
        } else {
            &mut self.saved_cursor
        };
        *slot = Some(saved);
    }

    /// DECRC: restore the cursor and the pen from the active buffer's slot
    /// without consuming it (a saved pending wrap is kept). With nothing saved
    /// the cursor homes and the pen resets, per DEC STD 070 and xterm (whose
    /// `CursorRestore` then reads a zeroed `SavedCursor`).
    const fn restore_cursor(&mut self) {
        let slot = if self.altscreen {
            self.alt_saved_cursor
        } else {
            self.saved_cursor
        };
        let saved = match slot {
            Some(s) => s,
            None => SavedCursor {
                row: 0,
                col: 0,
                pen: Pen::reset(),
            },
        };
        let last_row = self.rows.saturating_sub(1);
        self.row = if saved.row < last_row {
            saved.row
        } else {
            last_row
        };
        self.col = if saved.col < self.cols {
            saved.col
        } else {
            self.cols
        };
        self.pen = saved.pen;
    }

    /// RIS, as xterm's full `ReallyReset` (charproc.c): the SGR colours reset,
    /// then `FromAlternate(xw, True)` clears the alternate buffer if it is up and
    /// leaves it, `ClearScreen` clears the main one, and the cursor is homed and
    /// saved into the main slot (`CursorSave`, `sc[0]`: home and the reset pen,
    /// which a restore of an empty slot gives too). The alternate buffer and its
    /// DECSC slot (`sc[1]`) SURVIVE: nothing in the reset frees one or touches
    /// the other, so a `?47h` after a RIS on the main screen shows what the last
    /// visit left, and a DECRC there restores what it saved.
    fn reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        let alt = if self.altscreen {
            // `grid` is the alternate buffer: `FromAlternate`'s clear, in the
            // colours the reset has already put back.
            Some(blank_grid(cols, rows))
        } else {
            self.inactive.take()
        };
        let alt_saved_cursor = self.alt_saved_cursor;
        *self = Screen::new(cols, rows);
        self.inactive = alt;
        self.alt_saved_cursor = alt_saved_cursor;
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let cols = cols.clamp(1, MAX_DIM);
        let rows = rows.clamp(1, MAX_DIM);
        self.grid = reflow(&self.grid, self.cols, self.rows, cols, rows);
        if let Some(other) = &self.inactive {
            self.inactive = Some(reflow(other, self.cols, self.rows, cols, rows));
        }
        self.cols = cols;
        self.rows = rows;
        self.row = self.row.min(rows.saturating_sub(1));
        self.col = self.col.min(cols);
        // A resize invalidates the scrolling region: reset it to the full screen.
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
    }
}

/// Encode the 10-byte screen header `(cols, rows, row, col, cursor_visible, altscreen)`.
#[must_use]
pub const fn encode_screen_header(
    cols: u16,
    rows: u16,
    row: u16,
    col: u16,
    cursor_visible: bool,
    altscreen: bool,
) -> [u8; 10] {
    [
        cols as u8,
        (cols >> 8) as u8,
        rows as u8,
        (rows >> 8) as u8,
        row as u8,
        (row >> 8) as u8,
        col as u8,
        (col >> 8) as u8,
        cursor_visible as u8,
        altscreen as u8,
    ]
}

/// Decode a 10-byte screen header into `(cols, rows, row, col, cursor_visible, altscreen)`.
#[must_use]
pub const fn decode_screen_header(raw: &[u8; 10]) -> (u16, u16, u16, u16, bool, bool) {
    let cols = (raw[0] as u16) | ((raw[1] as u16) << 8);
    let rows = (raw[2] as u16) | ((raw[3] as u16) << 8);
    let row = (raw[4] as u16) | ((raw[5] as u16) << 8);
    let col = (raw[6] as u16) | ((raw[7] as u16) << 8);
    let cursor_visible = raw[8] != 0;
    let altscreen = raw[9] != 0;
    (cols, rows, row, col, cursor_visible, altscreen)
}

/// Encode one [`Cell`] into its 9-byte canonical screen serialization.
#[must_use]
pub const fn encode_cell_bytes(cell: &Cell) -> [u8; 9] {
    let cp = cell.ch as u32;
    let (ft, fv) = match cell.fg {
        Color::Default => (0u8, 0u8),
        Color::Indexed(n) => (1u8, n),
    };
    let (bt, bv) = match cell.bg {
        Color::Default => (0u8, 0u8),
        Color::Indexed(n) => (1u8, n),
    };
    [
        cp as u8,
        (cp >> 8) as u8,
        (cp >> 16) as u8,
        (cp >> 24) as u8,
        ft,
        fv,
        bt,
        bv,
        cell.attrs,
    ]
}

/// Decode a 9-byte canonical cell record into `(codepoint, fg, bg, attrs)`.
#[must_use]
pub const fn decode_cell_bytes(raw: &[u8; 9]) -> (u32, Color, Color, u8) {
    let cp = (raw[0] as u32)
        | ((raw[1] as u32) << 8)
        | ((raw[2] as u32) << 16)
        | ((raw[3] as u32) << 24);
    let fg = if raw[4] == 0 {
        Color::Default
    } else {
        Color::Indexed(raw[5])
    };
    let bg = if raw[6] == 0 {
        Color::Default
    } else {
        Color::Indexed(raw[7])
    };
    (cp, fg, bg, raw[8])
}

/// Encode the 5-byte pen state `(fg, bg, attrs)`.
#[must_use]
pub const fn encode_pen_bytes(fg: Color, bg: Color, attrs: u8) -> [u8; 5] {
    let (ft, fv) = match fg {
        Color::Default => (0u8, 0u8),
        Color::Indexed(n) => (1u8, n),
    };
    let (bt, bv) = match bg {
        Color::Default => (0u8, 0u8),
        Color::Indexed(n) => (1u8, n),
    };
    [ft, fv, bt, bv, attrs]
}

/// Decode a 5-byte pen state into `(fg, bg, attrs)`.
#[must_use]
pub const fn decode_pen_bytes(raw: &[u8; 5]) -> (Color, Color, u8) {
    let fg = if raw[0] == 0 {
        Color::Default
    } else {
        Color::Indexed(raw[1])
    };
    let bg = if raw[2] == 0 {
        Color::Default
    } else {
        Color::Indexed(raw[3])
    };
    (fg, bg, raw[4])
}

/// Encode an active saved-cursor slot into its 10-byte serialization (`1 || row || col || pen`).
#[must_use]
pub const fn encode_saved_cursor_bytes(
    row: u16,
    col: u16,
    fg: Color,
    bg: Color,
    attrs: u8,
) -> [u8; 10] {
    let (ft, fv) = match fg {
        Color::Default => (0u8, 0u8),
        Color::Indexed(n) => (1u8, n),
    };
    let (bt, bv) = match bg {
        Color::Default => (0u8, 0u8),
        Color::Indexed(n) => (1u8, n),
    };
    [
        1,
        row as u8,
        (row >> 8) as u8,
        col as u8,
        (col >> 8) as u8,
        ft,
        fv,
        bt,
        bv,
        attrs,
    ]
}

/// Decode a 10-byte saved-cursor slot into `(present, row, col, fg, bg, attrs)`.
#[must_use]
pub const fn decode_saved_cursor_bytes(raw: &[u8; 10]) -> (bool, u16, u16, Color, Color, u8) {
    let present = raw[0] != 0;
    let row = (raw[1] as u16) | ((raw[2] as u16) << 8);
    let col = (raw[3] as u16) | ((raw[4] as u16) << 8);
    let fg = if raw[5] == 0 {
        Color::Default
    } else {
        Color::Indexed(raw[6])
    };
    let bg = if raw[7] == 0 {
        Color::Default
    } else {
        Color::Indexed(raw[8])
    };
    (present, row, col, fg, bg, raw[9])
}

/// Encode the 4-byte scroll region `(scroll_top, scroll_bottom)`.
#[must_use]
pub const fn encode_scroll_region_bytes(scroll_top: u16, scroll_bottom: u16) -> [u8; 4] {
    [
        scroll_top as u8,
        (scroll_top >> 8) as u8,
        scroll_bottom as u8,
        (scroll_bottom >> 8) as u8,
    ]
}

/// Decode a 4-byte scroll region into `(scroll_top, scroll_bottom)`.
#[must_use]
pub const fn decode_scroll_region_bytes(raw: &[u8; 4]) -> (u16, u16) {
    let top = (raw[0] as u16) | ((raw[1] as u16) << 8);
    let bottom = (raw[2] as u16) | ((raw[3] as u16) << 8);
    (top, bottom)
}

/// Encode a `u32` length field in little-endian bytes.
#[must_use]
pub const fn encode_u32_le_bytes(v: u32) -> [u8; 4] {
    [v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
}

/// Decode a `u32` length field from little-endian bytes.
#[must_use]
pub const fn decode_u32_le_bytes(raw: &[u8; 4]) -> u32 {
    (raw[0] as u32) | ((raw[1] as u32) << 8) | ((raw[2] as u32) << 16) | ((raw[3] as u32) << 24)
}

/// Clip/extend a grid into new dimensions, copying the overlapping top-left
/// region. Does not reflow wrapped lines (a later rung).
fn reflow(old: &[Cell], old_cols: u16, old_rows: u16, new_cols: u16, new_rows: u16) -> Vec<Cell> {
    let new_cols = new_cols.clamp(1, MAX_DIM);
    let new_rows = new_rows.clamp(1, MAX_DIM);
    let mut g = blank_grid(new_cols, new_rows);
    let rows = old_rows.min(new_rows);
    let cols = old_cols.min(new_cols);
    for r in 0..rows {
        for c in 0..cols {
            let dst_idx = (r as usize)
                .saturating_mul(new_cols as usize)
                .saturating_add(c as usize);
            let src_idx = (r as usize)
                .saturating_mul(old_cols as usize)
                .saturating_add(c as usize);
            if let (Some(dst), Some(&src)) = (g.get_mut(dst_idx), old.get(src_idx)) {
                *dst = src;
            }
        }
    }
    g
}

/// Parse the `;`-form extended colour selector after a `38`/`48` at `params[*i]`,
/// advancing `i` past everything it consumed. `;5;n` selects a palette colour.
/// Truecolour `;2;r;g;b` (and CMY `;3;c;m;y`, CMYK `;4;c;m;y;k`) are out of scope
/// and select nothing, but are consumed *with* their sub-parameters — otherwise
/// `r`, `g`, `b` would be re-dispatched as top-level codes (`0` resetting the
/// pen, `1` setting bold, `30..37` recolouring). Any other selector consumes
/// itself; a truncated form consumes what is there.
fn parse_extended(params: &[u16], i: &mut usize) -> Option<Color> {
    let last = params.len().saturating_sub(1);
    let i1 = i.saturating_add(1);
    let i2 = i.saturating_add(2);
    let (after_selector, colour) = match params.get(i1) {
        None => return None,
        Some(5) => (
            1usize,
            params.get(i2).map(|n| Color::Indexed((*n).min(255) as u8)),
        ),
        Some(2) | Some(3) => (3, None),
        Some(4) => (4, None),
        Some(_) => (0, None),
    };
    *i = i1.saturating_add(after_selector).min(last);
    colour
}

const fn sgr_fg_std(param: u16) -> Color {
    if param >= 30 && param <= 37 {
        Color::Indexed((param - 30) as u8)
    } else {
        Color::Default
    }
}

const fn sgr_bg_std(param: u16) -> Color {
    if param >= 40 && param <= 47 {
        Color::Indexed((param - 40) as u8)
    } else {
        Color::Default
    }
}

const fn sgr_fg_bright(param: u16) -> Color {
    if param >= 90 && param <= 97 {
        // 90..=97 -> bright palette 8..=15 (`param - 82`).
        Color::Indexed((param - 82) as u8)
    } else {
        Color::Default
    }
}

const fn sgr_bg_bright(param: u16) -> Color {
    if param >= 100 && param <= 107 {
        // 100..=107 -> bright palette 8..=15 (`param - 92`).
        Color::Indexed((param - 92) as u8)
    } else {
        Color::Default
    }
}

/// The `:`-form colour sub-parameters after `38`/`48`: `5:n` selects a palette
/// colour; `2:r:g:b` / `2:cs:r:g:b` (truecolour) select nothing.
const fn parse_colon_colour(sub: &[u16]) -> Option<Color> {
    match sub {
        &[5, n, ..] => {
            let clamped = if n < 255 { n } else { 255 };
            Some(Color::Indexed(clamped as u8))
        }
        _ => None,
    }
}

const fn utf8_acc_step(acc: u32, b: u8) -> u32 {
    (acc << 6) | (b & 0x3f) as u32
}

#[allow(clippy::implicit_saturating_sub)]
const fn utf8_needed_dec(needed: u8) -> u8 {
    if needed > 1 {
        needed - 1
    } else {
        0
    }
}

const fn csi_acc_digit(cur: u16, c: u8) -> u16 {
    if c >= b'0' && c <= b'9' {
        cur.saturating_mul(10).saturating_add((c - b'0') as u16)
    } else {
        cur
    }
}

const fn csi_count0(nums: &[u16]) -> u16 {
    match nums {
        &[0, ..] | &[] => 1,
        &[n, ..] => n,
    }
}

const fn csi_coord0(nums: &[u16]) -> u16 {
    match nums {
        &[0, ..] | &[] => 0,
        &[n, ..] => n.saturating_sub(1),
    }
}

const fn csi_coord1(nums: &[u16]) -> u16 {
    if let &[_, n, ..] = nums {
        n.saturating_sub(1)
    } else {
        0
    }
}

#[allow(clippy::match_ref_pats)]
const fn csi_mode0(nums: &[u16]) -> u16 {
    match nums {
        &[m, ..] => m,
        &[] => 0,
    }
}

const fn csi_bottom0(nums: &[u16], rows: u16) -> u16 {
    if let &[_, n, ..] = nums {
        if n == 0 {
            rows.saturating_sub(1)
        } else {
            n.saturating_sub(1)
        }
    } else {
        rows.saturating_sub(1)
    }
}

// --- the byte-level VT parser --------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    /// `ESC` plus an intermediate byte (`ESC ( B`): the final byte is still to
    /// come, and the whole sequence is consumed without effect.
    EscIntermediate,
    Csi,
    /// Inside an OSC/DCS/APC/PM/SOS string: bytes are swallowed — never painted
    /// — until the terminator (`ESC \`; BEL too when `bel_ends`, i.e. OSC).
    Str {
        bel_ends: bool,
    },
    /// Mid UTF-8 multibyte sequence: bytes still needed, the accumulated
    /// codepoint, and the range `[lo, hi]` the next byte must fall in to keep
    /// the sequence well-formed (Unicode Table 3-7: narrower than `80..=BF`
    /// only for the byte after `E0`, `ED`, `F0` and `F4`).
    Utf8 {
        needed: u8,
        acc: u32,
        lo: u8,
        hi: u8,
    },
}

/// The most CSI parameter bytes the parser will accumulate. Every sequence this
/// fold models is far shorter (the longest, a truecolour SGR with both a
/// foreground and a background, is ~30 bytes); past the cap the CSI is ignored
/// outright, the way xterm treats one that overruns its own parameter limit.
/// This is what bounds the parser's state: `params` is the only buffer that
/// survives a record boundary, and an unterminated `ESC [` followed by an endless
/// run of `1;` would otherwise grow it for the life of the session.
const CSI_PARAM_MAX: usize = 128;

/// A persistent byte→action state machine. Persisting it across [`Parser::feed`]
/// calls is what lets an escape sequence (or a multibyte glyph) span two `Out`
/// records without corrupting the fold. Its state is bounded: a small enum, two
/// bytes, and at most [`CSI_PARAM_MAX`] parameter bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Parser {
    state: State,
    /// CSI parameter bytes (`0-9`, `;`, `:`) collected so far.
    params: String,
    /// The CSI private-marker byte (`?`, `>`, `=`, `<`), or 0 for none.
    marker: u8,
    /// The CSI is malformed or outside the subset (an intermediate byte, or a
    /// marker after parameters): its final byte dispatches nothing.
    csi_ignore: bool,
    /// When `true`, each top-level dispatch records its [`ScreenOp`] into `ops`.
    /// Off for the normal fold; on only inside [`emit_ops`].
    recording: bool,
    ops: Vec<ScreenOp>,
}

impl Parser {
    fn new() -> Parser {
        Parser {
            state: State::Ground,
            params: String::new(),
            marker: 0,
            csi_ignore: false,
            recording: false,
            ops: Vec::new(),
        }
    }

    /// Record one top-level op (no-op unless recording). Internal mutator
    /// cascades (deferred-wrap line-feed, line-feed scroll) are deliberately NOT
    /// recorded — they reproduce deterministically when the op is re-applied.
    fn rec(&mut self, op: ScreenOp) {
        if self.recording {
            self.ops.push(op);
        }
    }

    fn feed(&mut self, screen: &mut Screen, bytes: &[u8]) {
        for &b in bytes {
            match self.state {
                State::Ground => self.ground(screen, b),
                State::Esc => self.escape(screen, b),
                State::EscIntermediate => self.esc_intermediate(screen, b),
                State::Csi => self.csi(screen, b),
                State::Str { bel_ends } => self.string(b, bel_ends),
                State::Utf8 {
                    needed,
                    acc,
                    lo,
                    hi,
                } => self.utf8_cont(screen, b, needed, acc, (lo, hi)),
            }
        }
    }

    fn ground(&mut self, screen: &mut Screen, b: u8) {
        match b {
            0x1b => self.state = State::Esc,
            // VT and FF are line feeds too (xterm `CASE_VMOT`: "form feed, line
            // feed, vertical tab" all take `xtermIndex`).
            b'\n' | 0x0b | 0x0c => {
                screen.line_feed();
                self.rec(ScreenOp::LineFeed);
            }
            b'\r' => {
                screen.carriage_return();
                self.rec(ScreenOp::CarriageReturn);
            }
            b'\t' => {
                screen.tab();
                self.rec(ScreenOp::Tab);
            }
            0x08 => {
                screen.backspace();
                self.rec(ScreenOp::Backspace);
            }
            0x00..=0x1f | 0x7f => {} // other C0 controls and DEL: ignored
            0x20..=0x7e => {
                screen.put(b as char);
                self.rec(ScreenOp::Print(b as char));
            }
            _ => self.begin_utf8(screen, b),
        }
    }

    /// Emit U+FFFD for one maximal subpart of ill-formed UTF-8.
    fn replacement(&mut self, screen: &mut Screen) {
        screen.put('\u{fffd}');
        self.rec(ScreenOp::Print('\u{fffd}'));
    }

    /// A byte at or above 0x80 in the ground state. Malformed input is replaced
    /// one U+FFFD per MAXIMAL SUBPART (Unicode 3.9, "U+FFFD Substitution of
    /// Maximal Subparts"; the WHATWG decoder and aterm do the same): a byte
    /// that cannot start a well-formed sequence (`80..=C1`, `F5..=FF`) is one
    /// subpart on its own, and so the overlong `C0 80` is two replacements and
    /// the surrogate `ED A0 80` three, never one for the whole run. The
    /// replacements are cells, so the count decides where every later glyph
    /// lands.
    fn begin_utf8(&mut self, screen: &mut Screen, b: u8) {
        let (needed, acc, lo, hi) = match b {
            0xc2..=0xdf => (1u8, (b & 0x1f) as u32, 0x80, 0xbf),
            0xe0 => (2u8, 0, 0xa0, 0xbf),
            0xed => (2u8, 0x0d, 0x80, 0x9f),
            0xe1..=0xef => (2u8, (b & 0x0f) as u32, 0x80, 0xbf),
            0xf0 => (3u8, 0, 0x90, 0xbf),
            0xf4 => (3u8, 0x04, 0x80, 0x8f),
            0xf1..=0xf3 => (3u8, (b & 0x07) as u32, 0x80, 0xbf),
            _ => return self.replacement(screen),
        };
        self.state = State::Utf8 {
            needed,
            acc,
            lo,
            hi,
        };
    }

    fn utf8_cont(&mut self, screen: &mut Screen, b: u8, needed: u8, acc: u32, range: (u8, u8)) {
        if !(range.0..=range.1).contains(&b) {
            // The subpart so far ends here: replace it, then reprocess `b`
            // fresh (it may start a sequence, or be a control or ASCII).
            self.state = State::Ground;
            self.replacement(screen);
            self.ground(screen, b);
            return;
        }
        let acc = utf8_acc_step(acc, b);
        if needed > 1 {
            self.state = State::Utf8 {
                needed: utf8_needed_dec(needed),
                acc,
                lo: 0x80,
                hi: 0xbf,
            };
            return;
        }
        self.state = State::Ground;
        if (0x80..=0x9f).contains(&acc) {
            // A validly encoded C1 control: not modelled, ignored.
            return;
        }
        // Table 3-7's ranges exclude overlongs and surrogates, so every
        // completed sequence is a scalar value.
        let ch = char::from_u32(acc).unwrap_or('\u{fffd}');
        screen.put(ch);
        self.rec(ScreenOp::Print(ch));
    }

    fn escape(&mut self, screen: &mut Screen, b: u8) {
        match b {
            b'[' => {
                self.params.clear();
                self.marker = 0;
                self.csi_ignore = false;
                self.state = State::Csi;
            }
            b']' => self.state = State::Str { bel_ends: true }, // OSC
            b'P' | b'X' | b'^' | b'_' => self.state = State::Str { bel_ends: false }, // DCS/SOS/PM/APC
            // IND is a line feed (xterm `xtermIndex`), NEL a line feed and a
            // carriage return, RI the reverse index.
            b'D' => {
                screen.line_feed();
                self.rec(ScreenOp::LineFeed);
                self.state = State::Ground;
            }
            b'E' => {
                screen.line_feed();
                screen.carriage_return();
                self.rec(ScreenOp::LineFeed);
                self.rec(ScreenOp::CarriageReturn);
                self.state = State::Ground;
            }
            b'M' => {
                screen.reverse_index();
                self.rec(ScreenOp::ReverseIndex);
                self.state = State::Ground;
            }
            b'7' => {
                screen.save_cursor();
                self.rec(ScreenOp::SaveCursor);
                self.state = State::Ground;
            }
            b'8' => {
                screen.restore_cursor();
                self.rec(ScreenOp::RestoreCursor);
                self.state = State::Ground;
            }
            b'c' => {
                screen.reset();
                self.rec(ScreenOp::Reset);
                self.state = State::Ground;
            }
            0x20..=0x2f => self.state = State::EscIntermediate, // ESC ( B etc.
            0x1b => {} // ESC ESC: stay, treat the next byte as the escape
            0x18 | 0x1a => self.state = State::Ground, // CAN/SUB abort
            0x00..=0x1f => self.ground(screen, b), // a C0 control executes; the escape continues
            0x7f => {}
            0x30..=0x7e => self.state = State::Ground, // other two-byte escapes: consumed
            _ => {
                // An 8-bit byte cannot continue an escape: abort and reprocess it.
                self.state = State::Ground;
                self.ground(screen, b);
            }
        }
    }

    fn esc_intermediate(&mut self, screen: &mut Screen, b: u8) {
        match b {
            0x1b => self.state = State::Esc,
            0x18 | 0x1a => self.state = State::Ground,
            0x00..=0x1f => self.ground(screen, b),
            0x20..=0x2f | 0x7f => {}
            0x30..=0x7e => self.state = State::Ground, // the final byte: consumed
            _ => {
                self.state = State::Ground;
                self.ground(screen, b);
            }
        }
    }

    /// An OSC/DCS/APC/PM/SOS string body: swallowed byte by byte. `ESC` ends it
    /// (as `ESC \`, or as the start of a new escape), BEL ends an OSC, CAN/SUB
    /// abort; nothing is ever painted.
    fn string(&mut self, b: u8, bel_ends: bool) {
        match b {
            0x1b => self.state = State::Esc,
            0x07 if bel_ends => self.state = State::Ground,
            0x18 | 0x1a => self.state = State::Ground,
            _ => {}
        }
    }

    fn csi(&mut self, screen: &mut Screen, b: u8) {
        match b {
            // ESC anywhere begins a new escape (ECMA-48): an unterminated CSI is
            // abandoned, and the new sequence is parsed rather than painted.
            0x1b => self.state = State::Esc,
            0x18 | 0x1a => self.state = State::Ground, // CAN/SUB abort
            0x00..=0x1f => self.ground(screen, b),     // a C0 control executes inside a CSI
            0x20..=0x2f => self.csi_ignore = true,     // intermediate: outside the subset
            0x3c..=0x3f if self.marker == 0 && self.params.is_empty() && !self.csi_ignore => {
                self.marker = b; // leading private marker: `?`, `>`, `=`, `<`
            }
            0x3c..=0x3f => self.csi_ignore = true, // a marker after parameters: malformed
            0x30..=0x3b => {
                if !self.csi_ignore {
                    if self.params.len() >= CSI_PARAM_MAX {
                        // Longer than any sequence this fold models: ignore the whole
                        // CSI and stop accumulating, so the parser's state stays
                        // bounded no matter what the target emits.
                        self.csi_ignore = true;
                        self.params.clear();
                    } else {
                        self.params.push(b as char); // digits, `;`, `:`
                    }
                }
            }
            0x40..=0x7e => {
                if !self.csi_ignore {
                    self.dispatch_csi(screen, b);
                }
                self.state = State::Ground;
            }
            0x7f => {}
            _ => {
                // An 8-bit byte cannot continue a CSI: abort and reprocess it.
                self.state = State::Ground;
                self.ground(screen, b);
            }
        }
    }

    fn dispatch_csi(&mut self, screen: &mut Screen, final_byte: u8) {
        // Which (marker, final) pairs the fold models: plain sequences (SM/RM
        // `h`/`l` excepted), DEC private modes, and DECSED/DECSEL (`? J`/`? K`,
        // which erase like ED/EL with no protected cells). `>`/`=`/`<`-marked
        // sequences (XTMODKEYS `> Ps m`, tertiary DA, …) are consumed whole.
        match (self.marker, final_byte) {
            (0, b'h' | b'l') => return,
            (b'?', b'h' | b'l' | b'J' | b'K') | (0, _) => {}
            _ => return,
        }
        // `1;2:3;4` → values plus a per-value "is a `:` sub-parameter" flag. On
        // the stack, not the heap (this runs for every CSI): `params` holds at
        // most CSI_PARAM_MAX bytes, hence at most CSI_PARAM_MAX + 1 values.
        let mut vals = [0u16; CSI_PARAM_MAX + 1];
        let mut sub = [false; CSI_PARAM_MAX + 1];
        let mut n = 0;
        if !self.params.is_empty() {
            let mut cur: u16 = 0;
            let mut cur_sub = false;
            for c in self.params.bytes() {
                match c {
                    b'0'..=b'9' => cur = csi_acc_digit(cur, c),
                    _ => {
                        if let (Some(v_slot), Some(s_slot)) = (vals.get_mut(n), sub.get_mut(n)) {
                            *v_slot = cur;
                            *s_slot = cur_sub;
                            n = n.saturating_add(1);
                        }
                        cur = 0;
                        cur_sub = c == b':';
                    }
                }
            }
            if let (Some(v_slot), Some(s_slot)) = (vals.get_mut(n), sub.get_mut(n)) {
                *v_slot = cur;
                *s_slot = cur_sub;
                n = n.saturating_add(1);
            }
        }
        let vals = vals.get(..n).unwrap_or(&[]);
        let sub = sub.get(..n).unwrap_or(&[]);
        // Outside SGR a sub-parameter carries nothing the fold models.
        let mut nums = [0u16; CSI_PARAM_MAX + 1];
        let mut m = 0;
        for (&v, &s) in vals.iter().zip(sub) {
            if !s {
                if let Some(num_slot) = nums.get_mut(m) {
                    *num_slot = v;
                    m = m.saturating_add(1);
                }
            }
        }
        let nums = nums.get(..m).unwrap_or(&[]);

        match final_byte {
            b'A' => {
                let n = csi_count0(nums);
                screen.cursor_up(n);
                self.rec(ScreenOp::CursorUp(n));
            }
            b'B' => {
                let n = csi_count0(nums);
                screen.cursor_down(n);
                self.rec(ScreenOp::CursorDown(n));
            }
            b'C' => {
                let n = csi_count0(nums);
                screen.cursor_forward(n);
                self.rec(ScreenOp::CursorForward(n));
            }
            b'D' => {
                let n = csi_count0(nums);
                screen.cursor_back(n);
                self.rec(ScreenOp::CursorBack(n));
            }
            b'E' => {
                let n = csi_count0(nums);
                screen.cursor_down(n);
                screen.carriage_return();
                self.rec(ScreenOp::CursorDown(n));
                self.rec(ScreenOp::CarriageReturn);
            }
            b'F' => {
                let n = csi_count0(nums);
                screen.cursor_up(n);
                screen.carriage_return();
                self.rec(ScreenOp::CursorUp(n));
                self.rec(ScreenOp::CarriageReturn);
            }
            b'G' => {
                let (r, _) = screen.cursor();
                let c = csi_coord0(nums);
                screen.move_to(r, c);
                self.rec(ScreenOp::MoveTo(r, c));
            }
            b'd' => {
                let (_, c) = screen.cursor();
                let r = csi_coord0(nums);
                screen.move_to(r, c);
                self.rec(ScreenOp::MoveTo(r, c));
            }
            b'H' | b'f' => {
                let r = csi_coord0(nums);
                let c = csi_coord1(nums);
                screen.move_to(r, c);
                self.rec(ScreenOp::MoveTo(r, c));
            }
            b'J' => {
                let m = csi_mode0(nums);
                screen.erase_display(m);
                self.rec(ScreenOp::EraseDisplay(m));
            }
            b'K' => {
                let m = csi_mode0(nums);
                screen.erase_line(m);
                self.rec(ScreenOp::EraseLine(m));
            }
            b'm' => {
                screen.sgr(vals, sub);
                // Record the RESOLVED pen (after sgr ran), not the raw params —
                // this is what kills the two-party terminfo-disagreement class.
                self.rec(ScreenOp::SetSgr {
                    fg: screen.pen.fg,
                    bg: screen.pen.bg,
                    attrs: screen.pen.attrs,
                });
            }
            b'h' | b'l' => {
                let set = final_byte == b'h';
                screen.set_mode(nums, set);
                if self.recording {
                    self.rec(ScreenOp::SetMode {
                        params: nums.to_vec(),
                        set,
                    });
                }
            }
            b'r' => {
                // DECSTBM: set the scrolling region [top; bottom] (1-based, inclusive).
                // Absent/0 top -> 1; absent/0 bottom -> last row. Converted to 0-based.
                let (_, rows) = screen.dims();
                let top0 = csi_coord0(nums);
                let bottom0 = csi_bottom0(nums, rows);
                screen.set_scroll_region(top0, bottom0);
                self.rec(ScreenOp::SetScrollRegion {
                    top: top0,
                    bottom: bottom0,
                });
            }
            b'@' => {
                let n = csi_count0(nums);
                screen.insert_chars(n);
                self.rec(ScreenOp::InsertChars(n));
            }
            b'P' => {
                let n = csi_count0(nums);
                screen.delete_chars(n);
                self.rec(ScreenOp::DeleteChars(n));
            }
            b'X' => {
                let n = csi_count0(nums);
                screen.erase_chars(n);
                self.rec(ScreenOp::EraseChars(n));
            }
            b'L' => {
                let n = csi_count0(nums);
                screen.insert_lines(n, true);
                self.rec(ScreenOp::InsertLines(n));
            }
            b'M' => {
                let n = csi_count0(nums);
                screen.insert_lines(n, false);
                self.rec(ScreenOp::DeleteLines(n));
            }
            b's' => {
                screen.save_cursor();
                self.rec(ScreenOp::SaveCursor);
            }
            b'u' => {
                screen.restore_cursor();
                self.rec(ScreenOp::RestoreCursor);
            }
            _ => {}
        }
    }
}

/// Project records into the normalized typed screen-op stream via the **single
/// canonical parser** (the same parse that produces the fold). One parser emits
/// both, so two viewers cannot disagree.
pub fn emit_ops(cols: u16, rows: u16, records: &[Record]) -> Vec<ScreenOp> {
    let mut folder = Folder::new(cols, rows);
    folder.parser.recording = true;
    for rec in records {
        folder.apply(rec);
    }
    folder.parser.ops
}

/// Re-apply a screen-op stream to a fresh screen, re-driving the **same** private
/// mutators from typed ops — an independent dispatch path that bypasses only the
/// VT state machine. Reproduces the fold's screen byte-for-byte.
pub fn apply_ops(cols: u16, rows: u16, ops: &[ScreenOp]) -> Screen {
    let mut screen = Screen::new(cols, rows);
    for op in ops {
        match op {
            ScreenOp::Print(ch) => screen.put(*ch),
            ScreenOp::CarriageReturn => screen.carriage_return(),
            ScreenOp::LineFeed => screen.line_feed(),
            ScreenOp::Tab => screen.tab(),
            ScreenOp::Backspace => screen.backspace(),
            ScreenOp::CursorUp(n) => screen.cursor_up(*n),
            ScreenOp::CursorDown(n) => screen.cursor_down(*n),
            ScreenOp::CursorForward(n) => screen.cursor_forward(*n),
            ScreenOp::CursorBack(n) => screen.cursor_back(*n),
            ScreenOp::MoveTo(r, c) => screen.move_to(*r, *c),
            ScreenOp::EraseDisplay(m) => screen.erase_display(*m),
            ScreenOp::EraseLine(m) => screen.erase_line(*m),
            ScreenOp::SetSgr { fg, bg, attrs } => {
                screen.pen = Pen {
                    fg: *fg,
                    bg: *bg,
                    attrs: *attrs,
                }
            }
            ScreenOp::SetMode { params, set } => screen.set_mode(params, *set),
            ScreenOp::ReverseIndex => screen.reverse_index(),
            ScreenOp::InsertChars(n) => screen.insert_chars(*n),
            ScreenOp::DeleteChars(n) => screen.delete_chars(*n),
            ScreenOp::EraseChars(n) => screen.erase_chars(*n),
            ScreenOp::InsertLines(n) => screen.insert_lines(*n, true),
            ScreenOp::DeleteLines(n) => screen.insert_lines(*n, false),
            ScreenOp::SaveCursor => screen.save_cursor(),
            ScreenOp::RestoreCursor => screen.restore_cursor(),
            ScreenOp::SetScrollRegion { top, bottom } => screen.set_scroll_region(*top, *bottom),
            ScreenOp::Reset => screen.reset(),
            ScreenOp::Resize(c, r) => screen.resize(*c, *r),
        }
    }
    screen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(s: &[u8]) -> Vec<Record> {
        vec![Record::Out(s.to_vec())]
    }

    #[test]
    fn printable_text_lands_in_the_grid() {
        let s = fold(10, 2, &out(b"hi"));
        assert_eq!(s.line_text(0), "hi");
        assert_eq!(s.cursor(), (0, 2));
    }

    #[test]
    fn deferred_wrap_moves_to_next_row_only_on_the_next_glyph() {
        // Three columns, write "abc" then "d": the wrap is deferred until 'd'.
        let s = fold(3, 2, &out(b"abcd"));
        assert_eq!(s.line_text(0), "abc");
        assert_eq!(s.line_text(1), "d");
    }

    #[test]
    fn sgr_color_and_attr_apply_to_subsequent_cells() {
        let s = fold(10, 1, &out(b"\x1b[31;1mX"));
        let cell = s.cell(0, 0).unwrap();
        assert_eq!(cell.fg, Color::Indexed(1));
        assert_ne!(cell.attrs & attr::BOLD, 0);
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // Every single byte value, including stray UTF-8 continuations and
        // truncated escapes, must fold without panicking.
        let all: Vec<u8> = (0..=255u8).collect();
        let s = fold(8, 4, &out(&all));
        assert_eq!(s.dims(), (8, 4));
    }

    #[test]
    fn large_cursor_movement_params_do_not_overflow() {
        // A near-u16::MAX CSI param from a non-zero cursor must clamp to the last
        // cell, not overflow u16. Move to (3,3), then CUD 65535 and CUF 65535.
        let s = fold(10, 8, &out(b"\x1b[4;4H\x1b[65535B\x1b[65535C"));
        assert_eq!(s.cursor(), (7, 9)); // clamped to (rows-1, cols-1)
                                        // Tab from the far edge of a max-width screen must not overflow either.
        let wide = fold(MAX_DIM, 1, &out(b"\x1b[1;1000H\t"));
        assert_eq!(wide.cursor().1, MAX_DIM - 1);
        // An over-long parameter saturates rather than collapsing to 0.
        let big = fold(10, 8, &out(b"\x1b[99999999B"));
        assert_eq!(big.cursor(), (7, 0));
    }

    #[test]
    fn a_csi_split_across_records_still_dispatches() {
        let recs = vec![
            Record::Out(b"\x1b[3".to_vec()),
            Record::Out(b";2Hx".to_vec()),
        ];
        let s = fold(10, 4, &recs);
        assert_eq!(s.line_text(2), " x");
    }

    #[test]
    fn a_string_sequence_split_across_records_is_still_swallowed() {
        let recs = vec![
            Record::Out(b"a\x1b]0;half a ".to_vec()),
            Record::Out(b"title\x07b".to_vec()),
        ];
        let s = fold(20, 2, &recs);
        assert_eq!(s.line_text(0), "ab");
    }

    #[test]
    fn an_unterminated_csi_cannot_grow_the_parser_without_bound() {
        // A hostile (or wedged) target emitting `ESC [` and then an endless run of
        // `1;` never terminates the CSI, so its parameter bytes are carried across
        // every record. The buffer must stay capped rather than growing with the
        // stream — the fold is the pump's one piece of per-session state.
        let mut folder = Folder::new(20, 4);
        folder.apply(&Record::Out(b"\x1b[".to_vec()));
        for _ in 0..1000 {
            folder.apply(&Record::Out(b"1;".repeat(64)));
            assert!(
                folder.parser.params.len() <= CSI_PARAM_MAX,
                "params grew to {}",
                folder.parser.params.len()
            );
        }
        // And the over-long CSI is ignored, not dispatched: the next printable
        // byte after its final byte paints normally.
        folder.apply(&Record::Out(b"mX".to_vec()));
        assert_eq!(folder.screen().line_text(0), "X");

        // A sequence of legal length is unaffected — the cap is far above any the
        // fold models (truecolour SGR with a foreground and a background).
        let s = fold(20, 4, &out(b"\x1b[38;2;10;20;30;48;2;40;50;60mA\x1b[3;2Hb"));
        assert_eq!(s.line_text(0), "A");
        assert_eq!(s.line_text(2), " b");
    }

    #[test]
    fn a_csi_at_the_parameter_cap_dispatches() {
        // CSI_PARAM_MAX separators is the most values a dispatch can see
        // (CSI_PARAM_MAX + 1, all zero): an SGR reset, then a CUP home.
        let mut bytes = b"\x1b[31mA\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b';', CSI_PARAM_MAX));
        bytes.extend_from_slice(b"mB\x1b[");
        bytes.extend(std::iter::repeat_n(b':', CSI_PARAM_MAX));
        bytes.extend_from_slice(b"H");
        let s = fold(10, 2, &out(&bytes));
        assert_eq!(s.cell(0, 0).unwrap().fg, Color::Indexed(1));
        assert_eq!(s.cell(0, 1).unwrap().fg, Color::Default);
        assert_eq!(s.cursor(), (0, 0));
    }

    #[test]
    fn restore_with_nothing_saved_homes_the_cursor() {
        // DEC STD 070 / xterm: DECRC with no prior DECSC moves to the home position.
        let s = fold(10, 4, &out(b"\x1b[3;3H\x1b8"));
        assert_eq!(s.cursor(), (0, 0));
    }
}
