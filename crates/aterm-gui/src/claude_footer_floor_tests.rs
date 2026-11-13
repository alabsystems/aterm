// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Contrast floor tests of the glass's tokens (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
//! §4.1) across built-in and custom tab themes, under every palette and none.

use super::*;
use crate::autopilot::AutopilotView;
use aterm_effects::cli_marks::{ICON_CONTRAST, NAME_CONTRAST};
use aterm_effects::color_math::contrast_ratio;
use aterm_effects::glass_inks::VALUE_CONTRAST;
use aterm_effects::harness_card::CardPalette;
use aterm_types::scheme::{builtin, builtin_names};

/// Every palette the glass wears, and none.
const PALETTES: [Option<CardPalette>; 4] = [
    Some(CardPalette::Cruise),
    Some(CardPalette::Land),
    Some(CardPalette::Off),
    None,
];

fn pack(c: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, c[0], c[1], c[2]])
}

fn reads(a: [u8; 3], b: [u8; 3]) -> f32 {
    contrast_ratio(pack(a), pack(b))
}

struct Side {
    name: &'static str,
    /// The system accent the theme's band gives: the agy and Muse transcripts' marks, which the
    /// glass's tokens do not draw.
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

/// The marks [`every_fact`] draws: the account unit's one `⎆`, its figure attached markless.
const MARKS: [char; 7] = [
    footer::MODEL_MARK,
    footer::PATH_MARK,
    footer::BRANCH_MARK,
    footer::ACCOUNT_MARK,
    footer::WALL_MARK,
    footer::CONTEXT_MARK,
    footer::USAGE_MARK,
];

fn every_fact() -> footer::RuleFit {
    use footer::Segment;
    footer::RuleFit {
        segments: vec![
            Segment::plain(footer::ACCOUNT_MARK, "main"),
            Segment::attached(footer::ACCOUNT_MARK, "60% left"),
            Segment::plain(footer::WALL_MARK, "until 3pm"),
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

/// The facts every lit run of [`every_fact_mark_reads_on_every_ground`] and
/// [`every_fact_word_reads_on_every_ground`] reads: none, then each.
fn lit_runs() -> impl Iterator<Item = Option<FactKind>> {
    std::iter::once(None).chain(
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
    )
}

/// EVERY FACT MARK READS ON EVERY GROUND: under every palette and none, on every built-in tab
/// over each rule grey, every fact's mark reads at 3:1 or more, lit at 4.5:1, and at rest wears
/// the glass's marks ink of its own column ([`GlassInks::mark`]). CONTROLS: the mark the system
/// accent once gave (Dracula's, mixed into the light rule grey) read under 3:1 on Default Light,
/// and Dracula's accent floored for a graphic read under 4.5:1 there.
#[test]
fn every_fact_mark_reads_on_every_ground() {
    let sides = sides();
    let fit = every_fact();
    let room = 200;
    let at = rule_at_room(room);
    let mut checked = 0usize;
    let mut lit_marks = 0usize;
    let lit_mark = |kind: FactKind| match kind {
        FactKind::Model => Some(footer::MODEL_MARK),
        FactKind::Path => Some(footer::PATH_MARK),
        FactKind::Branch => Some(footer::BRANCH_MARK),
        FactKind::Context => Some(footer::CONTEXT_MARK),
        FactKind::Usage => Some(footer::USAGE_MARK),
        FactKind::Account => Some(footer::ACCOUNT_MARK),
        FactKind::Effort => None,
    };
    for palette in PALETTES {
        for tab in &sides {
            let blank = tab.blank;
            for rule in rule_greys(blank) {
                let glass = rule_glass(palette, rule, blank);
                for lit in lit_runs() {
                    let paint =
                        compose_rule_spans(&fit, None, rule, blank, (&glass, at), room, lit);
                    let marks: Vec<(usize, &RenderCell)> = paint
                        .cells
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| MARKS.contains(&c.ch))
                        .collect();
                    assert_eq!(marks.len(), MARKS.len(), "every mark is on the line");
                    for (col, c) in marks {
                        let k = reads(c.fg, blank.bg);
                        let what = format!(
                            "{palette:?} on {} over {:?}, {lit:?} lit: {:?} at {k:.2}:1",
                            tab.name, rule.fg, c.ch
                        );
                        assert!(k >= ICON_CONTRAST, "{what}");
                        if lit.is_none() {
                            assert_eq!(
                                c.fg,
                                rgb_of(glass.mark(at.t(col))),
                                "{what}: its column's marks ink"
                            );
                        }
                        if lit.and_then(lit_mark) == Some(c.ch) {
                            assert!(k >= NAME_CONTRAST, "{what}: lit");
                            lit_marks += 1;
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 4 * 17 * 3 * 8 * MARKS.len(), "{checked}");
    assert_eq!(
        lit_marks,
        PALETTES.len() * sides.len() * 3 * 6,
        "every lit fact's mark"
    );
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

/// EVERY FACT WORD READS ON EVERY GROUND: under every palette and none, a value at rest in the
/// glass's value ink at 7:1 (4.5:1 lit) and a hidden fact's stub in its quiet ink at 4.5:1 and
/// quieter than the quietest value, by its ink. Every word is terminal text, Normal script at
/// regular weight (glass design §4.3, S7, 2026-11-07): at full size a stroke covers whole pixels
/// on either ground, so the bold the subscript size gave the values and a light ground's stub is
/// gone, and a dark ground's stub stays thin, as main drew it (the black
/// theme keeps main's stub). NEGATIVE CONTROL: the old fact cell, bold in `Script::Sub`, fails
/// the weight and script pin.
#[test]
fn every_fact_word_reads_on_every_ground() {
    let sides = sides();
    let tabs = tabs(&sides);
    let fit = every_fact();
    let room = 200;
    // The cells of a hidden fact's stub in the set run, one past the facts' lead blank.
    let stub_cells: Vec<usize> = footer::set_facts(&fit.segments)
        .glyphs
        .iter()
        .filter(|g| g.quiet && !g.mark)
        .flat_map(|g| 1 + g.cell..1 + g.cell + g.cols)
        .collect();
    assert!(!stub_cells.is_empty(), "the line hides a fact");
    let (mut values, mut stubs) = (0usize, 0usize);
    let terminal_text =
        |c: &RenderCell| c.script == aterm_core::terminal::Script::Normal && !c.bold;
    assert!(
        !terminal_text(&RenderCell {
            script: aterm_core::terminal::Script::Sub { nudge: 0 },
            bold: true,
            ..RenderCell::default()
        }),
        "NEGATIVE CONTROL: the old fact cell"
    );
    let at = rule_at_room(room);
    for palette in PALETTES {
        for &(tab, blank) in &tabs {
            let dark = crate::claude_card::dark_ground(blank);
            for rule in rule_greys(blank) {
                let glass = rule_glass(palette, rule, blank);
                for lit in lit_runs() {
                    let paint =
                        compose_rule_spans(&fit, None, rule, blank, (&glass, at), room, lit);
                    let what = |c: &RenderCell| {
                        format!(
                            "{palette:?} on {tab} over {:?}, {lit:?} lit: {:?}",
                            rule.fg, c.ch
                        )
                    };
                    // Each word's role is its glyph's: a hidden fact's stub, or a value.
                    let words: Vec<(bool, &RenderCell)> = paint
                        .cells
                        .iter()
                        .enumerate()
                        .skip(paint.facts_at)
                        .filter(|(_, c)| {
                            c.ch != ' ' && c.ch != '\u{2500}' && !MARKS.contains(&c.ch)
                        })
                        .map(|(at, c)| {
                            let set = at.checked_sub(paint.facts_at);
                            (set.is_some_and(|k| stub_cells.contains(&k)), c)
                        })
                        .collect();
                    let quietest_value = words
                        .iter()
                        .filter(|(stub, _)| !stub)
                        .map(|(_, c)| reads(c.fg, blank.bg))
                        .fold(f32::INFINITY, f32::min);
                    assert!(quietest_value.is_finite(), "values are on the line");
                    for (stub, c) in words {
                        let k = reads(c.fg, blank.bg);
                        // Every word is terminal text: Normal script, regular weight.
                        assert!(
                            terminal_text(c),
                            "{}: a {} on a {} ground at {:?}, bold {}",
                            what(c),
                            if stub { "stub" } else { "value" },
                            if dark { "dark" } else { "light" },
                            c.script,
                            c.bold
                        );
                        if !stub {
                            assert!(k >= NAME_CONTRAST, "{}: a value at {k:.2}:1", what(c));
                            if lit.is_none() {
                                assert_eq!(c.fg, rgb_of(glass.value()), "{}: the value", what(c));
                                assert!(
                                    k >= VALUE_CONTRAST - 0.01,
                                    "{}: a value at rest at {k:.2}:1",
                                    what(c)
                                );
                            }
                            values += 1;
                        } else {
                            assert!(k >= NAME_CONTRAST, "{}: a stub at {k:.2}:1", what(c));
                            if lit.is_none() {
                                // A tab whose own ink reads under the floor leaves no room above
                                // it: there the stub reads as loud as a value, never louder.
                                let room = reads(blank.fg, blank.bg) >= NAME_CONTRAST;
                                assert!(
                                    k < quietest_value || (!room && k <= quietest_value),
                                    "{}: a stub at {k:.2}:1, a value at {quietest_value:.2}:1",
                                    what(c)
                                );
                                assert_eq!(c.fg, rgb_of(glass.quiet()), "{}: quiet", what(c));
                            }
                            stubs += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        values >= PALETTES.len() * tabs.len() * 3 * 8 * 20,
        "{values}"
    );
    assert!(
        reads(soft_tab().fg, soft_tab().bg) < NAME_CONTRAST,
        "CONTROL: only a tab whose own ink reads under the floor leaves no room"
    );
    assert!(stubs >= PALETTES.len() * tabs.len() * 3 * 8 * 10, "{stubs}");
    assert!(
        crate::claude_card::dark_ground(side(&sides, "Default").blank)
            && !crate::claude_card::dark_ground(side(&sides, "Default Light").blank),
        "CONTROL: Default's ground is dark and Default Light's light"
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

fn ghost_glyphs(blank: RenderCell, rule: RenderCell, lit: bool) -> Vec<RenderCell> {
    let run = TopRun {
        col: 2,
        len: 60,
        rule,
        voice: None,
        tail: None,
        end: 62,
    };
    let mut top = ComposedTop::default();
    compose_top(
        &TopKey {
            frame_row: 30,
            term_row: 20,
            run,
            program: aterm_phase::Program::Claude,
            build: Build::Vendor,
            switch: None,
            palette: None,
            blank,
            field: PaneField {
                col_off: 0,
                cols: 64,
            },
            face: KeyFace {
                ghost: Some(lit),
                tag: None,
                tag_lit: false,
                mark: None,
            },
            cell: (7, 14),
            clip: aterm_core::render::PxRect::EVERYWHERE,
        },
        &mut top,
        None,
        &[],
    );
    let (first, end) = top.ghost.expect("the ghost is drawn");
    top.cells
        .iter()
        .enumerate()
        .filter(|&(k, c)| (first..end).contains(&(top.col + k)) && c.ch != ' ')
        .map(|(_, c)| *c)
        .collect()
}

/// THE KEY'S GHOST READS ON EVERY GROUND: on every tab over each rule grey, the lit ghost wears
/// the glass's lit ink, one ink, at 4.5:1 or more, and at rest its quiet ink, thin, at 4.5:1
/// and quieter than lit, as a stub beside a value. CONTROL: the ghost as it was laid (Claude's
/// light grey, lifted a third toward the ink) read under the floors on Default Light.
#[test]
fn the_keys_ghost_reads_on_every_ground() {
    let sides = sides();
    let tabs = tabs(&sides);
    let glyphs = aterm_primer::CLAUDE_TOP_EFFORT_KEY.len() + "off".len();
    let mut checked = 0usize;
    for &(tab, blank) in &tabs {
        for rule in rule_greys(blank) {
            let what = format!("{tab} over {:?}", rule.fg);
            let glass = rule_glass(None, rule, blank);
            let rest = ghost_glyphs(blank, rule, false);
            let lit = ghost_glyphs(blank, rule, true);
            assert_eq!((rest.len(), lit.len()), (glyphs, glyphs), "{what}");
            let lit_k = reads(lit[0].fg, blank.bg);
            for c in &lit {
                assert!(lit_k >= NAME_CONTRAST, "{what}: lit at {lit_k:.2}:1");
                assert_eq!(c.fg, rgb_of(glass.lit()), "{what}: the lit ink");
            }
            // As a stub's beside a value ([`every_fact_word_reads_on_every_ground`]).
            let room = reads(blank.fg, blank.bg) >= NAME_CONTRAST;
            for c in &rest {
                let k = reads(c.fg, blank.bg);
                assert!(
                    k >= NAME_CONTRAST && (k < lit_k || (!room && k <= lit_k)) && !c.bold,
                    "{what}: {:?} at rest at {k:.2}:1, lit at {lit_k:.2}:1",
                    c.ch
                );
                assert_eq!(c.fg, rgb_of(glass.quiet()), "{what}: the quiet ink");
                checked += 1;
            }
        }
    }
    assert!(checked >= tabs.len() * 3 * glyphs, "{checked}");
    let light = side(&sides, "Default Light").blank;
    assert!(
        reads([0x99; 3], light.bg) < ICON_CONTRAST
            && reads(value_fg([0x99; 3], light.fg), light.bg) < NAME_CONTRAST,
        "CONTROL: the ghost as it was laid on Default Light"
    );
}

/// THE CRUISE CHIP READS ON EVERY GROUND: on every built-in tab, at the left, the middle and the
/// right of a pane, Cruise's `∞` wears the letter of its column, at 4.5:1 or more, whatever the
/// system accent. CONTROL: Dracula's accent, which the chip once wore, reads under 1.5:1 on
/// Default Light.
#[test]
fn the_cruise_chip_reads_on_every_ground() {
    let sides = sides();
    let mut checked = 0usize;
    for tab in &sides {
        for col in [0, 40, 79] {
            let at = crate::claude_lights::ChipAt {
                col,
                ..crate::claude_lights::ChipAt::cruise(tab.blank)
            };
            let block =
                crate::claude_lights::Block::of_autopilot(AutopilotView::Cruise, tab.blank, at);
            let glyph = block
                .lights
                .iter()
                .find(|c| c.ch == '\u{221e}')
                .expect("Cruise's `∞`");
            let k = reads(glyph.fg, tab.blank.bg);
            assert!(k >= NAME_CONTRAST, "{} at {col}: `∞` at {k:.2}:1", tab.name);
            assert_eq!(
                glyph.fg,
                at.letter_at(0),
                "{} at {col}: its letter",
                tab.name
            );
            checked += 1;
        }
    }
    assert!(checked >= 17 * 3, "{checked}");
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
