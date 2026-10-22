// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The top lines of the CLIs that hold no footer facts — Gemini CLI's
//! live-zone `─` rule, Codex's composer slab — spliced before the footer's
//! gates turn them away (design §6.2), and the hover title every lockup
//! answers the pointer with (§7): Gemini's lockup and switch on its rule and
//! never its band, Codex's only on a slab its cells prove, its switch only
//! for codex-alab, and a title that never moves the switch.

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
/// and its top line wears the theme. The guard shuts the host down.
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
/// Gemini's own. At rest the pass reads, composes and allocates nothing for
/// it, frame after frame, the glass the same.
///
/// NEGATIVE CONTROLS: with the UI details hidden, where Gemini draws no
/// rule, nothing is written and no layer stands (its band is never the
/// line); and the same screen under a shell's name gets nothing.
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
        // A second frame, every buffer grown.
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
        // CONTROL: the same screen under a shell's name.
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
    // CONTROL: the details hidden, no rule: Gemini's band is never the line.
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
/// `slab` (`None`: left on the pane's own ground), and the footer. Every
/// row cut to the width, so none wraps.
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
///
/// A picture the program placed on its own slab — codex-alab's composer
/// kitty, a 9×5-cell PNG over the pad row's last nine columns (0.164.0-dev,
/// the real-Codex lab, 2026-10-21) — is the program's ink: the run stops
/// at its first cell, so the lockup and the switch stand left of it.
///
/// NEGATIVE CONTROLS: the same rows on the pane's own ground — which the
/// text reader cannot tell apart (`composer_pad` names the same row) — read
/// no run; nor does a pad with a glyph on it, nor one over a prompt row on
/// another ground, nor one whose picture covers its left edge.
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
    // CONTROL: the same rows on the pane's own ground.
    let (pad, run, _) = read(&mut app, &codex_screen(cols, None));
    assert_eq!(pad, Some(PAD), "CONTROL: the text names the same row");
    assert_eq!(
        run, None,
        "CONTROL: a blank row on the pane's ground is no slab"
    );
    // CONTROL: a glyph on the pad.
    let marked = format!(
        "{}\x1b[6;40H\x1b[48;2;45;47;51mx\x1b[0m",
        codex_screen(cols, Some(SLAB))
    );
    let (_, run, _) = read(&mut app, &marked);
    assert_eq!(run, None, "CONTROL: a glyph on the pad");
    // CONTROL: the prompt row on another ground.
    let other = codex_screen(cols, Some(SLAB)).replace(
        "\x1b[7;1H\x1b[48;2;45;47;51m",
        "\x1b[7;1H\x1b[48;2;60;30;30m",
    );
    let (_, run, _) = read(&mut app, &other);
    assert_eq!(run, None, "CONTROL: the prompt row off the slab");
    // codex-alab's kitty over the pad's last nine columns: the run is the
    // slab left of it, every cell of it the slab's blank.
    let kitty: Vec<usize> = (cols - 9..cols).collect();
    let (pad, run, _) = read_with(&mut app, &codex_screen(cols, Some(SLAB)), &kitty);
    assert_eq!(pad, Some(PAD), "the text names the pad under the picture");
    let run = run.expect("the slab left of the picture is proven");
    assert_eq!((run.col, run.len, run.rule.bg), (0, cols - 9, SLAB));
    // CONTROL: a picture at the slab's left edge leaves no run.
    let (_, run, _) = read_with(&mut app, &codex_screen(cols, Some(SLAB)), &[0, 1]);
    assert_eq!(run, None, "CONTROL: a picture at the left edge");
}

/// A SLAB LEFT BEHIND BY A THEME CHANGE KEEPS ITS LETTERS READABLE (design
/// §6.1, §8): Codex keeps the slab it chose at launch when the theme moves
/// under it (an appearance pair turning light), and erases it in the new
/// theme's ink — the real codex-alab 0.164.0-dev's pad, `#2b2d33` on
/// `#2d2f33`, 2026-10-21. The run's ink, which every law lifts and quiets
/// toward, is the pane's own ink or ground, whichever reads farther from
/// the slab, so the switch's first letter reads at 4.5:1 or better on it
/// and its quiet label clears 3:1.
///
/// NEGATIVE CONTROL: the slab cell's own ink, as the laws took it before,
/// puts that letter under 4.5:1 (3.1 measured on the lab's frame).
#[test]
fn a_slab_left_by_a_theme_change_keeps_its_letters_readable() {
    use aterm_effects::color_math::contrast_ratio;
    let cols = 100usize;
    let (mut app, wid, session) = pane_of(24, cols as u16, "codex");
    // The pad as the reader resolves it: Codex's erase keeps the default
    // ink, which is now Default Light's `#2b2d33`, on the dark slab.
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
    // Default Light's pair (`aterm_types::scheme`): its ink and its paper.
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
    let quiet = mix(run.rule.fg, run.rule.bg, 0.3);
    assert!(
        reads(quiet) >= 3.0,
        "the quiet label reads {:.2}",
        reads(quiet)
    );
    // CONTROL: the slab cell's own ink.
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
/// it. On a slab the accent is floored along its own hue to 4.5:1 there, so
/// the pill's caps read on the slab and its letters read on the pill.
///
/// NEGATIVE CONTROL: the theme's accent as chosen reads under 3:1 on that
/// slab.
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
/// `codex_alab_runs`). Forgotten when the guard drops.
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
/// ALab image: no host admits it — wears the lockup on its slab's pad (its
/// blanks the slab's, never tinted) and no switch; a codex-alab pane whose
/// self-claim is proven wears the lockup and `alab harness ● on`, its glyphs
/// on Cruise's arc in the slab's letter law, and a switch whose press
/// writes the session's word `off`. Only the pad row is written: the card,
/// the prompt row and the footer are byte-identical to Codex's own.
///
/// NEGATIVE CONTROL: the same pane admitted by its copy alone, its claim
/// not proven, draws the bare switch — its `●` in the accent, none of its
/// glyphs on the arc.
#[test]
fn codex_gets_a_switch_only_as_codex_alab() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let mut lockups = 0;
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
                "{cols}: the lockup alone: {line:?}"
            );
            assert!(
                after[PAD].iter().all(|c| c.bg == SLAB),
                "{cols}: on the slab, its blanks untinted"
            );
            assert_eq!(marks_on(&app, wid, session), 1, "{cols}: one mark layer");
            lockups += 1;
        }
        // codex-alab, its self-claim proven.
        set_proven(&app, session, true);
        refill(&mut app, wid);
        let before = pane_cells(&app, wid);
        // The slab's blank, as Codex left it: every ink is laid on it.
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
        // The slab is never tinted: every cell the lockup and the switch
        // leave is Codex's own blank.
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
        // Its glyphs walk Cruise's arc in the slab's letter law.
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
        if cols >= 35 {
            assert!(
                glass_chars(&app, wid, PAD).starts_with("      codex ")
                    && glass_chars(&app, wid, PAD).contains(" alab harness \u{25cf} on "),
                "{cols}: {:?}",
                glass_chars(&app, wid, PAD)
            );
        }
    }
    assert!(lockups > 100, "{lockups}");
    // The press writes the session's word `off`.
    app.press_harness_switch(wid, session, HarnessSwitch::On);
    assert_eq!(
        app.session_autopilot(session).0,
        Some(aterm_agent::supervise::Autopilot::Off),
        "the press turned the session off"
    );
    // CONTROL: admitted by its copy alone, its claim not proven.
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
}

/// A SENT MESSAGE IS NEVER CODEX'S TOP LINE (design §5.6 invariant 1,
/// §10.2.7): a codex-alab pane whose self-claim is proven, its composer
/// replaced by an approval (`› 1. Yes, proceed`) under the message the
/// person sent (`› fix the tests` on the slab's tint, padded as the
/// composer is), is written nowhere: no row changes, no mark layer stands,
/// no switch is put where a press would reach it.
///
/// NEGATIVE CONTROL: the sent message's top pad is a slab its cells prove —
/// the glass alone cannot tell it from the composer's, so the text reader
/// is what refuses it — and the same pane with its composer back under the
/// message wears the lockup and the switch on the composer's pad alone.
#[test]
fn a_sent_message_is_never_codexs_top_line() {
    let cols = 100usize;
    let (mut app, wid, session) = pane_of(24, cols as u16, "codex");
    let _host = hosted(&mut app);
    set_proven(&app, session, true);
    let on = format!("\x1b[48;2;{};{};{}m", SLAB[0], SLAB[1], SLAB[2]);
    // The card, a blank row, the sent message's slab (rows 6–8), a blank
    // row, then `under` from row 10.
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
    // CONTROL: the composer back under the message.
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
};

/// The session's own Off: `○ off ▐ on ▌`.
const OFF: SwitchPaint = SwitchPaint {
    face: HarnessSwitch::Off(SwitchOff::Session),
    hover: false,
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

/// agy's top line on `run`, resting or with the pointer on its lockup.
fn agy_key(
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
        program: aterm_phase::Program::Agy,
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
/// `Err` names the first breach.
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
    // A title drawn leaves the switch its lead of `─`.
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
/// `1.4.0`, at every span from 8 to 80 and under each switch and theme —
/// lit on Cruise, Off in white, lit bare, no host — the composition with the
/// pointer on the lockup lays the switch at the resting columns and every
/// cell outside the lockup as resting (so the tint's stops), leaves two `─`
/// before the switch, and names `Antigravity CLI 1.4.0` (21 cells) where it
/// fits, `Antigravity CLI` where only the product does, and `agy` where
/// neither does — falling back in that order as the span narrows. On the
/// glass, the pointer resting on Claude Code's lockup shows `Claude Code
/// 2.1.283` with the switch where it stood; the press there is the pane's,
/// and the pointer leaving puts the name back.
///
/// NEGATIVE CONTROL: a title allowed into the switch's two-cell lead
/// (`compose_top_in` with no lead) is caught.
#[test]
fn the_hover_title_never_moves_the_switch() {
    let rank = |title: &str| match title {
        "Antigravity CLI 1.4.0" => 2,
        "Antigravity CLI" => 1,
        "agy" => 0,
        other => panic!("not a title: {other:?}"),
    };
    let mut seen = [0usize; 3];
    let mut caught = 0;
    for (switch, palette) in [
        (Some(LIT), Some(CardPalette::Cruise)),
        (Some(OFF), Some(CardPalette::Off)),
        (Some(LIT), None),
        (None, None),
    ] {
        let mut last = 2;
        for len in (8..=80).rev() {
            let run = plain_run(len);
            let (mut rest, mut hovered) = (ComposedTop::default(), ComposedTop::default());
            compose_top(
                &agy_key(run, switch, palette, false),
                &mut rest,
                Some("1.4.0"),
            );
            compose_top(
                &agy_key(run, switch, palette, true),
                &mut hovered,
                Some("1.4.0"),
            );
            keeps_the_layout(&rest, &hovered, run)
                .unwrap_or_else(|e| panic!("{len}, {switch:?} {palette:?}: {e}"));
            let full = rest.lockup.len() == aterm_effects::cli_marks::CliMark::Agy.lockup_cells();
            if !full {
                assert_eq!(rest.cells, hovered.cells, "{len}: no name, no title");
                continue;
            }
            let title = title_of(&hovered, run);
            let r = rank(&title);
            assert!(r <= last, "{len}: {title:?} after a narrower title");
            last = r;
            seen[r] += 1;
            // CONTROL: the title let into the switch's lead.
            let mut loose = ComposedTop::default();
            compose_top_in(
                &agy_key(run, switch, palette, true),
                &mut loose,
                Some("1.4.0"),
                0,
            );
            if keeps_the_layout(&rest, &loose, run).is_err() {
                caught += 1;
            }
        }
    }
    assert!(seen.iter().all(|&n| n > 0), "every title is seen: {seen:?}");
    assert!(
        caught > 0,
        "CONTROL: a title in the switch's lead is caught"
    );
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
    // The pointer on the lockup's name.
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
    // CONTROL: frame after frame under the still pointer, the title stands.
    for i in 0..3 {
        let _ = pass(&mut app, wid);
        assert!(
            app.windows[&wid].claude_lights.mark_hovered(session),
            "CONTROL {i}: the hover stands"
        );
        assert_eq!(glass_chars(&app, wid, top), hovered, "CONTROL {i}");
    }
    // The composer grows a row under the still pointer (a Shift+Enter): its
    // top rule moves up a row, from under the pointer, with no motion.
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
    // The pointer leaves: the name is back.
    let (x, y) = cell_px(&app, wid, (lockup.frame_row - 3, 4));
    let _ = app.track_claude_light_hover(wid, x, y);
    assert!(!app.windows[&wid].claude_lights.mark_hovered(session));
    let _ = pass(&mut app, wid);
    assert_eq!(glass_chars(&app, wid, top), resting, "the name is back");
}
