// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Right-to-left run shaping and adjacent LTR ligature tests.

use aterm_core::config::{BiDiConfig, BiDiMode, TerminalConfig};
use aterm_core::terminal::Terminal;
use aterm_render::{ColumnGlyph, Renderer, Theme};

const CASCADIA: &[u8] = include_bytes!("../assets/bundled/CascadiaMonoPL-Regular.ttf");
const JETBRAINS: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
const MARHABA: &str = "\u{645}\u{631}\u{62D}\u{628}\u{627}";
const SHALOM: &str = "\u{5E9}\u{5DC}\u{5D5}\u{5DD}";
const OLAM: &str = "\u{5E2}\u{5D5}\u{5DC}\u{5DD}";

fn plan_of(
    r: &mut Renderer,
    text: &str,
    cols: usize,
) -> (aterm_core::render::RenderInput, Vec<ColumnGlyph>) {
    let mut term = Terminal::new(1, cols as u16);
    term.process(format!("\x1b[?25l{text}").as_bytes());
    let input = term.cell_frame(1, cols);
    let mut plan = Vec::new();
    r.row_glyph_plan(&input, 0, &[], &mut plan);
    (input, plan)
}

fn logical_plan(input: &aterm_core::render::RenderInput, logical: &str) -> Vec<ColumnGlyph> {
    use aterm_font::shape::{Feature, ShapeFace, UnicodeBuffer};
    let face = ShapeFace::from_slice(CASCADIA, 0).unwrap();
    let features: Vec<Feature> = aterm_render::ligature_shaping::build_feature_list(&[], true)
        .iter()
        .map(|f| Feature::new(aterm_font::Tag::from_bytes(&f.tag), f.value))
        .collect();
    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(logical);
    let shaped = aterm_font::shape::shape(&face, &features, buffer);
    let chars: Vec<(usize, char)> = logical.char_indices().collect();
    let map = &input.visual_to_logical[0];
    let mut out = vec![ColumnGlyph::PerCell; map.len()];
    for info in shaped.glyph_infos() {
        let l = chars
            .iter()
            .position(|&(offset, _)| offset == info.cluster as usize)
            .expect("one glyph per character");
        let v = map.iter().position(|&m| usize::from(m) == l).unwrap();
        let cmap = face
            .face()
            .glyph_index(chars[l].1)
            .map_or(0, |g| u32::from(g.0));
        if info.glyph_id != cmap {
            out[v] = ColumnGlyph::Ligated(info.glyph_id as u16);
        }
    }
    out
}

fn recorded(case: &str) -> Vec<(u32, u32)> {
    let corpus = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/font-oracle/shaping.txt"
    ))
    .unwrap();
    let line = corpus
        .lines()
        .find(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            f.len() == 8
                && f[1] == "CascadiaMonoPL-Regular"
                && f[4] == "liga=1,calt=1"
                && f[5] == case
        })
        .unwrap_or_else(|| panic!("no recorded {case}"));
    let answer = line.rsplit('\t').next().unwrap();
    answer
        .split_once('|')
        .unwrap()
        .1
        .split_whitespace()
        .map(|t| {
            let mut p = t.split(':');
            (
                p.next().unwrap().parse().unwrap(),
                p.next().unwrap().parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn visual_rtl_runs_join_in_logical_order() {
    let mut r = Renderer::from_bytes(CASCADIA, 18.0, Theme::default()).unwrap();
    let (input, plan) = plan_of(&mut r, MARHABA, 8);
    assert_eq!(input.cells[0][0].ch, '\u{627}');
    assert_eq!(input.cells[0][4].ch, '\u{645}');
    let want = logical_plan(&input, MARHABA);
    assert_eq!(
        plan[..5],
        want[..5],
        "each letter takes its logical joining form"
    );
    let ligated = |c: usize| match plan[c] {
        ColumnGlyph::Ligated(gid) => gid,
        other => panic!("column {c} is {other:?}"),
    };
    // Negative control: shaping the visual run as logical text (`ar.marhaba.visual.0`)
    // draws meem final and alef isolated.
    let old: Vec<(u32, u32)> = recorded("ar.marhaba.visual.0");
    let old_at = |col: u32| old.iter().find(|&&(_, c)| c == 2 * col).unwrap().0;
    assert_ne!(
        u32::from(ligated(4)),
        old_at(4),
        "meem no longer takes the final form"
    );
    assert_ne!(
        u32::from(ligated(0)),
        old_at(0),
        "alef no longer stands isolated"
    );
    let logical = recorded("ar.marhaba.logical");
    let logical_at = |byte: u32| logical.iter().find(|&&(_, c)| c == byte).unwrap().0;
    assert_eq!(u32::from(ligated(4)), logical_at(0));
    assert_eq!(u32::from(ligated(0)), logical_at(8));
}

#[test]
fn brackets_mirror_by_level_not_by_run() {
    let mut r = Renderer::from_bytes(CASCADIA, 18.0, Theme::default()).unwrap();
    let text = format!("({MARHABA})");
    let (input, plan) = plan_of(&mut r, &text, 8);
    assert_eq!(input.cells[0][0].ch, '(');
    assert_eq!(input.cells[0][6].ch, ')');
    assert_eq!(plan[0], ColumnGlyph::PerCell, "{plan:?}");
    assert_eq!(plan[6], ColumnGlyph::PerCell, "{plan:?}");
    let face = aterm_font::Face::parse(CASCADIA, 0).unwrap();
    let close = u32::from(face.glyph_index(')').unwrap().0);
    let old = recorded("ar.parens.visual.0");
    assert_eq!(old.iter().find(|&&(_, c)| c == 0).unwrap().0, close);

    let inside = "\u{627}(\u{628})\u{62C}";
    let (input, _) = plan_of(&mut r, inside, 8);
    let row: String = input.cells[0][..5].iter().map(|c| c.ch).collect();
    assert_eq!(
        row, "\u{62C}(\u{628})\u{627}",
        "reversed, and each bracket mirrored"
    );
}

#[test]
fn logical_order_rows_join_as_written() {
    let mut r = Renderer::from_bytes(CASCADIA, 18.0, Theme::default()).unwrap();
    let mut term = Terminal::new(1, 8);
    term.apply_config(&TerminalConfig {
        bidi: BiDiConfig {
            mode: BiDiMode::Disabled,
            ..BiDiConfig::default()
        },
        ..TerminalConfig::default()
    });
    term.process(format!("\x1b[?25l{MARHABA}").as_bytes());
    let input = term.cell_frame(1, 8);
    assert!(input.visual_to_logical_row(0).is_empty(), "not reordered");
    assert_eq!(input.cells[0][0].ch, '\u{645}', "meem first: logical order");
    let mut plan = Vec::new();
    r.row_glyph_plan(&input, 0, &[], &mut plan);

    let face = aterm_font::Face::parse(CASCADIA, 0).unwrap();
    let cmap = |c: char| u32::from(face.glyph_index(c).unwrap().0);
    let at = |answer: &[(u32, u32)], byte: u32| answer.iter().find(|&&(_, c)| c == byte).unwrap().0;
    let column = |gid: u32, ch: char| {
        if gid == cmap(ch) {
            ColumnGlyph::PerCell
        } else {
            ColumnGlyph::Ligated(u16::try_from(gid).unwrap())
        }
    };
    let logical = recorded("ar.marhaba.logical");
    let want: Vec<ColumnGlyph> = (0u32..)
        .zip(MARHABA.chars())
        .map(|(j, ch)| column(at(&logical, 2 * j), ch))
        .collect();
    assert_eq!(plan[..5], want[..], "each letter takes its logical form");
    // Negative control: shaping the row reversed yields `ar.marhaba.visual.0` forms.
    let visual = recorded("ar.marhaba.visual.0");
    let backwards: Vec<ColumnGlyph> = (0u32..)
        .zip(MARHABA.chars())
        .map(|(j, ch)| column(at(&visual, 2 * (4 - j)), ch))
        .collect();
    assert_ne!(backwards[0], want[0], "meem's two forms differ");
    assert_ne!(backwards[4], want[4], "alef's two forms differ");
}

#[test]
fn operators_beside_an_rtl_word_ligate_as_in_an_ltr_run() {
    let mut r = Renderer::from_bytes(JETBRAINS, 18.0, Theme::default()).unwrap();
    let mut checked = 0;
    for op in ["==", "...", "!=", "//", "=>", "->"] {
        let k = op.chars().count();
        for (logical, at, ltr_text) in [
            (format!("{SHALOM}{op}"), 4, format!("abcd{op}")),
            (format!("{MARHABA}{op}"), 5, format!("abcde{op}")),
            (format!("{SHALOM}{op}{OLAM}"), 4, format!("abcd{op}efgh")),
        ] {
            let (input, plan) = plan_of(&mut r, &logical, 16);
            assert!(
                !input.visual_to_logical_row(0).is_empty(),
                "{logical}: the row was reordered"
            );
            let painted: String = input.cells[0][at..at + k].iter().map(|c| c.ch).collect();
            let (_, ltr) = plan_of(&mut r, &ltr_text, 16);
            let ltr_op = &ltr[at..at + k];
            if painted != op {
                // `->` between two Hebrew words is painted mirrored (`<-`).
                continue;
            }
            assert!(
                ltr_op.iter().any(|g| matches!(g, ColumnGlyph::Ligated(_))),
                "{op} ligates on JetBrains Mono: {ltr_op:?}"
            );
            assert_eq!(&plan[at..at + k], ltr_op, "{logical}: {plan:?}");
            let reversed: Vec<ColumnGlyph> = ltr_op.iter().rev().copied().collect();
            if reversed != ltr_op {
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 10,
        "only {checked} operators tell a reversal apart"
    );
}
