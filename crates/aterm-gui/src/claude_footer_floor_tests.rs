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
///
/// NEGATIVE CONTROL: that mix, as it was laid unfloored, reads under 3:1 —
/// Dracula's accent on a Default Light tab over a `#999999` rule.
#[test]
fn every_fact_mark_reads_on_every_ground() {
    let sides = sides();
    let fit = every_fact();
    let room = 200;
    let mut checked = 0usize;
    let mut kept = 0usize;
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
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 17 * 17 * 3 * 7 * MARKS.len(), "{checked}");
    assert!(kept > 0, "some theme's own mix reads and is kept");
    // CONTROL: the mix as it was laid.
    let dracula = side(&sides, "Dracula").accent;
    let light = side(&sides, "Default Light").blank;
    let old = mix(dracula, [0x99; 3], 0.5);
    assert!(
        reads(old, light.bg) < ICON_CONTRAST,
        "CONTROL: Dracula's mark at {:.2}:1 on Default Light",
        reads(old, light.bg)
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

/// The reply rows of agy's and Muse Code's transcript that wear the accent
/// once the harness holds the session (`collect_agy_transcript_edits`): the
/// reply's lead `●`, an H1 and an H2 heading, a link and inline code, a
/// checked box, a bullet and an ordered item.
const ACCENT_ROWS: [&str; 6] = [
    "\u{25cf} # Overview",
    "  ## Section",
    "  See [docs](https://example.com) and `code` now.",
    "  - [x] Done",
    "  - Item",
    "  1. First",
];

/// The glyphs of [`ACCENT_ROWS`] that are graphics, not letters.
const MARKDOWN_MARKS: [char; 3] = ['\u{25cf}', '\u{2713}', '\u{2022}'];

/// The edits the transcript rewrite lays on [`ACCENT_ROWS`] drawn on
/// `blank`'s tab, under `accent`, the program's rule in `rule`.
fn accent_row_edits(blank: RenderCell, rule: RenderCell, accent: [u8; 3]) -> Vec<AgyRowEdit> {
    let mut scratch = RenderInput::empty();
    scratch.rows = ACCENT_ROWS.len();
    scratch.cols = 60;
    scratch.cursor_visible = false;
    scratch.cells = ACCENT_ROWS
        .iter()
        .map(|r| {
            let mut row: Vec<RenderCell> = r.chars().map(|ch| RenderCell { ch, ..blank }).collect();
            row.resize(scratch.cols, blank);
            row
        })
        .collect();
    let rows: Vec<String> = ACCENT_ROWS.iter().map(|r| (*r).to_owned()).collect();
    let mut edits = Vec::new();
    collect_agy_transcript_edits(
        &scratch,
        aterm_phase::Program::Agy,
        &rows,
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

/// AGY'S AND MUSE CODE'S ACCENTED TRANSCRIPT READS ON EVERY GROUND: under
/// every built-in instance on every built-in tab theme, over each grey a
/// program draws its rule in, every glyph the rewrite inks from the accent
/// reads on the cell it is laid on — the lead `●`, `✓` and `•` at 3:1 or
/// more, the headings', the link's, the inline code's (on its pill) and the
/// ordered item's letters at 4.5:1 or more.
///
/// NEGATIVE CONTROL: the lead `●` in Dracula's accent, as it was laid,
/// reads under 3:1 on a Default Light tab.
#[test]
fn the_accented_transcript_reads_on_every_ground() {
    let sides = sides();
    let mut checked = 0usize;
    for instance in &sides {
        for tab in &sides {
            let blank = tab.blank;
            for rule in rule_greys(blank) {
                let edits = accent_row_edits(blank, rule, instance.accent);
                assert_eq!(edits.len(), ACCENT_ROWS.len(), "every row is rewritten");
                for (row, edit) in edits.iter().enumerate() {
                    for c in &edit.cells {
                        if c.ch == ' ' {
                            continue;
                        }
                        let what = format!(
                            "{}'s accent on {} over {:?}: {:?} of {:?}",
                            instance.name, tab.name, rule.fg, c.ch, ACCENT_ROWS[row]
                        );
                        if MARKDOWN_MARKS.contains(&c.ch) {
                            let k = reads(c.fg, c.bg);
                            assert!(k >= ICON_CONTRAST, "{what}: a mark at {k:.2}:1");
                            checked += 1;
                        } else if c.fg != blank.fg {
                            // The tab's own ink is the tab's: only the
                            // accent's inks are judged here.
                            let k = reads(c.fg, c.bg);
                            assert!(k >= NAME_CONTRAST, "{what}: a letter at {k:.2}:1");
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    // 28 accented glyphs across the rows; fewer where an ink is the tab's own.
    assert!(checked >= sides.len().pow(2) * 3 * 20, "{checked}");
    let dracula = side(&sides, "Dracula").accent;
    let light = side(&sides, "Default Light").blank;
    assert!(
        reads(dracula, light.bg) < ICON_CONTRAST,
        "CONTROL: the lead `●` at {:.2}:1 on Default Light",
        reads(dracula, light.bg)
    );
}
