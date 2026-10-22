// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FOOTER'S FACTS, CLICKED, END TO END (`crate::claude_facts`): a real
//! engine drawing Claude Code's bottom block, one frame through the capture
//! route's splices, the pointer moved to the pixel of the fact the painter
//! RECORDED, a real press — and the bytes the session's PTY sink received.
//! It drives the lights' gesture rig (`claude_lights::gesture_tests`).

use std::sync::Arc;

use aterm_agent::harness::footer::{FactKind, FactMask};
use aterm_window::event::{ElementState, MouseButton};

#[cfg(target_os = "macos")]
use super::Chip;
use super::gesture_tests::*;
use super::{Fact, FactHit, Target, UnderlineStyle};
use crate::claude_footer::FactSide;
use crate::{App, WindowId};

/// Where the painter put `fact` this frame.
fn fact_hit(app: &App, wid: WindowId, fact: Fact) -> FactHit {
    *app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .find(|h| h.fact == fact)
        .unwrap_or_else(|| panic!("{fact:?} is on the glass"))
}

/// The glyphs on the glass under `hit`'s cells, blanks left out.
fn glyphs(app: &App, wid: WindowId, hit: &FactHit) -> String {
    app.windows[&wid].input_scratch.cells[hit.frame_row][hit.cols.0..hit.cols.1]
        .iter()
        .filter(|c| c.ch != ' ')
        .map(|c| c.ch)
        .collect()
}

/// A press and release of `button` at frame cell `cell`, the pointer moved
/// there first.
fn press_at(app: &mut App, wid: WindowId, cell: (usize, usize), button: MouseButton) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, button);
    app.on_mouse_input(wid, ElementState::Released, button);
}

/// A left click on `fact` where the painter put it.
fn click_fact(app: &mut App, wid: WindowId, fact: Fact) {
    let hit = fact_hit(app, wid, fact);
    press_at(app, wid, (hit.frame_row, hit.cols.0), MouseButton::Left);
}

/// The mask of `kinds`.
fn mask(kinds: &[FactKind]) -> FactMask {
    kinds
        .iter()
        .fold(FactMask::default(), |m, k| m.with(*k, true))
}

/// The facts as the owner's [`publish_facts`] sets them, glyph for glyph.
const SET: [(Fact, &str); 4] = [
    (Fact::Model, "\u{25C6}Opus5.5"),
    (Fact::Effort, "xhigh"),
    (Fact::Path, "\u{2302}~/aterm"),
    (Fact::Branch, "\u{2387}main"),
];

/// Claude Code's bottom block as [`draw_claude`] draws it, at the bottom of
/// the session's terminal however many rows it has, its two rules in a gray
/// of Claude's own (`gray`, a 256-colour index; `None`: the default ink).
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

/// Publish `cwd` beside the session's facts, as the resolver does.
fn publish_side(app: &App, session: u64, cwd: &str) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.set_claude_side(PGID, Some(Arc::new(FactSide::at(Some(cwd.into()), None))));
}

/// THE TARGETS ARE WHERE THE WRITE PUT THE FACTS: each fact's hit covers
/// exactly its mark and its glyphs on the glass — with no chips' block, in
/// the one edit at the rule's column; with one, in the tail edit past the
/// rule's own glyphs (`claude_footer::FactWrite`). NEGATIVE CONTROL: a
/// write the splice skips (a side channel under the facts' cells) records
/// no fact, while the chips' own edit still records its chips.
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
    // With a block: Autopilot and the plan light at the left end.
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
    // A side channel lands under the model's first cell on the frame the
    // splice paints again (its rows and their rule run kept): the facts'
    // edit is skipped. It lands on the vendor's row, as a refill brings it:
    // the footer's write is undone first, since the undo refuses a span
    // holding a mark it did not publish and would leave the footer there.
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
    // CONTROL: the next clean frame records them again.
    frame(&mut app, wid);
    assert_eq!(app.windows[&wid].claude_lights.fact_hits.len(), SET.len());
}

/// A LEFT CLICK HIDES, A CLICK ON `off` SHOWS AGAIN (owner, 2026-10-19:
/// "if it is off, show greyed out text and say off … and likewise for
/// model, effort, working directory, branch"): the branch reads `⎇ main
/// off`, its name and its `off` in Claude's rule gray, not bold, and is a
/// stub that shows the branch again; nothing reaches the PTY and no
/// person's gesture is stamped. CONTROL: a click between two facts, and one on the rule's own
/// glyphs, hide nothing.
#[test]
fn a_click_on_the_branch_greys_it_off_and_a_click_on_off_brings_it_back() {
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_bottom(&app, session, PLAN, Some(244));
    frame(&mut app, wid);
    let human = human_seq(&app, session);
    let row = footer_row(&app, wid);
    let rule_cell = cell_at(&app, wid, (row, 1));
    assert_eq!(rule_cell.ch, '\u{2500}', "CONTROL: Claude's own rule glyph");

    click_fact(&mut app, wid, Fact::Branch);
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Branch]));
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
    assert!(value.bold, "CONTROL: a value is bold");
    assert_ne!(value.fg, rule_cell.fg, "CONTROL: and lifted from the gray");
    // Under the pointer still, the stub is lit: lifted and underlined.
    let lit = o(&app);
    assert_eq!(
        (lit.fg, lit.underline),
        (value.fg, UnderlineStyle::Single),
        "the hovered stub is lit"
    );
    // The pointer off it: Claude's rule gray, not bold.
    let (x, y) = px_of(&app, wid, (row, 2));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let rest = o(&app);
    assert_eq!(rest.fg, rule_cell.fg, "the stub is in Claude's rule gray");
    assert!(!rest.bold, "and not bold");
    assert_eq!(rest.underline, UnderlineStyle::None);
    let name = of(&app, 'm');
    assert_eq!(
        (name.fg, name.bold),
        (rule_cell.fg, false),
        "its name greyed with it"
    );

    click_fact(&mut app, wid, Fact::Branch);
    assert_eq!(app.fact_mask(session), FactMask::default(), "shown again");
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{2387} main"));
    assert!(pty(&mut reader, 0).is_empty(), "nothing reached the PTY");
    assert_eq!(human_seq(&app, session), human, "no person's gesture");

    // CONTROL: between two facts, and the rule's own glyphs.
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

/// The owner's facts ([`publish_facts`]) and the context fact read off
/// Claude Code's indicator ([`plan_with_indicator`]), `◐ ctx 84% left`.
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

/// The plan-mode row with Claude Code's own context indicator against its
/// right edge (`84% until auto-compact`, as 2.1.305 draws it), `cols` wide:
/// the reading the context fact follows, so the frame keeps it.
fn plan_with_indicator(app: &App, session: u64) -> String {
    let term = app.pool.get(session).expect("session").term.clone();
    let cols = usize::from(crate::term_lock(&term).cols());
    let end = "84% until auto-compact";
    let pad = cols - PLAN.chars().count() - end.chars().count();
    format!("{PLAN}{}{end}", " ".repeat(pad))
}

/// THE CONTEXT FACT IS CLICKED AS THE OTHERS ARE (main's `◐ ctx` fact,
/// clickable on the composed rule): a click on `◐ ctx 84% left` hides it
/// behind `◐ ctx 84% left off`, its words and its `off` in Claude's rule gray, not
/// bold, a stub that restores it; the stub's click shows it again. Nothing
/// reaches the PTY.
/// CONTROL: before the click the context's words are lifted from the gray,
/// as a shown fact's are.
#[test]
fn a_click_on_the_context_greys_it_off_and_a_click_on_its_stub_brings_it_back() {
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
    // The pointer off it first: a shown value is lifted from the gray.
    let (x, y) = px_of(&app, wid, (row, 2));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let words = ink(&app, &shown, 'x');
    assert!(
        words.bold && words.fg != rule_cell.fg,
        "CONTROL: the shown context is a value: {words:?}"
    );

    click_fact(&mut app, wid, Fact::Context);
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Context]));
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
            (rule_cell.fg, false),
            "{ch:?}: greyed in Claude's rule gray"
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

/// Each hideable fact hides by its own click and reads greyed with `off`
/// after it — the model's `◆ Opus 5.5 off xhigh`, the effort's `◆ Opus 5.5
/// xhigh off`, both `◆ Opus 5.5 off xhigh off` — and each stub's click shows
/// its own fact again, the other still hidden.
#[test]
fn each_fact_hides_alone_and_each_stub_shows_its_own() {
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let row = footer_row(&app, wid);
    click_fact(&mut app, wid, Fact::Model);
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{25C6} Opus 5.5 off xhigh"));
    click_fact(&mut app, wid, Fact::Effort);
    frame(&mut app, wid);
    let text = row_text(&app, wid, row);
    assert!(text.contains("\u{25C6} Opus 5.5 off xhigh off"), "{text:?}");
    assert_eq!(
        (
            fact_hit(&app, wid, Fact::Model).restores,
            fact_hit(&app, wid, Fact::Effort).restores
        ),
        (mask(&[FactKind::Model]), mask(&[FactKind::Effort]))
    );
    click_fact(&mut app, wid, Fact::Model);
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Effort]));
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{25C6} Opus 5.5 xhigh off"));
    click_fact(&mut app, wid, Fact::Effort);
    assert_eq!(app.fact_mask(session), FactMask::default());
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{25C6} Opus 5.5 xhigh"));
    click_fact(&mut app, wid, Fact::Path);
    frame(&mut app, wid);
    assert!(row_text(&app, wid, row).contains("\u{2302} ~/aterm off"));
    assert!(pty(&mut reader, 0).is_empty());
}

/// THE POINTER: a hand over a fact, and the fact the hover's; off it, over
/// the rule's own glyphs, no hand and no hover.
#[test]
fn the_pointer_is_a_hand_over_a_fact_and_not_off_it() {
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let path = fact_hit(&app, wid, Fact::Path);
    let (x, y) = px_of(&app, wid, (path.frame_row, path.cols.0 + 2));
    app.on_cursor_moved(wid, x, y);
    assert!(app.windows[&wid].hover_pointer, "a hand");
    assert_eq!(
        app.windows[&wid].claude_lights.hover,
        Some((session, Target::Fact(Fact::Path)))
    );
    let (x, y) = px_of(&app, wid, (path.frame_row, 2));
    app.on_cursor_moved(wid, x, y);
    assert!(!app.windows[&wid].hover_pointer, "no hand off it");
    assert_eq!(app.windows[&wid].claude_lights.hover, None);
}

/// A HOVER LIGHTS ITS FACT AND MOVES NONE: the row reads the same, every
/// fact where it was; the hovered fact's glyphs underlined, the others not.
/// With a block, the fact's title stands in the block's title slot.
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
    let branch = fact_hit(&app, wid, Fact::Branch);
    let (x, y) = px_of(&app, wid, (branch.frame_row, branch.cols.0));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let after = (
        row_text(&app, wid, row),
        app.windows[&wid].claude_lights.fact_hits.clone(),
    );
    assert_eq!(after, before, "no fact moved");
    let lit = |fact| {
        let hit = fact_hit(&app, wid, fact);
        app.windows[&wid].input_scratch.cells[row][hit.cols.0..hit.cols.1]
            .iter()
            .filter(|c| c.ch != ' ')
            .all(|c| c.underline == UnderlineStyle::Single)
    };
    assert!(lit(Fact::Branch), "the hovered fact is lit");
    assert!(!lit(Fact::Path) && !lit(Fact::Model), "no other");

    // With a block: the title in its slot, the facts where they were.
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
    assert!(text.contains("click: hide"), "the title: {text:?}");
    // A path's title names the absolute directory.
    let path = fact_hit(&app, wid, Fact::Path);
    let (x, y) = px_of(&app, wid, (path.frame_row, path.cols.0));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let text = row_text(&app, wid, row);
    assert!(
        text.contains("/Users//someone/aterm \u{b7} click: hide"),
        "{text:?}"
    );
}

/// EVERY OTHER BUTTON ON A FACT IS THE FACT'S: with the program reporting
/// the mouse, a middle and a right press on the branch send no report and
/// hide nothing — the right press opens the branch's menu
/// (`crate::fact_menu`), the middle one does nothing. CONTROL: the same
/// middle press on the rule's own glyphs, no menu open, is reported to the
/// program.
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

/// A FACT UNDER A BAND IS NO TARGET: once the paste banner claims the
/// footer's row, the fact the last paint recorded there is under no pixel,
/// and a click saved from before the band hides nothing.
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
    let branch = fact_hit(&app, wid, Fact::Branch);
    let (x, y) = px_of(&app, wid, (branch.frame_row, branch.cols.0));
    assert!(
        app.claude_glass_at(wid, x, y).is_some(),
        "CONTROL: a target"
    );
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree\nfour\nfive".to_owned(),
        Source::Controller,
        PasteFraming::Gesture { bracketed: true },
    ));
    app.splice_paste_banner(wid);
    let branch = fact_hit(&app, wid, Fact::Branch);
    assert!(app.chrome_owns_terminal_row(wid, branch.term_row));
    assert_eq!(app.claude_glass_at(wid, x, y), None);
    assert!(!app.press_visible_claude_fact(wid, session, Fact::Branch, FactMask::default()));
    assert_eq!(app.fact_mask(session), FactMask::default());
}

/// HIDDEN IS THE SESSION'S, IN EVERY VIEW OF IT: two panes showing the one
/// session both read `⎇ main off` after a click in either, and both show the
/// branch again after a click on either's stub.
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
    press_at(
        &mut app,
        wid,
        (both[1].frame_row, both[1].cols.0),
        MouseButton::Left,
    );
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

/// THE KEYBOARD WALKS THE CHIPS, THEN THE FACTS (macOS: elsewhere the
/// chord is `prev_tab`): Autopilot, the plan light, then each fact the
/// glass shows left to right, and round; Escape lets go. NEGATIVE CONTROL:
/// in a narrow pane the rule gives up the branch and the path, and the walk
/// never selects a fact the glass does not show.
#[cfg(target_os = "macos")]
#[test]
fn ctrl_shift_tab_walks_the_chips_then_the_facts_that_fit() {
    use aterm_window::keyboard::{KeyCode, NamedKey};
    let (mut app, wid, session, mut reader) = wide_app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let walk = [
        Target::Chip(Chip::Autopilot),
        Target::Chip(MODE),
        Target::Fact(Fact::Model),
        Target::Fact(Fact::Effort),
        Target::Fact(Fact::Path),
        Target::Fact(Fact::Branch),
        Target::Chip(Chip::Autopilot),
    ];
    for want in walk {
        chord(&mut app, wid);
        assert_eq!(
            app.windows[&wid].claude_lights.selected,
            Some((session, want))
        );
        frame(&mut app, wid);
    }
    tap(&mut app, wid, NamedKey::Escape, KeyCode::Escape, "\u{1b}");
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    // Narrow: what the glass gives up is never selected.
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

/// RETURN ON A SELECTED FACT IS ITS CLICK, AND SPACE TOO: the branch hides,
/// nothing is typed and the selection lets go; the stub selected, Space
/// shows it again. CONTROL: with nothing selected, Return is Claude's.
#[cfg(target_os = "macos")]
#[test]
fn return_on_a_selected_fact_is_its_click() {
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
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Branch]));
    assert_eq!(app.windows[&wid].claude_lights.selected, None);
    frame(&mut app, wid);
    assert!(row_text(&app, wid, footer_row(&app, wid)).contains("\u{2387} main off"));
    select(&mut app, Fact::Branch);
    tap(&mut app, wid, NamedKey::Space, KeyCode::Space, " ");
    assert_eq!(app.fact_mask(session), FactMask::default());
    assert!(pty(&mut reader, 0).is_empty(), "nothing typed");
    assert_eq!(human_seq(&app, session), human);
    tap(&mut app, wid, NamedKey::Enter, KeyCode::Enter, "\r");
    assert_eq!(pty(&mut reader, 1), b"\r", "CONTROL: Return is Claude's");
}

/// THE FACTS ARE BUTTONS NAMED IN WORDS: one node per fact on the glass,
/// at its cells, named as a reader says it and what a press does.
/// NEGATIVE CONTROL: a band over the row leaves no fact node.
#[cfg(a11y_tree)]
#[test]
fn facts_are_buttons_named_in_words() {
    use crate::a11y_tree::HarnessChip;
    let (mut app, wid, session, _reader) = app();
    publish_facts(&app, session);
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
            Some("Activate to hide it.".to_owned())
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
    // Hidden: the stub's node says so.
    click_fact(&mut app, wid, Fact::Branch);
    frame(&mut app, wid);
    let nodes = app.claude_chips_a11y(wid);
    let stub = nodes
        .iter()
        .find(|n| n.chip == HarnessChip::Branch)
        .expect("the stub's node");
    assert_eq!(
        (stub.label.as_str(), stub.description.as_deref()),
        ("Branch: hidden", Some("Activate to show it again."))
    );
}

/// A READER'S PRESS IS THE CLICK: the branch hides and shows again, and a
/// press for a fact the glass no longer shows does nothing.
#[cfg(a11y_tree)]
#[test]
fn a_reader_press_is_the_click() {
    use crate::a11y_tree::HarnessChip;
    let (mut app, wid, session, mut reader) = app();
    publish_facts(&app, session);
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Branch);
    assert_eq!(app.fact_mask(session), mask(&[FactKind::Branch]));
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Branch);
    assert_eq!(app.fact_mask(session), FactMask::default());
    // The usage is not on this glass: its press is nothing.
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Usage);
    assert_eq!(app.fact_mask(session), FactMask::default());
    assert!(pty(&mut reader, 0).is_empty());
}
