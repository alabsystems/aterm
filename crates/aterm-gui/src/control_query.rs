// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only screen introspection serializers — the SACRED AI-reads-the-screen
//! path. These verbs read the live [`Terminal`] grid/renderer and serialize it
//! to the control protocol's text/JSON replies; they NEVER mutate state. Moved
//! verbatim from `control.rs` (behavior-preserving); the shared JSON/encode
//! helpers (`json_*`, `pct_encode`, `visible_char`, `cursor_style_name`) stay in
//! `control.rs` and are imported via `super::`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use aterm_core::grid::extra::ImageData;
use aterm_core::grid::{CellFlags, Grid};
use aterm_core::search::{
    DEFAULT_MAX_CACHED_LINES, NarrowedSearch, SearchDirection as EngineSearchDirection,
    SearchMatch, SearchOptionsError, SearchResults, TerminalSearch, max_cached_for_retained,
};
use aterm_core::selection::SelectionType;
use aterm_core::terminal::{RenderCell, Terminal, UnderlineStyle};
use aterm_search::MAX_SEARCH_MATCHES;
use winit::event_loop::EventLoopProxy;

use super::{
    DimsSnapshot, control_media, cursor_style_name, image_payload, json_ok, json_str_field,
    pct_encode, visible_char,
};
use crate::{Wake, term_lock};

/// The visible, trailing-trimmed text of screen row `r`: the engine's
/// combining-aware `get_line_text` with interior control chars collapsed to
/// spaces and the tail trimmed. THE single source for a screen row's text —
/// `text`, `text --json`, and the pushed `subscribe screen` DELTA all route here
/// so the polled and pushed faces stay byte-identical. Caller holds the term lock.
pub(crate) fn visible_row(t: &Terminal, r: usize) -> String {
    let line = t.get_line_text(r as i32, None).unwrap_or_default();
    let mut out: String = line.chars().map(visible_char).collect();
    // Truncate in place instead of `trim_end().to_string()`: `trim_end` returns
    // a PREFIX slice, so its length is always a char boundary and the bytes that
    // survive are identical — but the old form allocated a second full row and
    // memcpy'd into it. This runs once per screen row per `subscribe screen`
    // push WITH THE TERMINAL LOCK HELD, so the copy stalled the PTY reader.
    // (Two statements: `out.truncate(out.trim_end().len())` cannot borrow-check.)
    let end = out.trim_end().len();
    out.truncate(end);
    out
}

/// How many leading rows a reply keeps once its trailing all-blank rows are dropped:
/// the index of the last row that is not blank (blank = empty after trimming
/// spaces) plus one, and 0 when every row is blank. Interior blank rows are KEPT —
/// only the tail goes — so row `i` of a trimmed reply is still screen row `i`, and
/// the `line`/`cell`/`search` coordinates an agent reads off it stay valid. THE one
/// trim rule: `text trim`, `text --json trim`, `turn … trim=1`, `blocktext … trim`,
/// `temporal … trim` and the `subscribe … screen,trim` DELTA all route here, so the
/// polled and pushed faces cannot disagree about what a trailing blank is. (F4: a
/// 55-row grid with 48 blank rows cost ~7.6 KB per read and a `grep -v` on every
/// reply; the count on a trimmed header is the count of rows actually sent.)
pub(crate) fn trimmed_len<'a>(rows: impl Iterator<Item = &'a str>) -> usize {
    rows.enumerate()
        .filter(|(_, r)| !r.trim_matches(' ').is_empty())
        .map(|(i, _)| i + 1)
        .last()
        .unwrap_or(0)
}

/// The `text` / `text --json` argument tail: empty for the whole grid, `trim` to
/// drop the trailing blank rows. ANYTHING else is `ERR usage: text [trim]` — the
/// dispatch used to drop this tail on the floor, so an agent guessing a modifier
/// (`text trim`, `text compact`) got the full grid back with no signal that it had
/// guessed wrong (F4's sub-finding). The usage line is the `Err` so both arms answer
/// with the same bytes.
pub(crate) fn text_trim_arg(rest: &str) -> Result<bool, String> {
    match rest.trim() {
        "" => Ok(false),
        "trim" => Ok(true),
        _ => Err("ERR usage: text [trim]\n".to_string()),
    }
}

/// Split a trailing `trim` token off a positional grammar's argument tail:
/// `(<tail without it>, true)` when the LAST whitespace-separated token is `trim`,
/// else `(<tail>, false)`. Shared by `blocktext <id> [trim]` and `temporal
/// [status|<tick>] [trim]`, whose own argument comes first — each verb still
/// validates what is left, so `trim` alone or a stray token stays a usage error.
pub(crate) fn split_trim_tail(rest: &str) -> (&str, bool) {
    let rest = rest.trim();
    match rest.strip_suffix("trim") {
        Some(head) if head.is_empty() || head.ends_with(char::is_whitespace) => {
            (head.trim_end(), true)
        }
        _ => (rest, false),
    }
}

/// `blocktext <id> [trim]`: the grammar is validated HERE, and only `<id>` reaches
/// the shared block-output emitter (`emit`). `trim` drops the block's trailing blank
/// rows through [`trim_lines_reply`]; a non-numeric id or any other tail is the
/// usage line rather than a token that vanishes (the `text` hole, on a second verb).
pub(crate) fn cmd_blocktext_args(rest: &str, emit: impl FnOnce(&str) -> String) -> String {
    let (id, trim) = split_trim_tail(rest);
    if id.parse::<u64>().is_err() {
        return "ERR usage: blocktext <id> [trim]\n".to_string();
    }
    let reply = emit(id);
    if trim {
        trim_lines_reply(&reply)
    } else {
        reply
    }
}

/// Re-frame a line-framed `OK <n>[ <marker…>]\n` + n rows reply with its trailing
/// blank rows dropped: `OK <sent>[ <marker…>] trimmed=<k>\n` + the first `sent` rows,
/// `sent` per [`trimmed_len`]. Header tokens after the count survive in place and
/// `trimmed=` closes the line (reply fields are additive, new ones go LAST). Any
/// other reply — an `ERR`, a status line — passes through untouched: there is
/// nothing to trim and the error must reach the caller as it was.
pub(crate) fn trim_lines_reply(reply: &str) -> String {
    let Some((header, body)) = reply.split_once('\n') else {
        return reply.to_string();
    };
    let mut toks = header.split_whitespace();
    if toks.next() != Some("OK") {
        return reply.to_string();
    }
    let Some(n) = toks.next().and_then(|t| t.parse::<usize>().ok()) else {
        return reply.to_string();
    };
    let sent = trimmed_len(body.lines().take(n));
    let mut out = format!("OK {sent}");
    for tok in toks {
        out.push(' ');
        out.push_str(tok);
    }
    {
        use std::fmt::Write as _;
        let _ = writeln!(out, " trimmed={}", n.saturating_sub(sent));
    }
    for row in body.split_inclusive('\n').take(sent) {
        out.push_str(row);
    }
    out
}

/// Frame a gathered screen `body` (`rows` newline-terminated rows, already out from
/// under the terminal lock) as a line-framed reply: `OK <n>[ <verdict>]\n` + all
/// `rows` rows, or with `trim`, `OK <sent>[ <verdict>] trimmed=<k>\n` + the first
/// `sent` rows ([`trimmed_len`]). `verdict` is `turn`'s `turn submitted=… hash=…`
/// run (empty for `text`); `trimmed=` always closes the header, so a client that
/// keys on the `turn` token at position two, or reads the count at position one,
/// sees exactly what it did before. Off by default: an untrimmed reply is
/// byte-identical to the pre-`trim` wire, because scripts count rows.
pub(crate) fn frame_rows_reply(body: &str, rows: usize, verdict: &str, trim: bool) -> String {
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

/// `text` -> `OK <nrows>\n` then each visible row (trailing spaces trimmed). The
/// bare form of [`cmd_text_opt`], kept for the callers that read the whole grid.
///
/// FIDELITY (I-1): each row is extracted through the engine's combining-aware
/// `get_line_text` — the SAME path `selection_to_string`/`copy` and the
/// renderer's `combining_row`/`cluster_row` use — so an NFD accent
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
    cmd_text_opt(term, false)
}

/// `text [trim]` -> `OK <n>[ trimmed=<k>]\n` then `<n>` visible rows. Bare, `n` is
/// the grid's row count. With `trim`, the rows after the last non-blank one are
/// dropped ([`trimmed_len`]), `n` is the count ACTUALLY SENT and `trimmed=<k>` says
/// how many went — the header stays honest for a client that frames the body by its
/// count, and every reader in the workspace takes only the first token (`aterm-ctl`'s
/// `stream_count`, `aterm-agent`'s `read_text`, nest's reader), so the marker is
/// additive. The rows themselves are the same [`visible_row`] text as ever.
pub(crate) fn cmd_text_opt(term: &Arc<Mutex<Terminal>>, trim: bool) -> String {
    // The body is gathered into ONE buffer sized up front (one full row + newline
    // each) so the row loop never reallocates-and-copies the accumulated screen
    // while holding the terminal lock. The header is written AFTER the lock is
    // released — a trimmed count is only known once every row has been read — and
    // the one memcpy that costs is paid with the PTY reader unblocked.
    let (rows, body) = {
        let t = term_lock(term);
        let rows = t.rows() as usize;
        let mut body = String::with_capacity(rows * (t.cols() as usize + 1));
        for r in 0..rows {
            body.push_str(&visible_row(&t, r));
            body.push('\n');
        }
        (rows, body)
    };
    frame_rows_reply(&body, rows, "", trim)
}

/// `cursor` -> `OK <row> <col> <visible:0|1> <style>\n` (0-based). `<style>`
/// is the terminal's DECSCUSR cursor style as a lowercase name:
/// `blinking_block` (default), `steady_block`, `blinking_underline`,
/// `steady_underline`, `blinking_bar`, `steady_bar`, `hidden`, `hollow_block`.
pub(crate) fn cmd_cursor(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let c = t.cursor();
    let vis = u8::from(t.cursor_visible());
    let style = cursor_style_name(t.cursor_style());
    format!("OK {} {} {} {}\n", c.row, c.col, vis, style)
}

/// `cell <r> <c>` -> `OK <grapheme> <fg> <bg> <attrs>\n` or `ERR <msg>\n`.
///
/// `<grapheme>` is the cell's FULL on-screen grapheme — the resolved base char
/// plus any complex-cluster string and combining marks — percent-encoded into a
/// single space-free token (decode it the same way as `cwd`/`cmdline`). It is
/// the SAME text the `text`/`search`/selection paths and the renderer's
/// `combining_row`/`cluster_row` produce, so a single-cell read of `é`
/// (`e`+U+0301) or a ZWJ family (👨‍👩‍👧) is FAITHFUL — not the base codepoint
/// alone (FIDELITY I-1; this REPLACES the previous `char as u32` codepoint
/// field, which silently dropped combining marks / emoji clusters). A blank or
/// wide-continuation cell yields an empty token (`%20`-free → ``). `<fg>`/`<bg>`
/// are the fully-resolved `RRGGBB` colors the renderer would paint; `<attrs>` is
/// a comma-separated list (or `none`) of the cell's active text attributes —
/// `bold,dim,italic,underline,blink,inverse,strike,hidden`.
pub(crate) fn cmd_cell(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let mut it = rest.split_whitespace();
    let (Some(rs), Some(cs)) = (it.next(), it.next()) else {
        return "ERR usage: cell <r> <c>\n".to_string();
    };
    let (Ok(r), Ok(c)) = (rs.parse::<usize>(), cs.parse::<usize>()) else {
        return "ERR bad args\n".to_string();
    };
    let t = term_lock(term);
    // Bound by the GRID (per `dims`), not by row content: `render_row` trims
    // trailing blanks, but every 0<=r<rows, 0<=c<cols is a real, readable cell.
    if r >= t.rows() as usize || c >= t.cols() as usize {
        return "ERR out of range\n".to_string();
    }
    // LIVE frame (offset-INDEPENDENT), matching the offset-independent `text`/
    // `cell_grapheme` reads — never a scrolled-back row's colours on a live glyph.
    let row = t.render_row_at_screen(r);
    let (fg, bg) = match row.get(c) {
        Some(cell) => (cell.fg, cell.bg),
        // A blank in-grid cell is the engine's implicit `Cell::EMPTY`.
        // Resolve it through the same live palette/default/reverse-video path
        // as a materialized cell; raw defaults alone are wrong under DECSCNM.
        None => {
            let blank = t.implicit_blank_render_cell();
            (blank.fg, blank.bg)
        }
    };
    // Combining-aware grapheme for THIS cell, via the same core extraction the
    // selection/text paths use. A wide-continuation cell yields "" (its glyph
    // belongs to the lead cell); a blank cell yields "" (the consumer infers a
    // space from the in-grid position, matching `text`'s trailing trim).
    let grapheme = t.cell_grapheme(r, c).unwrap_or_default();
    let grapheme_tok = pct_encode(&grapheme);
    // Width markers, so a consumer can distinguish a full-width (CJK) glyph from
    // an ASCII space without inferring from columns:
    //   `wide`      — the LEAD cell, which holds the double-width glyph
    //   `wide_cont` — its blank right-half spacer
    // PROTECTED (DECSCA) shares a flag bit with WIDE_CONTINUATION;
    // `is_wide_continuation_at` disambiguates via the left neighbor, so a
    // protected character gets NEITHER token (it is ordinary text).
    let flags = cell_attrs(t.grid(), r, c);
    let mut attrs = attrs_string(flags);
    let wide_tok = if flags.contains(CellFlags::WIDE) {
        Some("wide")
    } else if t.grid().is_wide_continuation_at_screen(r as u16, c as u16) {
        Some("wide_cont")
    } else {
        None
    };
    if let Some(tok) = wide_tok {
        if attrs == "none" {
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
    let link = t
        .hyperlink_at(r as u16, c as u16)
        .map(|u| format!(" link={}", pct_encode(u)))
        .unwrap_or_default();
    format!(
        "OK {grapheme_tok} {:02x}{:02x}{:02x} {:02x}{:02x}{:02x} {attrs}{link}\n",
        fg[0], fg[1], fg[2], bg[0], bg[1], bg[2],
    )
}

/// Resolve the effective [`CellFlags`] at grid `(r, c)`.
///
/// Inline-styled cells carry their attribute bits directly; cells that intern
/// their style in the grid's `StyleTable` keep only `USES_STYLE_ID` (plus any
/// extra flags) inline, so the real attributes are rehydrated from the table —
/// the same path [`Terminal::render_row`] uses for colors. Out-of-range
/// coordinates yield empty flags.
fn cell_attrs(grid: &Grid, r: usize, c: usize) -> CellFlags {
    let (Ok(row), Ok(col)) = (u16::try_from(r), u16::try_from(c)) else {
        return CellFlags::default();
    };
    // LIVE frame (`row_at_screen`, offset-INDEPENDENT), NOT the display-mapped
    // `grid.row` — so the attrs a socket read reports pair with the live glyph/colours
    // regardless of the GUI's scroll position (see `render_row_at_screen`).
    let Some(cell) = grid.row_at_screen(row).and_then(|gr| gr.get(col)) else {
        return CellFlags::default();
    };
    // DEAD-BRANCH PROBE: no production writer sets `USES_STYLE_ID` and the SGR
    // path no longer interns a `StyleId`, so the table rehydration below cannot
    // be reached from a live grid. Kept for encoding completeness; asserted so
    // the socket-introspection tests prove it rather than assume it.
    debug_assert!(
        !cell.uses_style_id(),
        "cell_attrs: a live cell carries USES_STYLE_ID, but nothing interns styles"
    );
    if cell.uses_style_id() {
        let extra = cell.flags().difference(CellFlags::USES_STYLE_ID);
        grid.resolve_style_to_colors(cell.style_id(), extra).2
    } else {
        cell.flags()
    }
}

/// Render active text attributes as a stable comma list, or `none` when bare.
///
/// `underline` is reported for any underline style (single/double/curly and the
/// dotted/dashed combinations, which all set one of those bits).
fn attrs_string(flags: CellFlags) -> String {
    let any_underline = CellFlags::UNDERLINE
        .union(CellFlags::DOUBLE_UNDERLINE)
        .union(CellFlags::CURLY_UNDERLINE);
    let mut parts: Vec<&str> = Vec::new();
    if flags.contains(CellFlags::BOLD) {
        parts.push("bold");
    }
    if flags.contains(CellFlags::DIM) {
        parts.push("dim");
    }
    if flags.contains(CellFlags::ITALIC) {
        parts.push("italic");
    }
    if flags.intersects(any_underline) {
        parts.push("underline");
    }
    if flags.contains(CellFlags::BLINK) {
        parts.push("blink");
    }
    if flags.contains(CellFlags::INVERSE) {
        parts.push("inverse");
    }
    if flags.contains(CellFlags::STRIKETHROUGH) {
        parts.push("strike");
    }
    if flags.contains(CellFlags::HIDDEN) {
        parts.push("hidden");
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(",")
    }
}

/// Gather one coherent `dims` snapshot on the main thread. The terminal handle,
/// rather than an already-stale `(rows, cols)` pair, crosses the event-loop hop;
/// the handler samples the grid and main-thread-owned window geometry in the
/// same event turn. It uses `try_lock`, so introspection never wedges rendering
/// behind the very terminal mutex it may be diagnosing.
fn read_dims(
    term: &Arc<Mutex<Terminal>>,
    session: u64,
    proxy: &EventLoopProxy<Wake>,
) -> Result<DimsSnapshot, &'static str> {
    control_media::call_main(proxy, |reply| Wake::ReadDims {
        session,
        term: Arc::clone(term),
        reply,
    })?
}

/// Read the grid dimensions for lock-free process metrics without ever waiting
/// on the terminal mutex. `None` means the terminal was actively locked; callers
/// expose that as `busy`/JSON `null` while still returning every scheduler and
/// renderer counter. A poisoned mutex is recoverable, matching [`term_lock`].
fn try_metrics_dims(term: Option<&Arc<Mutex<Terminal>>>) -> Option<(u32, u32)> {
    let Some(term) = term else {
        return Some((0, 0));
    };
    match term.try_lock() {
        Ok(terminal) => Some((u32::from(terminal.rows()), u32::from(terminal.cols()))),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => {
            let terminal = poisoned.into_inner();
            Some((u32::from(terminal.rows()), u32::from(terminal.cols())))
        }
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// Serialize a live `dims` snapshot. The four historical numeric fields remain
/// first and retain their grid-pixel meaning, so existing positional readers can
/// keep using them. Keyed facts expose the geometry that mattered during the
/// font-zoom artifact incident: exact cell/frame/surface sizes, centered bands,
/// and transient crop.
pub(crate) fn serialize_dims(snapshot: &DimsSnapshot) -> String {
    let window = snapshot
        .window
        .map_or_else(|| "none".to_string(), |window| window.to_string());
    let present_retry_in_ms = snapshot
        .present_retry_in_ms
        .map_or_else(|| "none".to_string(), |delay| delay.to_string());
    // The swapchain layer's live presentation state. `none` where the platform has
    // no such layer at all (off macOS, CPU backend, no window) — deliberately the
    // same spelling as every other absent keyed fact here, so a driver reads it
    // without a special case.
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
         tab_rows={} viewers={} visible_viewers={} geometry={} \
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

/// `metrics [reset]` -> one `OK k=v ...\n` line of live render/latency counters so a
/// driving AI can MEASURE responsiveness AND DETECT lag in the same loop it drives
/// with — `send`/`key`, then `metrics`. `metrics reset` first zeroes the
/// measurement-window stats (frames / maxima / slow count) so a SPECIFIC workload can
/// be timed: `metrics reset`, drive it, then `metrics`.
///
/// Fields: `backend=<cpu|gpu>`, grid `rows`/`cols` (`busy` in text / `null` in
/// JSON when the terminal mutex is occupied, without withholding the remaining
/// lock-free diagnostics), `frames` (successful application presents since reset
/// — a steady app-render frame does NOT advance it),
/// `last_/max_present_latency_ms` (the `output→application-present-return`
/// slice `$ATERM_TRACE_LATENCY` logs, most-recent + worst), and the
/// LAG SIGNATURE: `last_/max_frame_render_ms` + `slow_frames` (frames over the ~30 fps
/// budget, `slow_threshold_ms`). A non-zero `slow_frames`, a large
/// `max_frame_render_ms`, or `backend=cpu` under heavy output all mean the terminal is
/// lagging. Values are the process-global [`crate::metrics`] counters + the grid size.
///
/// PACING/SHED SIGNATURE (the 2026-07-05 incident class):
/// `last_/max_input_present_ms` (key arrival → the first attributed successful
/// content-present return — a software-side typing-latency proxy),
/// `sync_armed`/`sync_rel_end`/`sync_rel_timeout` (DEC-2026 hold episodes by release
/// cause — timeouts climbing during ordinary typing = the SYNC-1 bug), `sync_holding`,
/// and `perf_reduced` + `shed_transitions` (the load-shed latch; engaged during light
/// typing, or flapping at idle, are both wrong).
///
/// THE TAIL, ON THE SUMMARY LINE. `last_` is a momentary reading and `max_` is
/// an unbounded worst case; neither brackets a distribution. During the 2026-08
/// spin `last_input_present_ms=8.1` read healthy while the live p99 was 335 ms.
/// So the summary now also carries `n_input` + `input_p50/p95/p99_ms` and
/// `n_present` + `present_p50/p95/p99_ms` — the same histograms `percentiles`
/// publishes, so a healthy median cannot mask a sick tail from a reader who
/// asked only one question.
///
/// PRESENT-LATENCY HONESTY. `present_*` describes aterm ONLY while the window is
/// actually presenting, so an output→present sample taken while the window was
/// occluded/parked, or while a `video` capture was pacing presents, is booked to
/// a separate ledger: `present_tainted` / `last_/max_present_tainted_ms`, with
/// its own `present_tainted_p50/p95/p99_ms` under `percentiles`. `capture_episodes`
/// counts recordings opened in this window and `capture_active` is the live
/// gauge — any non-zero `capture_episodes` means a capture-based instrument ran
/// inside your measurement window. Nothing is discarded: `n_present +
/// n_present_tainted` still accounts for every content present.
///
/// WHAT THESE SLICES INCLUDE — READ BEFORE QUOTING ONE AS "LATENCY". Both
/// key→present slices are OPEN INTERVALS closed by the next qualifying present,
/// so any time in which nothing presented is INSIDE the number. They are not
/// floored by a sampling rate (that is the `video … keys` failure mode; see
/// `crate::video_key_analysis`), but they have their own two:
///
///  * `present_*` (`output→application-present-return`) opens on the LEADING
///    EDGE of a PTY output burst — a `compare_exchange(0, …)`, first edge wins —
///    and closes on the next content present that books it. Output that moves
///    no pixels opens the interval and does not close it, so the wait until
///    something else presents is published as if it were render time. The only
///    bound is a 5 s discard (`PRESENT_LATENCY_CAP_NS`), so a multi-SECOND
///    reading means "nothing presented for that long", NOT "a frame took that
///    long". Read it with `n_present` and the percentiles beside it, never as a
///    lone `last_`/`max_`.
///  * `input_*` (`key-arrival→content-present-return`) has the mirror problem in
///    the other direction, already stated at `INPUT_STAMP_NS`: a keystroke that
///    produces no output is discarded after 5 s rather than booked, and under
///    CONCURRENT streaming output the closing present may be a log-line frame
///    rather than the key's own echo — so it reads LOW, never high. It is a
///    starvation detector, not an echo-attribution profiler.
///
///  * `resize_present` has the same shape with a 2 s bound
///    (`RESIZE_SLICE_CAP_NS`) and closes on ANY successful present, not only a
///    content one — deliberately, since what ends a compositor rescale is a
///    frame at the new size, whatever drew it. Same caveat: idle inside the
///    interval is inside the number.
///
/// None of them is key→photon: all stop at application-present return, upstream
/// of compositor selection, scanout and display.
///
/// FRAME-EXTRACTION ATTRIBUTION. `frame_refills_scoped` / `frame_refills_full`
/// split the presented-path refills between the damage-scoped arm and the full
/// O(rows x cols) walk, and `frame_refill_full_causes=<clause>:<frames>,...` (a
/// JSON object in the structured twin) names WHICH continuity clause refused on
/// each of the full ones. The split alone cannot: `host_mutation` (some
/// per-frame scratch mutator broke the chain — fixable),
/// `terminal_mismatch` (panes sharing one scratch — fixable), `base_y` (the
/// content is scrolling — correct, nothing to fix) and `scratch_unstamped` (the
/// window's first frame — unavoidable) all read as the same climbing
/// `frame_refills_full`. Non-zero clauses only, so a healthy instance prints
/// one or two pairs. `frame_refills_skipped` is the third arm: presented frames
/// that took NO extraction because the effect-only reuse gate proved the engine
/// had not moved since the snapshot was filled. It is what keeps a FALL in
/// `frame_refills_scoped` readable as work avoided rather than work gone
/// missing — `scoped + full + skipped` is still exactly one per presented
/// non-rescan frame.
///
/// SCHEDULER ATTRIBUTION. `past_deadline_arms` is the global spin witness;
/// `deadline_arms_by_owner=<owner>:<arms>/<past>,...` (a JSON object in the
/// structured twin) names WHICH producer armed them. `deadline_owner` alone is a
/// last-writer snapshot and cannot do that — it reported the 2026-08 spin as
/// `title_summary` when the producer was the session-status observer, now its
/// own `session_status` owner. Beside it, `past_arm_streak_heals=<owner>:<n>,...`
/// names every producer whose windowed past-arm streak (> 90% of its last 32
/// arms already past, at ANY lateness) the fold is actively clamping to frame
/// cadence — a live, named scheduler bug, not a suspicion.
///
/// STARTUP: `first_present_ms` keeps the compatibility-stable GUI
/// `main_entry` → startup-metrics publication point inside the first
/// successful-present finalizer. That point follows a successful submit and
/// includes initial reveal, acknowledgement, and synchronous post-submit
/// recovery bookkeeping.
/// `rust_main_to_first_present_ms` adds the shipped one-binary Rust `main`
/// boundary before router work (0.00 in the thin GUI binary). Schema 1's eight
/// `startup_*_ms` fields exclusively partition that broader interval: router,
/// synchronous GUI preparation, winit dispatch, initial surface attachment,
/// wait/retries to the eventually successful redraw, that redraw's compose,
/// its surface transaction, and successful-present finalization.
/// `startup_phase_valid` is true only when every timestamp is present, ordered,
/// and sums exactly to both enclosing startup metrics. All startup facts remain
/// 0.00 until first present, survive `metrics reset`, and exclude
/// dyld/process-loader time. None observes compositor selection, display timing,
/// scanout, or photons.
///
/// `metrics percentiles` -> the latency DISTRIBUTIONS:
/// input→application-present-return, output→application-present-return,
/// frame-render, key→write, and pre-present compose histograms
/// as `n_*` + p50/p95/p99 ms fields, conservative by construction (bucket upper
/// edge), plus the occluded/parked/capture twin `present_tainted_*`. Same funnel
/// and honesty bounds as the scalars; zeroed by `metrics reset` like the maxima.
pub(crate) fn cmd_metrics(term: Option<&Arc<Mutex<Terminal>>>, rest: &str) -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    let wants_json = rest
        .split_whitespace()
        .any(|token| token == "json" || token == "--json");
    let command = rest
        .split_whitespace()
        .filter(|token| *token != "json" && *token != "--json")
        .collect::<Vec<_>>()
        .join(" ");
    if wants_json {
        return cmd_metrics_json(term, &command);
    }
    // `metrics percentiles` -> the latency DISTRIBUTIONS (same funnel as the
    // scalars, log-linear histograms, bucket-upper-edge = conservative):
    // per distribution `n_*` samples + p50/p95/p99 in ms (0.00 until a sample
    // lands — check `n`). Window stats: `metrics reset` zeroes them too.
    if rest.trim() == "percentiles" {
        let (input, present, render) = crate::metrics::distributions();
        let key_write = crate::metrics::key_write_distribution();
        let pre_present = crate::metrics::pre_present_distribution();
        // The drawable-park slice. Until this existed the largest known macOS typing
        // stall could only be estimated as `redraw_total - compose - render`, mixed
        // with the post-present tail — so a regression in it moved no published number.
        let acquire = crate::metrics::acquire_wait_distribution();
        // …and the two scalars the distribution cannot express. A p99 over
        // thousands of ~0.02 ms acquires absorbs a single 200 ms park; the max
        // is the only field that reports it. Published HERE beside the
        // percentiles it qualifies (and on the summary line, which is the read
        // most drivers actually take).
        let (last_acquire_ns, max_acquire_ns) = crate::metrics::acquire_wait_last_max_ns();
        // The live-drag STALE-FRAME window: bounds change → first frame submitted
        // at the new size. For that interval the compositor has new bounds and an
        // old drawable, so it shows the previous frame rescaled — the smear a drag
        // reads as "shredded" text. The `video` tap cannot measure it (its ring is
        // one geometry and it early-stops on the first resize), so this is the only
        // millisecond-resolution read of it.
        let resize = crate::metrics::resize_present_distribution();
        // Its twin: bounds change -> the engine committing the new GRID. The text
        // trails the window edge for exactly this long, and a width drag keeps it
        // deliberately long (the throttle bounding scrollback rewrap).
        let reflow = crate::metrics::resize_reflow_distribution();
        // The occluded/parked/capture twin of `present`. It is published rather
        // than dropped so an occluded or RECORDED run stays observable — and so
        // `n_present + n_present_tainted` still accounts for every content
        // present, which is what makes the split auditable instead of a silent
        // filter. See `metrics::tainted_present_distribution`.
        let tainted = crate::metrics::tainted_present_distribution();
        let p = |h: &crate::metrics::Histogram, q: f64| ms(h.percentile(q).unwrap_or(0));
        return format!(
            "OK n_input={} input_p50_ms={:.2} input_p95_ms={:.2} input_p99_ms={:.2} \
             n_present={} present_p50_ms={:.2} present_p95_ms={:.2} present_p99_ms={:.2} \
             n_render={} render_p50_ms={:.2} render_p95_ms={:.2} render_p99_ms={:.2} \
             n_key_write={} key_write_p50_ms={:.2} key_write_p95_ms={:.2} \
             key_write_p99_ms={:.2} n_pre_present={} pre_present_p50_ms={:.2} \
             pre_present_p95_ms={:.2} pre_present_p99_ms={:.2} \
             n_acquire={} acquire_p50_ms={:.2} acquire_p95_ms={:.2} acquire_p99_ms={:.2} \
             last_acquire_wait_ms={:.2} max_acquire_wait_ms={:.2} \
             n_resize={} resize_p50_ms={:.2} resize_p95_ms={:.2} resize_p99_ms={:.2} \
             n_reflow={} reflow_p50_ms={:.2} reflow_p95_ms={:.2} reflow_p99_ms={:.2} \
             n_present_tainted={} present_tainted_p50_ms={:.2} \
             present_tainted_p95_ms={:.2} present_tainted_p99_ms={:.2}{}\n",
            input.count(),
            p(input, 0.50),
            p(input, 0.95),
            p(input, 0.99),
            present.count(),
            p(present, 0.50),
            p(present, 0.95),
            p(present, 0.99),
            render.count(),
            p(render, 0.50),
            p(render, 0.95),
            p(render, 0.99),
            key_write.count(),
            p(key_write, 0.50),
            p(key_write, 0.95),
            p(key_write, 0.99),
            pre_present.count(),
            p(pre_present, 0.50),
            p(pre_present, 0.95),
            p(pre_present, 0.99),
            acquire.count(),
            p(acquire, 0.50),
            p(acquire, 0.95),
            p(acquire, 0.99),
            ms(last_acquire_ns),
            ms(max_acquire_ns),
            resize.count(),
            p(resize, 0.50),
            p(resize, 0.95),
            p(resize, 0.99),
            reflow.count(),
            p(reflow, 0.50),
            p(reflow, 0.95),
            p(reflow, 0.99),
            tainted.count(),
            p(tainted, 0.50),
            p(tainted, 0.95),
            p(tainted, 0.99),
            // ECHO ROUND TRIP (audit item 5): the one slice on this line that is
            // NOT aterm's own cost — bytes out to the PTY, first bytes back from
            // the child. Formatted by `echo_rtt` itself so its percentiles can
            // never be published apart from the counters that qualify them.
            crate::echo_rtt::percentile_fields_text(),
        );
    }
    if rest.trim() == "reset" {
        crate::metrics::reset();
        crate::echo_rtt::reset();
    }
    let (rows, cols) = try_metrics_dims(term).map_or_else(
        || ("busy".to_string(), "busy".to_string()),
        |(rows, cols)| (rows.to_string(), cols.to_string()),
    );
    let m = crate::metrics::snapshot();
    // TAIL SURFACING (audit item 10). The summary used to publish only `last_`
    // and `max_`, and during the 2026-08 spin `last_input_present_ms=8.1` read
    // perfectly healthy while the live p99 was 335 ms — a momentary reading and
    // an unbounded worst case cannot bracket a distribution between them. The
    // p95/p99 the `percentiles` verb already computed now ride the SUMMARY, so
    // a healthy median can never again mask a sick tail from a reader who did
    // not think to ask a second question.
    let (h_input, h_present, _) = crate::metrics::distributions();
    let pct = |h: &crate::metrics::Histogram, q: f64| ms(h.percentile(q).unwrap_or(0));
    let arms = crate::metrics::deadline_arm_attribution();
    let refill_causes = crate::metrics::frame_refill_full_causes();
    let streak_heals = crate::metrics::past_arm_streak_heal_attribution();
    let backend = if m.backend_gpu { "gpu" } else { "cpu" };
    format!(
        "OK backend={backend} rows={rows} cols={cols} frames={} \
         last_present_latency_ms={:.2} max_present_latency_ms={:.2} \
         last_frame_render_ms={:.2} max_frame_render_ms={:.2} \
         slow_frames={} slow_threshold_ms={:.1} \
         last_input_present_ms={:.2} max_input_present_ms={:.2} \
         n_input={} input_p50_ms={:.2} input_p95_ms={:.2} input_p99_ms={:.2} \
         n_present={} present_p50_ms={:.2} present_p95_ms={:.2} present_p99_ms={:.2} \
         last_key_write_ms={:.2} max_key_write_ms={:.2} \
         last_resize_present_ms={:.2} max_resize_present_ms={:.2} \
         last_resize_reflow_ms={:.2} max_resize_reflow_ms={:.2} \
         sync_armed={} sync_rel_end={} sync_rel_timeout={} sync_holding={} \
         perf_reduced={} shed_transitions={} wake_heals={} \
         last_redraw_total_ms={:.2} max_redraw_total_ms={:.2} \
         redraw_attempts={} redraw_early_outs={} redraw_sync_holds={} redraw_retry_gated={} \
         frame_refills_scoped={} frame_refills_full={} frame_refills_skipped={} \
         frame_refill_full_causes={} \
         pre_present_attempts={} last_pre_present_ms={:.2} pre_present_total_ms={:.2} \
         max_pre_present_ms={:.2} \
         last_acquire_wait_ms={:.2} max_acquire_wait_ms={:.2} \
         present_drops={} last_present_drop_reason={} last_present_drop_parked={} \
         present_tainted={} last_present_tainted_ms={:.2} max_present_tainted_ms={:.2} \
         capture_episodes={} capture_active={} \
         event_wakes={} timer_wakes={} wait_cancelled_wakes={} poll_wakes={} \
         wake_kind={} wake_owner={} wake_late_ms={:.2} deadline_owner={} \
         deadline_in_ms={:.2} deadline_late_ms={:.2} past_deadline_arms={} \
         deadline_arms_by_owner={} \
         past_arm_streak_heals={} \
         stale_arm_heals={} \
         max_frame_gap_ms={:.2} \
         rust_main_to_first_present_ms={:.2} \
         rust_main_to_first_visible_ms={:.2} \
         startup_phase_schema={} startup_phase_valid={} \
         startup_router_ms={:.2} startup_gui_prepare_ms={:.2} \
         startup_winit_dispatch_ms={:.2} startup_initial_surface_attach_ms={:.2} \
         startup_surface_to_successful_redraw_ms={:.2} \
         startup_successful_compose_ms={:.2} \
         startup_successful_surface_transaction_ms={:.2} \
         startup_successful_finalize_ms={:.2} \
         startup_attach_schema={} startup_attach_valid={} \
         startup_attach_dispatch_ms={:.2} startup_attach_prepare_ms={:.2} \
         startup_attach_window_create_ms={:.2} startup_attach_window_setup_ms={:.2} \
         startup_attach_backend_finalize_ms={:.2} \
         startup_attach_chrome_geometry_ms={:.2} \
         startup_attach_surface_create_ms={:.2} startup_attach_finish_ms={:.2} \
         startup_worker_schema={} startup_worker_valid={} \
         startup_worker_total_ms={:.2} startup_worker_overlap_ms={:.2} \
         startup_worker_after_join_ms={:.2} startup_worker_post_join_ms={:.2} \
         startup_worker_prelude_ms={:.2} startup_worker_gpu_build_ms={:.2} \
         startup_worker_font_admit_ms={:.2} startup_worker_font_apply_ms={:.2} \
         startup_worker_font_seal_ms={:.2} startup_worker_epilogue_ms={:.2} \
         startup_gpu_schema={} startup_gpu_valid={} \
         startup_gpu_instance_ms={:.2} startup_gpu_adapter_ms={:.2} \
         startup_gpu_device_ms={:.2} startup_gpu_context_tail_ms={:.2} \
         startup_gpu_font_thread_ms={:.2} startup_gpu_font_join_ms={:.2} \
         startup_gpu_pipelines_ms={:.2} startup_gpu_pipe_shader_ms={:.2} \
         startup_gpu_pipe_uniform_atlas_ms={:.2} startup_gpu_pipe_cell_ms={:.2} \
         startup_gpu_pipe_blit_ms={:.2} startup_gpu_pipe_tray_ms={:.2} \
         startup_gpu_pipe_bloom_ms={:.2} startup_gpu_pipe_vbuf_ms={:.2} \
         startup_gpu_pipe_tail_ms={:.2} startup_gpu_tail_ms={:.2} \
         startup_gpu_cell_pipeline_ms={} \
         effect_pipeline_builds={} effect_pipeline_build_ms={:.2} \
         effect_pipelines_built={} \
         first_present_ms={:.2} first_visible_ms={:.2}\n",
        m.frames_presented,
        ms(m.last_present_latency_ns),
        ms(m.max_present_latency_ns),
        ms(m.last_frame_render_ns),
        ms(m.max_frame_render_ns),
        m.slow_frames,
        ms(crate::metrics::SLOW_FRAME_THRESHOLD_NS),
        ms(m.last_input_present_ns),
        ms(m.max_input_present_ns),
        h_input.count(),
        pct(h_input, 0.50),
        pct(h_input, 0.95),
        pct(h_input, 0.99),
        h_present.count(),
        pct(h_present, 0.50),
        pct(h_present, 0.95),
        pct(h_present, 0.99),
        ms(m.last_key_write_ns),
        ms(m.max_key_write_ns),
        ms(m.last_resize_present_ns),
        ms(m.max_resize_present_ns),
        ms(m.last_resize_reflow_ns),
        ms(m.max_resize_reflow_ns),
        m.sync_holds_armed,
        m.sync_releases_end,
        m.sync_releases_timeout,
        u8::from(m.sync_holding),
        u8::from(m.perf_reduced),
        m.shed_transitions,
        m.wake_heals,
        ms(m.last_redraw_total_ns),
        ms(m.max_redraw_total_ns),
        m.redraw_attempts,
        m.redraw_early_outs,
        m.redraw_sync_holds,
        m.redraw_retry_gated,
        m.frame_refills_scoped,
        m.frame_refills_full,
        m.frame_refills_skipped,
        refill_cause_pairs(&refill_causes),
        m.pre_present_attempts,
        ms(m.last_pre_present_ns),
        ms(m.pre_present_total_ns),
        ms(m.max_pre_present_ns),
        // THE DRAWABLE PARK, AS A SCALAR. `acquire_p99_ms` (the `percentiles`
        // verb) is a statement about the bulk of thousands of ~0.02 ms samples;
        // a single 200 ms `nextDrawable` park cannot move it. These two were
        // recorded on every present and reset with the window since the slice
        // was instrumented, and no snapshot had ever read them — so the worst
        // acquire of a run was, until now, unpublishable.
        ms(m.last_acquire_wait_ns),
        ms(m.max_acquire_wait_ns),
        m.present_drops,
        m.last_present_drop_reason.as_str(),
        u8::from(m.last_present_drop_parked),
        m.tainted_present_samples,
        ms(m.last_tainted_present_latency_ns),
        ms(m.max_tainted_present_latency_ns),
        m.capture_episodes,
        u8::from(m.capture_active),
        m.event_wakes,
        m.timer_wakes,
        m.wait_cancelled_wakes,
        m.poll_wakes,
        m.last_wake_kind.as_str(),
        m.last_wake_owner.as_str(),
        ms(m.last_wake_late_ns),
        m.last_deadline_owner.as_str(),
        ms(m.deadline_in_ns),
        ms(m.last_deadline_late_ns),
        m.past_deadline_arms,
        deadline_arm_pairs(&arms),
        streak_heal_pairs(&streak_heals),
        m.stale_arm_heals,
        ms(m.max_frame_gap_ns),
        ms(m.rust_main_to_first_present_ns),
        ms(m.rust_main_to_first_visible_ns),
        m.startup_phase_schema,
        u8::from(m.startup_phase_valid),
        ms(m.startup_router_ns),
        ms(m.startup_gui_prepare_ns),
        ms(m.startup_winit_dispatch_ns),
        ms(m.startup_initial_surface_attach_ns),
        ms(m.startup_surface_to_successful_redraw_ns),
        ms(m.startup_successful_compose_ns),
        ms(m.startup_successful_surface_transaction_ns),
        ms(m.startup_successful_finalize_ns),
        m.startup_attach_schema,
        u8::from(m.startup_attach_valid),
        ms(m.startup_attach_dispatch_ns),
        ms(m.startup_attach_prepare_ns),
        ms(m.startup_attach_window_create_ns),
        ms(m.startup_attach_window_setup_ns),
        ms(m.startup_attach_backend_finalize_ns),
        ms(m.startup_attach_chrome_geometry_ns),
        ms(m.startup_attach_surface_create_ns),
        ms(m.startup_attach_finish_ns),
        m.startup_worker_schema,
        u8::from(m.startup_worker_valid),
        ms(m.startup_worker_total_ns),
        ms(m.startup_worker_overlap_ns),
        ms(m.startup_worker_after_join_ns),
        ms(m.startup_worker_post_join_ns),
        ms(m.startup_worker_prelude_ns),
        ms(m.startup_worker_gpu_build_ns),
        ms(m.startup_worker_font_admit_ns),
        ms(m.startup_worker_font_apply_ns),
        ms(m.startup_worker_font_seal_ns),
        ms(m.startup_worker_epilogue_ns),
        m.startup_gpu_schema,
        u8::from(m.startup_gpu_valid),
        ms(m.startup_gpu_instance_ns),
        ms(m.startup_gpu_adapter_ns),
        ms(m.startup_gpu_device_ns),
        ms(m.startup_gpu_context_tail_ns),
        ms(m.startup_gpu_font_thread_ns),
        ms(m.startup_gpu_font_join_ns),
        ms(m.startup_gpu_pipelines_ns),
        ms(m.startup_gpu_pipe_shader_ns),
        ms(m.startup_gpu_pipe_uniform_atlas_ns),
        ms(m.startup_gpu_pipe_cell_ns),
        ms(m.startup_gpu_pipe_blit_ns),
        ms(m.startup_gpu_pipe_tray_ns),
        ms(m.startup_gpu_pipe_bloom_ns),
        ms(m.startup_gpu_pipe_vbuf_ns),
        ms(m.startup_gpu_pipe_tail_ns),
        ms(m.startup_gpu_tail_ns),
        cell_pipeline_pairs(&m.startup_gpu_cell_pipeline_ns),
        m.effect_pipeline_builds,
        ms(m.effect_pipeline_build_ns),
        effect_pipeline_names(m.effect_pipeline_built_mask),
        ms(m.first_present_ns),
        ms(m.first_visible_ns),
    )
}

/// Render the per-owner deadline arm ledger as `owner:arms/past` pairs,
/// comma-joined — the same ONE self-labelling field discipline as
/// [`cell_pipeline_pairs`], for the same reason: a reader never has to know an
/// order, and a new `DeadlineOwner` cannot silently shift someone else's column.
///
/// WHY IT EXISTS (audit item 6). `past_deadline_arms` is a single global number
/// and `deadline_owner` is a last-writer snapshot, so the two together cannot
/// name a spin's producer — during the 2026-08 200 kHz spin they read
/// "31,913 past arms, owner=title_summary" while the producer was the
/// session-status observer folded under that label. This field names it.
/// Non-zero owners only, so a healthy instance prints two or three pairs and a
/// sick one puts its culprit in plain sight.
fn deadline_arm_pairs(arms: &[crate::metrics::OwnerArms]) -> String {
    if arms.is_empty() {
        // Never emit a bare `key=` — every field in this line carries a token,
        // so a whitespace-splitting reader always gets a value to parse.
        return "none".to_string();
    }
    arms.iter()
        .map(|a| format!("{}:{}/{}", a.owner, a.arms, a.past_arms))
        .collect::<Vec<_>>()
        .join(",")
}

/// The same ledger as a JSON object, so the structured twin stays parseable
/// without splitting a string.
fn deadline_arm_object(arms: &[crate::metrics::OwnerArms]) -> String {
    let body = arms
        .iter()
        .map(|a| {
            format!(
                "\"{}\":{{\"arms\":{},\"past\":{}}}",
                a.owner, a.arms, a.past_arms
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// Render the per-owner WINDOWED streak-heal ledger as `owner:heals` pairs,
/// comma-joined — the arms `record_deadline`'s 32-arm past/future window
/// detector clamped to frame cadence (wake follow-ups items 18/19). Surfaced
/// BESIDE `deadline_arms_by_owner` on purpose: that field says who kept
/// arming the past, this one says whose spin the fold is actively healing.
/// Same self-labelling discipline; `none` when no owner was ever clamped.
fn streak_heal_pairs(heals: &[(&'static str, u64)]) -> String {
    if heals.is_empty() {
        // Never emit a bare `key=` — every field in this line carries a token.
        return "none".to_string();
    }
    heals
        .iter()
        .map(|(owner, count)| format!("{owner}:{count}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The same streak-heal ledger as a JSON object for the structured twin.
fn streak_heal_object(heals: &[(&'static str, u64)]) -> String {
    let body = heals
        .iter()
        .map(|(owner, count)| format!("\"{owner}\":{count}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// Render the damage-scoped refill's FULL-arm attribution as `clause:frames`
/// pairs, comma-joined — the same ONE self-labelling field discipline as
/// [`deadline_arm_pairs`], for the same reason.
///
/// WHY IT EXISTS. a78dd8a1 wired DMG-1 into the shipping frontend and shipped
/// `frame_refills_scoped`/`frame_refills_full` with it, closing on the honest
/// admission that per-cause attribution "needs the engine's validity check to
/// report its failing clause — deliberately left as follow-up". Three
/// mechanisms now hang off that continuity chain (the DMG-1 carrier, the D-2
/// per-row revision lane, the tab-strip lane splice), so a Full-dominated
/// steady state had become a thing an operator could SEE and not diagnose.
/// Non-zero clauses only: a healthy instance prints its first-frame
/// `scratch_unstamped:1` and whatever its content is actually doing.
fn refill_cause_pairs(causes: &[crate::metrics::RefillCause]) -> String {
    if causes.is_empty() {
        // Never emit a bare `key=` — see `deadline_arm_pairs`.
        return "none".to_string();
    }
    causes
        .iter()
        .map(|c| format!("{}:{}", c.cause, c.frames))
        .collect::<Vec<_>>()
        .join(",")
}

/// The same attribution as a JSON object, so the structured twin stays
/// parseable without splitting a string.
fn refill_cause_object(causes: &[crate::metrics::RefillCause]) -> String {
    let body = causes
        .iter()
        .map(|c| format!("\"{}\":{}", c.cause, c.frames))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// Render the per-cell-pipeline split as `name:ms` pairs, comma-joined.
///
/// ONE self-labelling wire field rather than twelve positional ones: a reader
/// never has to know `build_cell_pipelines`' order, and adding or removing a
/// pipeline cannot silently shift someone else's column.
fn cell_pipeline_pairs(cell_ns: &[u64; aterm_gpu::startup_probe::CELL_PIPELINE_COUNT]) -> String {
    aterm_gpu::startup_probe::CELL_PIPELINE_NAMES
        .iter()
        .zip(cell_ns.iter())
        .map(|(name, ns)| format!("{name}:{:.2}", *ns as f64 / 1e6))
        .collect::<Vec<_>>()
        .join(",")
}

/// Name the EFFECT pipelines a `effect_pipeline_built_mask` says were compiled,
/// comma-joined, or `none` — the same ONE self-labelling field discipline as
/// [`cell_pipeline_pairs`], for the same reason: a reader never has to know the
/// slot order, and a new `EffectPipeline` cannot silently shift someone else's
/// bit. `none` is the healthy reading for a launch with the shipped defaults.
fn effect_pipeline_names(mask: u64) -> String {
    let built = aterm_gpu::EFFECT_PIPELINE_NAMES
        .iter()
        .enumerate()
        .filter(|(slot, _)| mask & (1u64 << slot) != 0)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>();
    if built.is_empty() {
        "none".to_string()
    } else {
        built.join(",")
    }
}

/// The same split as a JSON object, so the structured twin stays parseable
/// without splitting a string.
fn cell_pipeline_object(cell_ns: &[u64; aterm_gpu::startup_probe::CELL_PIPELINE_COUNT]) -> String {
    let body = aterm_gpu::startup_probe::CELL_PIPELINE_NAMES
        .iter()
        .zip(cell_ns.iter())
        .map(|(name, ns)| format!("\"{name}\":{:.2}", *ns as f64 / 1e6))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// Structured twin of [`cmd_metrics`]. All scheduler/redraw counters and typed
/// owner/reason labels are present so automation never has to scrape the text
/// line. `reset` and `percentiles` retain the text verb's semantics.
pub(crate) fn cmd_metrics_json(term: Option<&Arc<Mutex<Terminal>>>, command: &str) -> String {
    let ms = |ns: u64| ns as f64 / 1e6;
    if command.trim() == "percentiles" {
        let (input, present, render) = crate::metrics::distributions();
        let key_write = crate::metrics::key_write_distribution();
        let pre_present = crate::metrics::pre_present_distribution();
        // `acquire` was published by the TEXT form and omitted here, so a JSON
        // driver could not read the drawable-park slice at all; `resize` is new.
        // Both forms now carry the same set.
        let acquire = crate::metrics::acquire_wait_distribution();
        // Twin of the text form's park scalars (see there).
        let (last_acquire_ns, max_acquire_ns) = crate::metrics::acquire_wait_last_max_ns();
        let resize = crate::metrics::resize_present_distribution();
        let tainted = crate::metrics::tainted_present_distribution();
        let p = |h: &crate::metrics::Histogram, q: f64| ms(h.percentile(q).unwrap_or(0));
        return json_ok(&format!(
            "{{\"n_input\":{},\"input_p50_ms\":{:.2},\"input_p95_ms\":{:.2},\
             \"input_p99_ms\":{:.2},\"n_present\":{},\"present_p50_ms\":{:.2},\
             \"present_p95_ms\":{:.2},\"present_p99_ms\":{:.2},\"n_render\":{},\
             \"render_p50_ms\":{:.2},\"render_p95_ms\":{:.2},\"render_p99_ms\":{:.2},\
             \"n_key_write\":{},\"key_write_p50_ms\":{:.2},\"key_write_p95_ms\":{:.2},\
             \"key_write_p99_ms\":{:.2},\"n_pre_present\":{},\
             \"pre_present_p50_ms\":{:.2},\"pre_present_p95_ms\":{:.2},\
             \"pre_present_p99_ms\":{:.2},\"n_acquire\":{},\
             \"acquire_p50_ms\":{:.2},\"acquire_p95_ms\":{:.2},\
             \"acquire_p99_ms\":{:.2},\
             \"last_acquire_wait_ms\":{:.2},\"max_acquire_wait_ms\":{:.2},\
             \"n_resize\":{},\
             \"resize_p50_ms\":{:.2},\"resize_p95_ms\":{:.2},\
             \"resize_p99_ms\":{:.2},\"n_present_tainted\":{},\
             \"present_tainted_p50_ms\":{:.2},\"present_tainted_p95_ms\":{:.2},\
             \"present_tainted_p99_ms\":{:.2}{}}}",
            input.count(),
            p(input, 0.50),
            p(input, 0.95),
            p(input, 0.99),
            present.count(),
            p(present, 0.50),
            p(present, 0.95),
            p(present, 0.99),
            render.count(),
            p(render, 0.50),
            p(render, 0.95),
            p(render, 0.99),
            key_write.count(),
            p(key_write, 0.50),
            p(key_write, 0.95),
            p(key_write, 0.99),
            pre_present.count(),
            p(pre_present, 0.50),
            p(pre_present, 0.95),
            p(pre_present, 0.99),
            acquire.count(),
            p(acquire, 0.50),
            p(acquire, 0.95),
            p(acquire, 0.99),
            ms(last_acquire_ns),
            ms(max_acquire_ns),
            resize.count(),
            p(resize, 0.50),
            p(resize, 0.95),
            p(resize, 0.99),
            tainted.count(),
            p(tainted, 0.50),
            p(tainted, 0.95),
            p(tainted, 0.99),
            // Field-for-field twin of the text form's echo fragment.
            crate::echo_rtt::percentile_fields_json(),
        ));
    }
    if command.trim() == "reset" {
        crate::metrics::reset();
        crate::echo_rtt::reset();
    }
    let (rows, cols) = try_metrics_dims(term).map_or_else(
        || ("null".to_string(), "null".to_string()),
        |(rows, cols)| (rows.to_string(), cols.to_string()),
    );
    let m = crate::metrics::snapshot();
    // Same tail surfacing and same honesty split as the text form: the two
    // stay field-for-field twins so automation never has to scrape the line.
    let (h_input, h_present, _) = crate::metrics::distributions();
    let pct = |h: &crate::metrics::Histogram, q: f64| ms(h.percentile(q).unwrap_or(0));
    let arms = crate::metrics::deadline_arm_attribution();
    let refill_causes = crate::metrics::frame_refill_full_causes();
    let streak_heals = crate::metrics::past_arm_streak_heal_attribution();
    let backend = if m.backend_gpu { "gpu" } else { "cpu" };
    json_ok(&format!(
        "{{\"backend\":\"{backend}\",\"rows\":{rows},\"cols\":{cols},\
         \"frames\":{},\"last_present_latency_ms\":{:.2},\"max_present_latency_ms\":{:.2},\
         \"last_frame_render_ms\":{:.2},\"max_frame_render_ms\":{:.2},\"slow_frames\":{},\
         \"slow_threshold_ms\":{:.1},\"last_input_present_ms\":{:.2},\
         \"max_input_present_ms\":{:.2},\"n_input\":{},\"input_p50_ms\":{:.2},\
         \"input_p95_ms\":{:.2},\"input_p99_ms\":{:.2},\"n_present\":{},\
         \"present_p50_ms\":{:.2},\"present_p95_ms\":{:.2},\"present_p99_ms\":{:.2},\
         \"last_key_write_ms\":{:.2},\
         \"max_key_write_ms\":{:.2},\"last_resize_present_ms\":{:.2},\
         \"max_resize_present_ms\":{:.2},\"last_resize_reflow_ms\":{:.2},\
         \"max_resize_reflow_ms\":{:.2},\"sync_armed\":{},\"sync_rel_end\":{},\
         \"sync_rel_timeout\":{},\"sync_holding\":{},\"perf_reduced\":{},\
         \"shed_transitions\":{},\"wake_heals\":{},\"last_redraw_total_ms\":{:.2},\
         \"max_redraw_total_ms\":{:.2},\"redraw_attempts\":{},\"redraw_early_outs\":{},\
         \"redraw_sync_holds\":{},\"redraw_retry_gated\":{},\
         \"frame_refills_scoped\":{},\"frame_refills_full\":{},\
         \"frame_refills_skipped\":{},\
         \"frame_refill_full_causes\":{},\"pre_present_attempts\":{},\
         \"last_pre_present_ms\":{:.2},\"pre_present_total_ms\":{:.2},\
         \"max_pre_present_ms\":{:.2},\
         \"last_acquire_wait_ms\":{:.2},\"max_acquire_wait_ms\":{:.2},\
         \"present_drops\":{},\
         \"last_present_drop_reason\":\"{}\",\"last_present_drop_parked\":{},\
         \"present_tainted\":{},\"last_present_tainted_ms\":{:.2},\
         \"max_present_tainted_ms\":{:.2},\"capture_episodes\":{},\
         \"capture_active\":{},\
         \"event_wakes\":{},\"timer_wakes\":{},\"wait_cancelled_wakes\":{},\
         \"poll_wakes\":{},\"wake_kind\":\"{}\",\"wake_owner\":\"{}\",\
         \"wake_late_ms\":{:.2},\"deadline_owner\":\"{}\",\"deadline_in_ms\":{:.2},\
         \"deadline_late_ms\":{:.2},\"past_deadline_arms\":{},\
         \"deadline_arms_by_owner\":{},\"past_arm_streak_heals\":{},\
         \"stale_arm_heals\":{},\
         \"max_frame_gap_ms\":{:.2},\
         \"rust_main_to_first_present_ms\":{:.2},\
         \"rust_main_to_first_visible_ms\":{:.2},\
         \"startup_phase_schema\":{},\"startup_phase_valid\":{},\
         \"startup_router_ms\":{:.2},\"startup_gui_prepare_ms\":{:.2},\
         \"startup_winit_dispatch_ms\":{:.2},\"startup_initial_surface_attach_ms\":{:.2},\
         \"startup_surface_to_successful_redraw_ms\":{:.2},\
         \"startup_successful_compose_ms\":{:.2},\
         \"startup_successful_surface_transaction_ms\":{:.2},\
         \"startup_successful_finalize_ms\":{:.2},\
         \"startup_attach_schema\":{},\"startup_attach_valid\":{},\
         \"startup_attach_dispatch_ms\":{:.2},\"startup_attach_prepare_ms\":{:.2},\
         \"startup_attach_window_create_ms\":{:.2},\"startup_attach_window_setup_ms\":{:.2},\
         \"startup_attach_backend_finalize_ms\":{:.2},\
         \"startup_attach_chrome_geometry_ms\":{:.2},\
         \"startup_attach_surface_create_ms\":{:.2},\"startup_attach_finish_ms\":{:.2},\
         \"startup_worker_schema\":{},\"startup_worker_valid\":{},\
         \"startup_worker_total_ms\":{:.2},\"startup_worker_overlap_ms\":{:.2},\
         \"startup_worker_after_join_ms\":{:.2},\"startup_worker_post_join_ms\":{:.2},\
         \"startup_worker_prelude_ms\":{:.2},\"startup_worker_gpu_build_ms\":{:.2},\
         \"startup_worker_font_admit_ms\":{:.2},\"startup_worker_font_apply_ms\":{:.2},\
         \"startup_worker_font_seal_ms\":{:.2},\"startup_worker_epilogue_ms\":{:.2},\
         \"startup_gpu_schema\":{},\"startup_gpu_valid\":{},\
         \"startup_gpu_instance_ms\":{:.2},\"startup_gpu_adapter_ms\":{:.2},\
         \"startup_gpu_device_ms\":{:.2},\"startup_gpu_context_tail_ms\":{:.2},\
         \"startup_gpu_font_thread_ms\":{:.2},\"startup_gpu_font_join_ms\":{:.2},\
         \"startup_gpu_pipelines_ms\":{:.2},\"startup_gpu_pipe_shader_ms\":{:.2},\
         \"startup_gpu_pipe_uniform_atlas_ms\":{:.2},\"startup_gpu_pipe_cell_ms\":{:.2},\
         \"startup_gpu_pipe_blit_ms\":{:.2},\"startup_gpu_pipe_tray_ms\":{:.2},\
         \"startup_gpu_pipe_bloom_ms\":{:.2},\"startup_gpu_pipe_vbuf_ms\":{:.2},\
         \"startup_gpu_pipe_tail_ms\":{:.2},\"startup_gpu_tail_ms\":{:.2},\
         \"startup_gpu_cell_pipeline_ms\":{},\
         \"effect_pipeline_builds\":{},\"effect_pipeline_build_ms\":{:.2},\
         \"effect_pipelines_built\":\"{}\",\
         \"first_present_ms\":{:.2},\"first_visible_ms\":{:.2}}}",
        m.frames_presented,
        ms(m.last_present_latency_ns),
        ms(m.max_present_latency_ns),
        ms(m.last_frame_render_ns),
        ms(m.max_frame_render_ns),
        m.slow_frames,
        ms(crate::metrics::SLOW_FRAME_THRESHOLD_NS),
        ms(m.last_input_present_ns),
        ms(m.max_input_present_ns),
        h_input.count(),
        pct(h_input, 0.50),
        pct(h_input, 0.95),
        pct(h_input, 0.99),
        h_present.count(),
        pct(h_present, 0.50),
        pct(h_present, 0.95),
        pct(h_present, 0.99),
        ms(m.last_key_write_ns),
        ms(m.max_key_write_ns),
        ms(m.last_resize_present_ns),
        ms(m.max_resize_present_ns),
        ms(m.last_resize_reflow_ns),
        ms(m.max_resize_reflow_ns),
        m.sync_holds_armed,
        m.sync_releases_end,
        m.sync_releases_timeout,
        m.sync_holding,
        m.perf_reduced,
        m.shed_transitions,
        m.wake_heals,
        ms(m.last_redraw_total_ns),
        ms(m.max_redraw_total_ns),
        m.redraw_attempts,
        m.redraw_early_outs,
        m.redraw_sync_holds,
        m.redraw_retry_gated,
        m.frame_refills_scoped,
        m.frame_refills_full,
        m.frame_refills_skipped,
        refill_cause_object(&refill_causes),
        m.pre_present_attempts,
        ms(m.last_pre_present_ns),
        ms(m.pre_present_total_ns),
        ms(m.max_pre_present_ns),
        // Field-for-field twin of the text form's drawable-park scalars.
        ms(m.last_acquire_wait_ns),
        ms(m.max_acquire_wait_ns),
        m.present_drops,
        m.last_present_drop_reason.as_str(),
        m.last_present_drop_parked,
        m.tainted_present_samples,
        ms(m.last_tainted_present_latency_ns),
        ms(m.max_tainted_present_latency_ns),
        m.capture_episodes,
        m.capture_active,
        m.event_wakes,
        m.timer_wakes,
        m.wait_cancelled_wakes,
        m.poll_wakes,
        m.last_wake_kind.as_str(),
        m.last_wake_owner.as_str(),
        ms(m.last_wake_late_ns),
        m.last_deadline_owner.as_str(),
        ms(m.deadline_in_ns),
        ms(m.last_deadline_late_ns),
        m.past_deadline_arms,
        deadline_arm_object(&arms),
        streak_heal_object(&streak_heals),
        m.stale_arm_heals,
        ms(m.max_frame_gap_ns),
        ms(m.rust_main_to_first_present_ns),
        ms(m.rust_main_to_first_visible_ns),
        m.startup_phase_schema,
        m.startup_phase_valid,
        ms(m.startup_router_ns),
        ms(m.startup_gui_prepare_ns),
        ms(m.startup_winit_dispatch_ns),
        ms(m.startup_initial_surface_attach_ns),
        ms(m.startup_surface_to_successful_redraw_ns),
        ms(m.startup_successful_compose_ns),
        ms(m.startup_successful_surface_transaction_ns),
        ms(m.startup_successful_finalize_ns),
        m.startup_attach_schema,
        m.startup_attach_valid,
        ms(m.startup_attach_dispatch_ns),
        ms(m.startup_attach_prepare_ns),
        ms(m.startup_attach_window_create_ns),
        ms(m.startup_attach_window_setup_ns),
        ms(m.startup_attach_backend_finalize_ns),
        ms(m.startup_attach_chrome_geometry_ns),
        ms(m.startup_attach_surface_create_ns),
        ms(m.startup_attach_finish_ns),
        m.startup_worker_schema,
        m.startup_worker_valid,
        ms(m.startup_worker_total_ns),
        ms(m.startup_worker_overlap_ns),
        ms(m.startup_worker_after_join_ns),
        ms(m.startup_worker_post_join_ns),
        ms(m.startup_worker_prelude_ns),
        ms(m.startup_worker_gpu_build_ns),
        ms(m.startup_worker_font_admit_ns),
        ms(m.startup_worker_font_apply_ns),
        ms(m.startup_worker_font_seal_ns),
        ms(m.startup_worker_epilogue_ns),
        m.startup_gpu_schema,
        m.startup_gpu_valid,
        ms(m.startup_gpu_instance_ns),
        ms(m.startup_gpu_adapter_ns),
        ms(m.startup_gpu_device_ns),
        ms(m.startup_gpu_context_tail_ns),
        ms(m.startup_gpu_font_thread_ns),
        ms(m.startup_gpu_font_join_ns),
        ms(m.startup_gpu_pipelines_ns),
        ms(m.startup_gpu_pipe_shader_ns),
        ms(m.startup_gpu_pipe_uniform_atlas_ns),
        ms(m.startup_gpu_pipe_cell_ns),
        ms(m.startup_gpu_pipe_blit_ns),
        ms(m.startup_gpu_pipe_tray_ns),
        ms(m.startup_gpu_pipe_bloom_ns),
        ms(m.startup_gpu_pipe_vbuf_ns),
        ms(m.startup_gpu_pipe_tail_ns),
        ms(m.startup_gpu_tail_ns),
        cell_pipeline_object(&m.startup_gpu_cell_pipeline_ns),
        m.effect_pipeline_builds,
        ms(m.effect_pipeline_build_ns),
        effect_pipeline_names(m.effect_pipeline_built_mask),
        ms(m.first_present_ns),
        ms(m.first_visible_ns),
    ))
}

/// `lines` -> `OK <total_scrollback_lines>\n` — how many lines of history
/// (tiered + ring-buffer scrollback) currently exist above the visible screen.
pub(crate) fn cmd_lines(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    format!("OK {}\n", t.grid().scrollback_lines())
}

/// `line <n>` -> `OK <text>\n` for the line at MONOTONIC ABSOLUTE row `n`, or
/// `ERR out of range\n` / `ERR evicted\n`.
///
/// COORDINATE SPACE (B-2): `n` is an ABSOLUTE row — the same space `blocks` and
/// `search` report — NOT a 0-based history index. This is the ONE documented
/// read coordinate: `blocks` gives output/command/prompt rows as absolute
/// numbers and `search` returns absolute match rows, and BOTH are fed straight
/// to `line`/`text` with the conversion done HERE at the read site. The mapping
/// (identical to the engine's `text_range`):
///   `hist = n - grid.oldest_absolute_row()`
///   `hist <  scrollback_lines`        → scrollback history line `hist`
///   `hist >= scrollback_lines`        → visible row `hist - scrollback_lines`
/// A row OLDER than `oldest_absolute_row()` has scrolled past the scrollback cap
/// and is reported as an EXPLICIT `ERR evicted\n` (never silently-shifted text —
/// the same eviction contract `blocktext` honors). Control chars collapse to
/// spaces; trailing spaces are trimmed.
pub(crate) fn cmd_line(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    let Ok(n) = rest.trim().parse::<u64>() else {
        return "ERR usage: line <abs_row>\n".to_string();
    };
    let t = term_lock(term);
    let text = match abs_row_text(&t, n) {
        AbsRow::Text(s) => s,
        AbsRow::Evicted => return "ERR evicted\n".to_string(),
        AbsRow::OutOfRange => return "ERR out of range\n".to_string(),
    };
    let mut s: String = text.chars().map(visible_char).collect();
    while s.ends_with(' ') {
        s.pop();
    }
    format!("OK {s}\n")
}

/// Outcome of resolving an absolute row to its text (B-2 coordinate space).
pub(crate) enum AbsRow {
    /// The combining-aware, NOT-yet-control-collapsed line text.
    Text(String),
    /// Older than `oldest_absolute_row()` — scrolled past the scrollback cap.
    Evicted,
    /// Newer than the live bottom visible row (no such row).
    OutOfRange,
}

/// Resolve a MONOTONIC ABSOLUTE row to its grapheme-faithful text, in the ONE
/// documented read coordinate space shared by `blocks`/`search`/`line`/`text`.
///
/// Conversion is identical to the engine's `text_range`: an absolute row maps to
/// a history index relative to the oldest retained line; indices at/above the
/// scrollback count land on the visible screen. Scrollback lines come from
/// `get_history_line` (Line text); visible rows from the combining-aware
/// `get_line_text` so accents / ZWJ clusters survive (FIDELITY I-1).
pub(crate) fn abs_row_text(t: &Terminal, abs_row: u64) -> AbsRow {
    let grid = t.grid();
    let oldest = grid.oldest_absolute_row();
    if abs_row < oldest {
        return AbsRow::Evicted;
    }
    let scrollback = grid.scrollback_lines() as u64;
    let visible_rows = u64::from(t.rows());
    let rel = abs_row - oldest;
    if rel < scrollback {
        // Scrollback history line `rel` (0 = oldest retained).
        match grid.get_history_line(rel as usize) {
            Some(line) => AbsRow::Text(line.to_string()),
            None => AbsRow::OutOfRange,
        }
    } else {
        let visible = rel - scrollback;
        if visible >= visible_rows {
            return AbsRow::OutOfRange;
        }
        AbsRow::Text(t.get_line_text(visible as i32, None).unwrap_or_default())
    }
}

/// Whether absolute row `abs_row` is a soft-wrap CONTINUATION — the grid's
/// DESTINATION convention, where the row content flowed INTO carries the flag,
/// so the first row of a logical line always reads `false`.
///
/// A row outside the retained range is not a continuation: a run whose head has
/// been evicted is treated as starting at the oldest row still there, which is
/// the only logical line the search index can honestly key.
pub(crate) fn abs_row_wrapped(t: &Terminal, abs_row: u64) -> bool {
    let grid = t.grid();
    let oldest = grid.oldest_absolute_row();
    if abs_row <= oldest {
        return false;
    }
    let scrollback = grid.scrollback_lines() as u64;
    let rel = abs_row - oldest;
    if rel < scrollback {
        return grid
            .get_history_line(rel as usize)
            .is_some_and(|line| line.is_wrapped());
    }
    let visible = rel - scrollback;
    if visible >= u64::from(t.rows()) {
        return false;
    }
    u16::try_from(visible)
        .ok()
        .and_then(|row| grid.row(row))
        .is_some_and(aterm_core::grid::Row::is_wrapped)
}

/// How far back a logical-line walk will look for the row a soft-wrap run
/// starts on. A run this long is not a wrapped sentence, it is a file with no
/// newlines catted into the grid, and the walk runs once per anchored search —
/// so past this depth the callers take their bounded fallback (rebuild from the
/// retained floor / leave the anchor physical) instead of paying a scan over
/// the whole retained history for a coordinate nobody is reading.
const MAX_LOGICAL_LINE_WALK: usize = 1024;

/// The absolute row the search index keyed the logical line containing
/// `abs_row` under: walk back over soft-wrap continuations to the row the run
/// starts on, stopping at `floor` (the oldest row the index retains).
///
/// Every column the index reports is measured from THIS row's column 0, so a
/// caller holding a physical `(row, col)` point — a find anchor — has to come
/// through here before it can address the index at all. `None` means the run
/// reaches back further than [`MAX_LOGICAL_LINE_WALK`] without meeting either
/// its head or the floor.
pub(crate) fn logical_origin(t: &Terminal, abs_row: u64, floor: u64) -> Option<u64> {
    let mut origin = abs_row;
    for _ in 0..MAX_LOGICAL_LINE_WALK {
        if origin <= floor || !abs_row_wrapped(t, origin) {
            return Some(origin);
        }
        origin -= 1;
    }
    None
}

/// `search <pat…> [case] [regex]` -> `OK <count>[ incomplete]\n` then
/// `<abs_row> <col> <len>` per match.
///
/// SEARCH-1: backed by the engine's real `TerminalSearch`, indexing BOTH the
/// SCROLLBACK (`get_history_line(0..scrollback_lines)`) AND the visible rows
/// with grapheme-aware text — so a term that has scrolled OFF the screen is
/// still found, not just the visible page. Each match's row is an ABSOLUTE row
/// (B-2's one coordinate space): feed it straight to `line`/`text`, which
/// convert at the read site. `col`/`len` are grid/DISPLAY columns within that
/// row (a wide glyph occupies 2, per the engine's `ColumnMap`) — the same cell
/// coordinate space `cell <r> <c>` addresses, NOT character offsets.
///
/// SOFT WRAP: a long line is ONE line to the reader and several rows on the
/// grid, so a hit can straddle the boundary. Such a hit is reported ONCE, at
/// the row and column it starts on, with `col + len` running PAST the grid
/// width — the overflow continues at column 0 of the following (wrapped) row,
/// and so on. A client that wants per-row spans splits `len` at the width from
/// `dims`.
///
/// ANCHORS follow from that: the run is matched as the ONE logical line it is on
/// the glass, so `regex` `^` binds where the reader's line BEGINS and `$` where
/// it ENDS, never to the grid-row edges the wrap happened to fall on. A
/// continuation row has no `^` of its own, and the character a wrap merely
/// pushed to the end of a row is not at `$`. The wrap is the terminal's layout,
/// not part of the text; a pattern that wants the row edges asks about columns
/// (`cell`) instead. An UNWRAPPED row is its own logical line, so it anchors at
/// its own ends exactly as it always has.
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
/// INCOMPLETE (DL-2): if the search index evicted lines (the searchable window
/// is capped), the header carries a trailing ` incomplete` token so the AI knows
/// results are NOT exhaustive rather than trusting a short list silently.
///
/// LOCKING (P1.0c): the O(scrollback) index build NEVER runs under the Terminal
/// mutex — a rebuild while the shell is streaming output would stall the PTY
/// reader's `process()` and the frame path for the full build time. Instead the
/// line texts are SNAPSHOTTED out of the grid in bounded chunks (the lock is
/// released between chunks, the same bounded-hold discipline as the PTY
/// reader's `PROCESS_CHUNK`) and the grapheme-aware [`TerminalSearch`] is built
/// lock-free from the copies. The built index is cached per recent terminal, so
/// an unchanged repeat reuses it; matching still scales with candidates/results
/// and finishes with one short generation-validation lock. Under concurrent
/// output a torn result is marked inconsistent and the GUI makes one bounded
/// retry instead of installing stale coordinates.
pub(crate) fn cmd_search(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    // Strip recognized flags off the TAIL only — never the last remaining token
    // (a bare `search case` keeps "case" as its pattern) — so the head is the
    // rest-of-line pattern with its internal whitespace verbatim.
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
        return "OK 0\n".to_string();
    }
    match search_full_history(term, pat, case_sensitive, is_regex) {
        Ok(search) => {
            // Function-local (like the other `Write` uses in this file) so it can
            // never shadow an `io::Write` elsewhere in the module.
            use std::fmt::Write as _;
            let incomplete = if search.results.incomplete {
                " incomplete"
            } else {
                ""
            };
            let mut out = format!("OK {}{incomplete}\n", search.results.matches.len());
            // One buffer, sized once: a match line ("<row> <col> <len>\n") is ~16
            // bytes typical, and the result set is capped at MAX_SEARCH_MATCHES
            // (100_000) => at most ~2.4 MB, so this can neither overflow nor run
            // away. The old shape allocated a throwaway `String` per match purely
            // to memcpy it in, and grew `out` by ~17 doubling reallocs on a broad
            // pattern. Bytes on the wire are unchanged.
            out.reserve(search.results.matches.len() * 24);
            for m in &search.results.matches {
                // m.line is the ABSOLUTE row (the index is keyed by absolute row).
                // `writeln!` into a String is infallible, so the Result is moot.
                let _ = writeln!(out, "{} {} {}", m.line, m.start_col, m.len());
            }
            out
        }
        Err(e) => format!("ERR search: {e}\n"),
    }
}

/// Full-scrollback search over the LIVE terminal, shared by the `search` control verb
/// ([`cmd_search`]) and the GUI's Cmd-F find (`App::search_recompute`). Returns
/// [`SearchResults`] keyed by ABSOLUTE row (`SearchMatch.line`), plus the `incomplete`
/// eviction flag, or `Err` for an invalid regex.
///
/// LOCKING: snapshots only the newest configured/searchable suffix, holding the Terminal
/// lock at most [`SNAPSHOT_CHUNK_LINES`] rows at a time, and builds the trigram index with
/// the lock RELEASED. The index is cached per recent terminal (keyed by identity +
/// alt-screen + `content_seq`), so unchanged queries reuse it; matching still runs and a
/// short endpoint lock validates its coordinate generation. Socket and GUI callers share
/// this bounded cache.
/// Whether the off-lock chunked snapshot may be CACHED as "the content at the cache key".
/// Both conditions must hold: no chunk read a frame that diverged from the key DURING the
/// copy (`!torn` — this is what catches a main→alt→main round-trip that leaves the main
/// grid's `content_seq` unchanged, which an endpoint-only check would miss), AND the frame
/// STILL matches the key at the end (`alt_now`/`seq_now` equal the key). Either failing
/// means the index may be torn: fine to answer THIS query from its snapshot, never safe to
/// re-serve under the key. Pure, so the caching decision is unit-testable without having to
/// drive the (hard-to-reproduce) mid-copy swap.
fn snapshot_cacheable(
    torn: bool,
    key_alt: bool,
    alt_now: bool,
    key_seq: u64,
    seq_now: u64,
    key_revision: u64,
    revision_now: u64,
) -> bool {
    !torn && alt_now == key_alt && seq_now == key_seq && revision_now == key_revision
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
    pub(crate) content_seq: u64,
    /// Grid width the soft-wrap runs were laid out in. It travels WITH the
    /// results because a match's end column counts straight through a wrap
    /// boundary, and dividing that by any other width puts the continuation on
    /// the wrong row.
    pub(crate) cols: usize,
    /// False when an alt-screen/content transition occurred during the chunked
    /// snapshot. Such a mixed snapshot is never safe for GUI highlighting.
    pub(crate) consistent: bool,
}

/// The soft-wrap layout one query is answered in.
///
/// A run of soft-wrapped grid rows is ONE line to the reader, and a hit that
/// straddles the boundary lives in neither row's own text, so the index holds
/// the run JOINED, keyed at its first absolute row. Columns in that line
/// therefore run past the grid's width and address the run's later rows; `cols`
/// is the uniform stride that inverts them back to a physical `(row, col)`.
///
/// `logical_anchor` is the caller's physical find anchor carried into the same
/// frame. It is not a convenience: the lazy point walks address the index
/// directly, and a physical point compared against a joined line's columns
/// would step over every remaining hit on the line the anchor sits in.
#[derive(Clone, Copy)]
struct WrapFrame {
    /// Display columns one grid row holds — the width each continued row was
    /// padded out to before the join.
    cols: usize,
    /// [`WrapFrame::to_logical`] of the caller's anchor, when it has one.
    logical_anchor: Option<(usize, usize)>,
}

impl WrapFrame {
    /// Carry a physical `(row, col)` point into the joined line's columns,
    /// given the row that line starts on.
    fn to_logical(cols: usize, origin: usize, row: usize, col: usize) -> (usize, usize) {
        (
            origin,
            row.saturating_sub(origin)
                .saturating_mul(cols)
                .saturating_add(col),
        )
    }

    /// Re-express one engine match — keyed at a logical line's first row, its
    /// columns measured along the joined line — as the physical row and column
    /// the hit STARTS on, with `end_col` still measured from that row's column
    /// 0. A hit that straddles the wrap therefore keeps `end_col > cols`: one
    /// record to navigate to, carrying the span the highlight has to paint
    /// across the rows below it.
    fn to_physical(self, m: &SearchMatch) -> SearchMatch {
        // A zero-width grid cannot lay anything out; treat it as one column so
        // the division is total and the match keeps its own row.
        let cols = self.cols.max(1);
        let row_offset = m.start_col / cols;
        let start_col = m.start_col % cols;
        SearchMatch::new(
            m.line.saturating_add(row_offset),
            start_col,
            start_col.saturating_add(m.len()),
        )
    }

    /// [`Self::to_physical`] over a whole result set, in place.
    fn results_to_physical(self, results: &mut SearchResults) {
        for m in &mut results.matches {
            *m = self.to_physical(m);
        }
    }
}

/// Fold each soft-wrapped run of snapshot rows into the ONE logical line the
/// reader sees, in place: the run's head takes the whole text and every
/// continuation becomes empty, so a hit that straddles the boundary is found
/// exactly once, keyed at the row it starts on.
///
/// Every row a continuation follows is padded back out to the full `cols`
/// display columns it occupies on the glass, however few of them hold text. Grid
/// rows are read TRIMMED (the visible-row reader drops trailing blanks), so
/// without the padding a run whose head ends in spaces — anything printed
/// through a left-justified `%-Ns` — would splice its neighbour's first column
/// onto its last non-blank one, both inventing matches that are not on the glass
/// and shifting every column after it. With it, the joined line has a UNIFORM
/// stride: column `c` of the run is row `c / cols`, column `c % cols`, which is
/// the arithmetic [`WrapFrame::to_physical`] inverts. That uniformity is the
/// whole contract, so the padding is derived from the ROW COUNT and never from
/// the accumulated column total.
///
/// Joining also makes the logical line the unit a regex sees, so `^`/`$` bind to
/// the reader's line rather than to a grid row — the semantics
/// [`search_full_history_direction`] documents.
fn join_wrapped_rows(lines: &mut [String], wrapped: &[bool], cols: usize) {
    // A zero-column grid lays nothing out; leaving the rows alone keeps every
    // coordinate the caller already has, and there is no boundary to straddle.
    if cols == 0 {
        return;
    }
    let mut head = 0usize;
    while head < lines.len() {
        let mut end = head.saturating_add(1);
        while wrapped.get(end).copied().unwrap_or(false) {
            end = end.saturating_add(1);
        }
        let continuations = end.saturating_sub(head).saturating_sub(1);
        if continuations > 0
            && let Some((slot, rest)) = lines.get_mut(head..).and_then(<[String]>::split_first_mut)
        {
            let mut joined = std::mem::take(slot);
            // Carried, not re-measured: re-counting the accumulated text once
            // per continuation would make joining a long run quadratic in its
            // length, and a `cat` of a file with no newlines is exactly a long
            // run.
            let mut columns = aterm_search::display_columns(&joined);
            // How many columns the run occupies once the rows joined so far are
            // laid out: one grid width per ROW, counted here rather than derived
            // from `columns % cols`. A row whose cells are all blank trims to the
            // empty string, leaving the running count already a multiple of the
            // width, so a modulo would pad it by nothing and the joined line
            // would silently lose that row's entire stride — every later hit on
            // the run then inverts to one grid row too high, and find paints the
            // blank row while the real match stays dark. A blank row inside a run
            // is an ordinary redraw, not a corner case: `CSI 2 K` erases a row
            // without clearing the NEXT row's continuation flag.
            let mut occupied = cols;
            for row in rest.iter_mut().take(continuations) {
                let pad = occupied.saturating_sub(columns);
                joined.extend(std::iter::repeat_n(' ', pad));
                columns = columns.saturating_add(pad);
                let continuation = std::mem::take(row);
                columns = columns.saturating_add(aterm_search::display_columns(&continuation));
                joined.push_str(&continuation);
                occupied = occupied.saturating_add(cols);
            }
            *slot = joined;
        }
        head = end;
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the wrap frame rides alongside the query's own surface, like the anchor"
)]
fn query_search_index(
    index: &TerminalSearch,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
    wrap: WrapFrame,
) -> Result<(SearchResults, Option<SearchMatch>), SearchOptionsError> {
    let mut results =
        index.search_results_opts_direction(query, case_sensitive, is_regex, direction)?;
    // Physical FIRST: `select_point_match` orders results against the caller's
    // physical anchor, so a joined line's columns must never reach it.
    wrap.results_to_physical(&mut results);
    let point_match = select_point_match(
        index,
        &results,
        query,
        case_sensitive,
        is_regex,
        direction,
        anchor,
        strict,
        wrap,
    )?;
    Ok((results, point_match))
}

/// Anchored point-match selection over ALREADY-COMPUTED batch results — split
/// out of [`query_search_index`] verbatim so the narrowing path (SA-1) shares
/// the exact anchored/point/cap-edge policy instead of re-implementing it.
#[allow(
    clippy::too_many_arguments,
    reason = "verbatim split of query_search_index's policy tail; every flag is load-bearing"
)]
fn select_point_match(
    index: &TerminalSearch,
    results: &SearchResults,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
    wrap: WrapFrame,
) -> Result<Option<SearchMatch>, SearchOptionsError> {
    let point_match = if let Some(anchor) = anchor {
        if results.matches.len() < MAX_SEARCH_MATCHES {
            let qualifies = |found: &SearchMatch| {
                let point = (found.line, found.start_col);
                match direction {
                    EngineSearchDirection::Forward => {
                        if strict {
                            point > anchor
                        } else {
                            point >= anchor
                        }
                    }
                    EngineSearchDirection::Backward => {
                        if strict {
                            point < anchor
                        } else {
                            point <= anchor
                        }
                    }
                    _ => false,
                }
            };
            match direction {
                EngineSearchDirection::Forward => results
                    .matches
                    .iter()
                    .find(|found| qualifies(found))
                    .or_else(|| results.matches.first())
                    .cloned(),
                EngineSearchDirection::Backward => results
                    .matches
                    .iter()
                    .rfind(|found| qualifies(found))
                    .or_else(|| results.matches.last())
                    .cloned(),
                _ => None,
            }
        } else {
            // Past the batch cap the walk addresses the INDEX, whose lines are
            // joined soft-wrap runs — so it takes the anchor in that frame and
            // hands back a match that still has to be re-expressed physically.
            index
                .find_direction_opts(
                    query,
                    case_sensitive,
                    is_regex,
                    aterm_search::DirectedFind {
                        anchor: wrap.logical_anchor.unwrap_or(anchor),
                        direction,
                        inclusive: !strict,
                        wrap: true,
                    },
                )?
                .map(|found| wrap.to_physical(&found))
        }
    } else {
        None
    };
    Ok(point_match)
}

// ---------------------------------------------------------------------------
// SA-1: isearch-style prefix narrowing for the per-keystroke find-bar query.
//
// The find bar re-enters `search_full_history_direction` on EVERY text edit.
// On unchanged content that is a snapshot-cache hit — but the QUERY layer was
// stateless: each keystroke re-ran the complete batch query (full range scan
// for 1–2-char literals, posting-list decode + intersection for longer ones,
// up-to-100k match materialization) even though the consumer above is
// precisely a stateful incremental search whose grown query can only ever
// match INSIDE the previous query's occurrence set.
//
// A `NarrowSession` keeps, per terminal and per exact index generation
// (alt/content_seq/revision/max_lines/case key — any content change kills it
// by key mismatch, never by explicit invalidation), a PREFIX STACK of frames:
// the ascending occurrence-line sets of each typed prefix, produced by
// `TerminalSearch::search_literal_narrowed` (whose results are
// differential-tested equal to the batch path in `aterm-search`). A keystroke
// that extends a stacked prefix verifies ONLY that frame's lines — no range
// scan, no posting decode; backspace re-verifies the shorter prefix off its
// own frame; a non-suffix edit, a regex query, a capped previous set or any
// content change falls back to the batch path (and reseeds where legal).
//
// Frames hold OCCURRENCE lines (fold-level containment), not reported-match
// lines — the subset property `matches(q + c) ⊆ occurrences(q)` is pure
// string logic (`lower_fold(q + c) == lower_fold(q) + lower_fold(c)`), immune
// to the zero-display-width match drops that make reported-match frames
// unsound (ﬁ/ß multi-char folds, prepend clusters — see the engine tests).
// ---------------------------------------------------------------------------

/// One typed prefix's occurrence frame.
struct NarrowFrame {
    query: String,
    /// Ascending retained lines whose folded text contains the folded query.
    occurrence_lines: Vec<u32>,
}

/// A terminal's live narrowing stack, valid for exactly one index generation.
struct NarrowSession {
    /// Identity key only (never dereferenced) — same discipline as
    /// [`SearchSnapshot::term`].
    term: Weak<Mutex<Terminal>>,
    alt_screen: bool,
    content_seq: u64,
    absolute_row_revision: u64,
    max_lines: usize,
    case_sensitive: bool,
    /// Prefix stack: `frames[i].query` is a byte-prefix of `frames[i+1].query`.
    frames: Vec<NarrowFrame>,
}

/// A few concurrent find bars; matches the working-set thinking behind
/// [`SEARCH_SNAPSHOT_CAPACITY`] at a quarter the size (one session per
/// terminal, newest-kept).
const NARROW_SESSION_CAPACITY: usize = 4;

/// Retained-entry budget per session (~1.6 MB of `u32` at the bound). The
/// SHALLOWEST frames (the biggest — short prefixes) are dropped first; a deep
/// backspace past a dropped frame reseeds from the engine, which is always
/// correct, merely batch-priced.
const NARROW_SESSION_MAX_ENTRIES: usize = 4 * MAX_SEARCH_MATCHES;

static NARROW_SESSIONS: Mutex<VecDeque<NarrowSession>> = Mutex::new(VecDeque::new());

fn narrow_sessions_lock() -> MutexGuard<'static, VecDeque<NarrowSession>> {
    // Stale-on-poison is safe: every session is validated by its full key.
    NARROW_SESSIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// Take (remove) this terminal's session iff its ENTIRE key matches the
/// current query context. A key mismatch leaves nothing behind for this
/// terminal to reuse, but does not disturb other terminals' sessions.
fn take_narrow_session(
    term: &Arc<Mutex<Terminal>>,
    alt_screen: bool,
    content_seq: u64,
    absolute_row_revision: u64,
    max_lines: usize,
    case_sensitive: bool,
) -> Option<NarrowSession> {
    let mut sessions = narrow_sessions_lock();
    sessions.retain(|session| session.term.strong_count() != 0);
    let position = sessions.iter().position(|session| {
        std::ptr::eq(session.term.as_ptr(), Arc::as_ptr(term))
            && session.alt_screen == alt_screen
            && session.content_seq == content_seq
            && session.absolute_row_revision == absolute_row_revision
            && session.max_lines == max_lines
            && session.case_sensitive == case_sensitive
    })?;
    sessions.remove(position)
}

fn store_narrow_session(session: NarrowSession) {
    let mut sessions = narrow_sessions_lock();
    sessions.retain(|existing| {
        existing.term.strong_count() != 0
            && !std::ptr::eq(existing.term.as_ptr(), session.term.as_ptr())
    });
    if sessions.len() >= NARROW_SESSION_CAPACITY {
        sessions.pop_front();
    }
    sessions.push_back(session);
}

#[cfg(test)]
thread_local! {
    static NARROWED_QUERY_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_narrowed_query() {
    NARROWED_QUERY_STEPS.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(not(test))]
fn record_narrowed_query() {}

/// Take (read and reset) how many queries on THIS thread were served off a
/// narrowing frame — the deterministic reach signal for the wiring tests
/// (thread-local so parallel tests on other terminals cannot perturb it).
#[cfg(test)]
fn take_narrowed_query_steps() -> usize {
    NARROWED_QUERY_STEPS.with(|count| {
        let value = count.get();
        count.set(0);
        value
    })
}

/// The cache-hit query layer with narrowing: literal queries consult/extend
/// this terminal's [`NarrowSession`]; regex and empty queries (and any capped
/// backward edge) take the batch path unchanged. Results are ALWAYS equal to
/// [`query_search_index`] on the same inputs — narrowing changes which lines
/// get verified, never what a verified result is (engine-differential-tested;
/// re-pinned here by the wiring tests).
#[allow(
    clippy::too_many_arguments,
    reason = "the narrow-session key rides alongside query_search_index's own surface"
)]
fn query_search_index_narrowing(
    term: &Arc<Mutex<Terminal>>,
    index: &TerminalSearch,
    key_alt: bool,
    key_seq: u64,
    absolute_row_revision: u64,
    max_lines: usize,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
    wrap: WrapFrame,
) -> Result<(SearchResults, Option<SearchMatch>), SearchOptionsError> {
    if is_regex || query.is_empty() {
        // Regex has no prefix-subset property; empty is the neutral bar state.
        return query_search_index(
            index,
            query,
            case_sensitive,
            is_regex,
            direction,
            anchor,
            strict,
            wrap,
        );
    }
    let mut session = take_narrow_session(
        term,
        key_alt,
        key_seq,
        absolute_row_revision,
        max_lines,
        case_sensitive,
    );
    // Deepest stacked prefix of this query (equality included: a repeat or
    // backspaced-to query re-verifies off its OWN frame — occurrences ⊇
    // matches, so that is complete too).
    let frame_index = session.as_ref().and_then(|session| {
        session
            .frames
            .iter()
            .rposition(|frame| query.starts_with(frame.query.as_str()))
    });
    let narrowed = {
        let prev_lines = session.as_ref().and_then(|session| {
            frame_index.map(|at| session.frames[at].occurrence_lines.as_slice())
        });
        // COST GUARD: a frame is a SUBSET property, not a cost guarantee. The
        // keystroke that turns a COMMON prefix into a RARE query inverts the
        // comparison — the engine's trigram intersection visits a handful of
        // candidates while the inherited frame still holds every line the
        // common prefix occurred on (measured on a 60k-line log-shaped probe:
        // 2.60 ms off a 60 000-line frame against 0.085 ms through the index,
        // on exactly the keystroke whose latency the user feels most). So
        // narrow only while the frame is no bigger than the candidate set the
        // engine would walk itself; the fallback is the plain seed path, which
        // reseeds a fresh, SMALLER frame for the next keystroke, so the guard
        // costs one step and never compounds.
        //
        // The comparison carries a 2x slack because the two sides do not cost
        // the same PER CANDIDATE: a frame line is one verify, while an engine
        // candidate additionally pays the driver list's varint decode and a
        // binary-search probe per remaining trigram (~1.5x per candidate on
        // the same probe). Only a DECISIVELY smaller candidate set is worth
        // abandoning a frame for.
        let prev_lines = prev_lines.filter(|lines| {
            match index.literal_candidate_bound(query, case_sensitive) {
                // Sub-trigram query: the engine range-scans every retained
                // line, so any frame is at least as cheap.
                None => true,
                Some(bound) => lines.len() as u64 <= bound.saturating_mul(2),
            }
        });
        if prev_lines.is_some() {
            record_narrowed_query();
        }
        index.search_literal_narrowed(query, case_sensitive, prev_lines)
    };
    let NarrowedSearch {
        results: narrowed_results,
        occurrence_lines,
    } = narrowed;
    // A capped narrowed walk carries the FORWARD (oldest-retaining) cap edge;
    // a capped BACKWARD batch retains the newest matches instead — fall back
    // so the direction-aware edge semantics stay byte-identical.
    let capped = narrowed_results.matches.len() >= MAX_SEARCH_MATCHES;
    let mut results = if capped && direction == EngineSearchDirection::Backward {
        index.search_results_opts_direction(query, case_sensitive, false, direction)?
    } else {
        narrowed_results
    };
    // Physical FIRST, for the reason [`query_search_index`] states.
    wrap.results_to_physical(&mut results);
    if let Some(occurrence_lines) = occurrence_lines {
        let mut session = session.take().unwrap_or_else(|| NarrowSession {
            term: Arc::downgrade(term),
            alt_screen: key_alt,
            content_seq: key_seq,
            absolute_row_revision,
            max_lines,
            case_sensitive,
            frames: Vec::new(),
        });
        // Keep only ancestors (prefixes) of this query, then push/replace so
        // the stack invariant (each frame a byte-prefix of the next) holds.
        session.frames.truncate(frame_index.map_or(0, |at| at + 1));
        if session
            .frames
            .last()
            .is_some_and(|frame| frame.query == query)
        {
            session.frames.pop();
        }
        session.frames.push(NarrowFrame {
            query: query.to_string(),
            occurrence_lines,
        });
        let mut total: usize = session
            .frames
            .iter()
            .map(|frame| frame.occurrence_lines.len())
            .sum();
        while total > NARROW_SESSION_MAX_ENTRIES && session.frames.len() > 1 {
            let dropped = session.frames.remove(0);
            total = total.saturating_sub(dropped.occurrence_lines.len());
        }
        store_narrow_session(session);
    } else if let Some(session) = session {
        // Capped walk: no frame for THIS query, but the stacked ancestors are
        // still valid for other extensions of their prefixes — keep them.
        store_narrow_session(session);
    }
    let point_match = select_point_match(
        index,
        &results,
        query,
        case_sensitive,
        false,
        direction,
        anchor,
        strict,
        wrap,
    )?;
    Ok((results, point_match))
}

pub(crate) fn search_full_history(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
) -> Result<FullHistorySearch, SearchOptionsError> {
    search_full_history_direction(
        term,
        query,
        case_sensitive,
        is_regex,
        EngineSearchDirection::Forward,
        None,
        false,
    )
}

/// Direction-aware full-history query for the incremental search overlay.
///
/// The socket verb uses [`search_full_history`] and therefore retains the
/// historical oldest-first capped subset. Cmd-R uses this entry point so a
/// capped reverse search retains the newest matches instead.
///
/// THE UNIT OF MATCHING IS THE LOGICAL LINE, not the grid row. Every soft-wrap
/// run is joined ([`join_wrapped_rows`]) into the one line the reader sees
/// before the engine looks at it, so a hit may straddle a wrap; the results
/// come back in PHYSICAL coordinates ([`WrapFrame::to_physical`]) with such a
/// hit's `end_col` counting straight through the boundary. Two consequences a
/// caller has to know about, both deliberate:
///
/// - Regex `^`/`$` anchor the LOGICAL line. A continuation row's column 0 is
///   mid-line and does not match `^`; a character the wrap pushed to a row's
///   end is mid-line and does not match `$`. This is what "search across soft
///   wraps" means — the wrap is layout the terminal chose, never text the
///   program wrote — and an unwrapped row, being its own logical line, anchors
///   at its own ends unchanged.
/// - A query of only spaces can match the blank cells a continued row is padded
///   back out with. Those cells are genuinely blank on the glass and genuinely
///   inside the line, so they are matchable like any other run of spaces.
pub(crate) fn search_full_history_direction(
    term: &Arc<Mutex<Terminal>>,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
) -> Result<FullHistorySearch, SearchOptionsError> {
    // Short lock: capture the cache key and the coordinate frame only.
    let (key_alt, key_seq, oldest, scrollback, visible_rows, absolute_row_revision) = {
        let t = term_lock(term);
        (
            t.is_alternate_screen(),
            t.content_seq(),
            t.grid().oldest_absolute_row(),
            t.grid().scrollback_lines(),
            t.rows() as usize,
            t.absolute_row_revision(),
        )
    };
    let scrollback_u64 = u64::try_from(scrollback).unwrap_or(u64::MAX);
    let base_y = i64::try_from(oldest.saturating_add(scrollback_u64)).unwrap_or(i64::MAX);

    // The configured index depth cap (config `search_history_lines`): how many of the
    // newest lines the trigram index retains before evicting the oldest (and flagging
    // results incomplete). FLOORED so the LIVE SCREEN is always searchable even under a
    // tiny configured cap — only scrollback beyond the cap is evicted. The floor is
    // `max_cached_for_retained(visible_rows)`, NOT `visible_rows` itself: the engine
    // evicts to a 3/4 low-water mark, so a cap of exactly `visible_rows` would let the
    // oldest quarter of the visible screen be dropped (top on-screen rows returning "no
    // match"). Captured once so the cache key and the fresh build agree even across a
    // concurrent config reload (a resize bumps content_seq, so the floor tracks it).
    let max_lines = search_max_lines().max(max_cached_for_retained(visible_rows));

    // The indexed window, hoisted above the cache branch because the wrap frame
    // needs its floor: an anchor's logical-line walk stops where the index does.
    let total = scrollback + visible_rows;
    let retained_total = total.min(max_lines);
    let skipped_prefix = total.saturating_sub(retained_total);
    let retained_oldest = oldest.saturating_add(skipped_prefix as u64);
    let retained_base = usize::try_from(retained_oldest).unwrap_or(usize::MAX);
    let indexed_end = retained_base.saturating_add(retained_total);

    // The soft-wrap layout this query is answered in. `cols` comes from the same
    // grid the rows do, so the stride that un-joins a logical line's columns can
    // never belong to a different geometry than the rows that were joined; the
    // anchor is carried into that frame here, where the runs are readable.
    let wrap = {
        let t = term_lock(term);
        let cols = usize::from(t.grid().cols());
        WrapFrame {
            cols,
            logical_anchor: anchor.and_then(|(row, col)| {
                let origin =
                    logical_origin(&t, u64::try_from(row).unwrap_or(u64::MAX), retained_oldest)?;
                let origin = usize::try_from(origin).unwrap_or(row);
                Some(WrapFrame::to_logical(cols, origin, row, col))
            }),
        }
    };

    // Cache hit: clone the immutable index handle while holding the process-global
    // cache lock, then run the potentially large query after releasing it. Searches
    // in another window can therefore proceed concurrently instead of serializing
    // behind a common one-character query over a deep history.
    let cached_index =
        cached_search_index(term, key_alt, key_seq, absolute_row_revision, max_lines);
    if let Some(index) = cached_index {
        // SA-1: the per-keystroke query layer. Literal queries narrow from the
        // previous keystroke's occurrence frame (batch-identical results, no
        // index work for the dominant grown-by-one-char edit); regex/empty and
        // every fallback condition route through the plain batch path inside.
        let (results, point_match) = query_search_index_narrowing(
            term,
            &index,
            key_alt,
            key_seq,
            absolute_row_revision,
            max_lines,
            query,
            case_sensitive,
            is_regex,
            direction,
            anchor,
            strict,
            wrap,
        )?;
        #[cfg(test)]
        maybe_mutate_search_cache_hit(term);
        // A cache key proves the immutable index was current when this query
        // STARTED. Recheck after the potentially long query so a resize/output
        // or protected-footer splice cannot install a stale point coordinate.
        let consistent = {
            let terminal = term_lock(term);
            snapshot_cacheable(
                false,
                key_alt,
                terminal.is_alternate_screen(),
                key_seq,
                terminal.content_seq(),
                absolute_row_revision,
                terminal.absolute_row_revision(),
            )
        };
        return Ok(FullHistorySearch {
            results,
            point_match,
            base_y,
            absolute_row_revision,
            content_seq: key_seq,
            cols: wrap.cols,
            consistent,
        });
    }

    // Miss: snapshot only the newest suffix the configured index can retain.
    // Copying/indexing an older prefix merely to trigger repeated 25%-evictions
    // made cold search scale with unsearchable history (and churn O(n log n)
    // key sorts). `abs_row_text` converts each retained absolute row against the
    // LIVE frame at read time, so a row that scrolls off mid-snapshot reads as
    // evicted (empty) — exactly what a fresh build under the shifted frame lacks.
    let visible_base = usize::try_from(oldest.saturating_add(scrollback_u64)).unwrap_or(usize::MAX);
    let reusable =
        take_reusable_search_index(term, key_alt, absolute_row_revision, max_lines, indexed_end);
    let (mut index, snapshot_start) = if let Some((mut index, previous_visible_base)) = reusable {
        index.retain_history_from(retained_base);
        // Scrollback is immutable only after a row leaves the visible grid.
        // Refresh from the PREVIOUS visible boundary so a row edited while it
        // was visible and then scrolled away between searches cannot retain
        // stale text. This remains bounded to the old visible rows plus newly
        // appended rows (and is clipped at the retained-history floor).
        (index, previous_visible_base.max(retained_base))
    } else {
        (
            TerminalSearch::with_capacity_and_max(retained_total, max_lines),
            retained_base,
        )
    };
    // A soft-wrap run is indexed as ONE line keyed at the row it starts on, so a
    // refresh that began INSIDE a run would re-key its tail as an (empty)
    // continuation while the run's head still carried its old, unjoined text —
    // and the hits on the tail would vanish.
    //
    // The walk starts one row BELOW the refresh boundary, not at it. The row it
    // begins on is the previous generation's top VISIBLE row, so the program
    // could have rewritten it out of a run since; the run whose head must be
    // re-joined is then the one ending at the row ABOVE, and that row's own
    // flags are immutable history. Starting a row early catches both shapes and
    // costs one extra row on the ordinary unwrapped refresh. A run too deep to
    // walk back over falls to the retained floor, which re-indexes more rows
    // than needed but can never leave a half-joined line behind.
    let snapshot_start = {
        let t = term_lock(term);
        logical_origin(
            &t,
            u64::try_from(snapshot_start)
                .unwrap_or(u64::MAX)
                .saturating_sub(1)
                .max(retained_oldest),
            retained_oldest,
        )
        .and_then(|origin| usize::try_from(origin).ok())
        .unwrap_or(retained_base)
    };
    let snapshot_lines = indexed_end.saturating_sub(snapshot_start);
    let snapshot_start_u64 = u64::try_from(snapshot_start).unwrap_or(u64::MAX);
    let mut lines: Vec<String> = Vec::with_capacity(snapshot_lines);
    // Which snapshot rows are soft-wrap CONTINUATIONS, read in the same lock
    // holds as the text so the layout and the content can never come from two
    // different frames.
    let mut wrapped: Vec<bool> = Vec::with_capacity(snapshot_lines);
    // Track whether the active screen / content generation ever diverged from the cache key
    // DURING the chunked copy — not just at the endpoints. A main→alt→main round-trip leaves
    // the main grid's content_seq unchanged (the alt swap doesn't touch its cells), so an
    // endpoint-only check would pass yet a mid-copy chunk could have read alt-grid rows; that
    // torn index must never be cached under the (main, seq) key.
    let mut torn = false;
    while lines.len() < snapshot_lines {
        let t = term_lock(term);
        if t.is_alternate_screen() != key_alt
            || t.content_seq() != key_seq
            || t.absolute_row_revision() != absolute_row_revision
        {
            torn = true;
        }
        let end = (lines.len() + SNAPSHOT_CHUNK_LINES).min(snapshot_lines);
        for j in lines.len()..end {
            let abs = snapshot_start_u64.saturating_add(u64::try_from(j).unwrap_or(u64::MAX));
            let text = match abs_row_text(&t, abs) {
                AbsRow::Text(s) => s,
                AbsRow::Evicted | AbsRow::OutOfRange => String::new(),
            };
            // The FIRST snapshot row starts a logical line by construction (the
            // walk above put it on one), whatever the grid says about the row
            // above it — the index has nothing older to join it to.
            wrapped.push(j > 0 && abs_row_wrapped(&t, abs));
            lines.push(text);
        }
    }
    join_wrapped_rows(&mut lines, &wrapped, wrap.cols);

    // Build the index OUTSIDE any lock: line `j` keys at absolute row `oldest + j`, so
    // SearchMatch.line is already an absolute row and the eviction/incomplete semantics
    // are the engine's. Cap retained lines at `max_lines`: the oldest are evicted (the
    // newest — always including the visible screen — stay searchable) and the engine
    // flags results incomplete, which the callers surface honestly.
    record_search_snapshot_lines(lines.len());
    index.index_visible_content(snapshot_start, &lines);
    if skipped_prefix > 0 {
        index.mark_history_prefix_evicted(retained_base);
    }

    // Run the query BEFORE moving the index into the cache (the borrow ends with the owned
    // `results`). Compute results regardless of caching so an invalid regex still returns Err.
    let index = Arc::new(index);
    let results = query_search_index(
        &index,
        query,
        case_sensitive,
        is_regex,
        direction,
        anchor,
        strict,
        wrap,
    );

    // Cache only a PROVEN-consistent snapshot: if the content generation or active screen
    // moved during the chunked copy (per-chunk `torn`) or differs now, the index may be torn
    // (fine for this one query's snapshot semantics, but it must never be re-served as "the
    // content at key_seq"). The decision is [`snapshot_cacheable`] (pure, unit-tested).
    let consistent = {
        let t = term_lock(term);
        snapshot_cacheable(
            torn,
            key_alt,
            t.is_alternate_screen(),
            key_seq,
            t.content_seq(),
            absolute_row_revision,
            t.absolute_row_revision(),
        )
    };
    if consistent {
        store_search_snapshot(SearchSnapshot {
            term: Arc::downgrade(term),
            alt_screen: key_alt,
            content_seq: key_seq,
            absolute_row_revision,
            max_lines,
            visible_base,
            indexed_end,
            index: Arc::clone(&index),
        });
    }
    results.map(|(results, point_match)| FullHistorySearch {
        results,
        point_match,
        base_y,
        absolute_row_revision,
        content_seq: key_seq,
        cols: wrap.cols,
        consistent,
    })
}

/// One exact point-relative match from the immutable full-history cache.
/// Literal navigation is lazy and independent of [`aterm_search::MAX_SEARCH_MATCHES`].
pub(crate) struct FullHistoryPoint {
    pub(crate) point_match: Option<SearchMatch>,
    pub(crate) base_y: i64,
    pub(crate) absolute_row_revision: u64,
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
    let (key_alt, key_seq, oldest, scrollback, visible_rows, absolute_row_revision) = {
        let terminal = term_lock(term);
        (
            terminal.is_alternate_screen(),
            terminal.content_seq(),
            terminal.grid().oldest_absolute_row(),
            terminal.grid().scrollback_lines(),
            terminal.rows() as usize,
            terminal.absolute_row_revision(),
        )
    };
    let base_y =
        i64::try_from(oldest.saturating_add(u64::try_from(scrollback).unwrap_or(u64::MAX)))
            .unwrap_or(i64::MAX);
    let max_lines = search_max_lines().max(max_cached_for_retained(visible_rows));
    let retained_oldest = oldest.saturating_add(
        u64::try_from((scrollback + visible_rows).saturating_sub(max_lines)).unwrap_or(0),
    );
    // Same frame as the batch path: the index holds soft-wrapped runs joined, so
    // this walk hands the anchor over in the joined line's columns and hands the
    // hit back in the grid's.
    let wrap = {
        let terminal = term_lock(term);
        let cols = usize::from(terminal.grid().cols());
        WrapFrame {
            cols,
            logical_anchor: logical_origin(
                &terminal,
                u64::try_from(anchor.0).unwrap_or(u64::MAX),
                retained_oldest,
            )
            .map(|origin| {
                let origin = usize::try_from(origin).unwrap_or(anchor.0);
                WrapFrame::to_logical(cols, origin, anchor.0, anchor.1)
            }),
        }
    };
    let cached_index =
        cached_search_index(term, key_alt, key_seq, absolute_row_revision, max_lines);
    let Some(index) = cached_index else {
        let full = search_full_history_direction(
            term,
            query,
            case_sensitive,
            is_regex,
            direction,
            Some(anchor),
            strict,
        )?;
        return Ok(FullHistoryPoint {
            point_match: full.point_match,
            base_y: full.base_y,
            absolute_row_revision: full.absolute_row_revision,
            content_seq: full.content_seq,
            cols: full.cols,
            consistent: full.consistent,
        });
    };

    let point_match = index
        .find_direction_opts(
            query,
            case_sensitive,
            is_regex,
            aterm_search::DirectedFind {
                anchor: wrap.logical_anchor.unwrap_or(anchor),
                direction,
                inclusive: !strict,
                wrap: true,
            },
        )?
        .map(|found| wrap.to_physical(&found));
    let consistent = {
        let terminal = term_lock(term);
        terminal.is_alternate_screen() == key_alt
            && terminal.content_seq() == key_seq
            && terminal.absolute_row_revision() == absolute_row_revision
    };
    Ok(FullHistoryPoint {
        point_match,
        base_y,
        absolute_row_revision,
        content_seq: key_seq,
        cols: wrap.cols,
        consistent,
    })
}

/// The search index depth cap (config `search_history_lines`): the maximum number
/// of the newest addressable lines the trigram index retains before evicting the
/// oldest. Defaults to [`DEFAULT_MAX_CACHED_LINES`]; both the GUI ⌘F find and the
/// socket `search` verb read it through [`search_full_history`], so they always
/// build and share an identically-bounded index.
static SEARCH_MAX_LINES: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_CACHED_LINES);

/// Set the search index depth cap from config (`search_history_lines`). A change
/// invalidates the cached index (the cap is part of its key), so the next search
/// rebuilds at the new depth. Clamped to at least 1 line.
pub(crate) fn set_search_max_lines(max: usize) {
    SEARCH_MAX_LINES.store(max.max(1), Ordering::Relaxed);
}

/// The current search index depth cap (see [`set_search_max_lines`]).
fn search_max_lines() -> usize {
    SEARCH_MAX_LINES.load(Ordering::Relaxed)
}

/// Test-only serialization for the PROCESS-GLOBAL search depth cap. `SEARCH_MAX_LINES`
/// is shared across the whole test binary, so a test that lowers it to exercise eviction
/// would corrupt any scrollback-searching test running on another thread. Every test that
/// mutates the cap OR searches beyond the visible screen holds this guard for the duration,
/// serializing them; the mutator also saves/restores the prior value. Poison-tolerant (a
/// panicked holder left the cap in a known-restored or about-to-be-overwritten state).
#[cfg(test)]
pub(crate) fn search_cap_test_guard() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Grid rows copied per Terminal-lock hold while snapshotting for the search
/// index (see [`cmd_search`]): bounds each hold so the PTY reader and the
/// render path interleave with a full-scrollback snapshot instead of stalling
/// behind it.
const SNAPSHOT_CHUNK_LINES: usize = 1024;

#[cfg(test)]
thread_local! {
    static LAST_SEARCH_SNAPSHOT_LINES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn record_search_snapshot_lines(lines: usize) {
    LAST_SEARCH_SNAPSHOT_LINES.with(|count| count.set(lines));
}

#[cfg(not(test))]
fn record_search_snapshot_lines(_lines: usize) {}

#[cfg(test)]
fn take_last_search_snapshot_lines() -> usize {
    LAST_SEARCH_SNAPSHOT_LINES.with(|count| {
        let value = count.get();
        count.set(0);
        value
    })
}

/// One entry in the bounded per-terminal search-index cache.
///
/// `term` is a `Weak` used ONLY as an identity key: it pins the Arc control
/// block so a dead terminal's address cannot be recycled into a false pointer
/// match, and `Weak::as_ptr` never dereferences.
struct SearchSnapshot {
    term: Weak<Mutex<Terminal>>,
    alt_screen: bool,
    content_seq: u64,
    absolute_row_revision: u64,
    /// Depth cap the `index` was built under (see [`set_search_max_lines`]); part
    /// of the cache key so a config reload that changes it forces a rebuild.
    max_lines: usize,
    /// Absolute row at the top of the visible grid in this generation. A stale
    /// index refresh begins here because any of these rows could have been
    /// edited in place before later scrolling into immutable history.
    visible_base: usize,
    /// Exclusive absolute row already indexed in this snapshot.
    indexed_end: usize,
    index: Arc<TerminalSearch>,
}

/// Keep the frontmost working set hot without allowing unbounded terminal
/// lifetime retention. Eight immutable indexes covers normal split/tab use;
/// weak terminal identities let dead sessions be pruned eagerly.
const SEARCH_SNAPSHOT_CAPACITY: usize = 8;
static SEARCH_SNAPSHOTS: Mutex<VecDeque<SearchSnapshot>> = Mutex::new(VecDeque::new());

/// Deterministically force a mutation between a cache-hit query and its
/// endpoint validation. Targeted by terminal identity so unrelated parallel
/// search tests cannot consume the hook.
#[cfg(test)]
static SEARCH_CACHE_HIT_MUTATION: Mutex<Option<Weak<Mutex<Terminal>>>> = Mutex::new(None);

#[cfg(test)]
fn arm_search_cache_hit_mutation(term: &Arc<Mutex<Terminal>>) {
    *SEARCH_CACHE_HIT_MUTATION
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(term));
}

#[cfg(test)]
fn maybe_mutate_search_cache_hit(term: &Arc<Mutex<Terminal>>) {
    let target = {
        let mut hook = SEARCH_CACHE_HIT_MUTATION
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if hook
            .as_ref()
            .is_some_and(|target| Weak::as_ptr(target) == Arc::as_ptr(term))
        {
            hook.take()
        } else {
            None
        }
    };
    if let Some(target) = target.and_then(|target| target.upgrade()) {
        term_lock(&target).process(b"cache-race");
    }
}

fn search_cache_lock() -> MutexGuard<'static, VecDeque<SearchSnapshot>> {
    // A panicked holder leaves valid (if possibly stale) data; stale is safe —
    // the key check rejects it and the next miss overwrites the entry.
    SEARCH_SNAPSHOTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn cached_search_index(
    term: &Arc<Mutex<Terminal>>,
    alt_screen: bool,
    content_seq: u64,
    absolute_row_revision: u64,
    max_lines: usize,
) -> Option<Arc<TerminalSearch>> {
    let mut cache = search_cache_lock();
    cache.retain(|snapshot| snapshot.term.strong_count() != 0);
    let position = cache.iter().position(|snapshot| {
        std::ptr::eq(snapshot.term.as_ptr(), Arc::as_ptr(term))
            && snapshot.alt_screen == alt_screen
            && snapshot.content_seq == content_seq
            && snapshot.absolute_row_revision == absolute_row_revision
            && snapshot.max_lines == max_lines
    })?;
    let snapshot = cache.remove(position)?;
    let index = Arc::clone(&snapshot.index);
    cache.push_back(snapshot);
    Some(index)
}

/// Take ownership of a stale same-terminal index for an incremental ordinary
/// output refresh. Revision changes (reflow/protected row splices), shrinking
/// coordinate ranges, or concurrent readers conservatively fall back to a
/// bounded suffix rebuild.
fn take_reusable_search_index(
    term: &Arc<Mutex<Terminal>>,
    alt_screen: bool,
    absolute_row_revision: u64,
    max_lines: usize,
    indexed_end: usize,
) -> Option<(TerminalSearch, usize)> {
    let mut cache = search_cache_lock();
    let position = cache.iter().position(|snapshot| {
        std::ptr::eq(snapshot.term.as_ptr(), Arc::as_ptr(term))
            && snapshot.alt_screen == alt_screen
            && snapshot.absolute_row_revision == absolute_row_revision
            && snapshot.max_lines == max_lines
            && snapshot.indexed_end <= indexed_end
            && Arc::strong_count(&snapshot.index) == 1
    })?;
    let snapshot = cache.remove(position)?;
    let previous_visible_base = snapshot.visible_base;
    Arc::try_unwrap(snapshot.index)
        .ok()
        .map(|index| (index, previous_visible_base))
}

fn store_search_snapshot(snapshot: SearchSnapshot) {
    let mut cache = search_cache_lock();
    cache.retain(|existing| {
        existing.term.strong_count() != 0
            && !std::ptr::eq(existing.term.as_ptr(), snapshot.term.as_ptr())
    });
    if cache.len() >= SEARCH_SNAPSHOT_CAPACITY {
        cache.pop_front();
    }
    cache.push_back(snapshot);
}

/// `custody` -> ONE status line answering "why did my selection disappear?".
///
/// `OK last=<transition|none> event=<0-7|-> changed=<transition|none>
/// took_selection=<transition|none> offset=<n>
/// owner=<user|tail> selection=<yes|no> scrollback=<n>`.
///
/// `scrollback` is the grid's retained history depth. It was called `max_offset`,
/// which read as the `PressCustody` model's constant `MaxOffset` — a bound of 2 in
/// the abstract state space, not a line count — in a line whose other fields the doc
/// invites clients to read against the spec's vocabulary. A driving client comparing
/// the two would have been comparing a live terminal against a model constant.
///
/// The verb exists because offset and selection are observable after the fact and the
/// DECISION that moved them is not. Ten different events can take the reading position
/// or the highlight — typing, an auto-repeat tick, a bare modifier, a key release, a
/// wheel notch, a drag, a deselecting click, output re-pinning, output replacing the
/// selected rows, and an ED 3 / RIS that destroys the coordinate space — and several of
/// them leave IDENTICAL state behind, so no amount of reading `selection` and `scroll`
/// can tell them apart. `last` is the engine's own record of which one it was, written
/// by the site that made the decision.
///
/// `changed` is the last transition that actually TOOK something — the offset moved,
/// or a live highlight died. It is a separate slot because `last` alone cannot answer
/// the question the verb exists for: the record's most frequent writer is
/// `OutputAtLive`, an identity transition that every shell prompt, every `cat` and
/// every `tail -f` line writes, so a human typing `custody` a second after their
/// selection vanished reads back the last line of shell output. Press Enter and the
/// shell's newline overwrites `last=TypingPress` before the user can finish the word.
/// `changed` survives that, and the two together show both the raw sequence and the
/// event being asked about. When the two agree the last event is also the last one
/// that took anything.
///
/// `event` is the same fact as the `PressCustody` model's `last_event` tag, so a
/// driving client can compare a live terminal against the spec's own vocabulary:
/// 0 a user gesture, 1 typing, 2 an auto-repeat tick, 3 a bare modifier, 4 a release,
/// 5 output that missed the selected rows, 6 output that REPLACED them, 7 output that
/// invalidated the coordinate space. It reads `-` for
/// `OutputTookTheSelectionUnattributed`, which is a real answer with NO model action:
/// output took the highlight for one of the five reasons `post_process` cannot
/// attribute to a damage band, so the model has no name for it and the tag space has
/// no room for it. That is still strictly better than the `last=none` this case used
/// to print, which was indistinguishable from a terminal that had never done
/// anything — and it rules out the keyboard, which is what the user wanted to know.
///
/// `owner` is DERIVED from the offset (`TailOwnerAtBottom`: an offset above the tail
/// means the user owns the viewport), never carried separately — the same derivation
/// the Tier-1 conformance uses, for the same reason: a self-reported ownership flag
/// would agree with itself no matter what the viewport did.
///
/// Read-only; it reports no screen content, so it says nothing about what was typed
/// or what was selected — only what class of event last moved custody.
pub(crate) fn cmd_custody(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let offset = t.grid().display_offset();
    let (last, event) = t.last_custody_transition().map_or_else(
        || ("none".to_string(), "-".to_string()),
        |c| {
            let tag = c.last_event();
            // A negative tag is deliberately outside the model's `0..=7` space — see
            // `CustodyTransition::OutputTookTheSelectionUnattributed`.
            let event = if tag < 0 {
                "-".to_string()
            } else {
                tag.to_string()
            };
            (c.action().to_string(), event)
        },
    );
    let changed = t
        .last_custody_change()
        .map_or_else(|| "none".to_string(), |c| c.action().to_string());
    // The field the verb is actually FOR. `changed` means "the last thing that moved
    // custody", so a deliberate deselect overwrites it — and then "why did my
    // highlight vanish?" answers "you cleared it", which is true of the click and
    // useless about the loss. `took_selection` survives later activity so the
    // explanation is still there when someone gets round to asking.
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

/// `modes` -> `OK\n` then one `key=value` line per introspected mode:
/// `alt_screen`, `cursor_visible`, `app_cursor_keys` (DECCKM),
/// `app_keypad` (DECPAM), `bracketed_paste` (2004), `mouse_mode`
/// (`none|normal|button|any|x10`), and `mouse_encoding`
/// (`x10|utf8|sgr|urxvt|sgr_pixel`).
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
    // Keyboard input protocol state — which encoding a key press will use. Lets a
    // driving client (or a human debugging "why doesn't Shift+Enter work?") SEE
    // whether the running app negotiated the Kitty keyboard protocol or xterm
    // modifyOtherKeys. `kitty_keyboard` lists the active progressive-enhancement
    // flags (csv, or `none`); `modify_other_keys` is the xterm level 0/1/2.
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
    // Framed as `OK <n>` + n lines so the client streams the body (same shape
    // as `text`/`search`), rather than truncating to the status line.
    let lines = [
        format!("alt_screen={}", t.is_alternate_screen()),
        format!("cursor_visible={}", t.cursor_visible()),
        format!("app_cursor_keys={}", m.application_cursor_keys),
        format!("app_keypad={}", m.application_keypad),
        format!("bracketed_paste={}", m.bracketed_paste),
        format!("mouse_mode={mouse_mode}"),
        format!("mouse_encoding={mouse_encoding}"),
        // Affect how typed input / printed output lands, so a client driving the
        // terminal can predict behavior: IRM (insert vs overwrite), DECAWM
        // (auto-wrap at the right margin), DECOM (cursor origin = scroll region).
        format!("insert_mode={}", m.insert_mode),
        format!("auto_wrap={}", m.auto_wrap),
        format!("origin_mode={}", m.origin_mode),
        // Keyboard input protocol (see above): observe protocol negotiation live.
        format!("kitty_keyboard={kitty_str}"),
        format!("modify_other_keys={modify_other_keys}"),
    ];
    let mut out = format!("OK {}\n", lines.len());
    for l in &lines {
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// `title` -> `OK <window title>\n` (the OSC 0/2 window title; empty if unset).
/// `appstatus` -> the STATUS-SURFACE ledger (design §3): one `activity` line per
/// LIVE status-bar row, then one per finished activity the ring still holds,
/// oldest first.
///
/// Presentation is ephemeral and the record is not: a bar folds and its sentence
/// leaves the glass, but "what has this app been doing on its own initiative"
/// is exactly the question an operator — or a driving agent that was not looking
/// at the window — asks afterwards. Read-only, and every free-text field is
/// percent-encoded through the same `pct_encode` the `status`/`meta` verbs use,
/// so a program name or an error sentence can never break the line grammar.
///
/// The rows are rendered on the MAIN THREAD (`Wake::ReadAppStatus`) because the
/// state is App state; this function only frames the reply.
pub(crate) fn cmd_appstatus(proxy: &EventLoopProxy<Wake>) -> String {
    match control_media::call_main(proxy, |reply| Wake::ReadAppStatus { reply }) {
        Ok(rows) if rows.is_empty() => "OK 0\n".to_string(),
        Ok(rows) => format!("OK {}\n{}\n", rows.len(), rows.join("\n")),
        Err(error) => format!("ERR {error}\n"),
    }
}

pub(crate) fn cmd_title(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    format!("OK {}\n", t.title())
}

/// `cwd` -> `OK <working directory>\n` (the shell's directory as reported via
/// OSC 7; empty if never reported). Lets an introspecting client know where
/// commands will run without scraping the prompt.
pub(crate) fn cmd_cwd(term: &Arc<Mutex<Terminal>>) -> String {
    use crate::cwd_native::ReportedCwd as _;
    let t = term_lock(term);
    // A client asks this verb where to run a command, so it must answer in the
    // host platform's own path syntax: the engine keeps OSC 7's RFC 8089 URI
    // path, and `OK /C:/Users//m6-an` is not a Windows directory anyone can pass
    // to `cd` or to a process spawn. The conversion happens once, in
    // `cwd_native`, so this verb and `meta` cannot drift apart.
    let cwd = t.native_working_directory();
    // The cwd is percent-decoded from OSC 7, so it can hold raw newlines / C0 /
    // C1 / BiDi-override bytes. pct_encode it (as sibling verbs do) so it stays a
    // single token and cannot forge extra control-protocol reply lines.
    format!("OK {}\n", pct_encode(cwd.as_deref().unwrap_or("")))
}

/// `text --json` -> `{"rows":["<row0>",...],"cursor":{...},"seq":N,"dims":{...}}`.
/// The rows are the SAME grapheme-faithful, control-collapsed, tail-trimmed lines
/// `cmd_text` emits, the cursor/dims mirror the `cursor`/`dims` verbs, and `seq`
/// is the engine `content_seq` (so an agent can diff frames without re-reading).
/// The bare form of [`cmd_text_json_opt`] — test-only, like [`cmd_text`], since the
/// dispatch passes its tail through the `_opt` form (the name stays so the
/// `json_ok_sites_match_the_json_capable_verbs` scrape still binds `text`).
#[cfg(test)]
pub(crate) fn cmd_text_json(term: &Arc<Mutex<Terminal>>) -> String {
    cmd_text_json_opt(term, false)
}

/// `text --json [trim]`: [`cmd_text_json`], and with `trim` the `rows` array stops
/// after the last non-blank row ([`trimmed_len`]) and the object closes with
/// `"trimmed":k` — the JSON twin of the text header's `trimmed=<k>`. `dims.rows`
/// stays the GRID's row count, so `rows.len()` says what was sent and `dims` what
/// the screen is; the field is only written when trimming was asked for, keeping
/// the bare reply byte-identical.
pub(crate) fn cmd_text_json_opt(term: &Arc<Mutex<Terminal>>, trim: bool) -> String {
    // GATHER under ONE lock hold, SERIALIZE with the lock released — the shape the
    // styled frame already uses. Every field is read inside the single hold, so the
    // reply still describes one instant; the escaping and JSON assembly are pure
    // string work over owned data, and doing them under the mutex made the PTY
    // reader's `process()` and the frame snapshot queue behind a screen read.
    let (rows_text, c, vis, style, rows, cols, seq) = {
        let t = term_lock(term);
        let rows = t.rows() as usize;
        let rows_text: Vec<String> = (0..rows).map(|r| visible_row(&t, r)).collect();
        (
            rows_text,
            t.cursor(),
            t.cursor_visible(),
            cursor_style_name(t.cursor_style()),
            rows,
            t.cols(),
            t.content_seq(),
        )
    };
    // ONE buffer, written straight through. The retired shape allocated a quoting
    // `format!` + a `json_escape` per row, then copied the WHOLE payload three more
    // times (the `join`, the outer `format!`, and `json_ok`'s own `format!`).
    // Byte-identical: `json_escape` is `json_escape_into` into a fresh String, and
    // `json_ok` is exactly this `"OK 1\n"` prefix + `"\n"` suffix.
    let sent = if trim {
        trimmed_len(rows_text.iter().map(String::as_str))
    } else {
        rows
    };
    let mut out = String::with_capacity(rows * (cols as usize + 8) + 128);
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
             \"dims\":{{\"rows\":{rows},\"cols\":{cols}}},\"seq\":{seq}",
            c.row,
            c.col,
            json_str_field("style", style),
        );
        if trim {
            let _ = write!(out, ",\"trimmed\":{}", rows.saturating_sub(sent));
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
/// NOTE on semantic boundary: `dim`/`blink`/`inverse`/`hidden` are baked into the
/// resolved `fg`/`bg` by `render_row` and are deliberately NOT reported as attrs
/// (recovering them is the raw-flags path; byte-exact SGR replay is the `cast`
/// raw-bytes channel's job, not this resolved-screen view).
///
/// Serializes from a [`StyledCellSnap`] gathered under the terminal lock, so the
/// per-cell `format!` — the bulk of the frame cost — runs with the lock released.
/// Lowercase two-hex-digit bytes for one RGB triple, table-driven.
///
/// `write!("{:02x}")` routes through `core::fmt`'s dynamic machinery — trait
/// objects, a width/fill state machine — for what is ultimately two array
/// lookups. The lossless frame writes SIX of these per cell (fg, bg, and the
/// optional underline colour), so on a large window that is ~90 000 formatter
/// invocations per snapshot. Same bytes, none of the machinery.
fn push_hex_rgb(out: &mut String, rgb: [u8; 3]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in rgb {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
}

/// A JSON boolean without the formatter (four per cell).
fn push_bool(out: &mut String, v: bool) {
    out.push_str(if v { "true" } else { "false" });
}

/// One lossless cell, APPENDED to `out` — byte-identical to the allocating twin
/// above, with none of its allocations.
///
/// This is the hot loop of the whole introspection surface: `screen` and every
/// `subscribe … cells` push run it once per cell, up to ~15 000 per snapshot on
/// a large window, on every change, for every subscriber. The retired version
/// spent ~6 heap allocations per cell before it wrote a byte — three of them
/// (`underline_color`, `hyperlink`, `hyperlink_id`) existing only to spell the
/// literal `null`, plus a `Vec<&str>` of attribute names, a `String` per
/// attribute, and a `String` for the escaped glyph — then the result was copied
/// into a per-row `String` and again into the frame. Writing straight into the
/// destination removes all of it; the bytes are unchanged.
///
/// `glyph` is borrowed from its row's shared grapheme buffer
/// ([`StyledRowSnap::glyph`]) instead of being read off the cell, so the GATHER
/// no longer owns a `String` per cell either.
fn write_styled_cell_json(out: &mut String, snap: &StyledCellSnap, glyph: &str) {
    let cell = &snap.cell;
    out.push_str("{\"glyph\":\"");
    crate::control::json_escape_into(out, glyph);
    out.push_str("\",\"fg\":\"");
    push_hex_rgb(out, cell.fg);
    out.push_str("\",\"bg\":\"");
    push_hex_rgb(out, cell.bg);
    out.push_str("\",\"attrs\":[");
    // The attribute list, comma-joined in place (no Vec, no per-name String).
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
            push_hex_rgb(out, rgb);
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
    out.push_str(",\"hyperlink\":");
    match snap.hyperlink.as_deref() {
        Some(u) => {
            out.push('"');
            crate::control::json_escape_into(out, u);
            out.push('"');
        }
        None => out.push_str("null"),
    }
    out.push_str(",\"hyperlink_id\":");
    match snap.hyperlink_id.as_deref() {
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
/// scales these rows, so a lossless frame must carry them (audit finding F2).
fn line_size_name(s: aterm_core::grid::LineSize) -> &'static str {
    use aterm_core::grid::LineSize;
    match s {
        LineSize::SingleWidth => "single",
        LineSize::DoubleWidth => "double_width",
        LineSize::DoubleHeightTop => "double_height_top",
        LineSize::DoubleHeightBottom => "double_height_bottom",
        _ => "single",
    }
}

/// The DEC line size of visible row `r` (default single-width).
fn row_line_size(t: &Terminal, r: usize) -> aterm_core::grid::LineSize {
    u16::try_from(r)
        .ok()
        .and_then(|rr| t.grid().row(rr))
        .map_or(aterm_core::grid::LineSize::SingleWidth, |row| {
            row.line_size()
        })
}

/// Every DISTINCT inline image on the visible grid, each at its top-left grid
/// anchor `(row, col)` (deduplicated by payload identity). Shared shape with
/// `cmd_image_read`'s screen mode; consumed by the styled frame (audit finding F1)
/// so a `subscribe cells` / `screen` watcher sees images, not blank cells.
///
/// ROW-MAJOR, ascending column within a row: a socket client indexes into this
/// array, so the order is part of the wire contract
/// (`distinct_images_are_row_major_regardless_of_placement_order` pins it) and
/// NOT free to follow the extras map, whose `FxHashMap` iteration order is
/// arbitrary and is not insertion order either.
///
/// [`images_frame`](Terminal::images_frame) is the batch reader: ONE pass over
/// the extras map, each row sorted by column, instead of the `rows x cols`
/// `cell_extra` probes the per-row accessor costs. Its emptiness gate only
/// rescues a screen with NO extras at all, so before this every gather of a
/// screen carrying a single hyperlink — an `ls --hyperlink` listing, a TUI
/// status line — swept the whole grid looking for pictures that were not there.
///
/// The extras store's `any_image()` flag is deliberately NOT used as a second
/// gate: a Kitty Unicode placeholder shows its picture through
/// `transient.kitty_images` without ever attaching an image ref to a cell, so
/// that flag is false on screens that really do show images.
fn distinct_images(t: &Terminal) -> Vec<(usize, usize, std::sync::Arc<ImageData>)> {
    let mut seen: Vec<*const ImageData> = Vec::new();
    let mut out: Vec<(usize, usize, std::sync::Arc<ImageData>)> = Vec::new();
    for (r, row) in t.images_frame(t.rows() as usize).into_iter().enumerate() {
        for (col, iref) in row {
            let ptr = std::sync::Arc::as_ptr(&iref.image);
            if seen.contains(&ptr) {
                continue;
            }
            seen.push(ptr);
            let anchor_r = r.saturating_sub(iref.cell_row as usize);
            let anchor_c = col.saturating_sub(iref.cell_col as usize);
            out.push((anchor_r, anchor_c, iref.image.clone()));
        }
    }
    out
}

/// One inline image as a JSON object for the styled frame: anchor grid position,
/// cell footprint, format, raw byte length, and the base64 payload — so a watcher
/// reconstructs the picture the human sees, independent of the GUI framebuffer.
pub(crate) fn styled_image_json(anchor_r: usize, anchor_c: usize, img: &ImageData) -> String {
    let (fmt, b64) = image_payload(img); // F4: oversized -> ("truncated", "")
    format!(
        "{{\"row\":{anchor_r},\"col\":{anchor_c},\"cols\":{},\"rows\":{},\"format\":\"{fmt}\",\
         \"nbytes\":{},\"b64\":\"{b64}\"}}",
        img.cols,
        img.rows,
        img.bytes.len(),
    )
}

/// One cell's raw material for [`serialize_styled_frame`]: the resolved
/// [`RenderCell`] plus the three grid-side fields it does not carry — the
/// combining-aware grapheme, the raw `WIDE` lead flag (the live cell's own flag
/// bit, NOT the resolved `RenderCell::wide`), and the OSC 8 hyperlink target. All
/// copied under the terminal lock; serialized without it.
struct StyledCellSnap {
    cell: RenderCell,
    /// Byte range of this cell's grapheme inside its row's
    /// [`StyledRowSnap::glyphs`] buffer — read back through
    /// [`StyledRowSnap::glyph`], never on its own.
    glyph: std::ops::Range<usize>,
    wide_lead: bool,
    hyperlink: Option<String>,
    /// The OSC-8 `id=` grouping key, if the link carried one. Two adjacent runs with
    /// the SAME url but DIFFERENT id are DISTINCT clickable regions; the real GUI
    /// renderer groups hover/click spans by id, so a lossless mirror needs it to
    /// reproduce the grouping — the url alone cannot. `None` when the link had no id.
    hyperlink_id: Option<String>,
}

/// One row's cells plus the ONE buffer their graphemes live in.
///
/// The retired shape gave every cell its own `String`: rows x cols heap
/// allocations per gather — 1,920 on a 24x80 screen, 10,000 on 50x200 — and the
/// gather runs with the terminal lock HELD, so an agent polling `screen` (or a
/// `subscribe … cells` watcher waking on every frame) stalled the engine for all
/// of them. Even a blank cell allocated: the grapheme of a materialized blank is
/// a single SPACE, not the empty string.
///
/// The graphemes are written once and read once, by one serializer, in column
/// order — so one buffer per row plus a byte range per cell carries them for ONE
/// allocation per row. The buffer lives INSIDE the row rather than in a parallel
/// `Vec<String>` on the frame, and [`StyledRowSnap::glyph`] takes a COLUMN
/// rather than a cell — together those make a range-against-the-wrong-row
/// pairing something a caller cannot spell, not merely something unlikely.
struct StyledRowSnap {
    /// Every cell's grapheme in this row, concatenated in column order.
    glyphs: String,
    cells: Vec<StyledCellSnap>,
}

impl StyledRowSnap {
    /// Column `col`'s grapheme — byte-for-byte the `String` the retired per-cell
    /// field held. Takes the COLUMN, not a `&StyledCellSnap`, so a cell borrowed
    /// from some other row cannot be paired with this row's buffer — the caller
    /// never holds a range without the buffer it indexes. Indexing (not `get`)
    /// on purpose: the ranges are handed out by the gather as it appends to this
    /// very buffer, so a range outside it is a code defect that must not be able
    /// to hide as an empty glyph on the wire.
    fn glyph(&self, col: usize) -> &str {
        &self.glyphs[self.cells[col].glyph.clone()]
    }
}

/// Side-adjusted selection geometry for the styled control frame.
///
/// Bare `normalized_bounds()` cannot describe a selection: it loses the kind (a
/// block and a linear selection can share endpoints and paint different cells)
/// and the anchors' half-cell sides. This carries `TextSelection::project_range`
/// plus the kind, which recovers both.
///
/// It is the LOGICAL span, NOT a cell-for-cell mirror of the paint.
/// `project_range` sees anchors only, so it applies the renderer's half-cell
/// side adjustment and line expansion but cannot apply the content-dependent
/// widening a double-width glyph forces (`glyph_cell_span`): an edge resting on
/// half such a glyph paints the other half too, one column outside
/// `start_col`/`end_col`. A watcher that needs the painted cells reads the
/// frame's `cells` array and its `wide_lead` bits instead.
struct StyledSelectionSnap {
    start_row: i32,
    start_col: u16,
    end_row: i32,
    end_col: u16,
    kind: &'static str,
    is_block: bool,
}

/// Everything the styled JSON frame serializes, copied out of the [`Terminal`]
/// under ONE lock acquisition — the internal-consistency contract: seq, cursor,
/// cells, images, selection, and line sizes all describe the same instant.
/// Serialization (the per-cell `format!`s, the joins, and the per-image base64 —
/// the bulk of the old 10-25 ms in-lock cost) then runs with the lock RELEASED,
/// so keystroke encode and frame snapshots stop queueing behind a watcher.
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
    line_sizes: Vec<&'static str>,
    /// The selection's LOGICAL span and kind — see [`StyledSelectionSnap`] for
    /// where it parts company with the painted cells — or `None`.
    selection: Option<StyledSelectionSnap>,
    /// OSC 17 selection fill, or `None` for the renderer/theme default.
    selection_bg: Option<[u8; 3]>,
    /// OSC 19 selected-text ink, or `None` for automatic contrast.
    selection_fg: Option<[u8; 3]>,
    /// Distinct inline images as `Arc` clones; base64 happens at serialize time.
    images: Vec<(usize, usize, std::sync::Arc<ImageData>)>,
}

impl StyledFrameSnapshot {
    /// Cells in the gathered frame — `rows × cols` by the lossless no-trim
    /// contract. The serializer's reserve input, and the bench seam's REACH
    /// guard (a fixture that failed to paint, or a frame that trimmed, must not
    /// be priced as a fast one).
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

/// Serialize renderer-equivalent selection geometry while preserving the four
/// historical coordinate keys first for additive wire compatibility.
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

/// Gather the styled frame's raw material. Called with the `Terminal` lock HELD
/// (one acquisition covers every field). The blank-tail fallback mirrors
/// [`cmd_cell`] exactly: rows are padded to the FULL grid width with the
/// terminal's effective implicit blank (live defaults with DECSCNM folded in;
/// NO `trim_end`, unlike `text`) — the lossless `dims.rows × dims.cols`
/// contract.
pub(crate) fn gather_styled_frame(t: &Terminal) -> StyledFrameSnapshot {
    let rows = t.rows() as usize;
    let cols = t.cols() as usize;
    let blank = t.implicit_blank_render_cell();
    let mut cells: Vec<StyledRowSnap> = Vec::with_capacity(rows);
    let mut line_sizes: Vec<&'static str> = Vec::with_capacity(rows);
    // Both hyperlink fields come from ONE `Grid::cell_extra` entry, and that
    // lookup is `CellExtras::get` — a probe of the extras MAP alone, which
    // `is_empty()` reports on directly. So with no extras anywhere on screen the
    // per-cell probe is provably `None` for every cell, and the whole rows*cols
    // hash-probe sweep collapses to this single bool (the same gate
    // `images_row_into` and `stale_extras_pair` already use). `t` is borrowed
    // immutably for the gather, so the map cannot change underneath it.
    let any_extras = !t.grid().extras().is_empty();
    for r in 0..rows {
        // LIVE frame (offset-INDEPENDENT): colours+attrs from `render_row_at_screen`,
        // glyph from the live `cell_grapheme`, wide-lead from the live raw grid flag
        // (`RenderCell::wide` means the RIGHT-HALF continuation), and the
        // offset-blind extras probe (exactly what `hyperlink_at`/`hyperlink_id_at`
        // do internally) — so all four reads share one live frame and never stitch
        // a scrolled-back row's colours onto a live glyph.
        let rendered = t.render_row_at_screen(r);
        // ONE live-row handle for the whole row. `screen_row_view` is the
        // offset-INDEPENDENT twin of `visible_row_view` (and resolves through the
        // same `row_at_screen` the retired per-column `cell_attrs` call did), so the
        // live-frame contract above is unchanged — it is just resolved once per row
        // instead of once per column.
        let view = t.grid().screen_row_view(r as u16);
        let mut row_cells: Vec<StyledCellSnap> = Vec::with_capacity(cols);
        // ONE grapheme buffer for the whole row; each cell keeps its byte range
        // into it (see `StyledRowSnap`). `cols` bytes is the EXACT size of an
        // ASCII row and a tight lower bound otherwise, so an ordinary row lands
        // in a single allocation instead of doubling up from zero — the same
        // reserve rule `visible_row_bounds_to_string` uses before its identical
        // per-column push loop.
        let mut glyphs = String::with_capacity(cols);
        for c in 0..cols {
            let rc = rendered.get(c).copied();
            // ONE extras probe for both hyperlink fields: `hyperlink_at` and
            // `hyperlink_id_at` are literally this same `cell_extra` lookup, read
            // twice, and both fields fall out of the one `&CellExtra`.
            let extra = if any_extras {
                t.grid().cell_extra(r as u16, c as u16)
            } else {
                None
            };
            // Byte-for-byte the retired `cell_grapheme(r, c).unwrap_or_default()`
            // — the same `push_cell_text` produces both — with the per-cell
            // `String` replaced by a range into the row buffer. An out-of-range
            // cell appends nothing, which is that call's `unwrap_or_default()`
            // empty string; in-range is all this loop can reach anyway (`rows`
            // and `cols` are the grid's own).
            let glyph_start = glyphs.len();
            t.cell_grapheme_into(r, c, &mut glyphs);
            row_cells.push(StyledCellSnap {
                cell: rc.unwrap_or(blank),
                glyph: glyph_start..glyphs.len(),
                // Raw WIDE lead bit straight off the live cell. Identical to the
                // retired `cell_attrs(..).contains(WIDE)`, not an approximation:
                // `attrs_to_cell_flags` can never emit WIDE (`StyleAttrs` carries no
                // width bit), so in the style-interned branch WIDE could only have
                // come from the cell's own inline flags — the very bit read here —
                // and a missing cell yields `false` on both paths.
                wide_lead: view.cell(c as u16).is_some_and(|cell| cell.is_wide()),
                hyperlink: extra.and_then(|e| e.hyperlink()).map(|u| u.to_string()),
                hyperlink_id: extra.and_then(|e| e.hyperlink_id()).map(|u| u.to_string()),
            });
        }
        cells.push(StyledRowSnap {
            glyphs,
            cells: row_cells,
        });
        line_sizes.push(line_size_name(row_line_size(t, r))); // F2: DEC double-width/height
    }
    // F3: text selection highlight (a human/peer-initiated selection a watcher
    // would otherwise miss). `project_range` applies the renderer's half-cell
    // side adjustment and line expansion; the explicit kind preserves
    // block-vs-linear geometry when endpoints happen to match.
    //
    // Everything anchors can express, and nothing more: this is the LOGICAL
    // span, not a cell-for-cell mirror of the paint. An edge resting on half a
    // double-width glyph paints its other half too (`glyph_cell_span`), which is
    // content-dependent and so cannot be read off anchors at all. A watcher that
    // needs the painted cells has the `cells` array beside this and its
    // `wide_lead` bits.
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
        line_sizes,
        selection,
        selection_bg: t
            .selection_background()
            .map(|color| [color.r, color.g, color.b]),
        selection_fg: t
            .selection_foreground()
            .map(|color| [color.r, color.g, color.b]),
        // F1: distinct inline images at their anchors — `Arc` clones only; the
        // (up to multi-MiB per image) base64 encode is deferred off the lock.
        images: distinct_images(t),
    }
}

/// Serialize a gathered [`StyledFrameSnapshot`] to the single-line JSON frame.
/// Lock-free by construction (the snapshot owns everything it reads); all
/// payload-affecting terminal state was captured under the earlier single lock.
pub(crate) fn serialize_styled_frame(snap: &StyledFrameSnapshot) -> String {
    use std::fmt::Write as _;
    // ONE buffer for the whole frame — now literally one. The retired shape
    // built a `String` per cell, joined those into a `String` per row, then
    // joined the rows into the frame; the intermediate `rows_json` that survived
    // that cleanup still reserved a SECOND full-size buffer (~180 bytes/cell)
    // and memcpy'd the entire rows payload into `out` at the end — 864 KB
    // allocated twice and copied once on a 120x40 grid, per pushed frame.
    // Reserving up front and writing straight through removes all of it.
    // ~180 bytes/cell is the measured shape of an ordinary cell; a short read is
    // just one realloc, never wrong.
    let cell_count = snap.cell_count();
    let mut out = String::with_capacity(256 + cell_count * 180);
    // Every row-INDEPENDENT field is built first so the header, the streamed
    // rows, and the tail can be emitted in that order into the single buffer.
    // These are all tiny (a few bytes to a few hundred) and order-independent
    // pure functions of the snapshot; only the rows payload is large.
    let line_sizes_json = snap
        .line_sizes
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(",");
    // F1: inline images (OSC 1337), base64 once per distinct image at its anchor —
    // without this a `cells`/`screen` watcher sees blank cells where the human sees
    // a picture. Empty array (cheap) on the common no-image screen.
    let images_json = snap
        .images
        .iter()
        .map(|(ar, ac, img)| styled_image_json(*ar, *ac, img))
        .collect::<Vec<_>>()
        .join(",");
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
            write_styled_cell_json(&mut out, cell, row.glyph(c));
        }
        out.push(']');
    }
    let _ = write!(
        out,
        "],\"line_sizes\":[{line_sizes_json}],\"selection\":{selection_json},\
         \"selection_bg\":{selection_bg},\"selection_fg\":{selection_fg},\
         \"images\":[{images_json}]}}",
    );
    out
}

/// Build the whole styled-screen frame as a single-line JSON object:
/// `{"seq":N,"dims":{...},"cursor":{...},"rows":[[StyledCell,...],...]}`.
///
/// Called with the `Terminal` lock ALREADY HELD (the subscribe `cells` stream
/// reuses it under one lock so the frame is internally consistent) — gather +
/// serialize in one call. Callers that CAN drop the lock between the two phases
/// (the `screen` verb) use [`gather_styled_frame`] / [`serialize_styled_frame`]
/// directly so the expensive serialization never blocks the mutex.
#[cfg_attr(not(test), allow(dead_code))] // test parity wrapper; prod splits the phases
pub(crate) fn styled_frame_payload(t: &Terminal) -> String {
    serialize_styled_frame(&gather_styled_frame(t))
}

/// COMPILE-GATE (the GENERAL fix for the F1/F2/F3 dropped-field class): every field
/// of the renderer's input MUST be a CONSCIOUS decision — reflected in the lossless
/// styled frame, or explicitly omitted with a reason. This destructures
/// [`RenderInput`](aterm_core::render::RenderInput) WITHOUT `..`, so adding a new
/// renderer-consumed field fails to compile until someone decides whether
/// `styled_frame_payload` carries it. That turns "we silently dropped a field"
/// (F1 images, F2 line_sizes, F3 selection — all present in `RenderInput`, all once
/// missing from the frame) into a build error. Never called; it exists to type-check.
#[allow(dead_code)]
fn _styled_frame_covers_every_render_input_field(ri: &aterm_core::render::RenderInput) {
    let aterm_core::render::RenderInput {
        rows: _,                         // frame "dims.rows"
        cols: _,                         // frame "dims.cols"
        cells: _,                        // frame "rows" (per-cell styled_cell_json)
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
        display_offset: _, // OMITTED: viewport scroll position, not visible-cell content
        base_y: _, // OMITTED: host-consumed re-anchor metadata (absolute row of the top line), not visible-cell content
        absolute_row_revision: _, // OMITTED: host-consumed absolute-row coordinate-space revision, not visible-cell content
        scroll_frac_px: _, // OMITTED: M1b sub-row present translate, display-only (not cell content)
        grid_top_row: _,   // OMITTED: M1b grid/chrome partition, display-only
        grid_bot_row: _,   // OMITTED: M1b grid/chrome partition, display-only
        fx_clip: _, // OMITTED: focused-pane present-time post-fx clip box (split-pane audit), display-only
        selection: _, // frame "selection" kind + projected geometry (F3)
        selection_clip: _, // OMITTED: host-only split composition bounds; engine styled frames have none
        selection_bg: _, // frame "selection_bg" (OSC 17 fixed RGB, else the "default" policy token)
        selection_fg: _, // frame "selection_fg" (OSC 19 fixed RGB, else the "dynamic" auto-contrast token)
        selections: _, // OMITTED: compose-time per-pane selection list, the twin of `selection_clip` above. A styled frame is extracted from ONE Terminal via `cell_frame_into`, which clears it, so the scalar `selection` above is always this frame's whole selection authority.
        clusters: _,   // folded into per-cell "glyph" (cell_grapheme)
        combining: _,  // folded into per-cell "glyph" (cell_grapheme)
        line_sizes: _, // frame "line_sizes" (F2)
        line_size_spans: _, // OMITTED: compose-time per-pane refinement of `line_sizes`. This frame is extracted from ONE Terminal, whose rows are uniform, so it is always empty here; the split-pane composite is not the styled-frame source.
        default_bg_spans: _, // OMITTED: compose-time per-pane refinement of `default_bg`, empty for a single-Terminal frame; each cell already carries its own resolved bg.
        images: _,           // frame "images" (F1)
        wallpaper: _, // OMITTED: host-owned backdrop base layer (render bling), not engine cell content
        default_bg: _, // OMITTED: engine-resolved live default-bg for padding, not per-cell content (cells carry their own bg)
        default_fg: _, // OMITTED: its twin — the effects layer's tint anchor, not per-cell content
        cursor_color: _, // frame "cursor.color" (fixed RGB or "default")
        snapshot_seq: _, // frame "seq" (the engine content version stamp)
        process_sequence: _, // OMITTED: parser-batch provenance used only by host cursor-effect admission
        input_hot: _, // OMITTED: present-time bloom-defer latency hint, display-only (not cell content)
        // OMITTED (DMG-1 damage carrier): extraction-CONTINUITY tokens, not
        // cell content. They answer "may this scratch's undamaged rows be
        // retained by the next damage-scoped refill" — a question about the
        // producer/scratch pair, meaningless to a client reading one frame.
        // They are excluded from `RenderInput`'s own `PartialEq` for the same
        // reason `snapshot_seq` is metadata rather than pixels; serializing
        // them would leak engine-internal bookkeeping into the wire format and
        // make byte-identical frames compare unequal.
        terminal_id: _,
        extract_gen: _,
        engine_fill_seq: _,
        engine_alt: _,
        engine_row_order: _,
        // D-2 per-row revision lane: the same law as the carrier tokens above.
        // It is the engine's "which rows changed" fact, not pixels — a wire
        // frame that carried it would report two byte-identical screens as
        // different every time the revision advanced.
        row_rev: _,
        row_rev_lane: _,
        // D-2 SPLICE bookkeeping: how many host chrome rows are prepended and
        // the provenance token that says the prepend is all that happened.
        // Host composition state, not the terminal model a wire frame reports.
        row_shift: _,
        shifted_fill_seq: _,
        // K2 composed-frame provenance: the same law again — it says whether a
        // COMPOSITOR's last write is still intact in a host scratch buffer,
        // which is not a fact about the terminal model.
        composed_fill_seq: _,
    } = ri;
}

/// `screen` -> the full LOSSLESS engine-styled grid as a single-line JSON frame, wrapped
/// in the standard `OK 1\n<json>\n` read framing (so the existing line-count
/// client streams it unchanged). It carries per-cell style, selection, cursor,
/// dimensions, and sequence state. Host-owned visual effects and decorations are
/// intentionally omitted, so this is a terminal-model projection rather than a
/// claim about the application framebuffer or display. `--json` is implied.
pub(crate) fn cmd_screen_styled_json(term: &Arc<Mutex<Terminal>>) -> String {
    // Gather under ONE lock hold (internally consistent frame), then serialize
    // with the lock RELEASED — the per-cell format!/base64 never blocks keystroke
    // encode or frame snapshots, which share this mutex.
    let snap = {
        let t = term_lock(term);
        gather_styled_frame(&t)
    };
    json_ok(&serialize_styled_frame(&snap))
}

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
         \"viewers\":{},\"visible_viewers\":{},\"geometry\":\"{}\",\
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

/// `colors` -> the terminal's theme colors:
/// `OK fg=<rrggbb> bg=<rrggbb> cursor=<rrggbb|default>`.
/// Programs change these via OSC 10/11/12; the per-cell `cell` verb only reports
/// already-RESOLVED colors, so this surfaces the theme itself (the default
/// fg/bg and the cursor color) for a client deciding how to render or reason.
pub(crate) fn cmd_colors(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let h = |r: u8, g: u8, b: u8| format!("{r:02x}{g:02x}{b:02x}");
    let fg = t.default_foreground();
    let bg = t.default_background();
    let cursor = t
        .cursor_color()
        .map_or_else(|| "default".to_string(), |c| h(c.r, c.g, c.b));
    format!(
        "OK fg={} bg={} cursor={}\n",
        h(fg.r, fg.g, fg.b),
        h(bg.r, bg.g, bg.b),
        cursor,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::Terminal;

    use super::{
        cmd_cell, cmd_custody, cmd_search, distinct_images, gather_styled_frame, serialize_dims,
        serialize_dims_json, styled_frame_payload,
    };
    use crate::term_lock;

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
        /// Display one 1x1-cell kitty image, filled with `tag`, at 1-based
        /// `row`/`col`. `s`/`v` are the raster's pixel size — one cell each at the
        /// 10x20 cell metric below — and `f=32` requires exactly `s * v * 4` bytes.
        fn place(t: &mut Terminal, row: u16, col: u16, id: u8, tag: u8) {
            t.process(format!("\x1b[{row};{col}H").as_bytes());
            let raster = vec![tag; 10 * 20 * 4];
            let b64 = aterm_codec::base64::encode(&raster).expect("encode");
            let mut apc = format!("\x1b_Ga=T,f=32,s=10,v=20,i={id};").into_bytes();
            apc.extend_from_slice(b64.as_bytes());
            apc.extend_from_slice(b"\x1b\\");
            t.process(&apc);
        }

        let mut t = Terminal::new(10, 40);
        t.set_cell_pixel_size(10, 20);
        place(&mut t, 5, 3, 1, b'F');
        // Row 1 carries FOUR images at {2, 8, 20, 33}: raw map order for these
        // keys is [7, 1, 19, 32] (non-ascending), so an unsorted emit cannot
        // pass by accident.
        place(&mut t, 1, 33, 2, b'E');
        place(&mut t, 1, 8, 3, b'B');
        place(&mut t, 1, 2, 4, b'A');
        place(&mut t, 1, 20, 5, b'D');
        place(&mut t, 3, 10, 6, b'C');

        // The tag byte identifies WHICH picture landed at each anchor; the raster
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

        // THE SHELL'S NEXT PROMPT MUST NOT ERASE THE ANSWER. This is the batch that
        // follows every Enter, and it is the record's most frequent writer by orders
        // of magnitude. `last` moves to it, truthfully; `changed` does not, because
        // an `OutputAtLive` at offset 0 with nothing selected took NOTHING — and the
        // user asking "what took my selection" is asking one second later, not inside
        // the same step a harness reads back in.
        term_lock(&term).process(b"$ \r\n");
        assert_eq!(
            cmd_custody(&term),
            "OK last=OutputAtLive event=5 changed=TypingPress took_selection=TypingPress offset=0 owner=tail \
             selection=no scrollback=6\n",
            "ordinary output is the last EVENT but must not become the last CHANGE"
        );

        // Output records too, so "my selection vanished and I never touched the
        // keyboard" has an answer as well. This ED 3 lands on a live, unselected
        // prompt, so it names itself in `last` and — correctly — takes nothing.
        term_lock(&term).process(b"\x1b[3J");
        assert_eq!(
            cmd_custody(&term),
            "OK last=OutputInvalidatesTheCoordinateSpace event=7 changed=TypingPress \
             took_selection=OutputInvalidatesTheCoordinateSpace \
             offset=0 owner=tail selection=no scrollback=0\n",
            "ED 3 destroyed the coordinate space; the verb names that, not a press"
        );
    }

    /// FINDING 9's arm: output destroys a highlight for a reason that is NOT damage
    /// overlap — here a whole-interval eviction at the history floor.
    ///
    /// This is the flagship complaint, the one a user is most likely to be typing
    /// `custody` about, and the record used to answer it by setting itself to `None`:
    /// `last=none`, indistinguishable from a terminal that has never done anything,
    /// with whatever true record was standing destroyed on the way out. It must name
    /// itself instead — with a tag OUTSIDE the model's `0..=7` space, because the
    /// model genuinely has no action for this shape and a false model action would be
    /// worse than no answer.
    /// The explanation of an INVOLUNTARY loss must survive whatever the user does
    /// next — because "why did my highlight vanish?" is asked after the vanishing,
    /// and by then the user has usually clicked something.
    ///
    /// `changed=` cannot answer it. That latch means "the last thing that moved
    /// custody", and a deliberate deselect really does move custody, so one ordinary
    /// left-click overwrites the answer with `UserClear` — true of the click, useless
    /// about the loss, and the evidence is gone. `took_selection=` is latched only by
    /// the transitions that take a highlight the user did NOT release, so it keeps
    /// pointing at the culprit.
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

        // Three more lines: the selected row falls off the floor. Nothing REPLACED
        // it — it was evicted — so no damage band names it.
        term_lock(&term).process(b"a\r\nb\r\nc\r\n");
        assert!(
            !term_lock(&term).text_selection().has_selection(),
            "the eviction really did take the highlight"
        );
        assert_eq!(
            cmd_custody(&term),
            "OK last=OutputTookTheSelectionUnattributed event=- \
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

    /// Sparse tail cells must report the same effective defaults as a
    /// materialized cell. In particular DECSCNM swaps those defaults for both
    /// the one-cell verb and the full styled frame; using the raw OSC values as
    /// the fallback would disagree with the pixels on glass.
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
    }

    /// OSC 110/111 restore the configured defaults for an implicit tail too;
    /// the query path must not retain either a prior dynamic colour or a
    /// reverse-video fold after the mode is reset.
    #[test]
    fn sparse_blank_queries_follow_dynamic_color_reset() {
        let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
        let configured = {
            let t = term_lock(&term);
            (t.default_foreground(), t.default_background())
        };
        {
            let mut t = term_lock(&term);
            t.process(b"\x1b]10;rgb:11/22/33\x07");
            t.process(b"\x1b]11;rgb:44/55/66\x07");
            t.process(b"\x1b[?5h");
            t.process(b"\x1b[?5l");
            t.process(b"\x1b]110\x07\x1b]111\x07");
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
    }

    /// EQUIVALENCE GATE for the per-row grapheme buffer: every glyph the frame
    /// puts ON THE WIRE is byte-for-byte `Terminal::cell_grapheme` for that cell
    /// — the single-cell API the gather used to call once per cell, and the one
    /// the `cell` verb still answers with, so the two introspection surfaces
    /// cannot drift apart.
    ///
    /// One row carrying each hazard a shared buffer + byte ranges could confuse:
    /// an ASCII run; a written BLANK (whose grapheme is a single SPACE, not the
    /// empty string, so a "skip the blanks" shortcut would change the wire); a
    /// double-width CJK pair, where the lead owns the glyph and the continuation
    /// owns NOTHING (a zero-length range in the middle of the buffer); a
    /// combining cluster, whose byte length and char length disagree; and the
    /// untouched tail, which materializes as more spaces. Exact strings, because
    /// an off-by-one range still yields a plausible-looking glyph.
    #[test]
    fn styled_frame_glyphs_are_cell_grapheme_across_a_mixed_row() {
        // The wire arm below compares JSON-ESCAPED "glyph" fields against RAW
        // `cell_grapheme` strings. That only works because this fixture is
        // deliberately escape-free — no quote, backslash or control char —
        // so the escaper is the identity here. `json_escape_into`'s own
        // behaviour is covered by its unit tests; this test is about WHERE the
        // bytes come from, not how they are escaped.
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

        // And the SERIALIZED frame, which is the actual contract: the `glyph`
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

    #[test]
    fn styled_frame_distinguishes_wide_lead_from_continuation() {
        let mut term = Terminal::new(2, 8);
        term.process("界".as_bytes());
        let snap = gather_styled_frame(&term);

        assert_eq!(snap.glyph(0, 0), "界");
        assert!(
            snap.cells[0].cells[0].wide_lead,
            "the raw WIDE flag belongs to the glyph's lead cell"
        );
        assert!(
            !snap.cells[0].cells[0].cell.wide,
            "RenderCell::wide is not the lead marker"
        );
        assert!(
            !snap.cells[0].cells[1].wide_lead,
            "the continuation must not duplicate the lead marker"
        );
        assert!(
            snap.cells[0].cells[1].cell.wide,
            "the resolved continuation retains its right-half marker"
        );
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

    /// P1.0c: a repeat query on unchanged content (the cache-hit path, no
    /// snapshot) returns byte-identical results, and a content write
    /// invalidates the snapshot so new content is found — while the old
    /// needle's ABSOLUTE row survives the incremental refresh unchanged.
    #[test]
    fn repeat_search_stable_and_write_invalidates() {
        // Searches a needle 20 rows deep in scrollback, so it assumes the default (large)
        // cap — serialize against the cap-mutation test that lowers the process-global cap.
        let _serial = super::search_cap_test_guard();
        let term = term_with(&["NEEDLE_alpha"]);
        for i in 0..20 {
            term.lock()
                .unwrap()
                .process(format!("filler {i}\r\n").as_bytes());
        }
        let r1 = cmd_search(&term, "NEEDLE_alpha");
        assert!(r1.starts_with("OK 1"), "scrolled-off needle found: {r1}");
        let r2 = cmd_search(&term, "NEEDLE_alpha");
        assert_eq!(r1, r2, "cache-hit repeat must be byte-identical");
        assert!(cmd_search(&term, "NEEDLE_beta").starts_with("OK 0"));
        let _ = super::take_last_search_snapshot_lines();
        term.lock().unwrap().process(b"NEEDLE_beta\r\n");
        let rb = cmd_search(&term, "NEEDLE_beta");
        assert!(rb.starts_with("OK 1"), "post-write needle found: {rb}");
        assert!(
            super::take_last_search_snapshot_lines() <= 6,
            "ordinary output refreshes the four visible rows, at most one appended row, \
             and the one row below the boundary that a soft-wrap run could have reached \
             into it from"
        );
        assert_eq!(
            cmd_search(&term, "NEEDLE_alpha"),
            r1,
            "absolute row of the old needle is stable across the refresh"
        );
    }

    /// A cached visible row can be edited and then become scrollback before
    /// the next query. Incremental refresh must start at the OLD visible
    /// boundary, not the new one, or that now-historical row remains stale.
    #[test]
    fn incremental_refresh_captures_an_edited_row_that_scrolled_away() {
        let _serial = super::search_cap_test_guard();
        let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
        term.lock().unwrap().process(b"OLD_VISIBLE_TOKEN");
        assert!(cmd_search(&term, "OLD_VISIBLE_TOKEN").starts_with("OK 1"));

        // Replace the top visible row, then scroll it into history without an
        // intervening search/cache generation.
        term.lock()
            .unwrap()
            .process(b"\x1b[H\x1b[2KNEW_HISTORY_TOKEN\x1b[4;1H\r\n");

        assert!(cmd_search(&term, "NEW_HISTORY_TOKEN").starts_with("OK 1"));
        assert!(cmd_search(&term, "OLD_VISIBLE_TOKEN").starts_with("OK 0"));
    }

    /// A soft-wrap run is indexed as ONE line keyed at the row it starts on, so
    /// an incremental refresh that began INSIDE a run would leave that run's
    /// head holding text for rows the refresh has since re-read — a hit spliced
    /// out of content that is no longer on the glass. The refresh starts below
    /// the boundary and walks to the run's head, which is why the bound in
    /// `repeat_search_stable_and_write_invalidates` is the visible window plus
    /// one row rather than the visible window exactly.
    #[test]
    fn an_incremental_refresh_rejoins_the_run_its_boundary_landed_inside() {
        let _serial = super::search_cap_test_guard();
        let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
        // A wrapped run across the screen-top boundary: "0123456789" scrolls
        // into history, "ABCDEFGHIJ" stays as the top visible (continuation) row.
        term.lock()
            .unwrap()
            .process(b"0123456789ABCDEFGHIJ\r\naa\r\nbb\r\n");
        assert!(
            cmd_search(&term, "9A").starts_with("OK 1"),
            "the straddling pair is found while the run is whole"
        );

        // Rewrite the top visible row out of the run, then scroll so the next
        // query takes the incremental path with its boundary on that row.
        term.lock()
            .unwrap()
            .process(b"\x1b[H\x1b[2Kzz\x1b[4;1H\r\n");
        assert!(
            cmd_search(&term, "9A").starts_with("OK 0"),
            "the run's head must be re-joined, not left holding the rows it lost"
        );
        assert!(
            cmd_search(&term, "0123456789").starts_with("OK 1"),
            "the head keeps its own row's text"
        );
    }

    /// Read one absolute row back as the grid renders it, so a test can say
    /// which row a token is ACTUALLY printed on rather than trusting the search
    /// it is checking.
    fn row_text(term: &Arc<Mutex<Terminal>>, abs: u64) -> String {
        match super::abs_row_text(&term_lock(term), abs) {
            super::AbsRow::Text(s) => s,
            super::AbsRow::Evicted | super::AbsRow::OutOfRange => "<evicted>".to_string(),
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
    #[test]
    fn a_blank_row_inside_a_soft_wrap_run_still_holds_its_width_of_the_line() {
        let _serial = super::search_cap_test_guard();
        let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
        // Three wrapped rows, then the MIDDLE one erased in place.
        term.lock()
            .unwrap()
            .process(b"0123456789ABCDEFGHIJKLMNOPQRST");
        term.lock().unwrap().process(b"\x1b[2;1H\x1b[2K");
        assert_eq!(row_text(&term, 1), "", "the middle row really is blank");
        assert_eq!(
            row_text(&term, 2),
            "KLMNOPQRST",
            "the tail is printed on grid row 2"
        );
        assert_eq!(
            cmd_search(&term, "KLMNOPQRST"),
            "OK 1\n2 0 10\n",
            "the hit is reported on the row it is printed on"
        );
    }

    /// The same stride, lost at the run's HEAD rather than inside it: an erased
    /// first row still occupies the width its continuation is offset by.
    #[test]
    fn a_blank_head_row_of_a_soft_wrap_run_still_holds_its_width_of_the_line() {
        let _serial = super::search_cap_test_guard();
        let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
        term.lock().unwrap().process(b"0123456789ABCDEFGHIJ");
        term.lock().unwrap().process(b"\x1b[1;1H\x1b[2K");
        assert_eq!(row_text(&term, 0), "", "the head row really is blank");
        assert_eq!(
            row_text(&term, 1),
            "ABCDEFGHIJ",
            "the continuation is printed on grid row 1"
        );
        assert_eq!(
            cmd_search(&term, "ABCDEFGHIJ"),
            "OK 1\n1 0 10\n",
            "the hit is reported on the row it is printed on"
        );
    }

    /// A soft wrap is the terminal's LAYOUT, never part of the text, so the line
    /// a regex is matched against is the logical line the reader sees: `^` binds
    /// where that line begins and `$` where it ends, not to the grid-row edges
    /// the wrap happened to fall on. A continuation row therefore has no `^` of
    /// its own, and the character a wrap merely pushed to the end of a row is not
    /// at `$` — which is what a user asking to search across soft wraps means by
    /// "the start of the line".
    #[test]
    fn regex_anchors_bind_to_the_logical_line_not_the_rows_a_soft_wrap_split_it_into() {
        let _serial = super::search_cap_test_guard();
        let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
        // One 20-column logical line, laid out as grid rows 0 and 1.
        term.lock().unwrap().process(b"0123456789ABCDEFGHIJ");

        // Both wrap-interior positions: the far side of the boundary is not a
        // beginning, and its near side is not an end.
        assert_eq!(
            cmd_search(&term, "^ABC regex"),
            "OK 0\n",
            "a continuation row's column 0 is not where the line begins"
        );
        assert_eq!(
            cmd_search(&term, "9$ regex"),
            "OK 0\n",
            "the column the wrap pushed to a row's end is not where the line ends"
        );

        // The line's own ends do anchor, and a match past the boundary is still
        // re-expressed on the physical row it occupies.
        assert_eq!(cmd_search(&term, "^0123 regex"), "OK 1\n0 0 4\n");
        assert_eq!(cmd_search(&term, "J$ regex"), "OK 1\n1 9 1\n");
        assert_eq!(
            cmd_search(&term, "^0123456789ABCDEFGHIJ$ regex"),
            "OK 1\n0 0 20\n",
            "anchored end to end, the run is ONE line whose len counts through the wrap"
        );

        // An UNWRAPPED row is its own logical line, so its row edges anchor as
        // they always did — the rebinding follows the wrap, not the grid.
        term.lock().unwrap().process(b"\x1b[3;1Hxyz");
        assert_eq!(cmd_search(&term, "^xyz$ regex"), "OK 1\n2 0 3\n");
    }

    /// The snapshot cache is keyed by TERMINAL IDENTITY: a different session
    /// searched immediately after must never be answered from the first
    /// session's index.
    #[test]
    fn search_index_is_not_reused_across_terminals() {
        let a = term_with(&["NEEDLE_shared", "pad a"]);
        assert!(cmd_search(&a, "NEEDLE_shared").starts_with("OK 1"));
        let b = term_with(&["nothing here", "pad b"]);
        assert!(
            cmd_search(&b, "NEEDLE_shared").starts_with("OK 0"),
            "terminal B must not see terminal A's cached index"
        );
        assert!(cmd_search(&a, "NEEDLE_shared").starts_with("OK 1"));
    }

    /// An alt-screen swap changes the ACTIVE grid, so the main-screen index
    /// must not answer for the alt screen (and vice versa on return) — the
    /// legacy active-grid indexing semantics, preserved across the cache.
    #[test]
    fn alt_screen_swap_invalidates_cache() {
        let term = term_with(&["NEEDLE_main", "pad"]);
        assert!(cmd_search(&term, "NEEDLE_main").starts_with("OK 1"));
        term.lock().unwrap().process(b"\x1b[?1049h");
        assert!(
            cmd_search(&term, "NEEDLE_main").starts_with("OK 0"),
            "alt screen active: main-grid content is not addressable"
        );
        term.lock().unwrap().process(b"\x1b[?1049l");
        assert!(cmd_search(&term, "NEEDLE_main").starts_with("OK 1"));
    }

    /// SEARCH DEPTH CAP (fix #3/#5): the configured cap evicts the OLDEST scrollback and
    /// honestly flags results `incomplete`, YET the floor keeps the LIVE SCREEN searchable
    /// under any cap — and the cap is part of the index key, so lowering then raising it
    /// rebuilds rather than stale-serving. Mutates the process-global cap, so it holds
    /// [`super::search_cap_test_guard`] (serializing against scrollback-searching tests) and
    /// restores the prior value on the way out.
    #[test]
    fn search_cap_evicts_scrollback_but_floors_at_the_live_screen() {
        let _serial = super::search_cap_test_guard();
        let saved = super::search_max_lines();

        // A 4-row terminal (visible_rows = 4): OLDNEEDLE buried deep in scrollback,
        // NEWNEEDLE on the live screen.
        let mut lines: Vec<String> = vec!["OLDNEEDLE".to_string()];
        for i in 0..30 {
            lines.push(format!("filler {i}"));
        }
        lines.push("NEWNEEDLE".to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let term = term_with(&refs);

        // Tiny cap (1) → floored to the 4 visible rows: deep scrollback evicted, live kept.
        super::set_search_max_lines(1);
        let old = cmd_search(&term, "OLDNEEDLE");
        assert!(
            old.starts_with("OK 0"),
            "deep-scrollback needle evicted under a tiny cap: {old}"
        );
        assert!(
            old.contains("incomplete"),
            "the eviction is honestly flagged incomplete: {old}"
        );
        let new = cmd_search(&term, "NEWNEEDLE");
        assert!(
            new.starts_with("OK 1"),
            "the live screen is ALWAYS searchable no matter how small the cap (the floor): {new}"
        );

        // Cache-key proof: raising the cap re-finds the deep needle. A stale tiny index keyed
        // without the depth would still answer OK 0; a rebuild at the new depth finds it.
        super::set_search_max_lines(super::DEFAULT_MAX_CACHED_LINES);
        let deep = cmd_search(&term, "OLDNEEDLE");
        assert!(
            deep.starts_with("OK 1"),
            "raising the cap rebuilds at the new depth and re-finds the deep needle: {deep}"
        );
        assert!(
            !deep.contains("incomplete"),
            "the full-depth index reports complete: {deep}"
        );

        super::set_search_max_lines(saved);
    }

    /// SEARCH DEPTH CAP — WHOLE visible screen (fix #2): the floor is
    /// `max_cached_for_retained(visible_rows)`, NOT `visible_rows`, because the engine
    /// evicts to a 3/4 low-water mark. Places a needle on the TOP visible row (its absolute
    /// row == base_y, verified under a full cap) and asserts a TINY cap still finds it — a
    /// `visible_rows` floor would evict the oldest quarter of the screen and lose it.
    #[test]
    fn search_cap_floor_keeps_the_whole_visible_screen() {
        let _serial = super::search_cap_test_guard();
        let saved = super::search_max_lines();

        // 4-row terminal: TOPVIS lands on the TOP visible row (3rd-from-last after the
        // trailing newline scroll), with 40 scrollback lines beneath it.
        let mut lines: Vec<String> = (0..40).map(|i| format!("filler {i}")).collect();
        lines.push("TOPVIS".to_string());
        lines.push("z1".to_string());
        lines.push("z2".to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let term = term_with(&refs);

        // Full cap: TOPVIS is found AND its absolute row equals base_y — i.e. it really is
        // the top on-screen row (not scrollback), so the tiny-cap probe below tests the floor.
        super::set_search_max_lines(super::DEFAULT_MAX_CACHED_LINES);
        let full = cmd_search(&term, "TOPVIS");
        assert!(
            full.starts_with("OK 1"),
            "TOPVIS found under a full cap: {full}"
        );
        let abs: u64 = full
            .lines()
            .nth(1)
            .and_then(|l| l.split(' ').next())
            .and_then(|n| n.parse().ok())
            .expect("match row");
        let base_y = term.lock().unwrap().grid().base_y() as u64;
        assert_eq!(
            abs, base_y,
            "TOPVIS sits on the top visible row (absolute row == base_y)"
        );

        // Tiny cap: the floor must keep the ENTIRE visible screen — including its TOP row —
        // searchable, not just the newest 3/4 the raw eviction low-water mark would leave.
        super::set_search_max_lines(1);
        let tiny = cmd_search(&term, "TOPVIS");
        assert!(
            tiny.starts_with("OK 1"),
            "the top visible row is still found under a tiny cap (the retained-floor fix): {tiny}"
        );

        super::set_search_max_lines(saved);
    }

    /// Cold builds copy/index only the suffix that can survive the configured
    /// cap. The omitted prefix is still surfaced as incomplete history with an
    /// absolute retained watermark.
    #[test]
    fn cold_search_snapshot_work_is_bounded_by_retained_cap() {
        let _serial = super::search_cap_test_guard();
        let saved = super::search_max_lines();
        super::set_search_max_lines(16);
        let owned: Vec<String> = (0..200).map(|line| format!("row {line}")).collect();
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        let term = term_with(&refs);

        let _ = super::take_last_search_snapshot_lines();
        let found = super::search_full_history(&term, "row 199", false, false)
            .expect("bounded cold search");
        assert!(found.consistent);
        assert!(found.results.incomplete);
        assert!(found.results.lowest_retained_line > 0);
        assert!(
            super::take_last_search_snapshot_lines() <= 16,
            "cold copy must never exceed the configured retained suffix"
        );

        super::set_search_max_lines(saved);
    }

    #[test]
    fn alternating_terminal_searches_reuse_bounded_per_terminal_cache() {
        let _serial = super::search_cap_test_guard();
        let first = term_with(&["first needle"]);
        let second = term_with(&["second needle"]);
        super::search_full_history(&first, "needle", false, false).unwrap();
        super::search_full_history(&second, "needle", false, false).unwrap();
        let _ = super::take_last_search_snapshot_lines();

        let repeat = super::search_full_history(&first, "needle", false, false).unwrap();
        assert!(repeat.consistent);
        assert_eq!(
            super::take_last_search_snapshot_lines(),
            0,
            "switching back to a recent terminal must reuse its immutable index"
        );
    }

    /// The off-lock snapshot caches ONLY a proven-consistent index ([`snapshot_cacheable`]).
    /// Pins the decision so the hard-to-drive mid-copy tear branch stays covered: a `torn`
    /// snapshot is never cacheable even when the endpoints match (the main→alt→main
    /// round-trip that preserves `content_seq`), and a moved endpoint (seq or alt) also bars
    /// caching.
    #[test]
    fn snapshot_cacheable_requires_no_tear_and_a_matching_endpoint() {
        use super::snapshot_cacheable;
        // Clean: not torn, endpoint still matches the key → cacheable.
        assert!(snapshot_cacheable(false, false, false, 7, 7, 3, 3));
        // Torn mid-copy → NEVER cacheable, even though the endpoint matches (key_seq ==
        // seq_now): the exact main→alt→main-with-unchanged-content_seq case the flag exists for.
        assert!(!snapshot_cacheable(true, false, false, 7, 7, 3, 3));
        // Endpoint diverged — content generation moved...
        assert!(!snapshot_cacheable(false, false, false, 7, 8, 3, 3));
        // ...or the active screen flipped (either direction).
        assert!(!snapshot_cacheable(false, false, true, 7, 7, 3, 3));
        assert!(!snapshot_cacheable(false, true, false, 7, 7, 3, 3));
        // A protected-footer row splice can invalidate absolute coordinates
        // independently of the cached index key.
        assert!(!snapshot_cacheable(false, false, false, 7, 7, 3, 4));
    }

    /// A cache hit is only a START-point proof: mutation after the immutable
    /// query finishes must make the returned coordinate frame inconsistent.
    #[test]
    fn cache_hit_query_rechecks_terminal_generation_after_search() {
        let _serial = super::search_cap_test_guard();
        let term = term_with(&["needle", "other"]);
        let initial =
            super::search_full_history(&term, "needle", false, false).expect("initial search");
        assert!(initial.consistent);

        super::arm_search_cache_hit_mutation(&term);
        let raced =
            super::search_full_history(&term, "needle", false, false).expect("cache-hit search");
        assert!(!raced.consistent, "post-query mutation must fail closed");
        assert_ne!(
            raced.content_seq,
            term_lock(&term).content_seq(),
            "the result retains its original coordinate-generation stamp"
        );
    }

    #[test]
    fn metrics_json_exposes_redraw_scheduler_and_drop_diagnostics() {
        let text = super::cmd_metrics(None, "");
        for field in [
            "redraw_attempts=",
            "redraw_retry_gated=",
            "frame_refills_scoped=",
            "frame_refills_full=",
            // The effect-only reuse gate's honesty term: scoped + full + skipped
            // is still one per presented non-rescan frame.
            "frame_refills_skipped=",
            "frame_refill_full_causes=",
            "last_pre_present_ms=",
            "pre_present_total_ms=",
            "last_present_drop_reason=",
            "wake_owner=",
            "deadline_owner=",
            "past_deadline_arms=",
            // ITEM 6: the per-owner ledger that names a spin's producer.
            "deadline_arms_by_owner=",
            // ITEMS 18/19: the per-owner ledger that names the spin the fold
            // is actively healing (the windowed past-arm streak clamp).
            "past_arm_streak_heals=",
            // ITEM 10 tail surfacing: a healthy median must never again be the
            // only thing the summary shows.
            "n_input=",
            "input_p50_ms=",
            "input_p95_ms=",
            "input_p99_ms=",
            "n_present=",
            "present_p95_ms=",
            "present_p99_ms=",
            // ITEM 10 honesty split: the diverted samples stay VISIBLE.
            "present_tainted=",
            "max_present_tainted_ms=",
            "capture_episodes=",
            "capture_active=",
            "stale_arm_heals=",
            "rust_main_to_first_present_ms=",
            "startup_phase_schema=",
            "startup_phase_valid=",
            "startup_router_ms=",
            "startup_gui_prepare_ms=",
            "startup_winit_dispatch_ms=",
            "startup_initial_surface_attach_ms=",
            "startup_surface_to_successful_redraw_ms=",
            "startup_successful_compose_ms=",
            "startup_successful_surface_transaction_ms=",
            "startup_successful_finalize_ms=",
            "startup_attach_schema=",
            "startup_attach_valid=",
            "startup_attach_dispatch_ms=",
            "startup_attach_prepare_ms=",
            "startup_attach_window_create_ms=",
            "startup_attach_window_setup_ms=",
            "startup_attach_backend_finalize_ms=",
            "startup_attach_chrome_geometry_ms=",
            "startup_attach_surface_create_ms=",
            "startup_attach_finish_ms=",
            "startup_worker_schema=",
            "startup_worker_valid=",
            "startup_worker_total_ms=",
            "startup_worker_overlap_ms=",
            "startup_worker_after_join_ms=",
            "startup_worker_post_join_ms=",
            "startup_worker_gpu_build_ms=",
            "startup_worker_font_seal_ms=",
            "startup_worker_epilogue_ms=",
            "startup_gpu_schema=",
            "startup_gpu_valid=",
            "startup_gpu_instance_ms=",
            "startup_gpu_adapter_ms=",
            "startup_gpu_device_ms=",
            "startup_gpu_font_thread_ms=",
            "startup_gpu_font_join_ms=",
            "startup_gpu_pipelines_ms=",
            "startup_gpu_pipe_cell_ms=",
            "startup_gpu_tail_ms=",
            "startup_gpu_cell_pipeline_ms=",
            // The demand-build ledger: `0` on a default launch is the standing
            // proof the nine effect pipelines are not compiled for pixels the
            // config never draws.
            "effect_pipeline_builds=",
            "effect_pipeline_build_ms=",
            "effect_pipelines_built=",
            "first_present_ms=",
        ] {
            assert!(
                text.contains(field),
                "text metrics omitted `{field}`: {text}"
            );
        }
        let reply = super::cmd_metrics_json(None, "");
        let body = reply
            .strip_prefix("OK 1\n")
            .expect("status frame")
            .trim_end();
        let value: aterm_json::Value = aterm_json::from_str(body).expect("valid metrics JSON");
        for key in [
            "redraw_attempts",
            "redraw_early_outs",
            "redraw_retry_gated",
            "frame_refills_scoped",
            "frame_refills_full",
            "frame_refills_skipped",
            "frame_refill_full_causes",
            "pre_present_attempts",
            "last_pre_present_ms",
            "pre_present_total_ms",
            "max_pre_present_ms",
            "present_drops",
            "last_present_drop_reason",
            "last_present_drop_parked",
            "wake_kind",
            "wake_owner",
            "deadline_owner",
            "deadline_in_ms",
            "deadline_late_ms",
            "past_deadline_arms",
            "deadline_arms_by_owner",
            "past_arm_streak_heals",
            "n_input",
            "input_p50_ms",
            "input_p95_ms",
            "input_p99_ms",
            "n_present",
            "present_p95_ms",
            "present_p99_ms",
            "present_tainted",
            "last_present_tainted_ms",
            "max_present_tainted_ms",
            "capture_episodes",
            "capture_active",
            "stale_arm_heals",
            "rust_main_to_first_present_ms",
            "startup_phase_schema",
            "startup_phase_valid",
            "startup_router_ms",
            "startup_gui_prepare_ms",
            "startup_winit_dispatch_ms",
            "startup_initial_surface_attach_ms",
            "startup_surface_to_successful_redraw_ms",
            "startup_successful_compose_ms",
            "startup_successful_surface_transaction_ms",
            "startup_successful_finalize_ms",
            "startup_attach_schema",
            "startup_attach_valid",
            "startup_attach_dispatch_ms",
            "startup_attach_prepare_ms",
            "startup_attach_window_create_ms",
            "startup_attach_window_setup_ms",
            "startup_attach_backend_finalize_ms",
            "startup_attach_chrome_geometry_ms",
            "startup_attach_surface_create_ms",
            "startup_attach_finish_ms",
            "startup_worker_schema",
            "startup_worker_valid",
            "startup_worker_total_ms",
            "startup_worker_overlap_ms",
            "startup_worker_after_join_ms",
            "startup_worker_post_join_ms",
            "startup_worker_gpu_build_ms",
            "startup_worker_font_seal_ms",
            "startup_worker_epilogue_ms",
            "startup_gpu_schema",
            "startup_gpu_valid",
            "startup_gpu_instance_ms",
            "startup_gpu_adapter_ms",
            "startup_gpu_device_ms",
            "startup_gpu_font_thread_ms",
            "startup_gpu_font_join_ms",
            "startup_gpu_pipelines_ms",
            "startup_gpu_pipe_cell_ms",
            "startup_gpu_tail_ms",
            "startup_gpu_cell_pipeline_ms",
            "effect_pipeline_builds",
            "effect_pipeline_build_ms",
            "effect_pipelines_built",
            "first_present_ms",
        ] {
            assert!(
                value.get(key).is_some(),
                "metrics JSON omitted `{key}`: {reply}"
            );
        }

        let pct = super::cmd_metrics_json(None, "percentiles");
        let pct_body = pct.strip_prefix("OK 1\n").unwrap().trim_end();
        let pct_value: aterm_json::Value = aterm_json::from_str(pct_body).unwrap();
        for key in [
            "n_input",
            "input_p99_ms",
            "n_present",
            "present_p99_ms",
            "n_render",
            "render_p99_ms",
            "n_key_write",
            "key_write_p99_ms",
            "n_pre_present",
            "pre_present_p99_ms",
            "n_acquire",
            "acquire_p99_ms",
            "n_resize",
            "resize_p99_ms",
            // ECHO ROUND TRIP (audit item 5): the only slice on this line that
            // measures the CHILD rather than aterm. Its counters are asserted
            // alongside its percentiles because a percentile published without
            // its sample/expiry ledger cannot be read honestly.
            "n_echo",
            "echo_p50_ms",
            "echo_p95_ms",
            "echo_p99_ms",
            "echo_total",
            "echo_arms",
            "echo_coalesced",
            "echo_expired",
            "echo_dropped_locked",
            // ITEM 10 honesty split. The tainted twin is published BESIDE the
            // clean distribution, never instead of it: `n_present +
            // n_present_tainted` still accounts for every content present, so
            // the exclusion is auditable rather than a silent filter.
            "n_present_tainted",
            "present_tainted_p50_ms",
            "present_tainted_p95_ms",
            "present_tainted_p99_ms",
        ] {
            assert!(
                pct_value.get(key).is_some(),
                "percentiles JSON omitted `{key}`"
            );
        }

        let text_pct = super::cmd_metrics(None, "percentiles");
        for field in [
            "n_key_write=",
            "n_pre_present=",
            "pre_present_p99_ms=",
            "n_acquire=",
            "n_resize=",
            "resize_p99_ms=",
            "n_echo=",
            "echo_p50_ms=",
            "echo_p99_ms=",
            "echo_expired=",
            "echo_coalesced=",
            "n_present_tainted=",
            "present_tainted_p95_ms=",
            "present_tainted_p99_ms=",
        ] {
            assert!(
                text_pct.contains(field),
                "text percentiles omitted `{field}`: {text_pct}"
            );
        }
    }

    /// TIER-1 ITEM 3 (2026-08 draw-path audit): THE DRAWABLE-PARK MAX IS
    /// PUBLISHED.
    ///
    /// `metrics::note_acquire_wait` had recorded `LAST_ACQUIRE_WAIT_NS` and
    /// `MAX_ACQUIRE_WAIT_NS` on every present — and `reset` had cleared them —
    /// since the swapchain-acquire slice was first instrumented, while no
    /// snapshot ever read either one. The only acquire figures a reader could
    /// obtain were the histogram's percentiles, and a single 200 ms
    /// `nextDrawable` park (the largest known macOS typing stall: it blocks the
    /// winit main thread while keyDowns queue in the OS event queue) is
    /// arithmetically invisible in a p99 taken over thousands of ~0.02 ms
    /// samples. This pins both scalars into all four published forms — the
    /// summary a driver reads by default and the `percentiles` line they qualify
    /// — so the stall can never go dark again.
    #[test]
    fn the_acquire_wait_max_is_published_in_every_metrics_form() {
        let text = super::cmd_metrics(None, "");
        for field in ["last_acquire_wait_ms=", "max_acquire_wait_ms="] {
            assert!(
                text.contains(field),
                "summary text metrics omitted `{field}`: {text}"
            );
        }
        let text_pct = super::cmd_metrics(None, "percentiles");
        for field in ["last_acquire_wait_ms=", "max_acquire_wait_ms="] {
            assert!(
                text_pct.contains(field),
                "text percentiles omitted `{field}`: {text_pct}"
            );
        }
        for command in ["", "percentiles"] {
            let reply = super::cmd_metrics_json(None, command);
            let body = reply
                .strip_prefix("OK 1\n")
                .expect("status frame")
                .trim_end();
            let value: aterm_json::Value = aterm_json::from_str(body).expect("valid metrics JSON");
            for key in ["last_acquire_wait_ms", "max_acquire_wait_ms"] {
                assert!(
                    value.get(key).is_some(),
                    "metrics JSON (`{command}`) omitted `{key}`: {reply}"
                );
            }
        }
    }

    /// ITEM 6 wire shape. The per-owner ledger is a self-labelling field in
    /// both forms — an OBJECT in JSON, `owner:arms/past` pairs in text — so a
    /// reader never depends on position and a newly appended `DeadlineOwner`
    /// cannot shift someone else's column. `deadline_arms_by_owner` is spelled
    /// out in full precisely so a naive `grep deadline_arms=` still finds only
    /// the global `past_deadline_arms`.
    #[test]
    fn the_per_owner_arm_ledger_is_self_labelling_in_both_forms() {
        let reply = super::cmd_metrics_json(None, "");
        let body = reply
            .strip_prefix("OK 1\n")
            .expect("status frame")
            .trim_end();
        let value: aterm_json::Value = aterm_json::from_str(body).expect("valid metrics JSON");
        let arms = value
            .get("deadline_arms_by_owner")
            .expect("the per-owner ledger is published");
        assert!(
            arms.is_object(),
            "the ledger must be a keyed object, not a scraped string: {arms}"
        );
        for (owner, entry) in arms.as_object().expect("object") {
            assert!(!owner.is_empty(), "every entry is named");
            assert!(
                entry
                    .get("arms")
                    .and_then(aterm_json::Value::as_u64)
                    .is_some(),
                "{owner} publishes its arm count"
            );
            assert!(
                entry
                    .get("past")
                    .and_then(aterm_json::Value::as_u64)
                    .is_some(),
                "{owner} publishes its PAST arm count — the spin signature"
            );
        }

        // ITEMS 18/19: the windowed streak-heal ledger rides beside the arm
        // ledger with the same self-labelling shape — an object in JSON…
        let heals = value
            .get("past_arm_streak_heals")
            .expect("the streak-heal ledger is published");
        assert!(
            heals.is_object(),
            "the streak-heal ledger must be a keyed object: {heals}"
        );
        for (owner, count) in heals.as_object().expect("object") {
            assert!(!owner.is_empty(), "every heal entry is named");
            assert!(
                count.as_u64().is_some(),
                "{owner} publishes a plain heal count"
            );
        }

        // The text twin never emits a bare `key=`: a whitespace-splitting
        // reader (the spin probe is a shell one-liner) always gets a token.
        let text = super::cmd_metrics(None, "");
        let field = text
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("deadline_arms_by_owner="))
            .expect("the text form publishes the ledger");
        assert!(
            !field.is_empty(),
            "empty ledgers render as `none`, not `\"\"`"
        );
        // …and `owner:count` pairs (or `none`) in text.
        let heal_field = text
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("past_arm_streak_heals="))
            .expect("the text form publishes the streak-heal ledger");
        assert!(
            !heal_field.is_empty(),
            "an empty streak-heal ledger renders as `none`, not `\"\"`"
        );
    }

    /// The FULL-arm attribution's wire shape, in both forms — the same
    /// self-labelling discipline as the per-owner arm ledger, tested the same
    /// way, so a new `FullRefillCause` cannot shift anyone's column and a
    /// whitespace-splitting reader always gets a token.
    ///
    /// The counters are process-global and every other test in this binary
    /// shares them, so this asserts SHAPE only, never counts.
    #[test]
    fn the_full_refill_cause_attribution_is_self_labelling_in_both_forms() {
        let reply = super::cmd_metrics_json(None, "");
        let body = reply
            .strip_prefix("OK 1\n")
            .expect("status frame")
            .trim_end();
        let value: aterm_json::Value = aterm_json::from_str(body).expect("valid metrics JSON");
        let causes = value
            .get("frame_refill_full_causes")
            .expect("the per-cause attribution is published");
        assert!(
            causes.is_object(),
            "the attribution must be a keyed object, not a scraped string: {causes}"
        );
        for (cause, frames) in causes.as_object().expect("object") {
            assert!(!cause.is_empty(), "every entry is named");
            assert!(
                frames.as_u64().is_some_and(|n| n > 0),
                "{cause} publishes a non-zero frame count (the ledger is sparse)"
            );
        }

        // The text twin: one whitespace-free token, `none` when nothing has
        // refused yet rather than a bare `key=`.
        let text = super::cmd_metrics(None, "");
        let field = text
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("frame_refill_full_causes="))
            .expect("the text form publishes the attribution");
        assert!(
            !field.is_empty(),
            "an empty attribution renders as `none`, not `\"\"`"
        );
        if field != "none" {
            for pair in field.split(',') {
                let (cause, frames) = pair
                    .split_once(':')
                    .unwrap_or_else(|| panic!("`clause:frames` pairs, got `{pair}`"));
                assert!(!cause.is_empty(), "every pair is named: {field}");
                assert!(
                    frames.parse::<u64>().is_ok(),
                    "every pair carries a count: {field}"
                );
            }
        }

        // Every label the engine can produce must survive the wire round trip
        // as a distinct, splittable token — checked over the WHOLE enum rather
        // than over whichever causes this process happened to hit.
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
            text.contains("rows=busy cols=busy"),
            "text diagnostics identify only the unavailable grid fields: {text}"
        );
        assert!(text.contains("redraw_attempts="));

        let reply = super::cmd_metrics_json(Some(&term), "");
        let body = reply.strip_prefix("OK 1\n").unwrap().trim_end();
        let value: aterm_json::Value = aterm_json::from_str(body).unwrap();
        assert!(value["rows"].is_null());
        assert!(value["cols"].is_null());
        assert!(value.get("redraw_attempts").is_some());
    }

    /// SA-1 cost guard: narrowing must not be used when the index would visit
    /// FEWER lines than the inherited frame holds — the keystroke that turns a
    /// common prefix into a rare query. Results stay equal to the batch layer
    /// either way; what this pins is that the expensive choice is not taken
    /// (the reach counter must NOT advance on that keystroke) while the
    /// ordinary common-prefix keystrokes still narrow.
    #[test]
    fn narrowing_skips_frames_bigger_than_the_engines_own_candidate_set() {
        let _guard = super::search_cap_test_guard();
        let mut lines: Vec<String> = (0..40)
            .map(|i| format!("svc-api request completed size=100 seq={i}"))
            .collect();
        lines.push("svc-api request completed size=977 seq=rare".to_string());
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let term = term_with(&refs);
        let _ = super::take_narrowed_query_steps();

        let batch_oracle = |q: &str| {
            let (key_alt, key_seq, revision, rows, cols) = {
                let t = term_lock(&term);
                (
                    t.is_alternate_screen(),
                    t.content_seq(),
                    t.absolute_row_revision(),
                    t.rows() as usize,
                    usize::from(t.grid().cols()),
                )
            };
            let max_lines =
                super::search_max_lines().max(aterm_core::search::max_cached_for_retained(rows));
            let index = super::cached_search_index(&term, key_alt, key_seq, revision, max_lines)
                .expect("the previous search must have cached a consistent snapshot");
            let (results, _) = super::query_search_index(
                &index,
                q,
                false,
                false,
                super::EngineSearchDirection::Forward,
                None,
                false,
                super::WrapFrame {
                    cols,
                    logical_anchor: None,
                },
            )
            .expect("literal batch query cannot fail");
            results
        };
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

        // Common prefix: every retained line occurs, and the frames are no
        // bigger than the postings the engine would walk — these narrow.
        for q in ["siz", "size", "size="] {
            let full = narrowed(q);
            assert_eq!(full.results, batch_oracle(q), "keystroke {q:?}");
        }
        assert!(
            super::take_narrowed_query_steps() >= 1,
            "reach: the common-prefix keystrokes must narrow"
        );

        // The rare keystroke: the frame holds all 41 lines, the engine's own
        // candidate set is the single rare line — take the engine path.
        let rare = narrowed("size=9");
        assert_eq!(rare.results, batch_oracle("size=9"));
        assert_eq!(
            rare.results.matches.len(),
            1,
            "precondition: the grown query really is rare"
        );
        assert_eq!(
            super::take_narrowed_query_steps(),
            0,
            "cost guard: a frame bigger than the engine's candidate set must be skipped"
        );

        // ...and the reseeded (small) frame narrows again on the next
        // keystroke, so the guard costs nothing beyond the one step.
        let deeper = narrowed("size=97");
        assert_eq!(deeper.results, batch_oracle("size=97"));
        assert_eq!(
            super::take_narrowed_query_steps(),
            1,
            "the reseeded frame is small, so narrowing resumes immediately"
        );
    }

    /// SA-1 direction parity: the find bar's BACKWARD lane ("find previous")
    /// goes through the same narrowing layer, and an uncapped narrowed walk —
    /// which is the complete ascending match set — must equal the backward
    /// batch path verbatim at every keystroke. (The capped backward edge keeps
    /// the newest matches instead of the oldest, which is why
    /// `query_search_index_narrowing` falls back to the batch path there; this
    /// pins the uncapped half of that argument end to end.)
    #[test]
    fn narrowing_backward_direction_equals_the_batch_layer() {
        let _guard = super::search_cap_test_guard();
        let term = term_with(&[
            "find one here",
            "nothing on this row",
            "find two here",
            "find three here",
        ]);
        let _ = super::take_narrowed_query_steps();

        let oracle = |q: &str| {
            let (key_alt, key_seq, revision, rows, cols) = {
                let t = term_lock(&term);
                (
                    t.is_alternate_screen(),
                    t.content_seq(),
                    t.absolute_row_revision(),
                    t.rows() as usize,
                    usize::from(t.grid().cols()),
                )
            };
            let max_lines =
                super::search_max_lines().max(aterm_core::search::max_cached_for_retained(rows));
            let index = super::cached_search_index(&term, key_alt, key_seq, revision, max_lines)
                .expect("the previous search must have cached a consistent snapshot");
            let (results, point) = super::query_search_index(
                &index,
                q,
                false,
                false,
                super::EngineSearchDirection::Backward,
                None,
                false,
                super::WrapFrame {
                    cols,
                    logical_anchor: None,
                },
            )
            .expect("literal batch query cannot fail");
            (results, point)
        };

        for q in ["f", "fi", "fin", "find", "find t"] {
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
            let (results, _) = oracle(q);
            assert_eq!(
                full.results, results,
                "backward keystroke {q:?} must equal the backward batch layer"
            );
        }
        assert!(
            super::take_narrowed_query_steps() >= 2,
            "reach: the backward lane must also run off narrowing frames"
        );
    }

    /// SA-1 wiring: typing a query one char at a time through the real
    /// cache-hit path narrows from the previous keystroke's occurrence frame
    /// — results equal the plain batch layer at EVERY step (the oracle is
    /// `query_search_index` over the same cached index), narrowing really
    /// engages (thread-local reach counter — immune to parallel tests on
    /// other terminals), and backspace + a non-suffix edit stay equal too.
    #[test]
    fn incremental_typing_narrows_and_equals_the_batch_layer() {
        let _guard = super::search_cap_test_guard();
        let term = term_with(&[
            "plain ascii find line",
            // Zero-width character: "\u{200b}" alone spans no display column
            // and is dropped from reported matches, while "\u{200b}b" crosses
            // into the next cell and matches for real — the zero-display-width
            // hazard the occurrence frame must survive. (A U+0600 prepend
            // cluster shows the same shape in the engine, but the VT layer
            // drops that character before it reaches the grid, so the
            // end-to-end line uses the one the terminal really stores.)
            "a\u{200b}b zero width",
            "stra\u{df}e stop",
            "find find find",
            "no hits here",
        ]);
        let _ = super::take_narrowed_query_steps();

        let batch_oracle = |q: &str| {
            let (key_alt, key_seq, revision, rows, cols) = {
                let t = term_lock(&term);
                (
                    t.is_alternate_screen(),
                    t.content_seq(),
                    t.absolute_row_revision(),
                    t.rows() as usize,
                    usize::from(t.grid().cols()),
                )
            };
            let max_lines =
                super::search_max_lines().max(aterm_core::search::max_cached_for_retained(rows));
            let index = super::cached_search_index(&term, key_alt, key_seq, revision, max_lines)
                .expect("the previous search must have cached a consistent snapshot");
            let (results, _) = super::query_search_index(
                &index,
                q,
                false,
                false,
                super::EngineSearchDirection::Forward,
                None,
                false,
                super::WrapFrame {
                    cols,
                    logical_anchor: None,
                },
            )
            .expect("literal batch query cannot fail");
            results
        };
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

        // Grow: f → fi → fin → find. The first call is a snapshot MISS
        // (batch build); "fi" seeds the stack off the fresh hit; "fin"/"find"
        // must run off frames.
        for q in ["f", "fi", "fin", "find"] {
            let full = narrowed(q);
            assert!(full.consistent, "no concurrent mutation in this test");
            assert_eq!(
                full.results,
                batch_oracle(q),
                "narrowed keystroke {q:?} must equal the batch layer"
            );
        }
        assert!(
            super::take_narrowed_query_steps() >= 2,
            "reach: the grown keystrokes must have run off narrowing frames"
        );

        // Backspace to "fin": re-verifies off the stacked frame, still equal.
        let full = narrowed("fin");
        assert_eq!(full.results, batch_oracle("fin"));
        assert_eq!(
            super::take_narrowed_query_steps(),
            1,
            "reach: the backspaced query must be served off its own frame"
        );

        // Non-suffix edit ("xfind" shares no stacked prefix): batch reseed,
        // still equal — the fallback arm is equality-pinned too.
        let full = narrowed("xfind");
        assert_eq!(full.results, batch_oracle("xfind"));

        // The zero-width-only line end-to-end: "\u{200b}" has zero reported
        // matches on the zero-width line (it spans no display column) yet
        // "\u{200b}b" (grown from it) must find the straddling match — the
        // occurrence-frame property surviving the whole GUI path.
        let full_zw = narrowed("\u{200b}");
        assert_eq!(full_zw.results, batch_oracle("\u{200b}"));
        assert!(
            full_zw.results.matches.is_empty(),
            "precondition: the zero-width character spans no display column, so \
             it reports no match anywhere in this corpus"
        );
        let full_zwb = narrowed("\u{200b}b");
        assert_eq!(full_zwb.results, batch_oracle("\u{200b}b"));
        assert!(
            full_zwb.results.matches.iter().any(|m| m.line == 1),
            "the zero-width-straddling match on the zero-width line (abs row 1) \
             must survive narrowing"
        );
    }
}

/// The `trim` family: the ONE trailing-blank rule and the verbs that route through
/// it. Kept as its own module so the helpers stay testable as pure transforms.
#[cfg(test)]
mod trim_tests {
    use std::sync::{Arc, Mutex};

    use aterm_core::terminal::Terminal;

    use super::{
        cmd_blocktext_args, cmd_text, cmd_text_json, cmd_text_json_opt, cmd_text_opt,
        frame_rows_reply, split_trim_tail, text_trim_arg, trim_lines_reply, trimmed_len,
    };

    /// `trimmed_len` is `last non-blank index + 1`: all blank → 0; no blank → n;
    /// interior blanks are preserved because only the TAIL is measured.
    #[test]
    fn trimmed_len_is_last_nonblank_plus_one() {
        assert_eq!(trimmed_len(["", "", ""].into_iter()), 0, "all blank");
        assert_eq!(trimmed_len(["   ", " "].into_iter()), 0, "spaces are blank");
        assert_eq!(trimmed_len(std::iter::empty()), 0, "no rows");
        assert_eq!(trimmed_len(["a", "b", "c"].into_iter()), 3, "no blank → n");
        assert_eq!(
            trimmed_len(["a", "", "", "b", "", ""].into_iter()),
            4,
            "interior blanks kept, trailing dropped"
        );
        assert_eq!(trimmed_len(["", "x"].into_iter()), 2, "leading blank kept");
    }

    /// The `text` tail: empty and `trim` parse; anything else is the usage line.
    #[test]
    fn text_trim_arg_accepts_only_trim() {
        assert_eq!(text_trim_arg(""), Ok(false));
        assert_eq!(text_trim_arg("  "), Ok(false));
        assert_eq!(text_trim_arg("trim"), Ok(true));
        assert_eq!(text_trim_arg(" trim "), Ok(true));
        for bad in ["foo", "trim extra", "TRIM", "trim=1", "--trim"] {
            assert_eq!(
                text_trim_arg(bad),
                Err("ERR usage: text [trim]\n".to_string()),
                "{bad:?} is rejected, never silently ignored"
            );
        }
    }

    /// A trailing `trim` token is split off a positional tail; a `trim` glued to
    /// the argument, or one that is not last, is left for the verb to reject.
    #[test]
    fn split_trim_tail_takes_only_a_trailing_token() {
        assert_eq!(split_trim_tail("7 trim"), ("7", true));
        assert_eq!(split_trim_tail("trim"), ("", true));
        assert_eq!(split_trim_tail("  status   trim "), ("status", true));
        assert_eq!(split_trim_tail("7"), ("7", false));
        assert_eq!(split_trim_tail(""), ("", false));
        assert_eq!(
            split_trim_tail("7trim"),
            ("7trim", false),
            "glued is not a token"
        );
        assert_eq!(split_trim_tail("trim 7"), ("trim 7", false), "not last");
    }

    fn term_with(rows: u16, seed: &[u8]) -> Arc<Mutex<Terminal>> {
        let mut t = Terminal::new(rows, 40);
        t.process(seed);
        Arc::new(Mutex::new(t))
    }

    /// `text trim` sends the rows up to the last non-blank one and says so on the
    /// header; the bare form is unchanged (the grid, counted honestly).
    #[test]
    fn text_trim_drops_trailing_blank_rows_and_counts_what_it_sent() {
        let term = term_with(6, b"one\r\n\r\nthree");
        let bare = cmd_text(&term);
        assert_eq!(bare, "OK 6\none\n\nthree\n\n\n\n", "bare = the grid");
        assert_eq!(
            bare,
            cmd_text_opt(&term, false),
            "cmd_text is the untrimmed form"
        );
        assert_eq!(
            cmd_text_opt(&term, true),
            "OK 3 trimmed=3\none\n\nthree\n",
            "trimmed = rows sent, interior blank kept, k = rows dropped"
        );
        // An all-blank screen trims to nothing and says so.
        let blank = term_with(3, b"");
        assert_eq!(cmd_text_opt(&blank, true), "OK 0 trimmed=3\n");
        assert_eq!(cmd_text_opt(&blank, false), "OK 3\n\n\n\n");
    }

    /// `text --json trim`: `rows` stops at the last non-blank row and the object
    /// carries `"trimmed":k`; `dims.rows` stays the grid. The bare JSON is unchanged.
    #[test]
    fn text_json_trim_carries_trimmed_and_keeps_grid_dims() {
        let term = term_with(6, b"one\r\n\r\nthree");
        let bare = cmd_text_json(&term);
        assert!(bare.starts_with("OK 1\n{\"rows\":[\"one\",\"\",\"three\",\"\",\"\",\"\"],"));
        assert!(
            !bare.contains("trimmed"),
            "bare JSON carries no trimmed field"
        );
        assert_eq!(bare, cmd_text_json_opt(&term, false));
        let trimmed = cmd_text_json_opt(&term, true);
        assert!(
            trimmed.starts_with("OK 1\n{\"rows\":[\"one\",\"\",\"three\"],"),
            "{trimmed}"
        );
        assert!(
            trimmed.contains("\"dims\":{\"rows\":6,\"cols\":40}"),
            "{trimmed}"
        );
        assert!(trimmed.ends_with(",\"trimmed\":3}\n"), "{trimmed}");
    }

    /// The shared framer: `OK <n>[ <verdict>][ trimmed=<k>]` + rows — the `turn`
    /// verdict token lands AFTER the verdict, so a client keying on `turn` at token
    /// two or the count at token one is undisturbed; untrimmed is byte-identical to
    /// the pre-`trim` wire.
    #[test]
    fn frame_rows_reply_places_the_verdict_then_trimmed() {
        let body = "a\n\nb\n\n\n";
        assert_eq!(frame_rows_reply(body, 5, "", false), "OK 5\na\n\nb\n\n\n");
        assert_eq!(
            frame_rows_reply(body, 5, "", true),
            "OK 3 trimmed=2\na\n\nb\n"
        );
        let v = "turn submitted=1 status=settled seq=9 id=2 dur_ms=3 hash=00000000000000ab";
        assert_eq!(
            frame_rows_reply(body, 5, v, false),
            format!("OK 5 {v}\na\n\nb\n\n\n")
        );
        assert_eq!(
            frame_rows_reply(body, 5, v, true),
            format!("OK 3 {v} trimmed=2\na\n\nb\n")
        );
        assert_eq!(
            frame_rows_reply("\n\n", 2, v, true),
            format!("OK 0 {v} trimmed=2\n")
        );
    }

    /// `blocktext <id> [trim]`: the id alone reaches the emitter; `trim` re-frames
    /// the emitter's reply; anything else is the arm's usage line. An `ERR` from the
    /// emitter passes through untouched.
    #[test]
    fn blocktext_args_validate_grammar_and_trim_the_reply() {
        let emit = |id: &str| {
            assert_eq!(id, "7", "only the id reaches the emitter");
            "OK 4 marker\nout\n\n\n\n".to_string()
        };
        assert_eq!(cmd_blocktext_args("7", emit), "OK 4 marker\nout\n\n\n\n");
        assert_eq!(
            cmd_blocktext_args("7 trim", emit),
            "OK 1 marker trimmed=3\nout\n"
        );
        for bad in ["", "trim", "x", "7 foo", "7 trim foo", "7trim"] {
            assert_eq!(
                cmd_blocktext_args(bad, |_| unreachable!("usage errors never emit")),
                "ERR usage: blocktext <id> [trim]\n",
                "{bad:?}"
            );
        }
        assert_eq!(
            cmd_blocktext_args("7 trim", |_| "ERR no such block\n".to_string()),
            "ERR no such block\n",
            "an emitter error is not re-framed"
        );
        assert_eq!(
            trim_lines_reply("OK\n"),
            "OK\n",
            "a countless header passes through"
        );
        assert_eq!(trim_lines_reply("OK 0\n"), "OK 0 trimmed=0\n");
    }
}
