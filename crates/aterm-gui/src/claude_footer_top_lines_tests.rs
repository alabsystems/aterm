// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hosted composer controls and the native Codex composer boundary.

use super::probe::*;
use super::*;
use crate::autopilot::{HarnessSwitch, SwitchOff};
use aterm_effects::harness_card::CardPalette;

const PGID: i32 = 4343;

const LIT: SwitchPaint =
    SwitchPaint::bare(HarnessSwitch::On, Some(aterm_phase::CliHarness::ClaudeCode));
const OFF: SwitchPaint = SwitchPaint::bare(
    HarnessSwitch::Off(SwitchOff::Master),
    Some(aterm_phase::CliHarness::ClaudeCode),
);

const SLAB: [u8; 3] = [45, 47, 51];

fn set_program(app: &App, session: u64, program: &str) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some(program.into()));
}

fn held(app: &mut App, session: u64) -> Hosted {
    let host = hosted(app);
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .expect("the claim");
    host
}

fn fixture_rows(fixture: &str) -> Vec<String> {
    fixture
        .lines()
        .filter(|l| !l.starts_with("# "))
        .map(str::to_owned)
        .collect()
}

fn glass_chars(app: &App, wid: WindowId, row: usize) -> String {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    ws.input_scratch.cells[strip + row]
        .iter()
        .map(|c| c.ch)
        .collect()
}

fn pane_cells(app: &App, wid: WindowId) -> Vec<Vec<RenderCell>> {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    ws.input_scratch.cells[strip..].to_vec()
}

fn marks_on(app: &App, wid: WindowId, session: u64) -> usize {
    app.windows[&wid]
        .input_scratch
        .fx_layers
        .iter()
        .filter(|l| l.id == top_mark_id(session))
        .count()
}

fn pane_of(rows: u16, cols: u16, program: &str) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    let _ = app.apply_term_resize(wid, rows, cols);
    assert_eq!(
        (app.windows[&wid].rows, app.windows[&wid].cols),
        (rows, cols)
    );
    set_program(&app, session, program);
    (app, wid, session)
}

fn codex_screen(cols: usize, slab: Option<[u8; 3]>) -> String {
    let cut = |s: &str| s.chars().take(cols).collect::<String>();
    let on = slab.map_or(String::new(), |[r, g, b]| format!("\x1b[48;2;{r};{g};{b}m"));
    let card = [
        format!("\u{256D}{}\u{256E}", "\u{2500}".repeat(39)),
        format!(
            "\u{2502} >_ OpenAI Codex (v0.154.0){}\u{2502}",
            " ".repeat(12)
        ),
        format!("\u{2502}{}\u{2502}", " ".repeat(39)),
        format!("\u{2570}{}\u{256F}", "\u{2500}".repeat(39)),
    ];
    let mut out = String::from("\x1b[2J");
    for (i, row) in card.iter().enumerate() {
        out.push_str(&format!("\x1b[{};1H{}", i + 1, cut(row)));
    }
    out.push_str(&format!("\x1b[6;1H{on}\x1b[K\x1b[0m"));
    out.push_str(&format!(
        "\x1b[7;1H{on}\x1b[K{}\x1b[0m",
        cut("\u{203A} \x1b[2mAsk Codex to do anything\x1b[22m")
    ));
    out.push_str(&format!("\x1b[8;1H{on}\x1b[K\x1b[0m"));
    out.push_str(&format!("\x1b[9;1H{}", cut("  ? for shortcuts")));
    out.push_str("\x1b[7;3H");
    out
}

struct CopyGuard(String);

impl Drop for CopyGuard {
    fn drop(&mut self) {
        crate::self_claim::set_copy_for_test(&self.0, None);
    }
}

fn copy_for(app: &App, session: u64) -> CopyGuard {
    use aterm_agent::harness::self_claimed::{Copy, Seen};
    let sid = app
        .pool
        .get(session)
        .expect("session")
        .ctx
        .self_id
        .as_str()
        .to_owned();
    crate::self_claim::set_copy_for_test(
        &sid,
        Some(Copy {
            pid: PGID as u32,
            start: "Mon Oct 20 16:37:58 2026".to_string(),
            home: std::path::PathBuf::from("/nonexistent/codex-home"),
            seen: Seen::Never,
            resume: Err("no-record-yet"),
        }),
    );
    CopyGuard(sid)
}

fn set_proven(app: &App, session: u64, proven: bool) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let mut meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
    meta.supervisor = proven.then(|| crate::session_timeline::SupervisorClaim {
        holder: format!("codex-alab@{PGID}"),
        conn: Some(1),
        expires_us: None,
        proven: Some(crate::self_claim::Proven {
            pid: PGID as u32,
            start: "Mon Oct 20 16:37:58 2026".to_string(),
        }),
        scoped: None,
    });
}

fn switch_of(app: &App, wid: WindowId, session: u64) -> Option<crate::claude_lights::BorderSwitch> {
    app.windows[&wid]
        .claude_lights
        .border_toggles
        .iter()
        .find(|b| b.session == session)
        .cloned()
}

fn plain_run(len: usize) -> TopRun {
    TopRun {
        col: 2,
        len,
        rule: RenderCell {
            ch: '\u{2500}',
            fg: [90, 90, 90],
            ..RenderCell::default()
        },
        voice: None,
        tail: None,
        end: 2 + len,
    }
}

fn lockup_key(
    (program, build): (aterm_phase::Program, Build),
    run: TopRun,
    switch: Option<SwitchPaint>,
    palette: Option<CardPalette>,
    hover: bool,
) -> TopKey {
    TopKey {
        frame_row: 30,
        term_row: 20,
        run,
        program,
        build,
        switch,
        palette,
        blank: RenderCell {
            ch: ' ',
            fg: [220, 220, 220],
            ..RenderCell::default()
        },
        field: PaneField {
            col_off: 0,
            cols: 240,
        },
        face: KeyFace::default(),
        cell: (7, 14),
        clip: aterm_core::render::PxRect::EVERYWHERE,
        mark_hover: hover,
        title_gen: u64::from(hover),
    }
}

fn cell_at(top: &ComposedTop, run: TopRun, c: usize) -> RenderCell {
    c.checked_sub(top.col)
        .and_then(|k| top.cells.get(k))
        .copied()
        .unwrap_or(run.rule)
}

fn keeps_the_layout(rest: &ComposedTop, hovered: &ComposedTop, run: TopRun) -> Result<(), String> {
    if rest.switch != hovered.switch {
        return Err("the switch moved".into());
    }
    for c in run.col..run.col + run.len {
        if !hovered.lockup.contains(&c) && cell_at(rest, run, c) != cell_at(hovered, run, c) {
            return Err(format!("column {c} changed outside the lockup"));
        }
    }
    let limit = hovered
        .switch
        .as_ref()
        .map_or(run.col + run.len, |s| s.cols.start);
    if hovered.lockup != rest.lockup {
        let lead = hovered.lockup.end..limit;
        if lead.len() < TITLE_LEAD
            || lead
                .clone()
                .any(|c| cell_at(hovered, run, c).ch != '\u{2500}')
        {
            return Err(format!(
                "{} `\u{2500}` before the switch",
                lead.filter(|&c| cell_at(hovered, run, c).ch == '\u{2500}')
                    .count()
            ));
        }
    }
    Ok(())
}

fn title_of(top: &ComposedTop, run: TopRun) -> String {
    let at = top.lockup.start + aterm_effects::cli_marks::ICON_RUN;
    (at..top.lockup.end.saturating_sub(1))
        .map(|c| cell_at(top, run, c).ch)
        .collect()
}

#[test]
fn the_hover_title_never_moves_the_switch() {
    use aterm_phase::Program;
    let titles: [((Program, Build), &str, [&str; 3]); 2] = [
        (
            (Program::Agy, Build::Vendor),
            "1.4.0",
            ["agy", "Antigravity CLI", "Antigravity CLI 1.4.0"],
        ),
        (
            (Program::Claude, Build::Vendor),
            "2.1.305",
            ["claude", "Claude Code", "Claude Code 2.1.305"],
        ),
    ];
    let mut caught = 0;
    let mut cut = 0;
    for (lockup, version, ranks) in titles {
        let (program, build) = lockup;
        let mark = cli_mark(program).expect("a mark");
        let rank = |title: &str| {
            ranks
                .iter()
                .position(|r| !r.is_empty() && *r == title)
                .unwrap_or_else(|| panic!("{lockup:?}: not a title: {title:?}"))
        };
        let mut seen = [0usize; 3];
        for (switch, palette) in [
            (Some(LIT), Some(CardPalette::Cruise)),
            (Some(LIT), Some(CardPalette::Off)),
            (Some(OFF), None),
            (Some(LIT), None),
            (None, None),
        ] {
            let (mut last, mut rung) = (2, None);
            for len in (8..=80).rev() {
                let run = plain_run(len);
                let at = format!("{lockup:?} {len}, {switch:?} {palette:?}");
                let (mut rest, mut hovered) = (ComposedTop::default(), ComposedTop::default());
                compose_top(
                    &lockup_key(lockup, run, switch, palette, false),
                    &mut rest,
                    Some(version),
                    None,
                    &[],
                );
                compose_top(
                    &lockup_key(lockup, run, switch, palette, true),
                    &mut hovered,
                    Some(version),
                    None,
                    &[],
                );
                keeps_the_layout(&rest, &hovered, run).unwrap_or_else(|e| panic!("{at}: {e}"));
                let full = rest.lockup.len() == mark.lockup_cells(build);
                if !full {
                    assert_eq!(rest.cells, hovered.cells, "{at}: no name, no title");
                    continue;
                }
                assert_eq!(
                    title_of(&rest, run),
                    mark.name(build),
                    "{at}: at rest, the name"
                );
                // A rung is whether the switch stands and the label's tier: a narrower label
                // leaves the title more room.
                let tier = rest
                    .switch
                    .as_ref()
                    .map(|s| s.label.as_ref().map(|(_, t)| *t));
                if rung != Some(tier) {
                    (last, rung) = (2, Some(tier));
                }
                let title = title_of(&hovered, run);
                let r = rank(&title);
                assert!(r <= last, "{at}: {title:?} after a narrower title");
                last = r;
                seen[r] += 1;
                let mut loose = ComposedTop::default();
                compose_top_in(
                    &lockup_key(lockup, run, switch, palette, true),
                    &mut loose,
                    Some(version),
                    None,
                    &[],
                    0,
                );
                if keeps_the_layout(&rest, &loose, run).is_err() {
                    caught += 1;
                }
                {
                    let name = mark.name(build);
                    let short: String = name
                        .chars()
                        .take(name.chars().count().saturating_sub(1))
                        .collect();
                    assert!(short.len() < mark.name(build).len(), "{at}");
                    let mut narrow = ComposedTop {
                        cells: rest.cells.clone(),
                        col: rest.col,
                        switch: rest.switch.clone(),
                        lockup: rest.lockup.clone(),
                        ..ComposedTop::default()
                    };
                    let name_at = narrow.lockup.start + aterm_effects::cli_marks::ICON_RUN;
                    let pad = narrow.cells[narrow.lockup.start - narrow.col];
                    for c in name_at..narrow.lockup.end {
                        narrow.cells[c - narrow.col] = pad;
                    }
                    for (k, ch) in short.chars().enumerate() {
                        narrow.cells[name_at + k - narrow.col].ch = ch;
                    }
                    narrow.lockup.end = name_at + short.len() + 1;
                    assert!(
                        keeps_the_layout(&rest, &narrow, run).is_err(),
                        "CONTROL {at}: a title narrower than the name is caught"
                    );
                    cut += 1;
                }
            }
        }
        let want = ranks.iter().filter(|r| !r.is_empty()).count();
        assert_eq!(
            seen.iter().filter(|&&n| n > 0).count(),
            want,
            "{lockup:?}: every title is seen: {seen:?}"
        );
    }
    assert!(
        caught > 0,
        "CONTROL: a title in the switch's lead is caught"
    );
    assert!(cut > 0, "CONTROL: a narrower title is caught");
    hover_title_on_the_glass();
}

fn cell_px(app: &App, wid: WindowId, (row, col): (usize, usize)) -> (f64, f64) {
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

fn hover_title_on_the_glass() {
    let (mut app, wid, session) = probe_glass();
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.set_agent_reader_for_test(true);
    }
    let _host = held(&mut app, session);
    for _ in 0..2 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    let top = AT_REST.bottom() - 2;
    let resting = glass_chars(&app, wid, top);
    let switch = switch_of(&app, wid, session).expect("the switch");
    let lockup = app.windows[&wid]
        .claude_lights
        .mark_hits
        .iter()
        .find(|m| m.session == session)
        .expect("the lockup is a target")
        .clone();
    assert!(resting.contains("    claude \u{2500}"), "{resting:?}");
    let fp_rest = pass(&mut app, wid);
    let (x, y) = cell_px(&app, wid, (lockup.frame_row, lockup.cols.start + 5));
    assert_eq!(
        app.track_claude_light_hover(wid, x, y),
        crate::claude_lights::OverChips::No,
        "the lockup is decoration: the motion is the pane's"
    );
    assert!(app.windows[&wid].claude_lights.mark_hovered(session));
    let fp_hover = pass(&mut app, wid);
    assert_ne!(fp_rest, fp_hover, "the hover is in the repaint key");
    let hovered = glass_chars(&app, wid, top);
    assert!(
        hovered.contains("    Claude Code \u{2500}\u{2500}") && hovered.contains("(v2.1.283)"),
        "the title, its version on the label: {hovered:?}"
    );
    assert_eq!(
        switch_of(&app, wid, session).map(|b| b.cols),
        Some(switch.cols.clone()),
        "the switch stood"
    );
    app.windows.get_mut(&wid).expect("window").last_cursor_px = (x, y);
    assert!(
        !app.press_claude_light(wid, std::time::Instant::now()),
        "a press on the lockup is the pane's"
    );
    for i in 0..3 {
        let _ = pass(&mut app, wid);
        assert!(
            app.windows[&wid].claude_lights.mark_hovered(session),
            "CONTROL {i}: the hover stands"
        );
        assert_eq!(glass_chars(&app, wid, top), hovered, "CONTROL {i}");
    }
    let grown = Live {
        draft: &["fix the", "tests"],
        ..AT_REST
    };
    feed(&app, session, &screen(&grown));
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    let first = glass_chars(&app, wid, top - 1);
    assert!(
        first.contains("    claude \u{2500}") && !first.contains("Claude Code"),
        "the frame that moves the line draws no title for the pointer it left: {first:?}"
    );
    assert!(
        !app.windows[&wid].claude_lights.mark_hovered(session),
        "the lockup moved from under the pointer: the hover is let go"
    );
    let _ = pass(&mut app, wid);
    let moved = glass_chars(&app, wid, top - 1);
    assert!(
        moved.contains("    claude \u{2500}") && !moved.contains("Claude Code"),
        "the moved line wears the name, at rest: {moved:?}"
    );
    assert!(
        !glass_chars(&app, wid, top).contains("Claude Code"),
        "and no title is left on the row under the pointer"
    );
    feed(&app, session, &screen(&AT_REST));
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    let (x, y) = cell_px(&app, wid, (lockup.frame_row - 3, 4));
    let _ = app.track_claude_light_hover(wid, x, y);
    assert!(!app.windows[&wid].claude_lights.mark_hovered(session));
    let _ = pass(&mut app, wid);
    assert_eq!(glass_chars(&app, wid, top), resting, "the name is back");
}

fn screen_at(live: &Live<'_>, rows: u16, cols: u16) -> Vec<String> {
    let mut s: Vec<String> = screen(live)
        .into_iter()
        .map(|row| {
            if row.starts_with('\u{2500}') {
                "\u{2500}".repeat(usize::from(cols))
            } else {
                row
            }
        })
        .collect();
    let more = usize::from(rows).saturating_sub(s.len());
    s.splice(0..0, std::iter::repeat_n(String::new(), more));
    s
}

fn claude_held() -> (App, WindowId, u64, Hosted) {
    let (mut app, wid, session) = probe_glass();
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.set_agent_reader_for_test(true);
    }
    let host = held(&mut app, session);
    (app, wid, session, host)
}

fn regrid(app: &mut App, wid: WindowId, session: u64, rows: u16, cols: u16) {
    assert!(
        app.apply_term_resize(wid, rows, cols),
        "the window re-grids"
    );
    feed(app, session, &screen_at(&AT_REST, rows, cols));
}

fn next_frame(app: &mut App, wid: WindowId) {
    refill(app, wid);
    let _ = pass(app, wid);
}

fn point_at(app: &mut App, wid: WindowId, cell: (usize, usize)) -> crate::claude_lights::OverChips {
    let (x, y) = cell_px(app, wid, cell);
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.last_cursor_px = (x, y);
    ws.pointer_position_known = true;
    app.track_claude_light_hover(wid, x, y)
}

fn switch_hovered(app: &App, wid: WindowId) -> Option<u64> {
    app.windows[&wid].claude_lights.switch_hover
}

#[test]
fn a_regrid_hit_tests_the_glass_it_lands_on() {
    use crate::claude_lights::OverChips;
    let (mut app, wid, session, _host) = claude_held();
    for _ in 0..2 {
        next_frame(&mut app, wid);
    }
    let laid = switch_of(&app, wid, session).expect("the switch");
    let on = (laid.frame_row, laid.cols.start + laid.cols.len() / 2);
    assert_eq!(
        point_at(&mut app, wid, on),
        OverChips::Chip,
        "on the switch"
    );
    assert_eq!(switch_hovered(&app, wid), Some(session));
    next_frame(&mut app, wid);
    let hovered = pane_cells(&app, wid);

    let wide = COLS + 24;
    regrid(&mut app, wid, session, ROWS, wide);
    let stamp = *app
        .windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .drawn_at_mut();
    assert!(stamp.is_some(), "the last frame stamped its lists");
    *app.windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .drawn_at_mut() = app.glass_grid(wid);
    assert_eq!(
        point_at(&mut app, wid, on),
        OverChips::Chip,
        "CONTROL: unstamped, the old lists answer"
    );
    assert_eq!(switch_hovered(&app, wid), Some(session), "CONTROL");
    *app.windows
        .get_mut(&wid)
        .expect("window")
        .claude_lights
        .drawn_at_mut() = stamp;
    assert_eq!(
        point_at(&mut app, wid, on),
        OverChips::No,
        "lists laid on a grid the window no longer has answer nothing"
    );
    assert_eq!(switch_hovered(&app, wid), None, "nothing is hovered");
    assert!(
        !app.press_claude_light(wid, std::time::Instant::now()),
        "a press there is the pane's: no switch the glass shows is there"
    );
    next_frame(&mut app, wid);
    let moved = switch_of(&app, wid, session).expect("the switch, re-laid");
    assert!(
        moved.frame_row == laid.frame_row && !moved.cols.contains(&on.1),
        "the switch moved from under the pointer: {:?} -> {:?}",
        laid.cols,
        moved.cols
    );
    assert_eq!(
        switch_hovered(&app, wid),
        None,
        "the frame that laid it leaves the pointer on nothing"
    );

    let cursor = |app: &App| {
        let ws = &app.windows[&wid];
        (ws.hover_pointer, ws.native_text_cursor)
    };
    let (hand, beam) = ((true, false), (false, true));
    app.update_hover_cursor(wid);
    assert_eq!(cursor(&app), beam, "the pane's cursor");
    regrid(&mut app, wid, session, ROWS, COLS);
    next_frame(&mut app, wid);
    assert_eq!(
        switch_of(&app, wid, session).map(|b| b.cols),
        Some(laid.cols.clone()),
        "the switch is back under the pointer"
    );
    assert_eq!(
        switch_hovered(&app, wid),
        Some(session),
        "the frame that laid it under the still pointer hovers it"
    );
    assert_eq!(
        cursor(&app),
        beam,
        "the cursor waits for that frame's present"
    );
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(cursor(&app), hand, "and then it is the switch's hand");
    next_frame(&mut app, wid);
    assert_eq!(
        pane_cells(&app, wid),
        hovered,
        "and the next frame draws it hovered"
    );
    app.windows.get_mut(&wid).expect("window").hover_pointer = false;
    app.refresh_glass_cursor_for_frame(wid);
    assert!(!cursor(&app).0, "CONTROL: a frame at rest reads one flag");
    app.update_hover_cursor(wid);

    regrid(&mut app, wid, session, ROWS, wide);
    next_frame(&mut app, wid);
    assert_eq!(switch_hovered(&app, wid), None);
    app.on_cursor_left(wid);
    regrid(&mut app, wid, session, ROWS, COLS);
    next_frame(&mut app, wid);
    assert_eq!(
        switch_of(&app, wid, session).map(|b| b.cols),
        Some(laid.cols.clone()),
        "CONTROL: the switch is back"
    );
    assert_eq!(
        switch_hovered(&app, wid),
        None,
        "CONTROL: no pointer, no hover"
    );
}

#[test]
fn a_regrid_brings_the_hover_title_under_a_still_pointer() {
    let (mut app, wid, session, _host) = claude_held();
    for _ in 0..2 {
        next_frame(&mut app, wid);
    }
    let lockup = app.windows[&wid]
        .claude_lights
        .mark_hits
        .iter()
        .find(|m| m.session == session)
        .expect("the lockup is a target")
        .clone();
    let top = AT_REST.bottom() - 2;
    let resting = glass_chars(&app, wid, top);
    assert!(resting.contains("    claude \u{2500}"), "{resting:?}");
    let below = (lockup.frame_row + 2, lockup.cols.start + 5);
    let _ = point_at(&mut app, wid, below);
    assert!(!app.windows[&wid].claude_lights.mark_hovered(session));
    app.on_cursor_left(wid);
    regrid(&mut app, wid, session, ROWS + 2, COLS);
    next_frame(&mut app, wid);
    let grown = app.windows[&wid]
        .claude_lights
        .mark_hits
        .iter()
        .find(|m| m.session == session)
        .expect("the lockup, re-laid")
        .clone();
    assert_eq!(
        (grown.frame_row, grown.cols.clone()),
        (below.0, lockup.cols.clone()),
        "the lockup moved down two rows"
    );
    assert!(
        !app.windows[&wid].claude_lights.mark_hovered(session),
        "CONTROL: no pointer, no title"
    );
    next_frame(&mut app, wid);
    assert!(
        glass_chars(&app, wid, top + 2).contains("    claude \u{2500}"),
        "CONTROL: the name"
    );

    regrid(&mut app, wid, session, ROWS, COLS);
    next_frame(&mut app, wid);
    let _ = point_at(&mut app, wid, below);
    assert!(!app.windows[&wid].claude_lights.mark_hovered(session));
    regrid(&mut app, wid, session, ROWS + 2, COLS);
    next_frame(&mut app, wid);
    assert!(
        app.windows[&wid].claude_lights.mark_hovered(session),
        "the frame that laid the lockup under the still pointer hovers it"
    );
    next_frame(&mut app, wid);
    let titled = glass_chars(&app, wid, top + 2);
    assert!(
        titled.contains("    Claude Code \u{2500}\u{2500}"),
        "the title, its version on the label: {titled:?}"
    );
}

/// The hosted agy switch keeps one click target.
#[cfg(a11y_tree)]
#[test]
fn agys_lit_switch_is_one_switch_and_its_press_turns_it_off() {
    use crate::a11y_tree::HarnessChip;
    use crate::autopilot::AutopilotView;
    use aterm_a11y::model::Toggled;
    let rows = fixture_rows(include_str!(
        "../../aterm-phase/src/fixtures/agy-1.4.0-idle.txt"
    ));
    let (mut app, wid, session) = pane_of(40, 120, "agy");
    let _host = held(&mut app, session);
    feed(&app, session, &rows);
    for _ in 0..2 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    let nodes = |app: &App| -> Vec<crate::a11y_tree::GridChip> {
        app.claude_chips_a11y(wid)
            .into_iter()
            .filter(|c| c.chip == HarnessChip::Switch)
            .collect()
    };
    let b = switch_of(&app, wid, session).expect("agy's switch");
    assert_eq!(b.face, HarnessSwitch::On, "CONTROL: agy carried, lit");
    let drawn: String = app.windows[&wid].input_scratch.cells[b.frame_row][b.cols.clone()]
        .iter()
        .map(|c| c.ch)
        .collect();
    assert_eq!(drawn, " \u{25cf} on ");
    let lit = nodes(&app);
    assert_eq!(lit.len(), 1, "{lit:?}");
    assert_eq!(
        (lit[0].chip, lit[0].toggled, lit[0].col, lit[0].width),
        (
            HarnessChip::Switch,
            Some(Toggled::True),
            b.cols.start,
            b.cols.len()
        )
    );
    let label = harness_label(aterm_phase::CliHarness::Agy);
    assert_eq!(lit[0].label, format!("{label} on: harness"));
    let carried = app.session_autopilot(session).1;
    assert!(
        matches!(carried, AutopilotView::Cruise | AutopilotView::Land),
        "CONTROL: {carried:?}"
    );
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Switch);
    assert_eq!(
        app.session_autopilot(session).1,
        carried,
        "the reader's press writes no Autopilot word"
    );
    let wrote = app
        .messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some("harness.switch-save.harness.enabled"))
        .map(|r| r.detail.join(" "))
        .expect("the reader's press is the switch's click: the master switch's write");
    assert!(wrote.contains("harness.enabled = false"), "{wrote:?}");
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    for _ in 0..2 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    let off = nodes(&app);
    assert_eq!(
        off.iter()
            .map(|n| (n.chip, n.toggled, n.label.as_str()))
            .collect::<Vec<_>>(),
        [(
            HarnessChip::Switch,
            Some(Toggled::False),
            format!("{label} off: harness").as_str()
        )],
        "NEGATIVE CONTROL: the off face is one switch, toggled off"
    );
}

/// AGY'S LABEL WEARS THE MOONBOW UNDER AUTOPILOT OFF: on the moonbow's line (`CardPalette::Off`)
/// the label's `alab` is drawn bold in the moonbow's hero (the Off card's settled hero letters),
/// and the switch's words keep the pane's inks. CONTROL: under Cruise `alab` is another ink.
#[test]
fn agys_label_wears_the_moonbow_under_autopilot_off() {
    let rows = fixture_rows(include_str!(
        "../../aterm-phase/src/fixtures/agy-1.4.0-idle.txt"
    ));
    let (mut app, wid, session) = pane_of(40, 120, "agy");
    let _host = held(&mut app, session);
    feed(&app, session, &rows);
    let settle = |app: &mut App| {
        for _ in 0..2 {
            refill(app, wid);
            let _ = pass(app, wid);
        }
    };
    settle(&mut app);
    let read = |app: &App| -> (Vec<RenderCell>, Vec<RenderCell>) {
        let b = switch_of(app, wid, session).expect("agy's switch");
        assert_eq!(b.face, HarnessSwitch::On, "lit");
        let row = &app.windows[&wid].input_scratch.cells[b.frame_row];
        let label = b.label.clone().expect("the label");
        (
            row[label.start..label.start + 4].to_vec(),
            row[b.cols.clone()].to_vec(),
        )
    };
    let (cruise, cruise_switch) = read(&app);
    assert!(app.set_session_autopilot(
        session,
        Some(aterm_agent::supervise::Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    settle(&mut app);
    let (white, switch) = read(&app);
    assert_eq!(white.iter().map(|c| c.ch).collect::<String>(), "alab");
    let ground = app.windows[&wid].input_scratch.implicit_blank.bg;
    let ground = u32::from_be_bytes([0, ground[0], ground[1], ground[2]]);
    for (k, c) in white.iter().enumerate() {
        let hero = aterm_effects::harness_card::CardPalette::Off.hero_on(k as f32 / 3.0, ground);
        assert!(
            c.bold && c.fg == hero.to_be_bytes()[1..],
            "{:?} is bold in the moonbow's hero {hero:06x}: {:?}",
            c.ch,
            c.fg
        );
    }
    assert_eq!(
        switch, cruise_switch,
        "the switch's words keep the pane's inks"
    );
    assert_ne!(
        cruise[0].fg, white[0].fg,
        "CONTROL: under Cruise `alab` is Cruise's hero"
    );
}

/// ANOTHER CLI'S ACCOUNT CHIP NEVER NAMES A CLAUDE CODE ROW: a session whose footer names the
/// account Claude Code's roster judges (`claude-1`) reads, once its reader is Codex's, as `Codex
/// CLI` alone, since nothing judged its login in Codex's own pool. CONTROL: while it is Claude
/// Code's, its chip keeps the footer's own words, which name that account.
#[test]
fn another_clis_account_chip_never_names_the_row_claude_codes_roster_judges() {
    use crate::a11y_tree::HarnessChip;
    let (app, wid, session) = glass_with(
        &AT_REST,
        FooterFacts {
            account: Some("claude-1".into()),
            ..facts()
        },
    );
    let account = |app: &App| {
        app.claude_chips_a11y(wid)
            .into_iter()
            .find(|c| c.chip == HarnessChip::Account)
            .map(|c| c.label)
            .expect("an Account chip")
    };
    assert!(
        account(&app).contains("claude-1"),
        "CONTROL: {}",
        account(&app)
    );
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_agent_reader_program_for_test(Some(aterm_phase::Program::Codex));
    assert_eq!(account(&app), "Codex CLI");
}

/// EVERY CLI HARNESS SHARES THE SWITCH AND ITS LABEL: Claude Code, Antigravity, Muse Code and
/// all paint ` ● on ` when lit and ` ○ off ` when off, ending in the same cell, each
/// followed by its own label (`alab v0.5 (v<cli build>)`), every cell on the line's ground, the
/// switch never bold; when off no cell wears the theme's active green accent.
#[test]
fn every_cli_harness_shares_the_switch_and_never_glows_green_when_off() {
    let agy_rows = fixture_rows(include_str!(
        "../../aterm-phase/src/fixtures/agy-1.4.0-idle.txt"
    ));
    let rule = "\u{2500}".repeat(120);
    let muse_rows: Vec<String> = vec![
        String::new(),
        "  Muse Code 0.9.3".into(),
        String::new(),
        rule.clone(),
        "\u{276F} ".into(),
        rule,
        "  gpt-5.4 \u{00B7} high \u{00B7} ~/aterm \u{00B7} Auto-review".into(),
    ];
    let default_theme = App::headless_for_test().theme;
    let green_accent = crate::chrome_band::band_colors(default_theme).accent;
    assert!(
        green_accent[1] > 200 && green_accent[0] < 100,
        "CONTROL: default theme's accent is bright green: {green_accent:?}"
    );

    for prog in ["claude", "agy", "muse"] {
        let rows_count = if matches!(prog, "claude" | "agy") {
            ROWS
        } else {
            24
        };
        let (mut app, wid, session) = pane_of(rows_count, 120, prog);
        let _host = held(&mut app, session);
        match prog {
            "claude" => {
                let ctx = app.pool.get(session).expect("session").ctx.clone();
                let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                timeline.set_agent_reader_for_test(true);
                assert!(timeline.set_claude_footer(PGID, None, Some(facts())));
                drop(timeline);
                feed(&app, session, &screen(&AT_REST));
            }
            "agy" => feed(&app, session, &agy_rows),
            "muse" => {
                drawer(&app, session, PGID, "muse", "0.9.3");
                feed(&app, session, &muse_rows);
            }
            _ => unreachable!(),
        }
        // The running build each lane gives the label: the session registry's (Claude Code's),
        // the store image's (Muse Code's).
        let build = match prog {
            "claude" => Some("2.1.283"),
            "muse" => Some("0.9.3"),
            _ => None,
        };
        let settle = |app: &mut App| {
            for _ in 0..2 {
                refill(app, wid);
                let _ = pass(app, wid);
            }
        };
        let harness = match prog {
            "claude" => aterm_phase::CliHarness::ClaudeCode,
            "agy" => aterm_phase::CliHarness::Agy,
            "muse" => aterm_phase::CliHarness::Muse,
            _ => unreachable!("only hosted CLI harnesses have a switch"),
        };
        let read = |app: &App, b: &crate::claude_lights::BorderSwitch, want: &str| {
            let row = &app.windows[&wid].input_scratch.cells[b.frame_row];
            let ground = row[b.cols.start - 1].bg;
            let face: String = row[b.cols.clone()].iter().map(|c| c.ch).collect();
            assert_eq!(face, want, "{prog}: the switch");
            let label = b
                .label
                .clone()
                .unwrap_or_else(|| panic!("{prog}: the label"));
            let words: String = row[label.clone()].iter().map(|c| c.ch).collect();
            assert!(
                words.starts_with(&harness_label(harness)),
                "{prog}: the label {words:?}"
            );
            if let Some(core) = build {
                assert!(
                    words.ends_with(&format!(" (v{core})")),
                    "{prog}: the running build {core} named: {words:?}"
                );
            }
            for c in &row[b.cols.start..label.end] {
                assert_eq!(c.bg, ground, "{prog}: {:?} on the line's ground", c.ch);
            }
            assert!(
                row[b.cols.clone()].iter().all(|c| !c.bold),
                "{prog}: the switch, never bold"
            );
            row[b.cols.start..label.end].to_vec()
        };

        settle(&mut app);
        let on = switch_of(&app, wid, session).unwrap_or_else(|| panic!("{prog}: lit switch"));
        assert_eq!(on.face, HarnessSwitch::On, "{prog}");
        let _ = read(&app, &on, LIT_SWITCH);

        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
        );
        settle(&mut app);
        let off = switch_of(&app, wid, session).unwrap_or_else(|| panic!("{prog}: off switch"));
        assert_eq!(off.face, HarnessSwitch::Off(SwitchOff::Master), "{prog}");
        let cells = read(&app, &off, OFF_SWITCH);
        assert_eq!(
            (on.cols.end, on.label.clone()),
            (off.cols.end, off.label.clone()),
            "{prog}: ON and OFF end in the same cell, the label unmoved"
        );
        for c in &cells {
            assert_ne!(
                c.fg, green_accent,
                "{prog}: when OFF, no cell wears active green: {c:?}"
            );
        }
    }
}

/// The ⬆️ ring layers on `session`'s glass ([`top_ring_id`]).
fn rings_on(app: &App, wid: WindowId, session: u64) -> Vec<aterm_core::render::FxLayer> {
    app.windows[&wid]
        .input_scratch
        .fx_layers
        .iter()
        .filter(|l| l.id == top_ring_id(session))
        .cloned()
        .collect()
}

/// A SETTLED SWITCH BUILDS NOTHING: the switch is cells alone, so at rest, under the pointer and
/// turned off it lays no sprite and keeps none, and at rest frame after frame nothing is sealed or
/// copied. CONTROL: the pointer on the switch lifts its word, so the pass did draw it.
#[test]
fn a_settled_switch_builds_nothing() {
    let (mut app, wid, session, _host) = claude_held();
    for _ in 0..2 {
        next_frame(&mut app, wid);
    }
    let laid = switch_of(&app, wid, session).expect("the switch");
    let kept = |app: &App| app.windows[&wid].claude_footer_texts.borrow().rings.0.len();
    assert_eq!(
        (rings_on(&app, wid, session), kept(&app)),
        (vec![], 0),
        "no sprite at rest"
    );
    for i in 0..6 {
        if i % 2 == 0 {
            refill(&mut app, wid);
        }
        let before = aterm_core::render::fx_counts_here();
        let _ = pass(&mut app, wid);
        assert_eq!(
            aterm_core::render::fx_counts_here(),
            before,
            "frame {i}: nothing sealed or copied"
        );
        assert!(rings_on(&app, wid, session).is_empty(), "frame {i}");
    }
    let on = (laid.frame_row, laid.cols.start + 2);
    let resting = app.windows[&wid].input_scratch.cells[on.0][on.1];
    let _ = point_at(&mut app, wid, on);
    for _ in 0..2 {
        next_frame(&mut app, wid);
    }
    let lifted = app.windows[&wid].input_scratch.cells[on.0][on.1];
    assert!(
        lifted.ch == resting.ch && lifted.fg != resting.fg && lifted.bg == resting.bg,
        "CONTROL: the hover lifts the word on its ground: {resting:?} -> {lifted:?}"
    );
    assert_eq!(
        (rings_on(&app, wid, session), kept(&app)),
        (vec![], 0),
        "no sprite under the pointer"
    );
    let _ = point_at(&mut app, wid, (laid.frame_row - 3, 0));
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    for _ in 0..2 {
        next_frame(&mut app, wid);
    }
    assert_eq!(
        switch_of(&app, wid, session).map(|b| b.face),
        Some(HarnessSwitch::Off(SwitchOff::Master)),
        "turned off"
    );
    assert_eq!(
        (rings_on(&app, wid, session), kept(&app)),
        (vec![], 0),
        "no sprite turned off"
    );
}

/// A retained move is still the move from the running image, not an order over every later
/// revision of its numeric core. Replacing Muse outside the host cannot re-offer its old target.
#[test]
fn a_retained_upgrade_is_bound_to_its_running_source_revision() {
    use aterm_agent::harness::upgrade::{Agent, Phase};
    use aterm_agent::harness::upgrade_drive::Row;

    let (app, _wid, session) = pane_of(24, 120, "muse");
    drawer(&app, session, PGID, "muse", "1.4.10-R6427.1");
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    {
        let mut timeline = ctx.timeline.lock().unwrap();
        assert!(timeline.set_upgrade(Some(Row {
            agent: Agent::Muse,
            from: "1.4.10-R6427.1".into(),
            to: "1.4.10-R6500.1".into(),
            phase: Phase::Pending,
            ..Row::default()
        })));
        assert!(timeline.set_upgrade(None));
        assert_eq!(TopCli::from_timeline(&timeline).kept.state, UpState::Held);
    }
    drawer(&app, session, PGID, "muse", "1.4.10-R6501.1");
    let timeline = ctx.timeline.lock().unwrap();
    let cli = TopCli::from_timeline(&timeline);
    assert_eq!(cli.ver.as_str(), "1.4.10-R6501.1");
    assert_eq!(cli.kept.state, UpState::None);
}

/// Clearing a published row after another same-version job took the foreground must keep the
/// row's original owner. CONTROL: clearing before the change already hides it from the other
/// job and admits the unchanged original when it returns.
#[test]
fn a_retained_upgrade_keeps_its_owner_when_the_foreground_changes_before_the_clear() {
    use aterm_agent::harness::upgrade::{Agent, Phase};
    use aterm_agent::harness::upgrade_drive::Row;

    for moved_before_clear in [false, true] {
        let (mut app, _wid, session) = pane_of(24, 120, "claude");
        let ctx = app.pool.get(session).unwrap().ctx.clone();
        let front = |app: &App, pgid| {
            drawer(app, session, pgid, "claude", "2.1.305");
            ctx.timeline
                .lock()
                .unwrap()
                .set_program(pgid, Some("claude".into()));
        };
        let arrow = || {
            let timeline = ctx.timeline.lock().unwrap();
            SwitchPaint::of(
                HarnessSwitch::Off(SwitchOff::Master),
                HoverAt::None,
                Some(aterm_phase::Program::Claude),
                TopCli::from_timeline(&timeline),
            )
            .up
        };
        front(&app, PGID);
        app.apply_agent_upgrades(vec![Row {
            tab: app.session_sid(session).unwrap(),
            session: "00000000-0000-4000-8000-000000000031".into(),
            agent: Agent::Claude,
            from: "2.1.305".into(),
            to: "2.1.310".into(),
            phase: Phase::Pending,
            ..Row::default()
        }]);
        if moved_before_clear {
            front(&app, PGID + 1);
        }
        app.apply_agent_upgrades(Vec::new());
        assert_eq!(
            arrow().state,
            if moved_before_clear {
                UpState::None
            } else {
                UpState::Ready
            },
            "foreground moved before clearing: {moved_before_clear}"
        );
        front(&app, PGID + 1);
        assert_eq!(arrow().state, UpState::None, "another same-version job");
        front(&app, PGID);
        assert_eq!(
            arrow().state,
            UpState::Ready,
            "the unchanged original returns: {moved_before_clear}"
        );
    }
}

fn upgrade_owner_front(app: &App, session: u64, pgid: i32, started: u64) -> u64 {
    let token = drawer_started(app, session, pgid, "claude", "2.1.305", started);
    app.pool
        .get(session)
        .unwrap()
        .ctx
        .timeline
        .lock()
        .unwrap()
        .set_program(pgid, Some("claude".into()));
    token
}

fn upgrade_owner_row(app: &App, session: u64) -> aterm_agent::harness::upgrade_drive::Row {
    use aterm_agent::harness::upgrade::{Agent, Phase};
    aterm_agent::harness::upgrade_drive::Row {
        tab: app.session_sid(session).unwrap(),
        session: "00000000-0000-4000-8000-000000000031".into(),
        agent: Agent::Claude,
        from: "2.1.305".into(),
        to: "2.1.310".into(),
        phase: Phase::Pending,
        ..Default::default()
    }
}

/// The arrow of `session`'s rule with the harness off. A Claude Code row kept there stands as it
/// is, `Ready`: a press moves its tab once ([`TopUp::harness_off`]).
fn upgrade_owner_arrow(app: &App, session: u64) -> TopUp {
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let timeline = ctx.timeline.lock().unwrap();
    SwitchPaint::of(
        HarnessSwitch::Off(SwitchOff::Master),
        HoverAt::None,
        Some(aterm_phase::Program::Claude),
        TopCli::from_timeline(&timeline),
    )
    .up
}

/// A read captured before a foreground change cannot publish its old row onto the new job,
/// including a PID reused by the same CLI and version. A completed read for the original can.
#[test]
fn a_queued_upgrade_uses_the_foreground_captured_before_its_read() {
    use crate::upgrade_host::UpgradeSources;

    for (pgid, started) in [(PGID + 1, 1), (PGID, 2)] {
        let (mut app, _wid, session) = pane_of(24, 120, "claude");
        let original = upgrade_owner_front(&app, session, PGID, 1);
        let row = upgrade_owner_row(&app, session);
        assert_eq!(row.pid, 0, "pending Claude records have no recorded PID");
        let before_read = UpgradeSources::capture(&app.store);
        let replacement = upgrade_owner_front(&app, session, pgid, started);
        assert_ne!(original, replacement, "real holder image identity");
        app.apply_host_upgrades_from(vec![row.clone()], vec![], before_read.clone());
        assert_eq!(upgrade_owner_arrow(&app, session).state, UpState::None);
        assert!(
            app.upgrade_view.row_for(&row.tab).is_none(),
            "the stale batch was not admitted"
        );

        upgrade_owner_front(&app, session, PGID, 1);
        app.apply_host_upgrades_from(vec![row.clone()], vec![], before_read);
        assert_eq!(
            upgrade_owner_arrow(&app, session).state,
            UpState::None,
            "departing and returning does not make an old in-flight read current"
        );
        let after_read = UpgradeSources::capture(&app.store);
        app.apply_host_upgrades_from(vec![row], vec![], after_read);
        assert_eq!(
            upgrade_owner_arrow(&app, session).state,
            UpState::Ready,
            "a new completed read admits the unchanged original even with pid=0"
        );
    }
}

/// Once admitted, a record's owner survives unchanged refreshes, deliberately unreadable
/// records and delayed owner-word results; none may bind it to a new same-version job.
#[test]
fn rereads_kept_records_and_word_results_preserve_the_accepted_upgrade_owner() {
    use crate::upgrade_host::{UpgradeSources, WordAsk};
    use aterm_agent::harness::upgrade::Request;
    use aterm_messages::UpgradeWord;

    for path in ["reread", "kept", "word"] {
        let (mut app, _wid, session) = pane_of(24, 120, "claude");
        upgrade_owner_front(&app, session, PGID, 1);
        let row = upgrade_owner_row(&app, session);
        let tab = row.tab.clone();
        app.apply_host_upgrades(vec![row.clone()], vec![]);
        assert_eq!(
            upgrade_owner_arrow(&app, session).state,
            UpState::Ready,
            "{path}: admitted"
        );
        upgrade_owner_front(&app, session, PGID + 1, 1);
        match path {
            "reread" => {
                let sources = UpgradeSources::capture(&app.store);
                app.apply_host_upgrades_from(vec![row.clone()], vec![], sources);
            }
            "kept" => app.apply_host_upgrades(vec![], vec![tab.clone()]),
            "word" => app.apply_upgrade_word(
                &WordAsk {
                    tab: tab.clone(),
                    to: row.to.clone(),
                    session: row.session.clone(),
                    word: UpgradeWord::NotToday,
                    press: None,
                    scope: None,
                },
                Ok(aterm_agent::harness::upgrade_drive::Row {
                    request: Request::DeferUntil(crate::upgrade_host::now_s() + 86_400),
                    request_at: crate::upgrade_host::now_s(),
                    ..row.clone()
                }),
            ),
            _ => unreachable!(),
        }
        assert_eq!(
            upgrade_owner_arrow(&app, session).state,
            UpState::None,
            "{path}: another job"
        );
        upgrade_owner_front(&app, session, PGID, 1);
        assert_eq!(
            upgrade_owner_arrow(&app, session).state,
            UpState::Ready,
            "{path}: original returns"
        );
        if path == "word" {
            let timeline = app.pool.get(session).unwrap().ctx.timeline.lock().unwrap();
            assert!(
                matches!(timeline.upgrade().unwrap().request, Request::DeferUntil(_)),
                "the original accepted row received its word"
            );
        }
    }
}

/// A replacement with the same numeric PID has a different image token. Returning to the
/// original image admits its own retained row; the replacement never inherits it.
#[test]
fn a_retained_upgrade_rejects_a_reused_process_group() {
    let (mut app, _wid, session) = pane_of(24, 120, "claude");
    let original = upgrade_owner_front(&app, session, PGID, 1);
    let row = upgrade_owner_row(&app, session);
    app.apply_host_upgrades(vec![row], vec![]);
    let replacement = upgrade_owner_front(&app, session, PGID, 2);
    assert_ne!(original, replacement);
    app.apply_host_upgrades(vec![], vec![]);
    assert_eq!(upgrade_owner_arrow(&app, session).state, UpState::None);
    upgrade_owner_front(&app, session, PGID, 1);
    assert_eq!(upgrade_owner_arrow(&app, session).state, UpState::Ready);
}

/// A queued clear for the old job cannot consume a newer job's accepted publication, even
/// when both run the same CLI version and offer the same target.
#[test]
fn a_queued_clear_cannot_consume_another_runs_upgrade_publication() {
    use crate::upgrade_host::UpgradeSources;

    let (mut app, _wid, session) = pane_of(24, 120, "claude");
    upgrade_owner_front(&app, session, PGID, 1);
    let original = upgrade_owner_row(&app, session);
    app.apply_host_upgrades(vec![original.clone()], vec![]);
    let clearing_original = UpgradeSources::capture(&app.store);
    upgrade_owner_front(&app, session, PGID + 1, 2);
    let mut replacement = original;
    replacement.session = "00000000-0000-4000-8000-000000000032".into();
    replacement.proc_start = "Mon Oct 26 12:01:00 2026".into();
    app.apply_host_upgrades(vec![replacement.clone()], vec![]);
    app.apply_host_upgrades_from(vec![], vec![], clearing_original);
    assert_eq!(
        app.upgrade_view.row_for(&replacement.tab).unwrap().session,
        replacement.session
    );
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let timeline = ctx.timeline.lock().unwrap();
    assert_eq!(
        timeline.upgrade().unwrap().session,
        replacement.session,
        "the stale clear must leave the accepted live row live"
    );
    assert!(timeline.upgrade_kept().is_none());
}

/// A new process can resume the same conversation at the same build: the pending record's
/// existing process-birth stamp admits that new run. Target or phase changes alone cannot.
#[test]
fn a_new_pending_run_can_admit_a_new_owner_without_inheriting_the_old_runs_word() {
    use crate::upgrade_host::WordAsk;
    use aterm_agent::harness::upgrade::{Phase, Request};
    use aterm_messages::UpgradeWord;

    let (mut app, _wid, session) = pane_of(24, 120, "claude");
    upgrade_owner_front(&app, session, PGID, 1);
    let mut original = upgrade_owner_row(&app, session);
    original.proc_start = "Mon Oct 26 12:00:00 2026".into();
    app.apply_host_upgrades(vec![original.clone()], vec![]);
    upgrade_owner_front(&app, session, PGID + 1, 2);
    let mut retargeted = original.clone();
    retargeted.to = "2.1.311".into();
    retargeted.source = "native".into();
    retargeted.phase = Phase::Announced {
        asks: 1,
        at_s: crate::upgrade_host::now_s(),
    };
    retargeted.wait = "background".into();
    app.apply_host_upgrades(vec![retargeted.clone()], vec![]);
    assert_eq!(
        upgrade_owner_arrow(&app, session).state,
        UpState::None,
        "retargeting and phase changes grant no custody to the other job"
    );

    let mut resumed = retargeted.clone();
    resumed.proc_start = "Mon Oct 26 12:01:00 2026".into();
    resumed.phase = Phase::Pending;
    app.apply_host_upgrades(vec![resumed.clone()], vec![]);
    assert_eq!(resumed.pid, 0);
    assert_eq!(
        upgrade_owner_arrow(&app, session).state,
        UpState::Ready,
        "the real pending-run stamp admits the new same-build owner"
    );

    app.apply_upgrade_word(
        &WordAsk {
            tab: retargeted.tab.clone(),
            to: retargeted.to.clone(),
            session: retargeted.session.clone(),
            word: UpgradeWord::NotToday,
            press: None,
            scope: None,
        },
        Ok(aterm_agent::harness::upgrade_drive::Row {
            request: Request::DeferUntil(crate::upgrade_host::now_s() + 86_400),
            request_at: crate::upgrade_host::now_s(),
            ..retargeted
        }),
    );
    assert_eq!(
        app.upgrade_view.row_for(&resumed.tab).unwrap().proc_start,
        resumed.proc_start
    );
    assert_eq!(
        app.upgrade_view.row_for(&resumed.tab).unwrap().request,
        Request::None,
        "a reply from the previous run cannot replace this run's row"
    );
    app.apply_host_upgrades(vec![resumed.clone()], vec![]);
    assert_eq!(
        app.upgrade_view.row_for(&resumed.tab).unwrap().request,
        Request::None,
        "the stale word cannot be overlaid on a later fresh read either"
    );
    let timeline = app.pool.get(session).unwrap().ctx.timeline.lock().unwrap();
    assert_eq!(timeline.upgrade().unwrap().proc_start, resumed.proc_start);
    assert_eq!(timeline.upgrade().unwrap().request, Request::None);
    drop(timeline);

    let mut newer_target = resumed.clone();
    newer_target.to = "2.1.312".into();
    app.apply_host_upgrades(vec![newer_target.clone()], vec![]);
    app.apply_upgrade_word(
        &WordAsk {
            tab: resumed.tab.clone(),
            to: resumed.to.clone(),
            session: resumed.session.clone(),
            word: UpgradeWord::NotToday,
            press: None,
            scope: None,
        },
        Ok(aterm_agent::harness::upgrade_drive::Row {
            request: Request::DeferUntil(crate::upgrade_host::now_s() + 86_400),
            request_at: crate::upgrade_host::now_s(),
            ..resumed
        }),
    );
    assert_eq!(
        app.upgrade_view.row_for(&newer_target.tab).unwrap().to,
        newer_target.to,
        "an older target's reply cannot replace the current move either"
    );
    assert_eq!(
        app.upgrade_view.row_for(&newer_target.tab).unwrap().request,
        Request::None
    );
}

/// Known footer ownership distinguishes conversations inside one unchanged process image,
/// both while a publication waits and after its row has been retained.
#[test]
fn a_known_foreground_conversation_owns_its_upgrade_publication() {
    use crate::upgrade_host::UpgradeSources;
    use aterm_agent::harness::footer::{FactsOwner, FooterFacts};

    let (mut app, _wid, session) = pane_of(24, 120, "claude");
    upgrade_owner_front(&app, session, PGID, 1);
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let row = upgrade_owner_row(&app, session);
    let facts = |conversation: &str| {
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.set_agent_reader_for_test(true);
        timeline.set_claude_footer(
            PGID,
            Some(1),
            Some(FooterFacts {
                version: Some("2.1.305".into()),
                owner: Some(FactsOwner {
                    floor: Some(0),
                    session_id: conversation.into(),
                }),
                ..FooterFacts::default()
            }),
        );
    };
    facts(&row.session);
    let before_read = UpgradeSources::capture(&app.store);
    facts("00000000-0000-4000-8000-000000000032");
    app.apply_host_upgrades_from(vec![row.clone()], vec![], before_read);
    assert_eq!(upgrade_owner_arrow(&app, session).state, UpState::None);
    facts(&row.session);
    app.apply_host_upgrades(vec![row.clone()], vec![]);
    app.apply_host_upgrades(vec![], vec![]);
    assert_eq!(upgrade_owner_arrow(&app, session).state, UpState::Ready);
    facts("00000000-0000-4000-8000-000000000032");
    assert_eq!(upgrade_owner_arrow(&app, session).state, UpState::None);
    facts(&row.session);
    assert_eq!(
        upgrade_owner_arrow(&app, session).state,
        UpState::Ready,
        "the same conversation can return without reminting custody"
    );
}

/// Codex owns its composer even when its old blank pad would admit host artwork.
#[test]
fn codex_composer_is_unchanged_by_the_host_footer() {
    for cols in [44, 80, 120] {
        let (mut app, wid, session) = pane_of(24, cols, "codex");
        let _host = held(&mut app, session);
        set_program(&app, session, "claude");
        {
            let ctx = &app.pool.get(session).expect("session").ctx;
            let mut timeline = ctx.timeline.lock().unwrap();
            timeline.set_agent_reader_for_test(true);
            assert!(timeline.set_claude_footer(PGID, None, Some(facts())));
        }
        feed(&app, session, &screen(&AT_REST));
        next_frame(&mut app, wid);
        set_program(&app, session, "codex");
        let _copy = copy_for(&app, session);
        for proven in [false, true] {
            set_proven(&app, session, proven);
            for mode in [
                aterm_agent::supervise::Autopilot::Cruise,
                aterm_agent::supervise::Autopilot::Land,
                aterm_agent::supervise::Autopilot::Off,
            ] {
                assert!(app.set_session_autopilot(
                    session,
                    Some(mode),
                    crate::autopilot::Surface::Chip
                ));
                for own_line in [false, true] {
                    bytes(
                        &app,
                        session,
                        codex_screen(cols as usize, Some(SLAB)).as_bytes(),
                    );
                    if own_line {
                        bytes(
                            &app,
                            session,
                            b"\x1b[6;1Hcodex-alab 0.1.0  [Cruise] [Land] [Off]",
                        );
                    }
                    refill(&mut app, wid);
                    let before = pane_cells(&app, wid);
                    let _ = pass(&mut app, wid);
                    assert_eq!(
                        pane_cells(&app, wid),
                        before,
                        "{cols}/{proven}/{mode:?}/{own_line}"
                    );
                    assert!(switch_of(&app, wid, session).is_none());
                    assert_eq!(marks_on(&app, wid, session), 0);
                }
            }
        }
    }
}
