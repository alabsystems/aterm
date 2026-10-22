// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Screen and application introspection serializers, plus message write adapters.
//! Screen queries read the live [`Terminal`] grid/renderer and serialize it to
//! text/JSON replies. The `notice`, `appnotice` and `devtools` adapters instead
//! send explicit requests to the main-thread owner. Shared JSON/encode helpers
//! (`json_*`, `pct_encode`, `visible_char`, `cursor_style_name`) live in
//! `control.rs` and are imported via `super::`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use aterm_control::wire::{VisibleTrim, push_visible};
use aterm_core::grid::extra::ImageData;
use aterm_core::grid::{Cell, CellFlags};
use aterm_core::search::{
    DEFAULT_MAX_CACHED_LINES, SearchDirection as EngineSearchDirection, SearchMatch,
    SearchOptionsError, SearchResults,
};
use aterm_core::selection::SelectionType;
use aterm_core::terminal::{
    AbsRowAt, AbsRowText, BuiltSearch, RenderCell, SearchKey, SearchLayout, SearchRequest,
    SearchVerdict, Terminal, UnderlineStyle, WrapFrame,
};
use aterm_search::MAX_SEARCH_MATCHES;
use aterm_session::sink::SinkWriter;
use aterm_window::event_loop::EventLoopProxy;

use super::{
    DimsSnapshot, control_media, cursor_style_name, json_ok, json_str_field, pct_encode,
    visible_char,
};
use crate::frame_key::FrameKey;
use crate::frame_memo::{FrameSlot, ScreenText, Styled};
use crate::image_payload_memo::{ImagePayload, image_payloads};
use crate::metrics::wire::{
    FieldSink, FlagWords, Form, JsonSink, ReadCtx, Request, Syntax, TextSink,
};
use crate::session_waits::{RewrapGauge, RewrapReading};
use crate::{Wake, term_lock};

/// The visible, trailing-trimmed text of screen row `r`: the engine's combining-aware
/// `get_line_text` with control chars collapsed to spaces. The one source for a row's text, so
/// `text`, `text --json` and the `subscribe screen` delta stay byte-identical. Caller holds the
/// term lock. The body is `aterm_control::wire::visible_row` because `cast drift` replays
/// recordings through an engine where no GUI links and compares its rows with these.
pub(crate) fn visible_row(t: &Terminal, r: usize) -> String {
    aterm_control::wire::visible_row(t, r)
}

// THE one rule for a reply's blank tail (`text trim`, `turn … trim=1`, `temporal …
// trim`, the `subscribe … screen,trim` delta): it lives beside the collapse rule in
// `aterm_control::wire`, where `blocktext` counts with it before it frames a row.
pub(crate) use aterm_control::wire::trimmed_len;

/// The `text` grammar, as the wire states it on a usage error. It names `--json`, which
/// `control.rs` strips before [`text_args`] runs.
pub(crate) const TEXT_USAGE: &str =
    "ERR usage: text [--json] [trim] [tail=<n>|rows=<a>-<b>] [order=<logical|display>]\n";

/// The `screen` grammar, as the wire states it on a usage error. Any other argument is refused,
/// not ignored.
pub(crate) const SCREEN_USAGE: &str = "ERR usage: screen [--json] [since=<etag>]\n";

/// The answer to a `rows=<a>-<b>` span with `b < a`: the grammar is right, the numbers name
/// nothing.
pub(crate) const ROWS_REVERSED: &str = "ERR bad rows: the span ends before it starts\n";

/// The answer to a row span that meets no row of a `grid_rows`-row grid, naming the rows that
/// exist. `text rows=` and `await match|gone … rows` share it.
pub(crate) fn rows_off_grid(grid_rows: usize) -> String {
    format!(
        "ERR bad rows: the span meets no visible row (grid rows 0..={})\n",
        grid_rows.saturating_sub(1)
    )
}

/// Which rows of the visible grid a `text` read returns. The shaped forms serve bottom-pinned
/// TUIs: a composer on the last row defeats `trim`, so `tail=<n>` reads just the bottom and
/// `rows=<a>-<b>` any other region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TextShape {
    /// The whole grid.
    #[default]
    All,
    /// `tail=<n>`: the last `n` rows (`n >= 1`; more than the grid has is the grid).
    Tail(usize),
    /// `rows=<a>-<b>`: the inclusive 0-based span `a..=b` (`a <= b`), clamped to
    /// the grid when it is selected ([`TextShape::select`]).
    Rows(usize, usize),
}

impl TextShape {
    /// The half-open row range this shape selects on a `grid_rows`-row grid. `Tail` and `Rows`
    /// clamp to the grid; a `Rows` span that meets no row is [`rows_off_grid`] rather than an
    /// empty `OK 0`.
    pub(crate) fn select(self, grid_rows: usize) -> Result<(usize, usize), String> {
        match self {
            TextShape::All => Ok((0, grid_rows)),
            TextShape::Tail(n) => Ok((grid_rows.saturating_sub(n), grid_rows)),
            TextShape::Rows(a, b) => {
                if a >= grid_rows {
                    return Err(rows_off_grid(grid_rows));
                }
                Ok((a, b.saturating_add(1).min(grid_rows)))
            }
        }
    }
}

/// A parsed `text` / `text --json` argument tail ([`text_args`]): the rows, and whether that
/// slice's trailing blank rows are dropped (`trim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TextArgs {
    pub(crate) trim: bool,
    pub(crate) shape: TextShape,
    /// `order=display`: each row as the frame paints it, right-to-left runs reversed
    /// ([`Terminal::row_display_order`]), rather than the logical order a driver `send`s against
    /// (`order=logical`, the default). The JSON form then also carries each row's visual→logical
    /// column map.
    pub(crate) display: bool,
}

/// One row as [`TextArgs::display`] asks for it: the logical [`visible_row`],
/// or the display-order text and, when the order is not the identity, the
/// visual→logical column map (`map[v] == l`: logical column `l` is drawn at
/// visual column `v`).
fn row_in_order(t: &Terminal, r: usize, display: bool) -> (String, Option<Vec<usize>>) {
    if !display {
        return (visible_row(t, r), None);
    }
    let Some((text, map)) = u16::try_from(r).ok().and_then(|r| t.row_display_order(r)) else {
        return (visible_row(t, r), None);
    };
    let text: String = text.chars().map(visible_char).collect();
    let identity = map.iter().enumerate().all(|(v, &l)| v == l);
    (text, (!identity).then_some(map))
}

/// The `text` / `text --json` argument tail: `trim`, `order=`, and at most one of `tail=<n>` /
/// `rows=<a>-<b>`, in any order; empty for the whole grid. Anything else (`text 20`, a doubled
/// option, `tail=0`, a malformed span) is [`TEXT_USAGE`], so an agent guessing a modifier learns
/// it guessed wrong; a reversed span (`rows=5-2`) is [`ROWS_REVERSED`]. The `Err` is the reply
/// line.
pub(crate) fn text_args(rest: &str) -> Result<TextArgs, String> {
    let usage = || Err(TEXT_USAGE.to_string());
    let mut args = TextArgs::default();
    let mut shaped = false;
    let mut ordered = false;
    for tok in rest.split_whitespace() {
        if tok == "trim" {
            if args.trim {
                return usage();
            }
            args.trim = true;
            continue;
        }
        if let Some(order) = tok.strip_prefix("order=") {
            if ordered {
                return usage();
            }
            args.display = match order {
                "display" => true,
                "logical" => false,
                _ => return usage(),
            };
            ordered = true;
            continue;
        }
        if shaped {
            return usage();
        }
        args.shape = if let Some(n) = tok.strip_prefix("tail=") {
            match n.parse::<usize>() {
                Ok(n) if n > 0 => TextShape::Tail(n),
                _ => return usage(),
            }
        } else if let Some(span) = tok.strip_prefix("rows=") {
            let Some((a, b)) = span.split_once('-') else {
                return usage();
            };
            let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) else {
                return usage();
            };
            if a > b {
                return Err(ROWS_REVERSED.to_string());
            }
            TextShape::Rows(a, b)
        } else {
            return usage();
        };
        shaped = true;
    }
    Ok(args)
}

/// Split a trailing `trim` token off a positional argument tail: `(<tail without it>, true)`
/// when the last token is `trim`, else `(<tail>, false)`. Used by `blocktext <id> [trim]` and
/// `temporal [status|<tick>] [trim]`; each verb still validates what is left.
pub(crate) fn split_trim_tail(rest: &str) -> (&str, bool) {
    let rest = rest.trim();
    match rest.strip_suffix("trim") {
        Some(head) if head.is_empty() || head.ends_with(char::is_whitespace) => {
            (head.trim_end(), true)
        }
        _ => (rest, false),
    }
}

/// `blocktext <id> [trim]`: the grammar is validated HERE, and only the parsed id
/// and whether `trim` was given reach the shared block-output emitter (`emit`, which
/// frames the reply once, `trim` included, after the terminal is released). A
/// non-numeric id or any other tail is the usage line rather than a token that
/// vanishes (the `text` hole, on a second verb).
pub(crate) fn cmd_blocktext_args(rest: &str, emit: impl FnOnce(u64, bool) -> String) -> String {
    let (id, trim) = split_trim_tail(rest);
    let Ok(id) = id.parse::<u64>() else {
        return "ERR usage: blocktext <id> [trim]\n".to_string();
    };
    emit(id, trim)
}

/// Frame a gathered screen `body` (`rows` newline-terminated rows, the terminal lock already
/// released) as `OK <n>[ <verdict>]\n` + all rows, or with `trim`,
/// `OK <sent>[ <verdict>] trimmed=<k>\n` + the first `sent` rows ([`trimmed_len`]). `verdict`
/// is `turn`'s `turn submitted=… hash=…` run (empty for `text`). A nonzero `first` (a
/// `tail=`/`rows=` slice) closes the header with ` first=<row>`, so reply line i is screen row
/// `first+i`. New header fields go last; an untrimmed whole-grid reply keeps the bare framing,
/// because scripts count rows.
pub(crate) fn frame_rows_reply(
    body: &str,
    rows: usize,
    verdict: &str,
    trim: bool,
    first: usize,
) -> String {
    let sent = if trim {
        trimmed_len(body.lines())
    } else {
        rows
    };
    let mut out = String::with_capacity(body.len() + verdict.len() + 32);
    {
        use std::fmt::Write as _;
        let _ = write!(out, "OK {sent}");
        if !verdict.is_empty() {
            let _ = write!(out, " {verdict}");
        }
        if trim {
            let _ = write!(out, " trimmed={}", rows.saturating_sub(sent));
        }
        if first > 0 {
            let _ = write!(out, " first={first}");
        }
        out.push('\n');
    }
    if sent == rows {
        out.push_str(body);
    } else {
        for row in body.split_inclusive('\n').take(sent) {
            out.push_str(row);
        }
    }
    out
}

/// `text` -> `OK <nrows>\n` then each visible row (trailing spaces trimmed): the
/// bare form of [`cmd_text_opt`], which the fidelity tests read the whole grid by.
///
/// FIDELITY (I-1): each row is extracted through the engine's combining-aware
/// `get_line_text` — the SAME path `selection_to_string`/`copy` and the
/// render frame's `combining`/`clusters` lanes use — so an NFD accent
/// (`e`+U+0301) or a ZWJ emoji cluster (👨‍👩‍👧) reads back intact instead of
/// being flattened to its base codepoint. (The old per-`RenderCell` scan only
/// saw the resolved base char and silently dropped combining marks / clusters,
/// corrupting the AI's primary screen-read.) Control chars still collapse to
/// spaces via the extraction's NUL→space rule plus an explicit visible map.
///
/// Test-only since the dispatch started passing its tail through [`cmd_text_opt`]:
/// the fidelity tests read the whole grid by this name and nothing in the lib does.
#[cfg(test)]
pub(crate) fn cmd_text(term: &Arc<Mutex<Terminal>>) -> String {
    cmd_text_opt(term, &FrameSlot::default(), TextArgs::default())
}

/// THE SCREEN'S STAMP as a test reads it: `(content_seq, FNV-1a-64 of the
/// untrimmed visible screen)`, `status`'s `seq=`/`hash=` and the pair `turn`
/// returns and `history` keeps per turn id. Production reads the hash off the
/// session's published text ([`crate::frame_memo::ScreenText::fnv`]); this is
/// the ground truth it is held to.
#[cfg(test)]
pub(crate) fn screen_stamp(t: &Terminal) -> (u64, u64) {
    (t.content_seq(), screen_hash(t))
}

/// FNV-1a-64 of [`screen_text`]'s bytes, folded row by row so no full-screen
/// String is built: the hash carries its state across `write`s, so hashing
/// each row and then its `\n` is the hash of their concatenation. A test's
/// ground truth for [`crate::frame_memo::ScreenText::fnv`].
#[cfg(test)]
pub(crate) fn screen_hash(t: &Terminal) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    for r in 0..t.rows() as usize {
        h.write(visible_row(t, r).as_bytes());
        h.write(b"\n");
    }
    h.finish()
}

// The screen's generation, `<epoch>.<seq>` on the wire. The type and its parser live in
// `aterm-types` so the supervisor that sends `if-gen=` and this server share them. The epoch
// is `ContentScrollState::invalidation_epoch`, which every alternate-screen switch advances.
pub(crate) use aterm_types::control_verbs::ScreenGen;

/// [`ScreenGen`] of `t` now (the caller holds its lock).
pub(crate) fn screen_gen(t: &Terminal) -> ScreenGen {
    ScreenGen {
        epoch: t.content_scroll_state().invalidation_epoch,
        seq: t.content_seq(),
    }
}

/// FNV-1a-64 of the visible rows `first..=last`, each as [`visible_row`]
/// serves it followed by `\n`: the `turn if-rows=` fence's value by
/// [`aterm_types::control_verbs::RowsFence::hash_rows`], read row by row. A
/// test's ground truth for [`crate::frame_memo::ScreenText::rows_hash`], which
/// the fence reads. `None` for an empty range or one past the grid.
#[cfg(test)]
pub(crate) fn rows_hash(t: &Terminal, first: usize, last: usize) -> Option<u64> {
    if first > last || last >= t.rows() as usize {
        return None;
    }
    let rows: Vec<String> = (first..=last).map(|r| visible_row(t, r)).collect();
    Some(aterm_types::control_verbs::RowsFence::hash_rows(
        rows.iter().map(String::as_str),
    ))
}

/// The whole visible screen as text: every row [`visible_row`] renders, each
/// followed by a newline, untrimmed.
///
/// THE ONE CONSTRUCTION the hashed screen has. `turn` used to build this inline
/// and the stamp built it again, which made "the same pair `turn` returns" true
/// by coincidence rather than by construction: a trailing space added to one
/// loop and not the other would have made every stamped report unmatchable and
/// nothing would have failed until it did. Each session's published
/// [`crate::frame_memo::ScreenText`] is built by this, once per change of the
/// screen's text key, and `status hash=`, `turn hash=`, the `if-fp=` and
/// `if-rows=` fences and the logical `text` rows are all read off that one
/// build. `screen_stamp_matches_the_text_verbs_rows` pins that `text` serves
/// these bytes. Each row is appended straight into the one buffer
/// ([`aterm_control::wire::visible_row_into`]).
///
/// A memo, not a running hash on the engine's write path: a reader can still
/// RECOMPUTE the stamp from the rows `text` served, which is the whole value of
/// the stamp. COST of a build, measured 2026-09-20 on a 60x200 grid: 61.6 us,
/// of which 55.3 us renders the rows and ~6 us is the hash; a reader of an
/// unchanged screen pays neither.
/// The whole visible screen as text: every row [`visible_row`] renders, each
/// followed by a newline, untrimmed — the one construction of the screen
/// [`screen_hash`] folds and `turn` serves. `cmd_text_opt` keeps its own loop
/// because it serves a row span (`tail=`, `rows=`); that the whole-grid span
/// agrees with this is pinned by `screen_stamp_matches_the_text_verbs_rows`.
pub(crate) fn screen_text(t: &Terminal) -> String {
    let mut screen = String::with_capacity(t.rows() as usize * (t.cols() as usize + 1));
    for r in 0..t.rows() as usize {
        aterm_control::wire::visible_row_into(t, r, &mut screen);
        screen.push('\n');
    }
    screen
}

/// `text [trim] [tail=<n>|rows=<a>-<b>]` -> `OK <n>[ trimmed=<k>][ first=<row>]\n`
/// then `<n>` visible rows. Bare, `n` is the grid's row count. With `trim`, the rows
/// after the last non-blank one are dropped ([`trimmed_len`]), `n` is the count
/// ACTUALLY SENT and `trimmed=<k>` says how many went — the header stays honest for
/// a client that frames the body by its count, and every reader in the workspace
/// takes only the first token (`aterm-ctl`'s `stream_count`, `aterm-agent`'s
/// `read_text`, nest's reader), so the marker is additive. A shape
/// ([`TextShape::select`]) picks the rows BEFORE they are read, so a `tail=20` on
/// a 63-row grid extracts 20 rows, not 63 and then a cut; `trim` measures that
/// slice, and `first=<row>` says where it starts whenever that is not row 0. The
/// rows themselves are the same [`visible_row`] text as ever: in logical order,
/// a slice of the session's published [`ScreenText`].
pub(crate) fn cmd_text_opt(
    term: &Arc<Mutex<Terminal>>,
    frame: &FrameSlot,
    args: TextArgs,
) -> String {
    // Logical rows are a span of the session's published text, built under the
    // lock only when the screen changed since the last build and sliced with it
    // released. `order=display` reads its own rows into ONE buffer sized up front
    // (one full row + newline each), so the row loop never reallocates-and-copies
    // the accumulated screen while holding the terminal lock. The header is
    // written AFTER the lock is released — a trimmed count is only known once
    // every row has been read — and the one memcpy into the reply is paid with
    // the PTY reader unblocked.
    let mut look = frame.look_frame();
    let (first, end, text, display) = {
        let t = term_lock(term);
        let (first, end) = match args.shape.select(t.rows() as usize) {
            Ok(span) => span,
            Err(err) => return err,
        };
        if args.display {
            let mut body = String::with_capacity((end - first) * (t.cols() as usize + 1));
            for r in first..end {
                body.push_str(&row_in_order(&t, r, true).0);
                body.push('\n');
            }
            (first, end, None, body)
        } else {
            (first, end, Some(look.text(&t)), String::new())
        }
    };
    let body = text
        .as_deref()
        .map_or(display.as_str(), |text| text.span(first, end));
    frame_rows_reply(body, end - first, "", args.trim, first)
}

/// `cursor` -> `OK <row> <col> <visible:0|1> <style>\n` (0-based). `<style>` is the DECSCUSR
/// cursor style as [`cursor_style_name`] spells it (`blinking_block` by default).
pub(crate) fn cmd_cursor(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let c = t.cursor();
    let vis = u8::from(t.cursor_visible());
    let style = cursor_style_name(t.cursor_style());
    format!("OK {} {} {} {}\n", c.row, c.col, vis, style)
}

/// `cell <r> <c>` -> `OK <grapheme> <fg> <bg> <attrs>[ link=<url>]\n` or `ERR <msg>\n`.
///
/// `<grapheme>` is the cell's FULL on-screen grapheme — the resolved base char
/// plus any complex-cluster string and combining marks — percent-encoded into a
/// single space-free token (decode it the same way as `cwd`/`cmdline`). It is
/// the SAME text the `text`/`search`/selection paths and the renderer's
/// `combining`/`clusters` frame lanes produce, so a single-cell read of `é`
/// (`e`+U+0301) or a ZWJ family (👨‍👩‍👧) is faithful. A blank or
/// wide-continuation cell yields an empty token. `<fg>`/`<bg>`
/// are the fully-resolved `rrggbb` colors the renderer would paint; `<attrs>` is
/// a comma-separated list (or `none`) of the cell's active text attributes —
/// `bold,dim,italic,underline,blink,inverse,strike,hidden` — followed by the
/// width marker `wide` (the lead cell of a double-width glyph) or `wide_cont`
/// (its blank right-half spacer) when the cell is one; the marker joins the
/// list, replacing a bare `none` (pinned by `cell_attrs_carry_the_width_markers`).
/// `link=` is the cell's percent-encoded OSC 8 target, only when it has one.
///
/// `cell abs=<n> <c>` reads the cell of ABSOLUTE row `n` (B-2's one coordinate
/// space, `line`'s and `search`'s): a history row, materialized as a
/// scrolled-back viewport row is and resolved through the same colours and
/// decorations, whatever the viewport shows, or a live row as `cell <r> <c>`
/// reads it. A history row's cell carries no ` link=`. `ERR evicted` for a row
/// older than retained history and `ERR out of range` for one past the live
/// screen's end, as `line` says them. Why: a reader that walks a message up
/// into the history (`line`) can tell its rows by how they are drawn, as it can
/// on the screen (the window harness bounds agy's sent message by its cells).
pub(crate) fn cmd_cell(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let mut it = rest.split_whitespace();
    let (Some(rs), Some(cs)) = (it.next(), it.next()) else {
        return "ERR usage: cell <r> <c> | cell abs=<n> <c>\n".to_string();
    };
    let abs = match rs.strip_prefix("abs=") {
        Some(n) => match n.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => return "ERR bad args\n".to_string(),
        },
        None => None,
    };
    let Ok(c) = cs.parse::<usize>() else {
        return "ERR bad args\n".to_string();
    };
    let t = term_lock(term);
    let out_of_range = || {
        format!(
            "ERR out of range: rows 0-{}, cols 0-{}\n",
            t.rows().saturating_sub(1),
            t.cols().saturating_sub(1)
        )
    };
    let at = match abs {
        Some(n) => match t.abs_row_at(n) {
            AbsRowAt::Screen { row } => CellRow::Screen(row),
            AbsRowAt::History { rev_idx } => CellRow::History(rev_idx),
            AbsRowAt::Evicted => return "ERR evicted\n".to_string(),
            AbsRowAt::OutOfRange => return "ERR out of range\n".to_string(),
        },
        None => match rs.parse::<usize>() {
            Ok(r) => CellRow::Screen(r),
            Err(_) => return "ERR bad args\n".to_string(),
        },
    };
    // Bound by the GRID (per `dims`), not by row content: `render_row` trims
    // trailing blanks, but every 0<=r<rows, 0<=c<cols is a real, readable cell.
    if c >= t.cols() as usize || matches!(at, CellRow::Screen(r) if r >= t.rows() as usize) {
        return out_of_range();
    }
    // LIVE frame (offset-INDEPENDENT), matching the offset-independent `text`/
    // `cell_grapheme` reads — never a scrolled-back row's colours on a live glyph;
    // a history row its own materialized cells, whatever the viewport shows.
    let (row, view) = match at {
        CellRow::Screen(r) => (
            t.render_row_at_screen(r),
            t.grid().screen_row_view(r as u16),
        ),
        CellRow::History(rev_idx) => {
            let mut row = Vec::new();
            t.render_history_row_into(rev_idx, &mut row);
            (row, t.grid().history_row_view(rev_idx))
        }
    };
    let (fg, bg) = match row.get(c) {
        Some(cell) => (cell.fg, cell.bg),
        // A blank in-grid cell is the engine's implicit `Cell::EMPTY`, resolved through the
        // live palette/reverse-video path (raw defaults are wrong under DECSCNM).
        None => {
            let blank = t.implicit_blank_render_cell();
            (blank.fg, blank.bg)
        }
    };
    // ONE row handle (the row `render_row_at_screen` and `cell_grapheme`
    // resolve, or the history row rendered above) for the grapheme, the attrs
    // and the width markers.
    let col = c as u16;
    let cell = view.cell(col);
    // PROTECTED (DECSCA) shares a flag bit with WIDE_CONTINUATION;
    // `is_wide_continuation` disambiguates via the left neighbor, so a
    // protected character is ordinary text.
    let wide_cont = view.is_wide_continuation(col);
    // Combining-aware grapheme for THIS cell, via the same core extraction the
    // selection/text paths use (`cell_grapheme`'s bytes). A wide-continuation
    // cell yields "" (its glyph belongs to the lead cell); a blank cell yields a
    // space.
    let mut grapheme = String::new();
    if let Some(cell) = cell
        && !wide_cont
    {
        view.push_cell_text(col, cell, &mut grapheme);
    }
    let grapheme_tok = pct_encode(&grapheme);
    // Width markers, so a consumer can distinguish a full-width (CJK) glyph from
    // an ASCII space without inferring from columns:
    //   `wide`      — the LEAD cell, which holds the double-width glyph
    //   `wide_cont` — its blank right-half spacer
    // A protected character gets NEITHER token.
    let flags = cell.map_or_else(CellFlags::default, cell_attrs);
    let mut attrs = attrs_string(flags);
    let wide_tok = if flags.contains(CellFlags::WIDE) {
        Some(aterm_codec::cell::Attribute::Wide.word())
    } else if wide_cont {
        Some(aterm_codec::cell::Attribute::WideCont.word())
    } else {
        None
    };
    if let Some(tok) = wide_tok {
        if attrs == aterm_codec::cell::NONE {
            attrs = tok.to_string();
        } else {
            attrs.push(',');
            attrs.push_str(tok);
        }
    }
    // OSC 8 hyperlink target for this cell, surfaced so an introspecting
    // intelligence sees the link a human would click. Appended as a trailing
    // ` link=<url>` token only when present — positional fields 1-4 (grapheme,
    // fg, bg, attrs) are unchanged, so existing parsers keep working.
    let link = match at {
        CellRow::Screen(r) => t
            .hyperlink_at(r as u16, c as u16)
            .map(|u| format!(" link={}", pct_encode(u)))
            .unwrap_or_default(),
        CellRow::History(_) => String::new(),
    };
    format!(
        "OK {grapheme_tok} {:02x}{:02x}{:02x} {:02x}{:02x}{:02x} {attrs}{link}\n",
        fg[0], fg[1], fg[2], bg[0], bg[1], bg[2],
    )
}

/// The row [`cmd_cell`] reads: a live screen row, or a history row by how far
/// above the live screen (`cell abs=<n>`).
#[derive(Clone, Copy)]
enum CellRow {
    Screen(usize),
    History(usize),
}

/// The [`CellFlags`] of a live grid `cell`, read off the LIVE frame
/// (`screen_row_view`, offset-INDEPENDENT), NOT the display-mapped `grid.row` —
/// so the attrs a socket read reports pair with the live glyph/colours
/// regardless of the GUI's scroll position (see `render_row_at_screen`). A
/// cell carries its attribute bits inline.
fn cell_attrs(cell: Cell) -> CellFlags {
    cell.flags()
}

/// Render active text attributes as a stable comma list, or `none` when bare. `underline`
/// covers every underline style.
fn attrs_string(flags: CellFlags) -> String {
    let any_underline = CellFlags::UNDERLINE
        .union(CellFlags::DOUBLE_UNDERLINE)
        .union(CellFlags::CURLY_UNDERLINE);
    let mut parts: Vec<&str> = Vec::new();
    if flags.contains(CellFlags::BOLD) {
        parts.push(aterm_codec::cell::Attribute::Bold.word());
    }
    if flags.contains(CellFlags::DIM) {
        parts.push(aterm_codec::cell::Attribute::Dim.word());
    }
    if flags.contains(CellFlags::ITALIC) {
        parts.push(aterm_codec::cell::Attribute::Italic.word());
    }
    if flags.intersects(any_underline) {
        parts.push(aterm_codec::cell::Attribute::Underline.word());
    }
    if flags.contains(CellFlags::BLINK) {
        parts.push(aterm_codec::cell::Attribute::Blink.word());
    }
    if flags.contains(CellFlags::INVERSE) {
        parts.push(aterm_codec::cell::Attribute::Inverse.word());
    }
    if flags.contains(CellFlags::STRIKETHROUGH) {
        parts.push(aterm_codec::cell::Attribute::Strike.word());
    }
    if flags.contains(CellFlags::HIDDEN) {
        parts.push(aterm_codec::cell::Attribute::Hidden.word());
    }
    if parts.is_empty() {
        aterm_codec::cell::NONE.to_string()
    } else {
        parts.join(",")
    }
}

/// Gather one coherent `dims` snapshot on the main thread: the terminal handle crosses the
/// event-loop hop so the grid and window geometry are sampled in one turn. The handler uses
/// `try_lock`, so introspection never waits behind the mutex it may be diagnosing.
fn read_dims(
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    proxy: &EventLoopProxy<Wake>,
) -> Result<DimsSnapshot, control_media::MainHopError> {
    control_media::call_main(proxy, |reply| Wake::ReadDims {
        session,
        term: Arc::clone(term),
        reply,
    })?
    .map_err(control_media::MainHopError::Reason)
}

/// Read the grid dimensions and the scrollback loss counter for `metrics` without waiting on
/// the terminal mutex. `None` means the terminal was locked; callers report `busy`/JSON `null`
/// and still return every other counter. A poisoned mutex is recovered, as in [`term_lock`].
fn try_metrics_dims(term: Option<&Arc<Mutex<Terminal>>>) -> Option<(u32, u32, u64)> {
    let Some(term) = term else {
        return Some((0, 0, 0));
    };
    let read = |terminal: &Terminal| {
        (
            u32::from(terminal.rows()),
            u32::from(terminal.cols()),
            terminal.scrollback_truncated_lines(),
        )
    };
    match term.try_lock() {
        Ok(terminal) => Some(read(&terminal)),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(read(&poisoned.into_inner())),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// Serialize a live `dims` snapshot: the four positional fields (rows, cols, grid pixel w/h)
/// first for positional readers, then the keyed geometry (cell/frame/surface sizes, centered
/// bands, transient crop, present retry, layer presentation).
pub(crate) fn serialize_dims(snapshot: &DimsSnapshot) -> String {
    let window = snapshot
        .window
        .map_or_else(|| "none".to_string(), |window| window.to_string());
    let present_retry_in_ms = snapshot
        .present_retry_in_ms
        .map_or_else(|| "none".to_string(), |delay| delay.to_string());
    // The swapchain layer's live presentation state; `none` where there is no such layer (off
    // macOS, CPU backend, no window), the spelling of every other absent keyed fact.
    let (layer_gravity, layer_scale, layer_flipped) =
        snapshot.layer_presentation.as_ref().map_or_else(
            || ("none".to_string(), "none".to_string(), "none".to_string()),
            |(g, s, f)| (g.clone(), format!("{s:.2}"), f.to_string()),
        );
    format!(
        "OK {} {} {} {} session={} cell_w={} cell_h={} font_px={:.2} scale={:.2} window={} \
         window_rows={} window_cols={} composed_rows={} grid_w={} grid_h={} \
         frame_w={} frame_h={} surface_w={} surface_h={} offset_x={} offset_y={} \
         band_left={} band_right={} band_top={} band_bottom={} crop_left={} \
         crop_right={} crop_top={} crop_bottom={} pad={} pad_top={} pad_bottom={} head={} \
         tab_rows={} deck_y={} deck_h={} viewers={} visible_viewers={} geometry={} \
         present_retry_state={} present_retry_count={} present_retry_remaining={} \
         present_retry_in_ms={} \
         layer_gravity={} layer_scale={} layer_flipped={}\n",
        snapshot.rows,
        snapshot.cols,
        snapshot.pixel_w,
        snapshot.pixel_h,
        snapshot.session,
        snapshot.cell_w,
        snapshot.cell_h,
        snapshot.font_px,
        snapshot.scale,
        window,
        snapshot.window_rows,
        snapshot.window_cols,
        snapshot.composed_rows,
        snapshot.pixel_w,
        snapshot.pixel_h,
        snapshot.frame_w,
        snapshot.frame_h,
        snapshot.surface_w,
        snapshot.surface_h,
        snapshot.offset_x,
        snapshot.offset_y,
        snapshot.band_left,
        snapshot.band_right,
        snapshot.band_top,
        snapshot.band_bottom,
        snapshot.crop_left,
        snapshot.crop_right,
        snapshot.crop_top,
        snapshot.crop_bottom,
        snapshot.pad,
        snapshot.pad_top,
        snapshot.pad_bottom,
        snapshot.head,
        snapshot.tab_rows,
        snapshot.deck_y,
        snapshot.deck_h,
        snapshot.viewers,
        snapshot.visible_viewers,
        snapshot.geometry,
        snapshot.present_retry_state,
        snapshot.present_retry_count,
        snapshot.present_retry_remaining,
        present_retry_in_ms,
        layer_gravity,
        layer_scale,
        layer_flipped,
    )
}

/// `dims` -> `OK <rows> <cols> <pixel_w> <pixel_h> [key=value ...]\n`.
pub(crate) fn cmd_dims(
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    proxy: &EventLoopProxy<Wake>,
) -> String {
    match read_dims(term, session, proxy) {
        Ok(snapshot) => serialize_dims(&snapshot),
        Err(error) => format!("ERR {error}\n"),
    }
}

const METRICS_USAGE: &str = "ERR usage: metrics [reset|percentiles] [--json]\n";

/// `metrics [reset|percentiles] [--json]` -> one `OK k=v ...\n` line of live
/// render/latency counters, or with `--json` the same fields as one JSON object
/// (`OK 1\n{...}\n`), so a driving AI can MEASURE responsiveness AND DETECT lag
/// in the same loop it drives with — `send`/`key`, then `metrics`. `metrics
/// reset` first zeroes the measurement-window stats so a SPECIFIC workload can
/// be timed: `metrics reset`, drive it, then `metrics`. Every field is
/// documented where it is emitted, once for both forms:
/// [`crate::metrics::emit_summary`] and [`crate::metrics::emit_percentiles`].
/// An argument other than `reset`/`percentiles` is refused, never answered with
/// the summary: a mistyped `reset` must not read as a successful one. `metrics
/// --json` never reaches here: the dispatch hands it to [`cmd_metrics_json`]
/// (`control::json_ask`).
pub(crate) fn cmd_metrics(term: Option<&Arc<Mutex<Terminal>>>, rest: &str) -> String {
    metrics_reply(term, rest, Syntax::Text)
}

/// [`cmd_metrics`]'s JSON form, with `--json` already taken off the request by
/// the dispatch (`control::json_ask`).
pub(crate) fn cmd_metrics_json(term: Option<&Arc<Mutex<Terminal>>>, command: &str) -> String {
    metrics_reply(term, command, Syntax::Json)
}

/// Parse `rest`, reset if it asks, then emit its body once into the sink
/// `syntax` names. The terminal is only try-locked, and only for the summary's
/// grid fields.
fn metrics_reply(term: Option<&Arc<Mutex<Terminal>>>, rest: &str, syntax: Syntax) -> String {
    let Some(request) = Request::parse(rest) else {
        return METRICS_USAGE.to_string();
    };
    if request.reset {
        crate::metrics::reset();
        crate::echo_rtt::reset();
    }
    let ctx = ReadCtx::now();
    let emit = |s: &mut dyn FieldSink| match request.form {
        Form::Summary => crate::metrics::emit_summary(&ctx, try_metrics_dims(term), s),
        Form::Percentiles => crate::metrics::emit_percentiles(&ctx, s),
    };
    match syntax {
        Syntax::Text => {
            let mut sink = TextSink::new("OK", FlagWords::ZeroOne);
            emit(&mut sink);
            let mut line = sink.finish();
            line.push('\n');
            line
        }
        Syntax::Json => {
            let mut sink = JsonSink::new();
            emit(&mut sink);
            json_ok(&sink.finish())
        }
    }
}

/// `lines` -> `OK <n>\n`: the lines of history (tiered + ring scrollback) above the visible
/// screen, with ` truncated=<k>` once a flood cut has lost history (output arrived faster than
/// the compression worker could retain it), so a count that looks complete is never short.
pub(crate) fn cmd_lines(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    lines_reply(t.grid().scrollback_lines(), t.scrollback_truncated_lines())
}

/// `mainscreen`: force the primary screen of the resolved target, recovering an alternate
/// screen a killed full-screen app left up (no `?1049l`) when the shell sends no OSC 133
/// prompt mark to trigger the engine's own recovery. [`Terminal::leave_alternate_screen`]
/// does the work; this adds the repaint. `OK left=<0|1>` says whether it left. `left=1` is
/// aterm's screen only: under ConPTY, conhost stays on the dead app's buffer until a console
/// client writes `?1049l`.
pub(crate) fn cmd_mainscreen(host: &impl aterm_control::SessionHost, sid: u64) -> String {
    let Some(left) = host.with_terminal_mut(sid, Terminal::leave_alternate_screen) else {
        return "ERR no such session\n".to_string();
    };
    if left {
        host.request_redraw(sid);
    }
    format!("OK left={}\n", u8::from(left))
}

/// The `lines` reply for `lines` retained and `truncated` lost — the one
/// formatter, so the "only when nonzero" rule is testable without a flood.
fn lines_reply(lines: usize, truncated: u64) -> String {
    if truncated > 0 {
        format!("OK {lines} truncated={truncated}\n")
    } else {
        format!("OK {lines}\n")
    }
}

/// The `offscreen` grammar, as the wire states it on a usage error: every refusal of
/// [`offscreen_args`] answers these bytes.
pub(crate) const OFFSCREEN_USAGE: &str =
    "ERR usage: offscreen [since=<i>] [tail=<n>] [max=<n>] [screen=1]\n";

/// `offscreen since=<i>` naming a row newer than the archive's newest: an index this archive
/// never issued. A `since=<origin>:<i>` from another origin is not this error; it reads from
/// the archive's start and `origin=` shows the reset.
pub(crate) const OFFSCREEN_BAD_SINCE: &str = "ERR bad since\n";

/// How many archived rows one `offscreen` reply carries when `max=` does not say (an archive
/// holds up to 65,536). `more=1` marks a page with rows left and `since=<last>` reads the next
/// (`last=` is the page's own last row, not the archive's newest).
pub(crate) const OFFSCREEN_DEFAULT_MAX: usize = 2000;

/// Where an `offscreen` read starts: `since=<i>` (this archive's index) or
/// `since=<origin>:<i>` (an index minted under `origin`, e.g. a `history` row's
/// `arch=` mark). Exclusive, like `history since=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OffscreenSince {
    /// The origin the index was minted under; `None` = this archive's own.
    pub(crate) origin: Option<u64>,
    /// The last index the caller already has (0 = from the beginning).
    pub(crate) index: u64,
}

/// A parsed `offscreen` argument tail ([`offscreen_args`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct OffscreenArgs {
    /// `since=`: rows strictly after this index (`None` = from the beginning).
    pub(crate) since: Option<OffscreenSince>,
    /// `tail=<n>`: the newest n rows after `since` instead of the oldest.
    pub(crate) tail: Option<usize>,
    /// `max=<n>`: the page size (default [`OFFSCREEN_DEFAULT_MAX`]).
    pub(crate) max: Option<usize>,
    /// `screen=1`: append the current screen rows, read under the same lock.
    pub(crate) screen: bool,
}

/// A decimal number and nothing else: ASCII digits only, so `+5`, ` 5`, `5x` and
/// the empty string are all refused (`str::parse` would take the `+`).
fn offscreen_number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The `offscreen` argument tail: `since=<i>` or `since=<origin>:<i>`, `tail=<n>`, `max=<n>`
/// and `screen=1`, each at most once, in any order. Anything else (an unknown or repeated
/// token, `tail=0`, `max=0`, a non-number, `screen=` other than 1) is [`OFFSCREEN_USAGE`].
/// `tail=` and `max=` combine: the newest `min(tail, max)` rows.
pub(crate) fn offscreen_args(rest: &str) -> Result<OffscreenArgs, String> {
    let usage = || Err(OFFSCREEN_USAGE.to_string());
    let count = |v: &str| -> Option<usize> {
        offscreen_number(v)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| n > 0)
    };
    let mut args = OffscreenArgs::default();
    for tok in rest.split_whitespace() {
        let Some((key, value)) = tok.split_once('=') else {
            return usage();
        };
        match key {
            "since" if args.since.is_none() => {
                let since = match value.split_once(':') {
                    Some((origin, index)) => {
                        match (offscreen_number(origin), offscreen_number(index)) {
                            (Some(origin), Some(index)) => OffscreenSince {
                                origin: Some(origin),
                                index,
                            },
                            _ => return usage(),
                        }
                    }
                    None => match offscreen_number(value) {
                        Some(index) => OffscreenSince {
                            origin: None,
                            index,
                        },
                        None => return usage(),
                    },
                };
                args.since = Some(since);
            }
            "tail" if args.tail.is_none() => match count(value) {
                Some(n) => args.tail = Some(n),
                None => return usage(),
            },
            "max" if args.max.is_none() => match count(value) {
                Some(n) => args.max = Some(n),
                None => return usage(),
            },
            "screen" if !args.screen && value == "1" => args.screen = true,
            _ => return usage(),
        }
    }
    Ok(args)
}

/// `offscreen [since=<i>|since=<origin>:<i>] [tail=<n>] [max=<n>] [screen=1]` -> the rows an
/// alternate-screen app scrolled off its top, from the session's alt-screen archive
/// (`aterm_core::terminal::AltArchive`), line-framed:
///
/// `OK <n> first=<i> last=<j> lost=<k> breaks=<b> back=<d>[ back_at=<i> pin=<p>]
/// epoch=<e> origin=<o> alt=<0|1> seq=<s>[ enabled=0][ more=1][ screen_rows=<m>]`
/// then `<n>` lines.
///
/// * Archived rows come first, oldest first: line `m` is index `first + m`. `last` is the
///   page's last archived row (with no row, where the read stopped; 0 = nothing yet), so the
///   next page or poll is always `since=<last>`.
/// * `since=` is exclusive. An index from another origin (a restarted process, or a handoff
///   that could not carry the archive) reads from the start and `origin=` shows the reset; an
///   index past `last` is [`OFFSCREEN_BAD_SINCE`].
/// * At most `max=` rows (default [`OFFSCREEN_DEFAULT_MAX`]): the oldest after `since`, or with
///   `tail=<n>` the newest `n`; `more=1` when rows were left out.
/// * `lost=` counts rows after `since` evicted before this read; `breaks=` the discontinuities
///   at or after `since` (a redraw with no overlap, a resize, leaving the alt screen, a reset,
///   a restore); `back=` how many archived rows the screen still shows at the top of its
///   scrolling region (screen rows `pin .. pin+back` are archived rows `back_at ..`); `epoch=`
///   the baseline generation; `alt=` whether the alt screen is active; `seq=` the content seq.
///   All are read under the same lock as the rows. `enabled=0` means the archive is off, so an
///   empty reply is not "nothing scrolled off".
/// * `screen=1` appends the current screen rows (the `text` rows) under that same lock and
///   closes the header with `screen_rows=<m>`; `<n>` counts every line, so the archived rows
///   are the first `n - m`.
///   `frame=open` before `screen_rows=` says the program's DEC 2026 frame was unfinished at
///   the read ([`Terminal::frame_unfinished_at`], as `text --json`'s `"frame":"open"`): the
///   screen rows are a prefix of a redraw, to read again after `await frame`.
///
/// The archive's rows are cloned out as `Arc<str>` under the terminal lock (one
/// refcount bump each) and the reply is formatted AFTER the lock is released: a
/// full archive is 4 MiB, which must never be copied while the PTY reader and the
/// renderer wait on the lock.
pub(crate) fn cmd_offscreen(term: &Arc<Mutex<Terminal>>, frame: &FrameSlot, rest: &str) -> String {
    let args = match offscreen_args(rest) {
        Ok(args) => args,
        Err(err) => return err,
    };
    // `screen=1`'s rows are the session's published text.
    let mut look = frame.look_frame();
    let (read, alt, seq, screen) = {
        let t = term_lock(term);
        let archive = t.alt_archive();
        let since = match args.since {
            None => 0,
            // Minted under another origin: read from the start; `origin=` says why.
            Some(OffscreenSince {
                origin: Some(origin),
                ..
            }) if origin != archive.origin() => 0,
            Some(OffscreenSince { index, .. }) if index > archive.last() => {
                return OFFSCREEN_BAD_SINCE.to_string();
            }
            Some(OffscreenSince { index, .. }) => index,
        };
        let page = args.max.unwrap_or(OFFSCREEN_DEFAULT_MAX);
        let query = match args.tail {
            Some(n) => aterm_core::terminal::AltArchiveQuery::newest(since, n.min(page)),
            None => aterm_core::terminal::AltArchiveQuery::oldest(since, page),
        };
        let read = archive.read(query);
        let screen = args.screen.then(|| {
            (
                look.text(&t),
                t.frame_unfinished_at(std::time::Instant::now()),
            )
        });
        (read, t.is_alternate_screen(), t.content_seq(), screen)
    };
    let frame_open = screen.as_ref().is_some_and(|(_, open)| *open);
    format_offscreen_reply(
        &read,
        alt,
        seq,
        screen.as_ref().map(|(text, _)| &**text),
        frame_open,
    )
}

/// Frame an `offscreen` read (see [`cmd_offscreen`]); runs with no lock held.
fn format_offscreen_reply(
    read: &aterm_core::terminal::AltArchiveRead,
    alt: bool,
    seq: u64,
    screen: Option<&ScreenText>,
    frame_open: bool,
) -> String {
    use std::fmt::Write as _;
    let screen_rows = screen.map_or(0, ScreenText::row_count);
    let body_bytes: usize = read.rows.iter().map(|r| r.len() + 1).sum::<usize>()
        + screen.map_or(0, |s| s.whole().len());
    let mut out = String::with_capacity(body_bytes + 192);
    // The page's own last row, so `since=<last>` reads the next page.
    let page_last = read.page_last();
    let _ = write!(
        out,
        "OK {} first={} last={} lost={} breaks={} back={}",
        read.rows.len() + screen_rows,
        read.first,
        page_last,
        read.lost,
        read.gaps.len(),
        read.back,
    );
    if read.back > 0 {
        let _ = write!(out, " back_at={} pin={}", read.back_at, read.pin);
    }
    let _ = write!(
        out,
        " epoch={} origin={} alt={} seq={}",
        read.epoch,
        read.origin,
        u8::from(alt),
        seq,
    );
    if !read.enabled {
        out.push_str(" enabled=0");
    }
    if read.more {
        out.push_str(" more=1");
    }
    if frame_open {
        out.push_str(" frame=open");
    }
    if let Some(screen) = screen {
        let _ = write!(out, " screen_rows={}", screen.row_count());
    }
    out.push('\n');
    for row in &read.rows {
        // The archive stores rows already collapsed the way `text` collapses them;
        // collapse again anyway, because a control char left in a row (a newline
        // above all) would break the n-line framing for every client.
        push_visible(&mut out, row, VisibleTrim::None);
        out.push('\n');
    }
    if let Some(screen) = screen {
        out.push_str(screen.whole());
    }
    out
}

/// `line <n>` -> `OK <text>\n` for the line at MONOTONIC ABSOLUTE row `n`, or
/// `ERR out of range\n` / `ERR evicted\n` / `ERR unreadable\n`.
///
/// COORDINATE SPACE: `n` is an ABSOLUTE row — the space `blocks` (output,
/// command and prompt rows) and `search` (match rows) report — NOT a 0-based
/// history index, so either feeds `line` directly and the conversion is done HERE.
/// The mapping (identical to the engine's `text_range`):
///   `hist = n - grid.oldest_absolute_row()`
///   `hist <  scrollback_lines`        → scrollback history line `hist`
///   `hist >= scrollback_lines`        → visible row `hist - scrollback_lines`
/// A row OLDER than `oldest_absolute_row()` has scrolled past the scrollback cap
/// and is reported as an EXPLICIT `ERR evicted\n` (never silently-shifted text —
/// the same eviction contract `blocktext` honors); a retained history row whose
/// stored line cannot be read is `ERR unreadable\n`. Control chars collapse to
/// spaces; trailing spaces are trimmed ([`VisibleTrim::Spaces`]: the line as
/// written keeps a trailing NBSP or U+3000, which `text` drops). The row is read
/// whole ([`Terminal::abs_row_text_within`] with no byte bound), a history line
/// longer than the MiB search reads of it included, and framed after the
/// terminal is released.
pub(crate) fn cmd_line(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let Ok(n) = rest.trim().parse::<u64>() else {
        return "ERR usage: line <abs_row>\n".to_string();
    };
    let row = term_lock(term).abs_row_text_within(n, usize::MAX);
    let text = match row {
        AbsRowText::Text(s) => s,
        AbsRowText::Evicted => return "ERR evicted\n".to_string(),
        AbsRowText::OutOfRange => return "ERR out of range\n".to_string(),
        AbsRowText::Unreadable => return "ERR unreadable\n".to_string(),
    };
    let mut out = String::with_capacity(text.len() + 4);
    out.push_str("OK ");
    push_visible(&mut out, &text, VisibleTrim::Spaces);
    out.push('\n');
    out
}

/// `search <pat…> [case] [regex]` -> `OK <count>[ incomplete[ capped][ rewrapping][ long-line][ moving][ older]]\n`
/// then `<abs_row> <col> <len>` per match. The header ends with
/// `screen_first=<abs_row>`, the live screen's first row in the searched frame;
/// an empty pattern is `OK 0 screen_first=<abs_row>`.
///
/// SEARCH-1: backed by the engine's real `TerminalSearch`, indexing BOTH the
/// SCROLLBACK (one dense walk, [`Terminal::read_search_rows`]) AND the visible rows
/// with grapheme-aware text — so a term that has scrolled OFF the screen is
/// still found, not just the visible page. Each match's row is an ABSOLUTE row
/// (B-2's one coordinate space): feed it straight to `line`/`text`, which
/// convert at the read site. `col`/`len` are grid/DISPLAY columns within that
/// row (a wide glyph occupies 2, per the engine's `ColumnMap`) — the same cell
/// coordinate space `cell <r> <c>` addresses, NOT character offsets.
///
/// Searches scrollback and the visible rows through the engine's `TerminalSearch`. Rows are
/// absolute (feed them to `line`/`text`); `col`/`len` are display columns (a wide glyph is 2),
/// the space `cell <r> <c>` addresses. A soft-wrapped line is one logical line: a hit that
/// straddles a wrap is reported once at its start with `col + len` past the grid width (split
/// at the `dims` width for per-row spans), and regex `^`/`$` bind to the logical line's ends.
///
/// `case` makes the match case-sensitive; `regex` treats the pattern as a regular expression.
/// Flags are stripped off the tail only, so the rest (internal whitespace intact) is the
/// pattern and a lone token is always the pattern (`search case` finds "case"); a literal
/// ending in ` case`/` regex` needs the regex form (`worst\s+case regex`).
///
/// FLAGS: `case` = case-SENSITIVE match (default is case-insensitive); `regex`
/// = treat the pattern as a regular expression (requires the `aterm-search`
/// `regex` feature, enabled for the engine). Flags are stripped off the TAIL of
/// the line only; everything before them — internal whitespace intact — is the
/// pattern, so a literal containing spaces (`search command not found`) matches
/// verbatim. A single remaining token is always the PATTERN (`search case`
/// searches for "case"); a literal that itself ENDS in ` case`/` regex` needs
/// the regex form (e.g. `worst\s+case regex`).
///
/// INCOMPLETE: when the list is not exhaustive the header carries a
/// trailing ` incomplete` token, so a client never trusts a short list silently,
/// followed by every cause that applies, in this order (see
/// [`search_incomplete_marker`]): ` capped` when the list stopped at
/// [`MAX_SEARCH_MATCHES`], ` rewrapping` when a width change's rewrap overlapped
/// the search, so history it held away may not have been read, ` long-line`
/// when some line was not searched to its end — a regex scan could not finish
/// it within the line's budget (a soft-wrapped run of megabytes), or it is a
/// history line read only up to its first MiB (a crafted checkpoint can store
/// longer) — so matches past that point on that line are missing; the pattern
/// is fine, and the rest of the searched range was searched — ` moving` when
/// the screen's coordinate frame moved while the search ran (it switched
/// between the main and the alternate screen, changed width or ambiguous-width
/// policy, or renumbered its history rows: [`SearchKey::same_frame`]), so a
/// listed row may now name another line and the search is worth repeating —
/// and ` older` when, besides one of those, history older than the
/// newest `search_history_lines` lines was not read. The depth cap alone is a
/// bare ` incomplete`, as it has been since DL-2. Output that only appends or
/// rewrites lines during the search is not ` moving`: absolute rows stay put
/// when output scrolls a line into history, so every listed row still names
/// its line.
///
/// LOCKING: the O(scrollback) index build NEVER runs under the Terminal
/// mutex — a rebuild while the shell is streaming output would stall the PTY
/// reader's `process()` and the frame path for the full build time. Instead the
/// line texts are SNAPSHOTTED out of the grid in bounded chunks (the lock is
/// released between chunks, the same bounded-hold discipline as the PTY
/// reader's `PROCESS_CHUNK`) and the grapheme-aware `TerminalSearch` is built
/// lock-free from the copies. The built index stays in the terminal's slot
/// (`Terminal::search_plan`), so an unchanged repeat reuses it, and so does the
/// find bar's next search of the same terminal; at most [`SEARCH_RESIDENT_CAP`]
/// terminals keep one. An index the slot lets go of is freed after the hold
/// that let it go, never in it: freeing one is O(scrollback) too. Matching
/// still scales with candidates/results and finishes with one short
/// generation-validation lock. A result whose
/// frame moved during the search is named ` moving` rather than run again: the
/// client decides whether a second full-history pass is worth its cost.
///
/// `rewrap` is the session's rewrap gauge (`ctx.rewrap_gauge`): while a job
/// runs, the history is detached and the search reaches only the screen.
pub(crate) fn cmd_search(term: &Arc<Mutex<Terminal>>, rest: &str, rewrap: &RewrapGauge) -> String {
    cmd_search_capped(term, rest, search_max_lines(), || rewrap.reading())
}

/// [`cmd_search`] at an explicit depth cap ([`SEARCH_MAX_LINES`]), reading the rewrap gauge
/// through `rewrap`. The gauge is read before and after the history: the search takes the lock
/// once per [`SNAPSHOT_CHUNK_LINES`] rows and a width change can detach history between holds.
/// The detach and the gauge's `begin` share one hold, so a search that overlapped it sees the
/// job running at either reading, or the job count move.
fn cmd_search_capped(
    term: &Arc<Mutex<Terminal>>,
    rest: &str,
    depth_cap: usize,
    rewrap: impl Fn() -> RewrapReading,
) -> String {
    // Strip flags off the tail only, never the last remaining token, so the pattern keeps its
    // internal whitespace.
    let mut pat = rest.trim();
    let (mut case_sensitive, mut is_regex) = (false, false);
    while let Some((head, tail)) = pat.rsplit_once(char::is_whitespace) {
        match tail {
            "case" => case_sensitive = true,
            "regex" => is_regex = true,
            _ => break,
        }
        pat = head.trim_end();
    }
    if pat.is_empty() {
        let t = term_lock(term);
        let screen_first = t
            .grid()
            .oldest_absolute_row()
            .saturating_add(t.grid().scrollback_lines() as u64);
        return format!("OK 0 screen_first={screen_first}\n");
    }
    let before = rewrap();
    let searched = search_full_history_capped(term, pat, case_sensitive, is_regex, depth_cap);
    let after = rewrap();
    let rewrapping = before.running || after.running || before.jobs != after.jobs;
    search_reply(searched, rewrapping)
}

/// The `search` reply to one search ([`cmd_search`]): the header, with every
/// ` incomplete` cause that applies ([`search_incomplete_marker`]), then one
/// `<row> <col> <len>` line per match. `rewrapping` says a rewrap overlapped the
/// search.
fn search_reply(
    searched: Result<FullHistorySearch, SearchOptionsError>,
    rewrapping: bool,
) -> String {
    match searched {
        Ok(search) => {
            // Function-local so it never shadows an `io::Write` elsewhere in the module.
            use std::fmt::Write as _;
            let moving = !search.frame_stable;
            let qualifier = search_incomplete_marker(
                search.results.incomplete || rewrapping || moving,
                search.results.matches.len() >= MAX_SEARCH_MATCHES,
                rewrapping,
                search.results.cut_short_lines > 0,
                moving,
                search.searched_lines.is_some(),
            );
            let mut out = format!(
                "OK {}{qualifier} screen_first={}\n",
                search.results.matches.len(),
                search.base_y,
            );
            // Reserve an estimated 24 bytes per match in this capped list;
            // wider decimal coordinates can grow the buffer as needed.
            out.reserve(search.results.matches.len() * 24);
            for m in &search.results.matches {
                // `m.line` is the absolute row; `writeln!` into a String cannot fail.
                let _ = writeln!(out, "{} {} {}", m.line, m.start_col, m.len());
            }
            out
        }
        Err(e) => format!("ERR search: {e}\n"),
    }
}

/// The ` incomplete…` marker of a `search` reply header ([`cmd_search`]):
/// empty for an exhaustive list, else ` incomplete` followed by EVERY cause that
/// applies, in a fixed order — ` capped`, ` rewrapping`, ` long-line`,
/// ` moving`, then ` older` for the depth cap (`search_history_lines` left older history
/// unread). The depth cap alone stays a bare ` incomplete`, its form since DL-2,
/// so a reader that knows only that form still reads it right; the rule a reader
/// applies is "older history was not searched when no cause follows
/// ` incomplete`, or when ` older` does".
///
/// (R15 content-2 review): the header named ONE cause, and the
/// first one won, so a depth cap under a cut-short line (`long-line`), a full
/// list (`capped`) or a rewrap went unsaid, and an agent reading the verb help
/// concluded the older history had been searched. Every cause is now named; the
/// tokens follow the count, which is all the framing reads, so an older client
/// keeps every match and an older `aterm ctl` still names the first cause.
fn search_incomplete_marker(
    incomplete: bool,
    capped: bool,
    rewrapping: bool,
    cut_short: bool,
    moving: bool,
    older: bool,
) -> String {
    if !incomplete {
        return String::new();
    }
    let mut marker = String::from(" incomplete");
    let mut named = false;
    for (applies, token) in [
        (capped, " capped"),
        (rewrapping, " rewrapping"),
        (cut_short, " long-line"),
        (moving, " moving"),
    ] {
        if applies {
            marker.push_str(token);
            named = true;
        }
    }
    if older && named {
        marker.push_str(" older");
    }
    marker
}

/// Search results together with the absolute-row coordinate frame the index
/// was keyed against. Capturing these values in the same initial terminal lock
/// prevents the GUI from pairing matches with a `base_y` or protected-footer
/// insertion revision sampled from a different grid state.
pub(crate) struct FullHistorySearch {
    pub(crate) results: SearchResults,
    /// Exact point-relative match, unaffected by the batch result cap.
    pub(crate) point_match: Option<SearchMatch>,
    pub(crate) base_y: i64,
    pub(crate) absolute_row_revision: u64,
    /// `Grid::history_renumber_epoch()` the rows were keyed against: a width reflow renumbers
    /// every retained row without moving `absolute_row_revision`.
    pub(crate) history_renumber_epoch: u64,
    pub(crate) content_seq: u64,
    /// Grid width the soft-wrap runs were laid out in: a match's end column counts through the
    /// wrap, so dividing by any other width puts the continuation on the wrong row.
    pub(crate) cols: usize,
    /// False when the terminal left the key's generation during the search
    /// ([`SearchVerdict::consistent`]): output, a screen switch or a renumbering.
    /// Such a result is never safe for GUI highlighting, which reruns it.
    pub(crate) consistent: bool,
    /// False when the frame moved during the search ([`SearchKey::same_frame`]), so a
    /// listed row may name another line now. Output alone keeps it true: absolute rows
    /// stay put as lines scroll into history. The `search` verb names it ` moving`.
    pub(crate) frame_stable: bool,
    /// How many of the newest lines were searched when the depth cap
    /// (`search_history_lines`) left older history unread; `None` when the whole
    /// retained buffer was searched. The find surfaces say this number instead
    /// of a bare `+`.
    pub(crate) searched_lines: Option<usize>,
}

/// [`search_full_history_capped`] at the configured depth, as the socket verb
/// runs it.
#[cfg(test)]
pub(crate) fn search_full_history(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
) -> Result<FullHistorySearch, SearchOptionsError> {
    search_full_history_capped(term, query, case_sensitive, is_regex, search_max_lines())
}

/// The socket verb's full-history search (forward, unanchored) at an explicit
/// depth cap instead of the configured one (see [`SEARCH_MAX_LINES`]).
fn search_full_history_capped(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    depth_cap: usize,
) -> Result<FullHistorySearch, SearchOptionsError> {
    search_full_history_direction_capped(
        term,
        query,
        case_sensitive,
        is_regex,
        EngineSearchDirection::Forward,
        None,
        false,
        depth_cap,
    )
}

/// Direction-aware full-history query for the incremental search overlay.
///
/// The socket verb uses [`search_full_history`] and therefore retains the
/// historical oldest-first capped subset. Cmd-R uses this entry point so a
/// capped reverse search retains the newest matches instead.
///
/// THE UNIT OF MATCHING IS THE LOGICAL LINE, not the grid row. The index is
/// built in [`SearchLayout::Logical`]: every soft-wrap run is joined
/// ([`aterm_core::terminal::join_wrapped_rows`]) into the one line the reader
/// sees before the engine looks at it, so a hit may straddle a wrap; the results
/// come back in PHYSICAL coordinates ([`WrapFrame::to_physical`]) with such a
/// hit's `end_col` counting straight through the boundary. Two consequences a
/// caller has to know about, both deliberate:
///
/// Matching runs on logical lines: each soft-wrap run is joined ([`join_wrapped_rows`]) and
/// results come back physical ([`WrapFrame::to_physical`]), a straddling hit's `end_col`
/// counting through the boundary. So regex `^`/`$` anchor the logical line (a continuation
/// row's column 0 is not `^`), and a query of only spaces can match the padding of a
/// continued row, which is blank on the glass.
pub(crate) fn search_full_history_direction(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
) -> Result<FullHistorySearch, SearchOptionsError> {
    search_full_history_direction_capped(
        term,
        query,
        case_sensitive,
        is_regex,
        direction,
        anchor,
        strict,
        search_max_lines(),
    )
}

/// [`search_full_history_direction`] at an explicit depth cap ([`SEARCH_MAX_LINES`]). It
/// snapshots only the newest searchable suffix, holding the terminal lock at most
/// [`SNAPSHOT_CHUNK_LINES`] rows at a time, builds the trigram index with the lock released and
/// caches it per recent terminal ([`cached_search_index`] lists the key); a short endpoint lock
/// validates the coordinate generation. Socket and GUI callers share the cache.
#[allow(
    clippy::too_many_arguments,
    reason = "the depth cap rides alongside the query's own surface, like the anchor"
)]
fn search_full_history_direction_capped(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
    depth_cap: usize,
) -> Result<FullHistorySearch, SearchOptionsError> {
    let request = search_request(depth_cap, anchor);
    // SA-1: a literal query narrows from the previous keystroke's occurrence
    // frame kept beside the index (batch-identical results, no index work for
    // the dominant grown-by-one-char edit); regex, empty and every fallback
    // condition route through the plain batch path inside.
    full_history_search(search_terminal(term, &request, |built, _| {
        built.query(query, case_sensitive, is_regex, direction, strict)
    }))
}

/// A batch search's result as the GUI and the `search` verb read it: the
/// planned key's stamps and the finish's verdict beside the answer.
fn full_history_search(
    searched: Searched<Result<(SearchResults, Option<SearchMatch>), SearchOptionsError>>,
) -> Result<FullHistorySearch, SearchOptionsError> {
    let (results, point_match) = searched.answer?;
    let window = searched.key.window();
    Ok(FullHistorySearch {
        results,
        point_match,
        base_y: i64::try_from(window.visible_base).unwrap_or(i64::MAX),
        absolute_row_revision: searched.key.row_revision,
        history_renumber_epoch: searched.key.renumber_epoch,
        content_seq: searched.key.content_seq,
        cols: searched.wrap.cols(),
        consistent: searched.verdict.consistent,
        frame_stable: searched.verdict.frame_stable,
        searched_lines: window.searched_lines(),
    })
}

/// What every find and `search` asks of a terminal's index slot: soft-wrap runs
/// joined into the lines the reader sees ([`SearchLayout::Logical`]), the newest
/// `depth_cap` lines, and the find anchor carried into the plan's soft-wrap
/// frame. The anchor is no part of the slot's key, so a find and a `search` at
/// the same depth share one index.
fn search_request(depth_cap: usize, anchor: Option<(usize, usize)>) -> SearchRequest {
    SearchRequest {
        depth_cap,
        layout: SearchLayout::Logical,
        anchor,
    }
}

/// One search's answer, with the key it was planned under, the soft-wrap frame
/// its hits were re-expressed through, and how it stands against the terminal.
struct Searched<T> {
    answer: T,
    key: SearchKey,
    wrap: WrapFrame,
    verdict: SearchVerdict,
}

/// Run one search through `term`'s index slot (aterm-core search_index.rs): the
/// plan in one hold, a build's rows copied [`SNAPSHOT_CHUNK_LINES`] per hold,
/// the index built and `answer` run with no lock held, and the endpoint check
/// in one final hold. `answer` is told whether the slot served the index (a
/// hit). Whatever the holds took out of the slot is freed with no lock held:
/// the plan's by the build, the finish's after its hold. The terminal is then
/// the most recently searched one in the residency ledger ([`keep_resident`]).
fn search_terminal<T>(
    term: &Arc<Mutex<Terminal>>,
    request: &SearchRequest,
    answer: impl FnOnce(&mut BuiltSearch, bool) -> T,
) -> Searched<T> {
    let mut plan = term_lock(term).search_plan(request);
    let hit = plan.is_hit();
    while plan.rows_to_read() > 0 {
        term_lock(term).read_search_chunk(&mut plan, SNAPSHOT_CHUNK_LINES);
    }
    let mut built = plan.build();
    let answer = answer(&mut built, hit);
    let (key, wrap) = (*built.key(), built.wrap());
    let (verdict, discard) = term_lock(term).finish_search(built);
    drop(discard);
    keep_resident(term);
    Searched {
        answer,
        key,
        wrap,
        verdict,
    }
}

/// One exact point-relative match from the full-history index.
/// Literal navigation over the index the slot serves is lazy and independent of
/// [`aterm_search::MAX_SEARCH_MATCHES`].
pub(crate) struct FullHistoryPoint {
    pub(crate) point_match: Option<SearchMatch>,
    pub(crate) base_y: i64,
    pub(crate) absolute_row_revision: u64,
    /// See [`FullHistorySearch::history_renumber_epoch`].
    pub(crate) history_renumber_epoch: u64,
    pub(crate) content_seq: u64,
    /// See [`FullHistorySearch::cols`].
    pub(crate) cols: usize,
    pub(crate) consistent: bool,
}

pub(crate) fn search_full_history_point(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: (usize, usize),
    strict: bool,
) -> Result<FullHistoryPoint, SearchOptionsError> {
    // The batch path's request: the index holds soft-wrapped runs joined, so the
    // walk hands the anchor over in the joined line's columns and hands the hit
    // back in the grid's. An index the search had to build answers as the batch
    // path does, and so does a walk that found nothing but could not finish some
    // line: "no match" is not established, and the batch path's answer to that
    // is its own (`select_point_match` keeps the batch), never a refusal.
    let request = search_request(search_max_lines(), Some(anchor));
    full_history_point(search_terminal(term, &request, |built, hit| {
        let batch = |built: &mut BuiltSearch| {
            built
                .query(query, case_sensitive, is_regex, direction, strict)
                .map(|(_, point_match)| point_match)
        };
        if !hit {
            return batch(built);
        }
        let wrap = built.wrap();
        match built.index().find_direction_opts(
            query,
            case_sensitive,
            is_regex,
            aterm_search::DirectedFind {
                anchor: wrap.logical_anchor().unwrap_or(anchor),
                direction,
                inclusive: !strict,
                wrap: true,
            },
        ) {
            Err(SearchOptionsError::ScanCutShort) => batch(built),
            found => found.map(|found| found.map(|found| wrap.to_physical(&found))),
        }
    }))
}

/// A point search's result: the planned key's stamps and the finish's verdict
/// beside the match.
fn full_history_point(
    searched: Searched<Result<Option<SearchMatch>, SearchOptionsError>>,
) -> Result<FullHistoryPoint, SearchOptionsError> {
    Ok(FullHistoryPoint {
        point_match: searched.answer?,
        base_y: i64::try_from(searched.key.window().visible_base).unwrap_or(i64::MAX),
        absolute_row_revision: searched.key.row_revision,
        history_renumber_epoch: searched.key.renumber_epoch,
        content_seq: searched.key.content_seq,
        cols: searched.wrap.cols(),
        consistent: searched.verdict.consistent,
    })
}

/// The search index depth cap (config `search_history_lines`): how many of the newest lines the
/// trigram index retains. One per process, read by both ⌘F and the `search` verb so they share
/// one identically bounded index; the event loop writes it. Tests pass their own depth to a
/// `_capped` entry point and never write this.
static SEARCH_MAX_LINES: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_CACHED_LINES);

/// Set the search index depth cap from config (`search_history_lines`). A change
/// misses every terminal's kept index (the cap is part of its key,
/// [`SearchKey::max_lines`]), so the next search rebuilds at the new depth.
/// Clamped to at least 1 line.
pub(crate) fn set_search_max_lines(max: usize) {
    SEARCH_MAX_LINES.store(max.max(1), Ordering::Relaxed);
}

/// The current search index depth cap (see [`set_search_max_lines`]).
fn search_max_lines() -> usize {
    SEARCH_MAX_LINES.load(Ordering::Relaxed)
}

/// Grid rows copied per Terminal-lock hold while a search builds its index
/// ([`Terminal::read_search_chunk`]): bounds each hold so the PTY reader and the
/// render path interleave with a full-scrollback copy instead of stalling
/// behind it.
const SNAPSHOT_CHUNK_LINES: usize = 1024;

/// Most terminals that keep a search index at once. Eight covers normal split
/// and tab use; past it the least recently searched terminal gives its index
/// back, and its next search builds again.
const SEARCH_RESIDENT_CAP: usize = 8;

/// Which terminals keep their search index. A terminal's slot keeps the index
/// its last search built until the terminal dies or the slot is released, so
/// the process bounds how many slots stay full: past `cap`, the least recently
/// searched terminal's index is released. The ledger keeps no key and no index,
/// only the order terminals were searched in, each as a `Weak` identity (which
/// pins the allocation, so a dead terminal's address is never taken for a live
/// one's).
struct SearchResidency {
    cap: usize,
    ring: Mutex<VecDeque<Weak<Mutex<Terminal>>>>,
}

impl SearchResidency {
    const fn new(cap: usize) -> Self {
        Self {
            cap,
            ring: Mutex::new(VecDeque::new()),
        }
    }

    /// Make `term` the most recently searched terminal, and hand back the live
    /// terminal that fell past the cap, if any. Dead terminals leave first and
    /// never count against the cap; their slots died with them.
    fn touch(&self, term: &Arc<Mutex<Terminal>>) -> Option<Arc<Mutex<Terminal>>> {
        // A panicked holder leaves a whole ring; at worst it names a terminal
        // whose index is already gone.
        let mut ring = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        ring.retain(|held| {
            held.strong_count() != 0 && !std::ptr::eq(held.as_ptr(), Arc::as_ptr(term))
        });
        ring.push_back(Arc::downgrade(term));
        if ring.len() > self.cap {
            ring.pop_front().and_then(|evicted| evicted.upgrade())
        } else {
            None
        }
    }

    /// [`Self::touch`], then release the index of the terminal that fell past
    /// the cap. The ring's lock is dropped first: it is a leaf, never held
    /// across a terminal lock. The index is taken in the evicted terminal's
    /// hold and freed after it.
    fn keep(&self, term: &Arc<Mutex<Terminal>>) {
        if let Some(evicted) = self.touch(term) {
            let released = term_lock(&evicted).take_search_index();
            drop(released);
        }
    }
}

/// Keep `term`'s index resident as the most recently searched, in the
/// process's one ledger.
#[cfg(not(test))]
fn keep_resident(term: &Arc<Mutex<Terminal>>) {
    static SEARCH_RESIDENCY: SearchResidency = SearchResidency::new(SEARCH_RESIDENT_CAP);
    SEARCH_RESIDENCY.keep(term);
}

/// Keep `term`'s index resident as the most recently searched. Under test each
/// thread keeps its own ledger, so whether a test's index is still resident
/// never depends on how many terminals other tests searched meanwhile.
#[cfg(test)]
fn keep_resident(term: &Arc<Mutex<Terminal>>) {
    thread_local! {
        static SEARCH_RESIDENCY: SearchResidency = const { SearchResidency::new(SEARCH_RESIDENT_CAP) };
    }
    SEARCH_RESIDENCY.with(|ledger| ledger.keep(term));
}

/// `custody` -> one line answering "why did my selection disappear?":
/// `OK last=<transition|none> event=<0-8|-> changed=<transition|none>
/// took_selection=<transition|none> offset=<n> owner=<user|tail> selection=<yes|no>
/// scrollback=<n>`.
///
/// Offset and selection are observable after the fact; the decision that moved them is not,
/// and several events that take them leave identical state. `last` is the engine's record of
/// the last custody transition, written by the deciding site. `changed` is the last one that
/// took something (moved the offset or killed a highlight), since `last` is mostly
/// `OutputAtLive`, written by every line of output; `took_selection` the last one that took
/// the highlight. `event` is the `PressCustody` model's `last_event` tag: 0 a user gesture,
/// 1 typing, 2 an auto-repeat tick, 3 a bare modifier, 4 a release, 5 output that missed the
/// selected rows, 6 output that took the highlight (replaced its rows, or `post_process`
/// failed closed: `OutputTookTheSelectionUnattributed`), 7 output that invalidated the
/// coordinate space, 8 a gesture back toward live that is not typing; `-` only with
/// `last=none`. `owner` is derived from the offset (`TailOwnerAtBottom`), never carried
/// separately. `scrollback` is the retained history depth. No screen content is reported.
pub(crate) fn cmd_custody(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let offset = t.grid().display_offset();
    let (last, event) = t.last_custody_transition().map_or_else(
        || ("none".to_string(), "-".to_string()),
        |c| (c.action().to_string(), c.last_event().to_string()),
    );
    let changed = t
        .last_custody_change()
        .map_or_else(|| "none".to_string(), |c| c.action().to_string());
    // The field the verb is for: a deliberate deselect overwrites `changed`, so
    // `took_selection` keeps what last took the highlight for whoever asks later.
    let took = t
        .last_selection_taker()
        .map_or_else(|| "none".to_string(), |c| c.action().to_string());
    format!(
        "OK last={last} event={event} changed={changed} took_selection={took} \
         offset={offset} owner={} selection={} scrollback={}\n",
        if offset > 0 { "user" } else { "tail" },
        if t.text_selection().has_selection() {
            "yes"
        } else {
            "no"
        },
        t.grid().scrollback_lines(),
    )
}

/// `modes` -> `OK <n>\n` then `n` `key=value` lines, one per introspected mode —
/// fourteen, in this order, and the count is in the header so a client
/// streams the body (the `text`/`search` shape): `alt_screen`, `cursor_visible`,
/// `app_cursor_keys` (DECCKM), `app_keypad` (DECPAM), `bracketed_paste` (2004),
/// `mouse_mode` (`none|normal|button|any|x10`), `mouse_encoding`
/// (`x10|utf8|sgr|urxvt|sgr_pixel`), `insert_mode` (IRM), `auto_wrap` (DECAWM),
/// `origin_mode` (DECOM), `kitty_keyboard` (the active progressive-enhancement
/// flags, a csv drawn from `disambiguate`, `report_events`, `report_alternates`,
/// `report_all_keys`, `report_text`, or `none`) and `modify_other_keys` (the
/// xterm level, `0|1|2`), `focus_reporting` (1004) and `sync_output` (2026).
/// The last two joined on 2026-09-25 with the foreground handback: they are
/// two of the modes a program killed while it held the terminal leaves armed,
/// and a client checking that the handback ran needs to see them. Framing and
/// key set pinned by `modes_frames_its_count_over_distinct_key_lines`.
pub(crate) fn cmd_modes(term: &Arc<Mutex<Terminal>>) -> String {
    use aterm_types::mouse::{MouseEncoding, MouseMode};
    let t = term_lock(term);
    let m = t.modes();
    let mouse_mode = match m.mouse_mode {
        MouseMode::None => "none",
        MouseMode::Normal => "normal",
        MouseMode::ButtonEvent => "button",
        MouseMode::AnyEvent => "any",
        MouseMode::X10 => "x10",
        _ => "unknown",
    };
    let mouse_encoding = match m.mouse_encoding {
        MouseEncoding::X10 => "x10",
        MouseEncoding::Utf8 => "utf8",
        MouseEncoding::Sgr => "sgr",
        MouseEncoding::Urxvt => "urxvt",
        MouseEncoding::SgrPixel => "sgr_pixel",
        _ => "unknown",
    };
    // Which encoding a key press uses: whether the app negotiated the Kitty keyboard protocol
    // (active progressive-enhancement flags) or xterm modifyOtherKeys (level 0/1/2).
    let kbd = t.keyboard_mode();
    use aterm_types::keyboard::KeyboardMode as KM;
    let mut kitty = Vec::new();
    if kbd.contains(KM::DISAMBIGUATE_ESC_CODES) {
        kitty.push("disambiguate");
    }
    if kbd.contains(KM::REPORT_EVENT_TYPES) {
        kitty.push("report_events");
    }
    if kbd.contains(KM::REPORT_ALTERNATE_KEYS) {
        kitty.push("report_alternates");
    }
    if kbd.contains(KM::REPORT_ALL_KEYS_AS_ESC) {
        kitty.push("report_all_keys");
    }
    if kbd.contains(KM::REPORT_ASSOCIATED_TEXT) {
        kitty.push("report_text");
    }
    let kitty_str = if kitty.is_empty() {
        "none".to_string()
    } else {
        kitty.join(",")
    };
    let modify_other_keys = kbd.xterm_modify_other_keys_level();
    let lines = [
        format!("alt_screen={}", t.is_alternate_screen()),
        format!("cursor_visible={}", t.cursor_visible()),
        format!("app_cursor_keys={}", m.application_cursor_keys),
        format!("app_keypad={}", m.application_keypad),
        format!("bracketed_paste={}", m.bracketed_paste),
        format!("mouse_mode={mouse_mode}"),
        format!("mouse_encoding={mouse_encoding}"),
        // How typed input and printed output land: IRM (insert vs overwrite), DECAWM
        // (auto-wrap), DECOM (origin = scroll region).
        format!("insert_mode={}", m.insert_mode),
        format!("auto_wrap={}", m.auto_wrap),
        format!("origin_mode={}", m.origin_mode),
        // Keyboard input protocol (see above).
        format!("kitty_keyboard={kitty_str}"),
        format!("modify_other_keys={modify_other_keys}"),
        // Focus reports (1004) and synchronized output (2026): left armed by a program that
        // dies holding the terminal, and reset by the foreground handback.
        format!("focus_reporting={}", m.focus_reporting),
        format!("sync_output={}", m.synchronized_output),
    ];
    let mut out = format!("OK {}\n", lines.len());
    for l in &lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// `appstatus` -> the status-surface ledger: one `activity` line per live status-bar row, then
/// one per finished activity the ring still holds, oldest first, so what the app did on its own
/// initiative stays readable after its bar folds. Free-text fields are percent-encoded. The
/// rows are rendered on the main thread (`Wake::ReadAppStatus`), since the state is App state.
pub(crate) fn cmd_appstatus(proxy: &EventLoopProxy<Wake>) -> String {
    match control_media::call_main(proxy, |reply| Wake::ReadAppStatus { reply }) {
        Ok(rows) if rows.is_empty() => "OK 0\n".to_string(),
        Ok(rows) => format!("OK {}\n{}\n", rows.len(), rows.join("\n")),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `appnotice <toolchain|update|harness> <text>` -> `OK recorded`: text recorded in the
/// message log on the named lane from outside the process, never a row (free text has no
/// severity, action or progress). Toolchain text shaped like a `managed-current:` /
/// `machine-settings:` marker is taken as that marker; a `harness` note is an `appstatus`
/// entry of kind=harness. It is how a terminal `aterm pkg install claude` or an out-of-window
/// agent harness writes into the record the GUI's own passes write. The main thread bounds and
/// sanitizes the text; the lane is checked here, so a bad lane is a usage error, never a wake.
pub(crate) fn cmd_appnotice(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    const USAGE: &str = "ERR usage: appnotice <toolchain|update|harness> <text>\n";
    /// Longer than any marker body atpkg prints; a row is one line of chrome.
    const MAX_TEXT_BYTES: usize = 512;
    let t = rest.trim();
    let Some((lane, text)) = t.split_once(char::is_whitespace) else {
        return USAGE.to_string();
    };
    let text = text.trim();
    if !matches!(lane, "toolchain" | "update" | "harness") || text.is_empty() {
        return USAGE.to_string();
    }
    if text.len() > MAX_TEXT_BYTES {
        return format!("ERR appnotice: text is longer than {MAX_TEXT_BYTES} bytes\n");
    }
    let (lane, text) = (lane.to_string(), text.to_string());
    match control_media::call_main(proxy, |reply| Wake::AppNotice { lane, text, reply }) {
        Ok(Ok(crate::AppNoticeTaken::Recorded)) => "OK recorded\n".to_string(),
        Ok(Err(error)) => format!("ERR {error}\n"),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [live]` -> `OK <count>` and one
/// `message <id> …` row per record: the message log as text. The engine parses the arguments
/// here (`aterm_messages::wire::parse_read_args`, so junk is never a wake) and builds the rows
/// on the main thread (`Wake::ReadMessages`, `wire::message_rows`), free fields
/// percent-encoded.
pub(crate) fn cmd_messages(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let query = match aterm_messages::wire::parse_read_args(rest) {
        Ok(query) => query,
        Err(usage) => return format!("ERR {usage}\n"),
    };
    match control_media::call_main(proxy, |reply| Wake::ReadMessages { query, reply }) {
        Ok(rows) if rows.is_empty() => "OK 0\n".to_string(),
        Ok(rows) => format!("OK {}\n{}\n", rows.len(), rows.join("\n")),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `notice <post|progress|done|ask|later|dismiss|act> …` -> one status line: the message
/// band's WRITE face (design §5.2, rulings 163-179). The outside world's voice under
/// the attention rule — an info/success `post` is a record (ruling 163), a warn/error
/// `post` a row, `progress` a script's animated work in flight on one row per key
/// (`wire.<key>`, ruling 167), `done` its finish; `later` one keyed entry of the
/// exceptions digest, never a row; `dismiss` and `act` do what a
/// person's click would. Every usage, key, lane-tag and title-form refusal is
/// answered HERE by the engine (`wire::NoticeRequest::parse`, ruling 177), so junk
/// is never a wake; the caps, the mint budget, the paint pacing and every reply word
/// are the engine's too (`wire::apply`, rulings 169-170), run on the MAIN THREAD
/// (`Wake::Notice`). Owner-only at the socket.
pub(crate) fn cmd_notice(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let request = match aterm_messages::wire::NoticeRequest::parse(rest) {
        Ok(request) => request,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match control_media::call_main(proxy, |reply| Wake::Notice { request, reply }) {
        Ok(line) => format!("{line}\n"),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// The usage line every malformed `devtools` form answers.
pub(crate) const DEVTOOLS_USAGE: &str = "ERR usage: devtools hand-over [now]\n";

/// Parses `devtools hand-over [now]` on the control thread, so a malformed form is a usage
/// error and never a wake: `Ok(now)`. Pure.
pub(crate) fn parse_devtools(rest: &str) -> Result<bool, &'static str> {
    let mut words = rest.split_whitespace();
    let now = match (words.next(), words.next(), words.next()) {
        (Some("hand-over"), None, None) => false,
        (Some("hand-over"), Some("now"), None) => true,
        _ => return Err(DEVTOOLS_USAGE),
    };
    Ok(now)
}

/// The wire answer to one asked hand-over: `OK <word>` (`HandOver::as_str`) for whatever the
/// window decided, `ERR headless: …` on a headless instance, and `ERR` for a page that did not
/// open (an owner press's outcome; the verb never opens the page as it asks). Pure.
pub(crate) fn devtools_reply(outcome: crate::devtools_helper::HandOver) -> String {
    use crate::devtools_helper::HandOver;
    match outcome {
        HandOver::Headless => {
            "ERR headless: a headless instance brings no System Settings page up\n".to_string()
        }
        HandOver::OpenFailed => "ERR devtools: System Settings did not open\n".to_string(),
        other => format!("OK {}\n", other.as_str()),
    }
}

/// `devtools hand-over [now]` -> one status line: the window's one Developer Tools hand-over
/// (`App::hand_devtools_to_owner` with `devtools_helper::Origin::Ctl`), which `aterm doctor`
/// asks so the owner gets aterm's helper panel beside System Settings, not a bare page.
/// Owner-only at the socket, and still held to the window's own rules (Owner is every
/// in-session client): the bare form waits for a quiet moment; `now` shows only the panel and
/// leaves the page to its asker. Grammar: [`parse_devtools`]; answer: [`devtools_reply`]; the
/// hand-over runs on the main thread (`Wake::DevToolsHandOverAsked`). Off macOS, which has no
/// such list, the verb answers without waking the window.
pub(crate) fn cmd_devtools(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let now = match parse_devtools(rest) {
        Ok(now) => now,
        Err(usage) => return usage.to_string(),
    };
    if !cfg!(target_os = "macos") {
        return "ERR unsupported: the Developer Tools list exists only on macOS\n".to_string();
    }
    match control_media::call_main(proxy, |reply| Wake::DevToolsHandOverAsked { now, reply }) {
        Ok(outcome) => devtools_reply(outcome),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `title` -> `OK <window title>\n` (the OSC 0/2 title; empty if unset).
/// `title` -> `OK <window title>\n` (the OSC 0/2 window title; empty if unset).
pub(crate) fn cmd_title(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    format!("OK {}\n", t.title())
}

/// The usage line `story` answers every junk form with, spelled from
/// [`StoryVerb::TOLD_WORDS`](crate::presence::StoryVerb::TOLD_WORDS) so it and the parser agree.
pub(crate) fn story_usage() -> String {
    format!(
        "ERR usage: story <{}> [<text>]\n",
        crate::presence::StoryVerb::TOLD_WORDS.join("|")
    )
}

/// The grammar of `story <verb> [<text>]`, checked on the control thread so a
/// junk form is a usage error and never a wake: the verb is the CLOSED SET,
/// `chose` alone — the in-window supervisor answering Claude Code's question
/// box by policy (the question-policy contract v3 of 2026-09-24); the seven
/// words no process sent are usage errors since the owner surfaces
/// (2026-10-16 §7),
/// the text is optional, trimmed, at most
/// [`TOLD_TEXT_MAX_BYTES`] bytes, and carries no control byte (a newline could
/// forge a second reply line; the band would strip it, but the wire refuses
/// it first). Pure, so every refusal is unit-tested without an event loop.
pub(crate) fn parse_story(rest: &str) -> Result<(crate::presence::StoryVerb, String), String> {
    use crate::presence::{StoryVerb, TOLD_TEXT_MAX_BYTES};
    let t = rest.trim();
    let (word, text) = t
        .split_once(char::is_whitespace)
        .map_or((t, ""), |(w, x)| (w, x.trim()));
    let Some(verb) = StoryVerb::parse_told(word) else {
        return Err(story_usage());
    };
    if text.len() > TOLD_TEXT_MAX_BYTES {
        return Err(format!(
            "ERR story: text is longer than {TOLD_TEXT_MAX_BYTES} bytes\n"
        ));
    }
    if text.chars().any(char::is_control) {
        return Err("ERR story: text carries a control character\n".to_string());
    }
    Ok((verb, text.to_string()))
}

/// `story chose [<text>]` -> `OK story=<n>`: the supervisor's question choice
/// for the target session, a RECORD (`status story=`): the hosted loop's
/// `story chose <word>` is the one producer, and the seven other words no
/// process sent went with the presence row that showed them (owner surfaces,
/// 2026-10-16 §7). An Owner verb. `<n>` is the story point's seq — the number
/// `status story=` reports once it has landed, so a poller can tell its own
/// post from an older one. The grammar is [`parse_story`]'s (the text is
/// validated and reaches no surface); the point is noted on the main thread
/// (`Wake::Story`) because the slot is `App` state.
///
/// `by` is the teller the session log's `story` mark names (the wire's
/// `log_actor`).
pub(crate) fn cmd_story(
    proxy: &EventLoopProxy<Wake>,
    session: u64,
    rest: &str,
    by: aterm_slog::Actor,
) -> String {
    let verb = match parse_story(rest) {
        Ok((verb, _text)) => verb,
        Err(error) => return error,
    };
    match control_media::call_main(proxy, |reply| Wake::Story {
        session,
        verb,
        by,
        reply,
    }) {
        Ok(Ok(seq)) => format!("OK story={seq}\n"),
        Ok(Err(error)) => format!("ERR {error}\n"),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `cwd` -> `OK <working directory>\n`: the shell's directory as reported via OSC 7, empty if
/// never reported.
pub(crate) fn cmd_cwd(term: &Arc<Mutex<Terminal>>) -> String {
    use crate::cwd_native::ReportedCwd as _;
    let t = term_lock(term);
    // Answer in the host's path syntax: the engine keeps OSC 7's RFC 8089 URI path, and
    // `/C:/Users/…` is no Windows directory. `cwd_native` converts once, for this verb and
    // `meta`.
    let cwd = t.native_working_directory();
    // OSC 7 is percent-decoded, so the path can hold newlines or C0/C1/BiDi bytes; pct_encode
    // keeps it one token that cannot forge reply lines.
    format!("OK {}\n", pct_encode(cwd.as_deref().unwrap_or("")))
}

/// `text --json` -> `{"rows":["<row0>",...],"cursor":{...},"dims":{...},"seq":N,"gen":"E.S",
/// "human_ms":N|null}`.
/// The rows are the SAME grapheme-faithful, control-collapsed, tail-trimmed lines
/// `cmd_text` emits, the cursor/dims mirror the `cursor`/`dims` verbs, `seq` is the
/// engine `content_seq` (so an agent can diff frames without re-reading), and `gen`
/// is the [`ScreenGen`] of the same instant — the value a fenced press names
/// (`key if-gen=`), so a supervisor binds its press to the very read it judged
/// rather than to a `status` read taken after it.
///
/// `text --json [trim] [tail=<n>|rows=<a>-<b>]`: that object, and with `trim`
/// the `rows` array stops after the last non-blank row ([`trimmed_len`]) and the
/// object closes with `"trimmed":k` — the JSON twin of the text header's
/// `trimmed=<k>`. A shape ([`TextShape::select`]) picks which grid rows fill the
/// array, `trim` measures that slice, and when it does not start at row 0 the
/// object ends with `"first":<row>` (the twin of ` first=<row>`; after `trimmed`,
/// new fields go LAST). `dims.rows` stays the GRID's row count, so `rows.len()`
/// says what was sent, `first` where it starts and `dims` what the screen is; each
/// of those two is only written when its option was asked for. `gen` (2026-09-24,
/// harness/integrate) is the one field every reply carries, right after the `seq`
/// it qualifies: a `seq` alone repeats after an alternate-screen re-entry, and a
/// supervisor that fenced its press on the generation of a LATER `status` read
/// would press into whatever box replaced the one it judged.
///
/// The test-side spelling of [`cmd_text_json_read`] for a session no person
/// has keyed (`"human_ms":null`) and no shell has signed a command start in
/// (`"launch":null`).
#[cfg(test)]
pub(crate) fn cmd_text_json_opt(term: &Arc<Mutex<Terminal>>, args: TextArgs) -> String {
    cmd_text_json_read(term, &FrameSlot::default(), args, None, None, 0, None, None)
}

/// [`cmd_text_json_opt`] as the dispatch answers it: `human_ms` (2026-09-25)
/// is how long ago a PERSON last gave the session input
/// ([`crate::human_input`], `None` = never, `null` on the wire), written
/// right after `gen` — like `gen`, every reply carries it, and `trimmed` /
/// `first` stay last. A supervisor reads it off the very read it decides a
/// question dialog's key on, and keys nothing until a person has been quiet
/// long enough (the critique of 2026-09-25, R1). `human_seq` beside it is
/// how many person gestures have reached the session
/// ([`crate::human_input::HumanInputStamp::seq`]), read BEFORE the screen: a
/// write fenced `if-human=<it>` is refused if a person keyed since (R3b,
/// 2026-09-27). `launch` after it (2026-10-16) is the session's launch witness
/// ([`crate::launch_witness`]), read inside the same terminal-lock hold as the
/// rows and judged against that `human_seq`: `{"id","seq","word","why":"person"}`,
/// `{"why":"<word>"}` for any other word, `null` for no mark.
/// `text --json` as the session's own verb serves it: [`cmd_text_json_read`]
/// with the session's `human_ms` and `human_seq` — the count before the age,
/// both before the screen, so a person's key after this is one the fence
/// sees — and its submission count ([`crate::submit_count`], `"submits"`,
/// 2026-10-21), read AFTER the screen: a screen that shows the program's
/// answer to a submission comes with a count that holds it.
pub(crate) fn cmd_text_json_looked(
    term: &Arc<Mutex<Terminal>>,
    ctx: &crate::SessionCtx,
    args: TextArgs,
) -> String {
    let now_us = crate::metrics::now_us();
    let human_seq = ctx.human_input.seq();
    cmd_text_json_read(
        term,
        &ctx.frame,
        args,
        ctx.human_input.ms_since(now_us),
        ctx.human_input.unkeyed_ms(now_us),
        human_seq,
        Some(&ctx.launch),
        Some((&ctx.output_echo, &ctx.sink)),
    )
}

#[cfg(test)]
thread_local! {
    /// What a test does while this thread holds the terminal for a `text
    /// --json` read ([`cmd_text_json_read`]), once: an input write that lands
    /// between the counts before and after the rows, so a test sees which
    /// side of the rows each count is read on.
    static WHILE_ROWS_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// [`cmd_text_json_looked`]'s reply; `unkeyed_ms`
/// ([`crate::human_input::HumanInputStamp::unkeyed_ms`], the review of LC-4)
/// is how long the host has served a session no person keyed, written
/// `"unkeyed_ms":N` only beside a `"human_ms":null`: a never that says since
/// when. `submits`, the session's input writes, gives `"submits":N` after
/// inputs and before an optional frame marker ([`crate::app_input::OutputEchoTracker::submits`]), read
/// once the screen's hold is released, and `"inputs":[B,A]` before it: every
/// input write, submitting or not, counted before the rows with no active
/// write, partial frame or buffered spill (`B`, `null` without that proof,
/// [`crate::app_input::OutputEchoTracker::input_settled`]) and after them
/// (`A`). Equal counts exclude new input attempts and previously buffered
/// bytes, not a child's delayed handling of kernel-queued input. `None`
/// (a test's read of a bare terminal) writes neither. `"frame":"open"` comes last, and only when
/// the program's DEC 2026 frame is unfinished at the read
/// ([`Terminal::frame_unfinished_at`], read under the rows' lock): the rows
/// are a prefix of a redraw, and a reader of whole frames waits (`await
/// frame`) and reads again (`docs/measured/half-drawn-frame-2026-10-22.md`).
/// Absent, the frame is whole.
#[allow(
    clippy::too_many_arguments,
    reason = "the session's facts read beside one screen, each from its own leaf; a struct \
              would only rename them at the one dispatch that passes them"
)]
pub(crate) fn cmd_text_json_read(
    term: &Arc<Mutex<Terminal>>,
    frame: &FrameSlot,
    args: TextArgs,
    human_ms: Option<u64>,
    unkeyed_ms: Option<u64>,
    human_seq: u64,
    launch: Option<&crate::launch_witness::LaunchWitness>,
    input: Option<(&crate::app_input::OutputEchoTracker, &SinkWriter)>,
) -> String {
    // GATHER under ONE lock hold, SERIALIZE with the lock released — the shape the
    // styled frame uses. Every field is read inside the single hold, so the reply
    // describes one instant, and the PTY reader and frame snapshot never queue
    // behind the escaping.
    // Logical rows are the session's published text (built only when the screen
    // changed since the last build); `order=display` reads its own rows and maps.
    // No active write, partial frame or process-local spill at this count
    // ([`crate::app_input::OutputEchoTracker::input_settled`]). The sink
    // fence ends here, before frame/terminal locks. This is transport settlement,
    // never acknowledgment that the child processed a key.
    let inputs_before = input.map(|(tracker, sink)| tracker.input_settled(sink));
    let mut look = frame.look_frame();
    let (
        text,
        display_rows,
        maps,
        c,
        vis,
        style,
        rows,
        cols,
        seq,
        generation,
        first,
        end,
        launch,
        frame_open,
    ) = {
        let t = term_lock(term);
        #[cfg(test)]
        WHILE_ROWS_READ.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let rows = t.rows() as usize;
        let (first, end) = match args.shape.select(rows) {
            Ok(span) => span,
            Err(err) => return err,
        };
        let (text, (display_rows, maps)) = if args.display {
            let rows: (Vec<String>, Vec<Option<Vec<usize>>>) =
                (first..end).map(|r| row_in_order(&t, r, true)).unzip();
            (None, rows)
        } else {
            (Some(look.text(&t)), (Vec::new(), Vec::new()))
        };
        (
            text,
            display_rows,
            maps,
            t.cursor(),
            t.cursor_visible(),
            cursor_style_name(t.cursor_style()),
            rows,
            t.cols(),
            t.content_seq(),
            screen_gen(&t),
            first,
            end,
            // The launch lock is a leaf, taken after the terminal's.
            launch.map_or_else(|| "null".to_string(), |w| w.json(human_seq)),
            t.frame_unfinished_at(std::time::Instant::now()),
        )
    };
    // THE SUBMISSION COUNT AFTER THE SCREEN ([`crate::submit_count`]): every
    // submission whose answer the rows show is counted by now — and every
    // input write whose effect the rows show.
    let inputs_after = input.map(|(tracker, _)| tracker.input_sent());
    let submits = input.map(|(tracker, _)| tracker.submits());
    let rows_text: Vec<&str> = match &text {
        Some(text) => (first..end).map(|r| text.row(r)).collect(),
        None => display_rows.iter().map(String::as_str).collect(),
    };
    // ONE buffer, written straight through: `json_ok`'s exact `"OK 1\n"` prefix
    // and `"\n"` suffix, with each row escaped in place by `json_escape_into`.
    let selected = rows_text.len();
    let sent = if args.trim {
        trimmed_len(rows_text.iter().copied())
    } else {
        selected
    };
    let mut out = String::with_capacity(selected * (cols as usize + 8) + 128);
    out.push_str("OK 1\n");
    out.push_str("{\"rows\":[");
    for (i, row) in rows_text.iter().take(sent).enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        crate::control::json_escape_into(&mut out, row);
        out.push('"');
    }
    {
        use std::fmt::Write as _;
        let _ = write!(
            out,
            "],\"cursor\":{{\"row\":{},\"col\":{},\"visible\":{vis},{}}},\
             \"dims\":{{\"rows\":{rows},\"cols\":{cols}}},\"seq\":{seq},\"gen\":\"{generation}\",\
             \"human_ms\":{human},\"human_seq\":{human_seq},\"launch\":{launch}",
            c.row,
            c.col,
            json_str_field("style", style),
            human = human_ms.map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        );
        if args.trim {
            let _ = write!(out, ",\"trimmed\":{}", selected.saturating_sub(sent));
        }
        if first > 0 {
            let _ = write!(out, ",\"first\":{first}");
        }
        // `order=display`: `display_map[i]` is row i's visual→logical column map, `null` where
        // the order is the identity.
        if args.display {
            out.push_str(",\"order\":\"display\",\"display_map\":[");
            for (i, map) in maps.iter().take(sent).enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match map {
                    None => out.push_str("null"),
                    Some(map) => {
                        out.push('[');
                        for (j, l) in map.iter().enumerate() {
                            if j > 0 {
                                out.push(',');
                            }
                            let _ = write!(out, "{l}");
                        }
                        out.push(']');
                    }
                }
            }
            out.push(']');
        }
        if let (None, Some(ms)) = (human_ms, unkeyed_ms) {
            let _ = write!(out, ",\"unkeyed_ms\":{ms}");
        }
        if let (Some(before), Some(after)) = (inputs_before, inputs_after) {
            let before = before.map_or_else(|| "null".to_string(), |n| n.to_string());
            let _ = write!(out, ",\"inputs\":[{before},{after}]");
        }
        if let Some(n) = submits {
            let _ = write!(out, ",\"submits\":{n}");
        }
        if frame_open {
            out.push_str(",\"frame\":\"open\"");
        }
        out.push('}');
    }
    out.push('\n');
    out
}

/// `cursor --json` -> `{"row":R,"col":C,"visible":bool,"style":"<name>"}`.
pub(crate) fn cmd_cursor_json(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let c = t.cursor();
    json_ok(&format!(
        "{{\"row\":{},\"col\":{},\"visible\":{},{}}}",
        c.row,
        c.col,
        t.cursor_visible(),
        json_str_field("style", cursor_style_name(t.cursor_style())),
    ))
}

/// The wire name of an [`UnderlineStyle`]: lowercase, matching the SGR 4:x family.
fn underline_style_name(u: UnderlineStyle) -> &'static str {
    match u {
        UnderlineStyle::None => "none",
        UnderlineStyle::Single => "single",
        UnderlineStyle::Double => "double",
        UnderlineStyle::Curly => "curly",
        UnderlineStyle::Dotted => "dotted",
        UnderlineStyle::Dashed => "dashed",
    }
}

/// A JSON boolean without the formatter (four per cell).
fn push_bool(out: &mut String, v: bool) {
    out.push_str(if v { "true" } else { "false" });
}

/// Serialize ONE cell as the canonical `StyledCell` JSON object — the LOSSLESS,
/// fully-resolved view a styled-screen consumer (an outer agent driving an inner
/// TUI) needs. Every rendition field is read from the RESOLVED [`RenderCell`]
/// (the renderer's own decisions: palette/RGB/bold-bright/dim/inverse/hidden/
/// DECSCNM already folded into `fg`/`bg`), NOT the raw flag bits — so this carries
/// the four decorations the legacy `cell` verb dropped (underline SUBSTYLE,
/// overline, underline colour, emoji presentation). `glyph` is the combining-aware
/// grapheme (same source as `cell`/`text`); `wide_lead` is the only geometry field
/// (the raw `WIDE` flag), kept distinct from the `wide` right-half continuation.
///
/// NOTE on semantic boundary: `dim`/`inverse`/`hidden` are baked into the
/// resolved `fg`/`bg` by `render_row` and are deliberately NOT reported as attrs
/// (recovering them is the raw-flags path; byte-exact SGR replay is the `cast`
/// raw-bytes channel's job, not this resolved-screen view). `blink` is not a
/// colour, and `render_row` folds it into nothing, so it IS an attr, read off the
/// raw `BLINK` flag as `wide_lead` is off `WIDE`: without it the frame lost the
/// blink `cell` reported on the same cell (measured 2026-10-17, the inbox console
/// host's `ESC[5m`).
///
/// The hot loop of the introspection surface (`screen` and every `subscribe … cells` push run
/// it per cell, ~15 000 per snapshot on a large window), so it allocates nothing per cell.
fn write_styled_cell_json(
    out: &mut String,
    snap: &StyledCellSnap,
    glyph: &str,
    links: &[StyledLink],
) {
    let cell = &snap.cell;
    out.push_str("{\"glyph\":\"");
    crate::control::json_escape_into(out, glyph);
    out.push_str("\",\"fg\":\"");
    aterm_codec::hex::push_lower(out, &cell.fg);
    out.push_str("\",\"bg\":\"");
    aterm_codec::hex::push_lower(out, &cell.bg);
    out.push_str("\",\"attrs\":[");
    // The attribute list, comma-joined in place.
    let mut first = true;
    let mut attr = |out: &mut String, name: &str| {
        if !first {
            out.push(',');
        }
        first = false;
        out.push('"');
        out.push_str(name);
        out.push('"');
    };
    if cell.bold {
        attr(out, "bold");
    }
    if cell.italic {
        attr(out, "italic");
    }
    if cell.underline != UnderlineStyle::None {
        attr(out, "underline");
    }
    if snap.blink {
        attr(out, "blink");
    }
    if cell.strikethrough {
        attr(out, "strike");
    }
    out.push_str("],\"underline_style\":\"");
    out.push_str(underline_style_name(cell.underline));
    out.push_str("\",\"overline\":");
    push_bool(out, cell.overline);
    out.push_str(",\"underline_color\":");
    match cell.underline_color {
        Some(rgb) => {
            out.push('"');
            aterm_codec::hex::push_lower(out, &rgb);
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"emoji_presentation\":");
    push_bool(out, cell.emoji_presentation);
    out.push_str(",\"text_presentation\":");
    push_bool(out, cell.text_presentation);
    out.push_str(",\"wide\":");
    push_bool(out, cell.wide);
    out.push_str(",\"wide_lead\":");
    push_bool(out, snap.wide_lead);
    // One lookup serves both link keys: the interned entry is the cell's
    // `(hyperlink(), hyperlink_id())` pair, `NO_LINK` covers both `None` cases (no extra, or
    // one with no link), and `id` cannot outlive `url`, so one nullable entry is lossless.
    let link = links.get(snap.link as usize);
    out.push_str(",\"hyperlink\":");
    match link.map(|entry| &*entry.url) {
        Some(u) => {
            out.push('"');
            crate::control::json_escape_into(out, u);
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"hyperlink_id\":");
    match link.and_then(|entry| entry.id.as_deref()) {
        Some(i) => {
            out.push('"');
            crate::control::json_escape_into(out, i);
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push('}');
}

/// The wire name of a DEC [`LineSize`](aterm_core::grid::LineSize): the renderer
/// scales these rows, so a lossless frame must carry them.
pub(crate) fn line_size_name(s: aterm_core::grid::LineSize) -> &'static str {
    use aterm_core::grid::LineSize;
    match s {
        LineSize::SingleWidth => "single",
        LineSize::DoubleWidth => "double_width",
        LineSize::DoubleHeightTop => "double_height_top",
        LineSize::DoubleHeightBottom => "double_height_bottom",
        _ => "single",
    }
}

/// The DEC line size of live screen row `r` (default single-width), whatever
/// the viewport shows: the row the styled frame's cells are read from.
pub(crate) fn row_line_size(t: &Terminal, r: usize) -> aterm_core::grid::LineSize {
    u16::try_from(r)
        .ok()
        .and_then(|rr| t.grid().row_at_screen(rr))
        .map_or(aterm_core::grid::LineSize::SingleWidth, |row| {
            row.line_size()
        })
}

/// Every DISTINCT inline image on the live screen, each at its top-left grid
/// anchor `(row, col)` (deduplicated by payload identity), whatever the
/// viewport shows. Shared shape with `cmd_image_read`'s screen mode; consumed
/// by the styled frame so a `subscribe cells` / `screen`
/// watcher sees images, not blank cells, on the rows its cells describe.
///
/// ROW-MAJOR, ascending column within a row: a socket client indexes into this
/// array, so the order is part of the wire contract
/// (`distinct_images_are_row_major_regardless_of_placement_order` pins it) and
/// NOT free to follow the extras map, whose `FxHashMap` iteration order is
/// arbitrary and is not insertion order either.
///
/// [`live_images_frame`](Terminal::live_images_frame) is the batch reader: ONE pass over
/// the extras map, each row sorted by column, instead of the `rows x cols`
/// `cell_extra` probes the per-row accessor costs. Its emptiness gate only
/// rescues a screen with NO extras at all, so before this every gather of a
/// screen carrying a single hyperlink — an `ls --hyperlink` listing, a TUI
/// status line — swept the whole grid looking for pictures that were not there.
///
/// Row-major, ascending column within a row: clients index this array, so the order is wire
/// contract (`distinct_images_are_row_major_regardless_of_placement_order`), not the extras
/// map's arbitrary order. [`images_frame`](Terminal::images_frame) reads it in one pass over
/// the extras map. The extras store's `any_image()` is no gate: a Kitty Unicode placeholder
/// shows a picture without attaching an image ref to any cell.
fn distinct_images(t: &Terminal) -> Vec<(usize, usize, std::sync::Arc<ImageData>)> {
    // Dedup by identity set: a tiled image places one `ImageRef` per covered cell.
    let mut seen: std::collections::HashSet<*const ImageData> = std::collections::HashSet::new();
    let mut out: Vec<(usize, usize, std::sync::Arc<ImageData>)> = Vec::new();
    for (r, row) in t
        .live_images_frame(t.rows() as usize)
        .into_iter()
        .enumerate()
    {
        for (col, iref) in row {
            if !seen.insert(std::sync::Arc::as_ptr(&iref.image)) {
                continue;
            }
            let anchor_r = r.saturating_sub(iref.cell_row as usize);
            let anchor_c = col.saturating_sub(iref.cell_col as usize);
            out.push((anchor_r, anchor_c, iref.image.clone()));
        }
    }
    out
}

/// Append one inline image as a JSON object for the styled frame: anchor grid
/// position, cell footprint, format, raw byte length, and the base64 payload —
/// so a watcher reconstructs the picture the human sees, independent of the GUI
/// framebuffer. `payload` is the image's ([`image_payloads`]; F4: an oversized
/// image is `truncated` with an empty payload), and its base64 is pushed
/// straight into `out`, never formatted through a temporary.
pub(crate) fn styled_image_json(
    out: &mut String,
    anchor_r: usize,
    anchor_c: usize,
    img: &ImageData,
    payload: &ImagePayload,
) {
    use std::fmt::Write as _;
    let _ = write!(
        out,
        "{{\"row\":{anchor_r},\"col\":{anchor_c},\"cols\":{},\"rows\":{},\"format\":\"{}\",\
         \"nbytes\":{},\"b64\":\"",
        img.cols,
        img.rows,
        payload.format(),
        img.bytes.len(),
    );
    out.push_str(payload.b64());
    out.push_str("\"}");
}

/// Bytes a styled image object takes besides its base64.
const STYLED_IMAGE_BYTES: usize = 128;

/// Sentinel [`StyledCellSnap::link`] for a cell with no OSC 8 link. `u32::MAX`
/// is never a valid index into a table bounded by the grid's extras population,
/// and `slice::get` turns it into the `None` the serializer wants.
const NO_LINK: u32 = u32::MAX;

/// One distinct OSC 8 link on the frame, as `Arc` clones of the grid's shared url and id, so
/// the gather copies nothing under the lock. Interned by pointer identity, which errs safe:
/// equal bytes in two `Arc`s become two rows that serialize alike, and two urls never share an
/// address.
struct StyledLink {
    url: std::sync::Arc<str>,
    /// The OSC 8 `id=` grouping key, if any: adjacent runs with the same url and different ids
    /// are distinct clickable regions, which the renderer groups by id.
    id: Option<std::sync::Arc<str>>,
}

/// One cell's raw material for [`serialize_styled_frame`]: the resolved
/// [`RenderCell`] plus the four grid-side fields it does not carry — the
/// combining-aware grapheme, the raw `WIDE` lead flag (the live cell's own flag
/// bit, NOT the resolved `RenderCell::wide`), the raw `BLINK` flag, and the OSC 8
/// hyperlink target. All copied under the terminal lock; serialized without it.
/// It owns no heap: the grapheme lives in its row's buffer and the link in the
/// frame's table.
struct StyledCellSnap {
    cell: RenderCell,
    /// Byte range of this cell's grapheme in its row's [`StyledRowSnap::glyphs`], read back
    /// through [`StyledRowSnap::glyph`].
    glyph: std::ops::Range<usize>,
    wide_lead: bool,
    /// SGR 5/6, which [`RenderCell`] does not carry.
    blink: bool,
    /// Index into the frame's [`StyledFrameSnapshot::links`] table, or
    /// [`NO_LINK`].
    link: u32,
}

/// One row's cells plus the one buffer their graphemes live in: one allocation per row, not
/// one per cell under the terminal lock (a materialized blank's grapheme is a space, so even
/// blanks allocated). The buffer lives inside the row and [`StyledRowSnap::glyph`] takes a
/// column, so a range cannot be paired with the wrong row.
struct StyledRowSnap {
    /// Every cell's grapheme in this row, concatenated in column order.
    glyphs: String,
    cells: Vec<StyledCellSnap>,
}

impl StyledRowSnap {
    /// Column `col`'s grapheme. Takes the column, not a cell, so a cell from another row cannot
    /// be paired with this buffer. Indexes rather than `get`s: the gather hands out the ranges
    /// as it appends, so an out-of-range one is a defect that must not hide as an empty glyph.
    fn glyph(&self, col: usize) -> &str {
        &self.glyphs[self.cells[col].glyph.clone()]
    }
}

/// Side-adjusted selection geometry for the styled control frame: `TextSelection::project_range`
/// plus the kind (a block and a linear selection can share endpoints yet paint different
/// cells). It is the logical span, not the paint: `project_range` sees anchors only, so it
/// cannot apply the widening a double-width glyph forces (`glyph_cell_span`), and an edge on
/// half such a glyph paints one column outside `start_col`/`end_col`. A watcher that needs the
/// painted cells reads `cells` and their `wide_lead` bits.
struct StyledSelectionSnap {
    start_row: i32,
    start_col: u16,
    end_row: i32,
    end_col: u16,
    kind: &'static str,
    is_block: bool,
}

/// Everything the styled JSON frame serializes, copied out of the [`Terminal`] under one lock
/// acquisition so seq, cursor, cells, images, selection and line sizes describe one instant.
/// Serialization (per-cell output, base64) runs with the lock released.
pub(crate) struct StyledFrameSnapshot {
    seq: u64,
    rows: usize,
    cols: usize,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    cursor_style: &'static str,
    /// OSC 12 cursor colour, or `None` for the renderer/theme default.
    cursor_color: Option<[u8; 3]>,
    /// `rows × cols` cells, blank-padded to the full grid width (no trim).
    cells: Vec<StyledRowSnap>,
    /// The frame's distinct OSC 8 links, `Arc`-cloned from the grid. Cells index into this by
    /// [`StyledCellSnap::link`].
    links: Vec<StyledLink>,
    line_sizes: Vec<&'static str>,
    /// The selection's logical span and kind ([`StyledSelectionSnap`] says where it parts
    /// company with the painted cells), or `None`.
    selection: Option<StyledSelectionSnap>,
    /// OSC 17 selection fill, or `None` for the renderer/theme default.
    selection_bg: Option<[u8; 3]>,
    /// OSC 19 selected-text ink, or `None` for automatic contrast.
    selection_fg: Option<[u8; 3]>,
    /// Distinct inline images as `Arc` clones; base64 happens at serialize time.
    images: Vec<(usize, usize, std::sync::Arc<ImageData>)>,
    /// Whether the images carry their payloads. `screen` clears it for a frame that
    /// would pass the reply-line cap: each image is then `truncated` with its real
    /// `nbytes`, and `image read` serves the bytes one per line.
    image_payloads: bool,
}

impl StyledFrameSnapshot {
    /// Whether the frame serializes an image payload: it holds an image and
    /// [`Self::image_payloads`] stands.
    pub(crate) fn carries_image_payloads(&self) -> bool {
        self.image_payloads && !self.images.is_empty()
    }

    /// Cells in the gathered frame — `rows × cols` by the lossless no-trim
    /// contract. The serializer's reserve input.
    pub(crate) fn cell_count(&self) -> usize {
        self.cells.iter().map(|row| row.cells.len()).sum()
    }
}

#[cfg(test)]
impl StyledFrameSnapshot {
    /// `(row, col)`'s grapheme read exactly as the serializer reads it — the
    /// test-facing spelling of the per-row buffer + per-cell range pair.
    fn glyph(&self, row: usize, col: usize) -> &str {
        self.cells[row].glyph(col)
    }
}

/// Stable wire name for one selection kind.
fn selection_type_name(kind: SelectionType) -> &'static str {
    match kind {
        SelectionType::Simple => "simple",
        SelectionType::Block => "block",
        SelectionType::Semantic => "semantic",
        SelectionType::Lines => "lines",
        // `SelectionType` is non-exhaustive. A future kind must remain linear
        // until the selection engine exposes different projection semantics.
        _ => "linear",
    }
}

/// Serialize one optional RGB policy as a JSON string. Fixed colors use the same
/// lowercase `rrggbb` form as per-cell colors; the fallback token names the
/// renderer policy rather than inventing a color the terminal does not own.
fn styled_optional_rgb(color: Option<[u8; 3]>, fallback: &str) -> String {
    color.map_or_else(
        || format!("\"{fallback}\""),
        |[r, g, b]| format!("\"{r:02x}{g:02x}{b:02x}\""),
    )
}

/// Serialize the side-adjusted selection geometry, the four coordinate keys first.
fn styled_selection_json(selection: Option<&StyledSelectionSnap>) -> String {
    selection.map_or_else(
        || "null".to_string(),
        |sel| {
            format!(
                "{{\"start_row\":{},\"start_col\":{},\"end_row\":{},\"end_col\":{},\
                 \"kind\":\"{}\",\"is_block\":{}}}",
                sel.start_row, sel.start_col, sel.end_row, sel.end_col, sel.kind, sel.is_block,
            )
        },
    )
}

/// Every OSC 8 link on the visible grid: the deduplicated `(url, id)` table plus, per row, the
/// ascending `(col, table index)` pairs the gather merges against its cell loop.
pub(crate) struct FrameLinks {
    /// The distinct links, in first-seen order. Cell records index into this.
    table: Vec<StyledLink>,
    /// Per visible row, the linked columns ascending as `(col, table index)`.
    /// Empty (and allocation-free) for every row with no link.
    by_row: Vec<Vec<(u16, u32)>>,
}

/// Build the frame's [`FrameLinks`] in ONE pass over the extras map.
///
/// WHY A MAP WALK. `Grid::cell_extra(r, c)` is `CellExtras::get` — a HASH probe.
/// The retired gather ran one per cell, gated only on the map being globally
/// EMPTY, so a SINGLE hyperlink (or coloured underline, or combining mark)
/// anywhere on screen armed a full rows x cols hash sweep on every `screen` read
/// and every `subscribe cells` push, with the terminal mutex held. Modern CLI
/// output (git, cargo, eza, gh) emits OSC 8 constantly, so "extras non-empty" is
/// the normal state. Iterating the map is proportional to the extras actually
/// present, and a screen of extras with no link yields empty rows, where the
/// per-cell cost becomes one compare.
///
/// EXACTNESS. `CellExtras::iter()` yields EXTERNAL coordinates with stale
/// (scrolled-off) entries already filtered — the same view `get` resolves to,
/// and the shape `Terminal::live_images_frame` already ships for image placements.
/// Entries are bounds-checked against BOTH the caller's `rows`/`cols` and the
/// grid's own extent (they can differ mid-resize), exactly as the retired
/// `0..rows` / `0..cols` loops dropped them, and each row is sorted afterwards
/// because map iteration order is arbitrary while the merge cursor needs
/// ascending columns. `styled_frame_fusion_parity` holds the result to the
/// retired per-cell probe.
pub(crate) fn gather_frame_links(t: &Terminal, rows: usize, cols: usize) -> FrameLinks {
    let grid = t.grid();
    let extras = grid.extras();
    // With no extras anywhere there is provably no link. `t` is borrowed
    // immutably for the gather, so the map cannot change under it.
    if extras.is_empty() {
        return FrameLinks {
            table: Vec::new(),
            by_row: Vec::new(),
        };
    }
    let mut table: Vec<StyledLink> = Vec::new();
    let mut by_row: Vec<Vec<(u16, u32)>> = Vec::new();
    by_row.resize_with(rows, Vec::new);
    // Pointer-identity intern: an OSC 8 run shares one `Arc<str>` across every
    // cell it covers, so a whole run collapses to one table row. Keyed on the
    // `str` data addresses of url and id: two live `Arc<str>`s never share one.
    let mut seen: std::collections::HashMap<(*const u8, *const u8), u32> =
        std::collections::HashMap::new();
    let max_row = rows.min(usize::from(grid.rows()));
    let max_col = u16::try_from(cols).unwrap_or(u16::MAX).min(grid.cols());
    for (coord, extra) in extras.iter() {
        let r = usize::from(coord.row);
        if r >= max_row || coord.col >= max_col {
            continue;
        }
        let Some(url) = extra.hyperlink() else {
            continue;
        };
        let id = extra.hyperlink_id();
        let key = (url.as_ptr(), id.map_or(std::ptr::null(), |i| i.as_ptr()));
        let idx = *seen.entry(key).or_insert_with(|| {
            // A table past `u32` would alias the sentinel; overflow degrades the cell to no
            // link rather than mislabelling it.
            let idx = u32::try_from(table.len()).unwrap_or(NO_LINK);
            table.push(StyledLink {
                url: std::sync::Arc::clone(url),
                id: id.map(std::sync::Arc::clone),
            });
            idx
        });
        if idx != NO_LINK {
            by_row[r].push((coord.col, idx));
        }
    }
    for row in &mut by_row {
        row.sort_unstable_by_key(|&(col, _)| col);
    }
    FrameLinks { table, by_row }
}

/// Gather the styled frame's raw material. Called with the `Terminal` lock HELD
/// (one acquisition covers every field). Every field is read off the LIVE
/// screen, whatever the viewport shows: cells, glyphs, wide leads and links
/// (per row, below), line sizes ([`row_line_size`]) and images
/// ([`distinct_images`]). The blank-tail fallback mirrors
/// [`cmd_cell`] exactly: rows are padded to the FULL grid width with the
/// terminal's effective implicit blank (live defaults with DECSCNM folded in;
/// NO `trim_end`, unlike `text`) — the lossless `dims.rows × dims.cols`
/// contract.
pub(crate) fn gather_styled_frame(t: &Terminal) -> StyledFrameSnapshot {
    #[cfg(test)]
    crate::work_counts::styled_frame_gathered();
    let rows = t.rows() as usize;
    let cols = t.cols() as usize;
    let blank = t.implicit_blank_render_cell();
    let mut cells: Vec<StyledRowSnap> = Vec::with_capacity(rows);
    let mut line_sizes: Vec<&'static str> = Vec::with_capacity(rows);
    // One walk of the extras map for every OSC 8 link on screen ([`gather_frame_links`]).
    let FrameLinks {
        table: links,
        by_row: links_by_row,
    } = gather_frame_links(t, rows, cols);
    // One row buffer reused across rows, not a `Vec` per row under this lock.
    let mut rendered: Vec<RenderCell> = Vec::with_capacity(cols);
    for r in 0..rows {
        // LIVE frame (offset-INDEPENDENT): colours+attrs from
        // `render_row_at_screen_into`, glyph and wide-lead from the live row
        // (`RenderCell::wide` means the RIGHT-HALF continuation), and the links
        // from the offset-blind extras map (what `hyperlink_at`/`hyperlink_id_at`
        // read) — so all four reads share one live frame and never stitch a
        // scrolled-back row's colours onto a live glyph.
        t.render_row_at_screen_into(r, &mut rendered);
        // ONE live-row handle for the whole row. `screen_row_view` is the
        // offset-INDEPENDENT twin of `visible_row_view`, resolved through the same
        // `row_at_screen` as `cell_grapheme`, so the glyph and wide-lead reads below
        // resolve the row once per row instead of once per column.
        let view = t.grid().screen_row_view(r as u16);
        let mut row_cells: Vec<StyledCellSnap> = Vec::with_capacity(cols);
        // One grapheme buffer per row, each cell a byte range into it ([`StyledRowSnap`]);
        // `cols` bytes is exact for ASCII and a lower bound otherwise.
        let mut glyphs = String::with_capacity(cols);
        // Monotone merge cursor over this row's ascending link columns: one `u16` compare per
        // cell, nothing for a row with no link.
        let row_links: &[(u16, u32)] = links_by_row.get(r).map_or(&[][..], Vec::as_slice);
        let mut li = 0usize;
        // Whether the cell left of this column is a WIDE lead (false at col 0).
        let mut prev_wide = false;
        for c in 0..cols {
            let rc = rendered.get(c).copied();
            let col = c as u16;
            while li < row_links.len() && row_links[li].0 < col {
                li += 1;
            }
            let link = match row_links.get(li) {
                Some(&(lc, idx)) if lc == col => idx,
                _ => NO_LINK,
            };
            // ONE read of the live cell per column. Its raw WIDE bit is the wide
            // lead and its raw BLINK bit `cell`'s `blink`, off the cell's own
            // inline flags; a missing cell is `false` for both. The cell is a
            // continuation when bit 10 is set on a cell that is not WIDE
            // and its left neighbour is a WIDE lead: `Row::is_cell_wide_continuation`'s
            // rule, which keeps a DECSCA-protected cell (PROTECTED shares bit 10) as
            // text. Otherwise its text comes off the same held row
            // (`VisibleRowView::push_cell_text`). Byte-for-byte
            // `cell_grapheme(r, c).unwrap_or_default()`, as a range into the row
            // buffer: a continuation or a missing cell appends nothing, a blank a
            // space. `styled_frame_fusion_parity` holds it to that reference.
            let cell = view.cell(col);
            let wide_lead = cell.is_some_and(|cell| cell.is_wide());
            let continuation =
                prev_wide && !wide_lead && cell.is_some_and(|cell| cell.is_wide_continuation());
            prev_wide = wide_lead;
            let glyph_start = glyphs.len();
            if let Some(cell) = cell
                && !continuation
            {
                view.push_cell_text(col, cell, &mut glyphs);
            }
            row_cells.push(StyledCellSnap {
                cell: rc.unwrap_or(blank),
                glyph: glyph_start..glyphs.len(),
                wide_lead,
                blink: cell.is_some_and(|cell| cell.flags().contains(CellFlags::BLINK)),
                link,
            });
        }
        cells.push(StyledRowSnap {
            glyphs,
            cells: row_cells,
        });
        line_sizes.push(line_size_name(row_line_size(t, r))); // DEC double-width/height
    }
    // `project_range` applies the renderer's half-cell side adjustment and line expansion; the
    // kind keeps block and linear apart when endpoints match. The logical span only (see
    // [`StyledSelectionSnap`]).
    let sel = t.text_selection();
    let selection = sel
        .project_range(t.cols().saturating_sub(1))
        .map(|projected| StyledSelectionSnap {
            start_row: projected.start_row,
            start_col: projected.start_col,
            end_row: projected.end_row,
            end_col: projected.end_col,
            kind: selection_type_name(sel.selection_type()),
            is_block: projected.is_block,
        });
    let cur = t.cursor();
    StyledFrameSnapshot {
        seq: t.content_seq(),
        rows,
        cols,
        cursor_row: cur.row,
        cursor_col: cur.col,
        cursor_visible: t.cursor_visible(),
        cursor_style: cursor_style_name(t.cursor_style()),
        cursor_color: t.cursor_color().map(|color| [color.r, color.g, color.b]),
        cells,
        links,
        line_sizes,
        selection,
        selection_bg: t
            .selection_background()
            .map(|color| [color.r, color.g, color.b]),
        selection_fg: t
            .selection_foreground()
            .map(|color| [color.r, color.g, color.b]),
        // `Arc` clones only; the base64 encode (up to MiBs per image) runs off the lock.
        images: distinct_images(t),
        image_payloads: true,
    }
}

/// Serialize a gathered [`StyledFrameSnapshot`] to the single-line JSON frame, lock-free: the
/// snapshot owns everything it reads.
pub(crate) fn serialize_styled_frame(snap: &StyledFrameSnapshot) -> String {
    let payloads = styled_frame_payloads(snap);
    let mut out = String::with_capacity(styled_frame_estimate(snap, &payloads));
    write_styled_frame(&mut out, snap, &payloads);
    out
}

/// The wire payload of each image on the frame, in [`StyledFrameSnapshot::images`]
/// order: the base64 the process's payload memo holds, encoded only for an
/// image it does not; every one `truncated` (its real `nbytes` kept) when the
/// snapshot carries no payloads ([`StyledFrameSnapshot::image_payloads`]).
fn styled_frame_payloads(snap: &StyledFrameSnapshot) -> Vec<ImagePayload> {
    if !snap.image_payloads {
        return vec![ImagePayload::TRUNCATED; snap.images.len()];
    }
    let images: Vec<&Arc<ImageData>> = snap.images.iter().map(|(_, _, img)| img).collect();
    image_payloads(&images)
}

/// ~180 bytes/cell is the measured shape of an ordinary cell, and each image
/// adds its base64; a short read is just one realloc, never wrong.
fn styled_frame_estimate(snap: &StyledFrameSnapshot, payloads: &[ImagePayload]) -> usize {
    256 + snap.cell_count() * 180
        + payloads
            .iter()
            .map(|payload| STYLED_IMAGE_BYTES + payload.b64().len())
            .sum::<usize>()
}

/// Append [`serialize_styled_frame`]'s frame to `out`, so `screen` writes its
/// `OK 1\n` framing and the frame into ONE buffer. `payloads` are
/// [`styled_frame_payloads`]'s, one per image.
fn write_styled_frame(out: &mut String, snap: &StyledFrameSnapshot, payloads: &[ImagePayload]) {
    use std::fmt::Write as _;
    // ONE buffer for the whole frame — now literally one. The retired shape
    // built a `String` per cell, joined those into a `String` per row, then
    // joined the rows into the frame; the intermediate `rows_json` that survived
    // that cleanup still reserved a SECOND full-size buffer (~180 bytes/cell)
    // and memcpy'd the entire rows payload into `out` at the end — 864 KB
    // allocated twice and copied once on a 120x40 grid, per pushed frame.
    // The caller reserves up front and this writes straight through.
    // Every row-INDEPENDENT field is built first so the header, the streamed
    // rows, and the tail can be emitted in that order into the single buffer.
    // These are all tiny (a few bytes to a few hundred) and order-independent
    // pure functions of the snapshot; only the rows payload is large.
    // (`line_sizes` and the images are the exception: they stream straight
    // into the tail.)
    // Live OVERLAY state (OSC 12/17/19): render-time colour policy a watcher
    // cannot derive from the cells, so the frame names the policy explicitly
    // rather than letting a consumer invent a gray.
    let cursor_color = styled_optional_rgb(snap.cursor_color, "default");
    let selection_json = styled_selection_json(snap.selection.as_ref());
    let selection_bg = styled_optional_rgb(snap.selection_bg, "default");
    let selection_fg = styled_optional_rgb(snap.selection_fg, "dynamic");
    let _ = write!(
        out,
        "{{\"seq\":{},\"dims\":{{\"rows\":{},\"cols\":{}}},\
         \"cursor\":{{\"row\":{},\"col\":{},\"visible\":{},{},\"color\":{cursor_color}}},\
         \"rows\":[",
        snap.seq,
        snap.rows,
        snap.cols,
        snap.cursor_row,
        snap.cursor_col,
        snap.cursor_visible,
        json_str_field("style", snap.cursor_style),
    );
    for (r, row) in snap.cells.iter().enumerate() {
        if r > 0 {
            out.push(',');
        }
        out.push('[');
        for (c, cell) in row.cells.iter().enumerate() {
            if c > 0 {
                out.push(',');
            }
            write_styled_cell_json(out, cell, row.glyph(c), &snap.links);
        }
        out.push(']');
    }
    out.push_str("],\"line_sizes\":[");
    for (i, s) in snap.line_sizes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(s);
        out.push('"');
    }
    let _ = write!(
        out,
        "],\"selection\":{selection_json},\
         \"selection_bg\":{selection_bg},\"selection_fg\":{selection_fg},\
         \"images\":[",
    );
    // F1: inline images (OSC 1337), one object per distinct image at its
    // anchor — without this a `cells`/`screen` watcher sees blank cells where
    // the human sees a picture. Empty array (cheap) on the common no-image
    // screen.
    debug_assert_eq!(payloads.len(), snap.images.len(), "one payload per image");
    for (i, ((ar, ac, img), payload)) in snap.images.iter().zip(payloads).enumerate() {
        if i > 0 {
            out.push(',');
        }
        styled_image_json(out, *ar, *ac, img, payload);
    }
    out.push_str("]}");
}

/// The styled frame (`{"seq":N,"dims":{...},"cursor":{...},"rows":[[StyledCell,...],...]}`)
/// gathered and serialized in one call, for the tests; shipping callers split the two so
/// serialization never holds the `Terminal` mutex.
#[cfg(test)]
pub(crate) fn styled_frame_payload(t: &Terminal) -> String {
    serialize_styled_frame(&gather_styled_frame(t))
}

/// Compile gate: every field of the renderer's input is a conscious decision, carried by the
/// lossless styled frame or omitted with a reason. Destructuring
/// [`RenderInput`](aterm_core::render::RenderInput) without `..` makes a new renderer field a
/// build error until someone decides. Never called; it exists to type-check.
fn _styled_frame_covers_every_render_input_field(ri: &aterm_core::render::RenderInput) {
    let aterm_core::render::RenderInput {
        rows: _,                         // frame "dims.rows"
        cols: _,                         // frame "dims.cols"
        cells: _,                        // frame "rows" (per-cell write_styled_cell_json)
        cursor_row: _,                   // frame "cursor.row"
        cursor_col: _,                   // frame "cursor.col"
        cursor_visible: _,               // frame "cursor.visible"
        cursor_style: _,                 // frame "cursor.style"
        cursor_effect_style_override: _, // OMITTED: host/effect-owned render shape; cursor.style remains terminal DECSCUSR
        cursor_trail: _, // OMITTED: host-owned cursor-trail overlay cells (render bling), not engine cell content
        cursor_trail_color: _, // OMITTED: host-resolved cursor-trail tint, host-owned
        cursor_glow_add: _, // OMITTED: host-owned LUMEN aurora light quads, not engine cell content
        glow_halo: _, // OMITTED: host-owned GLOW-HALO radial cursor-effect light quads, not engine cell content
        glow_under: _, // OMITTED: host-owned EMBERFORGE under-glyph flame-body light quads, not engine cell content
        fire_patch: _, // OMITTED: host-owned EMBERFORGE per-pixel fire-field patches (render bling), not engine cell content
        cursor_glow_add_damage: _, // OMITTED: producer-side dirty-row revision for host-owned LUMEN quads, not engine cell content
        glow_halo_damage: _, // OMITTED: producer-side dirty-row revision for host-owned radial halos, not engine cell content
        glow_under_damage: _, // OMITTED: producer-side dirty-row revision for host-owned flame-body quads, not engine cell content
        fire_patch_damage: _, // OMITTED: producer-side dirty-row revision for host-owned fire patches, not engine cell content
        cursor_fill_override: _, // OMITTED: host-owned rainbow-cursor block fill, not engine cell content
        word_decorations: _, // OMITTED: host-owned sparkle-word decorations, not engine cell content
        ink: _, // OMITTED: host-owned animated-ink fg overrides (render bling), not engine cell content
        char_fg: _, // OMITTED: host-owned EMBERFORGE charred glyph-ink fg overrides (render bling), not engine cell content
        fire_halo: _, // OMITTED: host-owned EMBERFORGE fire contrast-halo strengths (render bling), not engine cell content
        cat_quads: _, // OMITTED: host-owned peeking-cat sprite quads (render bling), not engine cell content
        cat_atlas: _, // OMITTED: host-owned peeking-cat sprite atlas (texture), not engine cell content
        nova_add: _, // OMITTED: host-owned supernova additive-light quads (render bling), not engine cell content
        free_sprites: _, // OMITTED: host-owned free-overlay sprites (render bling; spliced into styled captures like cat_quads), not engine cell content
        free_atlas: _, // OMITTED: host-owned free-overlay atlas (texture), not engine cell content
        rain_quads: _, // OMITTED: host-owned PHOSPHOR rain sprite quads (render bling), not engine cell content
        rain_atlas: _, // OMITTED: host-owned PHOSPHOR rain glyph atlas (texture), not engine cell content
        rain_add: _, // OMITTED: host-owned PHOSPHOR rain additive halos (render bling), not engine cell content
        fx_layers: _, // OMITTED: host-owned retained fx layers (the engine's sprite/bitmap primitive), not engine cell content
        fx_occluders: _, // OMITTED: host chrome rects subtracted from the fx layers, not engine cell content
        fx_prefetch: _,  // OMITTED: GPU residency hints for fx arts; draw nothing
        display_offset: _, // OMITTED: viewport scroll position, not visible-cell content
        base_y: _, // OMITTED: host-consumed re-anchor metadata (absolute row of the top line), not visible-cell content
        absolute_row_revision: _, // OMITTED: host-consumed absolute-row coordinate-space revision, not visible-cell content
        history_renumber_epoch: _, // OMITTED: host-consumed wholesale-renumber epoch (its twin), not visible-cell content
        holder: _, // OMITTED: host-consumed holder fence (which rows a card may be decorated on), not visible-cell content
        scroll_frac_px: _, // OMITTED: sub-row present translate, display-only
        grid_top_row: _, // OMITTED: grid/chrome partition, display-only
        grid_bot_row: _, // OMITTED: grid/chrome partition, display-only
        apron_row: _, // OMITTED: incoming-row apron, the present translate's strip source
        apron_stamp: _, // OMITTED: the apron's extraction stamp, display-only bookkeeping
        fx_clip: _, // OMITTED: focused-pane present-time post-fx clip box, display-only
        selection: _, // frame "selection" kind + projected geometry
        selection_clip: _, // OMITTED: host-only split composition bounds; engine styled frames have none
        selection_bg: _, // frame "selection_bg" (OSC 17 fixed RGB, else the "default" policy token)
        selection_fg: _, // frame "selection_fg" (OSC 19 fixed RGB, else the "dynamic" auto-contrast token)
        selections: _,   // OMITTED: compose-time per-pane list; empty for a one-Terminal frame
        clusters: _,     // folded into per-cell "glyph" (cell_grapheme)
        combining: _,    // folded into per-cell "glyph" (cell_grapheme)
        visual_to_logical: _, // OMITTED: BiDi map; rows are logical, `text order=display` has it
        line_sizes: _,   // frame "line_sizes"
        line_size_spans: _, // OMITTED: compose-time per-pane refinement, empty for one Terminal
        default_bg_spans: _, // OMITTED: compose-time per-pane refinement; cells carry their bg
        chrome_rasters: _, // OMITTED: host-owned message-band meter raster (pixel ground, rail, ink split), not engine cell content
        images: _,         // frame "images"
        wallpaper: _, // OMITTED: host-owned backdrop base layer (render bling), not engine cell content
        default_bg: _, // OMITTED: engine-resolved live default-bg for padding, not per-cell content (cells carry their own bg)
        default_fg: _, // OMITTED: its twin — the effects layer's tint anchor, not per-cell content
        implicit_blank: _, // OMITTED: exact extraction-time empty cell for host overlays; styled rows carry their own resolved cells
        cursor_color: _,   // frame "cursor.color" (fixed RGB or "default")
        snapshot_seq: _,   // frame "seq" (the engine content version stamp)
        content_seq: _, // OMITTED: host effect-attribution clock paired with the exact snapshot, not wire content
        process_sequence: _, // OMITTED: parser-batch provenance used only by host cursor-effect admission
        input_hot: _, // OMITTED: present-time bloom-defer latency hint, display-only (not cell content)
        // OMITTED: extraction-continuity tokens (the damage carrier). They say whether a
        // scratch's undamaged rows may be kept by the next refill, a producer/scratch question
        // meaningless to a frame reader, and `RenderInput`'s own `PartialEq` excludes them.
        terminal_id: _,
        extract_gen: _,
        engine_fill_seq: _,
        engine_alt: _,
        engine_row_order: _,
        // The per-row revision lane: which rows changed, not pixels; carried, it would make
        // byte-identical screens differ.
        row_rev: _,
        row_rev_lane: _,
        // Splice bookkeeping: prepended host chrome rows and their provenance token.
        row_shift: _,
        shifted_fill_seq: _,
        // Composed-frame provenance: whether a compositor's last write survives in a host
        // scratch buffer; not a terminal-model fact.
        composed_fill_seq: _,
    } = ri;
}

/// Compile gate, per-cell half: every [`StyledCellSnap`] field is a conscious wire decision.
/// Destructured without `..`, so a new per-cell field fails to compile until someone decides
/// whether `write_styled_cell_json` emits it. Never called; it exists to type-check.
fn _styled_cell_snap_is_fully_serialized(snap: &StyledCellSnap) {
    let StyledCellSnap {
        // "fg"/"bg"/"attrs"/"underline_style"/"overline"/"underline_color"/
        // "emoji_presentation"/"text_presentation"/"wide" (the resolved RenderCell)
        cell: _,
        glyph: _,     // "glyph" — a range into its row's buffer
        wide_lead: _, // "wide_lead"
        blink: _,     // "blink" in "attrs"
        // "hyperlink" + "hyperlink_id" (ONE interned StyledLink, or NO_LINK)
        link: _,
    } = snap;
}

/// `screen` -> the lossless engine-styled grid as a single-line JSON frame in the
/// `OK 1\n<json>\n` read framing: per-cell style, selection, cursor, dimensions and sequence
/// state. Host-owned visual effects are omitted; this is a terminal-model projection, not the
/// framebuffer. `--json` is implied.
///
/// `since=<etag>` makes it CONDITIONAL: every full frame ends with
/// `"etag":"<16 hex>"` ([`FrameKey::etag`]), and a read that hands the same etag
/// back while nothing the frame depends on has moved is answered
/// `{"unchanged":true,"seq":<n>,"gen":"<epoch>.<seq>","etag":"<etag>"}` — same
/// `OK 1` framing, no grid gathered under the lock, no frame serialized or sent.
/// Any other etag, or a screen that moved, gets the full frame. Any argument but
/// `--json` and one `since=` is [`SCREEN_USAGE`].
///
/// The frame is the session's published one when it is the frame of the live
/// key (a `subscribe … cells` watch keeps it); otherwise it is gathered here,
/// and published only while a `cells` watch holds the slot.
pub(crate) fn cmd_screen_styled_json(
    term: &Arc<Mutex<Terminal>>,
    frame: &FrameSlot,
    rest: &str,
) -> String {
    let since = match screen_args(rest) {
        Ok(since) => since,
        Err(usage) => return usage,
    };
    // The frame of the key read under ONE lock hold (the published one, or
    // gathered in that hold), serialized with the lock RELEASED — the per-cell
    // format!/base64 never blocks keystroke encode or frame snapshots, which
    // share this mutex. The slot is looked at before the lock: the two never nest.
    let mut look = frame.look_frame();
    let (styled, key, etag) = {
        let t = term_lock(term);
        let key = FrameKey::read(&t);
        let etag = key.etag();
        if since == Some(etag) {
            // No gather under the mutex, no serialize, no frame on the socket.
            return json_ok(&format!(
                "{{\"unchanged\":true,\"seq\":{},\"gen\":\"{}\",\"etag\":\"{etag:016x}\"}}",
                key.seq(),
                key.generation(),
            ));
        }
        (look.styled(&t, &key), key, etag)
    };
    // `json_ok`'s exact `OK 1\n<frame>\n`. ONE key is appended by reopening the
    // object: every existing key, and their order, is untouched (an additive wire
    // change). The `subscribe … cells` push shares the frame and has no
    // conditional form, so it does not carry the fingerprint.
    let reply = |frame: &str| {
        let mut out = String::with_capacity(32 + frame.len());
        out.push_str("OK 1\n");
        out.push_str(frame);
        close_with_etag(out, etag)
    };
    // What the reply is rebuilt from without its image payloads, should it pass
    // the reply-line cap: the snapshot this read gathered, or, for a published
    // frame, whether that frame carries any payload to leave out.
    let (out, lighter) = match styled {
        Styled::Hit(published) => (
            reply(published.body()),
            Lighter::Published(published.carries_image_payloads()),
        ),
        Styled::Miss(snap) => {
            let out = if look.keeps_styled() {
                reply(look.publish_styled(key, &snap).body())
            } else {
                screen_frame_reply(&snap, etag)
            };
            (out, Lighter::Gathered(snap))
        }
    };
    drop(look);
    // The frame never passes the reply-line cap its own client enforces (as
    // `image --bytes` does): past it, once more without the image payloads (a
    // big picture on a small screen), and still past it, an `ERR` line (a grid
    // of about 35,000 cells or more), never a line aterm-ctl must reject. The
    // snapshot this read gathered is reused; a published frame is gathered
    // afresh, under the lock again, only when it carries a payload, so the
    // published frame keeps them for its `cells` watchers. A frame with no image
    // payload is answered the `ERR` line at once: a second gather of a grid this
    // size is a rows x cols walk under the mutex the PTY reader and keystroke
    // encode share, for the same answer.
    let cap = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;
    if frame_line_len(&out) <= cap {
        return out;
    }
    let out = match lighter {
        Lighter::Gathered(mut snap) if snap.carries_image_payloads() => {
            snap.image_payloads = false;
            screen_frame_reply(&snap, etag)
        }
        Lighter::Published(true) => {
            let (mut snap, etag) = {
                let t = term_lock(term);
                (gather_styled_frame(&t), FrameKey::read(&t).etag())
            };
            snap.image_payloads = false;
            screen_frame_reply(&snap, etag)
        }
        Lighter::Gathered(_) | Lighter::Published(false) => out,
    };
    let len = frame_line_len(&out);
    if len > cap {
        return format!(
            "ERR screen: frame too large ({len} bytes > {cap}); `text` or `cell` reads a grid this size\n"
        );
    }
    out
}

/// What an over-cap `screen` reply is rebuilt from without its image payloads
/// ([`cmd_screen_styled_json`]).
enum Lighter {
    /// The snapshot this read gathered.
    Gathered(StyledFrameSnapshot),
    /// The read was served a published frame: whether it carries an image
    /// payload.
    Published(bool),
}

/// `screen`'s `OK 1\n<frame>\n` reply for a gathered frame nobody keeps: written
/// straight into the reply's own buffer, never into a second one to be copied.
fn screen_frame_reply(snap: &StyledFrameSnapshot, etag: u64) -> String {
    let payloads = styled_frame_payloads(snap);
    let mut out = String::with_capacity(32 + styled_frame_estimate(snap, &payloads));
    out.push_str("OK 1\n");
    write_styled_frame(&mut out, snap, &payloads);
    close_with_etag(out, etag)
}

/// Close a full `screen` reply (`OK 1\n` + the frame object) with its etag key
/// and the framing newline.
fn close_with_etag(mut out: String, etag: u64) -> String {
    debug_assert!(out.ends_with('}'), "the styled frame is one JSON object");
    out.pop();
    let _ = std::fmt::Write::write_fmt(&mut out, format_args!(",\"etag\":\"{etag:016x}\"}}"));
    out.push('\n');
    out
}

/// The length of a `screen_frame_reply`'s frame line, without the `OK 1\n` head
/// or its newline: what aterm-ctl's line bound measures.
fn frame_line_len(reply: &str) -> usize {
    reply.len().saturating_sub("OK 1\n".len() + 1)
}

/// `screen`'s argument tail: `--json` (accepted for symmetry; the verb is always
/// JSON) and at most one `since=<16 hex digits>`, in any order. Anything else —
/// a malformed or repeated `since=`, a guessed modifier — is [`SCREEN_USAGE`],
/// never a silent full frame.
fn screen_args(rest: &str) -> Result<Option<u64>, String> {
    let usage = || Err(SCREEN_USAGE.to_string());
    let mut since = None;
    for tok in rest.split_whitespace() {
        if tok == "--json" || tok == "json" {
            continue;
        }
        let Some(hex) = tok.strip_prefix("since=") else {
            return usage();
        };
        if since.is_some() || hex.len() != 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return usage();
        }
        since = u64::from_str_radix(hex, 16).ok();
        if since.is_none() {
            return usage();
        }
    }
    Ok(since)
}

/// The etag a full `screen` frame carries ([`FrameKey::etag`]): the name the
/// tests that hold it to the frame call it by.
#[cfg(test)]
fn screen_etag(t: &Terminal) -> u64 {
    FrameKey::read(t).etag()
}

// The conditional read's promise as tests: the etag moves with every frame
// change a corpus and a seeded random walk make, the host's changes included.
#[cfg(test)]
#[path = "control_screen_etag_tests.rs"]
mod screen_etag_tests;

/// Serialize the JSON twin of [`serialize_dims`].
pub(crate) fn serialize_dims_json(snapshot: &DimsSnapshot) -> String {
    let window = snapshot
        .window
        .map_or_else(|| "null".to_string(), |window| window.to_string());
    let present_retry_in_ms = snapshot
        .present_retry_in_ms
        .map_or_else(|| "null".to_string(), |delay| delay.to_string());
    json_ok(&format!(
        "{{\"rows\":{},\"cols\":{},\"pixel_w\":{},\"pixel_h\":{},\
         \"session\":{},\"cell_w\":{},\"cell_h\":{},\"font_px\":{:.2},\"scale\":{:.2},\
         \"window\":{},\"window_rows\":{},\"window_cols\":{},\"composed_rows\":{},\
         \"grid_w\":{},\"grid_h\":{},\"frame_w\":{},\"frame_h\":{},\
         \"surface_w\":{},\"surface_h\":{},\"offset_x\":{},\"offset_y\":{},\
         \"band_left\":{},\"band_right\":{},\"band_top\":{},\"band_bottom\":{},\
         \"crop_left\":{},\"crop_right\":{},\"crop_top\":{},\"crop_bottom\":{},\
         \"pad\":{},\"pad_top\":{},\"pad_bottom\":{},\"head\":{},\"tab_rows\":{},\
         \"deck_y\":{},\"deck_h\":{},\"viewers\":{},\"visible_viewers\":{},\"geometry\":\"{}\",\
         \"present_retry_state\":\"{}\",\"present_retry_count\":{},\
         \"present_retry_remaining\":{},\"present_retry_in_ms\":{}}}",
        snapshot.rows,
        snapshot.cols,
        snapshot.pixel_w,
        snapshot.pixel_h,
        snapshot.session,
        snapshot.cell_w,
        snapshot.cell_h,
        snapshot.font_px,
        snapshot.scale,
        window,
        snapshot.window_rows,
        snapshot.window_cols,
        snapshot.composed_rows,
        snapshot.pixel_w,
        snapshot.pixel_h,
        snapshot.frame_w,
        snapshot.frame_h,
        snapshot.surface_w,
        snapshot.surface_h,
        snapshot.offset_x,
        snapshot.offset_y,
        snapshot.band_left,
        snapshot.band_right,
        snapshot.band_top,
        snapshot.band_bottom,
        snapshot.crop_left,
        snapshot.crop_right,
        snapshot.crop_top,
        snapshot.crop_bottom,
        snapshot.pad,
        snapshot.pad_top,
        snapshot.pad_bottom,
        snapshot.head,
        snapshot.tab_rows,
        snapshot.deck_y,
        snapshot.deck_h,
        snapshot.viewers,
        snapshot.visible_viewers,
        snapshot.geometry,
        snapshot.present_retry_state,
        snapshot.present_retry_count,
        snapshot.present_retry_remaining,
        present_retry_in_ms,
    ))
}

/// `dims --json` -> one live session/window/frame/surface geometry object.
pub(crate) fn cmd_dims_json(
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    proxy: &EventLoopProxy<Wake>,
) -> String {
    match read_dims(term, session, proxy) {
        Ok(snapshot) => serialize_dims_json(&snapshot),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `colors` -> the terminal's live default colours, then what programs set
/// over the theme:
/// `OK fg=<rrggbb> bg=<rrggbb> cursor=<rrggbb|default> palette_sets=<allowed|refused>
/// refused=<0|1> program_palette=<-|idx:rrggbb,…> program_slots=<-|fg,bg,…>`.
/// Programs change the defaults via OSC 10/11/12; the per-cell `cell` verb only
/// reports already-RESOLVED colors, so this surfaces the theme itself for a
/// client deciding how to render or reason. The tail says why a cell's colour
/// is not the theme's: `palette_sets` is the session's OSC 4/21 gate, `refused`
/// whether it refused a palette set, `program_palette` every indexed entry a
/// program set (ascending, at most 256 × 9 bytes: digits and hex, no escaping)
/// and `program_slots` the named slots it owns, in the order fg, bg, cursor,
/// selection_bg, selection_fg (a slot set back to dynamic counts). A program's
/// colours live for the session; `reset` puts them back.
pub(crate) fn cmd_colors(term: &Arc<Mutex<Terminal>>) -> String {
    use std::fmt::Write as _;
    let t = term_lock(term);
    let h = |r: u8, g: u8, b: u8| format!("{r:02x}{g:02x}{b:02x}");
    let fg = t.default_foreground();
    let bg = t.default_background();
    let cursor = t
        .cursor_color()
        .map_or_else(|| "default".to_string(), |c| h(c.r, c.g, c.b));
    let o = t.program_color_overrides();
    let mut palette = String::new();
    for e in &o.palette {
        if !palette.is_empty() {
            palette.push(',');
        }
        let _ = write!(palette, "{}:{}", e.index, h(e.rgb.r, e.rgb.g, e.rgb.b));
    }
    if palette.is_empty() {
        palette.push('-');
    }
    let slots: Vec<&str> = [
        ("fg", o.foreground.is_some()),
        ("bg", o.background.is_some()),
        ("cursor", o.cursor.is_some()),
        ("selection_bg", o.selection_background.is_some()),
        ("selection_fg", o.selection_foreground.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, set)| set.then_some(name))
    .collect();
    format!(
        "OK fg={} bg={} cursor={} palette_sets={} refused={} program_palette={} program_slots={}\n",
        h(fg.r, fg.g, fg.b),
        h(bg.r, bg.g, bg.b),
        cursor,
        if t.allow_palette_reconfigure() {
            "allowed"
        } else {
            "refused"
        },
        u8::from(t.palette_set_refused()),
        palette,
        if slots.is_empty() {
            "-".to_string()
        } else {
            slots.join(",")
        },
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::{Terminal, take_narrowed_query_steps};
    use aterm_messages::WaitEnd;

    use super::{
        cmd_cell, cmd_custody, cmd_search, distinct_images, gather_styled_frame, serialize_dims,
        serialize_dims_json, styled_frame_payload,
    };
    use crate::session_waits::{RewrapGauge, RewrapReading};
    use crate::term_lock;

    /// Display one 1x1-cell kitty image, filled with `tag`, at 1-based
    /// `row`/`col`. `s`/`v` are the raster's pixel size — one cell each at a
    /// 10x20 cell metric — and `f=32` requires exactly `s * v * 4` bytes.
    fn place(t: &mut Terminal, row: u16, col: u16, id: u8, tag: u8) {
        t.process(format!("\x1b[{row};{col}H").as_bytes());
        let raster = vec![tag; 10 * 20 * 4];
        let b64 = aterm_codec::base64::encode(&raster).expect("encode");
        let mut apc = format!("\x1b_Ga=T,f=32,s=10,v=20,i={id};").into_bytes();
        apc.extend_from_slice(b64.as_bytes());
        apc.extend_from_slice(b"\x1b\\");
        t.process(&apc);
    }

    /// The styled frame's image array is ROW-MAJOR, ascending column within a row
    /// — the order a socket client indexes into, and the one thing a "just iterate
    /// the extras map" rewrite of `distinct_images` would destroy silently:
    /// `FxHashMap` iteration order is arbitrary, is not insertion order, and is
    /// free to change under a rehash.
    ///
    /// Six pictures placed in deliberately wrong order — the bottom one first,
    /// and FOUR sharing one row at columns whose raw `FxHashMap` iteration order
    /// is provably non-ascending (measured on this key set: {2, 8, 20, 33}
    /// 1-based iterates as [7, 1, 19, 32]) — must come back sorted by
    /// `(row, col)`, each anchor still carrying ITS OWN payload. The shared-row
    /// spread is what makes the COLUMN half of the claim real: with only two
    /// images on a row the map can hand them back ascending by luck, and an
    /// earlier two-image form of this test kept passing with the column sort
    /// deleted. Mutation-checked: removing `sort_unstable_by_key` from
    /// `images_frame_into` turns this red. An order test that checked anchors
    /// alone would also pass on a shuffle that paired the right positions with
    /// the wrong pictures, so the tag byte rides along.
    ///
    /// Kitty `a=T` rather than iTerm2 OSC 1337 because the iTerm2 path is
    /// LEFT-ANCHORED at the margin by spec (`place_image(.., 0)`) and so cannot
    /// put two images on one row; kitty transmits-and-displays at the CURSOR.
    #[test]
    fn distinct_images_are_row_major_regardless_of_placement_order() {
        let mut t = Terminal::new(10, 40);
        t.set_cell_pixel_size(10, 20);
        place(&mut t, 5, 3, 1, b'F');
        // Row 1 carries four images at {2, 8, 20, 33}: raw map order for these
        // keys is [7, 1, 19, 32] (non-ascending), so an unsorted emit cannot
        // pass by accident.
        place(&mut t, 1, 33, 2, b'E');
        place(&mut t, 1, 8, 3, b'B');
        place(&mut t, 1, 2, 4, b'A');
        place(&mut t, 1, 20, 5, b'D');
        place(&mut t, 3, 10, 6, b'C');

        // The tag byte identifies which picture landed at each anchor; the raster
        // itself is uniform, so its first byte is the whole identity.
        let got: Vec<(usize, usize, u8)> = distinct_images(&t)
            .into_iter()
            .map(|(r, c, img)| (r, c, img.bytes[0]))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, 1, b'A'),
                (0, 7, b'B'),
                (0, 19, b'D'),
                (0, 32, b'E'),
                (2, 9, b'C'),
                (4, 2, b'F'),
            ],
            "images must be reported row-major, not in placement or map order"
        );
    }

    /// The styled frame encodes each picture once: a frame change that moves
    /// no picture encodes no image and carries the same `images` array. The
    /// negative control is a new picture, which is encoded.
    #[test]
    fn a_frame_change_that_moves_no_picture_encodes_no_image() {
        use crate::work_counts::take_images_encoded;
        let frame = styled_frame_payload;
        let images = |frame: &str| {
            let at = frame
                .rfind("\"images\":")
                .expect("the frame ends with its images");
            frame[at..].to_string()
        };
        let mut t = Terminal::new(6, 40);
        t.set_cell_pixel_size(10, 20);
        place(&mut t, 1, 2, 1, b'A');
        place(&mut t, 1, 8, 2, b'B');
        place(&mut t, 3, 10, 3, b'C');
        take_images_encoded();

        let first = frame(&t);
        assert_eq!(
            take_images_encoded(),
            3,
            "the first frame encodes each picture"
        );
        t.process(b"\x1b[6;1Hmore text");
        let second = frame(&t);
        assert_ne!(first, second, "the frame changed");
        assert_eq!(take_images_encoded(), 0, "and no picture is encoded again");
        assert_eq!(images(&first), images(&second), "the same images array");

        place(&mut t, 5, 20, 4, b'D');
        let third = frame(&t);
        assert_eq!(take_images_encoded(), 1, "a new picture is encoded");
        assert_eq!(images(&third).matches("\"b64\":").count(), 4);
    }

    /// The `custody` verb's whole reason to exist: it separates events that leave
    /// IDENTICAL state behind.
    ///
    /// A bare modifier, an auto-repeat tick and a key release each change nothing at
    /// all, so `scroll` and `selection` report the same three numbers after every one
    /// of them. Only the engine's own record can say which one just happened, and
    /// "which one just happened" is exactly what a user asking "why did my selection
    /// disappear?" needs. If this ever collapses to one answer the verb is a
    /// decoration.
    #[test]
    fn custody_names_the_three_events_that_change_nothing() {
        use crate::app_input::{PressKind, apply_press_custody};

        let mut t = Terminal::new(6, 20);
        for i in 0..10 {
            t.process(format!("line{i}\r\n").as_bytes());
        }
        t.scroll_display(1);
        {
            let sel = t.text_selection_mut();
            sel.start_selection(
                0,
                0,
                aterm_core::selection::SelectionSide::Left,
                aterm_core::selection::SelectionType::Simple,
            );
            sel.update_selection(1, 5, aterm_core::selection::SelectionSide::Right);
            sel.complete_selection();
        }
        let term = Arc::new(Mutex::new(t));

        let mut seen = Vec::new();
        for kind in [PressKind::Inert, PressKind::Repeat, PressKind::Release] {
            apply_press_custody(&mut term_lock(&term), kind);
            seen.push(cmd_custody(&term));
        }
        assert_eq!(
            seen,
            vec![
                "OK last=InertPress event=3 changed=UserScroll took_selection=none offset=1 owner=user \
                 selection=yes scrollback=5\n"
                    .to_string(),
                "OK last=RepeatPress event=2 changed=UserScroll took_selection=none offset=1 owner=user \
                 selection=yes scrollback=5\n"
                    .to_string(),
                "OK last=ReleaseEvent event=4 changed=UserScroll took_selection=none offset=1 owner=user \
                 selection=yes scrollback=5\n"
                    .to_string(),
            ],
            "three events that moved NOTHING must still be told apart by name — and \
             none of them may claim to have CHANGED anything: the scroll that really \
             took the viewport is still the answer to `changed`"
        );

        // …and the one press that really does take custody says so, with the state it
        // left behind changing in the same line.
        apply_press_custody(&mut term_lock(&term), PressKind::Typing);
        assert_eq!(
            cmd_custody(&term),
            "OK last=TypingPress event=1 changed=TypingPress took_selection=TypingPress offset=0 owner=tail \
             selection=no scrollback=5\n",
            "typing is the one handover, and the verb shows both the name and the effect"
        );

        // The shell's next prompt must not erase the answer: `last` moves to `OutputAtLive`,
        // `changed` does not, since it took nothing.
        term_lock(&term).process(b"$ \r\n");
        assert_eq!(
            cmd_custody(&term),
            "OK last=OutputAtLive event=5 changed=TypingPress took_selection=TypingPress offset=0 owner=tail \
             selection=no scrollback=6\n",
            "ordinary output is the last EVENT but must not become the last CHANGE"
        );

        // Output records too: this ED 3 lands on a live, unselected prompt, so it names itself
        // in `last` and takes nothing.
        term_lock(&term).process(b"\x1b[3J");
        assert_eq!(
            cmd_custody(&term),
            "OK last=OutputInvalidatesTheCoordinateSpace event=7 changed=TypingPress \
             took_selection=OutputInvalidatesTheCoordinateSpace \
             offset=0 owner=tail selection=no scrollback=0\n",
            "ED 3 destroyed the coordinate space; the verb names that, not a press"
        );
    }

    /// The explanation of an involuntary loss survives a later deliberate deselect: `changed=`
    /// moves to `UserClear`, `took_selection=` keeps the culprit.
    #[test]
    fn a_later_click_does_not_erase_why_the_selection_actually_died() {
        let mut t = Terminal::new(4, 20);
        for i in 0..8 {
            t.process(format!("line{i}\r\n").as_bytes());
        }
        {
            let sel = t.text_selection_mut();
            sel.start_selection(
                1,
                0,
                aterm_core::selection::SelectionSide::Left,
                aterm_core::selection::SelectionType::Simple,
            );
            sel.update_selection(1, 4, aterm_core::selection::SelectionSide::Right);
            sel.complete_selection();
        }
        let term = Arc::new(Mutex::new(t));

        // Output replaces the selected row: an involuntary loss with a real culprit.
        term_lock(&term).process(b"\x1b[2;1H\x1b[Kreplaced");
        assert!(
            !term_lock(&term).text_selection().has_selection(),
            "precondition: the output really did take the highlight"
        );
        let before = cmd_custody(&term);
        assert!(
            before.contains("took_selection=OutputDamagesTheSelectedRows"),
            "the culprit is named while it is fresh: {before}"
        );

        // …now the user clicks somewhere, which is a deliberate deselect.
        crate::app_mouse::note_selection_custody(&mut term_lock(&term), false);

        let after = cmd_custody(&term);
        assert!(
            after.contains("changed=UserClear"),
            "the click IS the last thing that moved custody, and `changed` says so: {after}"
        );
        assert!(
            after.contains("took_selection=OutputDamagesTheSelectedRows"),
            "…but the explanation of the involuntary loss must SURVIVE it: {after}"
        );
    }

    /// Output that takes a highlight for a reason other than damage overlap (an eviction at
    /// the history floor) names itself instead of erasing the record to `last=none`.
    #[test]
    fn custody_names_output_that_took_the_selection_with_no_action_to_blame() {
        let mut t = Terminal::new(4, 20);
        t.set_scrollback_line_limit(Some(2));
        for i in 0..8 {
            t.process(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(t.grid().scrollback_lines(), 2, "a two-line history floor");
        {
            let sel = t.text_selection_mut();
            sel.start_selection(
                -2,
                0,
                aterm_core::selection::SelectionSide::Left,
                aterm_core::selection::SelectionType::Simple,
            );
            sel.update_selection(-2, 5, aterm_core::selection::SelectionSide::Right);
            sel.complete_selection();
            assert!(sel.has_selection(), "the oldest retained row is selected");
        }
        let term = Arc::new(Mutex::new(t));
        // A true prior record, so the erasure this test is about would be visible.
        crate::app_input::apply_press_custody(
            &mut term_lock(&term),
            crate::app_input::PressKind::Inert,
        );

        // Three more lines: the selected row falls off the floor. Nothing replaced
        // it (it was evicted), so no damage band names it.
        term_lock(&term).process(b"a\r\nb\r\nc\r\n");
        assert!(
            !term_lock(&term).text_selection().has_selection(),
            "the eviction really did take the highlight"
        );
        assert_eq!(
            cmd_custody(&term),
            "OK last=OutputTookTheSelectionUnattributed event=6 \
             changed=OutputTookTheSelectionUnattributed \
             took_selection=OutputTookTheSelectionUnattributed offset=0 owner=tail \
             selection=no scrollback=2\n",
            "output took it for a reason the model cannot name — which is a real \
             answer that rules out the keyboard, and `last=none` was not"
        );
    }

    #[test]
    fn dims_serializers_expose_explicit_bottom_padding() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
        app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
        let term = app.front_terminal(wid).unwrap().term.clone();
        let snapshot = app.try_dims_snapshot(0, &term).unwrap();

        let text = serialize_dims(&snapshot);
        assert!(text.contains("pad=12 pad_top=2 pad_bottom=12 head="));
        assert!(!text.contains("pad_bottom=22"), "deleted inferred bottom");

        let json = serialize_dims_json(&snapshot);
        assert!(json.contains("\"pad\":12,\"pad_top\":2,\"pad_bottom\":12"));
        assert!(!json.contains("\"pad_bottom\":22"));
    }
    fn term_with(lines: &[&str]) -> Arc<Mutex<Terminal>> {
        let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
        for l in lines {
            term.lock().unwrap().process(format!("{l}\r\n").as_bytes());
        }
        term
    }

    /// **THE STAMP MUST MATCH THE ROWS.** `status`'s `hash=` is FNV-1a-64 over
    /// the untrimmed visible screen, and a reporter reads `status` for the
    /// stamp, `text` for the rows, and hashes the rows ITSELF
    /// to check the two describe one screen — posting nothing when they differ.
    /// So the bytes `screen_stamp` hashes must be exactly the bytes `text`
    /// sends under its header: same rows, same order, one `\n` after each,
    /// trailing spaces and all. A row of blanks or a trailing space on either
    /// side alone would make every report unpostable. The published text
    /// `status` reads its `hash=` off ([`ScreenText`]) is held to the same
    /// bytes.
    #[test]
    fn screen_stamp_matches_the_text_verbs_rows() {
        for lines in [
            vec![],
            vec![
                "\u{23fa} All 248 tests pass; the branch is ready.",
                "  Next: the e2e suite.",
            ],
            vec![
                "trailing spaces follow   ",
                "",
                "\u{4e16}\u{754c} wide glyphs",
            ],
        ] {
            let term = term_with(&lines);
            let (_, stamp) = super::screen_stamp(&term.lock().unwrap());
            let served = super::cmd_text(&term);
            // The header is the first line; the rows are the rest, each of
            // which the wire sends with its own `\n`.
            let body = served
                .split_once('\n')
                .map_or(String::new(), |(_, rest)| rest.to_string());
            assert_eq!(
                stamp,
                aterm_hash::fnv1a_64(body.as_bytes()),
                "stamp over {lines:?} must hash the rows `text` served:\n{served:?}"
            );
            let published = super::ScreenText::of(&term.lock().unwrap());
            assert_eq!(
                published.whole(),
                body,
                "the published text is `text`'s body"
            );
            assert_eq!(published.fnv(), stamp, "and its hash is the stamp");
        }
    }

    /// The agent-status sweep keeps only its live-zone tail, but its fingerprint hashes the
    /// whole `text` frame, across a deep grid, a resize and both sides of an alt-screen switch.
    #[test]
    fn agent_frame_stream_matches_whole_screen_text_across_grid_changes() {
        fn parity(t: &Terminal, phase: &str) {
            let text = super::screen_text(t);
            let all: Vec<&str> = text.split_terminator('\n').collect();
            assert_eq!(
                all.len(),
                t.rows() as usize,
                "{phase}: every row is present"
            );
            for tail in [0, 1, crate::presence::CLASSIFY_ROWS, 100] {
                let (fp, rows) = crate::session_status::screen_agent_frame(
                    &crate::frame_memo::ScreenText::of(t),
                    tail,
                );
                let from = all.len().saturating_sub(tail);
                let expected: Vec<String> =
                    all[from..].iter().map(|row| (*row).to_string()).collect();
                assert_eq!(
                    fp,
                    aterm_hash::fnv1a_64(text.as_bytes()),
                    "{phase}: fingerprint for tail={tail}"
                );
                assert_eq!(rows, expected, "{phase}: last {tail} rows");
            }
        }

        let mut t = Terminal::new(48, 80);
        parity(&t, "blank main grid");
        for n in 0..46 {
            t.process(format!("line {n:02}: 世界 e\u{301} trail   \r\n").as_bytes());
        }
        parity(&t, "deep Unicode main grid");
        t.process(b"\x1b[?1049h\x1b[H");
        t.process("代替 screen e\u{301}  ".as_bytes());
        parity(&t, "alternate grid");
        t.resize(24, 100);
        parity(&t, "resized alternate grid");
        t.process(b"\x1b[?1049l");
        parity(&t, "restored main grid");
    }

    /// The `modes` frame: `OK <n>` counts the `key=value` lines that follow (the
    /// doc once read `OK` while the handler sent `OK 12`), each one a distinct
    /// key a client indexes by name, so the key set is the wire contract (the
    /// supervisor reads `alt_screen=`, the live-upgrade probe `mouse_mode=`, the
    /// handback check `focus_reporting=`/`sync_output=`).
    #[test]
    fn modes_frames_its_count_over_distinct_key_lines() {
        let term = term_with(&[]);
        let out = super::cmd_modes(&term);
        let mut lines = out.lines();
        let count: usize = lines
            .next()
            .and_then(|header| header.strip_prefix("OK "))
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("an `OK <n>` header: {out}"));
        let keys: std::collections::BTreeSet<&str> = lines
            .map(|l| l.split_once('=').map(|(k, _)| k).expect("key=value"))
            .collect();
        assert_eq!(keys.len(), count, "{out}");
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "alt_screen",
                "app_cursor_keys",
                "app_keypad",
                "auto_wrap",
                "bracketed_paste",
                "cursor_visible",
                "focus_reporting",
                "insert_mode",
                "kitty_keyboard",
                "modify_other_keys",
                "mouse_encoding",
                "mouse_mode",
                "origin_mode",
                "sync_output",
            ]),
            "{out}"
        );
        assert!(out.ends_with('\n'), "{out}");
    }

    /// The `cell` doc's attrs vocabulary includes the two width markers; this
    /// pins that a CJK lead cell answers `wide`, its spacer `wide_cont`, and the
    /// ASCII neighbour a bare `none`. Positional split: the spacer's grapheme is
    /// the empty token, which a whitespace split would swallow.
    #[test]
    fn cell_attrs_carry_the_width_markers() {
        let term = term_with(&["\u{6f22}a"]);
        let attrs = |reply: String| reply.trim_end().split(' ').nth(4).map(str::to_owned);
        let lead = super::cmd_cell(&term, "0 0");
        let cont = super::cmd_cell(&term, "0 1");
        let ascii = super::cmd_cell(&term, "0 2");
        assert_eq!(attrs(lead.clone()).as_deref(), Some("wide"), "{lead}");
        assert_eq!(attrs(cont.clone()).as_deref(), Some("wide_cont"), "{cont}");
        assert_eq!(attrs(ascii.clone()).as_deref(), Some("none"), "{ascii}");
    }

    /// `cell abs=<n> <c>` READS A HISTORY ROW'S CELL AS IT READ ON THE SCREEN:
    /// rows drawn in a style — a caret in bold `5c5cff`, a blank row of spaces
    /// in that style, a blank row an erase drew under it with a background set,
    /// a plain row — read the same, glyph, colours and attrs, at every column,
    /// once scrolled into the history as `cell <r> <c>` read them on the
    /// screen, and `abs=` of a live row is that row. NEGATIVE CONTROLS: the
    /// plain row reads otherwise than the caret's, a row past the screen is
    /// `ERR out of range`, and a row number that is no number `ERR bad args`.
    #[test]
    fn a_history_rows_cell_reads_as_it_did_on_the_screen() {
        let term = Arc::new(Mutex::new(Terminal::new(5, 12)));
        term.lock().unwrap().process(
            b"\x1b[1;38;2;92;92;255m> Hi\x1b[0m\r\n\
              \x1b[1;38;2;92;92;255m            \x1b[0m\r\n\
              \x1b[1;38;2;92;92;255;48;2;1;2;3m\x1b[K\x1b[0m\r\n\
              \x1b[38;2;208;208;208m  plain\x1b[0m\r\n",
        );
        let cols = 0..12;
        let live: Vec<Vec<String>> = (0..4)
            .map(|r| {
                cols.clone()
                    .map(|c| cmd_cell(&term, &format!("{r} {c}")))
                    .collect()
            })
            .collect();
        for (r, row) in live.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                assert!(cell.starts_with("OK "), "{r} {c}: {cell}");
                assert_eq!(
                    &cmd_cell(&term, &format!("abs={r} {c}")),
                    cell,
                    "a live row by its absolute number, {r} {c}"
                );
            }
        }
        term.lock()
            .unwrap()
            .process(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
        assert!(
            term.lock().unwrap().grid().scrollback_lines() >= 4,
            "the rows are in the history"
        );
        for (r, row) in live.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                assert_eq!(
                    &cmd_cell(&term, &format!("abs={r} {c}")),
                    cell,
                    "history row {r}, column {c}"
                );
            }
        }
        // Past the ring of the newest rows, into the stored lines.
        let far: String = (0..6000).map(|i| format!("{i}\r\n")).collect();
        term.lock().unwrap().process(far.as_bytes());
        for (r, row) in live.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                assert_eq!(
                    &cmd_cell(&term, &format!("abs={r} {c}")),
                    cell,
                    "stored history row {r}, column {c}"
                );
            }
        }
        assert!(live[0][0].starts_with("OK > 5c5cff "), "{}", live[0][0]);
        assert!(live[0][0].ends_with(" bold\n"), "{}", live[0][0]);
        assert_ne!(live[3][0], live[0][0], "the control: the plain row");
        assert_eq!(cmd_cell(&term, "abs=100000 0"), "ERR out of range\n");
        assert_eq!(cmd_cell(&term, "abs=x 0"), "ERR bad args\n");
        assert!(cmd_cell(&term, "abs=0 12").starts_with("ERR out of range: rows"));
    }

    /// A cell past the grid is refused with the grid's size, so the caller
    /// needs no `dims` round trip to learn what it could have asked for.
    #[test]
    fn a_cell_past_the_grid_names_the_grid() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        assert_eq!(
            cmd_cell(&term, "2 0"),
            "ERR out of range: rows 0-1, cols 0-7\n"
        );
        assert_eq!(
            cmd_cell(&term, "0 8"),
            "ERR out of range: rows 0-1, cols 0-7\n"
        );
        assert!(cmd_cell(&term, "1 7").starts_with("OK"));
    }

    /// Sparse tail cells must report the same effective defaults as a
    /// materialized cell. In particular DECSCNM swaps those defaults for both
    /// the one-cell verb and the full styled frame; using the raw OSC values as
    /// the fallback would disagree with the pixels on glass. And OSC 110/111
    /// reset them for the tail as for any cell.
    #[test]
    fn sparse_blank_queries_fold_live_defaults_and_decscnm() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        {
            let mut t = term_lock(&term);
            t.process(b"\x1b]10;rgb:11/22/33\x07");
            t.process(b"\x1b]11;rgb:44/55/66\x07");
            t.process(b"X");
            assert!(
                t.render_row_at_screen(0).get(7).is_none(),
                "negative control: column 7 is an implicit sparse-tail cell"
            );
        }

        let normal = cmd_cell(&term, "0 7");
        assert!(
            normal.contains("112233 445566"),
            "normal implicit blank uses live OSC defaults: {normal:?}"
        );
        {
            let t = term_lock(&term);
            let snap = gather_styled_frame(&t);
            assert_eq!(snap.cells[0].cells[0].cell.fg, [0x11, 0x22, 0x33]);
            assert_eq!(snap.cells[0].cells[0].cell.bg, [0x44, 0x55, 0x66]);
            assert_eq!(
                snap.cells[0].cells[7].cell.fg,
                snap.cells[0].cells[0].cell.fg
            );
            assert_eq!(
                snap.cells[0].cells[7].cell.bg, snap.cells[0].cells[0].cell.bg,
                "materialized and implicit default-colour cells agree"
            );
        }

        term_lock(&term).process(b"\x1b[?5h");
        let reversed = cmd_cell(&term, "0 7");
        assert!(
            reversed.contains("445566 112233"),
            "DECSCNM swaps the implicit blank exactly like painted cells: {reversed:?}"
        );
        {
            let t = term_lock(&term);
            let snap = gather_styled_frame(&t);
            assert_eq!(snap.cells[0].cells[0].cell.fg, [0x44, 0x55, 0x66]);
            assert_eq!(snap.cells[0].cells[0].cell.bg, [0x11, 0x22, 0x33]);
            assert_eq!(
                snap.cells[0].cells[7].cell.fg,
                snap.cells[0].cells[0].cell.fg
            );
            assert_eq!(
                snap.cells[0].cells[7].cell.bg,
                snap.cells[0].cells[0].cell.bg
            );
        }

        // OSC 110/111 restore the configured defaults for the implicit tail
        // too: neither the dynamic colour nor the reverse-video fold lingers.
        let configured = {
            let fresh = Terminal::new(2, 8);
            (fresh.default_foreground(), fresh.default_background())
        };
        let mut t = term_lock(&term);
        t.process(b"\x1b[?5l\x1b]110\x07\x1b]111\x07");
        let snap = gather_styled_frame(&t);
        assert_eq!(
            snap.cells[0].cells[7].cell.fg,
            [configured.0.r, configured.0.g, configured.0.b]
        );
        assert_eq!(
            snap.cells[0].cells[7].cell.bg,
            [configured.1.r, configured.1.g, configured.1.b]
        );
    }

    /// Every glyph the frame puts on the wire is byte-for-byte `Terminal::cell_grapheme` for
    /// that cell (what the `cell` verb answers). One row carries each hazard for a shared
    /// buffer and byte ranges: ASCII, a written blank (a space, not empty), a wide pair (the
    /// continuation owns a zero-length range), a combining cluster, and the untouched tail.
    #[test]
    fn styled_frame_glyphs_are_cell_grapheme_across_a_mixed_row() {
        // The wire arm compares JSON-escaped `glyph` fields with raw `cell_grapheme` strings,
        // which works because this fixture has nothing to escape.
        let mut term = Terminal::new(2, 12);
        term.process("ab \u{754C}e\u{0301}".as_bytes());
        let snap = gather_styled_frame(&term);

        assert_eq!(snap.glyph(0, 0), "a");
        assert_eq!(snap.glyph(0, 1), "b");
        assert_eq!(snap.glyph(0, 2), " ", "a written blank is a SPACE");
        assert_eq!(snap.glyph(0, 3), "\u{754C}", "the wide LEAD owns the glyph");
        assert_eq!(snap.glyph(0, 4), "", "the wide continuation owns nothing");
        assert_eq!(
            snap.glyph(0, 5),
            "e\u{0301}",
            "combining mark kept with base"
        );
        assert_eq!(snap.glyph(0, 6), " ", "the untouched tail is blank cells");

        // Every cell, against the single-cell API — the frame is a sweep of it.
        for r in 0..snap.rows {
            for c in 0..snap.cols {
                assert_eq!(
                    snap.glyph(r, c),
                    term.cell_grapheme(r, c).expect("in-range cell"),
                    "snapshot glyph at ({r},{c})"
                );
            }
        }

        // And the serialized frame, which is the actual contract: the `glyph`
        // fields in row-major order are those same graphemes.
        let frame = styled_frame_payload(&term);
        let wire: Vec<&str> = frame
            .match_indices("\"glyph\":\"")
            .map(|(at, key)| {
                let rest = &frame[at + key.len()..];
                let end = rest.find("\",\"fg\"").expect("glyph field precedes fg");
                &rest[..end]
            })
            .collect();
        assert_eq!(
            wire.len(),
            snap.rows * snap.cols,
            "one glyph field per cell, no trim"
        );
        for (i, got) in wire.iter().enumerate() {
            let (r, c) = (i / snap.cols, i % snap.cols);
            assert_eq!(
                *got,
                term.cell_grapheme(r, c).expect("in-range cell"),
                "wire glyph at ({r},{c}) of {frame}"
            );
        }
    }

    /// A block selection and a linear selection can have byte-identical raw
    /// endpoints while painting radically different cells. The styled frame must
    /// preserve the kind, and its coordinates must be the side-adjusted projection
    /// used by the renderer rather than the raw anchors.
    #[test]
    fn styled_frame_selection_is_typed_and_side_adjusted() {
        use aterm_core::selection::{SelectionSide, SelectionType};

        let mut simple = Terminal::new(3, 10);
        {
            let sel = simple.text_selection_mut();
            sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(2, 4, SelectionSide::Right);
            sel.complete_selection();
        }
        let simple_frame = styled_frame_payload(&simple);
        assert!(
            simple_frame.contains(
                "\"selection\":{\"start_row\":0,\"start_col\":1,\"end_row\":2,\
                 \"end_col\":4,\"kind\":\"simple\",\"is_block\":false}"
            ),
            "linear selection kind/geometry must survive: {simple_frame}"
        );

        let mut block = Terminal::new(3, 10);
        {
            let sel = block.text_selection_mut();
            sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(2, 4, SelectionSide::Right);
            sel.complete_selection();
        }
        let block_frame = styled_frame_payload(&block);
        assert!(
            block_frame.contains(
                "\"selection\":{\"start_row\":0,\"start_col\":1,\"end_row\":2,\
                 \"end_col\":4,\"kind\":\"block\",\"is_block\":true}"
            ),
            "rectangular selection kind must not collapse into linear: {block_frame}"
        );
        assert_ne!(
            simple_frame, block_frame,
            "negative control: identical raw endpoints with different paint geometry"
        );

        let mut sided = Terminal::new(2, 10);
        {
            let sel = sided.text_selection_mut();
            sel.start_selection(0, 1, SelectionSide::Right, SelectionType::Simple);
            sel.update_selection(0, 5, SelectionSide::Left);
            sel.complete_selection();
        }
        let sided_frame = styled_frame_payload(&sided);
        assert!(
            sided_frame.contains(
                "\"selection\":{\"start_row\":0,\"start_col\":2,\"end_row\":0,\
                 \"end_col\":4,\"kind\":\"simple\",\"is_block\":false}"
            ),
            "half-cell sides must project to the renderer's 2..=4 range: {sided_frame}"
        );
        assert!(
            !sided_frame.contains(
                "\"selection\":{\"start_row\":0,\"start_col\":1,\"end_row\":0,\
                 \"end_col\":5"
            ),
            "negative control: raw one-cell-too-wide bounds must not leak"
        );
    }

    /// OSC 17/19 and OSC 12 are live render state, not properties of a cell.
    /// Carry their fixed/dynamic policies explicitly so a styled-frame consumer
    /// can reproduce the selection and cursor overlays instead of inventing gray.
    #[test]
    fn styled_frame_carries_live_selection_and_cursor_colors() {
        let mut term = Terminal::new(2, 8);
        let defaults = styled_frame_payload(&term);
        assert!(
            defaults.contains("\"cursor\":{")
                && defaults.contains("\"color\":\"default\"")
                && defaults.contains("\"selection_bg\":\"default\"")
                && defaults.contains("\"selection_fg\":\"dynamic\""),
            "default/dynamic overlay policies must be explicit: {defaults}"
        );

        term.process(
            b"\x1b]12;rgb:ab/cd/ef\x07\
              \x1b]17;rgb:12/34/56\x07\
              \x1b]19;rgb:fe/dc/ba\x07",
        );
        let fixed = styled_frame_payload(&term);
        assert!(
            fixed.contains("\"color\":\"abcdef\""),
            "OSC 12 cursor color lost: {fixed}"
        );
        assert!(
            fixed.contains("\"selection_bg\":\"123456\""),
            "OSC 17 selection background lost: {fixed}"
        );
        assert!(
            fixed.contains("\"selection_fg\":\"fedcba\""),
            "OSC 19 selection foreground lost: {fixed}"
        );

        term.process(b"\x1b]112\x07\x1b]117\x07\x1b]119\x07");
        let reset = styled_frame_payload(&term);
        assert!(
            reset.contains("\"color\":\"default\"")
                && reset.contains("\"selection_bg\":\"default\"")
                && reset.contains("\"selection_fg\":\"dynamic\""),
            "OSC 112/117/119 reset policies must survive: {reset}"
        );
    }

    /// The searches `term`'s slot has built so far, and how many of them
    /// refreshed a kept index (`Terminal::search_index_rebuilds`,
    /// `search_index_refreshes`); a hit moves neither.
    fn builds(term: &Arc<Mutex<Terminal>>) -> (u64, u64) {
        let t = term_lock(term);
        (t.search_index_rebuilds(), t.search_index_refreshes())
    }

    /// P1.0c: a repeat query on unchanged content (a hit on the terminal's
    /// slot, nothing copied) returns byte-identical results, and a content write
    /// is answered by refreshing the kept index, not rebuilding it, so new
    /// content is found while the old needle's ABSOLUTE row survives the
    /// refresh unchanged; the refresh copies only the rows ordinary output can
    /// have changed.
    #[test]
    fn repeat_search_stable_and_write_invalidates() {
        let term = term_with(&["NEEDLE_alpha"]);
        for i in 0..20 {
            term.lock()
                .unwrap()
                .process(format!("filler {i}\r\n").as_bytes());
        }
        let r1 = cmd_search(&term, "NEEDLE_alpha", &RewrapGauge::default());
        assert!(r1.starts_with("OK 1"), "scrolled-off needle found: {r1}");
        let built = builds(&term);
        let r2 = cmd_search(&term, "NEEDLE_alpha", &RewrapGauge::default());
        assert_eq!(r1, r2, "cache-hit repeat must be byte-identical");
        assert!(cmd_search(&term, "NEEDLE_beta", &RewrapGauge::default()).starts_with("OK 0"));
        assert_eq!(
            builds(&term),
            built,
            "unchanged content is served from the slot"
        );
        term.lock().unwrap().process(b"NEEDLE_beta\r\n");
        let rb = cmd_search(&term, "NEEDLE_beta", &RewrapGauge::default());
        assert!(rb.starts_with("OK 1"), "post-write needle found: {rb}");
        assert_eq!(
            builds(&term),
            (built.0 + 1, built.1 + 1),
            "ordinary output refreshes the kept index"
        );
        let after = cmd_search(&term, "NEEDLE_alpha", &RewrapGauge::default());
        let (before_head, before_hits) = r1.split_once('\n').expect("header");
        let (after_head, after_hits) = after.split_once('\n').expect("header");
        assert!(before_head.ends_with(" screen_first=18"), "{r1}");
        assert!(after_head.ends_with(" screen_first=19"), "{after}");
        assert_eq!(
            search_header_without_frame(&after),
            search_header_without_frame(&r1)
        );
        assert_eq!(
            after_hits, before_hits,
            "absolute row of the old needle is stable across the refresh"
        );
        // A refresh names its rows in its first chunk's hold; one row read, the
        // rest still to copy.
        term.lock().unwrap().process(b"NEEDLE_gamma\r\n");
        let mut plan =
            term_lock(&term).search_plan(&super::search_request(super::search_max_lines(), None));
        term_lock(&term).read_search_chunk(&mut plan, 1);
        let named = plan.rows_to_read() + 1;
        assert!(
            named <= 6,
            "{named} rows: ordinary output refreshes the four visible rows, at most one \
             appended row, and the one row below the boundary that a soft-wrap run could \
             have reached into it from"
        );
    }

    /// A cached visible row can be edited and then become scrollback before
    /// the next query. Incremental refresh must start at the old visible
    /// boundary, not the new one, or that now-historical row remains stale.
    #[test]
    fn incremental_refresh_captures_an_edited_row_that_scrolled_away() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
        term.lock().unwrap().process(b"OLD_VISIBLE_TOKEN");
        assert!(
            cmd_search(&term, "OLD_VISIBLE_TOKEN", &RewrapGauge::default()).starts_with("OK 1")
        );

        // Replace the top visible row, then scroll it into history without an
        // intervening search/cache generation.
        term.lock()
            .unwrap()
            .process(b"\x1b[H\x1b[2KNEW_HISTORY_TOKEN\x1b[4;1H\r\n");

        assert!(
            cmd_search(&term, "NEW_HISTORY_TOKEN", &RewrapGauge::default()).starts_with("OK 1")
        );
        assert!(
            cmd_search(&term, "OLD_VISIBLE_TOKEN", &RewrapGauge::default()).starts_with("OK 0")
        );
    }

    /// A rows-grow that reveals history moves the visible boundary to lower rows, where the
    /// revealed rows are editable again; the refresh starts at the lower boundary, so a repaint
    /// over the revealed band is found on the row it is printed on.
    #[test]
    fn incremental_refresh_re_reads_rows_a_rows_grow_revealed_and_output_rewrote() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
        for i in 0..10 {
            term.lock().unwrap().process(format!("L{i}\r\n").as_bytes());
        }
        assert!(
            cmd_search(&term, "L4", &RewrapGauge::default()).starts_with("OK 1"),
            "primed"
        );

        // L3..L6 come back on screen as rows 0..3, then a full-viewport
        // repaint (conhost's shape: `CSI H`, `<text> CSI K` per row) paints the
        // pre-grow viewport over them.
        term.lock().unwrap().resize(8, 20);
        assert_eq!(term.lock().unwrap().grid().scrollback_lines(), 3);
        let mut repaint = b"\x1b[HL7\x1b[K\r\nL8\x1b[K\r\nL9\x1b[K".to_vec();
        repaint.extend_from_slice(&b"\r\n\x1b[K".repeat(5));
        repaint.extend_from_slice(b"\x1b[4;1H");
        term.lock().unwrap().process(&repaint);

        assert!(
            cmd_search(&term, "L4", &RewrapGauge::default()).starts_with("OK 0"),
            "a line the repaint overwrote is gone"
        );
        let hit = cmd_search(&term, "L9", &RewrapGauge::default());
        let mut lines = hit.lines();
        assert_eq!(lines.next(), Some("OK 1 screen_first=3"), "{hit}");
        let row: u64 = lines
            .next()
            .and_then(|l| l.split(' ').next())
            .and_then(|r| r.parse().ok())
            .expect("a match row");
        assert_eq!(
            row_text(&term, row),
            "L9",
            "reported on the row it is printed on"
        );
    }

    /// With `ambiguous_width = "wide"` the grid draws an East Asian Ambiguous
    /// character (`→`) in two cells, and find measures the same way: `foo` after
    /// two arrows is reported in cells 4-6, and a soft-wrapped run whose head is
    /// four arrows and `ab` fills its row, so the join adds no padding and
    /// `abfoo` straddles the wrap at the cell `a` is drawn in.
    #[test]
    fn find_measures_ambiguous_characters_as_the_grid_draws_them() {
        let wide = aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        };
        let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
        term.lock().unwrap().apply_config(&wide);
        term.lock().unwrap().process("→→foo bar".as_bytes());
        assert_eq!(
            cmd_search(&term, "foo", &RewrapGauge::default()),
            "OK 1 screen_first=0\n0 4 3\n"
        );

        let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
        term.lock().unwrap().apply_config(&wide);
        term.lock().unwrap().process("→→→→abfoo".as_bytes());
        assert!(
            term.lock().unwrap().row_continues_previous(1),
            "precondition: `foo` wrapped"
        );
        assert_eq!(
            cmd_search(&term, "abfoo", &RewrapGauge::default()),
            "OK 1 screen_first=0\n0 8 5\n"
        );
    }

    /// `search` reports a hit in the cells the grid drew it in: flag halves drawn right one
    /// first stay two cells, so `XYZ` is at column 4.
    #[test]
    fn search_reports_a_hit_where_the_grid_drew_it() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
        term.lock()
            .unwrap()
            .process("\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ".as_bytes());
        assert_eq!(
            term.lock().unwrap().cell_grapheme(0, 4).as_deref(),
            Some("X"),
            "precondition: the grid draws X in column 4"
        );
        assert_eq!(
            cmd_search(&term, "XYZ", &RewrapGauge::default()),
            "OK 1 screen_first=0\n0 4 3\n"
        );
    }

    /// A live `ambiguous_width` switch moves no content generation, and the
    /// terminal's kept index is neither served nor extended where a fresh one
    /// would not equal it: the first search after the switch answers as a fresh
    /// index over the same terminal does (one that never searched before).
    ///
    /// A row written before the switch keeps the cells it was drawn in
    /// (`apply_config` reflows nothing) and find reads those cells, so the
    /// fresh answer is where the grid paints `foo`: after two one-cell arrows.
    /// (r15 content-5: until find read the grid's cells it
    /// re-measured such a row under the new policy and put `foo` two cells
    /// right of where it is drawn; this test pinned that as its precondition.)
    #[test]
    fn a_live_ambiguous_width_switch_is_not_served_from_the_find_cache() {
        let wide = aterm_core::config::TerminalConfig {
            ambiguous_width_double: true,
            ..Default::default()
        };
        let cached = Arc::new(Mutex::new(Terminal::new(4, 20)));
        cached.lock().unwrap().process("→→foo bar".as_bytes());
        let narrow = cmd_search(&cached, "foo", &RewrapGauge::default());
        cached.lock().unwrap().apply_config(&wide);
        let after_switch = cmd_search(&cached, "foo", &RewrapGauge::default());

        let fresh = Arc::new(Mutex::new(Terminal::new(4, 20)));
        fresh.lock().unwrap().process("→→foo bar".as_bytes());
        fresh.lock().unwrap().apply_config(&wide);
        let fresh = cmd_search(&fresh, "foo", &RewrapGauge::default());
        assert_eq!(
            fresh, "OK 1 screen_first=0\n0 2 3\n",
            "where the grid paints it"
        );
        assert_eq!(narrow, fresh, "a row keeps the cells it was drawn in");
        assert_eq!(after_switch, fresh);
    }

    /// Find answers the same for a line on the screen and after it scrolls into
    /// history: both are read as written, trailing blanks kept, so `foo$` and a
    /// query ending in the spaces the program printed do not change their hits
    /// as output scrolls (absolute rows do not move on a scroll).
    #[test]
    fn a_lines_hits_do_not_change_when_it_scrolls_into_history() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
        term.lock()
            .unwrap()
            .process(b"foo   \r\nbar \x1b[41m  \x1b[0m\r\nbaz\r\nfoo");
        // `search` trims its pattern, so the trailing spaces are spelled as a
        // regex repeat.
        let queries = ["foo$ regex", "bar$ regex", "foo {2} regex", "bar {3} regex"];
        let ask = |q: &str| cmd_search(&term, q, &RewrapGauge::default());
        let on_screen: Vec<String> = queries.iter().map(|q| ask(q)).collect();
        assert_eq!(
            on_screen[0], "OK 1 screen_first=0\n3 0 3\n",
            "only the line ending at `foo`"
        );
        assert_eq!(
            on_screen[2], "OK 1 screen_first=0\n0 0 5\n",
            "the printed spaces are text"
        );
        term.lock().unwrap().process(b"\r\n1\r\n2\r\n3\r\n4");
        let in_history: Vec<String> = queries.iter().map(|q| ask(q)).collect();
        for (before, after) in on_screen.iter().zip(&in_history) {
            let (before_head, before_rows) = before.split_once('\n').expect("header");
            let (after_head, after_rows) = after.split_once('\n').expect("header");
            assert!(before_head.ends_with(" screen_first=0"), "{before}");
            assert!(after_head.ends_with(" screen_first=4"), "{after}");
            assert_eq!(
                before_head
                    .split_once(" screen_first=")
                    .map(|(head, _)| head),
                after_head
                    .split_once(" screen_first=")
                    .map(|(head, _)| head),
            );
            assert_eq!(after_rows, before_rows, "absolute hit coordinates stay put");
        }
    }

    /// Read one absolute row back as the grid renders it, so a test can say
    /// which row a token is printed on rather than trusting the search it is
    /// checking.
    fn row_text(term: &Arc<Mutex<Terminal>>, abs: u64) -> String {
        use aterm_core::terminal::AbsRowText;
        let row = term_lock(term).abs_row_text_within(abs, usize::MAX);
        match row {
            AbsRowText::Text(s) => s,
            AbsRowText::Evicted | AbsRowText::OutOfRange | AbsRowText::Unreadable => {
                "<evicted>".to_string()
            }
        }
    }

    /// A row erased with `CSI 2 K` trims to the empty string, yet the erase does
    /// NOT clear the FOLLOWING row's continuation flag — the run still spans it,
    /// and its blank cells are real columns the reader's line runs through. The
    /// joined line has to hold that whole width, so the stride is counted per
    /// ROW; a count derived from the running column total modulo the width would
    /// pad a blank row by nothing and lose its entire stride, inverting every
    /// later hit on the run onto a grid row too high. Find would then scroll to
    /// and paint the blank row while the real match stayed dark.
    ///
    /// The same stride, lost at the run's HEAD rather than inside it: an erased
    /// first row still occupies the width its continuation is offset by.
    #[test]
    fn a_blank_row_inside_a_soft_wrap_run_still_holds_its_width_of_the_line() {
        for (shape, text, erase, blank, tail, hit) in [
            (
                "the middle row",
                &b"0123456789ABCDEFGHIJKLMNOPQRST"[..],
                &b"\x1b[2;1H\x1b[2K"[..],
                1,
                "KLMNOPQRST",
                "OK 1 screen_first=0\n2 0 10\n",
            ),
            (
                "the head row",
                &b"0123456789ABCDEFGHIJ"[..],
                &b"\x1b[1;1H\x1b[2K"[..],
                0,
                "ABCDEFGHIJ",
                "OK 1 screen_first=0\n1 0 10\n",
            ),
        ] {
            let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
            term.lock().unwrap().process(text);
            term.lock().unwrap().process(erase);
            assert_eq!(row_text(&term, blank), "", "{shape} really is blank");
            assert_eq!(row_text(&term, blank + 1), tail, "{shape}: printed below");
            assert_eq!(
                cmd_search(&term, tail, &RewrapGauge::default()),
                hit,
                "{shape}: the hit is reported on the row it is printed on"
            );
        }
    }

    /// Regex `^`/`$` bind to the logical line a soft wrap split into rows: a continuation row
    /// has no `^`, and a character the wrap pushed to a row's end is not at `$`.
    #[test]
    fn regex_anchors_bind_to_the_logical_line_not_the_rows_a_soft_wrap_split_it_into() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
        // One 20-column logical line, laid out as grid rows 0 and 1.
        term.lock().unwrap().process(b"0123456789ABCDEFGHIJ");

        // Both wrap-interior positions: the far side of the boundary is not a
        // beginning, and its near side is not an end.
        assert_eq!(
            cmd_search(&term, "^ABC regex", &RewrapGauge::default()),
            "OK 0 screen_first=0\n",
            "a continuation row's column 0 is not where the line begins"
        );
        assert_eq!(
            cmd_search(&term, "9$ regex", &RewrapGauge::default()),
            "OK 0 screen_first=0\n",
            "the column the wrap pushed to a row's end is not where the line ends"
        );

        // The line's own ends do anchor, and a match past the boundary is still
        // re-expressed on the physical row it occupies.
        assert_eq!(
            cmd_search(&term, "^0123 regex", &RewrapGauge::default()),
            "OK 1 screen_first=0\n0 0 4\n"
        );
        assert_eq!(
            cmd_search(&term, "J$ regex", &RewrapGauge::default()),
            "OK 1 screen_first=0\n1 9 1\n"
        );
        assert_eq!(
            cmd_search(
                &term,
                "^0123456789ABCDEFGHIJ$ regex",
                &RewrapGauge::default()
            ),
            "OK 1 screen_first=0\n0 0 20\n",
            "anchored end to end, the run is ONE line whose len counts through the wrap"
        );

        // An unwrapped row is its own logical line, so its row edges anchor: the rebinding
        // follows the wrap, not the grid.
        term.lock().unwrap().process(b"\x1b[3;1Hxyz");
        assert_eq!(
            cmd_search(&term, "^xyz$ regex", &RewrapGauge::default()),
            "OK 1 screen_first=0\n2 0 3\n"
        );
    }

    /// Each terminal keeps its own index: a different session searched
    /// immediately after must never be answered from the first session's.
    #[test]
    fn search_index_is_not_reused_across_terminals() {
        let a = term_with(&["NEEDLE_shared", "pad a"]);
        assert!(cmd_search(&a, "NEEDLE_shared", &RewrapGauge::default()).starts_with("OK 1"));
        let b = term_with(&["nothing here", "pad b"]);
        assert!(
            cmd_search(&b, "NEEDLE_shared", &RewrapGauge::default()).starts_with("OK 0"),
            "terminal B must not see terminal A's cached index"
        );
        assert!(cmd_search(&a, "NEEDLE_shared", &RewrapGauge::default()).starts_with("OK 1"));
    }

    /// An alt-screen swap changes the active grid, so the main-screen index must not answer for
    /// the alt screen, and vice versa.
    #[test]
    fn alt_screen_swap_invalidates_cache() {
        let term = term_with(&["NEEDLE_main", "pad"]);
        assert!(cmd_search(&term, "NEEDLE_main", &RewrapGauge::default()).starts_with("OK 1"));
        term.lock().unwrap().process(b"\x1b[?1049h");
        assert!(
            cmd_search(&term, "NEEDLE_main", &RewrapGauge::default()).starts_with("OK 0"),
            "alt screen active: main-grid content is not addressable"
        );
        term.lock().unwrap().process(b"\x1b[?1049l");
        assert!(cmd_search(&term, "NEEDLE_main", &RewrapGauge::default()).starts_with("OK 1"));
    }

    /// SEARCH DEPTH CAP (fix #3/#5): the configured cap evicts the OLDEST scrollback and
    /// honestly flags results `incomplete`, YET the floor keeps the WHOLE live screen
    /// searchable under any cap (fix #2: the floor is `max_cached_for_retained(visible_rows)`,
    /// NOT `visible_rows`, because the engine evicts to a 3/4 low-water mark — a
    /// `visible_rows` floor would lose the screen's TOP row) — and the cap is part of the
    /// index key, so lowering then raising it rebuilds rather than stale-serving. Each search
    /// names its own cap ([`super::cmd_search_capped`]), so no App applying its config can
    /// move it.
    #[test]
    fn search_cap_evicts_scrollback_but_floors_at_the_live_screen() {
        // A 4-row terminal: OLDNEEDLE buried deep in scrollback, TOPVIS on the TOP visible
        // row (3rd-from-last after the trailing newline scroll).
        let mut lines: Vec<String> = vec!["OLDNEEDLE".to_string()];
        lines.extend((0..40).map(|i| format!("filler {i}")));
        lines.extend(["TOPVIS", "z1", "z2"].map(str::to_string));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let term = term_with(&refs);
        let full = |needle: &str| {
            super::cmd_search_capped(
                &term,
                needle,
                super::DEFAULT_MAX_CACHED_LINES,
                RewrapReading::default,
            )
        };

        // PRECONDITION: under a full cap TOPVIS's absolute row is base_y — it really is
        // the top on-screen row, not scrollback.
        let top = full("TOPVIS");
        let abs: u64 = top
            .lines()
            .nth(1)
            .and_then(|l| l.split(' ').next())
            .and_then(|n| n.parse().ok())
            .expect("match row");
        assert_eq!(abs, term.lock().unwrap().grid().base_y() as u64, "{top}");

        // Tiny cap (1): deep scrollback evicted and flagged, the whole screen kept.
        let old = super::cmd_search_capped(&term, "OLDNEEDLE", 1, RewrapReading::default);
        assert!(
            old.starts_with("OK 0") && old.contains("incomplete"),
            "deep-scrollback needle evicted under a tiny cap, honestly flagged: {old}"
        );
        let tiny = super::cmd_search_capped(&term, "TOPVIS", 1, RewrapReading::default);
        assert!(
            tiny.starts_with("OK 1"),
            "the top visible row is still found under a tiny cap (the retained floor): {tiny}"
        );

        // Cache-key proof: raising the cap re-finds the deep needle. A stale tiny index keyed
        // without the depth would still answer OK 0; a rebuild at the new depth finds it.
        let deep = full("OLDNEEDLE");
        assert!(
            deep.starts_with("OK 1") && !deep.contains("incomplete"),
            "raising the cap rebuilds at the new depth, complete: {deep}"
        );
    }

    /// `incomplete` describes this query's index: after a `clear` (`ED 3`) the whole retained
    /// buffer fits under the cap, so a reused index must stop saying `incomplete`.
    #[test]
    fn a_clear_ends_the_incomplete_qualifier_instead_of_latching_it() {
        let owned: Vec<String> = (0..200).map(|line| format!("row {line}")).collect();
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let term = term_with(&refs);

        let partial = super::cmd_search_capped(&term, "row 199", 16, RewrapReading::default);
        assert!(
            partial.contains("incomplete"),
            "history deeper than the cap is honestly partial: {partial}"
        );

        // `clear`: home, erase the display, erase the scrollback.
        term.lock()
            .unwrap()
            .process(b"\x1b[H\x1b[2J\x1b[3JNEEDLE\r\n");

        let exact = super::cmd_search_capped(&term, "NEEDLE", 16, RewrapReading::default);
        assert!(
            exact.starts_with("OK 1"),
            "the post-clear needle is found: {exact}"
        );
        assert!(
            !exact.contains("incomplete"),
            "nothing is left un-searched, so the count is stated plainly: {exact}"
        );
        let found =
            super::search_full_history_capped(&term, "NEEDLE", false, false, 16).expect("search");
        assert!(!found.results.incomplete);
        assert_eq!(found.searched_lines, None, "the whole buffer was searched");
        assert_eq!(
            found.results.lowest_retained_line, 0,
            "an exhaustive index reports no retained-history watermark"
        );
    }

    /// The count and cause tokens, with the additive absolute-frame field
    /// separated from them. Full wire fixtures below check the field itself.
    fn search_header_without_frame(reply: &str) -> &str {
        let header = reply.lines().next().expect("search header");
        let (head, first) = header.split_once(" screen_first=").expect("search frame");
        first.parse::<u64>().expect("absolute screen row");
        head
    }

    /// The header names WHY a list is not exhaustive when it is not the depth
    /// cap: a full list (`capped`) over a buffer inside the cap, and a search
    /// a rewrap overlapped (`rewrapping`), which may have reached only the
    /// screen and must not read as a definite miss.
    #[test]
    fn the_search_header_names_the_match_limit_and_the_rewrap() {
        let row = "a".repeat(40);
        let rows = super::MAX_SEARCH_MATCHES / 40 + 100;
        let owned: Vec<&str> = (0..rows).map(|_| row.as_str()).collect();
        let term = term_with(&owned);
        let full = super::cmd_search_capped(
            &term,
            "a",
            super::DEFAULT_MAX_CACHED_LINES,
            RewrapReading::default,
        );
        let header = search_header_without_frame(&full);
        assert_eq!(
            header,
            format!("OK {} incomplete capped", super::MAX_SEARCH_MATCHES)
        );
        let found = super::search_full_history_capped(
            &term,
            "a",
            false,
            false,
            super::DEFAULT_MAX_CACHED_LINES,
        )
        .expect("search");
        assert_eq!(
            found.searched_lines, None,
            "the match limit is not a history cut"
        );

        let small = term_with(&["needle"]);
        let cap = super::DEFAULT_MAX_CACHED_LINES;
        let gauge = Arc::new(RewrapGauge::default());
        let held = gauge.begin_job(1);
        let away = super::cmd_search_capped(&small, "missing", cap, || gauge.reading());
        assert_eq!(away, "OK 0 incomplete rewrapping screen_first=0\n");
        held.finish(WaitEnd::Done);
        let home = super::cmd_search_capped(&small, "missing", cap, || gauge.reading());
        assert_eq!(
            home, "OK 0 screen_first=0\n",
            "a job that ended before the search is no overlap"
        );
    }

    /// A megabyte soft-wrapped run (a `cat` of a minified bundle) is searched in full, its regex
    /// budget scaling with the run; a pattern that still cannot finish a run is
    /// `incomplete long-line` with every other line's matches, never an invalid pattern.
    #[test]
    fn a_long_soft_wrapped_run_is_searched_and_one_left_unfinished_is_named() {
        let cap = super::DEFAULT_MAX_CACHED_LINES;
        let unit = "function a(b){return b+1};var needle_x=[1,2,3];";
        let len = 1_000_000;
        let mut bundle = unit.repeat(len / unit.len() + 1);
        bundle.truncate(len);
        let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
        term.lock()
            .unwrap()
            .process(format!("$ cat bundle.js\r\n{bundle}\r\n$ echo x=1\r\n").as_bytes());
        let reply = super::cmd_search_capped(&term, "[a-z]+= regex", cap, RewrapReading::default);
        let header = search_header_without_frame(&reply);
        let count: usize = header
            .strip_prefix("OK ")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("a complete count, not a refusal: {header}"));
        // One `x=` per unit of the bundle, and the prompt line's.
        assert!(count > len / unit.len(), "{header}");

        let long = "x".repeat(70_000);
        let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
        term.lock()
            .unwrap()
            .process(format!("before z\r\n{long}\r\nafter z\r\n").as_bytes());
        let reply = super::cmd_search_capped(
            &term,
            "(?:x?){1020}z case regex",
            cap,
            RewrapReading::default,
        );
        let mut lines = reply.lines();
        assert_eq!(
            search_header_without_frame(lines.next().expect("header")),
            "OK 2 incomplete long-line",
            "{reply}"
        );
        assert_eq!(
            lines.map(|line| line.split(' ').nth(1)).collect::<Vec<_>>(),
            vec![Some("7"), Some("6")],
            "the short lines around the unfinished run are still searched: {reply}"
        );
    }

    /// The header names every cause that applies: a depth cap beside `long-line` or `capped`
    /// adds ` older`; the depth cap alone stays a bare ` incomplete`.
    #[test]
    fn the_search_header_names_the_depth_cap_beside_another_cause() {
        let header = |reply: &str| search_header_without_frame(reply).to_string();
        let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
        let mut text = String::from("old z\r\n");
        for i in 0..400 {
            text.push_str(&format!("filler {i}\r\n"));
        }
        text.push_str(&"x".repeat(70_000));
        text.push_str("\r\nafter z\r\n");
        term.lock().unwrap().process(text.as_bytes());
        let costly = "(?:x?){1020}z case regex";
        let deep = super::cmd_search_capped(&term, costly, 200, RewrapReading::default);
        assert_eq!(header(&deep), "OK 1 incomplete long-line older", "{deep}");
        let whole = super::cmd_search_capped(
            &term,
            costly,
            super::DEFAULT_MAX_CACHED_LINES,
            RewrapReading::default,
        );
        assert_eq!(header(&whole), "OK 2 incomplete long-line", "{whole}");
        let plain = super::cmd_search_capped(&term, "z case regex", 200, RewrapReading::default);
        assert_eq!(header(&plain), "OK 1 incomplete", "{plain}");

        // A full list under the depth cap: every row holds 40 hits, so the
        // newest `cap` rows alone fill the list, and older rows are left out.
        let row = "a".repeat(40);
        let rows = super::MAX_SEARCH_MATCHES / 40 + 100;
        let owned: Vec<&str> = (0..rows).map(|_| row.as_str()).collect();
        let term = term_with(&owned);
        let cap = super::MAX_SEARCH_MATCHES / 40 + 10;
        let full = super::cmd_search_capped(&term, "a", cap, RewrapReading::default);
        assert_eq!(
            header(&full),
            format!("OK {} incomplete capped older", super::MAX_SEARCH_MATCHES)
        );
    }

    /// A width change that detaches history after the search's first gauge reading and
    /// re-attaches it before the second leaves the gauge idle at both; the moved job count
    /// still marks the short list (the search read only the screen).
    #[test]
    fn a_rewrap_inside_the_search_marks_its_short_list() {
        let mut t = Terminal::with_scrollback(
            4,
            40,
            8,
            aterm_scrollback::Scrollback::new(64, 512, 8_000_000),
        );
        let mut buf = b"NEEDLE\r\n".to_vec();
        for i in 0..200 {
            buf.extend_from_slice(format!("filler {i}\r\n").as_bytes());
        }
        t.process(&buf);
        let term = Arc::new(Mutex::new(t));
        let cap = super::DEFAULT_MAX_CACHED_LINES;
        let gauge = Arc::new(RewrapGauge::default());
        let away = RefCell::new(None);
        let reply = super::cmd_search_capped(&term, "NEEDLE", cap, || {
            let taken = away.borrow_mut().take();
            match taken {
                // Before the history: the resize paths detach it and book the
                // job in one hold.
                None => {
                    let now = gauge.reading();
                    let mut t = term_lock(&term);
                    let pending = t
                        .resize_offloading_scrollback(4, 30)
                        .expect("a width change detaches the tiered history");
                    let job = gauge.begin_job(pending.lines_total());
                    *away.borrow_mut() = Some((pending, job));
                    now
                }
                // After it: the rewrap re-attached and ended first.
                Some((pending, job)) => {
                    let next = term_lock(&term).finish_resize_offload(pending.reflow());
                    assert!(next.is_none(), "one pass");
                    job.finish(WaitEnd::Done);
                    gauge.reading()
                }
            }
        });
        assert!(!gauge.reading().running, "idle at both readings");
        assert_eq!(
            search_header_without_frame(&reply),
            "OK 0 incomplete rewrapping"
        );
        assert_eq!(reply.lines().count(), 1, "no matches in detached history");
        let home = super::cmd_search_capped(&term, "NEEDLE", cap, || gauge.reading());
        assert!(
            home.starts_with("OK 1 screen_first="),
            "the re-attached history holds the needle: {home}"
        );
    }

    /// A cold build copies only the suffix the configured depth cap retains:
    /// the newest `cap` rows. The omitted prefix is still surfaced as
    /// incomplete history with an absolute retained watermark.
    #[test]
    fn cold_search_snapshot_work_is_bounded_by_retained_cap() {
        let owned: Vec<String> = (0..200).map(|line| format!("row {line}")).collect();
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let term = term_with(&refs);

        let plan = term_lock(&term).search_plan(&super::search_request(16, None));
        assert_eq!(
            plan.rows_to_read(),
            16,
            "cold copy must never exceed the configured retained suffix"
        );
        drop(plan);
        let found = super::search_full_history_capped(&term, "row 199", false, false, 16)
            .expect("bounded cold search");
        assert!(found.consistent);
        assert!(found.results.incomplete);
        assert!(found.results.lowest_retained_line > 0);
        assert_eq!(
            found.searched_lines,
            Some(16),
            "the find surfaces learn how far it searched"
        );
    }

    /// Each terminal keeps its own index, so searching another terminal leaves
    /// the first one's in place: switching back is a hit. NEGATIVE CONTROL: a
    /// write to the first terminal in between is not.
    #[test]
    fn alternating_terminal_searches_reuse_bounded_per_terminal_cache() {
        for write in [false, true] {
            let first = term_with(&["first needle"]);
            let second = term_with(&["second needle"]);
            super::search_full_history(&first, "needle", false, false).unwrap();
            let built = builds(&first);
            super::search_full_history(&second, "needle", false, false).unwrap();
            if write {
                first.lock().unwrap().process(b"more\r\n");
            }
            let repeat = super::search_full_history(&first, "needle", false, false).unwrap();
            assert!(repeat.consistent);
            assert_eq!(
                builds(&first).0 - built.0,
                u64::from(write),
                "write={write}: switching back to a recent terminal reuses its index"
            );
        }
    }

    /// Whether `term`'s slot still keeps an index of the configured depth: the
    /// next search plans a hit. A miss frees the slot, so ask last.
    fn kept(term: &Arc<Mutex<Terminal>>) -> bool {
        let request = super::search_request(super::search_max_lines(), None);
        term_lock(term).search_plan(&request).is_hit()
    }

    /// Past [`super::SEARCH_RESIDENT_CAP`] terminals, the least recently
    /// searched one gives its index back, and the others keep theirs.
    /// NEGATIVE CONTROL: searching the oldest again before the ninth keeps its
    /// index, and the next oldest gives its index back instead.
    #[test]
    fn the_ninth_searched_terminal_releases_the_least_recent_index() {
        let cap = super::SEARCH_RESIDENT_CAP;
        for search_oldest_again in [false, true] {
            let terms: Vec<_> = (0..=cap)
                .map(|i| term_with(&[format!("needle {i}").as_str()]))
                .collect();
            let search = |term: &Arc<Mutex<Terminal>>| {
                super::search_full_history(term, "needle", false, false).expect("searched");
            };
            for term in &terms[..cap] {
                search(term);
            }
            if search_oldest_again {
                search(&terms[0]);
            }
            search(&terms[cap]);
            let released = usize::from(search_oldest_again);
            for (i, term) in terms.iter().enumerate() {
                assert_eq!(
                    kept(term),
                    i != released,
                    "search_oldest_again={search_oldest_again}: terminal {i}"
                );
            }
        }
    }

    /// The ledger counts only live terminals: a dropped terminal's index died
    /// with it, so it leaves without counting against the cap. NEGATIVE
    /// CONTROL: alive, it is the one that falls past the cap.
    #[test]
    fn a_dropped_terminal_leaves_the_residency_ledger_uncounted() {
        for drop_first in [false, true] {
            let ledger = super::SearchResidency::new(2);
            let mut first = Some(term_with(&[]));
            let (second, third) = (term_with(&[]), term_with(&[]));
            assert!(ledger.touch(first.as_ref().expect("alive")).is_none());
            assert!(ledger.touch(&second).is_none());
            if drop_first {
                first = None;
            }
            let evicted = ledger.touch(&third);
            assert_eq!(evicted.is_some(), !drop_first, "drop_first={drop_first}");
            if let (Some(evicted), Some(first)) = (&evicted, &first) {
                assert!(Arc::ptr_eq(evicted, first));
            }
        }
    }

    /// The find bar and the `search` verb read one index per terminal: a find
    /// with its anchor, then `search` over the same terminal, builds once.
    /// NEGATIVE CONTROL: output in between makes the verb's search build (a
    /// refresh of the find's index).
    #[test]
    fn find_and_socket_share_one_index() {
        for write in [false, true] {
            let term = term_with(&["needle one", "other", "needle two"]);
            let found = super::search_full_history_direction(
                &term,
                "needle",
                false,
                false,
                super::EngineSearchDirection::Forward,
                Some((1, 0)),
                false,
            )
            .expect("find");
            assert_eq!(found.results.matches.len(), 2);
            let built = builds(&term);
            if write {
                term.lock().unwrap().process(b"needle three\r\n");
            }
            let reply = cmd_search(&term, "needle", &RewrapGauge::default());
            let header = if write { "OK 3 " } else { "OK 2 " };
            assert!(reply.starts_with(header), "write={write}: {reply}");
            assert_eq!(
                builds(&term),
                (built.0 + u64::from(write), built.1 + u64::from(write)),
                "write={write}"
            );
        }
    }

    /// A terminal that moves while a search's answer runs off the lock is
    /// reported, and the result keeps the stamps of the key it was planned
    /// under. Output leaves the frame standing, so only `consistent` drops and
    /// the `search` header is exact (`OK 1`): absolute rows stay put as lines
    /// scroll into history. A screen switch or a width change moves the frame,
    /// and the header names it (`OK 1 incomplete moving`) for the client to
    /// decide whether to search again. Cold builds and hits alike, batch and
    /// point results alike. NEGATIVE CONTROL: with no change the result is
    /// consistent, its frame stood, and the header is `OK 1`.
    #[test]
    fn a_search_raced_off_the_lock_reports_the_verdict_under_the_planned_stamps() {
        type Race = fn(&mut Terminal);
        let races: [(&str, Option<Race>, bool); 4] = [
            ("none", None, true),
            ("output", Some(|t| t.process(b"more\r\n")), true),
            ("alt switch", Some(|t| t.process(b"\x1b[?1049h")), false),
            ("width", Some(|t| t.resize(4, 30)), false),
        ];
        let depth = super::search_max_lines();
        for (label, race, frame_stable) in races {
            for (warm, point) in [(false, false), (true, false), (false, true), (true, true)] {
                let case = format!("{label} warm={warm} point={point}");
                let term = term_with(&["needle", "other"]);
                if warm {
                    let _ = super::search_full_history(&term, "needle", false, false).expect(&case);
                }
                let planned = super::SearchKey::capture(&term_lock(&term), depth);
                let planned_stamps = (
                    i64::try_from(planned.window().visible_base).unwrap(),
                    planned.row_revision,
                    planned.renumber_epoch,
                    planned.content_seq,
                    usize::from(planned.cols),
                );
                let request = super::search_request(depth, Some((0, 0)));
                let searched = super::search_terminal(&term, &request, |built, hit| {
                    assert_eq!(hit, warm, "{case}: precondition");
                    if let Some(race) = race {
                        race(&mut term_lock(&term));
                    }
                    built.query(
                        "needle",
                        false,
                        false,
                        super::EngineSearchDirection::Forward,
                        false,
                    )
                });
                if point {
                    let found = super::full_history_point(super::Searched {
                        answer: searched.answer.map(|(_, point_match)| point_match),
                        key: searched.key,
                        wrap: searched.wrap,
                        verdict: searched.verdict,
                    })
                    .expect(&case);
                    assert_eq!(found.consistent, race.is_none(), "{case}");
                    assert_eq!(
                        (
                            found.base_y,
                            found.absolute_row_revision,
                            found.history_renumber_epoch,
                            found.content_seq,
                            found.cols,
                        ),
                        planned_stamps,
                        "{case}"
                    );
                    continue;
                }
                let found = super::full_history_search(searched).expect(&case);
                assert_eq!(
                    (found.consistent, found.frame_stable),
                    (race.is_none(), frame_stable),
                    "{case}"
                );
                assert_eq!(
                    (
                        found.base_y,
                        found.absolute_row_revision,
                        found.history_renumber_epoch,
                        found.content_seq,
                        found.cols,
                    ),
                    planned_stamps,
                    "{case}"
                );
                let header = format!(
                    "OK 1{} screen_first={}",
                    if frame_stable {
                        ""
                    } else {
                        " incomplete moving"
                    },
                    planned_stamps.0,
                );
                assert_eq!(
                    super::search_reply(Ok(found), false).lines().next(),
                    Some(header.as_str()),
                    "{case}"
                );
            }
        }
    }

    /// A point step answers a hit on the terminal's index as it answers a
    /// build: a regex that cannot finish a soft-wrapped run and matches nowhere
    /// else is no point either way (the batch path keeps its batch), never a
    /// refusal on the hit alone. NEGATIVE CONTROL: the lazy walk alone over the
    /// kept index refuses (`ScanCutShort`).
    #[test]
    fn a_point_step_answers_a_hit_as_it_answers_a_build() {
        let costly = "(?:x?){1020}z";
        let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
        term.lock()
            .unwrap()
            .process(format!("before\r\n{}\r\nafter\r\n", "x".repeat(70_000)).as_bytes());
        let point = || {
            super::search_full_history_point(
                &term,
                costly,
                true,
                true,
                super::EngineSearchDirection::Forward,
                (0, 0),
                true,
            )
            .map(|found| (found.point_match, found.consistent))
        };
        let built = point();
        assert_eq!(builds(&term).0, 1, "precondition: a build");
        let hit = point();
        assert_eq!(builds(&term).0, 1, "precondition: a hit");
        assert_eq!(built, Ok((None, true)));
        assert_eq!(hit, built);

        let request = super::search_request(super::search_max_lines(), Some((0, 0)));
        let walked = super::search_terminal(&term, &request, |built, hit| {
            assert!(hit, "precondition: a hit");
            built.index().find_direction_opts(
                costly,
                true,
                true,
                aterm_search::DirectedFind {
                    anchor: built.wrap().logical_anchor().unwrap_or((0, 0)),
                    direction: super::EngineSearchDirection::Forward,
                    inclusive: false,
                    wrap: true,
                },
            )
        });
        assert_eq!(walked.answer, Err(super::SearchOptionsError::ScanCutShort));
    }

    /// A mistyped argument is refused, never answered with the summary, so
    /// `metrics rest` cannot read as a successful reset while zeroing nothing.
    #[test]
    fn metrics_refuses_an_unknown_argument_and_resets_nothing() {
        crate::metrics::record_offscreen_raster(1_000_000);
        for arg in ["rest", "percentile", "reset now", "percentiles reset"] {
            assert_eq!(
                super::cmd_metrics(None, arg),
                super::METRICS_USAGE,
                "{arg:?}"
            );
            assert_eq!(
                super::cmd_metrics_json(None, arg),
                super::METRICS_USAGE,
                "{arg:?}"
            );
        }
        let summary = super::cmd_metrics(None, "");
        let rasters: u64 = summary
            .split("offscreen_rasters=")
            .nth(1)
            .and_then(|t| t.split_whitespace().next())
            .and_then(|t| t.parse().ok())
            .expect("offscreen_rasters is a number");
        assert!(rasters > 0, "a refused argument reset the counters");
    }

    /// The verb's routing: `metrics` and `metrics percentiles`, in text through
    /// `cmd_metrics`, and with `--json` taken off by the dispatch's
    /// `json_ask` and answered by `cmd_metrics_json`, carry exactly the keys
    /// their own emitter logs, in order, as one `OK` line or one `OK 1` framed
    /// object. The control: the two forms' key logs differ, so a form routed to
    /// the other's emitter or to the other syntax's sink fails here.
    #[test]
    fn each_metrics_form_reaches_the_wire_through_its_own_emitter_and_sink() {
        use crate::metrics::wire::{ReadCtx, law_tests};
        let ctx = ReadCtx::now();
        let forms = [
            (
                "",
                law_tests::body(|s| crate::metrics::emit_summary(&ctx, None, s)).keys,
            ),
            (
                "percentiles",
                law_tests::body(|s| crate::metrics::emit_percentiles(&ctx, s)).keys,
            ),
        ];
        assert_ne!(forms[0].1, forms[1].1);
        for (form, keys) in &forms {
            let reply = super::cmd_metrics(None, form);
            let line = reply
                .strip_suffix('\n')
                .filter(|line| line.starts_with("OK ") && !line.contains('\n'))
                .unwrap_or_else(|| panic!("{form:?}: not one OK line: {reply:?}"));
            let text_keys: Vec<&str> = law_tests::tokens(line).iter().map(|(k, _)| *k).collect();
            assert_eq!(text_keys, *keys, "{form:?}: {line}");
            let flagged = format!("{form} --json");
            let resolved = aterm_types::control_verbs::spec_for("metrics", &flagged)
                .expect("metrics is a verb");
            let body = match super::super::json_ask(&resolved, "metrics", &flagged) {
                super::super::JsonAsk::Emit(body) => body,
                other => panic!("{form:?}: the dispatch emits no JSON: {other:?}"),
            };
            for reply in [
                super::cmd_metrics_json(None, &body),
                super::cmd_metrics_json(None, form),
            ] {
                let object = reply
                    .strip_prefix("OK 1\n")
                    .and_then(|rest| rest.strip_suffix('\n'))
                    .filter(|object| object.starts_with('{') && !object.contains('\n'))
                    .unwrap_or_else(|| panic!("{form:?}: not one framed object: {reply:?}"));
                let json_keys: Vec<String> = law_tests::members(object)
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect();
                assert_eq!(json_keys, *keys, "{form:?}: {object}");
            }
        }
    }

    /// ITEM 6 wire shape, and the ledgers that copied it (the streak heals of
    /// items 18/19, the FULL-arm refill causes): each is self-labelling in both
    /// forms — a keyed OBJECT in JSON, one whitespace-free token in text
    /// (`none` when empty, never a bare `key=`, which a whitespace-splitting
    /// shell probe would misread) — so a reader never depends on position and
    /// a newly appended `DeadlineOwner` or `FullRefillCause` cannot shift
    /// anyone's column. `deadline_arms_by_owner` is spelled out in full so a
    /// naive `grep deadline_arms=` still finds only the global
    /// `past_deadline_arms`. The counters are process-global and shared by
    /// every test in this binary, so this asserts SHAPE, never counts.
    #[test]
    fn the_per_owner_ledgers_are_self_labelling_in_both_forms() {
        let reply = super::cmd_metrics_json(None, "");
        let body = reply
            .strip_prefix("OK 1\n")
            .expect("status frame")
            .trim_end();
        let value: aterm_json::Value = aterm_json::from_str(body).expect("valid metrics JSON");
        let text = super::cmd_metrics(None, "");
        for key in [
            "deadline_arms_by_owner",
            "past_arm_streak_heals",
            "frame_refill_full_causes",
        ] {
            let ledger = value
                .get(key)
                .unwrap_or_else(|| panic!("{key} is published"));
            let entries = ledger
                .as_object()
                .unwrap_or_else(|| panic!("{key} is a keyed object: {ledger}"));
            for (name, entry) in entries {
                assert!(!name.is_empty(), "{key}: every entry is named");
                let counts = if key == "deadline_arms_by_owner" {
                    vec![entry.get("arms"), entry.get("past")]
                } else {
                    vec![Some(entry)]
                };
                for count in counts {
                    assert!(
                        count.and_then(aterm_json::Value::as_u64).is_some(),
                        "{key}: {name} publishes its counts: {entry}"
                    );
                }
            }
            let prefix = format!("{key}=");
            let field = text
                .split_whitespace()
                .find_map(|tok| tok.strip_prefix(prefix.as_str()))
                .unwrap_or_else(|| panic!("the text form publishes {key}"));
            assert!(!field.is_empty(), "{key}: empty renders as `none`");
        }
        // Every refill cause the engine can produce survives the `a:1,b:2`
        // text encoding as a distinct token.
        for cause in aterm_core::render::FullRefillCause::ALL {
            let label = cause.as_str();
            assert!(
                !label.contains(',')
                    && !label.contains(':')
                    && !label.contains(char::is_whitespace),
                "`{label}` would break the `a:1,b:2` text encoding"
            );
        }
    }

    #[test]
    fn metrics_remain_available_while_the_terminal_mutex_is_busy() {
        let term = term_with(&[]);
        let _held = term.lock().unwrap();

        let text = super::cmd_metrics(Some(&term), "");
        assert!(
            text.contains("rows=busy cols=busy scrollback_truncated_lines=busy"),
            "text diagnostics identify only the unavailable grid fields: {text}"
        );
        assert!(text.contains("redraw_attempts="));

        let reply = super::cmd_metrics_json(Some(&term), "");
        let body = reply.strip_prefix("OK 1\n").unwrap().trim_end();
        let value: aterm_json::Value = aterm_json::from_str(body).unwrap();
        assert!(value["rows"].is_null());
        assert!(value["cols"].is_null());
        assert!(value["scrollback_truncated_lines"].is_null());
        assert!(value.get("redraw_attempts").is_some());
    }

    /// The plain batch layer over the index `term`'s slot keeps: the oracle the
    /// narrowing tests hold the per-keystroke path to. It is served from the
    /// slot and hands the narrowing frames back untouched.
    fn cached_batch(
        term: &Arc<Mutex<Terminal>>,
        q: &str,
        direction: super::EngineSearchDirection,
    ) -> (super::SearchResults, Option<super::SearchMatch>) {
        let request = super::search_request(super::search_max_lines(), None);
        let plan = term_lock(term).search_plan(&request);
        assert!(
            plan.is_hit(),
            "the previous search must have kept its index"
        );
        let built = plan.build();
        let found = aterm_core::terminal::query_search_index(
            built.index(),
            q,
            false,
            false,
            direction,
            None,
            false,
            built.wrap(),
        )
        .expect("literal batch query cannot fail");
        assert!(term_lock(term).finish_search(built).0.consistent);
        found
    }

    /// SA-1 cost guard: narrowing must not be used when the index would visit
    /// FEWER lines than the inherited frame holds — the keystroke that turns a
    /// common prefix into a rare query. Results stay equal to the batch layer
    /// either way; what this pins is that the expensive choice is not taken
    /// (the reach counter must NOT advance on that keystroke) while the
    /// ordinary common-prefix keystrokes still narrow.
    #[test]
    fn narrowing_skips_frames_bigger_than_the_engines_own_candidate_set() {
        let mut lines: Vec<String> = (0..40)
            .map(|i| format!("svc-api request completed size=100 seq={i}"))
            .collect();
        lines.push("svc-api request completed size=977 seq=rare".to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let term = term_with(&refs);
        let _ = take_narrowed_query_steps();

        let oracle = |q: &str| cached_batch(&term, q, super::EngineSearchDirection::Forward).0;
        let narrowed = |q: &str| {
            super::search_full_history_direction(
                &term,
                q,
                false,
                false,
                super::EngineSearchDirection::Forward,
                None,
                false,
            )
            .expect("search ok")
        };

        // Common prefix: frames no bigger than the engine's postings, so these narrow.
        for q in ["siz", "size", "size="] {
            let full = narrowed(q);
            assert_eq!(full.results, oracle(q), "keystroke {q:?}");
        }
        assert!(
            take_narrowed_query_steps() >= 1,
            "reach: the common-prefix keystrokes must narrow"
        );

        // The rare keystroke: the frame holds all 41 lines, the engine's candidate set one.
        let rare = narrowed("size=9");
        assert_eq!(rare.results, oracle("size=9"));
        assert_eq!(
            rare.results.matches.len(),
            1,
            "precondition: the grown query really is rare"
        );
        assert_eq!(
            take_narrowed_query_steps(),
            0,
            "cost guard: a frame bigger than the engine's candidate set must be skipped"
        );

        // ...and the reseeded small frame narrows again on the next keystroke.
        let deeper = narrowed("size=97");
        assert_eq!(deeper.results, oracle("size=97"));
        assert_eq!(
            take_narrowed_query_steps(),
            1,
            "the reseeded frame is small, so narrowing resumes immediately"
        );
    }

    /// A Kitty `CSI +T` unscroll slides every retained row's key one row and
    /// leaves `content_seq` standing. The index is rebuilt (the epoch is in its
    /// key), and the narrowing frames, whose rows were read before the slide,
    /// must die with it: a frame of pre-shift rows verifies the lines one row
    /// off, here the `beta` rows between the `alpha` ones, and loses every hit.
    /// Across the unscroll every other field the slot's key compares is
    /// unchanged, so only the epoch keeps that frame out. NEGATIVE CONTROL: the
    /// same keystrokes without the unscroll do narrow on the last one, so the
    /// cost guard is not what skips the frame.
    #[test]
    fn an_epoch_only_renumber_kills_the_narrowing_frames() {
        let forward = super::EngineSearchDirection::Forward;
        let run = |unscroll: bool| {
            // A tiered store under the ring: a ring-only grid routes an unscroll
            // to a plain region scroll.
            let mut t = Terminal::with_scrollback(
                4,
                40,
                8,
                aterm_scrollback::Scrollback::new(64, 512, 8_000_000),
            );
            for i in 0..40 {
                let line = if i % 2 == 0 {
                    format!("alpha-{i}\r\n")
                } else {
                    "beta\r\n".to_string()
                };
                t.process(line.as_bytes());
            }
            let term = Arc::new(Mutex::new(t));
            let search = |q: &str| {
                super::search_full_history_direction(&term, q, false, false, forward, None, false)
                    .expect("search ok")
            };
            // The build seeds the `alph` frame; the hit after it stacks `alpha`.
            search("alph");
            search("alpha");
            if unscroll {
                let cap = super::search_max_lines();
                let before = super::SearchKey::capture(&term_lock(&term), cap);
                term_lock(&term).process(b"\x1b[1+T");
                let after = super::SearchKey::capture(&term_lock(&term), cap);
                assert_ne!(before.renumber_epoch, after.renumber_epoch);
                let mut unslid = after;
                unslid.renumber_epoch = before.renumber_epoch;
                assert!(
                    before.same_generation(&unslid),
                    "precondition: the unscroll moves no generation field but the epoch"
                );
            }
            // The keystroke a frame of the `alpha` lines would serve. After the
            // slide it rebuilds the index, and the frames died with the old one.
            let _ = take_narrowed_query_steps();
            let grown = search("alpha-");
            let steps = take_narrowed_query_steps();
            assert_eq!(grown.results, cached_batch(&term, "alpha-", forward).0);
            assert_eq!(
                grown.results.matches.len(),
                20,
                "every alpha line, at its row after the slide"
            );
            steps
        };
        assert_eq!(
            run(false),
            1,
            "control: the stacked frame narrows the keystroke"
        );
        assert_eq!(run(true), 0, "a frame of pre-shift rows is never reused");
    }

    /// SA-1 wiring: typing a query one char at a time through the real
    /// cache-hit path narrows from the previous keystroke's occurrence frame
    /// — results equal the plain batch layer at EVERY step (the oracle is
    /// `query_search_index` over the same cached index), narrowing really
    /// engages (thread-local reach counter — immune to parallel tests on
    /// other terminals), and backspace, a non-suffix edit and the backward
    /// direction stay equal too.
    #[test]
    fn incremental_typing_narrows_and_equals_the_batch_layer() {
        let term = term_with(&[
            "plain ascii find line",
            // "\u{200b}" alone spans no display column and is dropped from reported matches,
            // while "\u{200b}b" crosses into the next cell: the zero-width hazard the
            // occurrence frame must survive.
            "a\u{200b}b zero width",
            "stra\u{df}e stop",
            "find find find",
            "no hits here",
        ]);
        let _ = take_narrowed_query_steps();

        let oracle = |q: &str| cached_batch(&term, q, super::EngineSearchDirection::Forward).0;
        let narrowed = |q: &str| {
            super::search_full_history_direction(
                &term,
                q,
                false,
                false,
                super::EngineSearchDirection::Forward,
                None,
                false,
            )
            .expect("search ok")
        };

        // Grow: f → fi → fin → find. The first call builds the index and seeds
        // the stack beside it; "fi", "fin" and "find" must run off frames.
        for q in ["f", "fi", "fin", "find"] {
            let full = narrowed(q);
            assert!(full.consistent, "no concurrent mutation in this test");
            assert_eq!(
                full.results,
                oracle(q),
                "narrowed keystroke {q:?} must equal the batch layer"
            );
        }
        assert!(
            take_narrowed_query_steps() >= 2,
            "reach: the grown keystrokes must have run off narrowing frames"
        );

        // Backspace to "fin": re-verifies off the stacked frame, still equal.
        let full = narrowed("fin");
        assert_eq!(full.results, oracle("fin"));
        assert_eq!(
            take_narrowed_query_steps(),
            1,
            "reach: the backspaced query must be served off its own frame"
        );

        // Non-suffix edit ("xfind" shares no stacked prefix): batch reseed, still equal.
        let full = narrowed("xfind");
        assert_eq!(full.results, oracle("xfind"));

        // "\u{200b}" reports no match (it spans no display column), yet "\u{200b}b" grown
        // from it finds the straddling match through the whole GUI path.
        let full_zw = narrowed("\u{200b}");
        assert_eq!(full_zw.results, oracle("\u{200b}"));
        assert!(
            full_zw.results.matches.is_empty(),
            "precondition: the zero-width character spans no display column, so \
             it reports no match anywhere in this corpus"
        );
        let full_zwb = narrowed("\u{200b}b");
        assert_eq!(full_zwb.results, oracle("\u{200b}b"));
        assert!(
            full_zwb.results.matches.iter().any(|m| m.line == 1),
            "the zero-width-straddling match on the zero-width line (abs row 1) \
             must survive narrowing"
        );

        // The find bar's BACKWARD lane ("find previous") runs through the same
        // layer: an uncapped narrowed walk — the complete ascending match set —
        // equals the backward batch path verbatim at every keystroke. (A capped
        // backward edge keeps the newest matches instead, which is why
        // `query_search_index_narrowing` falls back to the batch path there.)
        let _ = take_narrowed_query_steps();
        for q in ["f", "fi", "fin", "find", "find f"] {
            let full = super::search_full_history_direction(
                &term,
                q,
                false,
                false,
                super::EngineSearchDirection::Backward,
                None,
                false,
            )
            .expect("search ok");
            assert_eq!(
                full.results,
                cached_batch(&term, q, super::EngineSearchDirection::Backward).0,
                "backward keystroke {q:?} must equal the backward batch layer"
            );
        }
        assert!(
            take_narrowed_query_steps() >= 2,
            "reach: the backward lane must also run off narrowing frames"
        );
    }
}

/// The `trim` family: the one trailing-blank rule and the verbs that route through it.
#[cfg(test)]
mod trim_tests {
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::Terminal;

    use super::{
        ROWS_REVERSED, TEXT_USAGE, TextArgs, TextShape, cmd_blocktext_args, cmd_text_json_opt,
        frame_rows_reply, lines_reply, text_args,
    };

    /// `text` as a session with nothing published serves it.
    fn cmd_text_opt(term: &Arc<Mutex<Terminal>>, args: TextArgs) -> String {
        super::cmd_text_opt(term, &crate::frame_memo::FrameSlot::default(), args)
    }

    /// `lines` stays the bare `OK <n>` until history has actually been lost,
    /// and then says so — the field can never appear for a complete history
    /// and can never be absent for a cut one.
    #[test]
    fn lines_reply_names_truncation_only_when_something_was_lost() {
        assert_eq!(lines_reply(0, 0), "OK 0\n");
        assert_eq!(lines_reply(28_171, 0), "OK 28171\n");
        assert_eq!(lines_reply(28_171, 91_357), "OK 28171 truncated=91357\n");
        assert_eq!(lines_reply(0, 1), "OK 0 truncated=1\n");
    }

    /// `text trim`: the whole grid, minus its blank tail.
    const TRIM: TextArgs = TextArgs {
        trim: true,
        shape: TextShape::All,
        display: false,
    };

    /// The parsed form of one argument tail, for a tail the test knows is valid.
    fn args(rest: &str) -> TextArgs {
        text_args(rest).unwrap_or_else(|e| panic!("{rest:?} parses: {e}"))
    }

    /// `order=display` (INTROSPECTION F2): text and pixels disagreed on a
    /// right-to-left run — `text` served logical order while the frame reverses
    /// the run in place — and no read bridged them. The display form lays each
    /// row out as painted and, in JSON, carries the visual→logical column map
    /// (`null` for a row whose order is the identity). Logical order stays the
    /// default: it is what a driver `send`s against.
    #[test]
    fn text_order_display_reads_a_right_to_left_run_as_painted() {
        assert!(args("order=display").display);
        assert!(!args("order=logical trim").display);
        for bad in ["order=visual", "order=display order=display", "order="] {
            assert_eq!(text_args(bad), Err(TEXT_USAGE.to_string()), "{bad:?}");
        }
        let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
        term.lock()
            .unwrap()
            .process("ab \u{05D0}\u{05D1}\r\nplain".as_bytes());
        let logical = cmd_text_opt(&term, args("rows=0-1"));
        assert!(logical.contains("ab \u{05D0}\u{05D1}\n"), "{logical:?}");
        let display = cmd_text_opt(&term, args("rows=0-1 order=display"));
        assert!(
            display.contains("ab \u{05D1}\u{05D0}\n"),
            "painted order: {display:?}"
        );
        assert!(
            display.contains("plain\n"),
            "an LTR row is unchanged: {display:?}"
        );
        let json = cmd_text_json_opt(&term, args("rows=0-1 order=display"));
        assert!(
            json.contains("\"rows\":[\"ab \u{05D1}\u{05D0}\",\"plain\"]"),
            "{json}"
        );
        assert!(
            json.contains(",\"order\":\"display\",\"display_map\":[[0,1,2,4,3,5,6,7,8,9],null]"),
            "{json}"
        );
        let plain = cmd_text_json_opt(&term, args("rows=0-1"));
        assert!(!plain.contains("display_map"), "{plain}");
    }

    /// The `text` tail parses `trim` and at most one shape in any order; anything else is the
    /// usage line, and a reversed span (`rows=5-2`) is `ERR bad rows`.
    #[test]
    fn text_args_accepts_trim_and_one_shape_in_any_order() {
        let all = TextArgs::default();
        assert_eq!(text_args(""), Ok(all));
        assert_eq!(text_args("  "), Ok(all));
        assert_eq!(text_args("trim"), Ok(TRIM));
        assert_eq!(text_args(" trim "), Ok(TRIM));
        let tail5 = TextArgs {
            trim: false,
            shape: TextShape::Tail(5),
            display: false,
        };
        assert_eq!(text_args("tail=5"), Ok(tail5));
        assert_eq!(
            text_args("tail=5 trim"),
            Ok(TextArgs {
                trim: true,
                ..tail5
            })
        );
        assert_eq!(
            text_args("trim tail=5"),
            Ok(TextArgs {
                trim: true,
                ..tail5
            }),
            "any order"
        );
        assert_eq!(
            text_args("rows=20-23"),
            Ok(TextArgs {
                trim: false,
                shape: TextShape::Rows(20, 23),
                display: false,
            })
        );
        assert_eq!(
            text_args("rows=7-7 trim"),
            Ok(TextArgs {
                trim: true,
                shape: TextShape::Rows(7, 7),
                display: false,
            }),
            "a one-row span is a span"
        );
        for bad in [
            "foo",
            "20",
            "trim extra",
            "TRIM",
            "trim=1",
            "--trim",
            "trim trim",
            "tail=0",
            "tail=",
            "tail=x",
            "tail=-1",
            "tail",
            "tail=5 tail=6",
            "tail=5 rows=1-2",
            "rows=5",
            "rows=a-b",
            "rows=5-",
            "rows=-5",
            "rows=1-2-3",
            "rows",
        ] {
            assert_eq!(
                text_args(bad),
                Err(TEXT_USAGE.to_string()),
                "{bad:?} is rejected, never silently ignored"
            );
        }
        assert_eq!(
            text_args("rows=5-2"),
            Err(ROWS_REVERSED.to_string()),
            "well-formed, but names no row on any grid"
        );
    }

    fn term_with(rows: u16, seed: &[u8]) -> Arc<Mutex<Terminal>> {
        let mut t = Terminal::new(rows, 40);
        t.process(seed);
        Arc::new(Mutex::new(t))
    }

    /// `text --json` carries the GENERATION of the rows it sends (`"gen":"E.S"`,
    /// read under the same lock hold), so a supervisor can fence its press on the
    /// read it judged (`key if-gen=`). Box A and box B drawn in two alternate-screen
    /// sessions with the same number of writes: NEGATIVE CONTROL, the `seq` the two
    /// reads carry is the same — a fence on it would press into B — while `gen`
    /// differs, and each is the value `status gen=` prints for that screen.
    #[test]
    fn text_json_carries_the_generation_of_the_rows_it_sends() {
        const ALT_A: &[u8] = b"\x1b[?1049h\x1b[H Bash command\r\n   ls -la /tmp/work\r\n";
        const ALT_B: &[u8] = b"\x1b[H Bash command\r\n   rm -rf /tmp/work\r\n";
        let term = term_with(6, ALT_A);
        let field = |json: &str, key: &str| {
            let at = json.find(&format!("\"{key}\":")).expect(key) + key.len() + 3;
            json[at..]
                .split([',', '}'])
                .next()
                .unwrap()
                .trim_matches('"')
                .to_string()
        };
        let a = cmd_text_json_opt(&term, TextArgs::default());
        let gen_a = super::screen_gen(&term.lock().unwrap()).to_string();
        assert_eq!(field(&a, "gen"), gen_a, "{a}");
        term.lock()
            .unwrap()
            .process(&[b"\x1b[?1049l\x1b[?1049h".as_slice(), ALT_B].concat());
        let b = cmd_text_json_opt(&term, TextArgs::default());
        assert!(b.contains("rm -rf"), "PRECONDITION: box B is up: {b}");
        assert_eq!(
            field(&a, "seq"),
            field(&b, "seq"),
            "NEGATIVE CONTROL: seq repeats"
        );
        assert_ne!(field(&a, "gen"), field(&b, "gen"), "the generation moved");
        assert_eq!(
            field(&b, "gen"),
            super::screen_gen(&term.lock().unwrap()).to_string()
        );
        // A shaped read carries it too, before `trimmed`/`first`, which stay last.
        let tail = cmd_text_json_opt(&term, args("tail=2 trim"));
        assert_eq!(field(&tail, "gen"), field(&b, "gen"), "{tail}");
    }

    /// `"human_ms"` (the person's stamp, R1 of 2026-09-25) rides every reply
    /// right after `gen`: the milliseconds since a person keyed the session,
    /// `null` when none ever has; then `"human_seq"` and `"launch"` (`null`
    /// with no launch witness); a shaped read keeps `trimmed`/`first` last.
    #[test]
    fn text_json_carries_the_persons_stamp_after_gen() {
        let term = term_with(24, &numbered(22));
        let never = super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            TextArgs::default(),
            None,
            None,
            0,
            None,
            None,
        );
        assert!(
            never.contains(",\"human_ms\":null,\"human_seq\":0,\"launch\":null}"),
            "{never}"
        );
        let keyed = super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            TextArgs::default(),
            Some(12_345),
            None,
            3,
            None,
            None,
        );
        let gen_at = keyed.find("\"gen\":").expect("gen");
        let human_at = keyed
            .find(",\"human_ms\":12345,\"human_seq\":3,\"launch\":null}")
            .expect("human_ms, human_seq and launch, closing");
        assert!(gen_at < human_at, "{keyed}");
        let shaped = super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            args("tail=5 trim"),
            Some(7),
            None,
            9,
            None,
            None,
        );
        assert!(
            shaped.ends_with(
                ",\"human_ms\":7,\"human_seq\":9,\"launch\":null,\"trimmed\":2,\"first\":19}\n"
            ),
            "{shaped}"
        );
        // Negative control: the test spelling is the never-keyed session.
        assert_eq!(cmd_text_json_opt(&term, TextArgs::default()), never);
        // A never that says since when (`"unkeyed_ms"`, last, the review of
        // LC-4): beside `null` alone, never beside an age.
        let served = super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            args("tail=5 trim"),
            None,
            Some(5_000),
            0,
            None,
            None,
        );
        assert!(
            served.ends_with(
                ",\"human_ms\":null,\"human_seq\":0,\"launch\":null,\"trimmed\":2,\"first\":19,\
                 \"unkeyed_ms\":5000}\n"
            ),
            "{served}"
        );
        let keyed = super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            TextArgs::default(),
            Some(7),
            Some(5_000),
            0,
            None,
            None,
        );
        assert!(!keyed.contains("unkeyed_ms"), "{keyed}");
    }

    /// `"launch"` (the launch witness, `crate::launch_witness`) is judged
    /// against the read's own `"human_seq"`: the whole witness while that
    /// count is the mark's, the word `since` once a gesture moved it; it
    /// stays before `trimmed`/`first`.
    #[test]
    fn text_json_judges_the_launch_witness_by_its_own_count() {
        use crate::launch_witness::{Launch, LaunchWitness, Why};
        let term = term_with(24, &numbered(22));
        let w = LaunchWitness::default();
        w.mark_for_test(Launch {
            id: 4,
            seq: 9,
            word: Some("claude"),
            why: Why::Person,
        });
        let at = super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            args("tail=5 trim"),
            Some(7),
            None,
            9,
            Some(&w),
            None,
        );
        assert!(
            at.ends_with(
                ",\"human_seq\":9,\"launch\":{\"id\":4,\"seq\":9,\"word\":\"claude\",\
                 \"why\":\"person\"},\"trimmed\":2,\"first\":19}\n"
            ),
            "{at}"
        );
        let moved = super::cmd_text_json_read(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            TextArgs::default(),
            Some(7),
            None,
            10,
            Some(&w),
            None,
        );
        assert!(
            moved.ends_with(",\"human_seq\":10,\"launch\":{\"why\":\"since\"}}\n"),
            "{moved}"
        );
    }

    /// `"submits"` (the session's submission count, `crate::submit_count`)
    /// ends every reply the session's verb serves, after `trimmed`, `first`
    /// and `unkeyed_ms`: written by the session's own input writes, a
    /// control client's and a person's alike, and only an Enter over
    /// something composed moves it. `"inputs"` before it counts every input
    /// write, submitting or not, before and after the rows. Active writes,
    /// a held frame or accepted-but-buffered spill make the first count
    /// `null`. Once delivered to the owned peer and the frame hold released,
    /// the same count is admitted. A write begun while the rows are read
    /// appears only after them. These are transport observations, not child
    /// acknowledgment. A read with no session writes neither.
    #[cfg(unix)]
    #[test]
    fn text_json_ends_with_the_sessions_submission_count() {
        use crate::input::{EgressMode, InputEvent};
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        use std::io::Read as _;
        let term = term_with(24, &numbered(22));
        let tracker = std::sync::Arc::new(crate::app_input::OutputEchoTracker::default());
        let (mut peer, master) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        peer.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .expect("hang detector");
        master.set_nonblocking(true).expect("nonblocking master");
        let sink = aterm_session::sink::SinkWriter::new_owned(master.into());
        sink.note_master_nonblocking(true);
        // Arm the real drainer explicitly below, after the held-fence
        // assertions. No worker races the buffered-input negative control.
        sink.install_spill_arranger(|| {});
        let read = |args: TextArgs| {
            super::cmd_text_json_read(
                &term,
                &crate::frame_memo::FrameSlot::default(),
                args,
                None,
                Some(5_000),
                0,
                None,
                Some((&*tracker, &sink)),
            )
        };
        let before = read(args("tail=5 trim"));
        assert!(
            before.ends_with(
                ",\"launch\":null,\"trimmed\":2,\"first\":19,\"unkeyed_ms\":5000,\"inputs\":[0,0],\
                 \"submits\":0}\n"
            ),
            "{before}"
        );
        let modes = crate::mode_mirror_of(&term);
        let enter = InputEvent::Key {
            key: Key::Named(NamedKey::Enter),
            mods: Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: KeyEventType::Press,
        };
        let write = |ev: &InputEvent| {
            crate::app_input::tracked_egress(
                &term,
                &modes,
                &sink,
                &tracker,
                ev,
                EgressMode::Interactive,
            )
        };
        // NEGATIVE CONTROL: an Enter over nothing composed.
        write(&enter);
        assert!(read(TextArgs::default()).ends_with(",\"submits\":0}\n"));
        write(&InputEvent::KeySequence(b"keep going".to_vec()));
        assert!(
            read(TextArgs::default()).ends_with(",\"submits\":0}\n"),
            "a draft is no submission"
        );
        write(&enter);
        let after = read(TextArgs::default());
        assert!(after.ends_with(",\"submits\":1}\n"), "{after}");
        // Every write counted, the plain Enter and the draft too.
        assert!(after.contains(",\"inputs\":[3,3],"), "{after}");
        // A write in flight: counted after the rows, not before them.
        let flight = tracker.begin_event(&enter);
        let held = read(TextArgs::default());
        assert!(held.contains(",\"inputs\":[null,4],"), "{held}");
        drop(flight);
        // NEGATIVE CONTROL: over, it is counted before the rows again.
        let over = read(TextArgs::default());
        assert!(over.contains(",\"inputs\":[4,4],"), "{over}");
        // A write begun and over WHILE THE ROWS ARE READ (the review of
        // `b9/resid4`, R2): counted after them, never before, so the count
        // before is read ahead of the rows and the count after behind them.
        // FAILS with the count before read after the rows (`[5,5]`), or the
        // count after read before them (`[4,4]`).
        let landing = std::sync::Arc::clone(&tracker);
        let key = enter.clone();
        super::WHILE_ROWS_READ.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || drop(landing.begin_event(&key))));
        });
        let during = read(TextArgs::default());
        assert!(
            super::WHILE_ROWS_READ.with(|hook| hook.borrow().is_none()),
            "the write was made inside the read"
        );
        assert!(during.contains(",\"inputs\":[4,5],"), "{during}");
        // An empty spill is not enough while a frame fence is held.
        let fence = sink.try_fence_egress().expect("egress initially settled");
        let held = read(TextArgs::default());
        assert!(held.contains(",\"inputs\":[null,5],"), "{held}");
        // Accepted, but NOT delivered: a non-submitting clear key queues
        // behind the held frame after the tracked write itself has ended.
        let receipt = write(&InputEvent::KeySequence(vec![0x1b]));
        assert!(receipt.fully_accepted_nonempty() && !receipt.is_direct());
        assert_eq!(sink.spilled_bytes(), 1, "the clear key is still buffered");
        let buffered = read(TextArgs::default());
        assert!(buffered.contains(",\"inputs\":[null,6],"), "{buffered}");
        assert!(buffered.ends_with(",\"submits\":1}\n"), "{buffered}");
        drop(fence);
        sink.arrange_pending_drainer();
        let expected = b"\rkeep going\r\x1b";
        let mut delivered = vec![0; expected.len()];
        peer.read_exact(&mut delivered)
            .expect("the owned peer receives the bytes");
        assert_eq!(delivered.as_slice(), expected);
        assert!(
            aterm_spec::wait::within(
                aterm_spec::wait::MUST_HAPPEN,
                std::time::Duration::from_millis(2),
                || tracker.input_settled(&sink).filter(|&n| n == 6),
            )
            .is_some(),
            "the drainer releases its final frame hold"
        );
        let drained = read(TextArgs::default());
        assert!(drained.contains(",\"inputs\":[6,6],"), "{drained}");
        // A read with no session's tracker (the test spelling) writes none.
        let bare = cmd_text_json_opt(&term, TextArgs::default());
        assert!(
            !bare.contains("submits") && !bare.contains("inputs"),
            "{bare}"
        );
    }

    /// The shared framer: `OK <n>[ <verdict>][ trimmed=<k>][ first=<row>]` + rows, so the count
    /// and the `turn` token keep their positions.
    #[test]
    fn frame_rows_reply_places_the_verdict_then_trimmed() {
        let body = "a\n\nb\n\n\n";
        assert_eq!(
            frame_rows_reply(body, 5, "", false, 0),
            "OK 5\na\n\nb\n\n\n"
        );
        assert_eq!(
            frame_rows_reply(body, 5, "", true, 0),
            "OK 3 trimmed=2\na\n\nb\n"
        );
        let v = "turn submitted=1 status=settled seq=9 id=2 dur_ms=3 hash=00000000000000ab";
        assert_eq!(
            frame_rows_reply(body, 5, v, false, 0),
            format!("OK 5 {v}\na\n\nb\n\n\n")
        );
        assert_eq!(
            frame_rows_reply(body, 5, v, true, 0),
            format!("OK 3 {v} trimmed=2\na\n\nb\n")
        );
        assert_eq!(
            frame_rows_reply("\n\n", 2, v, true, 0),
            format!("OK 0 {v} trimmed=2\n")
        );
        // `first=` is the last token, after the verdict and after `trimmed=`; row 0
        // is the bare wire.
        assert_eq!(
            frame_rows_reply(body, 5, "", false, 19),
            "OK 5 first=19\na\n\nb\n\n\n"
        );
        assert_eq!(
            frame_rows_reply(body, 5, "", true, 19),
            "OK 3 trimmed=2 first=19\na\n\nb\n"
        );
        assert_eq!(
            frame_rows_reply(body, 5, v, true, 3),
            format!("OK 3 {v} trimmed=2 first=3\na\n\nb\n")
        );
    }

    /// `r0`..`r<n-1>`, one per row, with no trailing newline so a grid of exactly
    /// `n` rows never scrolls.
    fn numbered(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| format!("r{i}"))
            .collect::<Vec<_>>()
            .join("\r\n")
            .into_bytes()
    }

    /// `tail=<n>` / `rows=<a>-<b>` send only the selected rows and say where they
    /// start; a selection that is the whole grid is the bare wire (no `first=`).
    #[test]
    fn text_tail_and_rows_send_a_slice_and_say_where_it_starts() {
        let term = term_with(24, &numbered(24));
        let whole = cmd_text_opt(&term, TextArgs::default());
        assert!(whole.starts_with("OK 24\nr0\nr1\n"), "{whole}");
        assert_eq!(
            cmd_text_opt(&term, args("tail=5")),
            "OK 5 first=19\nr19\nr20\nr21\nr22\nr23\n"
        );
        assert_eq!(
            cmd_text_opt(&term, args("rows=20-23")),
            "OK 4 first=20\nr20\nr21\nr22\nr23\n"
        );
        assert_eq!(
            cmd_text_opt(&term, args("rows=23-99")),
            "OK 1 first=23\nr23\n",
            "the span's end is clamped to the grid"
        );
        assert_eq!(
            cmd_text_opt(&term, args("rows=0-0")),
            "OK 1\nr0\n",
            "row 0 carries no first="
        );
        assert_eq!(
            cmd_text_opt(&term, args("rows=99-100")),
            super::rows_off_grid(24),
            "no row of the span is on the grid"
        );
        // A tail larger than the grid is the grid, byte-identical to the bare read.
        assert_eq!(cmd_text_opt(&term, args("tail=99")), whole);
        assert_eq!(cmd_text_opt(&term, args("tail=24")), whole);
        assert_eq!(cmd_text_opt(&term, args("rows=0-99")), whole);
        assert_eq!(
            cmd_text_opt(&term, args(&format!("rows=0-{}", usize::MAX))),
            whole,
            "a span ending at usize::MAX clamps, never overflows"
        );
        assert!(!whole.contains("first="));
    }

    /// `trim` composes with a shape and measures the slice: the selected rows' blank tail is
    /// dropped and counted by `trimmed=`, `first=` still says where the slice began, and
    /// `trimmed=` comes before `first=`.
    #[test]
    fn text_trim_composes_with_the_shape_and_trims_the_slice() {
        // 22 numbered rows on a 24-row grid: rows 22 and 23 are blank.
        let term = term_with(24, &numbered(22));
        assert_eq!(
            cmd_text_opt(&term, TRIM),
            {
                let mut want = String::from("OK 22 trimmed=2\n");
                for i in 0..22 {
                    want.push_str(&format!("r{i}\n"));
                }
                want
            },
            "whole-grid trim is unchanged"
        );
        let want = "OK 3 trimmed=2 first=19\nr19\nr20\nr21\n";
        assert_eq!(cmd_text_opt(&term, args("tail=5 trim")), want);
        assert_eq!(cmd_text_opt(&term, args("trim tail=5")), want, "any order");
        assert_eq!(
            cmd_text_opt(&term, args("rows=19-23 trim")),
            want,
            "the same slice by span"
        );
        assert_eq!(
            cmd_text_opt(&term, args("rows=22-23 trim")),
            "OK 0 trimmed=2 first=22\n",
            "an all-blank slice trims to nothing and still says where it was"
        );
        assert_eq!(
            cmd_text_opt(&term, args("rows=0-4 trim")),
            "OK 5 trimmed=0\nr0\nr1\nr2\nr3\nr4\n",
            "a slice with no blank tail trims nothing; row 0 carries no first="
        );
    }

    /// `text --json` with a shape: `rows` is the slice, `dims.rows` stays the grid,
    /// and `"first":<row>` closes the object (after `"trimmed"`) whenever the slice
    /// does not start at row 0.
    #[test]
    fn text_json_carries_first_for_a_shaped_read() {
        let term = term_with(24, &numbered(22));
        let json = cmd_text_json_opt(&term, args("tail=5"));
        assert!(
            json.starts_with("OK 1\n{\"rows\":[\"r19\",\"r20\",\"r21\",\"\",\"\"],"),
            "{json}"
        );
        assert!(
            json.contains("\"dims\":{\"rows\":24,\"cols\":40}"),
            "dims stay the grid: {json}"
        );
        assert!(json.ends_with(",\"first\":19}\n"), "{json}");
        assert!(!json.contains("trimmed"), "{json}");
        let trimmed = cmd_text_json_opt(&term, args("tail=5 trim"));
        assert!(
            trimmed.starts_with("OK 1\n{\"rows\":[\"r19\",\"r20\",\"r21\"],"),
            "{trimmed}"
        );
        assert!(
            trimmed.ends_with(",\"trimmed\":2,\"first\":19}\n"),
            "{trimmed}"
        );
        let span = cmd_text_json_opt(&term, args("rows=20-21"));
        assert!(
            span.starts_with("OK 1\n{\"rows\":[\"r20\",\"r21\"],"),
            "{span}"
        );
        assert!(span.ends_with(",\"first\":20}\n"), "{span}");
        assert_eq!(
            cmd_text_json_opt(&term, args("rows=99-100")),
            super::rows_off_grid(24)
        );
        let whole = cmd_text_json_opt(&term, args("tail=99"));
        assert_eq!(
            whole,
            cmd_text_json_opt(&term, TextArgs::default()),
            "a tail larger than the grid is the bare JSON"
        );
        assert!(!whole.contains("\"first\""), "{whole}");
    }

    /// `blocktext <id> [trim]`: the parsed id and whether `trim` was given reach the
    /// emitter, whose reply is the verb's untouched (an `ERR` included); anything
    /// else is the arm's usage line and emits nothing.
    #[test]
    fn blocktext_args_validate_grammar_and_pass_trim_to_the_emitter() {
        let emit = |id: u64, trim: bool| format!("OK {id} trim={trim}\n");
        assert_eq!(cmd_blocktext_args("7", emit), "OK 7 trim=false\n");
        assert_eq!(cmd_blocktext_args(" 7  trim ", emit), "OK 7 trim=true\n");
        for bad in ["", "trim", "x", "7 foo", "7 trim foo", "7trim", "-7"] {
            assert_eq!(
                cmd_blocktext_args(bad, |_, _| unreachable!("usage errors never emit")),
                "ERR usage: blocktext <id> [trim]\n",
                "{bad:?}"
            );
        }
        assert_eq!(
            cmd_blocktext_args("7 trim", |_, _| "ERR no such block\n".to_string()),
            "ERR no such block\n",
            "an emitter error is the reply"
        );
    }
}

/// `offscreen` against a real engine fed a synthetic alt-screen app that repaints in place
/// inside DEC 2026 frames (never a real capture: those carry private content).
#[cfg(test)]
mod offscreen_tests {
    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::Terminal;

    use super::{
        OFFSCREEN_BAD_SINCE, OFFSCREEN_DEFAULT_MAX, OFFSCREEN_USAGE, OffscreenArgs, OffscreenSince,
        cmd_text, offscreen_args, visible_row,
    };

    /// `offscreen` as a session with nothing published serves it.
    fn cmd_offscreen(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
        super::cmd_offscreen(term, &crate::frame_memo::FrameSlot::default(), rest)
    }

    const ROWS: u16 = 8;
    const TRANSCRIPT: usize = 6;
    const ORIGIN: u64 = 0xA11_0000_0042;

    fn row(i: usize) -> String {
        format!("row {i:05} of the transcript")
    }

    /// The screen with transcript row `start` at the top: six transcript rows, then
    /// a composer and a footer that never move (the chrome the differ excludes).
    fn frame(start: usize) -> Vec<String> {
        let mut v: Vec<String> = (start..start + TRANSCRIPT).map(row).collect();
        v.push("> type a message".to_string());
        v.push("footer: ready".to_string());
        v
    }

    /// Paint one frame the way Claude Code does: CUP + text + EL per row inside a
    /// DEC 2026 synchronized update, and no scroll sequence at all.
    fn paint(t: &mut Terminal, rows: &[String]) {
        let mut b = String::from("\x1b[?2026h");
        for (r, text) in rows.iter().enumerate() {
            let _ = write!(b, "\x1b[{};1H{text}\x1b[K", r + 1);
        }
        b.push_str("\x1b[?2026l");
        t.process(b.as_bytes());
    }

    /// An engine on the alternate screen whose app painted frames `0..frames`, one
    /// row further down the transcript each time: `frames - 1` rows scrolled off
    /// its top, archived as indices `1..frames`, where index `i` holds `row(i - 1)`.
    fn scrolled_engine(frames: usize) -> Terminal {
        let mut t = Terminal::new(ROWS, 40);
        t.set_alt_archive_enabled(true); // whatever the default says here
        t.set_alt_archive_origin(ORIGIN);
        t.process(b"\x1b[?1049h");
        for s in 0..frames {
            paint(&mut t, &frame(s));
        }
        t
    }

    fn scrolled(frames: usize) -> Arc<Mutex<Terminal>> {
        Arc::new(Mutex::new(scrolled_engine(frames)))
    }

    /// A reply split into its header count, its `key=value` fields and its lines;
    /// asserts the count is the number of lines that follow (the framing).
    fn parse(reply: &str) -> (usize, HashMap<String, String>, Vec<String>) {
        let (header, body) = reply.split_once('\n').expect("a header line");
        let mut toks = header.split_whitespace();
        assert_eq!(toks.next(), Some("OK"), "{reply}");
        let n: usize = toks.next().unwrap().parse().expect("a count");
        let fields = toks
            .map(|t| {
                let (k, v) = t.split_once('=').expect("key=value");
                (k.to_string(), v.to_string())
            })
            .collect();
        let lines: Vec<String> = body.lines().map(str::to_string).collect();
        assert_eq!(
            lines.len(),
            n,
            "OK <n> counts every line that follows: {reply}"
        );
        assert!(body.is_empty() || body.ends_with('\n'), "{reply:?}");
        (n, fields, lines)
    }

    fn field(fields: &HashMap<String, String>, key: &str) -> u64 {
        fields
            .get(key)
            .unwrap_or_else(|| panic!("no {key}= in {fields:?}"))
            .parse()
            .unwrap()
    }

    fn rows(from: usize, to: usize) -> Vec<String> {
        (from..to).map(row).collect()
    }

    #[test]
    fn offscreen_args_accept_each_option_once_in_any_order() {
        assert_eq!(offscreen_args(""), Ok(OffscreenArgs::default()));
        assert_eq!(offscreen_args("   "), Ok(OffscreenArgs::default()));
        assert_eq!(
            offscreen_args("screen=1 max=5 tail=2 since=9"),
            Ok(OffscreenArgs {
                since: Some(OffscreenSince {
                    origin: None,
                    index: 9
                }),
                tail: Some(2),
                max: Some(5),
                screen: true,
            })
        );
        assert_eq!(
            offscreen_args("since=77:0").map(|a| a.since),
            Ok(Some(OffscreenSince {
                origin: Some(77),
                index: 0
            }))
        );
        assert_eq!(
            offscreen_args("since=0").map(|a| a.since.unwrap().index),
            Ok(0)
        );
    }

    /// Nothing is silently ignored: an unknown or repeated token, `tail=0`, `max=0`,
    /// a non-number (a sign included), a malformed origin mark, and any `screen=`
    /// but 1 all answer the one usage line.
    #[test]
    fn offscreen_args_refuse_junk_repeats_zeros_and_non_numbers() {
        for bad in [
            "junk",
            "5",
            "--json",
            "since",
            "since=",
            "since=x",
            "since=-1",
            "since=+3",
            "since=1:",
            "since=:1",
            "since=1:2:3",
            "since=a:2",
            "since=1 since=2",
            "tail=0",
            "tail=",
            "tail=-2",
            "tail=+2",
            "tail=2 tail=3",
            "max=0",
            "max=x",
            "max=3 max=3",
            "screen",
            "screen=0",
            "screen=2",
            "screen=1 screen=1",
            "tail=2 junk",
            "since=99999999999999999999999",
        ] {
            assert_eq!(
                offscreen_args(bad),
                Err(OFFSCREEN_USAGE.to_string()),
                "{bad:?}"
            );
            assert_eq!(
                cmd_offscreen(&scrolled(3), bad),
                OFFSCREEN_USAGE,
                "the verb answers the parser's line for {bad:?}"
            );
        }
    }

    /// The bare read: every archived row, oldest first, `first=1`, `last` the newest
    /// index, and the state the header promises read under the same lock.
    #[test]
    fn offscreen_returns_the_rows_the_app_scrolled_off_oldest_first() {
        let term = scrolled(10);
        let reply = cmd_offscreen(&term, "");
        let (n, f, lines) = parse(&reply);
        assert_eq!(n, 9, "{reply}");
        assert_eq!(lines, rows(0, 9));
        assert_eq!(field(&f, "first"), 1);
        assert_eq!(field(&f, "last"), 9);
        assert_eq!(field(&f, "lost"), 0);
        assert_eq!(field(&f, "breaks"), 0);
        assert_eq!(field(&f, "back"), 0);
        assert_eq!(field(&f, "origin"), ORIGIN);
        assert_eq!(field(&f, "alt"), 1);
        let t = term.lock().unwrap();
        assert_eq!(field(&f, "seq"), t.content_seq());
        assert_eq!(field(&f, "epoch"), u64::from(t.alt_archive().epoch()));
        assert!(!f.contains_key("more") && !f.contains_key("screen_rows"));
        // The screen still shows the rows that did not scroll off; `lines` stays 0.
        assert_eq!(t.grid().scrollback_lines(), 0);
    }

    /// `since=` is exclusive; `since=<last>` is the empty "nothing new" poll; one
    /// past `last` is `ERR bad since`. The origin-qualified form reads the same as
    /// the bare one under this archive's origin, and from the start under any
    /// other (a mark from a previous process), even one past `last`.
    #[test]
    fn offscreen_since_is_exclusive_and_origin_checked() {
        let term = scrolled(10);
        let (_, f, lines) = parse(&cmd_offscreen(&term, "since=4"));
        assert_eq!(lines, rows(4, 9));
        assert_eq!(field(&f, "first"), 5);
        let (n, f, _) = parse(&cmd_offscreen(&term, "since=9"));
        assert_eq!((n, field(&f, "first"), field(&f, "last")), (0, 10, 9));
        assert_eq!(cmd_offscreen(&term, "since=10"), OFFSCREEN_BAD_SINCE);
        assert_eq!(
            cmd_offscreen(&term, &format!("since={ORIGIN}:4")),
            cmd_offscreen(&term, "since=4")
        );
        assert_eq!(
            cmd_offscreen(&term, &format!("since={ORIGIN}:10")),
            OFFSCREEN_BAD_SINCE
        );
        for other in ["7:4", "7:10", "7:999999"] {
            let (_, f, lines) = parse(&cmd_offscreen(&term, &format!("since={other}")));
            assert_eq!(lines, rows(0, 9), "{other} reads from the start");
            assert_eq!(
                field(&f, "origin"),
                ORIGIN,
                "and says whose archive it read"
            );
        }
        // An archive that has archived nothing: since=0 is fine, since=1 is not.
        let empty = scrolled(1);
        let (n, f, _) = parse(&cmd_offscreen(&empty, "since=0"));
        assert_eq!((n, field(&f, "last"), field(&f, "first")), (0, 0, 1));
        assert_eq!(cmd_offscreen(&empty, "since=1"), OFFSCREEN_BAD_SINCE);
    }

    /// `max=` pages oldest-first with `more=1` until the last page (the paging
    /// loop: `paging_with_since_last_as_documented_reads_every_row`). `tail=`
    /// takes the newest rows instead, and combines with `max=`.
    #[test]
    fn offscreen_pages_with_max_and_tails_with_tail() {
        let term = scrolled(10);
        let (_, f, lines) = parse(&cmd_offscreen(&term, "tail=3"));
        assert_eq!(lines, rows(6, 9));
        assert_eq!(field(&f, "first"), 7);
        assert_eq!(f.get("more").map(String::as_str), Some("1"));
        let (_, f, lines) = parse(&cmd_offscreen(&term, "tail=3 since=7"));
        assert_eq!(lines, rows(7, 9), "tail never reaches back past since");
        assert!(!f.contains_key("more"));
        let (_, _, lines) = parse(&cmd_offscreen(&term, "max=2 tail=3"));
        assert_eq!(lines, rows(7, 9), "the newest min(tail, max)");
        let (_, f, lines) = parse(&cmd_offscreen(&term, "tail=50"));
        assert_eq!(lines, rows(0, 9));
        assert!(!f.contains_key("more"));
    }

    /// With no `max=`, one reply carries at most [`OFFSCREEN_DEFAULT_MAX`] rows and
    /// says `more=1`; `since=<last>` of that page reads the rest.
    #[test]
    fn offscreen_caps_a_reply_at_the_default_page() {
        let extra = 5;
        let term = scrolled(OFFSCREEN_DEFAULT_MAX + extra + 1);
        let (n, f, lines) = parse(&cmd_offscreen(&term, ""));
        assert_eq!(n, OFFSCREEN_DEFAULT_MAX);
        assert_eq!(f.get("more").map(String::as_str), Some("1"));
        assert_eq!(lines.first(), Some(&row(0)));
        let page_last = field(&f, "first") + n as u64 - 1;
        let (n, f, lines) = parse(&cmd_offscreen(&term, &format!("since={page_last}")));
        assert_eq!(n, extra);
        assert!(!f.contains_key("more"));
        assert_eq!(
            lines,
            rows(OFFSCREEN_DEFAULT_MAX, OFFSCREEN_DEFAULT_MAX + extra)
        );
    }

    /// Rows evicted at the byte budget are counted as `lost=` for a reader whose
    /// `since` predates them, and not for one that is past them.
    #[test]
    fn offscreen_counts_evicted_rows_as_lost() {
        let mut t = scrolled_engine(1);
        // Each row costs its bytes plus a fixed overhead: ten rows fit.
        t.set_alt_archive_budget(10 * aterm_core::terminal::alt_archive_row_charge(row(0).len()));
        for s in 1..31 {
            paint(&mut t, &frame(s));
        }
        let term = Arc::new(Mutex::new(t));
        let (n, f, lines) = parse(&cmd_offscreen(&term, "since=0"));
        let first = field(&f, "first");
        assert_eq!(field(&f, "last"), 30);
        assert!(first > 1 && n < 30, "some rows were evicted: {f:?}");
        assert_eq!(
            field(&f, "lost"),
            first - 1,
            "every row before first is lost"
        );
        assert_eq!(lines.first(), Some(&row(first as usize - 1)));
        let (_, f, _) = parse(&cmd_offscreen(&term, &format!("since={}", first - 1)));
        assert_eq!(field(&f, "lost"), 0, "nothing after since was evicted");
        let (_, f, _) = parse(&cmd_offscreen(&term, "since=3"));
        assert_eq!(field(&f, "lost"), first - 4);
    }

    /// `breaks=` counts the discontinuities at or after `since`; leaving the alt
    /// screen is one, and `alt=` follows the screen. `back=` is how many archived
    /// rows the screen shows again at its top after the app scrolled back.
    #[test]
    fn offscreen_reports_breaks_alt_and_back() {
        let mut t = scrolled_engine(10);
        paint(&mut t, &frame(8)); // the app scrolled BACK one row: row 8 is on screen again
        let term = Arc::new(Mutex::new(t));
        let (_, f, _) = parse(&cmd_offscreen(&term, ""));
        assert_eq!(field(&f, "back"), 1, "{f:?}");
        assert_eq!(field(&f, "last"), 9, "scrolling back archives nothing");
        term.lock().unwrap().process(b"\x1b[?1049l");
        let (_, f, _) = parse(&cmd_offscreen(&term, ""));
        assert_eq!(field(&f, "alt"), 0);
        assert_eq!(field(&f, "breaks"), 1, "leaving the alt screen is a break");
        let last = field(&f, "last");
        let (_, f, _) = parse(&cmd_offscreen(&term, &format!("since={last}")));
        assert_eq!(
            field(&f, "breaks"),
            1,
            "a break after last: the screen does not continue"
        );
    }

    /// `screen=1` appends the current screen, the same rows `text` shows, and the
    /// header's count covers them: the archived rows are the first `n - m` lines,
    /// the screen the last `screen_rows=<m>`.
    #[test]
    fn offscreen_screen_appends_the_live_screen_under_the_same_count() {
        let term = scrolled(10);
        let (n, f, lines) = parse(&cmd_offscreen(&term, "since=6 screen=1"));
        let m = field(&f, "screen_rows") as usize;
        assert_eq!(m, usize::from(ROWS));
        assert_eq!(n, 3 + m);
        assert_eq!(lines[..3], rows(6, 9)[..]);
        let text = cmd_text(&term);
        let screen: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(
            lines[3..].iter().map(String::as_str).collect::<Vec<_>>(),
            screen
        );
        assert_eq!(lines[3], row(9), "the screen continues the archive");
        // With nothing new archived, screen=1 is the screen alone.
        let (n, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
        assert_eq!((n, field(&f, "screen_rows")), (m, m as u64));
    }

    /// `screen=1` read while the program's DEC 2026 frame is unfinished says `frame=open`
    /// (read under the rows' lock, as `text --json`'s `"frame":"open"`): the screen half is a
    /// prefix of a redraw. NEGATIVE CONTROLS: the frame closed, and a read with no screen half,
    /// say nothing of it; past the hang detector the screen is read as it stands.
    #[test]
    fn offscreen_screen_says_when_the_frame_is_open() {
        let term = scrolled(10);
        term.lock()
            .unwrap()
            .process(b"\x1b[?2026h\x1b[2;1Hrow half-dra");
        let (_, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
        assert_eq!(f.get("frame").map(String::as_str), Some("open"), "{f:?}");
        let (_, f, _) = parse(&cmd_offscreen(&term, "since=9"));
        assert!(!f.contains_key("frame"), "no screen half: {f:?}");
        term.lock().unwrap().process(b"wn\x1b[K\x1b[?2026l");
        let (_, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
        assert!(!f.contains_key("frame"), "closed: {f:?}");
        let begun = std::time::Instant::now()
            .checked_sub(aterm_core::terminal::FRAME_HANG + std::time::Duration::from_secs(1))
            .expect("an instant a minute back");
        term.lock().unwrap().process_at(
            b"\x1b[?2026h\x1b[3;1Hhung",
            aterm_core::terminal::ClockReading {
                monotonic: begun,
                wall_ms: None,
            },
        );
        assert!(term.lock().unwrap().sync_frame_unfinished());
        let (_, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
        assert!(!f.contains_key("frame"), "past its hang detector: {f:?}");
    }

    // `offscreen`'s wire contract as a client uses it: paging with `since=<last>`,
    // an archive that is off, the re-shown rows under a pinned header, and the
    // row text itself .

    /// The documented paging loop. `OFFSCREEN_DEFAULT_MAX`'s doc says "`more=1`
    /// marks a page with rows left over, and `since=<last>` reads the next"; the
    /// catalog says "poll with since=<last>"; spec C says "page with
    /// since=<last> until more=0". A client that does exactly that gets every
    /// archived row once (`last=` was the archive's newest, so page 2 was empty).
    #[test]
    fn paging_with_since_last_as_documented_reads_every_row() {
        let term = scrolled(10);
        let mut all = Vec::new();
        let mut since = 0u64;
        let mut pages = 0;
        loop {
            let reply = cmd_offscreen(&term, &format!("since={since} max=4"));
            let (_, f, lines) = parse(&reply);
            all.extend(lines);
            since = field(&f, "last");
            pages += 1;
            if !f.contains_key("more") || pages > 10 {
                break;
            }
        }
        let want: Vec<String> = (0..9).map(row).collect();
        assert_eq!(
            all, want,
            "paging with since=<last> must neither skip nor repeat"
        );
    }

    /// An archive that is off reads `enabled=0`, not as an empty one; an archive that is on
    /// never carries the field.
    #[test]
    fn a_disabled_archive_is_not_reported_as_an_empty_one() {
        let mut t = scrolled_engine(0);
        t.set_alt_archive_enabled(false);
        for s in 0..10 {
            paint(&mut t, &frame(s));
        }
        let term = Arc::new(Mutex::new(t));
        let (n, f, _) = parse(&cmd_offscreen(&term, ""));
        assert_eq!(n, 0);
        assert_eq!(f.get("enabled").map(String::as_str), Some("0"), "{f:?}");
        term.lock().unwrap().set_alt_archive_enabled(true);
        let (_, f, _) = parse(&cmd_offscreen(&term, ""));
        assert!(!f.contains_key("enabled"), "{f:?}");
    }

    /// `back=` under a pinned header: the re-shown rows sit at screen rows `[pin, pin+back)`
    /// and are archived rows `back_at..`, so a joiner skips exactly the rows it already has.
    #[test]
    fn back_under_a_pinned_header_names_rows_the_archive_holds() {
        fn framed(start: usize) -> Vec<String> {
            let mut v = vec!["HEADER pinned title bar".to_string()];
            v.extend((start..start + 5).map(row));
            v.push("> type a message".to_string());
            v.push("footer: ready".to_string());
            v
        }
        let mut t = scrolled_engine(0);
        for s in 0..10 {
            paint(&mut t, &framed(s));
        }
        paint(&mut t, &framed(8)); // the app scrolled back one row
        let term = Arc::new(Mutex::new(t));
        let reply = cmd_offscreen(&term, "screen=1");
        let (n, f, lines) = parse(&reply);
        let m = field(&f, "screen_rows") as usize;
        let back = field(&f, "back") as usize;
        let (pin, back_at) = (field(&f, "pin") as usize, field(&f, "back_at"));
        let first = field(&f, "first");
        let (archived, screen) = lines.split_at(n - m);
        assert_eq!((back, pin), (1, 1), "{f:?}");
        for k in 0..back {
            let at = usize::try_from(back_at - first).unwrap() + k;
            assert_eq!(
                screen[pin + k],
                archived[at],
                "screen row pin+{k} is archived row back_at+{k}: {f:?}"
            );
        }
        assert_eq!(
            screen[0], "HEADER pinned title bar",
            "the header is not re-shown"
        );
        // No re-shown rows, no pin=/back_at=.
        term.lock()
            .unwrap()
            .process(b"\x1b[?2026h\x1b[2;1Hnew row\x1b[K\x1b[?2026l");
        let (_, f, _) = parse(&cmd_offscreen(&term, ""));
        if field(&f, "back") == 0 {
            assert!(
                !f.contains_key("pin") && !f.contains_key("back_at"),
                "{f:?}"
            );
        }
    }

    /// Rows are exactly what `text` showed: wide CJK, a combining mark, an emoji,
    /// a NUL-filled gap (CUF over unwritten cells).
    #[test]
    fn archived_rows_are_byte_identical_to_the_text_rows_they_were() {
        fn special(i: usize) -> String {
            format!("r{i:03} \u{65e5}\u{672c} cafe\u{301} \u{1F44D}\x1b[3Cgap")
        }
        let mut t = scrolled_engine(0);
        let mut shown = Vec::new();
        for s in 0..8 {
            let mut v: Vec<String> = (s..s + 6).map(special).collect();
            v.push("> type a message".to_string());
            v.push("footer: ready".to_string());
            paint(&mut t, &v);
            shown.push(visible_row(&t, 0));
        }
        let term = Arc::new(Mutex::new(t));
        let (_, _, lines) = parse(&cmd_offscreen(&term, ""));
        assert_eq!(lines, shown[..7].to_vec());
    }
}

#[cfg(test)]
mod story_tests {
    //! The `story` verb's grammar: every junk form is the usage line, the
    //! closed set is `chose` alone (the question-policy contract v3,
    //! 2026-09-24; the seven unsent words went with the presence row,
    //! 2026-10-16), and the text cap is the wire cap.

    use super::{parse_story, story_usage};
    use crate::presence::{StoryVerb, TOLD_TEXT_MAX_BYTES};

    #[test]
    fn every_junk_form_is_the_usage_line_and_the_closed_set_parses() {
        let usage = story_usage();
        for junk in [
            "",
            "   ",
            "approve",
            "chosen",
            "CHOSE",
            "choose",
            "42",
            "@s-1 chose",
            "story chose",
            // The seven words no process sent: refused now.
            "approved",
            "dismissed",
            "reconnected",
            "timeout",
            "exit",
            "compacted",
            "warned",
        ] {
            assert_eq!(parse_story(junk), Err(usage.clone()), "{junk:?}");
        }
        assert_eq!(parse_story("chose"), Ok((StoryVerb::Chose, String::new())));
        assert_eq!(
            parse_story("  chose   some text here  "),
            Ok((StoryVerb::Chose, "some text here".to_string())),
            "the text is trimmed and kept whole"
        );
        assert_eq!(StoryVerb::TOLD_WORDS.len(), 1, "the closed set is `chose`");
        for word in StoryVerb::TOLD_WORDS {
            assert!(StoryVerb::parse_told(word).is_some(), "{word}");
        }
    }

    #[test]
    fn the_text_is_capped_at_the_wire_cap_and_refuses_control_bytes() {
        let ok = "x".repeat(TOLD_TEXT_MAX_BYTES);
        assert_eq!(
            parse_story(&format!("chose {ok}")),
            Ok((StoryVerb::Chose, ok.clone()))
        );
        let long = "x".repeat(TOLD_TEXT_MAX_BYTES + 1);
        assert_eq!(
            parse_story(&format!("chose {long}")),
            Err(format!(
                "ERR story: text is longer than {TOLD_TEXT_MAX_BYTES} bytes\n"
            ))
        );
        // Bytes, not chars: a multi-byte text is measured as the wire sees it.
        let wide = "é".repeat(TOLD_TEXT_MAX_BYTES / 2 + 1);
        assert!(parse_story(&format!("chose {wide}")).is_err());
        assert_eq!(
            parse_story("chose session gone\nOK forged"),
            Err("ERR story: text carries a control character\n".to_string())
        );
        assert_eq!(
            parse_story("chose tab\there"),
            Err("ERR story: text carries a control character\n".to_string())
        );
    }
}

#[cfg(test)]
mod devtools_verb_tests {
    //! `devtools hand-over [now]`: the grammar is checked on the control thread, and every
    //! outcome of the window's one hand-over has one wire answer; the help names each one the
    //! verb can give.

    use super::{DEVTOOLS_USAGE, devtools_reply, parse_devtools};
    use crate::devtools_helper::{HandOver, Refusal};

    #[test]
    fn the_grammar_is_hand_over_with_an_optional_now() {
        assert_eq!(parse_devtools("hand-over"), Ok(false));
        assert_eq!(parse_devtools("  hand-over  "), Ok(false));
        assert_eq!(parse_devtools("hand-over now"), Ok(true));
        assert_eq!(parse_devtools("hand-over   now\t"), Ok(true));
        for junk in [
            "",
            "   ",
            "handover",
            "hand-over later",
            "hand-over now now",
            "now",
            "now hand-over",
            "HAND-OVER",
            "hand-over NOW",
            "open",
        ] {
            assert_eq!(parse_devtools(junk), Err(DEVTOOLS_USAGE), "{junk:?}");
        }
    }

    /// Each outcome is one line: `OK <word>` for what the window decided, `ERR` only where
    /// nothing could be decided (headless) or the page failed. Every answer the verb can give
    /// as it asks is named in its help, so an asker (`aterm doctor`) can key on the help's
    /// words.
    #[test]
    fn every_outcome_has_one_answer_and_the_help_names_the_verbs_own() {
        let asked = [
            (HandOver::Queued, "OK queued\n"),
            (HandOver::Shown, "OK shown\n"),
            (HandOver::Unshown, "OK unshown\n"),
            (HandOver::Refused(Refusal::Granted), "OK granted\n"),
            (HandOver::Refused(Refusal::Unknown), "OK unknown\n"),
            (HandOver::Refused(Refusal::Declined), "OK declined\n"),
            (HandOver::Refused(Refusal::Unstable), "OK unstable\n"),
            (HandOver::Refused(Refusal::Recent), "OK recent\n"),
            (HandOver::Refused(Refusal::Listed), "OK listed\n"),
        ];
        let help = aterm_types::control_verbs::spec("devtools")
            .expect("devtools is in the table")
            .help_line();
        for (outcome, want) in asked {
            assert_eq!(devtools_reply(outcome), want);
            let words = format!("`{}`", want.trim_end());
            assert!(help.contains(&words), "the help names {words}\n{help}");
        }
        let headless = devtools_reply(HandOver::Headless);
        assert!(headless.starts_with("ERR headless: "), "{headless}");
        assert!(help.contains("`ERR headless: \u{2026}`"), "{help}");
        // An owner press's outcomes: the mapping is total though the verb never opens the
        // page as it asks.
        assert_eq!(devtools_reply(HandOver::Opened), "OK opened\n");
        assert_eq!(devtools_reply(HandOver::Listing), "OK listing\n");
        assert!(devtools_reply(HandOver::OpenFailed).starts_with("ERR "));
    }
}

/// The native search over the engine's search-row reader
/// ([`Terminal::read_search_rows`]): the `search` verb and the find bar read every
/// row through it, so they key rows by absolute row across a corrupt tiered
/// segment, read a screen row's soft wrap from the live frame however far the
/// viewport is scrolled back, and name a history row read only up to its first
/// MiB.
#[cfg(test)]
mod engine_reader_tests {
    use std::sync::{Arc, Mutex};

    use aterm_core::grid::Row;
    use aterm_core::terminal::Terminal;

    use super::{
        EngineSearchDirection, FullHistorySearch, cmd_line, cmd_search,
        search_full_history_direction,
    };
    use crate::session_waits::RewrapGauge;
    use crate::term_lock;

    /// A terminal whose tiered history holds a corrupt warm block between its cold tier and
    /// its healthy warm blocks, a needle on each side, and soft-wrapped lines in the lazy/ring
    /// history and across the history/screen edge: what a skipping reader, or one taking the
    /// wrap flag from another row than the text, gets wrong.
    fn corrupt_tiered_terminal() -> (Terminal, usize) {
        let mut sb = aterm_scrollback::Scrollback::with_block_size(4, 12, 10_000_000, 4);
        for i in 0..40 {
            match i {
                2 => sb.push_str("NEEDLE_before the corrupt block"),
                38 => sb.push_str("NEEDLE_after the corrupt block"),
                _ => sb.push_str(&format!("carried {i:03}")),
            }
        }
        assert!(sb.cold_line_count() > 2, "the first needle sits in cold");
        let corrupt = 5;
        sb.inject_corrupted_warm_block(corrupt);
        let mut t = Terminal::with_scrollback(6, 40, 5, sb);
        for i in 0..20 {
            if i % 4 == 1 {
                // 95 columns into a 40-column grid: one head row, two
                // continuation rows.
                t.process(format!("wrapped {i:03} {}\r\n", "w".repeat(83)).as_bytes());
            } else {
                t.process(format!("live {i:03}\r\n").as_bytes());
            }
        }
        // End on a run whose head has scrolled into history and whose seven
        // rows' tail fills the screen: it straddles the history/screen edge.
        t.process(format!("edge {}", "e".repeat(250)).as_bytes());
        assert_eq!(
            t.grid()
                .history_lines_from(0)
                .filter(Option::is_none)
                .count(),
            corrupt,
            "the fixture's corrupt block is still in the store"
        );
        (t, corrupt)
    }

    /// PRODUCT LEVEL: the `search` verb (the find bar's engine) over a corrupt
    /// tiered segment keys each needle at its own absolute row — the corrupt
    /// block's placeholder rows sit between them.
    #[test]
    fn native_search_over_a_corrupt_tiered_segment_keeps_absolute_rows() {
        let (t, corrupt) = corrupt_tiered_terminal();
        let oldest = t.grid().oldest_absolute_row();
        let term = Arc::new(Mutex::new(t));
        let reply = cmd_search(&term, "NEEDLE_", &RewrapGauge::default());
        let rows: Vec<u64> = reply
            .lines()
            .skip(1)
            .map(|hit| hit.split(' ').next().unwrap().parse().unwrap())
            .collect();
        assert!(reply.starts_with("OK 2"), "both needles are found: {reply}");
        assert_eq!(rows, [oldest + 2, oldest + 38 + corrupt as u64], "{reply}");
    }

    /// PRODUCT LEVEL: `line` answers a corrupt tiered segment's placeholder
    /// row `ERR unreadable`, never the `ERR out of range` of the live screen's
    /// bottom, so a per-row client does not take a corrupt retained row for the
    /// end of the screen and stop a transcript there; a readable row in the same
    /// history still answers its text.
    #[test]
    fn line_over_a_corrupt_tiered_segment_says_unreadable_not_out_of_range() {
        let (t, _) = corrupt_tiered_terminal();
        let oldest = t.grid().oldest_absolute_row();
        let unreadable = oldest
            + t.grid()
                .history_lines_from(0)
                .position(|line| line.is_none())
                .expect("a corrupt row") as u64;
        let bottom = oldest + t.grid().scrollback_lines() as u64 + u64::from(t.rows());
        let term = Arc::new(Mutex::new(t));
        assert_eq!(cmd_line(&term, &unreadable.to_string()), "ERR unreadable\n");
        assert_eq!(cmd_line(&term, &bottom.to_string()), "ERR out of range\n");
        assert!(
            cmd_line(&term, &(oldest + 2).to_string()).contains("NEEDLE_before"),
            "a readable row in the same history remains available"
        );
    }

    /// Find reads each screen row's soft wrap from the live frame, so a viewport
    /// scrolled back, inside the ring or into the tiered history past it, answers as
    /// the live tail does: the hit straddling a wrap on the screen is found once, at
    /// the row it starts on, and an anchor on its continuation row is carried into
    /// the joined line.
    #[test]
    fn find_joins_a_soft_wrap_the_same_while_scrolled_back() {
        // A fresh terminal per offset: a viewport scroll moves no cache key.
        let scrolled = |offset: i32| {
            let mut t =
                Terminal::with_scrollback(6, 20, 8, aterm_scrollback::Scrollback::with_defaults());
            for i in 0..30 {
                t.process(format!("filler {i:02}\r\n").as_bytes());
            }
            // 25 columns into 20: `NEEDL` ends the head row, `E` starts the next.
            t.process(b"abcdefghijklmnoNEEDLEtail\r\nplain row\r\nNEEDLE again");
            t.scroll_display(offset);
            assert_eq!(
                t.grid().display_offset(),
                usize::try_from(offset).expect("non-negative"),
                "precondition: scrolled"
            );
            let rows = t.rows();
            let live: Vec<bool> = (0..rows)
                .map(|row| t.grid().row_at_screen(row).is_some_and(Row::is_wrapped))
                .collect();
            assert!(live.iter().any(|&w| w), "the screen holds the soft wrap");
            if offset > 0 {
                // NEGATIVE CONTROL: the viewport accessor maps through
                // `display_offset` (or reads nothing past the ring), so a reader
                // built on it reads other flags on this screen.
                let viewport: Vec<bool> = (0..rows)
                    .map(|row| t.grid().row(row).is_some_and(Row::is_wrapped))
                    .collect();
                assert_ne!(viewport, live, "offset {offset}");
            }
            let screen = t.grid().oldest_absolute_row() + t.grid().scrollback_lines() as u64;
            let term = Arc::new(Mutex::new(t));
            // Anchored on the continuation row, two cells in.
            let anchor = (usize::try_from(screen).expect("row") + 3, 2);
            let search: FullHistorySearch = search_full_history_direction(
                &term,
                "NEEDLE",
                false,
                false,
                EngineSearchDirection::Forward,
                Some(anchor),
                false,
            )
            .expect("searched");
            let hits: Vec<(usize, usize, usize)> = search
                .results
                .matches
                .iter()
                .map(|m| (m.line, m.start_col, m.len()))
                .collect();
            let point = search.point_match.map(|m| (m.line, m.start_col, m.len()));
            (screen as usize, hits, point)
        };
        let (screen, live_hits, live_point) = scrolled(0);
        assert_eq!(
            live_hits,
            [(screen + 2, 15, 6), (screen + 5, 0, 6)],
            "the straddling hit at the row it starts on, and the one after"
        );
        assert_eq!(
            live_point,
            Some((screen + 5, 0, 6)),
            "the next hit after the anchor"
        );
        // Inside the eight-row ring, and into the tiered history past it.
        for offset in [1, 2, 20] {
            assert_eq!(
                scrolled(offset),
                (screen, live_hits.clone(), live_point),
                "scrolled back {offset}"
            );
        }
    }

    /// A history line longer than the MiB a row is read to for search (a crafted
    /// checkpoint can store one) is searched up to the bound and named in the header,
    /// ` incomplete long-line`; `line` still reads it whole.
    #[test]
    fn search_names_a_history_line_cut_at_the_read_bound() {
        const BOUND: usize = 1024 * 1024;
        let terminal = |len: usize| {
            let mut line = b"NEEDLE ".to_vec();
            line.resize(len, b'A');
            let mut scrollback = aterm_scrollback::Scrollback::with_defaults();
            scrollback.push_line(aterm_scrollback::Line::from_bytes(&line));
            Arc::new(Mutex::new(Terminal::with_scrollback(
                4, 80, 1000, scrollback,
            )))
        };
        let header = |term: &Arc<Mutex<Terminal>>| {
            let reply = cmd_search(term, "needle", &RewrapGauge::default());
            reply.lines().next().expect("a header").to_string()
        };
        // NEGATIVE CONTROL: a line at the bound is read whole.
        assert_eq!(header(&terminal(BOUND)), "OK 1 screen_first=1");
        let cut = terminal(BOUND + 1);
        assert_eq!(header(&cut), "OK 1 incomplete long-line screen_first=1");
        let oldest = term_lock(&cut).grid().oldest_absolute_row();
        let reply = cmd_line(&cut, &oldest.to_string());
        assert_eq!(reply.len(), "OK ".len() + BOUND + 1 + "\n".len());
        assert!(reply.starts_with("OK NEEDLE A"), "the row as written");
    }
}

/// DIFFERENTIAL ORACLE for the one-walk link gather (the
/// `images_frame_into_matches_per_row` idiom): the frame this crate SHIPS must be
/// byte-identical to one built from the per-cell sources it replaced, over a
/// corpus that arms every extraction difference between them.
///
/// What the reference deliberately keeps from the retired shape:
///   * `Terminal::render_row_at_screen` — the ALLOCATING per-row accessor (the
///     gather reuses one buffer through `render_row_at_screen_into`);
///   * `Terminal::cell_grapheme(r, c)` — the per-cell glyph, which resolves the
///     row and applies the DECSCA-aware continuation rule for every cell (the
///     gather reads each cell once off the held row view, carrying the left
///     neighbour's WIDE bit);
///   * `Grid::cell_extra(r, c)` — the per-cell HASH probe, gated only on the map
///     being globally empty (the gather walks the map ONCE);
///   * link interning BY VALUE, not by `Arc` pointer: if the pointer intern ever
///     merged two links whose BYTES differ, the reference would not, and this
///     comparison would fail.
///
/// Everything the change did not touch (seq, cursor, selection, images, line
/// sizes) comes from the shipping gather, so a diff can only come from the cells
/// and the link table.
#[cfg(test)]
mod styled_frame_fusion_parity {
    use super::{
        NO_LINK, StyledCellSnap, StyledFrameSnapshot, StyledLink, StyledRowSnap,
        gather_styled_frame, serialize_styled_frame,
    };
    use aterm_core::terminal::Terminal;

    /// The per-cell reference extraction, in the snapshot's containers.
    fn reference_snapshot(t: &Terminal) -> StyledFrameSnapshot {
        let rows = t.rows() as usize;
        let cols = t.cols() as usize;
        let blank = t.implicit_blank_render_cell();
        let any_extras = !t.grid().extras().is_empty();
        let mut cells: Vec<StyledRowSnap> = Vec::new();
        let mut links: Vec<StyledLink> = Vec::new();
        for r in 0..rows {
            let rendered = t.render_row_at_screen(r);
            let view = t.grid().screen_row_view(r as u16);
            let mut glyphs = String::new();
            let mut row_cells: Vec<StyledCellSnap> = Vec::new();
            for c in 0..cols {
                let start = glyphs.len();
                glyphs.push_str(&t.cell_grapheme(r, c).unwrap_or_default());
                let extra = if any_extras {
                    t.grid().cell_extra(r as u16, c as u16)
                } else {
                    None
                };
                let url = extra.and_then(|e| e.hyperlink());
                let id = extra.and_then(|e| e.hyperlink_id());
                let link = match url {
                    None => NO_LINK,
                    Some(u) => {
                        let pos = links
                            .iter()
                            .position(|l| *l.url == **u && l.id.as_deref() == id.map(|i| &**i));
                        let pos = pos.unwrap_or_else(|| {
                            links.push(StyledLink {
                                url: std::sync::Arc::clone(u),
                                id: id.map(std::sync::Arc::clone),
                            });
                            links.len() - 1
                        });
                        u32::try_from(pos).expect("small link table")
                    }
                };
                row_cells.push(StyledCellSnap {
                    cell: rendered.get(c).copied().unwrap_or(blank),
                    glyph: start..glyphs.len(),
                    wide_lead: view.cell(c as u16).is_some_and(|cell| cell.is_wide()),
                    blink: view.cell(c as u16).is_some_and(|cell| {
                        cell.flags().contains(aterm_core::grid::CellFlags::BLINK)
                    }),
                    link,
                });
            }
            cells.push(StyledRowSnap {
                glyphs,
                cells: row_cells,
            });
        }
        let mut snap = gather_styled_frame(t);
        snap.cells = cells;
        snap.links = links;
        snap
    }

    /// One terminal per corpus case, each arming a different extraction
    /// difference between the one-walk gather and the per-cell sources.
    fn corpus() -> Vec<(&'static str, Terminal)> {
        let mut out: Vec<(&'static str, Terminal)> = Vec::new();
        out.push(("blank", Terminal::new(4, 12)));
        let mut t = Terminal::new(4, 12);
        t.process(b"hello");
        out.push(("plain_ascii", t));
        // A WIDE lead plus its continuation.
        let mut t = Terminal::new(3, 10);
        t.process("\u{4f60}\u{597d}ab".as_bytes());
        out.push(("wide_cjk", t));
        // An NFD combining mark: an extra that carries no link.
        let mut t = Terminal::new(2, 8);
        t.process("e\u{0301}x".as_bytes());
        out.push(("combining_nfd", t));
        // Extras but no link anywhere (an SGR 58 coloured underline).
        let mut t = Terminal::new(3, 10);
        t.process(b"\x1b[4:3m\x1b[58:2::255:0:0mUU\x1b[m plain");
        out.push(("extras_but_no_link", t));
        // One OSC 8 link with an id, surrounded by unlinked text.
        let mut t = Terminal::new(3, 20);
        t.process(b"pre \x1b]8;id=k1;https://example.com\x1b\\LINK\x1b]8;;\x1b\\ post");
        out.push(("one_link_with_id", t));
        // Two adjacent runs, same url, different id: distinct regions a url-only
        // intern would merge.
        let mut t = Terminal::new(2, 20);
        t.process(b"\x1b]8;id=a;https://x.example\x1b\\AA\x1b]8;;\x1b\\");
        t.process(b"\x1b]8;id=b;https://x.example\x1b\\BB\x1b]8;;\x1b\\");
        out.push(("same_url_two_ids", t));
        // A link with no id (the id half stays null).
        let mut t = Terminal::new(2, 16);
        t.process(b"\x1b]8;;https://noid.example\x1b\\N\x1b]8;;\x1b\\");
        out.push(("link_without_id", t));
        // A distinct link on every column: the intern table and the row sort.
        let mut t = Terminal::new(4, 24);
        for i in 0..24u32 {
            t.process(
                format!("\x1b]8;id=i{i};https://d{i}.example\x1b\\x\x1b]8;;\x1b\\").as_bytes(),
            );
        }
        out.push(("hyperlink_dense", t));
        // Linked lines pushed into history: the extras map's row offset and
        // `iter()`'s stale filter against `get`'s coordinate mapping.
        let mut t = Terminal::new(4, 20);
        for i in 0..40u32 {
            t.process(
                format!("\x1b]8;id=s{i};https://s{i}.example\x1b\\row{i}\x1b]8;;\x1b\\\r\n")
                    .as_bytes(),
            );
        }
        out.push(("scrolled_links", t));
        // DECSCNM reverse video: the implicit-blank padding colours swap.
        let mut t = Terminal::new(3, 10);
        t.process(b"\x1b[?5hrev");
        out.push(("reverse_video", t));
        // An inline image sharing the grid with a link.
        let mut t = Terminal::new(3, 12);
        t.process(b"\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\");
        t.process(b"\x1b]8;;https://img.example\x1b\\L\x1b]8;;\x1b\\");
        out.push(("image_and_link", t));
        // A DEC double-width line with a link on it.
        let mut t = Terminal::new(3, 12);
        t.process(b"\x1b[2;1H\x1b#6\x1b]8;;https://dw.example\x1b\\W\x1b]8;;\x1b\\");
        out.push(("double_width_line", t));
        // The narrowest grid the engine builds.
        out.push(("single_column", Terminal::new(3, 1)));
        // A non-BMP emoji: a complex cell whose text is the ring's one codepoint.
        let mut t = Terminal::new(2, 10);
        t.process("a\u{1F600}b".as_bytes());
        out.push(("ring_emoji", t));
        // A ZWJ family as the extras MAP stores a cluster (a row restored from
        // history, or `set_cell_complex_char`): one complex cell, several scalars.
        let mut t = Terminal::new(2, 10);
        t.process(b"ab");
        t.grid_mut()
            .set_cell_complex_char(0, 0, "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}");
        out.push(("map_cluster", t));
        // DECSCA-protected text around a wide glyph: PROTECTED shares bit 10 with
        // WIDE_CONTINUATION, so only the cell right of a WIDE lead is a
        // continuation and the protected narrow cells are text.
        let mut t = Terminal::new(2, 10);
        t.process("\x1b[1\"qP\u{4f60}Q\x1b[0\"qz".as_bytes());
        out.push(("decsca_protected", t));
        out
    }

    #[test]
    fn one_walk_link_gather_matches_the_per_cell_reference_over_a_corpus() {
        for (name, t) in &corpus() {
            assert_eq!(
                serialize_styled_frame(&gather_styled_frame(t)),
                serialize_styled_frame(&reference_snapshot(t)),
                "styled frame diverged on corpus case {name}"
            );
        }
    }
}

/// THE CONDITIONAL `screen` READ, proved two-sided.
///
/// The danger of a conditional read is a FALSE "unchanged": answering it over a
/// screen that moved hands an agent a stale picture. The implication "frame
/// moved ⇒ etag moved" over a mutation corpus (the Kitty store's re-transmit and
/// delete among its steps) is `control_screen_etag_tests`'; here are the
/// targeted arms: the screen generation, the Kitty animation clock, another
/// terminal at a reused address, the verb end to end (an untouched terminal
/// answers `unchanged`, a stale token answers the frame) and its grammar.
#[cfg(test)]
mod screen_conditional {
    use super::{SCREEN_USAGE, screen_args, screen_etag};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    /// `screen` as a session with nothing published serves it.
    fn cmd_screen_styled_json(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
        super::cmd_screen_styled_json(term, &crate::frame_memo::FrameSlot::default(), rest)
    }

    /// What a full `screen` read serves: the frame, with its etag stripped so
    /// the comparison is of the frame alone.
    fn frame(term: &Arc<Mutex<Terminal>>) -> String {
        let out = cmd_screen_styled_json(term, "");
        let at = out
            .rfind(",\"etag\":")
            .expect("a full frame carries its etag");
        out[..at].to_string()
    }

    fn etag(term: &Arc<Mutex<Terminal>>) -> u64 {
        screen_etag(&term.lock().expect("mutex"))
    }

    /// The screen GENERATION arm: a TUI that leaves and re-enters the
    /// alternate screen and draws a second dialog with the same number of
    /// writes shows the first dialog's content seq over a different screen
    /// (`ScreenGen`'s own incident: `ls -la /tmp/work` vs `rm -rf /tmp/work`).
    #[test]
    fn an_alt_screen_re_entry_at_the_same_seq_is_not_unchanged() {
        let term = Arc::new(Mutex::new(Terminal::new(6, 30)));
        let box_a = b"\x1b[?1049h\x1b[H ls -la /tmp/work\r\n [y/n]\r\n";
        let box_b = b"\x1b[?1049l\x1b[?1049h\x1b[H rm -rf /tmp/work\r\n [y/n]\r\n";
        term.lock().expect("mutex").process(box_a);
        let (seq_a, frame_a, etag_a) = {
            let seq = term.lock().expect("mutex").content_seq();
            (seq, frame(&term), etag(&term))
        };
        term.lock().expect("mutex").process(box_b);
        let seq_b = term.lock().expect("mutex").content_seq();
        assert_eq!(seq_a, seq_b, "the premise: both boxes read the same seq");
        assert_ne!(frame(&term), frame_a, "the premise: the frames differ");
        assert_ne!(
            etag(&term),
            etag_a,
            "a re-entered alt screen is a new generation"
        );
        let reply = cmd_screen_styled_json(&term, &format!("since={etag_a:016x}"));
        assert!(
            reply.contains("rm -rf") || reply.contains("\"glyph\""),
            "{reply}"
        );
        assert!(
            !reply.contains("\"unchanged\""),
            "box B served as box A: {reply}"
        );
    }

    /// A Kitty animation steps its frame on the HOST's timer
    /// ([`Terminal::advance_kitty_animations`], `kitty_animation.rs`) with no
    /// output fed: the content seq, the output batches and the viewport all stay
    /// put while the frame's `images` change.
    #[test]
    fn a_kitty_animation_frame_step_is_not_unchanged() {
        let (cw, ch) = (10usize, 20usize);
        let t0 = aterm_time::Instant::now();
        let at = |ms: u64| aterm_core::terminal::ClockReading {
            monotonic: t0 + std::time::Duration::from_millis(ms),
            wall_ms: None,
        };
        let fill = |rgba: [u8; 4]| {
            let raw: Vec<u8> = rgba.iter().copied().cycle().take(cw * ch * 4).collect();
            aterm_codec::base64::encode(&raw).expect("encode")
        };
        let (red, blue) = (fill([230, 20, 20, 255]), fill([20, 20, 230, 255]));
        let mut t = Terminal::new(4, 8);
        t.set_cell_pixel_size(cw as u16, ch as u16);
        // Frame 1 red, frame 2 blue, 30 ms gaps, looping; placed at the home
        // cell, then started (`s=3`, run).
        for command in [
            format!("\x1b_Ga=t,f=32,s={cw},v={ch},i=7,q=2;{red}\x1b\\"),
            format!("\x1b_Ga=f,f=32,s={cw},v={ch},i=7,z=30,q=2;{blue}\x1b\\"),
            "\x1b_Ga=a,i=7,r=1,z=30\x1b\\".to_string(),
            "\x1b[1;1H\x1b_Ga=p,i=7,C=1,q=2\x1b\\".to_string(),
            "\x1b_Ga=a,i=7,s=3\x1b\\".to_string(),
        ] {
            t.process_at(command.as_bytes(), at(0));
        }
        let term = Arc::new(Mutex::new(t));
        let (frame_0, etag_0) = (frame(&term), etag(&term));
        assert!(frame_0.contains("\"b64\":"), "the premise: an image shows");
        let step = term
            .lock()
            .expect("mutex")
            .advance_kitty_animations(at(30).monotonic);
        assert!(step.advanced, "the premise: the animation stepped");
        let (frame_1, etag_1) = (frame(&term), etag(&term));
        assert_ne!(frame_1, frame_0, "the premise: the served frame moved");
        assert_ne!(etag_1, etag_0, "an animation frame step is not unchanged");
        let reply = cmd_screen_styled_json(&term, &format!("since={etag_0:016x}"));
        assert!(
            !reply.contains("\"unchanged\""),
            "the verb serves the stepped frame: {reply}"
        );
    }

    /// The etag names its terminal, not its address: another terminal behind the same `Arc`
    /// whose counters agree never answers `unchanged`.
    #[test]
    fn another_terminal_at_the_same_address_is_not_unchanged() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 12)));
        term.lock().expect("mutex").process(b"hello");
        let (frame_0, etag_0) = (frame(&term), etag(&term));
        let (seq_0, gen_0) = {
            let t = term.lock().expect("mutex");
            (t.content_seq(), super::screen_gen(&t).to_string())
        };
        let mut other = Terminal::new(4, 12);
        other.process(b"world");
        *term.lock().expect("mutex") = other;
        {
            let t = term.lock().expect("mutex");
            assert_eq!(
                (t.content_seq(), super::screen_gen(&t).to_string()),
                (seq_0, gen_0),
                "the premise: the counters agree"
            );
        }
        assert_ne!(frame(&term), frame_0, "the premise: other text shows");
        let reply = cmd_screen_styled_json(&term, &format!("since={etag_0:016x}"));
        assert!(
            !reply.contains("\"unchanged\""),
            "another terminal's screen is never the first one's: {reply}"
        );
    }

    /// The gate end to end, through the verb.
    #[test]
    fn conditional_screen_answers_unchanged_only_when_unchanged() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 12)));
        term.lock().expect("mutex").process(b"first");
        let full = cmd_screen_styled_json(&term, "");
        assert!(
            full.starts_with("OK 1\n") && full.contains("\"glyph\""),
            "{full}"
        );
        let tag = etag(&term);
        assert!(
            full.trim_end()
                .ends_with(&format!(",\"etag\":\"{tag:016x}\"}}")),
            "the full frame ends with the etag the next poll sends back: {full}"
        );
        for arg in [
            format!("since={tag:016x}"),
            format!("--json since={tag:016x}"),
        ] {
            let cond = cmd_screen_styled_json(&term, &arg);
            assert!(cond.starts_with("OK 1\n"), "{arg}: same framing: {cond}");
            assert!(cond.contains("\"unchanged\":true"), "{arg}: {cond}");
            assert!(
                cond.contains("\"gen\":\""),
                "{arg}: names the generation: {cond}"
            );
            assert!(
                !cond.contains("\"glyph\""),
                "{arg}: no grid on an unchanged reply"
            );
            assert!(
                cond.len() * 4 < full.len(),
                "{arg}: far smaller than the frame"
            );
        }
        // A stale token: the screen moved, so the full frame comes back.
        term.lock().expect("mutex").process(b" second");
        let after = cmd_screen_styled_json(&term, &format!("since={tag:016x}"));
        assert!(
            after.contains("\"glyph\"") && !after.contains("\"unchanged\""),
            "{after}"
        );
    }

    /// The grammar: `--json` and one well-formed `since=`, in any order; anything else is the
    /// usage line, never a silent full frame.
    #[test]
    fn screen_takes_since_and_json_and_refuses_the_rest() {
        assert_eq!(screen_args(""), Ok(None));
        assert_eq!(screen_args("--json"), Ok(None));
        assert_eq!(screen_args("since=00000000000000ff"), Ok(Some(255)));
        assert_eq!(screen_args("--json since=00000000000000FF"), Ok(Some(255)));
        assert_eq!(screen_args("since=00000000000000ff --json"), Ok(Some(255)));
        for bad in [
            "since=",
            "since=ff",
            "since=zzzzzzzzzzzzzzzz",
            "since=00000000000000ff since=00000000000000ff",
            "since=000000000000000000ff",
            "trim",
            "since",
        ] {
            assert_eq!(screen_args(bad), Err(SCREEN_USAGE.to_string()), "{bad:?}");
        }
        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        assert_eq!(cmd_screen_styled_json(&term, "trim"), SCREEN_USAGE);
    }

    /// `"frame":"open"` ends a `text --json` reply only while the program's DEC 2026 frame is
    /// unfinished: written into and not closed. NEGATIVE CONTROLS: no bracket, a bracket with
    /// nothing written in it, and the bracket closed.
    #[test]
    fn text_json_says_open_only_inside_an_unfinished_frame() {
        let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
        term.lock().unwrap().process(b"busy\r\n");
        let read = || {
            super::cmd_text_json_read(
                &term,
                &crate::frame_memo::FrameSlot::default(),
                super::TextArgs::default(),
                None,
                None,
                0,
                None,
                None,
            )
        };
        assert!(!read().contains("\"frame\""), "no bracket");
        term.lock().unwrap().process(b"\x1b[?2026h");
        assert!(!read().contains("\"frame\""), "nothing written in it");
        term.lock().unwrap().process(b"\x1b[2;1Hhalf");
        assert!(read().ends_with(",\"frame\":\"open\"}\n"), "{}", read());
        term.lock().unwrap().process(b"\x1b[?2026l");
        assert!(!read().contains("\"frame\""), "closed");
        // Past its hang detector a frame is read as it stands: the run of reads ending inside
        // unfinished frames began a minute and a second ago.
        let begun = std::time::Instant::now()
            .checked_sub(aterm_core::terminal::FRAME_HANG + std::time::Duration::from_secs(1))
            .expect("an instant a minute back");
        term.lock().unwrap().process_at(
            b"\x1b[?2026h\x1b[3;1Hhung",
            aterm_core::terminal::ClockReading {
                monotonic: begun,
                wall_ms: None,
            },
        );
        assert!(term.lock().unwrap().sync_frame_unfinished());
        assert!(!read().contains("\"frame\""), "past its hang detector");
    }
}
