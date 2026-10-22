// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EVERY INK DERIVED FROM THE INSTANCE THEME READS ON THE PANE'S GROUND
//! (design §8): the footer's marks are the instance theme's accent
//! (`chrome_band::band_colors(app.theme)`: the window's chrome follows the
//! instance), and a tab may wear a theme of its own (`theme session set`,
//! Choose Tab Theme, `spawn theme=`, a split), whose paper the accent was
//! never chosen for. On every built-in instance theme over every built-in
//! tab theme, each such ink holds its floor on the ground it is laid on —
//! a graphic 3:1 (WCAG 1.4.11), a letter 4.5:1 (WCAG 1.4.3), its hue kept —
//! and a quiet ink that already reads keeps its quietness: it sits at the
//! floor, never under it, and where it clears the floor it is the theme's
//! own mix, unchanged.

use super::*;
use crate::autopilot::AutopilotView;
use aterm_effects::cli_marks::{ICON_CONTRAST, NAME_CONTRAST};
use aterm_effects::color_math::contrast_ratio;
use aterm_types::scheme::{builtin, builtin_names};

fn pack(c: [u8; 3]) -> u32 {
    u32::from_be_bytes([0, c[0], c[1], c[2]])
}

/// The contrast `a` reads at on `b`.
fn reads(a: [u8; 3], b: [u8; 3]) -> f32 {
    contrast_ratio(pack(a), pack(b))
}

/// One built-in theme as both sides see it: its name, the accent an
/// instance in it gives every footer, and the blank a tab in it lays every
/// footer on.
struct Side {
    name: &'static str,
    accent: [u8; 3],
    blank: RenderCell,
}

/// Every built-in theme.
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

/// The greys a program draws its rule in on `blank`'s tab: Claude Code's
/// (`#888888`, its top rule's, measured), the grey of its panel (`#999999`,
/// 2.1.304, measured), and the tab's own ink (a rule drawn in the default).
fn rule_greys(blank: RenderCell) -> [RenderCell; 3] {
    [[0x88; 3], [0x99; 3], blank.fg].map(|fg| RenderCell {
        ch: '\u{2500}',
        fg,
        ..blank
    })
}

/// A tab in a theme of the owner's own whose ink reads under the text
/// floor (`#7a7a7a` on white, 4.29:1): every built-in's ink clears 4.5:1, so
/// only such a theme shows whether an ink the glass takes from the pane
/// as it is — a lit value, the lights' row ink — is held to the floor.
fn soft_tab() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [0x7a; 3],
        bg: [0xff; 3],
        ..RenderCell::default()
    }
}

/// Every built-in tab's blank, by name, and [`soft_tab`]'s.
fn tabs(sides: &[Side]) -> Vec<(&'static str, RenderCell)> {
    let mut tabs: Vec<_> = sides.iter().map(|s| (s.name, s.blank)).collect();
    tabs.push(("a soft theme of one's own", soft_tab()));
    tabs
}

/// Every mark the facts lead with, and the wall's `♻`.
const MARKS: [char; 10] = [
    footer::MODEL_MARK,
    footer::PATH_MARK,
    footer::BRANCH_MARK,
    footer::ACCOUNT_MARK,
    footer::PLAN_MARK,
    footer::WEEKLY_MARK,
    footer::WALL_MARK,
    footer::CONTEXT_MARK,
    footer::USAGE_MARK,
    footer::RESET_MARK,
];

/// A line with every fact's mark on it — the account, the plan, the weekly
/// quota, a wall with its `♻`, the model with its effort hidden (a stub),
/// the path, the branch hidden (a stub), the context and the usage.
fn every_fact() -> footer::RuleFit {
    use footer::Segment;
    footer::RuleFit {
        segments: vec![
            Segment::plain(footer::ACCOUNT_MARK, "me@example.test"),
            Segment::plain(footer::PLAN_MARK, "max"),
            Segment::plain(footer::WEEKLY_MARK, "40%"),
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

/// THE FACTS' MARKS READ ON EVERY GROUND: on the composer's bottom rule of
/// Claude Code, agy and Muse Code, every mark — `◆ ⌂ ⎇ ⎆ ◇ ◷ ⧗ ◐ Σ` and the
/// wall's `♻`, a hidden fact's stub's included — at rest and with each fact
/// lit, reads at 3:1 or more on the tab's ground, under every built-in
/// instance's accent, on every built-in tab theme, over each grey a program
/// draws its rule in. At rest a mark whose theme's own mix (the accent
/// halfway to the rule's grey) already clears 3:1 is that mix, unchanged.
/// A lit fact's own mark (the model's, the path's, the branch's, the
/// context's, the usage's; the effort leads with none) is the accent at the
/// text floor, 4.5:1 or more, as the top line's is: an accent that already
/// reads there is the accent itself.
///
/// NEGATIVE CONTROLS: that mix, as it was laid unfloored, reads under 3:1 —
/// Dracula's accent on a Default Light tab over a `#999999` rule — and the
/// accent floored to the graphics floor alone (3:1) reads under 4.5:1 there.
#[test]
fn every_fact_mark_reads_on_every_ground() {
    let sides = sides();
    let fit = every_fact();
    let room = 200;
    let mut checked = 0usize;
    let mut kept = 0usize;
    let (mut lit_marks, mut lit_kept) = (0usize, 0usize);
    // The mark a lit fact leads with.
    let lit_mark = |kind: FactKind| match kind {
        FactKind::Model => Some(footer::MODEL_MARK),
        FactKind::Path => Some(footer::PATH_MARK),
        FactKind::Branch => Some(footer::BRANCH_MARK),
        FactKind::Context => Some(footer::CONTEXT_MARK),
        FactKind::Usage => Some(footer::USAGE_MARK),
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
        sides.len() * sides.len() * 3 * 5,
        "every lit fact's mark"
    );
    assert!(lit_kept > 0, "some accent reads and is kept lit");
    // CONTROLS: the mix as it was laid, and the accent at the graphics floor.
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

/// THE FACTS' WORDS READ ON EVERY GROUND, the pane's own inks
/// ([`FactInks`]): on the bottom rule, under every built-in instance, on
/// every built-in tab theme and a soft theme of one's own, over each grey a
/// program draws its rule in, with no fact lit and each one lit:
/// - every value — bold, in the rule's grey lifted toward the pane's ink, or
///   lit in the pane's ink — reads at 4.5:1 or more on the tab's ground, and
///   at rest one whose theme's own mix already reads is that mix exactly;
/// - a hidden fact's greyed `off` (and the model's hidden effort), not bold,
///   reads at 3:1 or more and below the line's values, and where the
///   program's grey already reads below them at 3:1 or more it is that grey
///   exactly.
///
/// NEGATIVE CONTROLS: on a Default Light tab over a `#999999` rule the
/// values as they were laid read under 4.5:1 and the grey under 3:1, and on
/// the soft theme the pane's ink, a lit value as it was laid, under 4.5:1.
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
    // CONTROLS: the inks as they were laid.
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

/// The top-effort key's ghost (` KEY off `) as [`compose_top`] lays it on a
/// Claude Code top rule drawn in `rule` on `blank`'s tab under the
/// instance's `accent`, `lit` or at rest: its glyphs' cells.
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

/// THE KEY'S GHOST READS ON EVERY GROUND (design §10.6.14): ` KEY off ` on
/// Claude Code's top rule is greyed as a hidden fact's `off` is
/// ([`FactInks`]). Under every built-in instance, on every built-in tab
/// theme and a soft theme of one's own, over each grey a program draws its
/// rule in, its glyphs at rest read at 3:1 or more and below the same glyphs
/// lit, which read at 4.5:1 or more; at rest a grey that already reads so is
/// the grey exactly, lit a theme's own mix that reads is that mix exactly.
///
/// NEGATIVE CONTROL: on a Default Light tab over a `#999999` rule the ghost
/// as it was laid reads under 3:1 at rest and under 4.5:1 lit.
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

/// CRUISE'S `∞` READS ON EVERY GROUND: the harness's Autopilot chip at the
/// bottom rule's left end wears the instance's accent; under every built-in
/// instance on every built-in tab theme it reads at the chips' floor (4.5:1,
/// the floor Land's amber and the permission light's hues are held to) on
/// the tab's ground, and an accent that already reads there is kept as it is.
///
/// NEGATIVE CONTROL: Dracula's accent, unfloored, reads under 1.5:1 on a
/// Default Light tab.
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

/// The reply rows of agy's transcript the rewrite inks once the harness
/// holds the session (`collect_agy_transcript_edits`): the reply's lead `●`,
/// an H1, an H2 and an H3 heading, a link and inline code, a checked and an
/// unchecked box, a bullet, a nested bullet, an ordered item and a quote.
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

/// [`AGY_ROWS`] as Muse Code writes them: its reply leads with `◆`.
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

/// The glyphs the rewrite lays that are graphics, not letters: the leads
/// `●` and `◆`, `✓`, `•`, the unchecked box's `☐` and the nested bullet's
/// `◦`.
const MARKDOWN_MARKS: [char; 6] = [
    '\u{25cf}', '\u{25c6}', '\u{2713}', '\u{2022}', '\u{2610}', '\u{25e6}',
];

/// The glyphs the rewrite draws structure with, in the rule's own grey: a
/// horizontal rule's `─`, a quote's bar `▎`.
const MARKDOWN_STRUCTURE: [char; 2] = ['\u{2500}', '\u{258e}'];

/// The edits the transcript rewrite lays on `program`'s `rows` drawn on
/// `blank`'s tab, under `accent`, the program's rule in `rule`.
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

/// AGY'S AND MUSE CODE'S TRANSCRIPT READS ON EVERY GROUND: under every
/// built-in instance on every built-in tab theme, over each grey a program
/// draws its rule in, every glyph the rewrite inks — from the accent, or
/// from the rule's grey and the pane's ink — reads on the cell it is laid
/// on, in agy's transcript and in Muse Code's: the leads `●` and `◆`, `✓`,
/// `•`, `☐` and `◦` at 3:1 or more; the headings' (an H3's too), the
/// link's, the inline code's (on its pill), the ordered item's and the
/// quote's letters at 4.5:1 or more. The rule's `─` and the quote's `▎` are
/// structure, in the program's own grey exactly.
///
/// NEGATIVE CONTROLS: the lead `●` in Dracula's accent, as it was laid,
/// reads under 3:1 on a Default Light tab; a quote's words as they were
/// laid (the rule's grey lifted a third) under 4.5:1 on Solarized Light over
/// a `#999999` rule.
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
                                // The tab's own ink is the tab's: only the
                                // inks the rewrite makes are judged here.
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
    // 59 inked glyphs across each program's rows; fewer where an ink is the
    // tab's own.
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
