// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Instance-theme contrast floor tests across built-in and custom tab themes.

use super::*;
use crate::autopilot::AutopilotView;
use aterm_effects::cli_marks::{ICON_CONTRAST, NAME_CONTRAST};
use aterm_effects::color_math::contrast_ratio;
use aterm_types::scheme::{builtin, builtin_names};

fn pack(c: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, c[0], c[1], c[2]])
}

fn reads(a: [u8; 3], b: [u8; 3]) -> f32 {
    contrast_ratio(pack(a), pack(b))
}

struct Side {
    name: &'static str,
    accent: [u8; 3],
    blank: RenderCell,
}

fn sides() -> Vec<Side> {
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let sides: Vec<Side> = builtin_names()
        .into_iter()
        .filter_map(|name| {
            let scheme = builtin(name)?;
            Some(Side {
                name,
                accent: crate::chrome_band::band_colors(aterm_render::Theme::from(&scheme)).accent,
                blank: RenderCell {
                    ch: ' ',
                    fg: rgb(scheme.foreground),
                    bg: rgb(scheme.background),
                    ..RenderCell::default()
                },
            })
        })
        .collect();
    assert!(sides.len() >= 17, "{} built-ins", sides.len());
    sides
}

fn side<'a>(sides: &'a [Side], name: &str) -> &'a Side {
    sides
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("{name} is a built-in"))
}

fn rule_greys(blank: RenderCell) -> [RenderCell; 3] {
    [[0x88; 3], [0x99; 3], blank.fg].map(|fg| RenderCell {
        ch: '\u{2500}',
        fg,
        ..blank
    })
}

fn soft_tab() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [0x7a; 3],
        bg: [0xff; 3],
        ..RenderCell::default()
    }
}

fn tabs(sides: &[Side]) -> Vec<(&'static str, RenderCell)> {
    let mut tabs: Vec<_> = sides.iter().map(|s| (s.name, s.blank)).collect();
    tabs.push(("a soft theme of one's own", soft_tab()));
    tabs
}

/// The marks [`every_fact`] draws: the join twice, before the plan and before the week.
const MARKS: [char; 10] = [
    footer::MODEL_MARK,
    footer::PATH_MARK,
    footer::BRANCH_MARK,
    footer::ACCOUNT_MARK,
    footer::JOIN_MARK,
    footer::JOIN_MARK,
    footer::WALL_MARK,
    footer::CONTEXT_MARK,
    footer::USAGE_MARK,
    footer::RESET_MARK,
];

fn every_fact() -> footer::RuleFit {
    use footer::Segment;
    footer::RuleFit {
        segments: vec![
            Segment::plain(footer::ACCOUNT_MARK, "me@example.test"),
            Segment::joined(footer::PLAN_MARK, "max"),
            Segment::joined(footer::WEEKLY_MARK, "40%"),
            Segment::plain(footer::WALL_MARK, format!("5h {} 3pm", footer::RESET_MARK)),
            Segment {
                split: Some("Opus 5.5 ".len()),
                quiet: (false, true),
                ..Segment::plain(footer::MODEL_MARK, "Opus 5.5 xhigh off")
            },
            Segment::plain(footer::PATH_MARK, "~/aterm"),
            Segment {
                quiet: (true, false),
                ..Segment::plain(footer::BRANCH_MARK, "main off")
            },
            Segment::plain(footer::CONTEXT_MARK, "ctx 84% left"),
            Segment::plain(footer::USAGE_MARK, "6m33s \u{2191}20M \u{2193}82k"),
        ],
        title: footer::TitleFit::None,
        chips: false,
    }
}

#[test]
fn every_fact_mark_reads_on_every_ground() {
    let sides = sides();
    let fit = every_fact();
    let room = 200;
    let mut checked = 0usize;
    let mut kept = 0usize;
    let (mut lit_marks, mut lit_kept) = (0usize, 0usize);
    let lit_mark = |kind: FactKind| match kind {
        FactKind::Model => Some(footer::MODEL_MARK),
        FactKind::Path => Some(footer::PATH_MARK),
        FactKind::Branch => Some(footer::BRANCH_MARK),
        FactKind::Context => Some(footer::CONTEXT_MARK),
        FactKind::Usage => Some(footer::USAGE_MARK),
        FactKind::Account => Some(footer::ACCOUNT_MARK),
        FactKind::Effort => None,
    };
    for instance in &sides {
        for tab in &sides {
            let blank = tab.blank;
            for rule in rule_greys(blank) {
                let old = mix(instance.accent, rule.fg, 0.5);
                for lit in std::iter::once(None).chain(
                    [
                        FactKind::Model,
                        FactKind::Effort,
                        FactKind::Path,
                        FactKind::Branch,
                        FactKind::Context,
                        FactKind::Usage,
                        FactKind::Account,
                    ]
                    .map(Some),
                ) {
                    let paint =
                        compose_rule_spans(&fit, None, rule, blank, instance.accent, room, lit);
                    let marks: Vec<&RenderCell> = paint
                        .cells
                        .iter()
                        .filter(|c| MARKS.contains(&c.ch))
                        .collect();
                    assert_eq!(marks.len(), MARKS.len(), "every mark is on the line");
                    for c in marks {
                        let k = reads(c.fg, blank.bg);
                        assert!(
                            k >= ICON_CONTRAST,
                            "{}'s accent on {} over {:?}, {lit:?} lit: {:?} at {k:.2}:1",
                            instance.name,
                            tab.name,
                            rule.fg,
                            c.ch
                        );
                        if lit.is_none() && reads(old, blank.bg) >= ICON_CONTRAST {
                            assert_eq!(
                                c.fg, old,
                                "{}'s accent on {} over {:?}: a mark that reads keeps its quietness",
                                instance.name, tab.name, rule.fg
                            );
                            kept += 1;
                        }
                        if lit.and_then(lit_mark) == Some(c.ch) {
                            assert!(
                                k >= NAME_CONTRAST,
                                "{}'s accent on {} over {:?}, {lit:?} lit: its {:?} at {k:.2}:1",
                                instance.name,
                                tab.name,
                                rule.fg,
                                c.ch
                            );
                            if reads(instance.accent, blank.bg) >= NAME_CONTRAST {
                                assert_eq!(
                                    c.fg, instance.accent,
                                    "{}'s accent on {}: a lit accent that reads is kept",
                                    instance.name, tab.name
                                );
                                lit_kept += 1;
                            }
                            lit_marks += 1;
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 17 * 17 * 3 * 7 * MARKS.len(), "{checked}");
    assert!(kept > 0, "some theme's own mix reads and is kept");
    assert_eq!(
        lit_marks,
        sides.len() * sides.len() * 3 * 6,
        "every lit fact's mark"
    );
    assert!(lit_kept > 0, "some accent reads and is kept lit");
    let dracula = side(&sides, "Dracula").accent;
    let light = side(&sides, "Default Light").blank;
    let old = mix(dracula, [0x99; 3], 0.5);
    assert!(
        reads(old, light.bg) < ICON_CONTRAST,
        "CONTROL: Dracula's mark at {:.2}:1 on Default Light",
        reads(old, light.bg)
    );
    let graphic = visible_on(dracula, light.bg);
    assert!(
        reads(graphic, light.bg) < NAME_CONTRAST,
        "CONTROL: Dracula's accent at the graphics floor, {:.2}:1 on Default Light",
        reads(graphic, light.bg)
    );
}

#[test]
fn every_fact_word_reads_on_every_ground() {
    let sides = sides();
    let tabs = tabs(&sides);
    let fit = every_fact();
    let room = 200;
    let (mut values, mut stubs, mut kept_values, mut kept_stubs) = (0usize, 0usize, 0, 0);
    for instance in &sides {
        for &(tab, blank) in &tabs {
            for rule in rule_greys(blank) {
                let old_value = mix(rule.fg, blank.fg, VALUE_LIFT);
                for lit in std::iter::once(None).chain(
                    [
                        FactKind::Model,
                        FactKind::Effort,
                        FactKind::Path,
                        FactKind::Branch,
                        FactKind::Context,
                        FactKind::Usage,
                        FactKind::Account,
                    ]
                    .map(Some),
                ) {
                    let paint =
                        compose_rule_spans(&fit, None, rule, blank, instance.accent, room, lit);
                    let what = |c: &RenderCell| {
                        format!(
                            "{}'s accent on {tab} over {:?}, {lit:?} lit: {:?}",
                            instance.name, rule.fg, c.ch
                        )
                    };
                    let words: Vec<&RenderCell> = paint
                        .cells
                        .iter()
                        .filter(|c| c.ch != ' ' && !MARKS.contains(&c.ch))
                        .collect();
                    let quietest_value = words
                        .iter()
                        .filter(|c| c.bold)
                        .map(|c| reads(c.fg, blank.bg))
                        .fold(f32::INFINITY, f32::min);
                    assert!(quietest_value.is_finite(), "values are on the line");
                    for c in words {
                        let k = reads(c.fg, blank.bg);
                        if c.bold {
                            assert!(k >= NAME_CONTRAST, "{}: a value at {k:.2}:1", what(c));
                            if lit.is_none() && reads(old_value, blank.bg) >= NAME_CONTRAST {
                                assert_eq!(c.fg, old_value, "{}: a value that reads", what(c));
                                kept_values += 1;
                            }
                            values += 1;
                        } else {
                            assert!(k >= ICON_CONTRAST, "{}: a stub at {k:.2}:1", what(c));
                            if lit.is_none() {
                                assert!(
                                    k < quietest_value,
                                    "{}: a stub at {k:.2}:1, a value at {quietest_value:.2}:1",
                                    what(c)
                                );
                                let grey = reads(rule.fg, blank.bg);
                                if grey >= ICON_CONTRAST && grey < quietest_value {
                                    assert_eq!(c.fg, rule.fg, "{}: a grey that reads", what(c));
                                    kept_stubs += 1;
                                }
                            }
                            stubs += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(values >= sides.len() * tabs.len() * 3 * 7 * 20, "{values}");
    assert!(stubs >= sides.len() * tabs.len() * 3 * 7 * 10, "{stubs}");
    assert!(
        kept_values > 0 && kept_stubs > 0,
        "a theme's own inks that read are kept: {kept_values} values, {kept_stubs} stubs"
    );
    let light = side(&sides, "Default Light").blank;
    let old_value = mix([0x99; 3], light.fg, VALUE_LIFT);
    assert!(
        reads(old_value, light.bg) < NAME_CONTRAST,
        "CONTROL: the value at {:.2}:1 on Default Light",
        reads(old_value, light.bg)
    );
    assert!(
        reads([0x99; 3], light.bg) < ICON_CONTRAST,
        "CONTROL: the stub at {:.2}:1 on Default Light",
        reads([0x99; 3], light.bg)
    );
    let soft = soft_tab();
    assert!(
        reads(soft.fg, soft.bg) < NAME_CONTRAST,
        "CONTROL: the soft theme's ink at {:.2}:1",
        reads(soft.fg, soft.bg)
    );
}

fn ghost_glyphs(
    blank: RenderCell,
    rule: RenderCell,
    accent: [u8; 3],
    lit: bool,
) -> Vec<RenderCell> {
    let run = TopRun {
        col: 2,
        len: 60,
        rule,
        voice: None,
    };
    let mut top = ComposedTop::default();
    compose_top(
        &TopKey {
            frame_row: 30,
            term_row: 20,
            line: TopLine::Rule,
            run,
            program: aterm_phase::Program::Claude,
            build: Build::Vendor,
            switch: None,
            palette: None,
            blank,
            mark_fg: accent,
            face: KeyFace {
                ghost: Some(lit),
                tag: None,
                mark: None,
            },
            cell: (7, 14),
            clip: aterm_core::render::PxRect::EVERYWHERE,
            mark_hover: false,
            title_gen: 0,
        },
        &mut top,
        None,
    );
    let (first, end) = top.ghost.expect("the ghost is drawn");
    top.cells
        .iter()
        .enumerate()
        .filter(|&(k, c)| (first..end).contains(&(top.col + k)) && c.ch != ' ')
        .map(|(_, c)| *c)
        .collect()
}

#[test]
fn the_keys_ghost_reads_on_every_ground() {
    let sides = sides();
    let tabs = tabs(&sides);
    let glyphs = aterm_primer::CLAUDE_TOP_EFFORT_KEY.len() + "off".len();
    let (mut checked, mut kept) = (0usize, 0usize);
    for instance in &sides {
        for &(tab, blank) in &tabs {
            for rule in rule_greys(blank) {
                let what = format!("{}'s accent on {tab} over {:?}", instance.name, rule.fg);
                let rest = ghost_glyphs(blank, rule, instance.accent, false);
                let lit = ghost_glyphs(blank, rule, instance.accent, true);
                assert_eq!((rest.len(), lit.len()), (glyphs, glyphs), "{what}");
                let lit_k = reads(lit[0].fg, blank.bg);
                let old_lit = value_fg(rule.fg, blank.fg);
                for c in &lit {
                    assert!(lit_k >= NAME_CONTRAST, "{what}: lit at {lit_k:.2}:1");
                    assert_eq!(c.fg, lit[0].fg, "{what}: one ink");
                    if reads(old_lit, blank.bg) >= NAME_CONTRAST {
                        assert_eq!(c.fg, old_lit, "{what}: a lit ink that reads is kept");
                    }
                }
                for c in &rest {
                    let k = reads(c.fg, blank.bg);
                    assert!(
                        k >= ICON_CONTRAST && k < lit_k && !c.bold,
                        "{what}: {:?} at rest at {k:.2}:1, lit at {lit_k:.2}:1",
                        c.ch
                    );
                    let grey = reads(rule.fg, blank.bg);
                    if grey >= ICON_CONTRAST && grey < lit_k {
                        assert_eq!(c.fg, rule.fg, "{what}: a grey that reads is kept");
                        kept += 1;
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(
        checked >= sides.len() * tabs.len() * 3 * glyphs,
        "{checked}"
    );
    assert!(kept > 0, "a grey that reads is kept");
    let light = side(&sides, "Default Light").blank;
    assert!(
        reads([0x99; 3], light.bg) < ICON_CONTRAST
            && reads(value_fg([0x99; 3], light.fg), light.bg) < NAME_CONTRAST,
        "CONTROL: the ghost as it was laid on Default Light"
    );
}

#[test]
fn the_cruise_chip_reads_on_every_ground() {
    let sides = sides();
    let mut checked = 0usize;
    for instance in &sides {
        for tab in &sides {
            let block = crate::claude_lights::Block::of_autopilot(
                AutopilotView::Cruise,
                tab.blank,
                instance.accent,
            );
            let glyph = block
                .lights
                .iter()
                .find(|c| c.ch == '\u{221e}')
                .expect("Cruise's `∞`");
            let k = reads(glyph.fg, tab.blank.bg);
            assert!(
                k >= NAME_CONTRAST,
                "{}'s accent on {}: `∞` at {k:.2}:1",
                instance.name,
                tab.name
            );
            if reads(instance.accent, tab.blank.bg) >= NAME_CONTRAST {
                assert_eq!(glyph.fg, instance.accent, "an accent that reads is kept");
            }
            checked += 1;
        }
    }
    assert!(checked >= 17 * 17, "{checked}");
    let dracula = side(&sides, "Dracula").accent;
    let light = side(&sides, "Default Light").blank;
    assert!(
        reads(dracula, light.bg) < 1.5,
        "CONTROL: Dracula's accent at {:.2}:1 on Default Light",
        reads(dracula, light.bg)
    );
}

const AGY_ROWS: [&str; 10] = [
    "\u{25cf} # Overview",
    "  ## Section",
    "  ### Detail",
    "  See [docs](https://example.com) and `code` now.",
    "  - [x] Done",
    "  - [ ] Todo",
    "  - Item",
    "      - Nested",
    "  1. First",
    "  > Quoted words",
];

const MUSE_ROWS: [&str; 10] = [
    "\u{25c6} # Overview",
    "  ## Section",
    "  ### Detail",
    "  See [docs](https://example.com) and `code` now.",
    "  - [x] Done",
    "  - [ ] Todo",
    "  - Item",
    "      - Nested",
    "  1. First",
    "  > Quoted words",
];

const MARKDOWN_MARKS: [char; 6] = [
    '\u{25cf}', '\u{25c6}', '\u{2713}', '\u{2022}', '\u{2610}', '\u{25e6}',
];

const MARKDOWN_STRUCTURE: [char; 2] = ['\u{2500}', '\u{258e}'];

fn transcript_edits(
    program: aterm_phase::Program,
    rows: &[&str],
    blank: RenderCell,
    rule: RenderCell,
    accent: [u8; 3],
) -> Vec<AgyRowEdit> {
    let mut scratch = RenderInput::empty();
    scratch.rows = rows.len();
    scratch.cols = 60;
    scratch.cursor_visible = false;
    scratch.cells = rows
        .iter()
        .map(|r| {
            let mut row: Vec<RenderCell> = r.chars().map(|ch| RenderCell { ch, ..blank }).collect();
            row.resize(scratch.cols, blank);
            row
        })
        .collect();
    let text: Vec<String> = rows.iter().map(|r| (*r).to_owned()).collect();
    let mut edits = Vec::new();
    collect_agy_transcript_edits(
        &scratch,
        program,
        &text,
        PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: scratch.rows,
            cols: scratch.cols,
        },
        Some(scratch.rows),
        blank,
        rule,
        accent,
        &mut edits,
    );
    edits
}

#[test]
fn the_accented_transcript_reads_on_every_ground() {
    let sides = sides();
    let mut checked = 0usize;
    for (program, rows) in [
        (aterm_phase::Program::Agy, AGY_ROWS),
        (aterm_phase::Program::Muse, MUSE_ROWS),
    ] {
        for instance in &sides {
            for tab in &sides {
                let blank = tab.blank;
                for rule in rule_greys(blank) {
                    let edits = transcript_edits(program, &rows, blank, rule, instance.accent);
                    assert_eq!(
                        edits.len(),
                        rows.len(),
                        "{program:?}: every row is rewritten"
                    );
                    for (row, edit) in edits.iter().enumerate() {
                        for c in &edit.cells {
                            if c.ch == ' ' {
                                continue;
                            }
                            let what = format!(
                                "{program:?}: {}'s accent on {} over {:?}: {:?} of {:?}",
                                instance.name, tab.name, rule.fg, c.ch, rows[row]
                            );
                            if MARKDOWN_STRUCTURE.contains(&c.ch) {
                                assert_eq!(c.fg, rule.fg, "{what}: the program's grey");
                            } else if MARKDOWN_MARKS.contains(&c.ch) {
                                let k = reads(c.fg, c.bg);
                                assert!(k >= ICON_CONTRAST, "{what}: a mark at {k:.2}:1");
                                checked += 1;
                            } else if c.fg != blank.fg {
                                let k = reads(c.fg, c.bg);
                                assert!(k >= NAME_CONTRAST, "{what}: a letter at {k:.2}:1");
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(checked >= 2 * sides.len().pow(2) * 3 * 40, "{checked}");
    let dracula = side(&sides, "Dracula").accent;
    let light = side(&sides, "Default Light").blank;
    assert!(
        reads(dracula, light.bg) < ICON_CONTRAST,
        "CONTROL: the lead `●` at {:.2}:1 on Default Light",
        reads(dracula, light.bg)
    );
    let solarized = side(&sides, "Solarized Light").blank;
    let old_quote = mix([0x99; 3], solarized.fg, VALUE_LIFT);
    assert!(
        reads(old_quote, solarized.bg) < NAME_CONTRAST,
        "CONTROL: a quote's words at {:.2}:1 on Solarized Light",
        reads(old_quote, solarized.bg)
    );
}
