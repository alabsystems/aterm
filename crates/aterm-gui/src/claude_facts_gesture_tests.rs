// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER'S FACTS, CLICKED, END TO END (`crate::claude_facts`).

use std::sync::Arc;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_window::event::{ElementState, MouseButton};

#[cfg(target_os = "macos")]
use super::Chip;
use super::gesture_tests::*;
use super::{Fact, FactHit, Target, UnderlineStyle};
use crate::claude_footer::FactSide;
use crate::{App, WindowId};

fn fact_hit(app: &App, wid: WindowId, fact: Fact) -> FactHit {
    *app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .find(|h| h.fact == fact)
        .unwrap_or_else(|| panic!("{fact:?} is on the glass"))
}

fn glyphs(app: &App, wid: WindowId, hit: &FactHit) -> String {
    app.windows[&wid].input_scratch.cells[hit.frame_row][hit.cols.0..hit.cols.1]
        .iter()
        .filter(|c| c.ch != ' ')
        .map(|c| c.ch)
        .collect()
}

fn press_at(app: &mut App, wid: WindowId, cell: (usize, usize), button: MouseButton) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, button);
    app.on_mouse_input(wid, ElementState::Released, button);
}

fn click_fact(app: &mut App, wid: WindowId, fact: Fact) {
    let hit = fact_hit(app, wid, fact);
    press_at(app, wid, (hit.frame_row, hit.cols.0), MouseButton::Left);
}

/// Name the program of `session`'s foreground group, `PGID`: only Claude Code's model and
/// effort a click opens a card for (`crate::claude_facts`' `App::session_picks`).
fn set_program(app: &App, session: u64, program: &str) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some(program.into()));
}

fn mask(kinds: &[FactKind]) -> FactMask {
    kinds
        .iter()
        .fold(FactMask::default(), |m, k| m.with(*k, true))
}

const SET: [(Fact, &str); 4] = [
    (Fact::Model, "\u{25C6}Opus5.5"),
    (Fact::Effort, "xhigh"),
    (Fact::Path, "\u{2302}~/aterm"),
    (Fact::Branch, "\u{2387}main"),
];

fn draw_bottom(app: &App, session: u64, mode: &str, gray: Option<u8>) {
    let term = app.pool.get(session).expect("session").term.clone();
    let (rows, cols) = {
        let t = crate::term_lock(&term);
        (usize::from(t.rows()), usize::from(t.cols()))
    };
    let (on, off) = gray.map_or((String::new(), ""), |g| {
        (format!("\x1b[38;5;{g}m"), "\x1b[0m")
    });
    let rule = format!("{on}{}{off}", "\u{2500}".repeat(cols));
    let (top, caret, bottom, last) = (rows - 3, rows - 2, rows - 1, rows);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[{};1H\u{25CF} done\x1b[{top};1H{rule}\x1b[{caret};1H\u{276F} \
         \x1b[{bottom};1H{rule}\x1b[{last};1H{mode}\x1b[{caret};3H",
        rows - 5
    );
    feed(app, session, screen.as_bytes());
}

fn publish_side(app: &App, session: u64, cwd: &str) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.set_claude_side(PGID, Some(Arc::new(FactSide::at(Some(cwd.into()), None))));
}

#[test]
fn the_spans_land_on_the_written_tail_edit() {
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    assert!(
        app.windows[&wid].claude_lights.hits.is_empty(),
        "CONTROL: no chips' block here"
    );
    for (fact, set) in SET {
        let hit = fact_hit(&app, wid, fact);
        assert_eq!(glyphs(&app, wid, &hit), set, "{fact:?}, no block");
        assert_eq!(hit.restores, FactMask::default());
        assert_eq!(hit.session, session);
    }
    let _host = host(&mut app);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let chips_end = app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .map(|h| h.col + 1)
        .max()
        .expect("CONTROL: the chips are drawn");
    for (fact, set) in SET {
        let hit = fact_hit(&app, wid, fact);
        assert_eq!(glyphs(&app, wid, &hit), set, "{fact:?}, with a block");
        assert!(hit.cols.0 > chips_end, "{fact:?} past the chips");
    }
    let model = fact_hit(&app, wid, Fact::Model);
    let ws = app.windows.get_mut(&wid).expect("window");
    assert!(
        crate::claude_footer::undo(&mut ws.claude_footer_undo, &mut ws.input_scratch),
        "CONTROL: the footer's write comes off"
    );
    ws.input_scratch.combining[model.frame_row].push((model.cols.0, Box::new(['\u{301}'])));
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    app.splice_claude_footer(wid, &plan, route);
    let lights = &app.windows[&wid].claude_lights;
    assert!(
        lights.fact_hits.is_empty(),
        "a skipped write records no fact: {:?}",
        lights.fact_hits
    );
    assert!(!lights.hits.is_empty(), "the chips' own edit took");
    frame(&mut app, wid);
    assert_eq!(app.windows[&wid].claude_lights.fact_hits.len(), SET.len());
}

/// THE BRANCH TAKES NO CLICK, AND ITS STUB'S CLICK SHOWS IT: a click on `⎇ main` hides nothing,
/// opens nothing and reaches no program, and the pointer on it is the arrow over an unlit fact;
/// hidden behind its greyed `off`, it lights under the pointer in the glass's one hover and a
/// click shows it again.
#[test]
fn a_click_on_the_branch_does_nothing_and_a_click_on_its_stub_brings_it_back() {
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_bottom(&app, session, PLAN, Some(244));
    frame(&mut app, wid);
    let human = human_seq(&app, session);
    let row = footer_row(&app, wid);
    let rule_cell = cell_at(&app, wid, (row, 1));
    assert_eq!(rule_cell.ch, '\u{2500}', "CONTROL: Claude's own rule glyph");
    assert!(
        crate::claude_card::dark_ground(rule_cell),
        "CONTROL: the app's own theme lays a dark ground"
    );

    click_fact(&mut app, wid, Fact::Branch);
    assert_eq!(app.fact_mask(session), FactMask::default(), "hides nothing");
    assert!(app.autopilot_menu.is_none(), "opens nothing");
    assert!(
        !app.windows[&wid].hover_pointer,
        "the arrow: nothing to click"
    );
    frame(&mut app, wid);
    let shown = fact_hit(&app, wid, Fact::Branch);
    assert!(
        app.windows[&wid].input_scratch.cells[row][shown.cols.0..shown.cols.1]
            .iter()
            .all(|c| c.underline == UnderlineStyle::None),
        "and no hover paint under the pointer"
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing reached the PTY");
    // Hidden, as a program whose own line prints it starts with it.
    app.set_fact_mask(session, mask(&[FactKind::Branch]));
    frame(&mut app, wid);
    let text = row_text(&app, wid, row);
    assert!(text.contains("\u{2387} main off"), "{text:?}");
    let stub = fact_hit(&app, wid, Fact::Branch);
    assert_eq!(stub.restores, mask(&[FactKind::Branch]), "a stub");
    assert_eq!(glyphs(&app, wid, &stub), "\u{2387}mainoff");
    let of = |app: &App, ch: char| {
        app.windows[&wid].input_scratch.cells[row][stub.cols.0..stub.cols.1]
            .iter()
            .find(|c| c.ch == ch)
            .copied()
            .expect("the stub's glyph")
    };
    let o = |app: &App| of(app, 'o');
    let value =
        app.windows[&wid].input_scratch.cells[row][fact_hit(&app, wid, Fact::Path).cols.1 - 1];
    assert!(!value.bold, "a value is terminal text, regular");
    assert_ne!(value.fg, rule_cell.fg, "CONTROL: and lifted from the gray");
    let (x, y) = px_of(&app, wid, (row, stub.cols.0));
    app.on_cursor_moved(wid, x, y);
    assert!(
        app.windows[&wid].hover_pointer,
        "a hand: the stub's click shows"
    );
    frame(&mut app, wid);
    let lit = o(&app);
    let lit_ink = lit_ink(&app, wid);
    assert_ne!(lit_ink, quiet_ink(&app, wid), "CONTROL: lit is not quiet");
    assert_eq!(
        (lit.fg, lit.underline, lit.underline_color, lit.bold),
        (lit_ink, UnderlineStyle::Single, Some(lit_ink), false),
        "the hovered stub wears the one hover"
    );
    let (x, y) = px_of(&app, wid, (row, 2));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let rest = o(&app);
    let quiet = quiet_ink(&app, wid);
    assert_ne!(quiet, value.fg, "CONTROL: the quiet ink is not the value's");
    assert_eq!(rest.fg, quiet, "the stub is in the glass's quiet ink");
    assert!(!rest.bold, "and regular, as every word on line D");
    assert_eq!(rest.underline, UnderlineStyle::None);
    let name = of(&app, 'm');
    assert_eq!(
        (name.fg, name.bold),
        (quiet, false),
        "its name greyed with it"
    );

    click_fact(&mut app, wid, Fact::Branch);
    assert_eq!(app.fact_mask(session), FactMask::default(), "shown again");
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{2387} main"));
    assert!(pty(&mut reader, 0).is_empty(), "nothing reached the PTY");
    assert_eq!(human_seq(&app, session), human, "no person's gesture");

    let (path, branch) = (
        fact_hit(&app, wid, Fact::Path),
        fact_hit(&app, wid, Fact::Branch),
    );
    assert!(path.cols.1 < branch.cols.0, "a gap between them");
    press_at(&mut app, wid, (row, path.cols.1), MouseButton::Left);
    press_at(&mut app, wid, (row, 2), MouseButton::Left);
    assert_eq!(
        app.fact_mask(session),
        FactMask::default(),
        "nothing hidden"
    );
    assert!(pty(&mut reader, 0).is_empty());
}

fn publish_with_context(app: &App, session: u64) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    assert!(timeline.set_claude_footer(
        PGID,
        None,
        Some(aterm_agent::harness::footer::FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            context: Some("ctx 84% left".into()),
            ..Default::default()
        }),
    ));
}

fn plan_with_indicator(app: &App, session: u64) -> String {
    let term = app.pool.get(session).expect("session").term.clone();
    let cols = usize::from(crate::term_lock(&term).cols());
    let end = "84% until auto-compact";
    let pad = cols - PLAN.chars().count() - end.chars().count();
    format!("{PLAN}{}{end}", " ".repeat(pad))
}

/// THE CONTEXT TAKES NO CLICK, AND ITS STUB'S CLICK SHOWS IT, as the branch.
#[test]
fn a_click_on_the_context_does_nothing_and_a_click_on_its_stub_brings_it_back() {
    let (mut app, wid, session, mut reader) = wide_app();
    publish_with_context(&app, session);
    let mode = plan_with_indicator(&app, session);
    draw_bottom(&app, session, &mode, Some(244));
    frame(&mut app, wid);
    let row = footer_row(&app, wid);
    let rule_cell = cell_at(&app, wid, (row, 1));
    assert!(
        row_text(&app, wid, row).contains("\u{25D0} ctx 84% left"),
        "{:?}",
        row_text(&app, wid, row)
    );
    let shown = fact_hit(&app, wid, Fact::Context);
    assert_eq!(glyphs(&app, wid, &shown), "\u{25D0}ctx84%left");
    let ink = |app: &App, hit: &FactHit, ch: char| {
        app.windows[&wid].input_scratch.cells[row][hit.cols.0..hit.cols.1]
            .iter()
            .find(|c| c.ch == ch)
            .copied()
            .expect("the fact's glyph")
    };
    let (x, y) = px_of(&app, wid, (row, 2));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let words = ink(&app, &shown, 'x');
    assert!(
        !words.bold && words.fg != rule_cell.fg,
        "CONTROL: the shown context is a value, regular and lifted from the gray: {words:?}"
    );

    click_fact(&mut app, wid, Fact::Context);
    assert_eq!(app.fact_mask(session), FactMask::default(), "hides nothing");
    assert!(app.autopilot_menu.is_none(), "opens nothing");
    assert!(
        !app.windows[&wid].hover_pointer,
        "the arrow: nothing to click"
    );
    app.set_fact_mask(session, mask(&[FactKind::Context]));
    let (x, y) = px_of(&app, wid, (row, 2));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let text = row_text(&app, wid, row);
    assert!(text.contains("\u{25D0} ctx 84% left off"), "{text:?}");
    let stub = fact_hit(&app, wid, Fact::Context);
    assert_eq!(stub.restores, mask(&[FactKind::Context]), "a stub");
    assert_eq!(glyphs(&app, wid, &stub), "\u{25D0}ctx84%leftoff");
    for ch in ['x', 'f'] {
        let cell = ink(&app, &stub, ch);
        assert_eq!(
            (cell.fg, cell.bold),
            (quiet_ink(&app, wid), false),
            "{ch:?}: greyed in the glass's quiet ink, regular"
        );
    }

    click_fact(&mut app, wid, Fact::Context);
    assert_eq!(app.fact_mask(session), FactMask::default(), "shown again");
    frame(&mut app, wid);
    let text = row_text(&app, wid, row);
    assert!(
        text.contains("\u{25D0} ctx 84% left") && !text.contains("ctx 84% left off"),
        "{text:?}"
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing reached the PTY");
}

/// A CLICK HIDES NO FACT OF A CLAUDE CODE SESSION, AND EACH STUB SHOWS ITS OWN: the model and
/// the effort open their cards, the folder and the branch take no click, and a click on a
/// hidden fact's stub shows that fact alone.
#[test]
fn a_click_hides_no_fact_and_each_stub_shows_its_own() {
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
    set_program(&app, session, "claude");
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let row = footer_row(&app, wid);
    for fact in [Fact::Model, Fact::Effort] {
        click_fact(&mut app, wid, fact);
        assert_eq!(
            app.autopilot_menu.as_ref().map(|m| m.owner),
            Some(crate::autopilot_menu::MenuOwner::Fact(fact)),
            "{fact:?}: a click opens its card"
        );
        app.close_autopilot_menu();
        assert_eq!(
            app.fact_mask(session),
            FactMask::default(),
            "{fact:?}: and hides nothing"
        );
    }
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{25C6} Opus 5.5 xhigh"));
    for fact in [Fact::Path, Fact::Branch] {
        click_fact(&mut app, wid, fact);
        assert_eq!(
            (app.fact_mask(session), app.autopilot_menu.is_none()),
            (FactMask::default(), true),
            "{fact:?}: a click does nothing"
        );
    }
    app.set_fact_mask(session, mask(&[FactKind::Path, FactKind::Branch]));
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{2302} ~/aterm off"));
    assert_eq!(
        (
            fact_hit(&app, wid, Fact::Path).restores,
            fact_hit(&app, wid, Fact::Branch).restores
        ),
        (mask(&[FactKind::Path]), mask(&[FactKind::Branch]))
    );
    click_fact(&mut app, wid, Fact::Path);
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Branch]));
    click_fact(&mut app, wid, Fact::Branch);
    assert_eq!(app.fact_mask(session), FactMask::default());
}

/// WHERE NOTHING TYPES `/model`, A CLICK HIDES THE MODEL AND ITS EFFORT: aterm types into Claude
/// Code alone, so another program's model and effort keep the click that hides them, their
/// titles say so, their cards offer no choice and no slider, and nothing reaches the program.
/// Control: the same pane named Claude Code opens the model's choices and hides nothing.
#[test]
fn a_model_click_hides_it_where_no_claude_code_takes_the_choice() {
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    assert!(!app.session_picks(session));
    assert_eq!(app.model_picks(session), None);
    assert_eq!(app.effort_stops(session), None);
    assert_eq!(app.fact_title(session, Fact::Model).1, "click: hide");
    click_fact(&mut app, wid, Fact::Model);
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Model]));
    click_fact(&mut app, wid, Fact::Effort);
    assert_eq!(
        app.fact_mask(session),
        mask(&[FactKind::Model, FactKind::Effort])
    );
    assert!(pty(&mut reader, 0).is_empty(), "nothing reached the PTY");
    assert!(app.autopilot_menu.is_none(), "no card opened");
    app.set_fact_mask(session, FactMask::default());
    frame(&mut app, wid);
    let hit = fact_hit(&app, wid, Fact::Model);
    app.open_fact_menu(
        wid,
        session,
        Fact::Model,
        (hit.frame_row, hit.cols.0),
        false,
    );
    let offers: Vec<String> = app
        .autopilot_menu
        .as_ref()
        .expect("the model's card")
        .rows
        .iter()
        .filter_map(|row| match row {
            crate::autopilot_menu::MenuRow::Item { act, .. } => Some(act.key()),
            _ => None,
        })
        .collect();
    assert_eq!(
        offers,
        ["fact-menu/model/hide/model"],
        "its card hides and shows, and offers no model"
    );
    app.close_autopilot_menu();
    set_program(&app, session, "claude");
    frame(&mut app, wid);
    assert!(app.session_picks(session), "control: Claude Code");
    assert_eq!(
        app.fact_title(session, Fact::Model).1,
        "click: choose model"
    );
    click_fact(&mut app, wid, Fact::Model);
    assert_eq!(
        app.fact_mask(session),
        FactMask::default(),
        "nothing hidden"
    );
    let offers: Vec<String> = app
        .autopilot_menu
        .as_ref()
        .expect("control: the click opens the model's card")
        .rows
        .iter()
        .filter_map(|row| match row {
            crate::autopilot_menu::MenuRow::Item { act, .. } => Some(act.key()),
            _ => None,
        })
        .collect();
    assert_eq!(
        offers,
        [
            "fact-menu/model/set-model/fable",
            "fact-menu/model/set-model/opus"
        ]
    );
    assert!(pty(&mut reader, 0).is_empty(), "the click typed nothing");
}

/// THE POINTER IS A HAND ONLY OVER A FACT A CLICK ACTS ON: over the model (its click hides it
/// here, opens its card in Claude Code) the hand; over the folder, whose click does nothing, the
/// arrow, though the pointer still reads its title; off the facts, the arrow and no reading.
#[test]
fn the_pointer_is_a_hand_over_a_fact_a_click_acts_on_and_not_off_it() {
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let model = fact_hit(&app, wid, Fact::Model);
    let (x, y) = px_of(&app, wid, (model.frame_row, model.cols.0 + 2));
    app.on_cursor_moved(wid, x, y);
    assert!(app.windows[&wid].hover_pointer, "a hand");
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Model)))
    );
    let path = fact_hit(&app, wid, Fact::Path);
    let (x, y) = px_of(&app, wid, (path.frame_row, path.cols.0 + 2));
    app.on_cursor_moved(wid, x, y);
    assert!(!app.windows[&wid].hover_pointer, "the arrow: no click here");
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Path))),
        "its title is still read"
    );
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .lit_fact(session, FactMask::default()),
        None,
        "and nothing lights"
    );
    let (x, y) = px_of(&app, wid, (path.frame_row, 2));
    app.on_cursor_moved(wid, x, y);
    assert!(!app.windows[&wid].hover_pointer, "no hand off it");
    assert_eq!(app.windows[&wid].claude_lights.hover, None);
}

#[test]
fn hovering_a_fact_moves_no_fact() {
    let (mut app, wid, session, _reader) = wider_app();
    publish_facts(&app, session);
    publish_side(&app, session, "/Users//someone/aterm");
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let row = footer_row(&app, wid);
    let before = (
        row_text(&app, wid, row),
        app.windows[&wid].claude_lights.fact_hits.clone(),
    );
    let model = fact_hit(&app, wid, Fact::Model);
    let (x, y) = px_of(&app, wid, (model.frame_row, model.cols.0));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let after = (
        row_text(&app, wid, row),
        app.windows[&wid].claude_lights.fact_hits.clone(),
    );
    assert_eq!(after, before, "no fact moved");
    // Lit: every cell of the fact's hit, its blanks too, under one line in the lit ink, each
    // glyph in that ink whatever it rests in, and no weight changed.
    let ink = lit_ink(&app, wid);
    let lit = |app: &App, fact| {
        let hit = fact_hit(app, wid, fact);
        app.windows[&wid].input_scratch.cells[row][hit.cols.0..hit.cols.1]
            .iter()
            .all(|c| {
                (c.underline, c.underline_color) == (UnderlineStyle::Single, Some(ink))
                    && (c.ch == ' ' || c.fg == ink)
                    && !c.bold
            })
    };
    let plain = |app: &App, fact| {
        let hit = fact_hit(app, wid, fact);
        app.windows[&wid].input_scratch.cells[row][hit.cols.0..hit.cols.1]
            .iter()
            .all(|c| c.underline == UnderlineStyle::None)
    };
    assert!(lit(&app, Fact::Model), "the hovered fact is lit");
    assert!(
        plain(&app, Fact::Path) && plain(&app, Fact::Branch),
        "no other"
    );
    // The branch has nothing to click: under the pointer it lights nothing, and neither does
    // any other fact.
    let branch = fact_hit(&app, wid, Fact::Branch);
    let (x, y) = px_of(&app, wid, (branch.frame_row, branch.cols.0));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert!(
        plain(&app, Fact::Branch) && plain(&app, Fact::Model) && plain(&app, Fact::Path),
        "a fact with no click takes no hover paint"
    );
    assert_eq!(
        (
            row_text(&app, wid, row),
            app.windows[&wid].claude_lights.fact_hits.clone()
        ),
        before,
        "and moves nothing"
    );

    let _host = host(&mut app);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let hits = app.windows[&wid].claude_lights.fact_hits.clone();
    let branch = fact_hit(&app, wid, Fact::Branch);
    let (x, y) = px_of(&app, wid, (branch.frame_row, branch.cols.0));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    assert_eq!(app.windows[&wid].claude_lights.fact_hits, hits);
    let text = row_text(&app, wid, row);
    assert!(
        text.contains("\u{2387} main \u{b7} right-click: copy") && !text.contains("click: hide"),
        "the title names no click: {text:?}"
    );
    let path = fact_hit(&app, wid, Fact::Path);
    let (x, y) = px_of(&app, wid, (path.frame_row, path.cols.0));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let text = row_text(&app, wid, row);
    assert!(
        text.contains("/Users//someone/aterm \u{b7} right-click: copy, new tab"),
        "{text:?}"
    );
}

/// THE POINTER ON ANY PART OF THE ACCOUNT UNIT LIGHTS THE WHOLE UNIT: line D's fallback unit,
/// shown while line E is not the quota row (whose own lights `claude_footer::quota::tests`
/// hold). On the verdict as on the name, the unit is one fact on the glass, every glyph before its warned verdict lifts (the
/// verdict keeps its warn), one underline lies under the whole of it, and no other fact changes.
#[test]
fn hovering_any_part_of_the_account_lights_the_whole_unit() {
    let (mut app, wid, session, _reader) = wider_app();
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        assert!(timeline.set_claude_footer(
            PGID,
            None,
            Some(aterm_agent::harness::footer::FooterFacts {
                model: Some("Opus 5.5".into()),
                effort: Some("xhigh".into()),
                path: Some("~/aterm".into()),
                account: Some("main".into()),
                subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                    email: Some("owner@example.com".into()),
                    plan: Some("Max 20x".into()),
                    weekly: Some("weekly 37% left".into()),
                    ..Default::default()
                }),
                quota: Some(aterm_agent::harness::quota_view::QuotaView::sample()),
                ..Default::default()
            }),
        ));
    }
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let row = footer_row(&app, wid);
    let account = fact_hit(&app, wid, Fact::Account);
    assert_eq!(
        glyphs(&app, wid, &account),
        "\u{2386}main37%left\u{b7}\u{21ba}Sat10:00\u{b7}\u{2573}~Thu12Nov04:00",
        "one fact from the mark to the verdict's last word"
    );
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_hits
            .iter()
            .filter(|h| h.fact == Fact::Account)
            .count(),
        1
    );
    // Each glyph of `fact`: its column, its char, its ink, and whether it is underlined.
    let glyph_cells = |app: &App, fact: Fact| -> Vec<(usize, char, [u8; 3], bool)> {
        let hit = fact_hit(app, wid, fact);
        let cells = &app.windows[&wid].input_scratch.cells[row];
        (hit.cols.0..hit.cols.1)
            .filter(|&c| cells[c].ch != ' ')
            .map(|c| {
                let cell = cells[c];
                (
                    c,
                    cell.ch,
                    cell.fg,
                    cell.underline == UnderlineStyle::Single,
                )
            })
            .collect()
    };
    let rest = glyph_cells(&app, Fact::Account);
    let model_rest = glyph_cells(&app, Fact::Model);
    assert!(rest.iter().all(|g| !g.3), "CONTROL: nothing lit at rest");
    let warned = rest
        .iter()
        .position(|g| g.1 == '\u{2573}')
        .expect("the verdict's gap");
    for col in [account.cols.0, account.cols.1 - 1] {
        let (x, y) = px_of(&app, wid, (account.frame_row, col));
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.hover,
            Some((session, Target::Fact(Fact::Account))),
            "{col}"
        );
        let lit = glyph_cells(&app, Fact::Account);
        // The name wears the lit ink at rest; where the hovered letter ink is that same ink
        // (no palette) it stays. Every other glyph before the verdict lifts.
        let lit_ink = rest.iter().find(|g| g.1 == 'm').map(|g| g.2);
        assert!(
            lit.iter()
                .zip(&rest)
                .enumerate()
                .all(|(i, (l, r))| l.0 == r.0
                    && if i < warned {
                        l.2 != r.2 || Some(r.2) == lit_ink
                    } else {
                        l.2 == r.2
                    }),
            "{col}: every glyph before the verdict lifts, the verdict keeps its warn: {lit:?}, \
             at rest {rest:?}"
        );
        assert!(
            lit.iter().zip(&rest).any(|(l, r)| l.2 != r.2),
            "{col}: NEGATIVE CONTROL: the hover changed some ink"
        );
        // The underline lies under the ink: every glyph of the unit, from its mark to the
        // verdict's last word, stands in its own cell, and each is underlined.
        assert!(
            lit.iter().all(|g| g.3),
            "{col}: the whole unit is underlined: {lit:?}"
        );
        assert_eq!(
            glyph_cells(&app, Fact::Model),
            model_rest,
            "{col}: no other fact"
        );
    }
}

/// THE ACCOUNT LIGHTS WHOLE BESIDE A WALL: the wall (`⧗ until 3pm`) and the fallback unit
/// (`⎆ main …`) are two spans of one fact, the account, which the pointer and the keyboard light
/// as one target and a click opens as one. With the pointer on the unit, the unit lifts and both
/// spans are underlined; on the wall, the same. NEGATIVE CONTROL (run once, restored): lighting
/// only the first span of the lit fact (`find` in `compose_rule_spans`) underlines the wall and
/// leaves the hovered unit at rest, and fails.
#[test]
fn hovering_the_account_beside_a_wall_lights_both_its_spans() {
    use aterm_agent::harness::session_usage::{ModelTokens, More, UsageFacts, Wall};
    let (mut app, wid, session, _reader) = wider_app();
    {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(PGID);
        assert!(timeline.set_claude_footer(
            PGID,
            None,
            Some(aterm_agent::harness::footer::FooterFacts {
                model: Some("Opus 5.5".into()),
                effort: Some("xhigh".into()),
                path: Some("~/aterm".into()),
                account: Some("main".into()),
                subscription: Some(aterm_agent::harness::profile::SubscriptionDisplay {
                    plan: Some("Max 20x".into()),
                    weekly: Some("weekly 37% left".into()),
                    ..Default::default()
                }),
                quota: Some(aterm_agent::harness::quota_view::QuotaView::sample()),
                usage: UsageFacts::of(
                    (
                        vec![ModelTokens {
                            label: "opus".into(),
                            input: 48_000_000,
                            output: 310_000,
                        }],
                        More::default(),
                    ),
                    Some(Wall {
                        label: "5h",
                        resets: "3pm".into(),
                    }),
                    None,
                ),
                ..Default::default()
            }),
        ));
    }
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let row = footer_row(&app, wid);
    let mut spans: Vec<FactHit> = app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .filter(|h| h.fact == Fact::Account)
        .copied()
        .collect();
    spans.sort_by_key(|h| h.cols.0);
    let [wall, unit] = spans[..] else {
        panic!("the wall and the unit, two spans of the account: {spans:?}");
    };
    assert_eq!(glyphs(&app, wid, &wall), "\u{29D7}until3pm");
    assert!(
        glyphs(&app, wid, &unit).starts_with("\u{2386}main37%left"),
        "{}",
        glyphs(&app, wid, &unit)
    );
    // Each glyph of `hit`: its char, its ink, and whether it is underlined.
    let read = |app: &App, hit: &FactHit| -> Vec<(char, [u8; 3], bool)> {
        let cells = &app.windows[&wid].input_scratch.cells[row];
        (hit.cols.0..hit.cols.1)
            .filter(|&c| cells[c].ch != ' ')
            .map(|c| {
                (
                    cells[c].ch,
                    cells[c].fg,
                    cells[c].underline == UnderlineStyle::Single,
                )
            })
            .collect()
    };
    let (wall_rest, unit_rest) = (read(&app, &wall), read(&app, &unit));
    assert!(
        wall_rest.iter().chain(&unit_rest).all(|g| !g.2),
        "CONTROL: nothing lit at rest"
    );
    for (hovered, name) in [(unit, "the unit"), (wall, "the wall")] {
        let (x, y) = px_of(&app, wid, (hovered.frame_row, hovered.cols.0));
        app.on_cursor_moved(wid, x, y);
        frame(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.hover,
            Some((session, Target::Fact(Fact::Account))),
            "{name}"
        );
        let (wall_lit, unit_lit) = (read(&app, &wall), read(&app, &unit));
        assert!(
            wall_lit.iter().chain(&unit_lit).all(|g| g.2),
            "{name}: both spans of the account are underlined: {wall_lit:?} {unit_lit:?}"
        );
        assert!(
            unit_lit.iter().zip(&unit_rest).any(|(l, r)| l.1 != r.1),
            "{name}: the unit lifts: {unit_lit:?}"
        );
    }
}

#[test]
fn any_other_button_on_a_fact_reaches_no_program() {
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
    frame(&mut app, wid);
    let branch = fact_hit(&app, wid, Fact::Branch);
    for button in [MouseButton::Middle, MouseButton::Right] {
        press_at(&mut app, wid, (branch.frame_row, branch.cols.0), button);
        assert!(pty(&mut reader, 0).is_empty(), "{button:?}: no report");
        assert_eq!(app.fact_mask(session), FactMask::default(), "{button:?}");
    }
    assert_eq!(
        app.autopilot_menu.as_ref().map(|menu| menu.owner),
        Some(crate::autopilot_menu::MenuOwner::Fact(Fact::Branch)),
        "the right press opened the branch's menu"
    );
    app.close_autopilot_menu();
    press_at(&mut app, wid, (branch.frame_row, 2), MouseButton::Middle);
    assert!(
        !pty(&mut reader, 1).is_empty(),
        "CONTROL: a press on the grid is reported"
    );
}

#[test]
fn a_fact_under_a_band_is_no_target() {
    use crate::input::{PasteFraming, Source};
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
    assert!(app.apply_term_resize(wid, 6, 120));
    let rule = "\u{2500}".repeat(120);
    feed(
        &app,
        session,
        format!(
            "\x1b[2J\x1b[1;1H\u{25CF} done\x1b[3;1H{rule}\x1b[4;1H\u{276F} \
             \x1b[5;1H{rule}\x1b[6;1H{PLAN}\x1b[4;3H"
        )
        .as_bytes(),
    );
    frame(&mut app, wid);
    let model = fact_hit(&app, wid, Fact::Model);
    let (x, y) = px_of(&app, wid, (model.frame_row, model.cols.0));
    app.on_cursor_moved(wid, x, y);
    assert!(
        app.claude_glass_at(wid, x, y).is_some(),
        "CONTROL: a target"
    );
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Model)))
    );
    assert!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Model)
            .is_some()
    );
    assert!(app.windows[&wid].hover_pointer);
    assert!(app.open_fact_menu_at(wid, x, y));
    assert!(app.autopilot_menu.is_some());
    #[cfg(target_os = "macos")]
    {
        chord(&mut app, wid);
        assert!(app.windows[&wid].claude_lights.selected.is_some());
    }
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree\nfour\nfive".to_owned(),
        Source::Controller,
        PasteFraming::Gesture { bracketed: true },
    ));
    app.splice_paste_banner(wid);
    app.refresh_glass_cursor_for_frame(wid);
    let model = fact_hit(&app, wid, Fact::Model);
    assert!(app.chrome_owns_terminal_row(wid, model.term_row));
    assert!(app.autopilot_menu.is_none());
    #[cfg(target_os = "macos")]
    {
        chord(&mut app, wid);
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
    }
    assert_eq!(app.claude_glass_at(wid, x, y), None);
    assert_eq!(app.windows[&wid].claude_lights.hover, None);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Model),
        None
    );
    assert!(!app.windows[&wid].hover_pointer);
    assert!(!app.press_visible_claude_fact(wid, session, Fact::Model, FactMask::default()));
    assert_eq!(app.fact_mask(session), FactMask::default());

    app.paste_banner = None;
    frame(&mut app, wid);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Model)))
    );
    assert!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Model)
            .is_some()
    );
    assert!(app.windows[&wid].hover_pointer);
    assert!(app.open_fact_menu_at(wid, x, y));
    assert!(app.autopilot_menu.is_some());
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree\nfour\nfive".to_owned(),
        Source::Controller,
        PasteFraming::Gesture { bracketed: true },
    ));
    frame(&mut app, wid);
    app.splice_autopilot_menu(wid);
    assert!(app.autopilot_menu.is_none());
    app.paste_banner = None;
    frame(&mut app, wid);
    app.refresh_glass_cursor_for_frame(wid);

    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    #[cfg(target_os = "macos")]
    {
        chord(&mut app, wid);
        assert!(app.windows[&wid].claude_lights.selected.is_some());
    }
    app.windows
        .get_mut(&wid)
        .expect("window")
        .chrome_rows
        .push(0..6);
    #[cfg(target_os = "macos")]
    {
        chord(&mut app, wid);
        assert_eq!(app.windows[&wid].claude_lights.selected, None);
    }
    app.splice_claude_footer(wid, &plan, route);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(app.windows[&wid].claude_lights.hover, None);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Model),
        None
    );
    assert!(!app.windows[&wid].hover_pointer);

    frame(&mut app, wid);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Model)))
    );
    assert!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Model)
            .is_some()
    );
    assert!(app.windows[&wid].hover_pointer);
}

#[test]
fn the_hidden_state_is_the_sessions_in_every_view_of_it() {
    let (mut app, wid, session, _reader) = wider_app();
    publish_facts(&app, session);
    let mirror = app
        .view_store
        .insert_terminal(session)
        .expect("a second view of the session");
    app.pool.attach(session);
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        assert!(
            ws.tab_set
                .active_mut()
                .expect("active tab")
                .split_focused(crate::tab_model::SplitAxis::Horizontal, mirror)
        );
        let active = ws.tabs.active;
        assert!(ws.layouts[active].split_focused(crate::pane::SplitDir::Vertical, session));
    }
    app.sync_window(wid);
    draw_bottom(&app, session, PLAN, None);
    frame(&mut app, wid);
    let branches = |app: &App| -> Vec<FactHit> {
        app.windows[&wid]
            .claude_lights
            .fact_hits
            .iter()
            .filter(|h| h.fact == Fact::Branch)
            .copied()
            .collect()
    };
    let both = branches(&app);
    assert_eq!(both.len(), 2, "CONTROL: the branch in both views: {both:?}");
    let off = |app: &App| {
        branches(app)
            .iter()
            .map(|h| glyphs(app, wid, h))
            .collect::<Vec<_>>()
    };
    app.set_fact_mask(session, mask(&[FactKind::Branch]));
    frame(&mut app, wid);
    assert_eq!(
        off(&app),
        ["\u{2387}mainoff", "\u{2387}mainoff"],
        "hidden in both"
    );
    let stubs = branches(&app);
    press_at(
        &mut app,
        wid,
        (stubs[0].frame_row, stubs[0].cols.0),
        MouseButton::Left,
    );
    frame(&mut app, wid);
    assert_eq!(off(&app), ["\u{2387}main", "\u{2387}main"], "shown in both");
}

/// THE KEYBOARD WALKS THE CHIPS.
#[cfg(target_os = "macos")]
#[test]
fn ctrl_shift_tab_walks_the_chips_then_the_facts_that_fit() {
    use aterm_window::keyboard::{KeyCode, NamedKey};
    let (mut app, wid, session, mut reader) = wide_app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let switch = app.windows[&wid]
        .claude_lights
        .shows(session, Target::Switch);
    let walk: Vec<Target> = [
        Target::Chip(Chip::Autopilot),
        Target::Chip(MODE),
        Target::Fact(Fact::Model),
        Target::Fact(Fact::Effort),
        Target::Fact(Fact::Path),
        Target::Fact(Fact::Branch),
    ]
    .into_iter()
    .chain(switch.then_some(Target::Switch))
    .chain([Target::Chip(Chip::Autopilot)])
    .collect();
    for want in walk {
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, want))
        );
        for (fact, _) in SET {
            assert_eq!(
                app.windows[&wid]
                    .claude_lights
                    .fact_reading(session, fact)
                    .is_some(),
                want == Target::Fact(fact),
                "{want:?}: reading of {fact:?}"
            );
        }
        frame(&mut app, wid);
    }
    tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    assert!(app.apply_term_resize(wid, 24, 44));
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let shown = app.windows[&wid].claude_lights.facts_on_glass(session);
    assert!(
        !shown.contains(&Fact::Branch),
        "CONTROL: the narrow rule gave the branch up: {shown:?}"
    );
    for _ in 0..12 {
        chord(&mut app, wid);
        let (_, at) = app.windows[&wid]
            .claude_lights
            .selected
            .expect("a selection");
        if let Target::Fact(fact) = at {
            assert!(shown.contains(&fact), "{fact:?} is on the glass");
        }
        frame(&mut app, wid);
    }
    assert!(pty(&mut reader, 0).is_empty(), "the walk types nothing");
}

/// RETURN ON A SELECTED FACT IS ITS CLICK.
#[cfg(target_os = "macos")]
#[test]
fn return_on_a_selected_fact_activates_it() {
    use aterm_window::keyboard::{KeyCode, NamedKey};
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let human = human_seq(&app, session);
    let select = |app: &mut App, fact: Fact| {
        for _ in 0..8 {
            chord(app, wid);
            frame(app, wid);
            if app.windows[&wid].claude_lights.selected == Some((session, Target::Fact(fact))) {
                return;
            }
        }
        panic!("{fact:?} never selected");
    };
    select(&mut app, Fact::Branch);
    assert!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Branch)
            .is_some()
    );
    tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Branch),
        None
    );
    // The keyboard's selection lights the branch, which the pointer does not: it is how the
    // keyboard says where it is.
    select(&mut app, Fact::Branch);
    let hit = fact_hit(&app, wid, Fact::Branch);
    assert!(
        app.windows[&wid].input_scratch.cells[hit.frame_row][hit.cols.0..hit.cols.1]
            .iter()
            .all(|c| c.underline == UnderlineStyle::Single),
        "the selected branch wears the one hover"
    );
    // Return on it opens its card, the one thing it has: its click does nothing.
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(app.fact_mask(session), FactMask::default(), "hides nothing");
    assert_eq!(
        app.autopilot_menu.as_ref().map(|m| m.owner),
        Some(crate::autopilot_menu::MenuOwner::Fact(Fact::Branch)),
        "Return on the branch opens its card"
    );
    app.close_autopilot_menu();
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .fact_reading(session, Fact::Branch),
        None
    );
    // Return on a fact a click acts on is that click: here, no Claude Code, the model's hides it.
    frame(&mut app, wid);
    select(&mut app, Fact::Model);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Model]));
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    frame(&mut app, wid);
    select(&mut app, Fact::Model);
    tap(&mut app, wid, NamedKey::Space, KeyCode::Space, " ");
    assert_eq!(app.fact_mask(session), FactMask::default());
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    assert_eq!(human_seq(&app, session), human);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"\r", "CONTROL: Return is Claude's");
}

/// THE FACTS ARE BUTTONS NAMED IN WORDS.
#[cfg(a11y_tree)]
#[test]
fn facts_are_buttons_named_in_words() {
    use crate::a11y_tree::HarnessChip;
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
    set_program(&app, session, "claude");
    publish_side(&app, session, "/Users//someone/aterm");
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let nodes = app.claude_chips_a11y(wid);
    let named = |chip| {
        nodes
            .iter()
            .find(|n| n.chip == chip)
            .map(|n| (n.label.clone(), n.description.clone()))
            .unwrap_or_else(|| panic!("{chip:?} has a node: {nodes:?}"))
    };
    assert_eq!(
        named(HarnessChip::Model),
        (
            "Model: Opus 5.5".to_owned(),
            Some("Activate to choose the model.".to_owned())
        )
    );
    assert_eq!(named(HarnessChip::Effort).0, "Effort: xhigh");
    assert_eq!(
        named(HarnessChip::Path).0,
        "Working directory: /Users//someone/aterm"
    );
    assert_eq!(named(HarnessChip::Branch).0, "Branch: main");
    let strip = usize::from(app.chrome_rows(wid));
    let branch = fact_hit(&app, wid, Fact::Branch);
    let node = nodes
        .iter()
        .find(|n| n.chip == HarnessChip::Branch)
        .expect("the branch's node");
    assert_eq!(
        (node.row, node.col, node.width, node.session),
        (
            branch.frame_row - strip,
            branch.cols.0,
            branch.cols.1 - branch.cols.0,
            session
        )
    );
    assert_eq!(
        named(HarnessChip::Branch).1.as_deref(),
        Some("Activate for its menu: copy it."),
        "a press on the branch opens its card: it hides nothing"
    );
    app.set_fact_mask(session, mask(&[FactKind::Branch]));
    frame(&mut app, wid);
    let nodes = app.claude_chips_a11y(wid);
    let stub = nodes
        .iter()
        .find(|n| n.chip == HarnessChip::Branch)
        .expect("the stub's node");
    let hit = fact_hit(&app, wid, Fact::Branch);
    let drawn = crate::claude_footer::read_glass(
        &app.windows[&wid].input_scratch.cells[hit.frame_row][hit.cols.0..hit.cols.1],
    );
    let words: Vec<&str> = drawn
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    assert_eq!(
        words,
        ["main", "off"],
        "CONTROL: the greyed stub: {drawn:?}"
    );
    assert_eq!(
        (stub.label.as_str(), stub.description.as_deref()),
        ("Branch: main off", Some("Activate to show it again.")),
        "the stub is named in the words the glass draws"
    );
}

/// A READER'S PRESS IS THE CLICK.
#[cfg(a11y_tree)]
#[test]
fn a_reader_press_is_the_click() {
    use crate::a11y_tree::HarnessChip;
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    // The branch has nothing to click: a reader's press opens its card, which a reader reaches
    // no other way.
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Branch);
    assert_eq!(app.fact_mask(session), FactMask::default(), "hides nothing");
    assert_eq!(
        app.autopilot_menu.as_ref().map(|m| m.owner),
        Some(crate::autopilot_menu::MenuOwner::Fact(Fact::Branch))
    );
    app.close_autopilot_menu();
    // A press on a stub shows its fact, and on a fact a click acts on is that click.
    app.set_fact_mask(session, mask(&[FactKind::Branch]));
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Branch);
    assert_eq!(app.fact_mask(session), FactMask::default());
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Model);
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Model]));
    app.set_fact_mask(session, FactMask::default());
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Usage);
    assert_eq!(app.fact_mask(session), FactMask::default());
    assert!(pty(&mut reader, 0).is_empty());
}

#[test]
fn open_stats_lens_shields_claude_glass_hover_and_restores_on_close() {
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);

    let model = fact_hit(&app, wid, Fact::Model);
    let (bx, by) = px_of(&app, wid, (model.frame_row, model.cols.0));
    app.on_cursor_moved(wid, bx, by);
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Model))),
        "hovering uncovered Model fact lights its hover"
    );

    // Open a stats lens card covering the whole window: it must evict the hovered fact
    // and shield `claude_glass_at` and `resolve_hover_cursor` while open.
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.stats_lens.open = Some(crate::stats_lens::Lens {
            focus: crate::stats_paint::Instrument::Cpu,
            bar: crate::stats_paint::Bar::Global,
            how: crate::stats_lens::Opened::Pinned,
        });
        ws.stats_lens.rect = Some((0, 0, 2_000, 2_000));
    }
    app.note_band_covered(wid);
    app.evict_stale_glass_hover(wid);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(
        app.windows[&wid].claude_lights.hover, None,
        "opening stats lens over hovered fact evicts its hover"
    );
    assert_eq!(
        app.claude_glass_at(wid, bx, by),
        None,
        "open stats lens shields claude_glass_at underneath"
    );
    assert!(
        !app.windows[&wid].hover_pointer,
        "open stats lens clears hand cursor from covered Claude glass target"
    );

    // Shrinking or moving the open stats lens card so it no longer covers `(bx, by)` while
    // remaining open immediately restores the hover on the next frame composition.
    app.windows.get_mut(&wid).unwrap().stats_lens.rect = Some((1_500, 0, 200, 200));
    frame(&mut app, wid);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Model))),
        "shrinking stats lens away from pointer restores hover even while lens remains open"
    );
    assert!(
        app.windows[&wid].hover_pointer,
        "shrinking stats lens away from pointer restores hand cursor"
    );

    // Covering `(bx, by)` again and then closing the stats lens also restores hover on close.
    app.windows.get_mut(&wid).unwrap().stats_lens.rect = Some((0, 0, 2_000, 2_000));
    app.note_band_covered(wid);
    app.evict_stale_glass_hover(wid);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(app.windows[&wid].claude_lights.hover, None);
    {
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.stats_lens.open = None;
        ws.stats_lens.rect = None;
    }
    frame(&mut app, wid);
    app.refresh_glass_cursor_for_frame(wid);
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Model))),
        "closing stats lens restores hover on uncovered Model fact without pointer motion"
    );
    assert!(
        app.windows[&wid].hover_pointer,
        "closing stats lens restores hand cursor on uncovered Model fact"
    );
}
