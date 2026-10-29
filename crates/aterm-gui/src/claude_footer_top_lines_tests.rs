// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The top lines of the CLIs that hold no footer facts.

use super::probe::*;
use super::*;
use crate::autopilot::{HarnessSwitch, SwitchOff};
use aterm_effects::harness_card::CardPalette;

const PGID: i32 = 4343;

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

fn changed_rows(before: &[Vec<RenderCell>], after: &[Vec<RenderCell>]) -> Vec<usize> {
    (0..before.len().max(after.len()))
        .filter(|&r| before.get(r) != after.get(r))
        .collect()
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
    assert!(app.apply_term_resize(wid, rows, cols));
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

const PAD: usize = 5;

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
    let off = SwitchPaint::bare(
        HarnessSwitch::Off(SwitchOff::Master),
        Some(aterm_phase::CliHarness::ClaudeCode),
    );
    let quiet = FaceInks::of(&off, None, run.rule, [0x1a, 0x7f, 0x37]).muted;
    assert!(
        reads(quiet) >= aterm_effects::cli_marks::NAME_CONTRAST,
        "the quiet label reads {:.2}",
        reads(quiet)
    );
    let letter = CardPalette::Cruise.letter(0.0, true);
    let letter = [(letter >> 16) as u8, (letter >> 8) as u8, letter as u8];
    let before = mix(letter, slab.fg, 0.25);
    assert!(
        reads(before) < aterm_effects::cli_marks::NAME_CONTRAST,
        "CONTROL: the slab's own ink, unfloored, leaves the letter at {:.2}",
        reads(before)
    );
}

#[test]
fn the_switchs_accent_reads_on_a_slab_the_theme_did_not_choose() {
    use aterm_effects::color_math::contrast_ratio;
    let floor = aterm_effects::cli_marks::NAME_CONTRAST;
    let cols = 100usize;
    let (mut app, wid, session) = pane_of(24, cols as u16, "codex");
    let _host = hosted(&mut app);
    set_proven(&app, session, true);
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
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
    let pack = |c: [u8; 3]| u32::from_be_bytes([0, c[0], c[1], c[2]]);
    let reads = |a: [u8; 3], b: [u8; 3]| contrast_ratio(pack(a), pack(b));
    let accent = crate::chrome_band::band_colors(app.theme).accent;
    assert!(
        reads(accent, SLAB) < 3.0,
        "CONTROL: the light accent reads {:.2} on the dark slab",
        reads(accent, SLAB)
    );
    let cells = &pane_cells(&app, wid)[PAD];
    let reads_on_slab = |c: &RenderCell, what: &str| {
        assert_eq!(c.bg, SLAB, "{what} {:?}: on the slab", c.ch);
        assert!(
            reads(c.fg, SLAB) >= floor,
            "{what} {:?}: {:.2} on the slab",
            c.ch,
            reads(c.fg, SLAB)
        );
    };
    let letters = |range: std::ops::Range<usize>, what: &str| {
        cells[range]
            .iter()
            .filter(|c| c.ch != ' ')
            .inspect(|c| reads_on_slab(c, what))
            .count()
    };
    assert_eq!(
        letters(switch.cols.clone(), "the switch"),
        4,
        "the `○ off` switch"
    );
    let label = switch.label.clone().expect("the label");
    assert!(
        letters(label, "the label")
            >= format!("alabv{}", aterm_phase::CliHarness::ClaudeCode.version_str()).len(),
        "the label read"
    );
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\n"),
    );
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    let on_switch = switch_of(&app, wid, session).expect("codex-alab's lit switch");
    assert_eq!(on_switch.face, HarnessSwitch::On);
    let on_cells = &pane_cells(&app, wid)[PAD];
    for c in on_cells[on_switch.cols.clone()]
        .iter()
        .filter(|c| c.ch != ' ')
    {
        reads_on_slab(c, "the lit switch");
    }
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

#[test]
fn codex_gets_a_switch_only_as_codex_alab() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let (mut lockups, mut alab_names) = (0, 0);
    let mut cruise_alab = None;
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
        assert!(
            chip_on_pad(&app, wid, session).0.is_empty(),
            "{cols}: vendor codex has no chip"
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
        // The harness's cells: its Autopilot chip, its switch, its label and the close's blank.
        let chip = chip_on_pad(&app, wid, session).0;
        let harness = chip.first().copied().unwrap_or(switch.cols.start)
            ..switch.label.as_ref().map_or(switch.cols.end, |l| l.end) + 1;
        for (c, cell) in after[PAD].iter().enumerate() {
            if !lockup.contains(&c) && !harness.contains(&c) {
                assert_eq!(*cell, before[PAD][c], "{cols}: column {c} untinted");
            }
        }
        for c in &after[PAD][harness.clone()] {
            assert_eq!(c.bg, SLAB, "{cols}: {:?} on the slab", c.ch);
        }
        assert!(
            after[PAD][switch.cols.clone()].iter().all(|c| !c.bold),
            "{cols}: the lit switch's word, never bold"
        );
        if let Some(label) = &switch.label {
            let alab = &after[PAD][label.start..label.start + 4];
            assert!(
                alab.iter().map(|c| c.ch).eq("alab".chars()) && alab.iter().all(|c| c.bold),
                "{cols}: the label's `alab`, bold: {alab:?}"
            );
            cruise_alab = Some(alab[0].fg);
        }
        let line = glass_chars(&app, wid, PAD);
        let lockup_text: String = line.chars().take(lockup.end).collect();
        assert!(
            !lockup_text.contains(" codex "),
            "{cols}: codex-alab's lockup never reads the vendor's name: {line:?}"
        );
        if cols >= 55 {
            assert!(
                line.starts_with("      codex-alab ")
                    && line.contains(&format!("{LIT_SWITCH} alab ")),
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
    app.press_harness_switch(wid, session, HarnessSwitch::On, std::time::Instant::now());
    assert_eq!(
        app.session_autopilot(session).0,
        None,
        "the press writes no Autopilot word"
    );
    let wrote = app
        .messages
        .log()
        .records()
        .rfind(|r| r.key.as_deref() == Some("harness.switch-save.harness.enabled"))
        .map(|r| r.detail.join(" "))
        .expect("the master switch's write was made");
    assert!(
        wrote.contains("harness.enabled = false"),
        "the press turned the whole harness off: {wrote:?}"
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
    let _ = pass(&mut app, wid);
    let switch = switch_of(&app, wid, session).expect("CONTROL: admitted, a switch");
    assert_eq!(switch.face, HarnessSwitch::On, "CONTROL");
    let cells = &pane_cells(&app, wid)[PAD];
    let lit = cells[switch.cols.clone()]
        .iter()
        .find(|c| c.ch == '\u{25cf}')
        .expect("CONTROL: the lit state");
    assert_eq!(lit.bg, SLAB, "CONTROL: the switch's mark keeps the slab");
    let label = switch.label.clone().expect("CONTROL: the label");
    assert_ne!(
        Some(cells[label.start].fg),
        cruise_alab,
        "CONTROL: bare, `alab` is not Cruise's"
    );
    assert!(
        glass_chars(&app, wid, PAD).starts_with("      codex-alab "),
        "CONTROL: admitted, codex-alab: {:?}",
        glass_chars(&app, wid, PAD)
    );
}

#[test]
fn the_pet_holds_codexs_lockup_chip_and_switch_never_the_pad_between() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let (mut gaps, mut chips) = (0, 0);
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
            // The harness's cells: its switch, its label and the close's blank, and its
            // Autopilot chip.
            let switch = switch_of(&app, wid, session)
                .map(|s| s.cols.start..s.label.as_ref().map_or(s.cols.end, |l| l.end) + 1);
            assert_eq!(switch.is_some(), proven, "{cols}: a switch for codex-alab");
            let chip = chip_on_pad(&app, wid, session).0;
            assert!(
                proven || chip.is_empty(),
                "{cols}: a chip for codex-alab alone"
            );
            chips += usize::from(!chip.is_empty());
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
                lockup.contains(&col)
                    || switch.as_ref().is_some_and(|s| s.contains(&col))
                    || chip.contains(&col)
            };
            let wrong: Vec<usize> = (0..cols).filter(|&c| covered(c) != ours(c)).collect();
            assert!(
                wrong.is_empty(),
                "{cols}, proven {proven}: lockup {lockup:?}, harness {switch:?}: columns \
                 held other than the host's marks, or marks not held: {wrong:?}"
            );
            if let Some(s) = &switch {
                gaps += s.start - lockup.end - chip.len();
            }
        }
    }
    assert!(gaps > 100, "the slab's pad lies between the marks: {gaps}");
    assert!(chips >= 2, "the chip held at 100 and 160 columns: {chips}");
}

/// `session`'s Autopilot chip on the pad: its frame columns, and its glyphs as the glass reads them.
fn chip_on_pad(app: &App, wid: WindowId, session: u64) -> (Vec<usize>, String) {
    let ws = &app.windows[&wid];
    let strip = ws.input_scratch.cells.len() - usize::from(ws.rows);
    let painted = ws
        .claude_lights
        .painted_cells(session, crate::claude_lights::Chip::Autopilot);
    assert!(
        painted.iter().all(|&(row, _)| row == strip + PAD),
        "the chip on the pad: {painted:?}"
    );
    let cols: Vec<usize> = painted.into_iter().map(|(_, col)| col).collect();
    let text = cols
        .iter()
        .map(|&c| ws.input_scratch.cells[strip + PAD][c])
        .filter(|c| !c.wide)
        .map(|c| c.ch)
        .collect();
    (cols, text)
}

/// `session`'s lockup on the glass.
fn lockup_of(app: &App, wid: WindowId, session: u64) -> std::ops::Range<usize> {
    app.windows[&wid]
        .claude_lights
        .mark_hits
        .iter()
        .find(|m| m.session == session)
        .map(|m| m.cols.clone())
        .unwrap_or_default()
}

/// The engine's rows and columns.
fn engine_size(app: &App, session: u64) -> (u16, u16) {
    let term = app.pool.get(session).expect("session").term.clone();
    let t = crate::term_lock(&term);
    (t.rows(), t.cols())
}

/// One frame of codex-alab's composer at `cols`, its claim proven: refilled, then passed.
fn codex_alab_frame(app: &mut App, wid: WindowId, session: u64, cols: usize) {
    let _ = app.apply_term_resize(wid, 24, cols as u16);
    bytes(app, session, codex_screen(cols, Some(SLAB)).as_bytes());
    set_proven(app, session, true);
    refill(app, wid);
    let _ = pass(app, wid);
}

/// `session`'s Autopilot word, written as a chip's click writes it.
fn word(app: &mut App, session: u64, word: aterm_agent::supervise::Autopilot) {
    assert!(app.set_session_autopilot(session, Some(word), crate::autopilot::Surface::Chip));
}

/// codex-alab's own task face, as its proven claim tells it (`meta harness_act`).
fn self_task(app: &mut App, session: u64, face: aterm_agent::harness::acts::TaskFace) {
    let sid = {
        let ctx = &app.pool.get(session).expect("session").ctx;
        ctx.timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .queue_self_task(face);
        ctx.self_id.as_str().to_owned()
    };
    app.apply_harness_fact(&sid, crate::HarnessFact::SelfTask(face));
}

/// THE CHIP TAKES THE PAD'S GAP: on a proven codex-alab's slab, at every width from 20 to 160
/// columns, the Autopilot chip lays two cells after the lockup and ends two before the switch,
/// on the slab, wherever the gap has room for it and nowhere else; only the pad row changes, and
/// neither the window's chrome rows nor the engine's size move. Its faces are Claude Code's:
/// Cruise, Land, Off, a timed Cruise, codex-alab's own landing and a person's exchange. Controls:
/// a vendor codex, an off master switch and a pad codex drew on lay no chip.
#[test]
fn codex_alab_lays_its_autopilot_chip_in_the_pads_gap() {
    use aterm_agent::harness::acts::TaskFace;
    use aterm_agent::supervise::Autopilot;
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let (mut laid, mut narrow) = (0, 0);
    for cols in 20..=160usize {
        assert!(app.apply_term_resize(wid, 24, cols as u16));
        bytes(&app, session, codex_screen(cols, Some(SLAB)).as_bytes());
        set_proven(&app, session, true);
        refill(&mut app, wid);
        let before = pane_cells(&app, wid);
        let (chrome, size) = (app.chrome_rows(wid), engine_size(&app, session));
        let _ = pass(&mut app, wid);
        let after = pane_cells(&app, wid);
        let changed = changed_rows(&before, &after);
        assert!(
            changed.iter().all(|&r| r == PAD),
            "{cols}: the pad alone: {changed:?}"
        );
        assert_eq!(app.chrome_rows(wid), chrome, "{cols}: no chrome row");
        assert_eq!(engine_size(&app, session), size, "{cols}: no re-grid");
        let switch = switch_of(&app, wid, session).expect("codex-alab's switch");
        let lockup = lockup_of(&app, wid, session);
        let (chip, text) = chip_on_pad(&app, wid, session);
        let room = if lockup.is_empty() {
            0
        } else {
            switch.cols.start.saturating_sub(lockup.end)
        };
        // Cruise's face, `∞`, is one cell.
        let fits = room >= CHIP_LEAD + 1 + CHIP_TAIL;
        assert_eq!(
            !chip.is_empty(),
            fits,
            "{cols}: a chip wherever the gap holds one: lockup {lockup:?}, switch {:?}: {:?}",
            switch.cols,
            glass_chars(&app, wid, PAD)
        );
        if chip.is_empty() {
            narrow += 1;
            continue;
        }
        assert_eq!(
            chip[0],
            lockup.end + CHIP_LEAD,
            "{cols}: two after the lockup"
        );
        assert!(
            chip[chip.len() - 1] + 1 + CHIP_TAIL <= switch.cols.start,
            "{cols}: two before the switch: {chip:?} {:?}",
            switch.cols
        );
        assert_eq!(text, "\u{221e}", "{cols}: Cruise's face");
        for &c in &chip {
            assert_eq!(after[PAD][c].bg, SLAB, "{cols}: column {c} on the slab");
        }
        for c in lockup.end..switch.cols.start {
            if !chip.contains(&c) {
                assert_eq!(
                    after[PAD][c], before[PAD][c],
                    "{cols}: column {c} the vendor's pad"
                );
            }
        }
        laid += 1;
    }
    assert!(laid > 100 && narrow > 0, "{laid} laid, {narrow} too narrow");

    let face = |app: &mut App| {
        codex_alab_frame(app, wid, session, 100);
        chip_on_pad(app, wid, session).1
    };
    assert_eq!(face(&mut app), "\u{221e}", "Cruise");
    word(&mut app, session, Autopilot::Land);
    assert_eq!(face(&mut app), "\u{2913} land", "Land");
    self_task(&mut app, session, TaskFace::Landed);
    assert_eq!(
        face(&mut app),
        "\u{2713} landed",
        "codex-alab's own landing"
    );
    self_task(&mut app, session, TaskFace::Theirs);
    assert_eq!(face(&mut app), "\u{2913} quiet", "a person's own exchange");
    word(&mut app, session, Autopilot::Off);
    assert_eq!(face(&mut app), "\u{25cb} off", "Off");
    let lands = crate::autopilot_timers::wall_ms() + 2 * 3_600_000;
    assert!(app.set_session_autopilot_timed(
        session,
        Some(Autopilot::Cruise),
        Some(lands),
        crate::autopilot::Surface::Chip
    ));
    let timed = face(&mut app);
    assert!(
        timed.starts_with("\u{221e} \u{2913}") && timed.len() > "\u{221e} \u{2913}".len(),
        "a timed Cruise: {timed:?}"
    );

    set_proven(&app, session, false);
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    assert!(
        switch_of(&app, wid, session).is_none() && chip_on_pad(&app, wid, session).0.is_empty(),
        "CONTROL: a vendor codex lays no chip"
    );
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    codex_alab_frame(&mut app, wid, session, 100);
    assert_eq!(
        switch_of(&app, wid, session).map(|s| s.face),
        Some(HarnessSwitch::Off(SwitchOff::Master)),
        "CONTROL: the master switch off"
    );
    assert!(
        chip_on_pad(&app, wid, session).0.is_empty(),
        "CONTROL: under an off switch, the switch alone"
    );
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\n"),
    );
    codex_alab_frame(&mut app, wid, session, 100);
    assert!(
        !chip_on_pad(&app, wid, session).0.is_empty(),
        "CONTROL: the switch on, the chip back"
    );
    let on = format!("\x1b[48;2;{};{};{}m", SLAB[0], SLAB[1], SLAB[2]);
    bytes(&app, session, format!("\x1b[6;40H{on}x\x1b[0m").as_bytes());
    refill(&mut app, wid);
    let _ = pass(&mut app, wid);
    assert!(
        switch_of(&app, wid, session).is_none() && chip_on_pad(&app, wid, session).0.is_empty(),
        "CONTROL: a pad codex drew on lays neither the switch nor the chip"
    );
}

/// THE CHIP NEVER MOVES THE SWITCH: at every width from 12 to 200 columns, a codex-alab top line
/// with its chip laid and the same line with none laid (`without_top_chips`) have the same switch,
/// label, ⬆️ and face, and the same cells everywhere but the chip's own.
#[test]
fn the_chip_never_moves_codexs_switch_or_label() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let mut chipped = 0;
    for cols in 12..=200usize {
        codex_alab_frame(&mut app, wid, session, cols);
        let with = (switch_of(&app, wid, session), pane_cells(&app, wid));
        let (chip, _) = chip_on_pad(&app, wid, session);
        let _ = without_top_chips(|| pass(&mut app, wid));
        assert!(
            chip_on_pad(&app, wid, session).0.is_empty(),
            "{cols}: CONTROL: the seam lays none"
        );
        let bare = (switch_of(&app, wid, session), pane_cells(&app, wid));
        assert_eq!(
            with.0, bare.0,
            "{cols}: the switch, its label and its arrow stand"
        );
        for (c, (a, b)) in with.1[PAD].iter().zip(&bare.1[PAD]).enumerate() {
            if !chip.contains(&c) {
                assert_eq!(a, b, "{cols}: column {c} is the line's own");
            }
        }
        assert_eq!(
            changed_rows(&with.1, &bare.1),
            if chip.is_empty() { vec![] } else { vec![PAD] },
            "{cols}"
        );
        chipped += usize::from(!chip.is_empty());
    }
    assert!(chipped > 150, "{chipped}");
}

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

const LIT: SwitchPaint =
    SwitchPaint::bare(HarnessSwitch::On, Some(aterm_phase::CliHarness::ClaudeCode));

const OFF: SwitchPaint = SwitchPaint::bare(
    HarnessSwitch::Off(SwitchOff::Master),
    Some(aterm_phase::CliHarness::ClaudeCode),
);

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
        chip: TopChip::NONE,
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

/// A codex pane `cols` wide whose session writes its PTY input to a private observer.
#[cfg(unix)]
fn observed_codex(cols: u16) -> (App, WindowId, u64, std::os::unix::net::UnixStream) {
    let (reader, writer) = std::os::unix::net::UnixStream::pair().expect("a private PTY observer");
    reader
        .set_nonblocking(true)
        .expect("a nonblocking observer");
    let mut app = App::headless_for_test_with_sink(std::sync::Arc::new(
        aterm_session::sink::SinkWriter::new_owned(writer.into()),
    ));
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, 24, cols));
    set_program(&app, session, "codex");
    (app, wid, session, reader)
}

/// What the session typed into its PTY since the last read.
#[cfg(unix)]
fn typed(reader: &mut std::os::unix::net::UnixStream) -> Vec<u8> {
    use std::io::Read;
    let mut got = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => panic!("reading the PTY observer: {e}"),
        }
    }
    got
}

/// The `meta-change field=autopilot` events on `session`'s timeline.
#[cfg(unix)]
fn autopilot_moves(app: &App, session: u64) -> Vec<String> {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .since(None)
        .filter(|e| e.kind == "meta-change" && e.payload.contains("field=autopilot "))
        .map(|e| e.payload.to_string())
        .collect()
}

/// The frame cell of `session`'s chip's first painted cell.
#[cfg(unix)]
fn chip_cell(app: &App, wid: WindowId, session: u64) -> (usize, usize) {
    let ws = &app.windows[&wid];
    *ws.claude_lights
        .painted_cells(session, crate::claude_lights::Chip::Autopilot)
        .first()
        .expect("the chip is painted")
}

/// A CLICK ON CODEX-ALAB'S CHIP CYCLES ITS WORD AND TYPES NOTHING: the left press writes the
/// session's next word (Cruise to Land) as the chip's (`Surface::Chip`), one `meta-change
/// field=autopilot` on its timeline, nothing on the PTY, and the next frame repaints to the new
/// face once; the pointer's hover moves the repaint key, and so does codex-alab's own landing
/// (`bare_top_term`'s fold of `autopilot_term`). A right-click, and a control-click on macOS,
/// open the Autopilot menu anchored on the chip's first cell; a right-click on the switch opens
/// nothing.
#[cfg(unix)]
#[test]
fn a_codex_chip_click_cycles_the_word_and_types_nothing() {
    use aterm_agent::harness::acts::TaskFace;
    use aterm_agent::supervise::Autopilot;
    use aterm_window::event::{ElementState, MouseButton};
    let (mut app, wid, session, mut reader) = observed_codex(100);
    let _host = hosted(&mut app);
    codex_alab_frame(&mut app, wid, session, 100);
    assert!(typed(&mut reader).is_empty(), "a frame types nothing");
    assert_eq!(chip_on_pad(&app, wid, session).1, "\u{221e}");
    let at = chip_cell(&app, wid, session);
    let rest = pass(&mut app, wid);
    assert_eq!(
        pass(&mut app, wid),
        rest,
        "CONTROL: an idle pane's key stands"
    );
    assert_eq!(
        point_at(&mut app, wid, at),
        crate::claude_lights::OverChips::Chip,
        "the pointer on the chip"
    );
    let hovered = pass(&mut app, wid);
    assert_ne!(hovered, rest, "the chip's hover is in the repaint key");
    let moves = autopilot_moves(&app, session);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
    assert!(typed(&mut reader).is_empty(), "a click types nothing");
    assert_eq!(app.session_autopilot(session).0, Some(Autopilot::Land));
    let set_by = app
        .pool
        .get(session)
        .expect("session")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .autopilot_set_by
        .and_then(|by| by.surface);
    assert_eq!(
        set_by,
        Some(crate::autopilot::Surface::Chip),
        "the chip's write"
    );
    let now = autopilot_moves(&app, session);
    assert_eq!(
        now[moves.len()..],
        ["field=autopilot value=land".to_string()],
        "one meta-change, which codex-alab follows"
    );
    let clicked = pass(&mut app, wid);
    assert_ne!(clicked, hovered, "the click repaints");
    assert_eq!(
        chip_on_pad(&app, wid, session).1,
        "\u{2913} land",
        "to the new face"
    );
    assert_eq!(pass(&mut app, wid), clicked, "once");
    let _ = point_at(&mut app, wid, (at.0, 1));
    let away = pass(&mut app, wid);
    self_task(&mut app, session, TaskFace::Landed);
    let landed = pass(&mut app, wid);
    assert_ne!(
        landed, away,
        "codex-alab's own landing repaints an idle pane"
    );
    assert_eq!(chip_on_pad(&app, wid, session).1, "\u{2713} landed");

    let at = chip_cell(&app, wid, session);
    let _ = point_at(&mut app, wid, at);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    assert_eq!(
        app.autopilot_menu.as_ref().map(|m| (m.session, m.anchor)),
        Some((session, at)),
        "a right-click opens the Autopilot menu on the chip"
    );
    assert!(app.close_autopilot_menu());
    if cfg!(target_os = "macos") {
        app.windows.get_mut(&wid).expect("window").mods =
            aterm_window::keyboard::ModifiersState::CONTROL;
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        app.windows.get_mut(&wid).expect("window").mods =
            aterm_window::keyboard::ModifiersState::empty();
        assert_eq!(
            app.autopilot_menu.as_ref().map(|m| (m.session, m.anchor)),
            Some((session, at)),
            "a control-click opens it too"
        );
        assert_eq!(
            app.session_autopilot(session).0,
            Some(Autopilot::Land),
            "and turns nothing"
        );
        assert!(app.close_autopilot_menu());
    }
    let _ = pass(&mut app, wid);
    let switch = switch_of(&app, wid, session).expect("the switch");
    let _ = point_at(&mut app, wid, (switch.frame_row, switch.cols.start + 1));
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Right);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Right);
    assert!(
        app.autopilot_menu.is_none(),
        "a right-click on the switch opens nothing"
    );
    assert_eq!(app.session_autopilot(session).0, Some(Autopilot::Land));
    assert!(typed(&mut reader).is_empty(), "nothing typed");
}

/// `session`'s `autopilot` line in `aterm ctl chrome`.
fn chrome_line(app: &App, session: u64) -> String {
    let sid = format!(
        "autopilot sid={} ",
        app.pool.get(session).expect("session").ctx.self_id.as_str()
    );
    app.drawn_chip_lines()
        .into_iter()
        .find(|l| l.starts_with(&sid))
        .expect("the session's chrome line")
}

/// AN UNCARRIED CODEX-ALAB LAYS ITS SWITCH ALONE: a tab admitted as codex-alab by what its claim
/// left (the claim's copy here; on glass, its ALab image) with no proven claim standing is one
/// nothing carries (`autopilot::carried`): codex-alab's vanilla mode never claims and reads no
/// Autopilot word, and it gives its claim back for Autopilot Off. Under every word its line keeps
/// the lit switch and lays no chip, `aterm ctl chrome` says `why=switch-only`, and the tab's
/// hover names no driver, so nothing on the glass names a mode nothing acts on. CONTROL: the
/// proven claim lays the chip, `○ off` under Off while the claim stands.
#[test]
fn an_uncarried_codex_alab_lays_its_switch_alone() {
    use aterm_agent::supervise::Autopilot;
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let _copy = copy_for(&app, session);
    for (said, face, drawn) in [
        (None, "cruise", "\u{221e}"),
        (Some(Autopilot::Land), "land", "\u{2913} land"),
        (Some(Autopilot::Off), "off", "\u{25cb} off"),
    ] {
        if let Some(said) = said {
            word(&mut app, session, said);
        }
        set_proven(&app, session, false);
        bytes(&app, session, codex_screen(100, Some(SLAB)).as_bytes());
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
        let switch = switch_of(&app, wid, session).expect("admitted: the switch");
        assert_eq!(switch.face, HarnessSwitch::On, "{face}: the switch lit");
        assert!(
            chip_on_pad(&app, wid, session).0.is_empty(),
            "{face}: no chip: {:?}",
            glass_chars(&app, wid, PAD)
        );
        let line = chrome_line(&app, session);
        assert!(
            line.contains(&format!(" face={face} drawn=- why=switch-only ")),
            "{face}: {line}"
        );
        assert_eq!(app.driven_by_aterm(session), None, "{face}: no driver");
        codex_alab_frame(&mut app, wid, session, 100);
        assert_eq!(
            chip_on_pad(&app, wid, session).1,
            drawn,
            "CONTROL: {face}: the proven claim carries it, and lays the chip"
        );
    }
}

/// THE GOAL LEAVES THE CHIP ITS CELLS: with an active goal on codex-alab's pad (`CodexSurvey::goal`,
/// read off codex-alab's session record), at every width from 40 to 200 columns the Autopilot
/// chip, Cruise's one-cell `∞` and Land's worded `⤓ land` alike, lays whole at the columns it lays
/// at with no goal, whether the goal is cut or whole; the goal lays after the chip and its tail
/// wherever both fit, and the switch never moves. CONTROL: with no goal the chip lays at every
/// width the gap holds it.
#[test]
fn a_goal_on_codexs_pad_leaves_the_chip_its_cells() {
    use aterm_agent::supervise::Autopilot;
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let set_goal = |app: &App, goal: Option<&str>| {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        assert!(timeline.set_codex_survey(
            PGID,
            crate::survey::CodexSurvey {
                goal: goal.map(str::to_owned),
                ..crate::survey::CodexSurvey::default()
            }
        ));
    };
    for (said, face) in [
        (Autopilot::Cruise, "\u{221e}"),
        (Autopilot::Land, "\u{2913} land"),
    ] {
        word(&mut app, session, said);
        let (mut chipped, mut both) = (0, 0);
        for cols in 40..=200usize {
            set_goal(&app, None);
            codex_alab_frame(&mut app, wid, session, cols);
            let bare = chip_on_pad(&app, wid, session);
            let switch = switch_of(&app, wid, session).map(|s| s.cols);
            assert!(
                !glass_chars(&app, wid, PAD).contains(GOAL_MARK),
                "{cols}: CONTROL: no goal"
            );
            if bare.0.is_empty() {
                continue;
            }
            assert_eq!(bare.1, face, "{cols}: CONTROL: the face whole");
            chipped += 1;
            for goal in [
                "implement the aos aterm vision across every surface",
                "fix it",
            ] {
                set_goal(&app, Some(goal));
                codex_alab_frame(&mut app, wid, session, cols);
                let chip = chip_on_pad(&app, wid, session);
                let line = glass_chars(&app, wid, PAD);
                assert_eq!(chip, bare, "{cols}: {goal:?}: the chip stands: {line:?}");
                assert_eq!(
                    switch_of(&app, wid, session).map(|s| s.cols),
                    switch,
                    "{cols}: {goal:?}: the switch stands"
                );
                if let (Some(&last), Some(mark)) =
                    (chip.0.last(), line.chars().position(|c| c == GOAL_MARK))
                {
                    assert!(
                        mark > last + CHIP_TAIL,
                        "{cols}: {goal:?}: the goal after the chip and its tail: {line:?}"
                    );
                    both += 1;
                }
            }
        }
        assert!(
            chipped > 120 && both > 200,
            "{face}: {chipped} chipped, {both} beside a goal"
        );
    }
}

/// A CODEX LINE THAT LAYS NO SWITCH NOTES NO CHIP: at the widths too narrow for a carried
/// codex-alab's switch, its line notes no chip row, so `aterm ctl chrome` says its chip was not
/// drawn (`why=not-drawn`, never a `no-room` it was never laid for), and on macOS ctrl+shift+tab,
/// which walks a window only where it drew a chip row or a switch, is codex's to read.
#[test]
fn a_codex_line_that_lays_no_switch_notes_no_chip_row() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let (mut bare, mut switched) = (0, 0);
    for cols in 4..=40usize {
        codex_alab_frame(&mut app, wid, session, cols);
        if switch_of(&app, wid, session).is_some() {
            switched += 1;
            continue;
        }
        let line = chrome_line(&app, session);
        assert!(line.contains(" drawn=- why=not-drawn "), "{cols}: {line}");
        #[cfg(target_os = "macos")]
        {
            use aterm_window::keyboard::{
                Key, KeyCode, KeyLocation, NamedKey, PhysicalKey, SmolStr,
            };
            let tab = aterm_window::event::KeyEvent::synthetic_for_test(
                PhysicalKey::Code(KeyCode::Tab),
                Key::Named(NamedKey::Tab),
                Some(SmolStr::new("\t")),
                KeyLocation::Standard,
                aterm_window::event::ElementState::Pressed,
                false,
            );
            let chord = aterm_window::keyboard::ModifiersState::CONTROL
                | aterm_window::keyboard::ModifiersState::SHIFT;
            assert!(
                !app.on_key_claude_lights(wid, chord, &tab),
                "{cols}: ctrl+shift+tab is codex's"
            );
        }
        bare += 1;
    }
    assert!(
        bare > 0 && switched > 0,
        "{bare} with no switch, {switched} with one"
    );
}

/// A TAB THAT RAN CLAUDE CODE LAYS THE AUTOPILOT CHIP ALONE ON CODEX: a session's lights are kept
/// per session, so what Claude Code left of them (its plan mode's light, a worker's Team chip)
/// is let go on codex-alab's top line, where Autopilot is the one chip.
#[test]
fn a_tab_that_ran_claude_lays_the_autopilot_chip_alone_on_codex() {
    use crate::claude_lights::Chip;
    use aterm_agent::harness::lights::Light;
    let plan = Live {
        under: &["  \u{23F8} plan mode on (shift+tab to cycle)"],
        ..AT_REST
    };
    let (mut app, wid, session) = glass_with(&plan, facts());
    let _host = hosted(&mut app);
    app.teams.insert(
        session,
        crate::team::Team {
            manager: Some(crate::team::Manager {
                local: None,
                label: std::sync::Arc::from("lab-sweep"),
            }),
            ..crate::team::Team::default()
        },
    );
    for _ in 0..2 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    let painted = |app: &App, chip: Chip| {
        !app.windows[&wid]
            .claude_lights
            .painted_cells(session, chip)
            .is_empty()
    };
    assert!(
        painted(&app, Chip::Light(Light::Mode)) && painted(&app, Chip::Team),
        "CONTROL: Claude Code's plan mode lights its chip, and the worker's Team chip"
    );
    set_program(&app, session, "codex");
    codex_alab_frame(&mut app, wid, session, 120);
    assert!(
        !painted(&app, Chip::Light(Light::Mode)) && !painted(&app, Chip::Team),
        "neither on codex-alab's pad"
    );
    assert_eq!(
        chip_on_pad(&app, wid, session).1,
        "\u{221e}",
        "Autopilot alone"
    );
}

/// THE CHIP'S TITLE LAYS BESIDE IT AND NEVER MOVES THE SWITCH: with the pointer on codex-alab's
/// chip, its title lays two cells right of it, ending two before the switch, whose cells stand,
/// at every width the chip lays at; with the pointer on the lockup, whose title may grow its name
/// into the pad (`compose_top`'s room for it leaves the chip's cells), the chip lays two cells
/// after the lockup and its title, at every width it lays at with the pointer away, and the
/// switch stands.
#[test]
fn a_codex_chips_title_lays_beside_it_and_never_moves_the_switch() {
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    let mut titled = 0;
    for cols in 30..=160usize {
        codex_alab_frame(&mut app, wid, session, cols);
        let Some(&at) = app.windows[&wid]
            .claude_lights
            .painted_cells(session, crate::claude_lights::Chip::Autopilot)
            .first()
        else {
            continue;
        };
        let rest = switch_of(&app, wid, session).expect("the switch");
        let _ = point_at(&mut app, wid, at);
        let _ = pass(&mut app, wid);
        let (chip, _) = chip_on_pad(&app, wid, session);
        assert_eq!(chip.first(), Some(&at.1), "{cols}: the chip stands");
        assert_eq!(
            switch_of(&app, wid, session).map(|s| (s.cols, s.label)),
            Some((rest.cols.clone(), rest.label.clone())),
            "{cols}: the switch stands"
        );
        if let Some(title) = app.windows[&wid]
            .claude_lights
            .title_hits
            .iter()
            .find(|t| t.session == session)
        {
            assert_eq!(
                title.cols.start,
                chip[chip.len() - 1] + 1 + 2,
                "{cols}: two cells right of the chip"
            );
            assert!(
                title.cols.end + CHIP_TAIL <= rest.cols.start,
                "{cols}: never into the switch's lead: {:?} {:?}",
                title.cols,
                rest.cols
            );
            let words: String = glass_chars(&app, wid, PAD)
                .chars()
                .skip(title.cols.start)
                .take(title.cols.len())
                .collect();
            assert!(words.starts_with("autopilot"), "{cols}: {words:?}");
            titled += 1;
        }
        let _ = point_at(&mut app, wid, (at.0, 0));
    }
    assert!(titled > 50, "{titled}");

    let strip = {
        let ws = &app.windows[&wid];
        ws.input_scratch.cells.len() - usize::from(ws.rows)
    };
    let mut hovered = 0;
    for cols in 30..=160usize {
        codex_alab_frame(&mut app, wid, session, cols);
        let (away, _) = chip_on_pad(&app, wid, session);
        let rest = switch_of(&app, wid, session).map(|s| s.cols);
        let lockup = lockup_of(&app, wid, session);
        if lockup.is_empty() {
            continue;
        }
        let _ = point_at(&mut app, wid, (strip + PAD, lockup.start + 1));
        assert!(
            app.windows[&wid].claude_lights.mark_hovered(session),
            "{cols}: on the lockup"
        );
        let _ = pass(&mut app, wid);
        let titled = lockup_of(&app, wid, session);
        let (chip, _) = chip_on_pad(&app, wid, session);
        assert_eq!(
            chip.is_empty(),
            away.is_empty(),
            "{cols}: the chip lays wherever it lays with the pointer away: {:?}",
            glass_chars(&app, wid, PAD)
        );
        if let Some(&first) = chip.first() {
            assert_eq!(
                first,
                titled.end + CHIP_LEAD,
                "{cols}: two cells after the lockup and its title: {:?}",
                glass_chars(&app, wid, PAD)
            );
            hovered += 1;
        }
        assert_eq!(
            switch_of(&app, wid, session).map(|s| s.cols),
            rest,
            "{cols}: the switch stands"
        );
        let _ = point_at(&mut app, wid, (strip + PAD + 3, 2));
        let _ = pass(&mut app, wid);
        assert_eq!(
            chip_on_pad(&app, wid, session).0,
            away,
            "{cols}: CONTROL: the pointer gone, the chip back in its place"
        );
    }
    assert!(hovered > 100, "{hovered}");
}

/// A SCREEN READER MEETS ONE AUTOPILOT NODE ON CODEX-ALAB'S PAD: the chip's node, its label the
/// face's spoken words, beside the switch's node.
#[cfg(a11y_tree)]
#[test]
fn a_codex_alab_pad_speaks_one_autopilot_node_beside_its_switch() {
    use crate::a11y_tree::HarnessChip;
    use aterm_agent::supervise::Autopilot;
    let (mut app, wid, session) = pane_of(24, 100, "codex");
    let _host = hosted(&mut app);
    word(&mut app, session, Autopilot::Land);
    codex_alab_frame(&mut app, wid, session, 100);
    let nodes = app.claude_chips_a11y(wid);
    let autopilot: Vec<_> = nodes
        .iter()
        .filter(|n| n.session == session && n.chip == HarnessChip::Autopilot)
        .collect();
    assert_eq!(autopilot.len(), 1, "one Autopilot node: {nodes:#?}");
    let face = crate::claude_lights::ChipFace::of(
        crate::autopilot::AutopilotView::Land,
        &app.chip_glass(session),
    );
    assert_eq!(
        autopilot[0].label,
        face.spoken(&app.chip_glass(session), None),
        "the face's spoken words"
    );
    assert_eq!(autopilot[0].label, "Autopilot: land.");
    assert!(
        nodes
            .iter()
            .any(|n| n.session == session && n.chip == HarnessChip::Switch),
        "and the switch's node: {nodes:#?}"
    );
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    codex_alab_frame(&mut app, wid, session, 100);
    let nodes = app.claude_chips_a11y(wid);
    assert!(
        !nodes.iter().any(|n| n.chip == HarnessChip::Autopilot)
            && nodes.iter().any(|n| n.chip == HarnessChip::Switch),
        "CONTROL: an off switch speaks alone: {nodes:#?}"
    );
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

/// AGY'S LIT SWITCH IS ONE SWITCH (review 13): agy's lit switch, ` ● on `, draws its state
/// as its mark and word. A screen reader meets one `Role::Switch` over those cells, toggled on
/// and named as drawn, and its press does what a click on them does: the whole harness turns
/// off (`harness.enabled = false`, no Autopilot word). NEGATIVE CONTROL: once that reads back,
/// the off face is one switch too, toggled off.
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
    assert_eq!(lit[0].label, format!("{label} on"));
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
            format!("{label} off").as_str()
        )],
        "NEGATIVE CONTROL: the off face is one switch, toggled off"
    );
}

/// AGY'S LABEL WEARS THE WHITE UNDER AUTOPILOT OFF: on the white line (`CardPalette::Off`) the
/// label's `alab` is drawn bold in the moonbow's white like the rule it closes, and the switch's
/// words keep the pane's inks. CONTROL: under Cruise `alab` keeps the arc's end.
#[test]
fn agys_label_wears_the_white_under_autopilot_off() {
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
    for c in &white {
        let spread = c.fg.iter().max().unwrap() - c.fg.iter().min().unwrap();
        assert!(
            c.bold && spread <= 24,
            "{:?} is white and bold: {:?}",
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
        "CONTROL: under Cruise `alab` keeps the arc's end"
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
/// `codex-alab` all paint ` ● on ` when lit and ` ○ off ` when off, ending in the same cell, each
/// followed by its own label (`alab <cli> harness v0.5`), every cell on the line's ground, the
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

    for prog in ["claude", "agy", "muse", "codex"] {
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
            "codex" => {
                set_proven(&app, session, true);
                bytes(&app, session, codex_screen(120, Some(SLAB)).as_bytes());
                // The ALab build its image runs, as the rollout tap publishes it.
                let ctx = app.pool.get(session).expect("session").ctx.clone();
                let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                assert!(timeline.set_codex_build(
                    PGID,
                    Some("0.1.0-dev.13764+codex.0.164.0.g4b3a367a0".into())
                ));
            }
            _ => unreachable!(),
        }
        // The running build each lane gives the label: the session registry's (Claude Code's),
        // the store image's (Muse Code's), the ALab build's (codex-alab's).
        let build = match prog {
            "claude" => Some("2.1.283"),
            "muse" => Some("0.9.3"),
            "codex" => Some("0.1.0"),
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
            _ => aterm_phase::CliHarness::CodexAlab,
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
