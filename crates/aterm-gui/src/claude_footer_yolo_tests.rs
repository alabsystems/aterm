// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claude Code's mode row rewritten on the glass to its jobs, and the glass-column map back to
//! Claude's cells ([`collect_mode_row`], [`GlassColumns`]).

use super::*;

use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use aterm_session::sink::SinkWriter;
use aterm_window::event::{ElementState, MouseButton};

use crate::term_lock;

const PGID: i32 = 4242;
const COLS: usize = 120;
const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
const MANUAL: &str = "  \u{23F8} manual mode on \u{B7} ? for shortcuts \u{B7} \u{2190} for agents";
const AUTO: &str = "  \u{23F5}\u{23F5} auto mode on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
const VIM_LEAD: &str = "  -- INSERT -- ";
const VIM_BYPASS: &str = "  -- INSERT -- \u{23F5}\u{23F5} bypass permissions on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
const VIM_AUTO: &str = "  -- INSERT -- \u{23F5}\u{23F5} auto mode on \u{B7} 14 shells \u{B7} \u{2190} for agents \u{B7} \u{2193} to manage";
const PILL_FG: [u8; 3] = [255, 110, 90];
const DIM_FG: [u8; 3] = [130, 130, 130];
const NOTICE: &str = "/rc active";

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

struct Held(crate::harness_host::HostHandle);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

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

fn claude(app: &App, session: u64) {
    claude_at(app, session, Some("xhigh"));
}

/// [`claude`], its facts naming `effort` (`None`: no effort said yet, as after a relaunch).
fn claude_at(app: &App, session: u64, effort: Option<&str>) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some("claude".into()));
    assert!(timeline.set_claude_footer(
        PGID,
        None,
        Some(FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: effort.map(str::to_owned),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }),
    ));
}

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

fn mode_row(app: &App, wid: WindowId) -> usize {
    strip(app, wid) + 23
}

fn glass_of(app: &App, wid: WindowId, row: usize) -> String {
    read_glass(&app.windows[&wid].input_scratch.cells[row])
}

fn grid_text(app: &App, session: u64, row: usize) -> String {
    let term = app.pool.get(session).expect("session").term.clone();
    term_lock(&term).row_text(row).expect("the row")
}

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

fn col_of(row: &str, needle: &str) -> usize {
    row[..row
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} in {row:?}"))]
        .chars()
        .count()
}

/// What the glass rewrites BYPASS and AUTO to: the job, then the live notice.
const JOBS: &str = "14 shells \u{B7} /rc active";

/// THE MODE ROW READS CLAUDE'S JOBS ON THE GLASS AND THE GRID KEEPS CLAUDE'S ROW: under the
/// gate every known mode's row is rewritten to its jobs and the live notice, each cell-copied
/// in Claude's own style, flush at column 0 and joined by ` · ` in the ink of Claude's own `·`,
/// blanks after; the pill, its hint and the key hints are gone (the mode is the lights block's).
/// The grid keeps Claude's row, and undoing the write puts Claude's cells back and drops the map.
#[test]
fn the_mode_row_reads_claudes_jobs_and_the_grid_keeps_claudes_row() {
    for (mode, kept) in [(BYPASS, JOBS), (MANUAL, NOTICE), (AUTO, JOBS)] {
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
            top.contains("alab claude harness"),
            "the border is aterm's: {top:?}"
        );
        let glass = glass_of(&app, wid, row);
        assert_eq!(glass.trim_end(), kept, "{mode:?}");
        let cells = &app.windows[&wid].input_scratch.cells[row];
        let mut at = 0;
        for item in kept.split(" \u{B7} ") {
            let from = col_of(&drawn, item);
            let n = item.chars().count();
            assert_eq!(
                cells[at..at + n],
                vendor[from..from + n],
                "{item}: Claude's cells"
            );
            at += n;
            if at < kept.chars().count() {
                assert_eq!(
                    (cells[at + 1].ch, cells[at + 1].fg),
                    ('\u{B7}', DIM_FG),
                    "{mode:?}: Claude's separator ink"
                );
                at += 3;
            }
        }
        assert!(
            cells[at..].iter().all(|c| c.ch == ' '),
            "{mode:?}: blanks after"
        );
        assert_eq!(cells.len(), vendor.len());
        assert_eq!(grid_text(&app, session, 23).trim_end(), drawn.trim_end());
        let ws = app.windows.get_mut(&wid).unwrap();
        assert!(undo(&mut ws.claude_footer_undo, &mut ws.input_scratch));
        assert_eq!(ws.input_scratch.cells[row], vendor, "{mode:?}: undone");
        assert_eq!(
            ws.claude_footer_undo.grid_col(23, 0),
            0,
            "the map went with it"
        );
    }
}

/// CLAUDE'S INKS READ ON A LIGHT PAGE: on a tab themed Default Light, a kept item Claude drew in
/// its dark-theme cyan (`#00cccc`, under 2:1 on that page) is laid at the words' floor, 4.5:1 on
/// its ground, its hue kept and nothing else of the cell changed. CONTROL: on the dark default
/// the item's cells are Claude's, byte for byte.
#[test]
fn claudes_inks_read_on_a_light_page() {
    use aterm_effects::cli_marks::NAME_CONTRAST;
    use aterm_effects::color_math::contrast_ratio;
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let shells = "14 shells";
    for light in [true, false] {
        let (mut app, wid, session, _reader, _host) = app();
        if light {
            app.set_session_theme(
                session,
                Some(crate::session_theme::SessionTheme {
                    selector: "Default Light".into(),
                    origin: crate::session_theme::SessionThemeOrigin::Ctl,
                }),
            );
        }
        draw(&app, session, BYPASS);
        let at = col_of(BYPASS, shells);
        feed(
            &app,
            session,
            format!(
                "\x1b7\x1b[24;{}H\x1b[38;2;0;204;204m{shells}\x1b[0m\x1b8",
                at + 1
            )
            .as_bytes(),
        );
        frame(&mut app, wid);
        let row = mode_row(&app, wid);
        let vendor = app.windows[&wid].input_scratch.cells[row].clone();
        claude(&app, session);
        frame(&mut app, wid);
        let glass = glass_of(&app, wid, row);
        assert!(
            glass.starts_with(JOBS),
            "light {light}: the rewrite holds: {glass:?}"
        );
        let cells = &app.windows[&wid].input_scratch.cells[row];
        for k in 0..shells.len() {
            let (cell, claude) = (cells[k], vendor[at + k]);
            if !light || claude.ch == ' ' {
                assert_eq!(cell, claude, "light {light}: Claude's cell {k}");
                continue;
            }
            assert_eq!(claude.fg, [0, 204, 204], "Claude's cyan");
            assert!(
                reads(claude.fg, claude.bg) < NAME_CONTRAST,
                "CONTROL: Claude's ink at {:.2}:1 on the light page",
                reads(claude.fg, claude.bg)
            );
            let floor = reads(cell.fg, cell.bg);
            assert!(floor >= NAME_CONTRAST, "{:?} at {floor:.2}:1", cell.ch);
            let [r, g, b] = cell.fg;
            assert!(
                r < g && g.abs_diff(b) <= 1,
                "{:?}: the cyan's hue kept: {:?}",
                cell.ch,
                cell.fg
            );
            assert_eq!(
                RenderCell {
                    fg: claude.fg,
                    ..cell
                },
                claude,
                "only the ink moves"
            );
        }
    }
}

/// CLAUDE'S INVERSE HIGHLIGHT MOVES WITH ITS ITEM: a kept item Claude highlights keeps its
/// cells' ground at its new column; a highlight on a dropped item stands the row down.
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
        assert!(glass.starts_with(JOBS), "the rewrite holds: {glass:?}");
        let cells = &app.windows[&wid].input_scratch.cells[row];
        for k in 0..shells.len() {
            assert_eq!(
                cells[k],
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

/// NO REWRITE WHERE THE ROW IS NOT PLAINLY CLAUDE'S OR THE HARNESS IS OFF: the harness off, no
/// host, the cursor on the row and a selection in the window each leave Claude's row as drawn
/// and the columns unmapped. CONTROL: auto's row is rewritten like bypass's.
#[test]
fn no_rewrite_where_the_row_is_not_plainly_claudes_or_the_harness_is_off() {
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
            app.windows[&wid].claude_footer_undo.grid_col(23, 0),
        )
    };
    let none = |_: &mut App, _: WindowId, _: u64| {};
    let (glass, _, col) = vendor_after(AUTO, &none);
    assert_eq!(glass.trim_end(), JOBS, "CONTROL: auto is rewritten");
    assert_eq!(
        usize::from(col),
        col_of(AUTO, "14 shells"),
        "CONTROL: and mapped"
    );
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
    assert_eq!(col, 0);
    let unhosted = |app: &mut App, _: WindowId, _: u64| app.harness = None;
    let (glass, vendor, col) = vendor_after(BYPASS, &unhosted);
    assert_eq!(glass.trim_end(), vendor.trim_end(), "no host: Claude's row");
    assert_eq!(col, 0);
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

/// A PRESS ON THE REWRITTEN ROW REPORTS CLAUDE'S COLUMN, AND A VACATED CELL TAKES IT: a press on
/// a kept item reaches Claude at that item's own column; a press, of any button, on a cell the
/// rewrite vacated reaches nothing (never the pill once under it). CONTROL: unhosted, Claude's
/// row takes every press at its own column.
#[test]
fn a_press_on_the_rewritten_row_reports_claudes_column() {
    for rewritten in [true, false] {
        let (mut app, wid, session, mut reader, _host) = app();
        if !rewritten {
            app.harness = None;
        }
        draw(&app, session, BYPASS);
        claude(&app, session);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        let row = mode_row(&app, wid);
        let glass = glass_of(&app, wid, row);
        let grid = grid_text(&app, session, 23);
        assert_eq!(glass.starts_with(JOBS), rewritten, "{glass:?}");
        // Each press reads what reached Claude since the last read, up to `want` bytes (none read
        // for `want` 0).
        let mut press = |col: usize, button: MouseButton, want: usize| {
            let (x, y) = px_of(&app, wid, (row, col));
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, button);
            let got = if want == 0 {
                Vec::new()
            } else {
                pty(&mut reader, want)
            };
            app.on_mouse_input(wid, ElementState::Released, button);
            String::from_utf8(got).unwrap()
        };
        let report = |code: u8, col: usize, end: char| format!("\x1b[<{code};{};24{end}", col + 1);
        let shells = (
            col_of(&glass, "14 shells"),
            report(0, col_of(&grid, "14 shells"), 'M'),
        );
        let released = |press: &str| press.replace('M', "m");
        assert_eq!(
            press(shells.0, MouseButton::Left, shells.1.len()),
            shells.1,
            "rewritten={rewritten}: the glass's `14 shells` is Claude's"
        );
        let notice = report(0, col_of(&grid, NOTICE), 'M');
        let want = format!("{}{notice}", released(&shells.1));
        assert_eq!(
            press(col_of(&glass, NOTICE), MouseButton::Left, want.len()),
            want,
            "rewritten={rewritten}: the notice, after the last press's release"
        );
        let mut last = released(&notice);
        let vacated = JOBS.chars().count() + 4;
        for (button, code) in [(MouseButton::Left, 0), (MouseButton::Middle, 1)] {
            if rewritten {
                // Nothing reaches Claude, neither the press nor its release: the next press's
                // report follows the last release.
                let _ = press(vacated, button, 0);
                let want = format!("{last}{}", shells.1);
                assert_eq!(
                    press(shells.0, MouseButton::Left, want.len()),
                    want,
                    "rewritten: a vacated cell takes the {button:?} press"
                );
                last = released(&shells.1);
            } else {
                let own = report(code, vacated, 'M');
                let want = format!("{last}{own}");
                assert_eq!(
                    press(vacated, button, want.len()),
                    want,
                    "CONTROL: Claude's own cell, {button:?}"
                );
                last = released(&own);
            }
        }
    }
}

/// VIM'S INDICATOR LEADS THE REWRITTEN ROW: `-- INSERT --` keeps Claude's cells and ink at
/// column 0, the jobs after it, each mapped back to Claude's own column.
#[test]
fn vims_indicator_leads_the_rewritten_row() {
    for mode in [VIM_BYPASS, VIM_AUTO] {
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
            glass.trim_end(),
            format!("-- INSERT -- \u{B7} {JOBS}"),
            "{mode:?}"
        );
        assert_eq!(grid.trim_end(), read_glass(&vendor).trim_end(), "the grid");
        let lead = VIM_LEAD.trim().chars().count();
        let cells = &app.windows[&wid].input_scratch.cells[row];
        assert_eq!(cells[..lead], vendor[2..2 + lead], "the indicator as drawn");
        assert_eq!(cells[0].fg, DIM_FG, "the indicator's ink");
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
            "{mode:?}: the glass's `14 shells` is Claude's"
        );
        let indicator = "\x1b[<0;6;24M";
        assert_eq!(press(3, indicator), indicator, "{mode:?}: the indicator");
    }
}

/// THE WRAPPED EFFORT HINT IS BLANKED WITH THE ROW: where Claude's effort hint wrapped to the
/// row under the mode row, that row is blank on the glass and its cells map to nothing; the grid
/// keeps both rows, and undoing the write puts them back.
#[test]
fn the_wrapped_effort_hint_is_blanked_with_the_row() {
    let (mut app, wid, session, _reader, _host) = app();
    let rule = "\u{2500}".repeat(COLS);
    let hint = format!("{:>w$}", "\u{25D0} medium \u{B7} /effort", w = COLS - 2);
    feed(
        &app,
        session,
        format!(
            "\x1b[?2004h\x1b[2J\x1b[18;1H\u{25CF} done\x1b[20;1H{rule}\x1b[21;1H\u{276F} \
             \x1b[22;1H{rule}\x1b[23;1H  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell\
             \x1b[24;1H{hint}\x1b[21;3H"
        )
        .as_bytes(),
    );
    frame(&mut app, wid);
    let (mode, under) = (strip(&app, wid) + 22, strip(&app, wid) + 23);
    let vendor = [mode, under].map(|r| app.windows[&wid].input_scratch.cells[r].clone());
    assert!(read_glass(&vendor[1]).contains("/effort"), "CONTROL: drawn");
    claude(&app, session);
    frame(&mut app, wid);
    assert_eq!(glass_of(&app, wid, mode).trim_end(), "1 shell");
    assert_eq!(glass_of(&app, wid, under).trim(), "", "the hint is blanked");
    assert!(grid_text(&app, session, 23).contains("/effort"), "the grid");
    let ws = app.windows.get_mut(&wid).unwrap();
    assert!(ws.claude_footer_undo.swallows(23, 100), "nothing under it");
    assert!(undo(&mut ws.claude_footer_undo, &mut ws.input_scratch));
    assert_eq!(
        [mode, under].map(|r| ws.input_scratch.cells[r].clone()),
        vendor,
        "undone"
    );
}

/// THE EFFORT HINT STAYS WHILE THE RULE NAMES NO EFFORT: with no effort in the bottom rule's
/// `◆` (right after a relaunch, before the transcript says one), the level of Claude's
/// `◐ medium · /effort` follows the jobs on the rewritten row in Claude's own cells (`◐ medium`,
/// its `/effort` key hint dropped), and a hint wrapped to the row under stands there as drawn.
/// CONTROL: once `◆` names an effort, the hint goes from both.
#[test]
fn the_effort_hint_stays_while_the_rule_names_no_effort() {
    const HINT: &str = "\u{25D0} medium \u{B7} /effort";
    const LEVEL: &str = "\u{25D0} medium";
    for wrapped in [false, true] {
        for named in [false, true] {
            let (mut app, wid, session, _reader, _host) = app();
            let rule = "\u{2500}".repeat(COLS);
            let mode = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell";
            let (row23, row24) = if wrapped {
                (mode.to_owned(), format!("{HINT:>w$}", w = COLS - 2))
            } else {
                (format!("{mode}  {HINT}"), String::new())
            };
            feed(
                &app,
                session,
                format!(
                    "\x1b[?2004h\x1b[2J\x1b[18;1H\u{25CF} done\x1b[20;1H{rule}\x1b[21;1H\u{276F} \
                     \x1b[22;1H{rule}\x1b[23;1H{row23}\x1b[24;1H{row24}\x1b[21;3H"
                )
                .as_bytes(),
            );
            frame(&mut app, wid);
            let (at, under) = (strip(&app, wid) + 22, strip(&app, wid) + 23);
            let vendor = app.windows[&wid].input_scratch.cells[at].clone();
            claude_at(&app, session, named.then_some("medium"));
            frame(&mut app, wid);
            let what = format!("wrapped={wrapped} named={named}");
            let glass = glass_of(&app, wid, at);
            let below = glass_of(&app, wid, under);
            match (wrapped, named) {
                (false, false) => {
                    assert_eq!(
                        glass.trim_end(),
                        format!("1 shell \u{B7} {LEVEL}"),
                        "{what}"
                    );
                    let from = col_of(&read_glass(&vendor), HINT);
                    let n = LEVEL.chars().count();
                    let cells = &app.windows[&wid].input_scratch.cells[at];
                    assert_eq!(cells[10..10 + n], vendor[from..from + n], "Claude's cells");
                }
                (true, false) => {
                    assert_eq!(glass.trim_end(), "1 shell", "{what}");
                    assert_eq!(below.trim(), HINT, "{what}: the wrapped hint stands");
                }
                (_, true) => {
                    assert_eq!(glass.trim_end(), "1 shell", "CONTROL: {what}");
                    assert_eq!(below.trim(), "", "CONTROL: {what}");
                }
            }
        }
    }
}

/// THE PET HOLDS THE REWRITE'S WORDS AND STANDS ON THE BLANK AFTER THEM: the resident pet reads
/// Claude Code's cells, before the glass lays its rewrite over them, so the footer hands it
/// what the glass shows there. The words the rewrite lays (`1 shell`) are the host's chrome;
/// the rest of the mode row it blanks, out to the end of Claude's own words, and the row the
/// effort hint wrapped to are blank glass ([`crate::pet_chrome::PetChrome::held_blanked`]), never
/// chrome: Claude's letters under them are hidden, and the cat may rest there by the words.
/// With facts naming no effort the rewrite keeps Claude's hint's level: on the mode row it is
/// among the words the pet holds (`1 shell · ◐ medium`), and wrapped to the row under it is
/// Claude's own row, neither held nor blanked.
/// NEGATIVE CONTROL (as run with every write held as painted): the mode row was held from the
/// words out to Claude's last letter (columns 7 to 35) and the effort row whole, and nothing
/// was blanked; the pet read Claude's hidden letters as ink and the blank as chrome, and rested
/// out past them (aterm-effects' `the_cat_rests_by_the_words_the_footer_shows`).
#[test]
fn the_pet_holds_the_rewrites_words_and_stands_on_the_blank_after_them() {
    const HINT: &str = "\u{25D0} medium \u{B7} /effort";
    let jobs = "  \u{23F5}\u{23F5} bypass permissions on \u{B7} 1 shell";
    let hint_end = COLS - 2;
    for (wrapped, effort) in [(true, Some("xhigh")), (true, None), (false, None)] {
        let what = format!("wrapped={wrapped} effort={effort:?}");
        let (mut app, wid, session, _reader, _host) = app();
        let rule = "\u{2500}".repeat(COLS);
        let (row, under) = if wrapped {
            (jobs.to_owned(), format!("{HINT:>hint_end$}"))
        } else {
            (format!("{jobs}  {HINT}"), String::new())
        };
        feed(
            &app,
            session,
            format!(
                "\x1b[?2004h\x1b[2J\x1b[18;1H\u{25CF} done\x1b[20;1H{rule}\x1b[21;1H\u{276F} \
                 \x1b[22;1H{rule}\x1b[23;1H{row}\x1b[24;1H{under}\x1b[21;3H"
            )
            .as_bytes(),
        );
        claude_at(&app, session, effort);
        frame(&mut app, wid);
        // The pass's chrome is the pet's once the menu's splice commits it.
        app.splice_autopilot_menu(wid);
        let (mode_at, under_at) = (strip(&app, wid) + 22, strip(&app, wid) + 23);
        let shown = match (wrapped, effort) {
            (false, None) => "1 shell \u{B7} \u{25D0} medium".to_owned(),
            _ => "1 shell".to_owned(),
        };
        assert_eq!(
            glass_of(&app, wid, mode_at).trim_end(),
            shown,
            "fixture: {what}"
        );
        let below = match (wrapped, effort) {
            (true, None) => HINT,
            _ => "",
        };
        assert_eq!(
            glass_of(&app, wid, under_at).trim(),
            below,
            "fixture: {what}: the hint blanked, or Claude's own"
        );
        let (words, end) = (shown.chars().count(), row.chars().count());
        let chrome = &app.windows[&wid].pet_chrome;
        let held = |row: usize, col: usize| {
            let (r, c) = (row as f32 + 0.5, col as f32 + 0.5);
            chrome.held().iter().any(|rect| {
                rect.row <= r
                    && r < rect.row + rect.rows
                    && rect.col <= c
                    && c < rect.col + rect.cols
            })
        };
        let wrong: Vec<(usize, usize)> = (0..COLS)
            .map(|c| (22, c))
            .chain((0..COLS).map(|c| (23, c)))
            .filter(|&(r, c)| held(r, c) != (r == 22 && c < words))
            .collect();
        assert!(
            wrong.is_empty(),
            "{what}: the rewrite's words are held, and nothing else on its rows: {wrong:?} \
             (held {:?})",
            chrome.held()
        );
        let mut blanked = vec![aterm_effects::pet_world::PetRect::new(
            22.0,
            words as f32,
            1.0,
            (end - words) as f32,
        )];
        if wrapped && effort.is_some() {
            blanked.push(aterm_effects::pet_world::PetRect::new(
                23.0,
                0.0,
                1.0,
                hint_end as f32,
            ));
        }
        assert_eq!(
            chrome.held_blanked(),
            blanked,
            "{what}: the blank after the words, out to Claude's last letter, and a blanked hint's row"
        );
    }
}

#[test]
fn the_rewrite_allocates_nothing_once_its_buffers_are_grown() {
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
    let run = |out: &mut Vec<RowWrite>, cells: &mut Vec<RenderCell>, glass: &mut GlassColumns| {
        out.clear();
        cells.clear();
        glass.clear();
        let mode = footer::mode_row_rewrite_under(&rows, 22).expect("the row");
        collect_mode_row(&scratch, &rows, region, mode, |_| false, blank, out, cells);
        for e in out.iter() {
            glass.push(e.shift);
        }
        glass.grid_col(23, 0)
    };
    let col = run(&mut out, &mut cells, &mut glass);
    assert_eq!(out.len(), 1, "the rewrite is collected");
    let shells = u16::try_from(col_of(BYPASS, "14 shells")).unwrap();
    assert_eq!(
        col, shells,
        "the glass's column 0 is the grid's `14 shells`"
    );
    let (col, allocations) =
        crate::work_counts::allocations_in(|| run(&mut out, &mut cells, &mut glass));
    assert_eq!((col, allocations), (shells, 0));
    let _ = session;
}

#[test]
fn a_window_rewrites_no_more_rows_than_it_keeps_shifts_for() {
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
    let mode = footer::mode_row_rewrite_under(&rows, 22).expect("the row");
    let mut out = Vec::new();
    let mut cells = Vec::new();
    let taken: Vec<bool> = (0..=GLASS_SHIFTS)
        .map(|_| {
            collect_mode_row(
                &scratch,
                &rows,
                region,
                mode,
                |_| false,
                scratch.implicit_blank,
                &mut out,
                &mut cells,
            )
        })
        .collect();
    let mut glass = GlassColumns::default();
    for e in &out {
        glass.push(e.shift);
    }
    assert!(
        taken[..GLASS_SHIFTS].iter().all(|&t| t),
        "CONTROL: each is rewritten"
    );
    assert!(!taken[GLASS_SHIFTS], "the pane past the cap stays Claude's");
    assert_eq!(glass.len, out.len(), "every rewrite keeps its map");
    let _ = session;
}

#[test]
fn the_mode_row_is_read_with_the_rule_not_every_frame() {
    let (mut app, wid, session, _reader, _host) = app();
    draw(&app, session, BYPASS);
    claude(&app, session);
    frame(&mut app, wid);
    frame(&mut app, wid);
    let _ = crate::work_counts::take_mode_row_reads();
    for _ in 0..5 {
        frame(&mut app, wid);
    }
    feed(&app, session, b"\x1b[22;3Hx");
    frame(&mut app, wid);
    assert_eq!(
        crate::work_counts::take_mode_row_reads(),
        0,
        "at rest, and under a keystroke"
    );
    assert!(glass_of(&app, wid, mode_row(&app, wid)).starts_with(JOBS));
    draw(&app, session, MANUAL);
    frame(&mut app, wid);
    assert_eq!(
        crate::work_counts::take_mode_row_reads(),
        1,
        "CONTROL: the mode row changed"
    );
    assert!(glass_of(&app, wid, mode_row(&app, wid)).starts_with(NOTICE));
}

#[test]
fn a_hosted_off_pane_wears_the_border_and_rewrites_with_it() {
    for (what, enabled, rewritten) in [("hosted off", true, true), ("master off", false, false)] {
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
            rewritten,
            "{what}: the border"
        );
        let glass = glass_of(&app, wid, mode_row(&app, wid));
        assert_eq!(glass.starts_with(JOBS), rewritten, "{what}: {glass:?}");
        if !rewritten {
            assert_eq!(glass.trim_end(), grid_text(&app, session, 23).trim_end());
        }
        if let Some(host) = app.harness.take() {
            host.shutdown_and_join();
        }
    }
}
