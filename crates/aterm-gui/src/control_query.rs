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
    DEFAULT_MAX_CACHED_LINES, SearchDirection as EngineSearchDirection, SearchMatch,
    SearchOptionsError, SearchResults, TerminalSearch, max_cached_for_retained,
};
use aterm_core::selection::SelectionType;
use aterm_core::terminal::{RenderCell, Terminal, UnderlineStyle};
use aterm_search::MAX_SEARCH_MATCHES;
use winit::event_loop::EventLoopProxy;

use super::{
    DimsSnapshot, control_media, cursor_style_name, image_payload, json_escape, json_ok,
    json_str_field, pct_encode, visible_char,
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

/// `text` -> `OK <nrows>\n` then each visible row (trailing spaces trimmed).
///
/// FIDELITY (I-1): each row is extracted through the engine's combining-aware
/// `get_line_text` — the SAME path `selection_to_string`/`copy` and the
/// renderer's `combining_row`/`cluster_row` use — so an NFD accent
/// (`e`+U+0301) or a ZWJ emoji cluster (👨‍👩‍👧) reads back intact instead of
/// being flattened to its base codepoint. (The old per-`RenderCell` scan only
/// saw the resolved base char and silently dropped combining marks / clusters,
/// corrupting the AI's primary screen-read.) Control chars still collapse to
/// spaces via the extraction's NUL→space rule plus an explicit visible map.
pub(crate) fn cmd_text(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let rows = t.rows() as usize;
    // Sized for the whole reply up front (header + one full row + newline each)
    // so the row loop never reallocates-and-copies the accumulated screen while
    // holding the terminal lock.
    let mut out = String::with_capacity(rows * (t.cols() as usize + 1) + 16);
    {
        use std::fmt::Write as _;
        let _ = writeln!(out, "OK {rows}");
    }
    for r in 0..rows {
        out.push_str(&visible_row(&t, r));
        out.push('\n');
    }
    out
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
    let (layer_gravity, layer_scale, layer_flipped) = snapshot
        .layer_presentation
        .as_ref()
        .map_or_else(
            || ("none".to_string(), "none".to_string(), "none".to_string()),
            |(g, s, f)| (g.clone(), format!("{s:.2}"), f.to_string()),
        );
    format!(
        "OK {} {} {} {} session={} cell_w={} cell_h={} font_px={:.2} window={} \
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
/// edge). Same funnel and honesty bounds as the scalars; zeroed by `metrics
/// reset` like the maxima.
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
        let p = |h: &crate::metrics::Histogram, q: f64| ms(h.percentile(q).unwrap_or(0));
        return format!(
            "OK n_input={} input_p50_ms={:.2} input_p95_ms={:.2} input_p99_ms={:.2} \
             n_present={} present_p50_ms={:.2} present_p95_ms={:.2} present_p99_ms={:.2} \
             n_render={} render_p50_ms={:.2} render_p95_ms={:.2} render_p99_ms={:.2} \
             n_key_write={} key_write_p50_ms={:.2} key_write_p95_ms={:.2} \
             key_write_p99_ms={:.2} n_pre_present={} pre_present_p50_ms={:.2} \
             pre_present_p95_ms={:.2} pre_present_p99_ms={:.2} \
             n_acquire={} acquire_p50_ms={:.2} acquire_p95_ms={:.2} acquire_p99_ms={:.2} \
             n_resize={} resize_p50_ms={:.2} resize_p95_ms={:.2} resize_p99_ms={:.2} \
             n_reflow={} reflow_p50_ms={:.2} reflow_p95_ms={:.2} reflow_p99_ms={:.2}\n",
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
            resize.count(),
            p(resize, 0.50),
            p(resize, 0.95),
            p(resize, 0.99),
            reflow.count(),
            p(reflow, 0.50),
            p(reflow, 0.95),
            p(reflow, 0.99),
        );
    }
    if rest.trim() == "reset" {
        crate::metrics::reset();
    }
    let (rows, cols) = try_metrics_dims(term).map_or_else(
        || ("busy".to_string(), "busy".to_string()),
        |(rows, cols)| (rows.to_string(), cols.to_string()),
    );
    let m = crate::metrics::snapshot();
    let backend = if m.backend_gpu { "gpu" } else { "cpu" };
    format!(
        "OK backend={backend} rows={rows} cols={cols} frames={} \
         last_present_latency_ms={:.2} max_present_latency_ms={:.2} \
         last_frame_render_ms={:.2} max_frame_render_ms={:.2} \
         slow_frames={} slow_threshold_ms={:.1} \
         last_input_present_ms={:.2} max_input_present_ms={:.2} \
         last_key_write_ms={:.2} max_key_write_ms={:.2} \
         last_resize_present_ms={:.2} max_resize_present_ms={:.2} \
         last_resize_reflow_ms={:.2} max_resize_reflow_ms={:.2} \
         sync_armed={} sync_rel_end={} sync_rel_timeout={} sync_holding={} \
         perf_reduced={} shed_transitions={} wake_heals={} \
         last_redraw_total_ms={:.2} max_redraw_total_ms={:.2} \
         redraw_attempts={} redraw_early_outs={} redraw_sync_holds={} redraw_retry_gated={} \
         pre_present_attempts={} last_pre_present_ms={:.2} pre_present_total_ms={:.2} \
         max_pre_present_ms={:.2} \
         present_drops={} last_present_drop_reason={} last_present_drop_parked={} \
         event_wakes={} timer_wakes={} wait_cancelled_wakes={} poll_wakes={} \
         wake_kind={} wake_owner={} wake_late_ms={:.2} deadline_owner={} \
         deadline_in_ms={:.2} deadline_late_ms={:.2} past_deadline_arms={} \
         max_frame_gap_ms={:.2} \
         rust_main_to_first_present_ms={:.2} \
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
         first_present_ms={:.2}\n",
        m.frames_presented,
        ms(m.last_present_latency_ns),
        ms(m.max_present_latency_ns),
        ms(m.last_frame_render_ns),
        ms(m.max_frame_render_ns),
        m.slow_frames,
        ms(crate::metrics::SLOW_FRAME_THRESHOLD_NS),
        ms(m.last_input_present_ns),
        ms(m.max_input_present_ns),
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
        m.pre_present_attempts,
        ms(m.last_pre_present_ns),
        ms(m.pre_present_total_ns),
        ms(m.max_pre_present_ns),
        m.present_drops,
        m.last_present_drop_reason.as_str(),
        u8::from(m.last_present_drop_parked),
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
        ms(m.max_frame_gap_ns),
        ms(m.rust_main_to_first_present_ns),
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
        ms(m.first_present_ns),
    )
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
        let resize = crate::metrics::resize_present_distribution();
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
             \"acquire_p99_ms\":{:.2},\"n_resize\":{},\
             \"resize_p50_ms\":{:.2},\"resize_p95_ms\":{:.2},\
             \"resize_p99_ms\":{:.2}}}",
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
            resize.count(),
            p(resize, 0.50),
            p(resize, 0.95),
            p(resize, 0.99),
        ));
    }
    if command.trim() == "reset" {
        crate::metrics::reset();
    }
    let (rows, cols) = try_metrics_dims(term).map_or_else(
        || ("null".to_string(), "null".to_string()),
        |(rows, cols)| (rows.to_string(), cols.to_string()),
    );
    let m = crate::metrics::snapshot();
    let backend = if m.backend_gpu { "gpu" } else { "cpu" };
    json_ok(&format!(
        "{{\"backend\":\"{backend}\",\"rows\":{rows},\"cols\":{cols},\
         \"frames\":{},\"last_present_latency_ms\":{:.2},\"max_present_latency_ms\":{:.2},\
         \"last_frame_render_ms\":{:.2},\"max_frame_render_ms\":{:.2},\"slow_frames\":{},\
         \"slow_threshold_ms\":{:.1},\"last_input_present_ms\":{:.2},\
         \"max_input_present_ms\":{:.2},\"last_key_write_ms\":{:.2},\
         \"max_key_write_ms\":{:.2},\"last_resize_present_ms\":{:.2},\
         \"max_resize_present_ms\":{:.2},\"last_resize_reflow_ms\":{:.2},\
         \"max_resize_reflow_ms\":{:.2},\"sync_armed\":{},\"sync_rel_end\":{},\
         \"sync_rel_timeout\":{},\"sync_holding\":{},\"perf_reduced\":{},\
         \"shed_transitions\":{},\"wake_heals\":{},\"last_redraw_total_ms\":{:.2},\
         \"max_redraw_total_ms\":{:.2},\"redraw_attempts\":{},\"redraw_early_outs\":{},\
         \"redraw_sync_holds\":{},\"redraw_retry_gated\":{},\"pre_present_attempts\":{},\
         \"last_pre_present_ms\":{:.2},\"pre_present_total_ms\":{:.2},\
         \"max_pre_present_ms\":{:.2},\"present_drops\":{},\
         \"last_present_drop_reason\":\"{}\",\"last_present_drop_parked\":{},\
         \"event_wakes\":{},\"timer_wakes\":{},\"wait_cancelled_wakes\":{},\
         \"poll_wakes\":{},\"wake_kind\":\"{}\",\"wake_owner\":\"{}\",\
         \"wake_late_ms\":{:.2},\"deadline_owner\":\"{}\",\"deadline_in_ms\":{:.2},\
         \"deadline_late_ms\":{:.2},\"past_deadline_arms\":{},\"max_frame_gap_ms\":{:.2},\
         \"rust_main_to_first_present_ms\":{:.2},\
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
         \"first_present_ms\":{:.2}}}",
        m.frames_presented,
        ms(m.last_present_latency_ns),
        ms(m.max_present_latency_ns),
        ms(m.last_frame_render_ns),
        ms(m.max_frame_render_ns),
        m.slow_frames,
        ms(crate::metrics::SLOW_FRAME_THRESHOLD_NS),
        ms(m.last_input_present_ns),
        ms(m.max_input_present_ns),
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
        m.pre_present_attempts,
        ms(m.last_pre_present_ns),
        ms(m.pre_present_total_ns),
        ms(m.max_pre_present_ns),
        m.present_drops,
        m.last_present_drop_reason.as_str(),
        m.last_present_drop_parked,
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
        ms(m.max_frame_gap_ns),
        ms(m.rust_main_to_first_present_ns),
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
        ms(m.first_present_ns),
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
            let incomplete = if search.results.incomplete {
                " incomplete"
            } else {
                ""
            };
            let mut out = format!("OK {}{incomplete}\n", search.results.matches.len());
            for m in &search.results.matches {
                // m.line is the ABSOLUTE row (the index is keyed by absolute row).
                out.push_str(&format!("{} {} {}\n", m.line, m.start_col, m.len()));
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
    /// False when an alt-screen/content transition occurred during the chunked
    /// snapshot. Such a mixed snapshot is never safe for GUI highlighting.
    pub(crate) consistent: bool,
}

fn query_search_index(
    index: &TerminalSearch,
    query: &str,
    case_sensitive: bool,
    is_regex: bool,
    direction: EngineSearchDirection,
    anchor: Option<(usize, usize)>,
    strict: bool,
) -> Result<(SearchResults, Option<SearchMatch>), SearchOptionsError> {
    let results =
        index.search_results_opts_direction(query, case_sensitive, is_regex, direction)?;
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
            index.find_direction_opts(
                query,
                case_sensitive,
                is_regex,
                aterm_search::DirectedFind {
                    anchor,
                    direction,
                    inclusive: !strict,
                    wrap: true,
                },
            )?
        }
    } else {
        None
    };
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

    // Cache hit: clone the immutable index handle while holding the process-global
    // cache lock, then run the potentially large query after releasing it. Searches
    // in another window can therefore proceed concurrently instead of serializing
    // behind a common one-character query over a deep history.
    let cached_index =
        cached_search_index(term, key_alt, key_seq, absolute_row_revision, max_lines);
    if let Some(index) = cached_index {
        let (results, point_match) = query_search_index(
            &index,
            query,
            case_sensitive,
            is_regex,
            direction,
            anchor,
            strict,
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
            consistent,
        });
    }

    // Miss: snapshot only the newest suffix the configured index can retain.
    // Copying/indexing an older prefix merely to trigger repeated 25%-evictions
    // made cold search scale with unsearchable history (and churn O(n log n)
    // key sorts). `abs_row_text` converts each retained absolute row against the
    // LIVE frame at read time, so a row that scrolls off mid-snapshot reads as
    // evicted (empty) — exactly what a fresh build under the shifted frame lacks.
    let total = scrollback + visible_rows;
    let retained_total = total.min(max_lines);
    let skipped_prefix = total.saturating_sub(retained_total);
    let retained_oldest = oldest.saturating_add(skipped_prefix as u64);
    let retained_base = usize::try_from(retained_oldest).unwrap_or(usize::MAX);
    let indexed_end = retained_base.saturating_add(retained_total);
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
    let snapshot_lines = indexed_end.saturating_sub(snapshot_start);
    let snapshot_start_u64 = u64::try_from(snapshot_start).unwrap_or(u64::MAX);
    let mut lines: Vec<String> = Vec::with_capacity(snapshot_lines);
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
            let text = match abs_row_text(
                &t,
                snapshot_start_u64.saturating_add(u64::try_from(j).unwrap_or(u64::MAX)),
            ) {
                AbsRow::Text(s) => s,
                AbsRow::Evicted | AbsRow::OutOfRange => String::new(),
            };
            lines.push(text);
        }
    }

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
            consistent: full.consistent,
        });
    };

    let point_match = index.find_direction_opts(
        query,
        case_sensitive,
        is_regex,
        aterm_search::DirectedFind {
            anchor,
            direction,
            inclusive: !strict,
            wrap: true,
        },
    )?;
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
pub(crate) fn cmd_title(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    format!("OK {}\n", t.title())
}

/// `cwd` -> `OK <working directory>\n` (the shell's directory as reported via
/// OSC 7; empty if never reported). Lets an introspecting client know where
/// commands will run without scraping the prompt.
pub(crate) fn cmd_cwd(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    // The cwd is percent-decoded from OSC 7, so it can hold raw newlines / C0 /
    // C1 / BiDi-override bytes. pct_encode it (as sibling verbs do) so it stays a
    // single token and cannot forge extra control-protocol reply lines.
    format!(
        "OK {}\n",
        pct_encode(t.current_working_directory().unwrap_or(""))
    )
}

/// `text --json` -> `{"rows":["<row0>",...],"cursor":{...},"seq":N,"dims":{...}}`.
/// The rows are the SAME grapheme-faithful, control-collapsed, tail-trimmed lines
/// `cmd_text` emits, the cursor/dims mirror the `cursor`/`dims` verbs, and `seq`
/// is the engine `content_seq` (so an agent can diff frames without re-reading).
pub(crate) fn cmd_text_json(term: &Arc<Mutex<Terminal>>) -> String {
    let t = term_lock(term);
    let rows = t.rows() as usize;
    let cols = t.cols();
    let mut row_items: Vec<String> = Vec::with_capacity(rows);
    for r in 0..rows {
        row_items.push(format!("\"{}\"", json_escape(&visible_row(&t, r))));
    }
    let c = t.cursor();
    let vis = t.cursor_visible();
    let style = cursor_style_name(t.cursor_style());
    json_ok(&format!(
        "{{\"rows\":[{}],\"cursor\":{{\"row\":{},\"col\":{},\"visible\":{vis},{}}},\
         \"dims\":{{\"rows\":{rows},\"cols\":{cols}}},\"seq\":{}}}",
        row_items.join(","),
        c.row,
        c.col,
        json_str_field("style", style),
        t.content_seq(),
    ))
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
fn write_styled_cell_json(out: &mut String, snap: &StyledCellSnap) {
    let cell = &snap.cell;
    out.push_str("{\"glyph\":\"");
    crate::control::json_escape_into(out, &snap.glyph);
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
fn distinct_images(t: &Terminal) -> Vec<(usize, usize, std::sync::Arc<ImageData>)> {
    let mut seen: Vec<*const ImageData> = Vec::new();
    let mut out: Vec<(usize, usize, std::sync::Arc<ImageData>)> = Vec::new();
    for r in 0..t.rows() as usize {
        for (col, iref) in t.images_row(r) {
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
/// combining-aware grapheme, the raw `WIDE` lead flag (`cell_attrs`, NOT the
/// resolved `RenderCell::wide`), and the OSC 8 hyperlink target. All copied
/// under the terminal lock; serialized without it.
struct StyledCellSnap {
    cell: RenderCell,
    glyph: String,
    wide_lead: bool,
    hyperlink: Option<String>,
    /// The OSC-8 `id=` grouping key, if the link carried one. Two adjacent runs with
    /// the SAME url but DIFFERENT id are DISTINCT clickable regions; the real GUI
    /// renderer groups hover/click spans by id, so a lossless mirror needs it to
    /// reproduce the grouping — the url alone cannot. `None` when the link had no id.
    hyperlink_id: Option<String>,
}

/// Side-adjusted selection geometry for the styled control frame.
///
/// The old frame serialized only `TextSelection::normalized_bounds()`. That lost
/// both the selection kind (a block and a linear selection can share the same
/// endpoints but paint different cells) and the anchors' half-cell sides. Store
/// the renderer-equivalent `TextSelection::project_range` result instead.
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
    cells: Vec<Vec<StyledCellSnap>>,
    line_sizes: Vec<&'static str>,
    /// Renderer-equivalent selected range (or `None`).
    selection: Option<StyledSelectionSnap>,
    /// OSC 17 selection fill, or `None` for the renderer/theme default.
    selection_bg: Option<[u8; 3]>,
    /// OSC 19 selected-text ink, or `None` for automatic contrast.
    selection_fg: Option<[u8; 3]>,
    /// Distinct inline images as `Arc` clones; base64 happens at serialize time.
    images: Vec<(usize, usize, std::sync::Arc<ImageData>)>,
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
    let mut cells: Vec<Vec<StyledCellSnap>> = Vec::with_capacity(rows);
    let mut line_sizes: Vec<&'static str> = Vec::with_capacity(rows);
    for r in 0..rows {
        // LIVE frame (offset-INDEPENDENT): colours+attrs from `render_row_at_screen`,
        // glyph from the live `cell_grapheme`, wide-lead from the live raw grid flag
        // (`RenderCell::wide` means the RIGHT-HALF continuation), and the
        // offset-blind `hyperlink_at` — so all four reads share one live frame and
        // never stitch a scrolled-back row's colours onto a live glyph.
        let rendered = t.render_row_at_screen(r);
        let mut row_cells: Vec<StyledCellSnap> = Vec::with_capacity(cols);
        for c in 0..cols {
            let rc = rendered.get(c).copied();
            row_cells.push(StyledCellSnap {
                cell: rc.unwrap_or(blank),
                glyph: t.cell_grapheme(r, c).unwrap_or_default(),
                wide_lead: cell_attrs(t.grid(), r, c).contains(CellFlags::WIDE),
                hyperlink: t.hyperlink_at(r as u16, c as u16).map(str::to_string),
                hyperlink_id: t.hyperlink_id_at(r as u16, c as u16).map(str::to_string),
            });
        }
        cells.push(row_cells);
        line_sizes.push(line_size_name(row_line_size(t, r))); // F2: DEC double-width/height
    }
    // F3: text selection highlight (a human/peer-initiated selection a watcher
    // would otherwise miss). `project_range` applies the SAME half-cell side
    // adjustment and line expansion as the renderer; the explicit kind preserves
    // block-vs-linear geometry when endpoints happen to match.
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
    let cell_count = snap.cells.iter().map(Vec::len).sum::<usize>();
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
    for (r, row_cells) in snap.cells.iter().enumerate() {
        if r > 0 {
            out.push(',');
        }
        out.push('[');
        for (c, cell) in row_cells.iter().enumerate() {
            if c > 0 {
                out.push(',');
            }
            write_styled_cell_json(&mut out, cell);
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
        rows: _,                  // frame "dims.rows"
        cols: _,                  // frame "dims.cols"
        cells: _,                 // frame "rows" (per-cell styled_cell_json)
        cursor_row: _,            // frame "cursor.row"
        cursor_col: _,            // frame "cursor.col"
        cursor_visible: _,        // frame "cursor.visible"
        cursor_style: _,          // frame "cursor.style"
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
        clusters: _,     // folded into per-cell "glyph" (cell_grapheme)
        combining: _,    // folded into per-cell "glyph" (cell_grapheme)
        line_sizes: _,   // frame "line_sizes" (F2)
        line_size_spans: _, // OMITTED: compose-time per-pane refinement of `line_sizes`. This frame is extracted from ONE Terminal, whose rows are uniform, so it is always empty here; the split-pane composite is not the styled-frame source.
        default_bg_spans: _, // OMITTED: compose-time per-pane refinement of `default_bg`, empty for a single-Terminal frame; each cell already carries its own resolved bg.
        images: _,           // frame "images" (F1)
        default_bg: _, // OMITTED: engine-resolved live default-bg for padding, not per-cell content (cells carry their own bg)
        cursor_color: _, // frame "cursor.color" (fixed RGB or "default")
        snapshot_seq: _, // frame "seq" (the engine content version stamp)
        input_hot: _, // OMITTED: present-time bloom-defer latency hint, display-only (not cell content)
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
         \"session\":{},\"cell_w\":{},\"cell_h\":{},\"font_px\":{:.2},\
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
        cmd_cell, cmd_search, gather_styled_frame, serialize_dims, serialize_dims_json,
        styled_frame_payload,
    };
    use crate::term_lock;

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
            assert_eq!(snap.cells[0][0].cell.fg, [0x11, 0x22, 0x33]);
            assert_eq!(snap.cells[0][0].cell.bg, [0x44, 0x55, 0x66]);
            assert_eq!(snap.cells[0][7].cell.fg, snap.cells[0][0].cell.fg);
            assert_eq!(
                snap.cells[0][7].cell.bg, snap.cells[0][0].cell.bg,
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
            assert_eq!(snap.cells[0][0].cell.fg, [0x44, 0x55, 0x66]);
            assert_eq!(snap.cells[0][0].cell.bg, [0x11, 0x22, 0x33]);
            assert_eq!(snap.cells[0][7].cell.fg, snap.cells[0][0].cell.fg);
            assert_eq!(snap.cells[0][7].cell.bg, snap.cells[0][0].cell.bg);
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
                snap.cells[0][7].cell.fg,
                [configured.0.r, configured.0.g, configured.0.b]
            );
            assert_eq!(
                snap.cells[0][7].cell.bg,
                [configured.1.r, configured.1.g, configured.1.b]
            );
        }
    }

    #[test]
    fn styled_frame_distinguishes_wide_lead_from_continuation() {
        let mut term = Terminal::new(2, 8);
        term.process("界".as_bytes());
        let snap = gather_styled_frame(&term);

        assert_eq!(snap.cells[0][0].glyph, "界");
        assert!(
            snap.cells[0][0].wide_lead,
            "the raw WIDE flag belongs to the glyph's lead cell"
        );
        assert!(
            !snap.cells[0][0].cell.wide,
            "RenderCell::wide is not the lead marker"
        );
        assert!(
            !snap.cells[0][1].wide_lead,
            "the continuation must not duplicate the lead marker"
        );
        assert!(
            snap.cells[0][1].cell.wide,
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
            super::take_last_search_snapshot_lines() <= 5,
            "ordinary output refreshes the four visible rows plus at most one appended row"
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
            "last_pre_present_ms=",
            "pre_present_total_ms=",
            "last_present_drop_reason=",
            "wake_owner=",
            "deadline_owner=",
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
        let value: serde_json::Value = serde_json::from_str(body).expect("valid metrics JSON");
        for key in [
            "redraw_attempts",
            "redraw_early_outs",
            "redraw_retry_gated",
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
            "first_present_ms",
        ] {
            assert!(
                value.get(key).is_some(),
                "metrics JSON omitted `{key}`: {reply}"
            );
        }

        let pct = super::cmd_metrics_json(None, "percentiles");
        let pct_body = pct.strip_prefix("OK 1\n").unwrap().trim_end();
        let pct_value: serde_json::Value = serde_json::from_str(pct_body).unwrap();
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
        ] {
            assert!(
                text_pct.contains(field),
                "text percentiles omitted `{field}`: {text_pct}"
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
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(value["rows"].is_null());
        assert!(value["cols"].is_null());
        assert!(value.get("redraw_attempts").is_some());
    }
}
