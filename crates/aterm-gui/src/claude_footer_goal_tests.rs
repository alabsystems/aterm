// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the agent's active goal on the composer's top line.

use super::*;
use crate::autopilot::HarnessSwitch;
use aterm_effects::harness_card::CardPalette;

const LIT: SwitchPaint =
    SwitchPaint::bare(HarnessSwitch::On, Some(aterm_phase::CliHarness::ClaudeCode));

const VISION: &str = "implement the aos aterm vision";

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [0xd0, 0xd0, 0xd0],
        bg: [0x1e, 0x1e, 0x1e],
        ..RenderCell::default()
    }
}

fn run(len: usize) -> TopRun {
    TopRun {
        col: 4,
        len,
        rule: RenderCell {
            ch: '\u{2500}',
            fg: [0x60, 0x60, 0x60],
            ..blank()
        },
        voice: None,
        tail: None,
        end: 4 + len,
    }
}

fn key(program: aterm_phase::Program, len: usize, palette: Option<CardPalette>) -> TopKey {
    TopKey {
        frame_row: 30,
        term_row: 20,
        line: TopLine::Rule,
        run: run(len),
        program,
        build: Build::Vendor,
        switch: Some(LIT),
        palette,
        blank: blank(),
        mark_fg: [0xd7, 0x77, 0x57],
        face: KeyFace::default(),
        cell: (7, 14),
        clip: aterm_core::render::PxRect::EVERYWHERE,
        mark_hover: false,
        title_gen: 0,
    }
}

fn composed(key: &TopKey, goal: Option<&str>) -> ComposedTop {
    let mut top = ComposedTop::default();
    compose_top(key, &mut top, None, goal, &[]);
    top
}

/// The text the line's cells at frame columns `cols` say.
fn text_at(top: &ComposedTop, cols: std::ops::Range<usize>) -> String {
    top.cells[cols.start - top.col..cols.end - top.col]
        .iter()
        .filter(|c| !c.wide)
        .map(|c| c.ch)
        .collect()
}

#[test]
fn a_goal_is_laid_whole_where_it_fits_and_cut_where_it_does_not() {
    let whole = lay_goal(VISION, 10, 60).expect("room for it");
    assert_eq!(
        whole.cols.start, 12,
        "two of the rule's own after the lockup"
    );
    let words: String = whole
        .glyphs
        .iter()
        .map(|g| match *g {
            GoalGlyph::Pad => ' ',
            GoalGlyph::Mark => GOAL_MARK,
            GoalGlyph::Text(ch) => ch,
            GoalGlyph::Wide => '_',
        })
        .collect();
    assert_eq!(words, format!(" \u{25ce} {VISION} "));
    let cut = lay_goal(VISION, 10, 30).expect("room for a part");
    assert!(
        cut.cols.end + GOAL_RULE <= 30,
        "two of the rule's own before the switch"
    );
    assert_eq!(
        cut.glyphs.len(),
        30 - 10 - 2 * GOAL_RULE,
        "the room taken whole"
    );
    assert_eq!(
        cut.glyphs[cut.glyphs.len() - 2],
        GoalGlyph::Text('\u{2026}')
    );
    assert_eq!(
        lay_goal(VISION, 10, 25),
        None,
        "fewer than eight of its cells say nothing"
    );
    assert!(
        lay_goal("fix it", 10, 24).is_some(),
        "a short goal needs only its own cells"
    );
    assert_eq!(lay_goal("", 10, 60), None);
    let wide = lay_goal("\u{76ee}\u{6a19}\u{0301}x", 0, 40).expect("room");
    assert_eq!(
        wide.glyphs[3..wide.glyphs.len() - 1],
        [
            GoalGlyph::Text('\u{76ee}'),
            GoalGlyph::Wide,
            GoalGlyph::Text('\u{6a19}'),
            GoalGlyph::Wide,
            GoalGlyph::Text('x'),
        ],
        "a wide glyph takes two cells, a combining mark none"
    );
}

#[test]
fn the_goal_sits_between_the_lockup_and_the_switch_and_moves_neither() {
    for program in [
        aterm_phase::Program::Claude,
        aterm_phase::Program::Agy,
        aterm_phase::Program::Muse,
        aterm_phase::Program::Codex,
    ] {
        for palette in [None, Some(CardPalette::Cruise)] {
            let key = key(program, 120, palette);
            let bare = composed(&key, None);
            let top = composed(&key, Some(VISION));
            let at = format!("{program:?} {palette:?}");
            assert_eq!(bare.goal_cols, None, "{at}: no goal, none laid");
            let cols = top.goal_cols.clone().expect("laid");
            assert_eq!(
                text_at(&top, cols.clone()),
                format!(" \u{25ce} {VISION} "),
                "{at}"
            );
            assert_eq!(top.lockup, bare.lockup, "{at}: the lockup stays");
            assert_eq!(top.switch, bare.switch, "{at}: the switch stays");
            assert!(cols.start >= top.lockup.end + GOAL_RULE, "{at}");
            let switch = top.switch.as_ref().expect("switch").cols.start;
            assert!(cols.end + GOAL_RULE <= switch, "{at}");
            assert!(
                !top.glass.clone().any(|k| cols.contains(&(top.col + k))),
                "{at}: the vendor's glass never takes the goal's cells"
            );
            let mark = top.cells[cols.start + 1 - top.col];
            assert_eq!(mark.ch, GOAL_MARK);
            assert_eq!(mark.fg, key.mark_fg, "{at}: the mark's ink");
            let word = top.cells[cols.start + 3 - top.col];
            assert_eq!(word.bg, key.run.rule.bg, "{at}: on the line's own ground");
            assert_ne!(
                word.fg, key.run.rule.fg,
                "{at}: inked to read, not as the rule"
            );
        }
    }
}

#[test]
fn the_hover_title_and_a_narrow_rule_take_the_room_from_the_goal() {
    let hovered = TopKey {
        mark_hover: true,
        ..key(aterm_phase::Program::Claude, 120, None)
    };
    assert_eq!(composed(&hovered, Some(VISION)).goal_cols, None);
    let narrow = key(aterm_phase::Program::Claude, 40, None);
    let top = composed(&narrow, Some(VISION));
    assert_eq!(
        top.goal_cols, None,
        "no room past the lockup and the switch"
    );
    assert_eq!(top.switch, composed(&narrow, None).switch);
}

#[test]
fn a_line_with_no_lockup_still_shows_the_goal_before_its_switch() {
    let key = key(aterm_phase::Program::Generic, 90, None);
    let top = composed(&key, Some(VISION));
    let cols = top.goal_cols.clone().expect("laid");
    assert_eq!(cols.start, key.run.col + GOAL_RULE);
    assert_eq!(top.col, cols.start, "the write starts at the goal");
    assert_eq!(text_at(&top, cols), format!(" \u{25ce} {VISION} "));
}

#[test]
fn a_pane_footer_recomposes_its_top_line_when_the_goal_moves() {
    let mut footer = PaneFooter::default();
    let mut marks = MarkContents::default();
    let mut rings = RingContents::default();
    let key = key(aterm_phase::Program::Claude, 120, None);
    assert_eq!(
        footer
            .compose_top(key, &[], &mut marks, &mut rings)
            .goal_cols,
        None
    );
    footer.note_goal(Some(VISION));
    assert!(
        footer
            .compose_top(key, &[], &mut marks, &mut rings)
            .goal_cols
            .is_some()
    );
    footer.note_goal(Some("ship it"));
    let top = footer.compose_top(key, &[], &mut marks, &mut rings);
    let cols = top.goal_cols.clone().expect("laid");
    assert_eq!(text_at(top, cols), " \u{25ce} ship it ");
    footer.note_goal(None);
    assert_eq!(
        footer
            .compose_top(key, &[], &mut marks, &mut rings)
            .goal_cols,
        None
    );
    assert_eq!(goal_term(None), 0);
    assert_ne!(goal_term(Some("a")), goal_term(Some("b")));
}
