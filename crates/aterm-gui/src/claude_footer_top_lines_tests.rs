// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The top lines of the CLIs that hold no footer facts.

use super::probe::*;
use super::*;
use crate::autopilot::{HarnessSwitch, SwitchOff};
use aterm_effects::harness_card::CardPalette;

/// The foreground group the tests' CLIs run as.
const PGID: i32 = 4343;

/// Codex's slab on a dark ground, measured (`48;2;45;47;51`, the 0.154.0
/// take).
const SLAB: [u8; 3] = [45, 47, 51];

/// Session `session` runs `program` (its published foreground name), as the
/// status sweep publishes it.
fn set_program(app: &App, session: u64, program: &str) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some(program.into()));
}

/// This window's host holding `session`'s claim: the harness carries it,
/// and its top line wears the theme.
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

/// The rows of a measured screen (`aterm-phase`'s fixtures, `aterm ctl
/// text` of the real CLI): its comment lines (`# …`) aside.
fn fixture_rows(fixture: &str) -> Vec<String> {
    fixture
        .lines()
        .filter(|l| !l.starts_with("# "))
        .map(str::to_owned)
        .collect()
}

/// Frame row `row` of the pane as the glass shows it, char by char.
fn glass_chars(app: &App, wid: WindowId, row: usize) -> String {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    ws.input_scratch.cells[strip + row]
        .iter()
        .map(|c| c.ch)
        .collect()
}

/// The pane's rows on the glass, cell for cell, below the strip.
fn pane_cells(app: &App, wid: WindowId) -> Vec<Vec<RenderCell>> {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    ws.input_scratch.cells[strip..].to_vec()
}

/// The rows `after` changed from `before`: each one's index.
fn changed_rows(before: &[Vec<RenderCell>], after: &[Vec<RenderCell>]) -> Vec<usize> {
    (0..before.len().max(after.len()))
        .filter(|&r| before.get(r) != after.get(r))
        .collect()
}

/// The mark layers of `session`'s top line on the window's frame.
fn marks_on(app: &App, wid: WindowId, session: u64) -> usize {
    app.windows[&wid]
        .input_scratch
        .fx_layers
        .iter()
        .filter(|l| l.id == top_mark_id(session))
        .count()
}

/// What one pass cost the top line: allocations in the whole pass, its
/// reads and its compositions.
fn top_cost(app: &mut App, wid: WindowId) -> (usize, usize, usize) {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    let _ = (
        crate::work_counts::take_footer_top_reads(),
        crate::work_counts::take_footer_tops_composed(),
    );
    let (_, allocations) = crate::work_counts::allocations_in(|| pass_on(app, wid, &plan, route));
    (
        allocations,
        crate::work_counts::take_footer_top_reads(),
        crate::work_counts::take_footer_tops_composed(),
    )
}

/// The window, `rows` × `cols`, showing `screen` in its one pane, which
/// runs `program`.
fn pane_of(rows: u16, cols: u16, program: &str) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, rows, cols));
    set_program(&app, session, program);
    (app, wid, session)
}

/// GEMINI CLI WEARS THE LOCKUP ON ITS LIVE-ZONE RULE (design §6.1–§6.2,
/// test 13's glass): on the measured 0.46.0 screens — `idle` and
/// `end-of-turn` at 144 columns, `narrow-end-of-turn` at 72 — a Gemini pane
/// this window's host holds, which holds no footer facts, gets its lockup
/// two `─` in on the `─` rule above its mode row and `alab harness ● on` at
/// its right, one mark layer over the icon; every other row — the `▄` input
/// band, the prompt, the `▀` band, the status row — is byte-identical to
/// Gemini's own.
#[test]
fn gemini_wears_the_lockup_on_its_live_zone_rule() {
    for (fixture, cols) in [
        (
            include_str!("../../aterm-phase/src/fixtures/gemini-0.46.0-idle.txt"),
            144u16,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/gemini-0.46.0-end-of-turn.txt"),
            144,
        ),
        (
            include_str!("../../aterm-phase/src/fixtures/gemini-0.46.0-narrow-end-of-turn.txt"),
            72,
        ),
    ] {
        let rows = fixture_rows(fixture);
        let rule = aterm_phase::gemini::top_rule(&rows).expect("Gemini's live-zone rule");
        assert!(
            rows[rule].chars().all(|c| c == '\u{2500}')
                && rows[rule].chars().count() == cols.into(),
            "the rule full width: {:?}",
            rows[rule]
        );
        let (mut app, wid, session) = pane_of(50, cols, "gemini");
        let _host = held(&mut app, session);
        feed(&app, session, &rows);
        refill(&mut app, wid);
        let before = pane_cells(&app, wid);
        let _ = pass(&mut app, wid);
        let after = pane_cells(&app, wid);
        assert_eq!(
            changed_rows(&before, &after),
            vec![rule],
            "{cols}: the rule alone is written"
        );
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
        assert_eq!(
            pane_cells(&app, wid),
            after,
            "{cols}: the second frame's glass"
        );
        let line = glass_chars(&app, wid, rule);
        assert!(
            line.starts_with("\u{2500}\u{2500}    gemini \u{2500}")
                && line
                    .trim_end()
                    .ends_with(" alab harness \u{25cf} on \u{2500}\u{2500}"),
            "{cols}: the lockup and the lit switch: {line:?}"
        );
        assert_eq!(marks_on(&app, wid, session), 1, "{cols}: one mark layer");
        assert!(
            app.windows[&wid]
                .claude_lights
                .border_toggles
                .iter()
                .any(|b| b.session == session && b.program == aterm_phase::Program::Gemini),
            "{cols}: the switch is a target"
        );
        for i in 0..6 {
            if i % 2 == 0 {
                refill(&mut app, wid);
            }
            let (allocations, reads, composed) = top_cost(&mut app, wid);
            assert_eq!(
                (reads, composed),
                (0, 0),
                "{cols}, frame {i}: nothing read or composed"
            );
            if i % 2 == 1 {
                assert_eq!(allocations, 0, "{cols}, frame {i}: nothing allocated");
            }
            assert_eq!(glass_chars(&app, wid, rule), line, "{cols}, frame {i}");
        }
        set_program(&app, session, "zsh");
        refill(&mut app, wid);
        let before = pane_cells(&app, wid);
        let _ = pass(&mut app, wid);
        assert_eq!(
            changed_rows(&before, &pane_cells(&app, wid)),
            Vec::<usize>::new(),
            "CONTROL {cols}: a shell's screen is its own"
        );
        assert_eq!(marks_on(&app, wid, session), 0, "CONTROL {cols}: no layer");
    }
    let rows = fixture_rows(include_str!(
        "../../aterm-phase/src/fixtures/gemini-0.46.0-details-hidden-idle.txt"
    ));
    assert_eq!(aterm_phase::gemini::top_rule(&rows), None);
    let (mut app, wid, session) = pane_of(50, 144, "gemini");
    let _host = held(&mut app, session);
    feed(&app, session, &rows);
    refill(&mut app, wid);
    let before = pane_cells(&app, wid);
    let _ = pass(&mut app, wid);
    assert_eq!(
        changed_rows(&before, &pane_cells(&app, wid)),
        Vec::<usize>::new(),
        "CONTROL: no rule, nothing written"
    );
    assert_eq!(marks_on(&app, wid, session), 0, "CONTROL: no layer");
}

/// The pane's rows of a Codex screen `cols` wide (the 0.154.0 take's
/// shape, [`aterm_phase::codex`]'s module header): the session card, a
/// blank row, the composer's three rows — pad, prompt, pad — erased onto
/// `slab` (`None`: left on the pane's own ground), and the footer.
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

/// The slab's top pad row in [`codex_screen`] (0-based).
const PAD: usize = 5;

/// A CODEX PAD READS ONLY WHERE ITS CELLS PROVE THE SLAB (design §6.1,
/// §10.2.7; test 14's glass): on Codex's composer erased onto its slab, the
/// pad row above `›` reads as a `Slab` run — the whole pane row, its blank
/// the slab's — and the pane's footer and card read none.
#[test]
fn a_codex_pad_reads_only_where_its_cells_prove_the_slab() {
    let cols = 100usize;
    let (app, wid, session) = pane_of(24, cols as u16, "codex");
    let read_with = |app: &mut App, screen: &str, pictures: &[usize]| {
        bytes(app, session, screen.as_bytes());
        refill(app, wid);
        let ws = app.windows.get_mut(&wid).expect("window");
        let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
        for &col in pictures {
            ws.input_scratch.images[strip + PAD].push((col, super::cost_tests::image()));
        }
        let scratch = &ws.input_scratch;
        let text: Vec<String> = scratch.cells[strip..]
            .iter()
            .map(|row| row.iter().map(|c| c.ch).collect())
            .collect();
        let region = PaneTextRegion {
            strip,
            row_off: 0,
            col_off: 0,
            rows: 24,
            cols,
        };
        let blank = scratch.implicit_blank;
        (
            aterm_phase::codex::composer_pad(&text),
            read_top_run(scratch, region, PAD, TopLine::Slab, blank, false),
            read_top_run(scratch, region, PAD + 3, TopLine::Slab, blank, false),
        )
    };
    let read = |app: &mut App, screen: &str| read_with(app, screen, &[]);
    let mut app = app;
    let (pad, run, footer) = read(&mut app, &codex_screen(cols, Some(SLAB)));
    assert_eq!(pad, Some(PAD), "the text names the pad");
    let run = run.expect("the slab proves its pad");
    assert_eq!((run.col, run.len, run.rule.bg), (0, cols, SLAB));
    assert_eq!(run.rule.ch, ' ', "its blank is the slab's");
    assert_eq!(footer, None, "the footer is on the pane's ground");
    let (pad, run, _) = read(&mut app, &codex_screen(cols, None));
    assert_eq!(pad, Some(PAD), "CONTROL: the text names the same row");
    assert_eq!(
        run, None,
        "CONTROL: a blank row on the pane's ground is no slab"
    );
    let marked = format!(
        "{}\x1b[6;40H\x1b[48;2;45;47;51mx\x1b[0m",
        codex_screen(cols, Some(SLAB))
    );
    let (_, run, _) = read(&mut app, &marked);
    assert_eq!(run, None, "CONTROL: a glyph on the pad");
    let other = codex_screen(cols, Some(SLAB)).replace(
        "\x1b[7;1H\x1b[48;2;45;47;51m",
        "\x1b[7;1H\x1b[48;2;60;30;30m",
    );
    let (_, run, _) = read(&mut app, &other);
    assert_eq!(run, None, "CONTROL: the prompt row off the slab");
    let kitty: Vec<usize> = (cols - 9..cols).collect();
    let (pad, run, _) = read_with(&mut app, &codex_screen(cols, Some(SLAB)), &kitty);
    assert_eq!(pad, Some(PAD), "the text names the pad under the picture");
    let run = run.expect("the slab left of the picture is proven");
    assert_eq!((run.col, run.len, run.rule.bg), (0, cols - 9, SLAB));
    let (_, run, _) = read_with(&mut app, &codex_screen(cols, Some(SLAB)), &[0, 1]);
    assert_eq!(run, None, "CONTROL: a picture at the left edge");
}

/// A SLAB LEFT BEHIND BY A THEME CHANGE KEEPS ITS LETTERS READABLE (design
/// §6.1, §8): Codex keeps the slab it chose at launch when the theme moves
/// under it (an appearance pair turning light), and erases it in the new
/// theme's ink — the real codex-alab 0.164.0-dev's pad, `#2b2d33` on
/// `#2d2f33`, 2026-10-21.
#[test]
fn a_slab_left_by_a_theme_change_keeps_its_letters_readable() {
    use aterm_effects::color_math::contrast_ratio;
    let cols = 100usize;
    let (mut app, wid, session) = pane_of(24, cols as u16, "codex");
    let screen = codex_screen(cols, Some(SLAB)).replace(
        "\x1b[6;1H\x1b[48;2;45;47;51m\x1b[K",
        &format!(
            "\x1b[6;1H\x1b[38;2;43;45;51;48;2;45;47;51m{}",
            " ".repeat(cols)
        ),
    );
    bytes(&app, session, screen.as_bytes());
    refill(&mut app, wid);
    let ws = &app.windows[&wid];
    let scratch = &ws.input_scratch;
    let strip = scratch.cells.len() - usize::from(ws.rows);
    let region = PaneTextRegion {
        strip,
        row_off: 0,
        col_off: 0,
        rows: 24,
        cols,
    };
    let slab = scratch.cells[strip + PAD][0];
    assert_eq!(
        (slab.fg, slab.bg),
        ([43, 45, 51], SLAB),
        "the pad as Codex left it"
    );
    let paper = RenderCell {
        fg: [0x2B, 0x2D, 0x33],
        bg: [0xF6, 0xF7, 0xF9],
        ..scratch.implicit_blank
    };
    let run = read_top_run(scratch, region, PAD, TopLine::Slab, paper, false).expect("the slab");
    assert_eq!(run.rule.bg, SLAB, "the ground is the slab's");
    assert_eq!(
        run.rule.fg, paper.bg,
        "the pane's paper reads on the dark slab"
    );
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |fg: [u8; 3]| contrast_ratio(pack(fg), pack(SLAB));
    let first = brand_letter_fg(CardPalette::Cruise, 0.0, run.rule);
    assert!(
        reads(first) >= aterm_effects::cli_marks::NAME_CONTRAST,
        "the switch's first letter reads {:.2}",
        reads(first)
    );
    let off = SwitchPaint {
        face: HarnessSwitch::Off(SwitchOff::Master),
        hover: false,
        version: None,
    };
    let quiet = switch_cell(
        'a',
        SwitchInk::Label,
        off,
        None,
        run.rule,
        [0x1a, 0x7f, 0x37],
    )
    .fg;
    assert!(
        reads(quiet) >= aterm_effects::cli_marks::NAME_CONTRAST,
        "the quiet label reads {:.2}",
        reads(quiet)
    );
    let before = brand_letter_fg(CardPalette::Cruise, 0.0, slab);
    assert!(
        reads(before) < aterm_effects::cli_marks::NAME_CONTRAST,
        "CONTROL: the slab's own ink leaves the letter at {:.2}",
        reads(before)
    );
}

/// THE ACCENT ON A SLAB THE THEME DID NOT CHOOSE READS ON IT (design §8):
/// the switch's `on` pill is the theme's accent, chosen for the theme's own
/// ground; Codex's slab is its own, and a dark slab outlives a turn to
/// Default Light — the real codex-alab 0.164.0-dev, 2026-10-21, where the
/// light accent's pill read 2.6:1 on the slab and its letters the same on
/// it.
#[test]
fn the_switchs_accent_reads_on_a_slab_the_theme_did_not_choose() {
    use aterm_effects::color_math::contrast_ratio;
    let floor = aterm_effects::cli_marks::NAME_CONTRAST;
    let cols = 100usize;
    let (mut app, wid, session) = pane_of(24, cols as u16, "codex");
    let _host = hosted(&mut app);
    set_proven(&app, session, true);
    assert!(app.set_session_autopilot(
        session,
        Some(aterm_agent::supervise::Autopilot::Off),
        crate::autopilot::Surface::Chip
    ));
    let light = aterm_types::scheme::builtin("Default Light").expect("a built-in");
    app.theme = aterm_render::Theme::from(&light);
    bytes(&app, session, codex_screen(cols, Some(SLAB)).as_bytes());
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    let switch = switch_of(&app, wid, session).expect("codex-alab's switch");
    assert!(
        matches!(switch.face, HarnessSwitch::Off(_)),
        "{:?}",
        switch.face
    );
    let button = switch.button.clone().expect("the off switch's button");
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let accent = crate::chrome_band::band_colors(app.theme).accent;
    assert!(
        reads(accent, SLAB) < 3.0,
        "CONTROL: the light accent reads {:.2} on the dark slab",
        reads(accent, SLAB)
    );
    let cells = &pane_cells(&app, wid)[PAD];
    let mut letters = 0;
    for c in &cells[button] {
        if c.ch.is_alphabetic() {
            letters += 1;
            assert!(
                reads(c.bg, SLAB) >= floor && reads(c.fg, c.bg) >= floor,
                "{:?}: the pill {:.2} on the slab, its letter {:.2} on it",
                c.ch,
                reads(c.bg, SLAB),
                reads(c.fg, c.bg)
            );
        } else if c.ch != ' ' && c.bg == SLAB {
            assert!(
                reads(c.fg, SLAB) >= floor,
                "{:?}: a cap {:.2} on the slab",
                c.ch,
                reads(c.fg, SLAB)
            );
        }
    }
    assert_eq!(letters, 2, "the button's `on`");
}

/// Codex's agent switch's copy for the session's group: what admits a
/// codex-alab whose self-claim is not proven (`harness_host`'s
/// `codex_alab_runs`).
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

/// Session `session`'s claim: codex-alab's own, PROVEN (`self_claim`), or
/// none.
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
    });
}

/// The switch `session`'s top line put on the glass, if any.
fn switch_of(app: &App, wid: WindowId, session: u64) -> Option<crate::claude_lights::BorderSwitch> {
    app.windows[&wid]
        .claude_lights
        .border_toggles
        .iter()
        .find(|b| b.session == session)
        .cloned()
}

/// CODEX GETS A SWITCH ONLY AS CODEX-ALAB (design §6.2): at every width
/// from 20 to 160 a vendor `codex` pane — no proven self-claim, no copy, no
/// ALab image: no host admits it — wears the lockup named `codex` on its
/// slab's pad (its blanks the slab's, never tinted) and no switch; a
/// codex-alab pane whose self-claim is proven wears the lockup named
/// `codex-alab` (the owner's name for it, its letters floored to 4.5:1 on
/// the slab) and `alab harness ● on`, its glyphs on Cruise's arc in the
/// slab's letter law, and a switch whose press writes the session's word
/// `off`; it never reads the vendor's bare `codex`.
#[test]
fn codex_gets_a_switch_only_as_codex_alab() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let (mut lockups, mut alab_names) = (0, 0);
    for cols in 20..=160usize {
        assert!(app.apply_term_resize(wid, 24, cols as u16));
        bytes(&app, session, codex_screen(cols, Some(SLAB)).as_bytes());
        set_proven(&app, session, false);
        refill(&mut app, wid);
        let before = pane_cells(&app, wid);
        let _ = pass(&mut app, wid);
        let after = pane_cells(&app, wid);
        let changed = changed_rows(&before, &after);
        assert!(
            changed.iter().all(|&r| r == PAD),
            "{cols}: vendor codex writes its pad alone: {changed:?}"
        );
        assert!(
            switch_of(&app, wid, session).is_none(),
            "{cols}: vendor codex has no switch"
        );
        let line = glass_chars(&app, wid, PAD);
        if cols >= 14 {
            assert!(
                line.starts_with("      codex ") && !line.contains("alab"),
                "{cols}: the lockup alone, the vendor's name: {line:?}"
            );
            assert!(
                after[PAD].iter().all(|c| c.bg == SLAB),
                "{cols}: on the slab, its blanks untinted"
            );
            assert_eq!(marks_on(&app, wid, session), 1, "{cols}: one mark layer");
            lockups += 1;
        }
        set_proven(&app, session, true);
        refill(&mut app, wid);
        let before = pane_cells(&app, wid);
        let slab = before[PAD][0];
        let _ = pass(&mut app, wid);
        let after = pane_cells(&app, wid);
        let changed = changed_rows(&before, &after);
        assert!(
            changed.iter().all(|&r| r == PAD),
            "{cols}: codex-alab writes its pad alone: {changed:?}"
        );
        let switch = switch_of(&app, wid, session).expect("codex-alab's switch");
        assert_eq!(
            (switch.face, switch.program),
            (HarnessSwitch::On, aterm_phase::Program::Codex),
            "{cols}"
        );
        let lockup = app.windows[&wid]
            .claude_lights
            .mark_hits
            .iter()
            .find(|m| m.session == session)
            .map(|m| m.cols.clone())
            .unwrap_or_default();
        for (c, cell) in after[PAD].iter().enumerate() {
            if !lockup.contains(&c) && !switch.cols.contains(&c) {
                assert_eq!(*cell, before[PAD][c], "{cols}: column {c} untinted");
            }
        }
        let glyphs: Vec<&RenderCell> = after[PAD][switch.switch.clone()]
            .iter()
            .filter(|c| c.ch != ' ')
            .collect();
        let n = glyphs.len();
        for (k, c) in glyphs.iter().enumerate() {
            let t = if n > 1 { k * 255 / (n - 1) } else { 0 };
            let want = brand_letter_fg(
                CardPalette::Cruise,
                f32::from(u8::try_from(t).expect("a stop")) / 255.0,
                slab,
            );
            assert!(
                c.fg == want && c.bold && c.bg == SLAB,
                "{cols}: glyph {k} ({:?}) on Cruise's arc",
                c.ch
            );
        }
        let line = glass_chars(&app, wid, PAD);
        assert!(
            !line.contains(" codex "),
            "{cols}: codex-alab never reads the vendor's name: {line:?}"
        );
        if cols >= 40 {
            assert!(
                line.starts_with("      codex-alab ")
                    && line.contains(" alab harness \u{25cf} on "),
                "{cols}: {line:?}"
            );
            let ground = u32::from_be_bytes([0, slab.bg[0], slab.bg[1], slab.bg[2]]);
            for (k, c) in after[PAD][6..16].iter().enumerate() {
                let fg = u32::from_be_bytes([0, c.fg[0], c.fg[1], c.fg[2]]);
                assert!(
                    c.bold
                        && c.bg == SLAB
                        && fg == aterm_effects::cli_marks::name_fg(CliMark::Codex, k, ground)
                        && aterm_effects::color_math::contrast_ratio(fg, ground)
                            >= aterm_effects::cli_marks::NAME_CONTRAST,
                    "{cols}: letter {k} ({:?}) floored on the slab",
                    c.ch
                );
            }
            alab_names += 1;
        }
    }
    assert!(lockups > 100 && alab_names > 100, "{lockups} {alab_names}");
    app.press_harness_switch(wid, session, HarnessSwitch::On);
    assert_eq!(
        app.session_autopilot(session).0,
        Some(aterm_agent::supervise::Autopilot::Off),
        "the press turned the session off"
    );
    assert!(app.set_session_autopilot(
        session,
        Some(aterm_agent::supervise::Autopilot::Cruise),
        crate::autopilot::Surface::Chip
    ));
    set_proven(&app, session, false);
    let _copy = copy_for(&app, session);
    assert!(app.apply_term_resize(wid, 24, 100));
    bytes(&app, session, codex_screen(100, Some(SLAB)).as_bytes());
    refill(&mut app, wid);
    let slab = pane_cells(&app, wid)[PAD][0];
    let _ = pass(&mut app, wid);
    let switch = switch_of(&app, wid, session).expect("CONTROL: admitted, a switch");
    assert_eq!(switch.face, HarnessSwitch::On, "CONTROL");
    let cells = &pane_cells(&app, wid)[PAD];
    let lit = cells[switch.switch.clone()]
        .iter()
        .find(|c| c.ch == '\u{25cf}')
        .expect("CONTROL: the lit state");
    let mark_fg = crate::chrome_band::band_colors(app.theme).accent;
    assert_eq!(lit.fg, mark_fg, "CONTROL: bare, `●` in the accent");
    let first = cells[switch.switch.clone()]
        .iter()
        .find(|c| c.ch != ' ')
        .expect("CONTROL: the label");
    assert_ne!(
        first.fg,
        brand_letter_fg(CardPalette::Cruise, 0.0, slab),
        "CONTROL: bare, no glyph on the arc"
    );
    assert!(
        glass_chars(&app, wid, PAD).starts_with("      codex-alab "),
        "CONTROL: admitted, codex-alab: {:?}",
        glass_chars(&app, wid, PAD)
    );
}

/// THE PET HOLDS CODEX'S LOCKUP AND SWITCH, NEVER THE PAD BETWEEN
/// (`crate::pet_chrome`): what the footer's write puts on the glass is the
/// resident pet's protected ground, and on Codex's slab every cell aterm
/// writes is a blank of the slab, so the write is all the pet has to go by.
#[test]
fn the_pet_holds_codexs_lockup_and_switch_never_the_pad_between() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let mut gaps = 0;
    for cols in [40usize, 100, 160] {
        assert!(app.apply_term_resize(wid, 24, cols as u16));
        for proven in [false, true] {
            bytes(&app, session, codex_screen(cols, Some(SLAB)).as_bytes());
            set_proven(&app, session, proven);
            refill(&mut app, wid);
            let _ = pass(&mut app, wid);
            app.splice_autopilot_menu(wid);
            let lockup = app.windows[&wid]
                .claude_lights
                .mark_hits
                .iter()
                .find(|m| m.session == session)
                .map(|m| m.cols.clone())
                .expect("the lockup on the slab");
            let switch = switch_of(&app, wid, session).map(|s| s.cols);
            assert_eq!(switch.is_some(), proven, "{cols}: a switch for codex-alab");
            let held = app.windows[&wid].pet_chrome.held().to_vec();
            let covered = |col: usize| {
                let (r, c) = (PAD as f32 + 0.5, col as f32 + 0.5);
                held.iter().any(|rect| {
                    rect.row <= r
                        && r < rect.row + rect.rows
                        && rect.col <= c
                        && c < rect.col + rect.cols
                })
            };
            let ours = |col: usize| {
                lockup.contains(&col) || switch.as_ref().is_some_and(|s| s.contains(&col))
            };
            let wrong: Vec<usize> = (0..cols).filter(|&c| covered(c) != ours(c)).collect();
            assert!(
                wrong.is_empty(),
                "{cols}, proven {proven}: lockup {lockup:?}, switch {switch:?}: columns \
                 held other than the host's marks, or marks not held: {wrong:?}"
            );
            if let Some(s) = &switch {
                gaps += s.start - lockup.end;
            }
        }
    }
    assert!(gaps > 100, "the slab's pad lies between the marks: {gaps}");
}

/// A SENT MESSAGE IS NEVER CODEX'S TOP LINE (design §5.6 invariant 1,
/// §10.2.7): a codex-alab pane whose self-claim is proven, its composer
/// replaced by an approval (`› 1. Yes, proceed`) under the message the
/// person sent (`› fix the tests` on the slab's tint, padded as the
/// composer is), is written nowhere: no row changes, no mark layer stands,
/// no switch is put where a press would reach it.
#[test]
fn a_sent_message_is_never_codexs_top_line() {
    let cols = 100usize;
    let (mut app, wid, session) = pane_of(24, cols as u16, "codex");
    let _host = hosted(&mut app);
    set_proven(&app, session, true);
    let on = format!("\x1b[48;2;{};{};{}m", SLAB[0], SLAB[1], SLAB[2]);
    let screen = |under: &str| {
        let mut out = codex_screen(cols, Some(SLAB));
        out.push_str("\x1b[9;1H\x1b[K");
        out.push_str(&format!("\x1b[7;1H{on}\x1b[K\u{203A} fix the tests\x1b[0m"));
        out.push_str(under);
        out
    };
    let approval = screen(concat!(
        "\x1b[11;1H  Would you like to run the following command?",
        "\x1b[13;1H\u{203A} 1. Yes, proceed",
        "\x1b[14;1H  2. No, and tell Codex what to do differently",
        "\x1b[13;3H",
    ));
    bytes(&app, session, approval.as_bytes());
    refill(&mut app, wid);
    let ws = &app.windows[&wid];
    let scratch = &ws.input_scratch;
    let strip = scratch.cells.len() - usize::from(ws.rows);
    let text: Vec<String> = scratch.cells[strip..]
        .iter()
        .map(|row| row.iter().map(|c| c.ch).collect())
        .collect();
    assert!(
        text[PAD + 1].starts_with("\u{203A} fix the tests"),
        "{text:#?}"
    );
    assert_eq!(aterm_phase::codex::composer_pad(&text), None, "no composer");
    let region = PaneTextRegion {
        strip,
        row_off: 0,
        col_off: 0,
        rows: 24,
        cols,
    };
    assert!(
        read_top_run(
            scratch,
            region,
            PAD,
            TopLine::Slab,
            scratch.implicit_blank,
            false
        )
        .is_some(),
        "CONTROL: the message's pad is a slab its cells prove"
    );
    let before = pane_cells(&app, wid);
    let _ = pass(&mut app, wid);
    assert_eq!(
        changed_rows(&before, &pane_cells(&app, wid)),
        Vec::<usize>::new(),
        "the approval's screen is Codex's own"
    );
    assert_eq!(marks_on(&app, wid, session), 0, "no layer");
    assert!(switch_of(&app, wid, session).is_none(), "no switch");
    let composer = screen(&format!(
        "\x1b[11;1H{on}\x1b[K\x1b[0m\x1b[12;1H{on}\x1b[K\u{203A} Ask Codex to do anything\x1b[0m\x1b[13;1H{on}\x1b[K\x1b[0m\x1b[14;1H  ? for shortcuts\x1b[12;3H"
    ));
    bytes(&app, session, composer.as_bytes());
    refill(&mut app, wid);
    let before = pane_cells(&app, wid);
    let _ = pass(&mut app, wid);
    assert_eq!(
        changed_rows(&before, &pane_cells(&app, wid)),
        vec![10],
        "CONTROL: the composer's pad alone"
    );
    assert_eq!(marks_on(&app, wid, session), 1, "CONTROL: one layer");
    assert_eq!(
        switch_of(&app, wid, session).map(|b| b.frame_row - strip),
        Some(10),
        "CONTROL: the switch on the composer's pad"
    );
}

/// The lit switch.
const LIT: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::On,
    hover: false,
    version: None,
};

/// The session's own Off: `○ off ▐ on ▌`.
const OFF: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Off(SwitchOff::Session),
    hover: false,
    version: None,
};

/// A plain top rule's run of `len` cells at frame column 2.
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
    }
}

/// The top line of `program`'s `build` on `run`, resting or with the
/// pointer on its lockup.
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
        line: TopLine::Rule,
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
        mark_fg: [0xd7, 0x77, 0x57],
        face: KeyFace::default(),
        cell: (7, 14),
        clip: aterm_core::render::PxRect::EVERYWHERE,
        mark_hover: hover,
        title_gen: u64::from(hover),
    }
}

/// Column `c` of a composition over `run` (the run's own `─` where it
/// writes none).
fn cell_at(top: &ComposedTop, run: TopRun, c: usize) -> RenderCell {
    c.checked_sub(top.col)
        .and_then(|k| top.cells.get(k))
        .copied()
        .unwrap_or(run.rule)
}

/// Whether a hovered composition `hovered` keeps `rest`'s layout: the
/// switch at the same columns, every cell outside its lockup (pads and
/// title included) the resting cell — so the same tint stops — and, where
/// a title is drawn, two `─` after it before the switch (or the span's end).
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

/// The title a hovered composition shows: the cells from the name's place
/// to the lockup's trailing pad.
fn title_of(top: &ComposedTop, run: TopRun) -> String {
    let at = top.lockup.start + aterm_effects::cli_marks::ICON_RUN;
    (at..top.lockup.end.saturating_sub(1))
        .map(|c| cell_at(top, run, c).ch)
        .collect()
}

/// THE HOVER TITLE NEVER MOVES THE SWITCH (design §7): for agy, version
/// `1.4.0`, and for codex-alab, whose name is the longest, version
/// `0.164.0`, at every span from 8 to 80 and under each switch and theme —
/// lit on Cruise, Off in white, lit bare, no host — the composition with the
/// pointer on the lockup lays the switch at the resting columns and every
/// cell outside the lockup as resting (so the tint's stops), leaves two `─`
/// before the switch, and names `Antigravity CLI 1.4.0` (21 cells) where it
/// fits, `Antigravity CLI` where only the product does, and `agy` where
/// neither does — falling back in that order as the span narrows on one
/// form of the switch (a narrower form frees room again); and
/// `Codex 0.164.0` where it fits, else `codex-alab`: never the product alone,
/// `Codex`, narrower than the name it would cover.
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
            (Program::Codex, Build::Alab),
            "0.164.0",
            ["codex-alab", "", "Codex 0.164.0"],
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
            (Some(OFF), Some(CardPalette::Off)),
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
                );
                compose_top(
                    &lockup_key(lockup, run, switch, palette, true),
                    &mut hovered,
                    Some(version),
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
                let tier = rest.switch.as_ref().map(|s| s.tier);
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
                    0,
                );
                if keeps_the_layout(&rest, &loose, run).is_err() {
                    caught += 1;
                }
                if build == Build::Alab {
                    let short = program.product();
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

/// Window pixel at the centre of frame cell `(row, col)` of `wid`: the
/// inverse of `App::frame_cell_at`, checked against it.
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

/// [`the_hover_title_never_moves_the_switch`] on the glass: Claude Code's
/// pane this window's host holds, its facts naming build 2.1.283 — and the
/// title let go with no motion once its line moves from under the still
/// pointer (the composer grown a row), the moved line at rest; while the
/// line stays under it, the title stands frame after frame (CONTROL).
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
        hovered.contains("    Claude Code 2.1.283 \u{2500}\u{2500}"),
        "the title: {hovered:?}"
    );
    assert_eq!(
        switch_of(&app, wid, session).map(|b| b.cols),
        Some(switch.cols.clone()),
        "the switch stood"
    );
    app.windows.get_mut(&wid).expect("window").last_cursor_px = (x, y);
    assert!(
        !app.press_claude_light(wid),
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

/// The probe's screen as Claude lays it in a window `rows` × `cols`: its
/// rules as wide as the window, a blank transcript row above it for every
/// row more.
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

/// Claude Code's pane ([`probe_glass`]), read as Claude, whose top line the
/// harness holds — its lockup and its switch on the glass — and the host's
/// guard.
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

/// The window re-gridded to `rows` × `cols` and Claude's screen re-laid in
/// it — no frame painted yet.
fn regrid(app: &mut App, wid: WindowId, session: u64, rows: u16, cols: u16) {
    assert!(
        app.apply_term_resize(wid, rows, cols),
        "the window re-grids"
    );
    feed(app, session, &screen_at(&AT_REST, rows, cols));
}

/// The next frame: extracted and the footer's pass run on it.
fn next_frame(app: &mut App, wid: WindowId) {
    refill(app, wid);
    let _ = pass(app, wid);
}

/// The pointer moved to frame cell `cell` and resting there: the motion's
/// pixel kept as the window's, and the glass asked as a motion asks it.
fn point_at(app: &mut App, wid: WindowId, cell: (usize, usize)) -> crate::claude_lights::OverChips {
    let (x, y) = cell_px(app, wid, cell);
    let ws = app.windows.get_mut(&wid).expect("window");
    ws.last_cursor_px = (x, y);
    ws.pointer_position_known = true;
    app.track_claude_light_hover(wid, x, y)
}

/// The session whose harness switch the pointer is over, as the next frame
/// draws it.
fn switch_hovered(app: &App, wid: WindowId) -> Option<u64> {
    app.windows[&wid].claude_lights.switch_hover
}

/// A RE-GRID'S POINTER IS HIT-TESTED ON THE GLASS IT LANDS ON: Claude Code's
/// pane this window's host holds, the pointer resting on its top line's
/// switch.
#[test]
fn a_regrid_hit_tests_the_glass_it_lands_on() {
    use crate::claude_lights::OverChips;
    let (mut app, wid, session, _host) = claude_held();
    for _ in 0..2 {
        next_frame(&mut app, wid);
    }
    let laid = switch_of(&app, wid, session).expect("the switch");
    let on = (laid.frame_row, laid.switch.start + laid.switch.len() / 2);
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
        !app.press_claude_light(wid),
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

/// A RE-GRID BRINGS THE HOVER TITLE UNDER A STILL POINTER: the pointer
/// resting two rows under Claude Code's top line, on the composer's bottom
/// rule, where the line's lockup lands when the window grows two rows and
/// Claude lays its screen two rows lower.
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
        titled.contains("    Claude Code 2.1.283 \u{2500}\u{2500}"),
        "the title: {titled:?}"
    );
}
