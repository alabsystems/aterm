// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude's mode pill relabelled on the glass (`YOLO on` / `YOLO off`,
//! owner 2026-10-19) and the glass→grid column map behind it
//! ([`collect_pill_relabel`], [`GlassColumns`], `App::pixel_to_cell`).

use super::*;

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use aterm_session::sink::SinkWriter;
use aterm_window::event::{ElementState, MouseButton};

use crate::term_lock;

/// Claude Code's foreground process group, as the resolver names it.
const PGID: i32 = 4242;
const COLS: usize = 120;
/// The owner's footer row: the bypass pill, the shells, the agents.
const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
const MANUAL: &str = "  \u{23F8} manual mode on \u{B7} ? for shortcuts \u{B7} \u{2190} for agents";
const AUTO: &str = "  \u{23F5}\u{23F5} auto mode on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
/// Vim INSERT's indicator and its one cell, before the pill (MEASURED on
/// 2.1.304: the indicator dim at column 2, the pill at column 15).
const VIM_LEAD: &str = "  -- INSERT -- ";
/// The owner's footer row under vim INSERT: the indicator, then [`BYPASS`]'s
/// pill and items.
const VIM_BYPASS: &str = "  -- INSERT -- \u{23F5}\u{23F5} bypass permissions on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
/// The same row in auto mode, the measured screen's.
const VIM_AUTO: &str = "  -- INSERT -- \u{23F5}\u{23F5} auto mode on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
/// The pill's ink and the rest of the row's, as `draw` paints them.
const PILL_FG: [u8; 3] = [255, 110, 90];
const DIM_FG: [u8; 3] = [130, 130, 130];
/// Claude's right-hand notice, at the row's end.
const NOTICE: &str = "/rc active";

/// A headless App, `COLS` wide, whose one session writes its PTY input to
/// a private socket, and no harness host: its composer wears no harness
/// border, so nothing relabels Claude's pill.
fn bare() -> (App, WindowId, u64, UnixStream) {
    let (reader, writer) = UnixStream::pair().expect("private PTY observer");
    reader.set_nonblocking(true).expect("nonblocking observer");
    let mut app =
        App::headless_for_test_with_sink(std::sync::Arc::new(SinkWriter::new_owned(writer.into())));
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, COLS as u16));
    (app, wid, session, reader)
}

/// This window's harness host, holding the session; dropped, it shuts
/// down.
struct Held(crate::harness_host::HostHandle);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// [`bare`] with a harness host holding its session (Cruise, the table's
/// default): the composer wears aterm's border and its switch, the gate the
/// relabel follows (`relabels_pill`).
fn app() -> (App, WindowId, u64, UnixStream, Held) {
    let (mut app, wid, session, reader) = bare();
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    (app, wid, session, reader, Held(host))
}

fn feed(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("session").term.clone();
    term_lock(&term).process(bytes);
}

/// The session is Claude Code's, with its facts published: what the
/// footer, and the relabel, are drawn for.
fn claude(app: &App, session: u64) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some("claude".into()));
    assert!(timeline.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }),
    ));
}

/// Claude Code's bottom block on the 24-row engine: a transcript row, the
/// composer between its rules (the caret at row 22, column 3), and `mode`
/// on the last row — vim INSERT's indicator ([`VIM_LEAD`]), where it leads,
/// in [`DIM_FG`], its pill to `on` in [`PILL_FG`], the rest in [`DIM_FG`],
/// [`NOTICE`] right-aligned.
fn draw(app: &App, session: u64, mode: &str) {
    let rule = "\u{2500}".repeat(COLS);
    let lead = if mode.starts_with(VIM_LEAD) {
        VIM_LEAD.len()
    } else {
        0
    };
    let at = mode.find(" on").map_or(mode.len(), |i| i + 3);
    let pad = COLS - 2 - mode.chars().count() - NOTICE.len();
    let [pr, pg, pb] = PILL_FG;
    let [dr, dg, db] = DIM_FG;
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{rule}\x1b[22;1H\u{276F} \
         \x1b[23;1H{rule}\x1b[24;1H\x1b[38;2;{dr};{dg};{db}m{}\x1b[38;2;{pr};{pg};{pb}m{}\x1b[38;2;{dr};{dg};{db}m{}{}{NOTICE}\x1b[0m\x1b[22;3H",
        &mode[..lead],
        &mode[lead..at],
        &mode[at..],
        " ".repeat(pad),
    );
    feed(app, session, screen.as_bytes());
}

/// One frame through the capture route's splices: extract, the strip, the
/// footer.
fn frame(app: &mut App, wid: WindowId) {
    let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(Instant::now()),
    );
    let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
        panic!("the headless capture must produce a frame");
    };
    app.splice_tab_strip(wid);
    app.splice_claude_footer(
        wid,
        &plan,
        VisibleContentRoute::Terminal {
            composed: grid.composed,
        },
    );
}

fn strip(app: &App, wid: WindowId) -> usize {
    app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
}

/// Claude's mode row, as a frame row.
fn mode_row(app: &App, wid: WindowId) -> usize {
    strip(app, wid) + 23
}

fn glass_of(app: &App, wid: WindowId, row: usize) -> String {
    read_glass(&app.windows[&wid].input_scratch.cells[row])
}

/// The engine's own text of terminal row `row`: what `text`/`screen` read.
fn grid_text(app: &App, session: u64, row: usize) -> String {
    let term = app.pool.get(session).expect("session").term.clone();
    term_lock(&term).row_text(row).expect("the row")
}

/// The window pixel at the centre of frame cell `(row, col)`.
fn px_of(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + row * ch + ch / 2) as f64;
    assert_eq!(
        app.frame_cell_at(wid, x, y),
        Some((row, col)),
        "the pixel names the cell"
    );
    (x, y)
}

/// What the session's PTY received: up to a minute for `want` bytes (a
/// hang detector), and every byte already written past them.
fn pty(reader: &mut UnixStream, want: usize) -> Vec<u8> {
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if got.len() >= want || Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("reading the PTY observer: {e}"),
        }
    }
    got
}

/// The column `needle` starts at in `row`, counted in chars (= cells here).
fn col_of(row: &str, needle: &str) -> usize {
    row[..row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} in {row:?}"))]
        .chars()
        .count()
}

/// THE BYPASS PILL READS `YOLO on` ON THE GLASS: in the pill's own ink, the
/// row's left items packed left behind it, the right-hand notice where
/// Claude drew it — while the GRID keeps Claude's row (`text`/`screen` and
/// the supervisor read it), and the undo puts every cell back. The gate's
/// witness: the composer border aterm draws on the same pane. Manual mode
/// reads `YOLO off` the same way.
#[test]
fn the_bypass_pill_reads_yolo_on_on_the_glass_and_the_grid_keeps_claudes_row() {
    for (mode, label) in [(BYPASS, "YOLO on"), (MANUAL, "YOLO off")] {
        let (mut app, wid, session, _reader, _host) = app();
        draw(&app, session, mode);
        frame(&mut app, wid);
        let row = mode_row(&app, wid);
        let vendor = app.windows[&wid].input_scratch.cells[row].clone();
        let drawn = read_glass(&vendor);
        claude(&app, session);
        frame(&mut app, wid);
        let top = glass_of(&app, wid, strip(&app, wid) + 20);
        assert!(
            top.contains("alab harness"),
            "the border is aterm's: {top:?}"
        );

        let glass = glass_of(&app, wid, row);
        let tail = &mode[mode.find(" on").unwrap() + 3..];
        assert!(
            glass.starts_with(&format!("  {label}{tail}")),
            "{label}: packed behind the label: {glass:?}"
        );
        assert_eq!(
            col_of(&glass, NOTICE),
            col_of(&drawn, NOTICE),
            "the notice stays"
        );
        let cells = &app.windows[&wid].input_scratch.cells[row];
        for (k, ch) in label.chars().enumerate() {
            assert_eq!(
                (cells[2 + k].ch, cells[2 + k].fg),
                (ch, PILL_FG),
                "{label}: {k}"
            );
        }
        assert_eq!(
            cells[2 + label.len() + 1].fg,
            DIM_FG,
            "the run keeps its ink"
        );
        assert_eq!(cells.len(), vendor.len());
        // The grid is Claude's, whatever the glass shows.
        assert_eq!(grid_text(&app, session, 23).trim_end(), drawn.trim_end());
        // The undo restores every cell.
        let ws = app.windows.get_mut(&wid).unwrap();
        assert!(undo(&mut ws.claude_footer_undo, &mut ws.input_scratch));
        assert_eq!(ws.input_scratch.cells[row], vendor, "{label}: undone");
        assert_eq!(
            ws.claude_footer_undo.grid_col(23, 12),
            12,
            "the map went with it"
        );
    }
}

/// CLAUDE'S HIGHLIGHT MOVES WITH ITS ITEM (the cross-track review of
/// 2026-10-19): Claude draws the item under the pointer — or the one `↓`
/// selected — in inverse (SGR 7); the relabel holds, and the highlighted
/// cells keep their own style where the packed run moves them, so the row
/// never snaps back to Claude's under the pointer. NEGATIVE CONTROL: a pill
/// cell of a ground of its own is no plain pill, and the row stays Claude's.
#[test]
fn claudes_inverse_highlight_moves_with_its_item() {
    let shells = "14 shells";
    for pill_lit in [false, true] {
        let (mut app, wid, session, _reader, _host) = app();
        draw(&app, session, BYPASS);
        let at = col_of(BYPASS, shells);
        let [dr, dg, db] = DIM_FG;
        feed(
            &app,
            session,
            format!(
                "\x1b7\x1b[24;{}H\x1b[7;38;2;{dr};{dg};{db}m{shells}\x1b[0m\x1b8",
                at + 1
            )
            .as_bytes(),
        );
        if pill_lit {
            feed(
                &app,
                session,
                b"\x1b7\x1b[24;3H\x1b[48;2;40;40;90m\xe2\x8f\xb5\x1b[0m\x1b8",
            );
        }
        frame(&mut app, wid);
        let row = mode_row(&app, wid);
        let vendor = app.windows[&wid].input_scratch.cells[row].clone();
        let drawn = read_glass(&vendor);
        claude(&app, session);
        frame(&mut app, wid);
        let glass = glass_of(&app, wid, row);
        if pill_lit {
            assert_eq!(glass, drawn, "NEGATIVE CONTROL: Claude's row stands");
            continue;
        }
        assert!(
            glass.starts_with(&format!("  YOLO on \u{B7} {shells}")),
            "the relabel holds: {glass:?}"
        );
        let moved = col_of(&glass, shells);
        assert!(moved < at, "CONTROL: packed left ({moved} < {at})");
        let cells = &app.windows[&wid].input_scratch.cells[row];
        for k in 0..shells.len() {
            assert_eq!(
                cells[moved + k],
                vendor[at + k],
                "the highlight's cell {k} moved with its text"
            );
        }
        assert_ne!(
            vendor[at].bg,
            vendor[at - 2].bg,
            "CONTROL: Claude's highlight has a ground of its own"
        );
        assert_eq!(grid_text(&app, session, 23).trim_end(), drawn.trim_end());
    }
}

/// Negative controls: the other modes keep Claude's pill whole; the
/// harness's master switch off (no border drawn) relabels nothing, and nor
/// does a window with no harness host; the cursor on the row, or a
/// selection in the window, leaves the row as drawn.
#[test]
fn no_relabel_where_the_row_is_not_plainly_claudes_or_the_harness_is_off() {
    let vendor_after = |mode: &str, setup: &dyn Fn(&mut App, WindowId, u64)| {
        let (mut app, wid, session, _reader, _host) = app();
        draw(&app, session, mode);
        frame(&mut app, wid);
        let row = mode_row(&app, wid);
        claude(&app, session);
        setup(&mut app, wid, session);
        frame(&mut app, wid);
        (
            glass_of(&app, wid, row),
            grid_text(&app, session, 23),
            app.windows[&wid].claude_footer_undo.grid_col(23, 12),
        )
    };
    let none = |_: &mut App, _: WindowId, _: u64| {};
    let (glass, vendor, col) = vendor_after(AUTO, &none);
    assert_eq!(
        glass.trim_end(),
        vendor.trim_end(),
        "auto mode keeps its pill"
    );
    assert_eq!(col, 12);
    let off = |app: &mut App, _: WindowId, _: u64| {
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
        );
    };
    let (glass, vendor, col) = vendor_after(BYPASS, &off);
    assert_eq!(
        glass.trim_end(),
        vendor.trim_end(),
        "harness off: Claude's row"
    );
    assert_eq!(col, 12);
    // The window lets its host go (the guard shuts it down after the run).
    let unhosted = |app: &mut App, _: WindowId, _: u64| app.harness = None;
    let (glass, vendor, col) = vendor_after(BYPASS, &unhosted);
    assert_eq!(glass.trim_end(), vendor.trim_end(), "no host: Claude's row");
    assert_eq!(col, 12);
    let cursor = |app: &mut App, _: WindowId, session: u64| feed(app, session, b"\x1b[24;40H");
    let (glass, vendor, _) = vendor_after(BYPASS, &cursor);
    assert_eq!(glass.trim_end(), vendor.trim_end(), "the cursor on the row");
    let select = |app: &mut App, wid: WindowId, _: u64| {
        let (x, y) = px_of(app, wid, (strip(app, wid) + 18, 0));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (x, y) = px_of(app, wid, (strip(app, wid) + 18, 5));
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    };
    let (glass, vendor, _) = vendor_after(BYPASS, &select);
    assert_eq!(
        glass.trim_end(),
        vendor.trim_end(),
        "a selection in the window"
    );
}

/// A CLICK ON THE GLASS'S `14 shells` IS A CLICK ON CLAUDE'S: with the
/// mouse tracked (SGR 1000/1006) the press reports the GRID column of
/// Claude's own `14 shells`, not the glass column it shows at — and the
/// notice past the run, as drawn. CONTROL: with nothing relabelled (auto
/// mode) every column reports itself.
#[test]
fn a_press_on_the_relabelled_row_reports_claudes_column() {
    for (mode, relabelled) in [(BYPASS, true), (AUTO, false)] {
        let (mut app, wid, session, mut reader, _host) = app();
        draw(&app, session, mode);
        claude(&app, session);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let row = mode_row(&app, wid);
        let glass = glass_of(&app, wid, row);
        let grid = grid_text(&app, session, 23);
        assert_eq!(glass.starts_with("  YOLO on"), relabelled, "{glass:?}");
        let mut press = |col: usize| {
            let (x, y) = px_of(&app, wid, (row, col));
            app.on_cursor_moved(wid, x, y);
            let _ = pty(&mut reader, 0);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            let want = b"\x1b[<0;".len();
            let got = String::from_utf8(pty(&mut reader, want + 6)).unwrap();
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
            let _ = pty(&mut reader, 0);
            got
        };
        let shells = col_of(&glass, "14 shells");
        assert_eq!(
            press(shells),
            format!("\x1b[<0;{};24M", col_of(&grid, "14 shells") + 1),
            "{mode}: the glass's `14 shells` is Claude's"
        );
        let notice = col_of(&glass, NOTICE);
        assert_eq!(
            press(notice),
            format!("\x1b[<0;{};24M", notice + 1),
            "{mode}: the notice"
        );
    }
}

/// AFTER VIM INSERT'S INDICATOR (`  -- INSERT -- ⏵⏵ bypass permissions on
/// …`, MEASURED on 2.1.304) the bypass pill reads `YOLO on` at column 15,
/// in the pill's own ink, the indicator's cells as Claude drew them before
/// it and the row's items packed behind it; with the mouse tracked, a press
/// on the glass's `14 shells` reports the GRID column of Claude's own, and
/// one on the indicator its own column. The grid keeps Claude's row.
/// NEGATIVE CONTROL: the auto pill after the indicator keeps Claude's
/// words, every column reporting itself.
#[test]
fn the_pill_after_vim_inserts_indicator_relabels_at_its_own_column() {
    for (mode, relabelled) in [(VIM_BYPASS, true), (VIM_AUTO, false)] {
        let (mut app, wid, session, mut reader, _host) = app();
        draw(&app, session, mode);
        frame(&mut app, wid);
        let row = mode_row(&app, wid);
        let vendor = app.windows[&wid].input_scratch.cells[row].clone();
        claude(&app, session);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let glass = glass_of(&app, wid, row);
        let grid = grid_text(&app, session, 23);
        assert_eq!(
            glass.starts_with(&format!("{VIM_LEAD}YOLO on \u{B7} 14 shells")),
            relabelled,
            "{glass:?}"
        );
        assert_eq!(grid.trim_end(), read_glass(&vendor).trim_end(), "the grid");
        if !relabelled {
            assert_eq!(glass.trim_end(), grid.trim_end(), "CONTROL: Claude's row");
        }
        let lead = VIM_LEAD.len();
        let cells = &app.windows[&wid].input_scratch.cells[row];
        assert_eq!(cells[..lead], vendor[..lead], "the indicator as drawn");
        assert_eq!(cells[2].fg, DIM_FG, "the indicator's ink");
        if relabelled {
            for (k, ch) in "YOLO on".chars().enumerate() {
                assert_eq!(
                    (cells[lead + k].ch, cells[lead + k].fg),
                    (ch, PILL_FG),
                    "{k}"
                );
            }
        }
        // The press's report, read for as many bytes as `want` holds.
        let mut press = |col: usize, want: &str| {
            let (x, y) = px_of(&app, wid, (row, col));
            app.on_cursor_moved(wid, x, y);
            let _ = pty(&mut reader, 0);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            let got = String::from_utf8(pty(&mut reader, want.len())).unwrap();
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
            let _ = pty(&mut reader, 0);
            got
        };
        let shells = format!("\x1b[<0;{};24M", col_of(&grid, "14 shells") + 1);
        assert_eq!(
            press(col_of(&glass, "14 shells"), &shells),
            shells,
            "{mode}: the glass's `14 shells` is Claude's"
        );
        let indicator = "\x1b[<0;6;24M";
        assert_eq!(press(5, indicator), indicator, "{mode}: the indicator");
    }
}

/// THE RELABEL ALLOCATES NOTHING once the splice's buffers are grown: the
/// finder, the collector into the reused lists, and the map's push and
/// lookup.
#[test]
fn the_relabel_allocates_nothing_once_its_buffers_are_grown() {
    let (mut app, wid, session, _reader, _host) = app();
    draw(&app, session, BYPASS);
    frame(&mut app, wid);
    let scratch = app.windows[&wid].input_scratch.clone();
    let rows: Vec<String> = (0..24)
        .map(|r| {
            let row = &scratch.cells[strip(&app, wid) + r];
            row.iter().map(|c| c.ch).collect()
        })
        .collect();
    let region = PaneTextRegion {
        strip: strip(&app, wid),
        row_off: 0,
        col_off: 0,
        rows: 24,
        cols: COLS,
    };
    let blank = scratch.implicit_blank;
    let mut out = Vec::new();
    let mut cells = Vec::new();
    let mut glass = GlassColumns::default();
    let run =
        |out: &mut Vec<RelabelWrite>, cells: &mut Vec<RenderCell>, glass: &mut GlassColumns| {
            out.clear();
            cells.clear();
            glass.clear();
            let relabel = footer::relabel_under(&rows, 22).expect("the pill");
            collect_pill_relabel(
                &scratch,
                &rows,
                region,
                relabel,
                |_| false,
                blank,
                out,
                cells,
            );
            for e in out.iter() {
                glass.push(e.shift);
            }
            glass.grid_col(23, 12)
        };
    let col = run(&mut out, &mut cells, &mut glass);
    assert_eq!(out.len(), 1, "the relabel is collected");
    assert_eq!(col, 12 + 17, "the glass's column 12 is the grid's 29");
    let (col, allocations) =
        crate::work_counts::allocations_in(|| run(&mut out, &mut cells, &mut glass));
    assert_eq!((col, allocations), (29, 0));
    let _ = session;
}

/// THE PILL IS READ WITH THE RULE, NOT EVERY FRAME (the cross-track review
/// of 2026-10-19): frames at rest — the screen unchanged, or a keystroke's
/// echo in the composer — read Claude's mode row for its relabel no more:
/// the reading kept with the rule's says where the pill is. CONTROL: a
/// change to the mode row reads it once, and the relabel follows it.
#[test]
fn the_pill_is_read_with_the_rule_not_every_frame() {
    let (mut app, wid, session, _reader, _host) = app();
    draw(&app, session, BYPASS);
    claude(&app, session);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let _ = crate::work_counts::take_pill_relabel_reads();
    for _ in 0..5 {
        frame(&mut app, wid);
    }
    feed(&app, session, b"\x1b[22;3Hx");
    frame(&mut app, wid);
    assert_eq!(
        crate::work_counts::take_pill_relabel_reads(),
        0,
        "at rest, and under a keystroke"
    );
    assert!(glass_of(&app, wid, mode_row(&app, wid)).starts_with("  YOLO on"));
    draw(&app, session, MANUAL);
    frame(&mut app, wid);
    assert_eq!(
        crate::work_counts::take_pill_relabel_reads(),
        1,
        "CONTROL: the mode row changed"
    );
    assert!(glass_of(&app, wid, mode_row(&app, wid)).starts_with("  YOLO off"));
}

/// THE PILL FOLLOWS THE BORDER, OFF'S WHITE INCLUDED: a pane this window's
/// host supervised and the owner set Off still wears aterm's composer border
/// (Off's moonbow, `autopilot::harness_theme`), so its bypass pill reads
/// `YOLO on` as a Cruise pane's does. NEGATIVE CONTROL: the master switch
/// off, the border is gone and the row is Claude's.
#[test]
fn a_hosted_off_pane_wears_the_border_and_relabels_with_it() {
    for (what, enabled, relabelled) in [("hosted off", true, true), ("master off", false, false)] {
        let (mut app, wid, session, _reader) = bare();
        let host = crate::harness_host::HostHandle::for_supervised_test(
            Vec::new(),
            std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
        );
        app.harness = Some(host.clone());
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the claim");
        assert!(app.set_session_autopilot(
            session,
            Some(aterm_agent::supervise::Autopilot::Off),
            crate::autopilot::Surface::Chip
        ));
        crate::session_timeline::release_supervisor(
            &ctx,
            None,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        );
        if !enabled {
            app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
                crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
            );
        }
        draw(&app, session, BYPASS);
        claude(&app, session);
        frame(&mut app, wid);
        let (word, view, supervised) = app.session_autopilot(session);
        assert_eq!(
            crate::autopilot::harness_theme(
                view,
                supervised,
                word,
                app.unhosted_glass(Some(aterm_phase::Program::Claude))
            )
            .is_some(),
            relabelled,
            "{what}: the border"
        );
        let glass = glass_of(&app, wid, mode_row(&app, wid));
        assert_eq!(
            glass.starts_with("  YOLO on"),
            relabelled,
            "{what}: {glass:?}"
        );
        if !relabelled {
            assert_eq!(glass.trim_end(), grid_text(&app, session, 23).trim_end());
        }
        if let Some(host) = app.harness.take() {
            host.shutdown_and_join();
        }
    }
}
