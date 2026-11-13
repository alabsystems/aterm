// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl` handlers: the screen reads (`text`, `screen`, `cursor`, `cell`, `line`, `lines`, `offscreen`, `search`), `dims`, `metrics`, `modes`, `custody`, `title`, `cwd`, `colors`, `appstatus` and `messages`, and the writes `mainscreen`, `appnotice`, `notice`, `story`, `devtools` and `metrics reset`.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use aterm_control::wire::{FRAME_OPEN_WORD, VisibleTrim, frame_open_tail, push_visible};
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
use crate::frame_memo::{FrameSlot, ScreenText, Styled, styled_body_within};
use crate::image_payload_memo::{ImagePayload, image_payloads};
use crate::metrics::wire::{
    FieldSink, FlagWords, Form, JsonSink, ReadCtx, Request, Syntax, TextSink,
};
use crate::park::Lane;
use crate::session_waits::{RewrapGauge, RewrapReading};
use crate::{ReadOn, Wake, term_lock, term_lock_whole, term_lock_whole_when};

/// The visible, trailing-trimmed text of screen row `r`.
pub(crate) fn visible_row(t: &Terminal, r: usize) -> String {
    aterm_control::wire::visible_row(t, r)
}

pub(crate) use aterm_control::wire::trimmed_len;

#[path = "control_reveal.rs"]
mod reveal;
pub(crate) use reveal::{SpawnMark, cmd_reveal, mark_ctl_spawned, marks_of};

/// The `text` grammar, as the wire states it on a usage error.
pub(crate) const TEXT_USAGE: &str =
    "ERR usage: text [--json] [trim] [tail=<n>|rows=<a>-<b>] [order=<logical|display>]\n";

/// The `screen` grammar, as the wire states it on a usage error.
pub(crate) const SCREEN_USAGE: &str = "ERR usage: screen [--json] [since=<etag>]\n";

/// The answer to a `rows=<a>-<b>` span with `b < a`.
pub(crate) const ROWS_REVERSED: &str = "ERR bad rows: the span ends before it starts\n";

/// The answer to a row span that meets no row of a `grid_rows`-row grid, naming the rows that exist.
pub(crate) fn rows_off_grid(grid_rows: usize) -> String {
    format!(
        "ERR bad rows: the span meets no visible row (grid rows 0..={})\n",
        grid_rows.saturating_sub(1)
    )
}

/// Which rows of the visible grid a `text` read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum TextShape {
    #[default]
    All,
    Tail(usize),
    Rows(usize, usize),
}

impl TextShape {
    /// The half-open row range this shape selects on a `grid_rows`-row grid.
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

/// A parsed `text` / `text --json` argument tail ([`text_args`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TextArgs {
    pub(crate) trim: bool,
    pub(crate) shape: TextShape,
    pub(crate) display: bool,
}

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

/// The `text` / `text --json` argument tail.
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
            match (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| n.parse::<usize>().ok())
                .flatten()
            {
                Some(n) if n > 0 => TextShape::Tail(n),
                _ => return usage(),
            }
        } else if let Some(span) = tok.strip_prefix("rows=") {
            let Some((a, b)) = span.split_once('-') else {
                return usage();
            };
            if a.is_empty()
                || b.is_empty()
                || !a.bytes().all(|b| b.is_ascii_digit())
                || !b.bytes().all(|b| b.is_ascii_digit())
            {
                return usage();
            }
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

/// Split a trailing `trim` token off a positional argument tail.
pub(crate) fn split_trim_tail(rest: &str) -> (&str, bool) {
    let rest = rest.trim();
    match rest.strip_suffix("trim") {
        Some(head) if head.is_empty() || head.ends_with(char::is_whitespace) => {
            (head.trim_end(), true)
        }
        _ => (rest, false),
    }
}

/// `blocktext <id> [trim]`.
pub(crate) fn cmd_blocktext_args(rest: &str, emit: impl FnOnce(u64, bool) -> String) -> String {
    let (id, trim) = split_trim_tail(rest);
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return "ERR usage: blocktext <id> [trim]\n".to_string();
    }
    let Ok(id) = id.parse::<u64>() else {
        return "ERR usage: blocktext <id> [trim]\n".to_string();
    };
    emit(id, trim)
}

/// Frame a gathered screen `body` (`rows` newline-terminated rows, the terminal lock already released) as `OK <n>[ <verdict>]\n` + all rows.
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

/// `text` -> `OK <nrows>\n` then each visible row (trailing spaces trimmed).
#[cfg(test)]
pub(crate) fn cmd_text(term: &Arc<Mutex<Terminal>>) -> String {
    cmd_text_opt(term, &FrameSlot::default(), TextArgs::default())
}

/// THE SCREEN'S STAMP as a test reads it.
#[cfg(test)]
pub(crate) fn screen_stamp(t: &Terminal) -> (u64, u64) {
    (t.content_seq(), screen_hash(t))
}

/// FNV-1a-64 of [`screen_text`]'s bytes, folded row by row so no full-screen String is built.
#[cfg(test)]
pub(crate) fn screen_hash(t: &Terminal) -> u64 {
    let mut h = aterm_hash::Fnv1a64::new();
    for r in 0..t.rows() as usize {
        h.write(visible_row(t, r).as_bytes());
        h.write(b"\n");
    }
    h.finish()
}

pub(crate) use aterm_types::control_verbs::ScreenGen;

/// [`ScreenGen`] of `t` now (the caller holds its lock).
pub(crate) const fn screen_gen(t: &Terminal) -> ScreenGen {
    ScreenGen {
        epoch: t.content_scroll_state().invalidation_epoch,
        seq: t.content_seq(),
    }
}

/// FNV-1a-64 of the visible rows `first..=last`, each as [`visible_row`] serves it followed by `\n`.
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

/// The whole visible screen as text.
pub(crate) fn screen_text(t: &Terminal) -> String {
    let mut screen = String::with_capacity(t.rows() as usize * (t.cols() as usize + 1));
    for r in 0..t.rows() as usize {
        aterm_control::wire::visible_row_into(t, r, &mut screen);
        screen.push('\n');
    }
    screen
}

/// `text [trim] [tail=<n>|rows=<a>-<b>] [order=<logical|display>]` -> `OK <n>[ frame=open][ trimmed=<k>][ first=<row>]\n` then `<n>` visible rows.
/// Read in a whole frame ([`term_lock_whole`]); one still unfinished at the read's bound is read as it stands, `frame=open` after the count.
pub(crate) fn cmd_text_opt(
    term: &Arc<Mutex<Terminal>>,
    frame: &FrameSlot,
    args: TextArgs,
) -> String {
    let mut look = frame.look_frame();
    let (first, end, text, display, open) = {
        let t = term_lock_whole(term);
        let (first, end) = match args.shape.select(t.rows() as usize) {
            Ok(span) => span,
            Err(err) => return err,
        };
        let open = crate::frame_unfinished_now(&t);
        if args.display {
            let mut body = String::with_capacity((end - first) * (t.cols() as usize + 1));
            for r in first..end {
                body.push_str(&row_in_order(&t, r, true).0);
                body.push('\n');
            }
            (first, end, None, body, open)
        } else {
            (first, end, Some(look.text(&t)), String::new(), open)
        }
    };
    let body = text
        .as_deref()
        .map_or(display.as_str(), |text| text.span(first, end));
    let word = if open { FRAME_OPEN_WORD } else { "" };
    frame_rows_reply(body, end - first, word, args.trim, first)
}

/// `cursor` -> `OK <row> <col> <visible:0|1> <style>[ frame=open]\n` (0-based), read in a whole frame ([`term_lock_whole`]).
pub(crate) fn cmd_cursor(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock_whole(term);
    let c = t.cursor();
    let vis = u8::from(t.cursor_visible());
    let style = cursor_style_name(t.cursor_style());
    let open = frame_open_tail(crate::frame_unfinished_now(&t));
    format!("OK {} {} {} {}{open}\n", c.row, c.col, vis, style)
}

/// `cell <r> <c>` -> `OK <grapheme> <fg> <bg> <attrs>[ link=<url>][ frame=open]\n` or `ERR <msg>\n`.
/// A screen row is read in a whole frame ([`term_lock_whole_when`]), a history row at once with no word; a trailing ` frame=open` is no style evidence to [`aterm_codec::cell::parse_style`].
pub(crate) fn cmd_cell(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let mut it = rest.split_whitespace();
    let (Some(rs), Some(cs)) = (it.next(), it.next()) else {
        return "ERR usage: cell <r> <c> | cell abs=<n> <c>\n".to_string();
    };
    let abs = match rs.strip_prefix("abs=") {
        Some(n) => {
            match (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .then(|| n.parse::<u64>().ok())
                .flatten()
            {
                Some(n) => Some(n),
                None => return "ERR bad args\n".to_string(),
            }
        }
        None => None,
    };
    if cs.is_empty() || !cs.bytes().all(|b| b.is_ascii_digit()) {
        return "ERR bad args\n".to_string();
    }
    let Ok(c) = cs.parse::<usize>() else {
        return "ERR bad args\n".to_string();
    };
    let live = |t: &Terminal| abs.is_none_or(|n| n >= screen_first_abs(t));
    let t = term_lock_whole_when(term, Lane::SERVED, live);
    let out_of_range = || {
        let cols = t.cols().saturating_sub(1);
        if abs.is_some() {
            format!("ERR out of range: cols 0-{cols}\n")
        } else {
            format!(
                "ERR out of range: rows 0-{}, cols 0-{cols}\n",
                t.rows().saturating_sub(1)
            )
        }
    };
    let at = match abs {
        Some(n) => match t.abs_row_at(n) {
            AbsRowAt::Screen { row } => CellRow::Screen(row),
            AbsRowAt::History { rev_idx } => CellRow::History(rev_idx),
            AbsRowAt::Evicted => return "ERR evicted\n".to_string(),
            AbsRowAt::OutOfRange => return "ERR out of range\n".to_string(),
        },
        None => {
            match (!rs.is_empty() && rs.bytes().all(|b| b.is_ascii_digit()))
                .then(|| rs.parse::<usize>().ok())
                .flatten()
            {
                Some(r) => CellRow::Screen(r),
                None => return "ERR bad args\n".to_string(),
            }
        }
    };
    if c >= t.cols() as usize || matches!(at, CellRow::Screen(r) if r >= t.rows() as usize) {
        return out_of_range();
    }
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
        None => {
            let blank = t.implicit_blank_render_cell();
            (blank.fg, blank.bg)
        }
    };
    let col = c as u16;
    let cell = view.cell(col);
    let wide_cont = view.is_wide_continuation(col);
    let mut grapheme = String::new();
    if let Some(cell) = cell
        && !wide_cont
    {
        view.push_cell_text(col, cell, &mut grapheme);
    }
    let grapheme_tok = pct_encode(&grapheme);
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
    let link = match at {
        CellRow::Screen(r) => t
            .hyperlink_at(r as u16, c as u16)
            .map(|u| format!(" link={}", pct_encode(u)))
            .unwrap_or_default(),
        CellRow::History(_) => String::new(),
    };
    let open = match at {
        CellRow::Screen(_) => frame_open_tail(crate::frame_unfinished_now(&t)),
        CellRow::History(_) => "",
    };
    format!(
        "OK {grapheme_tok} {:02x}{:02x}{:02x} {:02x}{:02x}{:02x} {attrs}{link}{open}\n",
        fg[0], fg[1], fg[2], bg[0], bg[1], bg[2],
    )
}

#[derive(Clone, Copy)]
enum CellRow {
    Screen(usize),
    History(usize),
}

const fn cell_attrs(cell: Cell) -> CellFlags {
    cell.flags()
}

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

/// Serialize a live `dims` snapshot.
pub(crate) fn serialize_dims(snapshot: &DimsSnapshot) -> String {
    let window = snapshot
        .window
        .map_or_else(|| "none".to_string(), |window| window.to_string());
    let present_retry_in_ms = snapshot
        .present_retry_in_ms
        .map_or_else(|| "none".to_string(), |delay| delay.to_string());
    let (layer_gravity, layer_scale, layer_flipped) =
        snapshot.layer_presentation.as_ref().map_or_else(
            || ("none".to_string(), "none".to_string(), "none".to_string()),
            |(g, s, f)| (g.clone(), format!("{s:.2}"), f.to_string()),
        );
    let presented_seq = snapshot
        .presented_seq
        .map_or_else(|| "none".to_string(), |seq| seq.to_string());
    let session = snapshot
        .session
        .map_or_else(|| "none".to_string(), |session| session.to_string());
    format!(
        "OK {} {} {} {} session={} cell_w={} cell_h={} font_px={:.2} scale={:.2} window={} \
         window_rows={} window_cols={} composed_rows={} grid_w={} grid_h={} \
         frame_w={} frame_h={} surface_w={} surface_h={} offset_x={} offset_y={} \
         band_left={} band_right={} band_top={} band_bottom={} crop_left={} \
         crop_right={} crop_top={} crop_bottom={} pad={} pad_top={} pad_bottom={} head={} \
         tab_rows={} deck_y={} deck_h={} viewers={} visible_viewers={} geometry={} \
         present_retry_state={} present_retry_count={} present_retry_remaining={} \
         present_retry_in_ms={} \
         layer_gravity={} layer_scale={} layer_flipped={} presented_seq={}\n",
        snapshot.rows,
        snapshot.cols,
        snapshot.pixel_w,
        snapshot.pixel_h,
        session,
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
        presented_seq,
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

/// `dims` (plain, or `--json` when `json`) with no terminal in front: the front window's own
/// grid and geometry ([`crate::App::window_dims_snapshot`], `session=none`).
pub(crate) fn cmd_window_dims(proxy: &EventLoopProxy<Wake>, json: bool) -> String {
    match control_media::call_main(proxy, |reply| Wake::ReadWindowDims { reply }) {
        Ok(Some(snapshot)) if json => serialize_dims_json(&snapshot),
        Ok(Some(snapshot)) => serialize_dims(&snapshot),
        Ok(None) => "ERR no window\n".to_string(),
        Err(error) => format!("ERR {error}\n"),
    }
}

const METRICS_USAGE: &str = "ERR usage: metrics [reset|percentiles] [--json]\n";

/// `metrics [reset|percentiles] [--json]` -> one `OK k=v ...\n` line of live render/latency counters.
pub(crate) fn cmd_metrics(term: Option<&Arc<Mutex<Terminal>>>, rest: &str) -> String {
    metrics_reply(term, rest, Syntax::Text)
}

/// [`cmd_metrics`]'s JSON form, with `--json` already taken off the request by the dispatch.
pub(crate) fn cmd_metrics_json(term: Option<&Arc<Mutex<Terminal>>>, command: &str) -> String {
    metrics_reply(term, command, Syntax::Json)
}

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

/// `lines` -> `OK <n>[ truncated=<k>]\n` (`<k>`: lines the scrollback limit dropped).
pub(crate) fn cmd_lines(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    lines_reply(t.grid().scrollback_lines(), t.scrollback_truncated_lines())
}

/// `mainscreen`.
pub(crate) fn cmd_mainscreen(host: &impl aterm_control::SessionHost, sid: u64) -> String {
    let Some(left) = host.with_terminal_mut(sid, Terminal::leave_alternate_screen) else {
        return "ERR no such session\n".to_string();
    };
    if left {
        host.request_redraw(sid);
    }
    format!("OK left={}\n", u8::from(left))
}

fn lines_reply(lines: usize, truncated: u64) -> String {
    if truncated > 0 {
        format!("OK {lines} truncated={truncated}\n")
    } else {
        format!("OK {lines}\n")
    }
}

/// The `offscreen` grammar, as the wire states it on a usage error.
pub(crate) const OFFSCREEN_USAGE: &str =
    "ERR usage: offscreen [since=<i>] [tail=<n>] [max=<n>] [screen=1]\n";

/// `offscreen since=<i>` naming a row newer than the archive's newest.
pub(crate) const OFFSCREEN_BAD_SINCE: &str = "ERR bad since\n";

/// How many archived rows one `offscreen` reply carries when `max=` does not say (an archive holds up to 65,536).
pub(crate) const OFFSCREEN_DEFAULT_MAX: usize = 2000;

/// Where an `offscreen` read starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OffscreenSince {
    pub(crate) origin: Option<u64>,
    pub(crate) index: u64,
}

/// A parsed `offscreen` argument tail ([`offscreen_args`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct OffscreenArgs {
    pub(crate) since: Option<OffscreenSince>,
    pub(crate) tail: Option<usize>,
    pub(crate) max: Option<usize>,
    pub(crate) screen: bool,
}

fn offscreen_number(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// The `offscreen` argument tail.
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

/// `offscreen [since=<i>|since=<origin>:<i>] [tail=<n>] [max=<n>] [screen=1]` -> the rows an alternate-screen app scrolled off its top.
/// `screen=1`'s rows are read in a whole frame ([`term_lock_whole`]), with `frame=open` before `screen_rows=` when not.
pub(crate) fn cmd_offscreen(term: &Arc<Mutex<Terminal>>, frame: &FrameSlot, rest: &str) -> String {
    let args = match offscreen_args(rest) {
        Ok(args) => args,
        Err(err) => return err,
    };
    let mut look = frame.look_frame();
    let (read, alt, seq, screen) = {
        let t = term_lock_whole_when(term, Lane::SERVED, |_| args.screen);
        let archive = t.alt_archive();
        let since = match args.since {
            None => 0,
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
        let screen = args
            .screen
            .then(|| (look.text(&t), crate::frame_unfinished_now(&t)));
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
    out.push_str(frame_open_tail(frame_open));
    if let Some(screen) = screen {
        let _ = write!(out, " screen_rows={}", screen.row_count());
    }
    out.push('\n');
    for row in &read.rows {
        push_visible(&mut out, row, VisibleTrim::None);
        out.push('\n');
    }
    if let Some(screen) = screen {
        out.push_str(screen.whole());
    }
    out
}

/// `line <n>` -> `OK <text>\n` for the line at MONOTONIC ABSOLUTE row `n`, or `ERR out of range\n` / `ERR evicted\n` / `ERR unreadable\n`.
/// A live row is read in a whole frame ([`term_lock_whole_when`]) with no word for one still unfinished; a history row at once.
pub(crate) fn cmd_line(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let trimmed = rest.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return "ERR usage: line <abs_row>\n".to_string();
    }
    let Ok(n) = trimmed.parse::<u64>() else {
        return "ERR usage: line <abs_row>\n".to_string();
    };
    let live = |t: &Terminal| n >= screen_first_abs(t);
    let row = term_lock_whole_when(term, Lane::SERVED, live).abs_row_text_within(n, usize::MAX);
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

/// The absolute row of the live screen's first row: a row before it is history, no part of a frame.
fn screen_first_abs(t: &Terminal) -> u64 {
    let grid = t.grid();
    grid.oldest_absolute_row()
        .saturating_add(grid.scrollback_lines() as u64)
}

/// Execute `aterm ctl search` across scrollback and visible grid rows.
/// The plan is held in a whole frame ([`term_lock_whole`]); ` frame=open` says a hold found the frame unfinished, which is no ` incomplete` cause.
pub(crate) fn cmd_search(term: &Arc<Mutex<Terminal>>, rest: &str, rewrap: &RewrapGauge) -> String {
    cmd_search_capped(term, rest, search_max_lines(), || rewrap.reading())
}

fn cmd_search_capped(
    term: &Arc<Mutex<Terminal>>,
    rest: &str,
    depth_cap: usize,
    rewrap: impl Fn() -> RewrapReading,
) -> String {
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
        let t = term_lock_whole(term);
        let open = frame_open_tail(crate::frame_unfinished_now(&t));
        return format!("OK 0{open} screen_first={}\n", screen_first_abs(&t));
    }
    let before = rewrap();
    let searched = search_full_history_capped(term, pat, case_sensitive, is_regex, depth_cap);
    let after = rewrap();
    let rewrapping = before.running || after.running || before.jobs != after.jobs;
    search_reply(searched, rewrapping)
}

fn search_reply(
    searched: Result<FullHistorySearch, SearchOptionsError>,
    rewrapping: bool,
) -> String {
    match searched {
        Ok(search) => {
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
            let open = frame_open_tail(search.frame_open);
            let mut out = format!(
                "OK {}{qualifier}{open} screen_first={}\n",
                search.results.matches.len(),
                search.base_y,
            );
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

/// Search results together with the absolute-row coordinate frame the index was keyed against.
pub(crate) struct FullHistorySearch {
    pub(crate) results: SearchResults,
    pub(crate) point_match: Option<SearchMatch>,
    pub(crate) base_y: i64,
    pub(crate) absolute_row_revision: u64,
    pub(crate) history_renumber_epoch: u64,
    pub(crate) content_seq: u64,
    pub(crate) cols: usize,
    pub(crate) consistent: bool,
    pub(crate) frame_stable: bool,
    pub(crate) searched_lines: Option<usize>,
    /// A hold of the search found the program's DEC 2026 frame unfinished.
    pub(crate) frame_open: bool,
}

/// [`search_full_history_capped`] at the configured depth, as the socket verb runs it.
#[cfg(test)]
pub(crate) fn search_full_history(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
) -> Result<FullHistorySearch, SearchOptionsError> {
    search_full_history_capped(term, query, case_sensitive, is_regex, search_max_lines())
}

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
        ReadOn::Control,
    )
}

/// Direction-aware full-history query for the incremental search overlay.
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
        ReadOn::EventLoop,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the depth cap and the reading thread ride alongside the query's own surface, like \
              the anchor"
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
    on: ReadOn,
) -> Result<FullHistorySearch, SearchOptionsError> {
    let request = search_request(depth_cap, anchor);
    full_history_search(search_terminal(term, &request, on, |built, _| {
        built.query(query, case_sensitive, is_regex, direction, strict)
    }))
}

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
        frame_open: searched.frame_open,
    })
}

const fn search_request(depth_cap: usize, anchor: Option<(usize, usize)>) -> SearchRequest {
    SearchRequest {
        depth_cap,
        layout: SearchLayout::Logical,
        anchor,
    }
}

struct Searched<T> {
    answer: T,
    key: SearchKey,
    wrap: WrapFrame,
    verdict: SearchVerdict,
    /// A hold found the program's DEC 2026 frame unfinished.
    frame_open: bool,
}

/// One search through `term`'s index slot; only a control thread's plan hold waits for a whole frame.
fn search_terminal<T>(
    term: &Arc<Mutex<Terminal>>,
    request: &SearchRequest,
    on: ReadOn,
    answer: impl FnOnce(&mut BuiltSearch, bool) -> T,
) -> Searched<T> {
    let unfinished = crate::frame_unfinished_now;
    let (mut plan, mut frame_open) = {
        let mut t = term_lock_whole_when(term, on.lane(), |_| on.waits());
        (t.search_plan(request), unfinished(&t))
    };
    let hit = plan.is_hit();
    while plan.rows_to_read() > 0 {
        let t = term_lock(term);
        frame_open |= unfinished(&t);
        t.read_search_chunk(&mut plan, SNAPSHOT_CHUNK_LINES);
    }
    let mut built = plan.build();
    let answer = answer(&mut built, hit);
    let (key, wrap) = (*built.key(), built.wrap());
    let (verdict, discard) = {
        let mut t = term_lock(term);
        frame_open |= unfinished(&t);
        t.finish_search(built)
    };
    drop(discard);
    keep_resident(term);
    Searched {
        answer,
        key,
        wrap,
        verdict,
        frame_open,
    }
}

/// One exact point-relative match from the full-history index.
pub(crate) struct FullHistoryPoint {
    pub(crate) point_match: Option<SearchMatch>,
    pub(crate) base_y: i64,
    pub(crate) absolute_row_revision: u64,
    pub(crate) history_renumber_epoch: u64,
    pub(crate) content_seq: u64,
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
    let request = search_request(search_max_lines(), Some(anchor));
    full_history_point(search_terminal(
        term,
        &request,
        ReadOn::EventLoop,
        |built, hit| {
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
        },
    ))
}

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

static SEARCH_MAX_LINES: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_CACHED_LINES);

/// Set the search index depth cap from config (`search_history_lines`).
pub(crate) fn set_search_max_lines(max: usize) {
    SEARCH_MAX_LINES.store(max.max(1), Ordering::Relaxed);
}

fn search_max_lines() -> usize {
    SEARCH_MAX_LINES.load(Ordering::Relaxed)
}

const SNAPSHOT_CHUNK_LINES: usize = 1024;

const SEARCH_RESIDENT_CAP: usize = 8;

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

    fn touch(&self, term: &Arc<Mutex<Terminal>>) -> Option<Arc<Mutex<Terminal>>> {
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

    fn keep(&self, term: &Arc<Mutex<Terminal>>) {
        if let Some(evicted) = self.touch(term) {
            let released = term_lock(&evicted).take_search_index();
            drop(released);
        }
    }
}

#[cfg(not(test))]
fn keep_resident(term: &Arc<Mutex<Terminal>>) {
    static SEARCH_RESIDENCY: SearchResidency = SearchResidency::new(SEARCH_RESIDENT_CAP);
    SEARCH_RESIDENCY.keep(term);
}

#[cfg(test)]
fn keep_resident(term: &Arc<Mutex<Terminal>>) {
    thread_local! {
        static SEARCH_RESIDENCY: SearchResidency = const { SearchResidency::new(SEARCH_RESIDENT_CAP) };
    }
    SEARCH_RESIDENCY.with(|ledger| ledger.keep(term));
}

/// `custody` -> one line answering "why did my selection disappear?".
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

const MODE_KEYS: [&str; 14] = [
    "alt_screen",
    "cursor_visible",
    "app_cursor_keys",
    "app_keypad",
    "bracketed_paste",
    "mouse_mode",
    "mouse_encoding",
    "insert_mode",
    "auto_wrap",
    "origin_mode",
    "kitty_keyboard",
    "modify_other_keys",
    "focus_reporting",
    "sync_output",
];

const fn const_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const _: () = {
    let mut i = 0;
    while i < MODE_KEYS.len() {
        let key = MODE_KEYS[i].as_bytes();
        assert!(!key.is_empty(), "empty mode key");
        let mut b = 0;
        while b < key.len() {
            assert!(
                key[b].is_ascii_lowercase() || key[b] == b'_',
                "mode key must be lowercase snake_case"
            );
            b += 1;
        }
        let mut j = i + 1;
        while j < MODE_KEYS.len() {
            assert!(
                !const_bytes_eq(key, MODE_KEYS[j].as_bytes()),
                "duplicate mode key"
            );
            j += 1;
        }
        i += 1;
    }
};

/// `modes` -> `OK <n>\n` then `n` `key=value` lines, one per introspected mode.
pub(crate) fn cmd_modes(term: &Arc<Mutex<Terminal>>) -> String {
    use aterm_types::keyboard::KeyboardMode as KM;
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
    let kbd = t.keyboard_mode();
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
    let values: [String; MODE_KEYS.len()] = [
        t.is_alternate_screen().to_string(),
        t.cursor_visible().to_string(),
        m.application_cursor_keys.to_string(),
        m.application_keypad.to_string(),
        m.bracketed_paste.to_string(),
        mouse_mode.to_string(),
        mouse_encoding.to_string(),
        m.insert_mode.to_string(),
        m.auto_wrap.to_string(),
        m.origin_mode.to_string(),
        kitty_str,
        modify_other_keys.to_string(),
        m.focus_reporting.to_string(),
        m.synchronized_output.to_string(),
    ];
    let mut out = format!("OK {}\n", MODE_KEYS.len());
    for (key, value) in MODE_KEYS.into_iter().zip(values) {
        out.push_str(key);
        out.push('=');
        out.push_str(&value);
        out.push('\n');
    }
    out
}

/// `appstatus` -> the status-surface ledger.
pub(crate) fn cmd_appstatus(proxy: &EventLoopProxy<Wake>) -> String {
    match control_media::call_main(proxy, |reply| Wake::ReadAppStatus { reply }) {
        Ok(rows) if rows.is_empty() => "OK 0\n".to_string(),
        Ok(rows) => format!("OK {}\n{}\n", rows.len(), rows.join("\n")),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// Observe native focus without an input request, activation or key-event post.
pub(crate) fn cmd_native_focus(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    native_focus_with(rest, || {
        control_media::call_main(proxy, |reply| Wake::NativeFocus { reply })
            .map_err(|error| error.to_string())
    })
}

fn native_focus_with(
    rest: &str,
    read: impl FnOnce() -> Result<(Option<crate::platform::NativeFocus>, u64), String>,
) -> String {
    if !rest.trim().is_empty() {
        return "ERR usage: native-focus\n".to_string();
    }
    match read() {
        Ok((Some(focus), posted)) => format!(
            "OK available=1 window={} app_active={} window_key={} key={} posted_keys={posted}\n",
            focus.window,
            u8::from(focus.app_active),
            u8::from(focus.window_key),
            u8::from(focus.app_active && focus.window_key),
        ),
        Ok((None, posted)) => format!(
            "OK available=0 window=0 app_active=- window_key=- key=- posted_keys={posted}\n"
        ),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `appnotice <toolchain|update|harness|proc> <text>` -> `OK recorded`.
pub(crate) fn cmd_appnotice(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    const USAGE: &str = "ERR usage: appnotice <toolchain|update|harness|proc> <text>\n";
    const MAX_TEXT_BYTES: usize = 512;
    let t = rest.trim();
    let Some((lane, text)) = t.split_once(char::is_whitespace) else {
        return USAGE.to_string();
    };
    let text = text.trim();
    if !matches!(lane, "toolchain" | "update" | "harness" | "proc") || text.is_empty() {
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

/// `messages [<n>] [since=<id>] [tag=<tag>] [sev=<sev>] [need=later] [live]` -> `OK <count>` and one `message <id> …` row per record.
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

/// `notice <post|progress|done|settle|ask|later|dismiss|act> …` -> one status line.
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

/// `members requests | allow [--again] <name> | invite | ignore <name>` -> the window's reply.
/// `allow` and `invite` never carry the phrase: it shows on this Mac's screen alone
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.5).
#[cfg(unix)]
pub(crate) fn cmd_members(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let asked = match crate::members_desk::Asked::parse(rest) {
        Ok(asked) => asked,
        Err(usage) => return usage.to_owned(),
    };
    match control_media::call_main(proxy, |reply| Wake::Members { asked, reply }) {
        Ok(reply) => reply,
        Err(error) => format!("ERR {error}\n"),
    }
}

/// The usage line every malformed `devtools` form answers.
pub(crate) const DEVTOOLS_USAGE: &str = "ERR usage: devtools hand-over [now]\n";

/// Parses `devtools hand-over [now]` on the control thread, so a malformed form is a usage error and never a wake.
pub(crate) fn parse_devtools(rest: &str) -> Result<bool, &'static str> {
    let mut words = rest.split_whitespace();
    let now = match (words.next(), words.next(), words.next()) {
        (Some("hand-over"), None, None) => false,
        (Some("hand-over"), Some("now"), None) => true,
        _ => return Err(DEVTOOLS_USAGE),
    };
    Ok(now)
}

/// The wire answer to one asked hand-over.
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

/// Read the existing setup observation without a session, probe, input or focus change.
pub(crate) fn cmd_setup(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    setup_status_over(rest, &|| {
        control_media::call_main_within(proxy, std::time::Duration::from_millis(500), |reply| {
            Wake::ReadSetupStatus { reply }
        })
        .map_err(|error| error.to_string())
    })
}

fn setup_status_over(rest: &str, read: &dyn Fn() -> Result<String, String>) -> String {
    if rest.trim() != "status" {
        return "ERR usage: setup status\n".into();
    }
    match read() {
        Ok(state) => crate::system_state_access::setup_status_answer(&state),
        Err(error) => format!("ERR {error}\n"),
    }
}

#[cfg(test)]
#[test]
fn background_setup_status_only_reads_and_rejects_action_words_before_posting() {
    use aterm_types::macos_setup::{Observation, Plan, Selection, Setting, Subject};

    let mut selected = Selection::none();
    selected.set(Setting::CapsLock, true);
    let mut unread = Observation::unknown(Subject::Setting(Setting::CapsLock));
    unread.note = Some("private approval sentinel".into());
    unread.detail = Some("private source sentinel".into());
    unread.now = Some("private value sentinel".into());
    unread.wanted = "private wanted sentinel".into();
    let mut status = crate::SetupStatus {
        plan: Some(Plan::from(selected)),
        observed: vec![unread],
        observed_generation: 7,
        finished: true,
        ..crate::SetupStatus::default()
    };
    let reads = std::cell::Cell::new(0);
    let reply = setup_status_over("status", &|| {
        reads.set(reads.get() + 1);
        Ok(crate::system_state_access::background_setup_fields(
            &status,
            1_700_000_000,
            false,
        ))
    });
    // The real writer's answer: no note, value or wanted word of a row, only its fixed words.
    assert_eq!(
        reply,
        format!(
            "OK schema={} setup=unread:capslock verified=0 verified_at=- observed_generation=7 \
             rows=capslock:unread caps_live=unknown busy=0 finished=1 queued=0 changing=0 \
             retrying=0 running=-\n",
            aterm_types::macos_setup::SETUP_STATUS_SCHEMA
        )
    );
    status.observed.clear();
    assert_eq!(
        crate::system_state_access::background_setup_fields(&status, 1_700_000_000, false),
        "setup=pending:capslock verified=0 verified_at=- observed_generation=7 \
         rows=capslock:unknown caps_live=unknown busy=0 finished=1 queued=0 changing=0 \
         retrying=0 running=-"
    );
    let cached = crate::system_state_access::background_setup_fields(&status, 1_700_000_000, false);
    let pending = crate::system_state_access::background_setup_fields(&status, 1_700_000_000, true);
    assert_eq!(
        pending.split(" busy=").next(),
        cached.split(" busy=").next(),
        "pending window work changes no readiness evidence"
    );
    assert!(pending.split_whitespace().any(|field| field == "busy=1"));
    for (busy, queued, changing, retrying) in [
        (true, false, false, false),
        (false, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        status.busy = busy;
        status.queued = queued;
        status.changing = changing;
        status.retrying = retrying;
        status.finished = false;
        status.running = vec![Setting::Repositories, Setting::CapsLock, Setting::CapsLock];
        let fields =
            crate::system_state_access::background_setup_fields(&status, 1_700_000_000, false);
        for field in [
            format!("busy={}", u8::from(busy)),
            format!("queued={}", u8::from(queued)),
            format!("changing={}", u8::from(changing)),
            format!("retrying={}", u8::from(retrying)),
            "finished=0".into(),
            "running=capslock,repos".into(),
            "verified=0".into(),
        ] {
            assert!(fields.split_whitespace().any(|word| word == field));
        }
    }
    let initial = crate::system_state_access::background_setup_fields(
        &crate::SetupStatus::default(),
        1_700_000_000,
        false,
    );
    assert!(
        initial
            .split_whitespace()
            .any(|field| field == "finished=0")
    );
    assert!(initial.split_whitespace().any(|field| field == "running=-"));
    assert_eq!(reads.get(), 1);
    for rest in ["", "start", "on", "status now"] {
        assert!(
            setup_status_over(rest, &|| panic!("an invalid query posted a wake"))
                .starts_with("ERR usage:")
        );
    }
    assert!(setup_status_over("status", &|| Err("unavailable".into())).starts_with("ERR "));
}

/// `devtools hand-over [now]` -> one status line. `now` is `permissions walk now devtools`:
/// the walk, Developer Tools first.
pub(crate) fn cmd_devtools(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let now = match parse_devtools(rest) {
        Ok(now) => now,
        Err(usage) => return usage.to_string(),
    };
    if !cfg!(target_os = "macos") {
        return "ERR unsupported: the Developer Tools list exists only on macOS\n".to_string();
    }
    if now {
        return ask_walk(proxy, vec![crate::permission_walk::Subject::DeveloperTools]);
    }
    match control_media::call_main(proxy, |reply| Wake::DevToolsHandOverAsked { reply }) {
        Ok(outcome) => devtools_reply(outcome),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// The usage line every malformed `permissions` form answers.
pub(crate) const PERMISSIONS_USAGE: &str =
    "ERR usage: permissions walk now [fda|devtools|accessibility]...\n";

/// Parses `permissions walk now [<subject>…]` on the control thread: the subjects named, in
/// order, so a malformed form is a usage error and never a wake.
pub(crate) fn parse_permissions(
    rest: &str,
) -> Result<Vec<crate::permission_walk::Subject>, &'static str> {
    let mut words = rest.split_whitespace();
    if (words.next(), words.next()) != (Some("walk"), Some("now")) {
        return Err(PERMISSIONS_USAGE);
    }
    words
        .map(|word| crate::permission_walk::Subject::parse(word).ok_or(PERMISSIONS_USAGE))
        .collect()
}

/// `permissions walk now [<subject>…]` -> one status line ([`crate::permission_walk::Started::reply`]).
pub(crate) fn cmd_permissions(proxy: &EventLoopProxy<Wake>, rest: &str) -> String {
    let subjects = match parse_permissions(rest) {
        Ok(subjects) => subjects,
        Err(usage) => return usage.to_string(),
    };
    if !cfg!(target_os = "macos") {
        return "ERR unsupported: aterm's permission walk is macOS's\n".to_string();
    }
    ask_walk(proxy, subjects)
}

/// How long the `permissions walk now` lane waits for the main thread to take the ask before it
/// gives the ask up ([`serve_ask_walk`]): half the 10 s a front-door client (`aterm doctor`,
/// `aterm init`, a sign-out) waits for its answer, so the client hears whether a walk starts,
/// and none starts once it stopped listening.
const WALK_ASK_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

fn ask_walk(
    proxy: &EventLoopProxy<Wake>,
    subjects: Vec<crate::permission_walk::Subject>,
) -> String {
    if let Some(refusal) = control_media::main_thread_refusal() {
        return format!("ERR {refusal}\n");
    }
    serve_ask_walk(
        subjects,
        &mut control_media::MainPost::new(proxy),
        WALK_ASK_WITHIN,
        &mut crate::control::caller_hung_up,
        crate::control::hangup_park,
    )
}

/// [`ask_walk`]'s lane past the main-thread refusal: posts the ask with a
/// [`control_media::Claim`] and waits up to `within` for the main thread's answer, re-checking
/// `hung_up` between `park` slices. An ask the main thread has not taken by then, or by the
/// client's hang-up, the lane gives up: it answers `ERR late: …` (or
/// [`crate::control::HUNG_UP_REPLY`] to a client gone), and the main thread starts no walk for
/// it when it comes to it ([`crate::App::take_permission_walk_ask`]). One the main thread took
/// first is answered with the walk's answer.
pub(crate) fn serve_ask_walk(
    subjects: Vec<crate::permission_walk::Subject>,
    post: &mut control_media::MainPost<'_>,
    within: std::time::Duration,
    hung_up: &mut dyn FnMut() -> bool,
    park: fn(std::time::Duration) -> std::time::Duration,
) -> String {
    use control_media::Waited;
    let claim = control_media::Claim::default();
    let asked = claim.clone();
    let (answer, _hop) = match post.ask(|reply| Wake::PermissionWalkAsked {
        subjects,
        reply,
        claim: asked,
    }) {
        Ok(posted) => posted,
        Err(error) => return format!("ERR {error}\n"),
    };
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(within)
        .unwrap_or_else(|| now + std::time::Duration::from_secs(365 * 24 * 3600));
    let gave_up = match control_media::park_for_answer(&answer, deadline, hung_up, park) {
        Waited::Answer(started) => return started.reply(),
        Waited::Dropped => return "ERR main-thread reply dropped\n".to_owned(),
        Waited::HungUp => crate::control::HUNG_UP_REPLY.to_owned(),
        Waited::Deadline => format!(
            "ERR late: the window did not take the ask within {}s; no walk starts\n",
            within.as_secs()
        ),
    };
    if claim.give_up() {
        return gave_up;
    }
    // The main thread took the ask first: its answer follows in the same turn.
    match control_media::recv_main(&answer) {
        Ok(started) => started.reply(),
        Err(error) => format!("ERR {error}\n"),
    }
}

/// `title` -> `OK <window title>\n` (the OSC 0/2 title; empty if unset).
pub(crate) fn cmd_title(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    format!("OK {}\n", t.title())
}

/// The usage line `story` answers every junk form with, spelled from [`StoryVerb::TOLD_WORDS`](crate::presence::StoryVerb::TOLD_WORDS) so it and the parser agree.
pub(crate) fn story_usage() -> String {
    format!(
        "ERR usage: story <{}> [<text>]\n",
        crate::presence::StoryVerb::TOLD_WORDS.join("|")
    )
}

/// The grammar of `story <verb> [<text>]`, checked on the control thread so a junk form is a usage error and never a wake.
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

/// `story chose [<text>]` -> `OK story=<n>`; `<text>` is checked ([`parse_story`]), not kept.
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

/// `cwd` -> `OK <pct-encoded shell-reported cwd>\n` (empty until the shell reports one).
pub(crate) fn cmd_cwd(term: &Arc<Mutex<Terminal>>) -> String {
    use crate::cwd_native::ReportedCwd as _;
    let t = term_lock(term);
    let cwd = t.native_working_directory();
    format!("OK {}\n", pct_encode(cwd.as_deref().unwrap_or("")))
}

/// `text --json` with no person, launch or input stamps -> `OK 1\n{"rows":[...],"cursor":{...},"dims":{...},"seq":N,"gen":"E.S","title":"<OSC 0/2 title, \"\" if unset>","human_ms":null,"human_seq":0,"launch":null[,...]}`.
#[cfg(test)]
pub(crate) fn cmd_text_json_opt(term: &Arc<Mutex<Terminal>>, args: TextArgs) -> String {
    cmd_text_json_read(
        term,
        &FrameSlot::default(),
        args,
        || (None, None, 0),
        None,
        None,
    )
}

/// `cmd_text_json_opt` as the dispatch answers it.
pub(crate) fn cmd_text_json_looked(
    term: &Arc<Mutex<Terminal>>,
    ctx: &crate::SessionCtx,
    args: TextArgs,
) -> String {
    let person = || {
        let human_seq = ctx.human_input.seq();
        let now_us = crate::metrics::now_us();
        PersonStamps {
            human_ms: ctx.human_input.ms_since(now_us),
            unkeyed_ms: ctx.human_input.unkeyed_ms(now_us),
            human_seq,
            typed_ms: ctx.human_input.typed_ms_since(now_us),
        }
    };
    cmd_text_json_read(
        term,
        &ctx.frame,
        args,
        person,
        Some(&ctx.launch),
        Some((&ctx.output_echo, &ctx.sink)),
    )
}

/// The person's stamps a `text --json` read carries: `human_ms`, `unkeyed_ms`, `human_seq` and
/// `human_typed_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PersonStamps {
    /// The last gesture's age ([`crate::human_input::HumanInputStamp::ms_since`]).
    pub(crate) human_ms: Option<u64>,
    /// How long the session was served with no gesture
    /// ([`crate::human_input::HumanInputStamp::unkeyed_ms`]).
    pub(crate) unkeyed_ms: Option<u64>,
    /// The gestures counted ([`crate::human_input::HumanInputStamp::seq`]).
    pub(crate) human_seq: u64,
    /// The last typing's age ([`crate::human_input::HumanInputStamp::typed_ms_since`]), written
    /// only where it differs from `human_ms`.
    pub(crate) typed_ms: Option<u64>,
}

impl From<(Option<u64>, Option<u64>, u64)> for PersonStamps {
    /// `human_ms`, `unkeyed_ms` and `human_seq`, the last gesture typing, so the read writes no
    /// `human_typed_ms` of its own.
    fn from((human_ms, unkeyed_ms, human_seq): (Option<u64>, Option<u64>, u64)) -> Self {
        Self {
            human_ms,
            unkeyed_ms,
            human_seq,
            typed_ms: human_ms,
        }
    }
}

#[cfg(test)]
thread_local! {
    static WHILE_ROWS_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// `person` and the first `inputs` count are taken under the rows' hold once its wait is over, so neither is older
/// than the frame read; before the rows, so a gesture they miss moves `human_seq` past an `if-human=` fence.
/// `"title"` is read in that same hold, so the title and the rows are one snapshot (a separate `title` read takes
/// its own lock and can fall on either side of a frame).
pub(crate) fn cmd_text_json_read<P: Into<PersonStamps>>(
    term: &Arc<Mutex<Terminal>>,
    frame: &FrameSlot,
    args: TextArgs,
    person: impl FnOnce() -> P,
    launch: Option<&crate::launch_witness::LaunchWitness>,
    input: Option<(&crate::app_input::OutputEchoTracker, &SinkWriter)>,
) -> String {
    let mut look = frame.look_frame();
    let (person_stamps, inputs_before): (PersonStamps, _);
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
        title,
        first,
        end,
        launch,
        frame_open,
    ) = {
        let t = term_lock_whole(term);
        person_stamps = person().into();
        let human_seq = person_stamps.human_seq;
        inputs_before = input.map(|(tracker, sink)| tracker.input_settled(sink));
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
            t.title().to_owned(),
            first,
            end,
            launch.map_or_else(|| "null".to_string(), |w| w.json(human_seq)),
            t.frame_unfinished_at(std::time::Instant::now()),
        )
    };
    let PersonStamps {
        human_ms,
        unkeyed_ms,
        human_seq,
        typed_ms,
    } = person_stamps;
    let inputs_after = input.map(|(tracker, _)| tracker.input_sent());
    let submits = input.map(|(tracker, _)| tracker.submits());
    let rows_text: Vec<&str> = match &text {
        Some(text) => (first..end).map(|r| text.row(r)).collect(),
        None => display_rows.iter().map(String::as_str).collect(),
    };
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
             \"dims\":{{\"rows\":{rows},\"cols\":{cols}}},\"seq\":{seq},\"gen\":\"{generation}\",{},\
             \"human_ms\":{human},\"human_seq\":{human_seq},\"launch\":{launch}",
            c.row,
            c.col,
            json_str_field("style", style),
            json_str_field("title", &title),
            human = human_ms.map_or_else(|| "null".to_string(), |ms| ms.to_string()),
        );
        if args.trim {
            let _ = write!(out, ",\"trimmed\":{}", selected.saturating_sub(sent));
        }
        if first > 0 {
            let _ = write!(out, ",\"first\":{first}");
        }
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
        // The person's typing, where it is not their last gesture: absent, `human_ms` says it.
        if typed_ms != human_ms {
            let typed = typed_ms.map_or_else(|| "null".to_string(), |ms| ms.to_string());
            let _ = write!(out, ",\"human_typed_ms\":{typed}");
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

/// `cursor --json` -> `{"row":R,"col":C,"visible":bool,"style":"<name>"[,"frame":"open"]}`, read as `cursor` is.
pub(crate) fn cmd_cursor_json(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock_whole(term);
    let c = t.cursor();
    json_ok(&format!(
        "{{\"row\":{},\"col\":{},\"visible\":{},{}{}}}",
        c.row,
        c.col,
        t.cursor_visible(),
        json_str_field("style", cursor_style_name(t.cursor_style())),
        frame_open_field(&t),
    ))
}

/// `,"frame":"open"` when `t`'s frame is unfinished, else nothing: a JSON reply's last key.
fn frame_open_field(t: &Terminal) -> &'static str {
    if crate::frame_unfinished_now(t) {
        ",\"frame\":\"open\""
    } else {
        ""
    }
}

const fn underline_style_name(u: UnderlineStyle) -> &'static str {
    match u {
        UnderlineStyle::None => "none",
        UnderlineStyle::Single => "single",
        UnderlineStyle::Double => "double",
        UnderlineStyle::Curly => "curly",
        UnderlineStyle::Dotted => "dotted",
        UnderlineStyle::Dashed => "dashed",
    }
}

fn push_bool(out: &mut String, v: bool) {
    out.push_str(if v { "true" } else { "false" });
}

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

/// The wire name of a DEC [`LineSize`](aterm_core::grid::LineSize).
pub(crate) const fn line_size_name(s: aterm_core::grid::LineSize) -> &'static str {
    use aterm_core::grid::LineSize;
    match s {
        LineSize::SingleWidth => "single",
        LineSize::DoubleWidth => "double_width",
        LineSize::DoubleHeightTop => "double_height_top",
        LineSize::DoubleHeightBottom => "double_height_bottom",
        _ => "single",
    }
}

/// The DEC line size of live screen row `r` (default single-width), whatever the viewport shows.
pub(crate) fn row_line_size(t: &Terminal, r: usize) -> aterm_core::grid::LineSize {
    u16::try_from(r)
        .ok()
        .and_then(|rr| t.grid().row_at_screen(rr))
        .map_or(aterm_core::grid::LineSize::SingleWidth, |row| {
            row.line_size()
        })
}

fn distinct_images(t: &Terminal) -> Vec<(usize, usize, std::sync::Arc<ImageData>)> {
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

/// Append one inline image as a JSON object for the styled frame.
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

const STYLED_IMAGE_BYTES: usize = 128;

const NO_LINK: u32 = u32::MAX;

struct StyledLink {
    url: std::sync::Arc<str>,
    id: Option<std::sync::Arc<str>>,
}

struct StyledCellSnap {
    cell: RenderCell,
    glyph: std::ops::Range<usize>,
    wide_lead: bool,
    blink: bool,
    link: u32,
}

struct StyledRowSnap {
    glyphs: String,
    cells: Vec<StyledCellSnap>,
}

impl StyledRowSnap {
    fn glyph(&self, col: usize) -> &str {
        &self.glyphs[self.cells[col].glyph.clone()]
    }
}

struct StyledSelectionSnap {
    start_row: i32,
    start_col: u16,
    end_row: i32,
    end_col: u16,
    kind: &'static str,
    is_block: bool,
}

/// Everything the styled JSON frame serializes.
pub(crate) struct StyledFrameSnapshot {
    seq: u64,
    rows: usize,
    cols: usize,
    cursor_row: u16,
    cursor_col: u16,
    cursor_visible: bool,
    cursor_style: &'static str,
    cursor_color: Option<[u8; 3]>,
    cells: Vec<StyledRowSnap>,
    links: Vec<StyledLink>,
    line_sizes: Vec<&'static str>,
    selection: Option<StyledSelectionSnap>,
    selection_bg: Option<[u8; 3]>,
    selection_fg: Option<[u8; 3]>,
    images: Vec<(usize, usize, std::sync::Arc<ImageData>)>,
    image_payloads: bool,
}

impl StyledFrameSnapshot {
    /// Whether the frame serializes an image payload.
    pub(crate) const fn carries_image_payloads(&self) -> bool {
        self.image_payloads && !self.images.is_empty()
    }

    /// Cells in the gathered frame.
    pub(crate) fn cell_count(&self) -> usize {
        self.cells.iter().map(|row| row.cells.len()).sum()
    }
}

#[cfg(test)]
impl StyledFrameSnapshot {
    fn glyph(&self, row: usize, col: usize) -> &str {
        self.cells[row].glyph(col)
    }
}

const fn selection_type_name(kind: SelectionType) -> &'static str {
    match kind {
        SelectionType::Simple => "simple",
        SelectionType::Block => "block",
        SelectionType::Semantic => "semantic",
        SelectionType::Lines => "lines",
        _ => "linear",
    }
}

fn styled_optional_rgb(color: Option<[u8; 3]>, fallback: &str) -> String {
    color.map_or_else(
        || format!("\"{fallback}\""),
        |[r, g, b]| format!("\"{r:02x}{g:02x}{b:02x}\""),
    )
}

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

/// Every OSC 8 link on the visible grid.
pub(crate) struct FrameLinks {
    table: Vec<StyledLink>,
    by_row: Vec<Vec<(u16, u32)>>,
}

/// Build the frame's [`FrameLinks`] in ONE pass over the extras map.
pub(crate) fn gather_frame_links(t: &Terminal, rows: usize, cols: usize) -> FrameLinks {
    let grid = t.grid();
    let extras = grid.extras();
    if extras.is_empty() {
        return FrameLinks {
            table: Vec::new(),
            by_row: Vec::new(),
        };
    }
    let mut table: Vec<StyledLink> = Vec::new();
    let mut by_row: Vec<Vec<(u16, u32)>> = Vec::new();
    by_row.resize_with(rows, Vec::new);
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

/// Gather the styled frame's raw material.
pub(crate) fn gather_styled_frame(t: &Terminal) -> StyledFrameSnapshot {
    #[cfg(test)]
    crate::work_counts::styled_frame_gathered();
    let rows = t.rows() as usize;
    let cols = t.cols() as usize;
    let blank = t.implicit_blank_render_cell();
    let mut cells: Vec<StyledRowSnap> = Vec::with_capacity(rows);
    let mut line_sizes: Vec<&'static str> = Vec::with_capacity(rows);
    let FrameLinks {
        table: links,
        by_row: links_by_row,
    } = gather_frame_links(t, rows, cols);
    let mut rendered: Vec<RenderCell> = Vec::with_capacity(cols);
    for r in 0..rows {
        t.render_row_at_screen_into(r, &mut rendered);
        let view = t.grid().screen_row_view(r as u16);
        let mut row_cells: Vec<StyledCellSnap> = Vec::with_capacity(cols);
        let mut glyphs = String::with_capacity(cols);
        let row_links: &[(u16, u32)] = links_by_row.get(r).map_or(&[][..], Vec::as_slice);
        let mut li = 0usize;
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
        images: distinct_images(t),
        image_payloads: true,
    }
}

/// Serialize a gathered [`StyledFrameSnapshot`] to the single-line JSON frame, lock-free and
/// unbounded, for the tests: a reply builds its frame within its cap
/// ([`serialize_styled_frame_within`]).
#[cfg(test)]
pub(crate) fn serialize_styled_frame(snap: &StyledFrameSnapshot) -> String {
    let payloads = styled_frame_payloads(snap);
    let mut out = String::with_capacity(styled_frame_estimate(snap, &payloads));
    write_styled_frame(&mut out, snap, &payloads, usize::MAX);
    out
}

/// The fewest bytes [`write_styled_cell_json`] writes for one cell: its keys and the shortest
/// value each can take (232 bytes) less a margin, so a grid of `cells` serializes to no fewer
/// than `cells` times this.
pub(crate) const STYLED_CELL_FLOOR_BYTES: usize = 200;

/// A lower bound on the styled frame of a `rows` by `cols` grid, known before anything is
/// gathered: a frame past a cap by this bound need not be built to be refused.
pub(crate) const fn styled_frame_floor(rows: usize, cols: usize) -> usize {
    rows.saturating_mul(cols)
        .saturating_mul(STYLED_CELL_FLOOR_BYTES)
}

/// Serialize `snap` (its image payloads only when `with_payloads`) within `cap` bytes, or
/// `Err` with the bytes written when it passed `cap`: the writer stops at the row or image
/// that passes it, so it holds at most `cap` and one row or image more. Payloads are encoded
/// only when their base64 and the grid's [`styled_frame_floor`] fit `cap` together, else `Err`
/// with that sum, nothing encoded.
pub(crate) fn serialize_styled_frame_within(
    snap: &StyledFrameSnapshot,
    with_payloads: bool,
    cap: usize,
) -> Result<String, usize> {
    let payloads = if with_payloads && snap.image_payloads {
        let charge = snap
            .images
            .iter()
            .filter(|(_, _, img)| img.bytes.len() <= control_media::MAX_IMAGE_PAYLOAD_BYTES)
            .map(|(_, _, img)| img.bytes.len().div_ceil(3).saturating_mul(4))
            .fold(
                styled_frame_floor(snap.rows, snap.cols),
                usize::saturating_add,
            );
        if charge > cap {
            return Err(charge);
        }
        styled_frame_payloads(snap)
    } else {
        vec![ImagePayload::TRUNCATED; snap.images.len()]
    };
    let mut out = String::with_capacity(styled_frame_estimate(snap, &payloads).min(cap));
    if write_styled_frame(&mut out, snap, &payloads, cap) {
        Ok(out)
    } else {
        Err(out.len())
    }
}

fn styled_frame_payloads(snap: &StyledFrameSnapshot) -> Vec<ImagePayload> {
    if !snap.image_payloads {
        return vec![ImagePayload::TRUNCATED; snap.images.len()];
    }
    let images: Vec<&Arc<ImageData>> = snap.images.iter().map(|(_, _, img)| img).collect();
    image_payloads(&images)
}

fn styled_frame_estimate(snap: &StyledFrameSnapshot, payloads: &[ImagePayload]) -> usize {
    256 + snap.cell_count() * 180
        + payloads
            .iter()
            .map(|payload| STYLED_IMAGE_BYTES + payload.b64().len())
            .sum::<usize>()
}

/// Write the frame, `false` (the frame cut short) as soon as `out` holds more than `budget`.
fn write_styled_frame(
    out: &mut String,
    snap: &StyledFrameSnapshot,
    payloads: &[ImagePayload],
    budget: usize,
) -> bool {
    use std::fmt::Write as _;
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
        if out.len() > budget {
            return false;
        }
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
    debug_assert_eq!(payloads.len(), snap.images.len(), "one payload per image");
    for (i, ((ar, ac, img), payload)) in snap.images.iter().zip(payloads).enumerate() {
        if i > 0 {
            out.push(',');
        }
        styled_image_json(out, *ar, *ac, img, payload);
        if out.len() > budget {
            return false;
        }
    }
    out.push_str("]}");
    out.len() <= budget
}

/// The styled frame (`{"seq":N,"dims":{...},"cursor":{...},"rows":[[StyledCell,...],...],"line_sizes":[...],"selection":...,"selection_bg":...,"selection_fg":...,"images":[...]}`) gathered and serialized in one call, for the tests.
#[cfg(test)]
pub(crate) fn styled_frame_payload(t: &Terminal) -> String {
    serialize_styled_frame(&gather_styled_frame(t))
}

const fn _styled_frame_covers_every_render_input_field(ri: &aterm_core::render::RenderInput) {
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
        cursor_glow_add_ramps: _, // OMITTED: its twin's ramps (the same lights, a slab a ramp), not engine cell content
        glow_halo: _, // OMITTED: host-owned GLOW-HALO radial cursor-effect light quads, not engine cell content
        glow_under: _, // OMITTED: host-owned EMBERFORGE under-glyph flame-body light quads, not engine cell content
        glow_under_ramps: _, // OMITTED: its twin's ramps (the same lights, a slab a ramp), not engine cell content
        fire_patch: _, // OMITTED: host-owned EMBERFORGE per-pixel fire-field patches (render bling), not engine cell content
        cursor_glow_add_damage: _, // OMITTED: producer-side dirty-row revision for host-owned LUMEN quads, not engine cell content
        glow_halo_damage: _, // OMITTED: producer-side dirty-row revision for host-owned radial halos, not engine cell content
        glow_under_damage: _, // OMITTED: producer-side dirty-row revision for host-owned flame-body quads, not engine cell content
        fire_patch_damage: _, // OMITTED: producer-side dirty-row revision for host-owned fire patches, not engine cell content
        cursor_fill_override: _, // OMITTED: host-owned rainbow-cursor block fill, not engine cell content
        bloom_gate: _, // OMITTED: host-owned bloom hold of the cursor effects, not engine cell content
        word_decorations: _, // OMITTED: host-owned sparkle-word decorations, not engine cell content
        ink: _, // OMITTED: host-owned animated-ink fg overrides (render bling), not engine cell content
        char_fg: _, // OMITTED: host-owned EMBERFORGE charred glyph-ink fg overrides (render bling), not engine cell content
        fire_halo: _, // OMITTED: host-owned EMBERFORGE fire contrast-halo strengths (render bling), not engine cell content
        nova_add: _, // OMITTED: host-owned supernova additive-light quads (render bling), not engine cell content
        rain_add: _, // OMITTED: host-owned PHOSPHOR rain additive halos (render bling), not engine cell content
        fx_layers: _, // OMITTED: host-owned retained fx layers (the engine's sprite/bitmap primitive: the companions, the rain's rows, the cards), not engine cell content
        fx_occluders: _, // OMITTED: host chrome rects subtracted from the fx layers, not engine cell content
        fx_prefetch: _,  // OMITTED: GPU residency hints for fx arts; draw nothing
        display_offset: _, // OMITTED: viewport scroll position, not visible-cell content
        base_y: _, // OMITTED: host-consumed re-anchor metadata (absolute row of the top line), not visible-cell content
        absolute_row_revision: _, // OMITTED: host-consumed absolute-row coordinate-space revision, not visible-cell content
        history_renumber_epoch: _, // OMITTED: host-consumed wholesale-renumber epoch (its twin), not visible-cell content
        holder: _, // OMITTED: host-consumed holder fence (which rows a card may be decorated on), not visible-cell content
        screen_mutations: _, // OMITTED: the dev card probe's snapshot date (the terminal's screen mutations), not visible-cell content
        scroll_frac_px: _,   // OMITTED: sub-row present translate, display-only
        grid_top_row: _,     // OMITTED: grid/chrome partition, display-only
        grid_bot_row: _,     // OMITTED: grid/chrome partition, display-only
        apron_row: _,        // OMITTED: incoming-row apron, the present translate's strip source
        apron_stamp: _,      // OMITTED: the apron's extraction stamp, display-only bookkeeping
        fx_clip: _,          // OMITTED: focused-pane present-time post-fx clip box, display-only
        selection: _,        // frame "selection" kind + projected geometry
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
        chrome_rasters: _, // OMITTED: host-owned chrome rasters (the strip's seam and find-bar rails; find-bar, card and scene icons), not engine cell content
        chrome_icon_spares: _, // OMITTED: host scratch storage (retired chrome rasters' emptied icon lists), not frame content
        images: _,             // frame "images"
        wallpaper: _, // OMITTED: host-owned backdrop base layer (render bling), not engine cell content
        default_bg: _, // OMITTED: engine-resolved live default-bg for padding, not per-cell content (cells carry their own bg)
        default_fg: _, // OMITTED: its twin — the effects layer's tint anchor, not per-cell content
        implicit_blank: _, // OMITTED: exact extraction-time empty cell for host overlays; styled rows carry their own resolved cells
        cursor_color: _,   // frame "cursor.color" (fixed RGB or "default")
        snapshot_seq: _,   // frame "seq" (the engine content version stamp)
        content_seq: _, // OMITTED: host effect-attribution clock paired with the exact snapshot, not wire content
        drawn_at: _, // OMITTED: when that content was drawn, the host's effect-attribution age, not wire content
        process_sequence: _, // OMITTED: parser-batch provenance used only by host cursor-effect admission
        input_hot: _, // OMITTED: present-time bloom-defer latency hint, display-only (not cell content)
        terminal_id: _,
        extract_gen: _,
        engine_fill_seq: _,
        engine_alt: _,
        grid_instance: _,
        engine_row_order: _,
        row_rev: _,
        row_rev_lane: _,
        row_shift: _,
        shifted_fill_seq: _,
        composed_fill_seq: _,
    } = ri;
}

const fn _styled_cell_snap_is_fully_serialized(snap: &StyledCellSnap) {
    let StyledCellSnap {
        cell: _,
        glyph: _,     // "glyph" — a range into its row's buffer
        wide_lead: _, // "wide_lead"
        blink: _,     // "blink" in "attrs"
        link: _,
    } = snap;
}

/// `screen` -> the lossless engine-styled grid as a single-line JSON frame in the `OK 1\n<json>\n` read framing.
/// Read in a whole frame ([`term_lock_whole`]); a reply read on one still unfinished closes with `"frame":"open"`.
/// A frame past the reply-line cap is never built: a grid whose [`styled_frame_floor`] passes it,
/// or whose frame a `cells` watch already refused ([`crate::frame_memo::Look::refused`]), is
/// refused ungathered, and any other frame is serialized only up to the cap
/// ([`styled_body_within`]), its pictures left out when they do not fit.
pub(crate) fn cmd_screen_styled_json(
    term: &Arc<Mutex<Terminal>>,
    frame: &FrameSlot,
    rest: &str,
) -> String {
    let since = match screen_args(rest) {
        Ok(since) => since,
        Err(usage) => return usage,
    };
    let cap = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;
    let too_large = |bytes: usize| {
        format!(
            "ERR screen: frame too large ({bytes} bytes > {cap}); `text` or `cell` reads a grid this size\n"
        )
    };
    let mut look = frame.look_frame();
    let (styled, key, tail) = {
        let t = term_lock_whole(term);
        let key = FrameKey::read(&t);
        let etag = key.etag();
        if since == Some(etag) {
            return json_ok(&format!(
                "{{\"unchanged\":true,\"seq\":{},\"gen\":\"{}\",\"etag\":\"{etag:016x}\"{}}}",
                key.seq(),
                key.generation(),
                frame_open_field(&t),
            ));
        }
        let floor = styled_frame_floor(t.rows() as usize, t.cols() as usize);
        if floor > cap {
            return too_large(floor);
        }
        if let Some(reached) = look.refused(&key) {
            return too_large(reached);
        }
        let tail = ScreenTail {
            etag,
            open: crate::frame_unfinished_now(&t),
        };
        (look.styled(&t, &key), key, tail)
    };
    let reply = |frame: &str, tail: ScreenTail| {
        let mut out = String::with_capacity(32 + frame.len());
        out.push_str("OK 1\n");
        out.push_str(frame);
        close_with_etag(out, tail)
    };
    let (out, lighter) = match styled {
        Styled::Hit(published) => (
            Ok(reply(published.body(), tail)),
            published.carries_image_payloads(),
        ),
        Styled::Miss(snap) => {
            let built = if look.keeps_styled() {
                look.publish_styled_within(key, &snap)
                    .map(|frame| reply(frame.body(), tail))
            } else {
                styled_body_within(&snap).map(|(body, _)| reply(&body, tail))
            };
            (built, false)
        }
    };
    drop(look);
    let out = match out {
        Ok(out) if frame_line_len(&out) > cap && lighter => {
            let (snap, tail) = {
                let t = term_lock_whole(term);
                if styled_frame_floor(t.rows() as usize, t.cols() as usize) > cap {
                    return too_large(frame_line_len(&out));
                }
                let tail = ScreenTail {
                    etag: FrameKey::read(&t).etag(),
                    open: crate::frame_unfinished_now(&t),
                };
                (gather_styled_frame(&t), tail)
            };
            serialize_styled_frame_within(&snap, false, cap).map(|body| reply(&body, tail))
        }
        out => out,
    };
    match out {
        Ok(out) if frame_line_len(&out) <= cap => out,
        Ok(out) => too_large(frame_line_len(&out)),
        Err(reached) => too_large(reached),
    }
}

/// The keys a full `screen` reply closes with, read in its frame's hold; neither is in the published frame.
#[derive(Clone, Copy)]
struct ScreenTail {
    etag: u64,
    open: bool,
}

fn close_with_etag(mut out: String, tail: ScreenTail) -> String {
    debug_assert!(out.ends_with('}'), "the styled frame is one JSON object");
    out.pop();
    let _ = std::fmt::Write::write_fmt(&mut out, format_args!(",\"etag\":\"{:016x}\"", tail.etag));
    if tail.open {
        out.push_str(",\"frame\":\"open\"");
    }
    out.push_str("}\n");
    out
}

const fn frame_line_len(reply: &str) -> usize {
    reply.len().saturating_sub("OK 1\n".len() + 1)
}

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

#[cfg(test)]
fn screen_etag(t: &Terminal) -> u64 {
    FrameKey::read(t).etag()
}

#[cfg(test)]
#[path = "control_screen_etag_tests.rs"]
mod screen_etag_tests;

/// Serialize `dims --json`: every field of [`serialize_dims`] but `layer_gravity`, `layer_scale` and `layer_flipped`.
pub(crate) fn serialize_dims_json(snapshot: &DimsSnapshot) -> String {
    let window = snapshot
        .window
        .map_or_else(|| "null".to_string(), |window| window.to_string());
    let present_retry_in_ms = snapshot
        .present_retry_in_ms
        .map_or_else(|| "null".to_string(), |delay| delay.to_string());
    let presented_seq = snapshot
        .presented_seq
        .map_or_else(|| "null".to_string(), |seq| seq.to_string());
    let session = snapshot
        .session
        .map_or_else(|| "null".to_string(), |session| session.to_string());
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
         \"present_retry_remaining\":{},\"present_retry_in_ms\":{},\"presented_seq\":{}}}",
        snapshot.rows,
        snapshot.cols,
        snapshot.pixel_w,
        snapshot.pixel_h,
        session,
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
        presented_seq,
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

/// `colors` -> the terminal's live default colours, then what programs set over the theme.
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
#[path = "control_query_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "control_query_trim_tests.rs"]
mod trim_tests;

#[cfg(test)]
#[path = "control_query_offscreen_tests.rs"]
mod offscreen_tests;

#[cfg(test)]
#[path = "control_query_story_tests.rs"]
mod story_tests;

#[cfg(test)]
#[path = "control_query_permissions_verb_tests.rs"]
mod permissions_verb_tests;

#[cfg(test)]
#[path = "control_query_devtools_verb_tests.rs"]
mod devtools_verb_tests;

#[cfg(test)]
#[path = "control_query_engine_reader_tests.rs"]
mod engine_reader_tests;

#[cfg(test)]
#[path = "control_query_styled_frame_fusion_parity_tests.rs"]
mod styled_frame_fusion_parity;

#[cfg(test)]
#[path = "control_query_screen_conditional_tests.rs"]
mod screen_conditional;
