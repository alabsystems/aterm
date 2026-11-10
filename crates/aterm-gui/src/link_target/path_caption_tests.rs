// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent pre-change caption pipeline and paired whole-row measurements.

use super::caption_test_support::instructions;
use super::*;

fn previous_safe_form(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\u{21}'..='\u{7e}') {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(HEX[usize::from(b >> 4)]);
                out.push(HEX[usize::from(b & 0x0f)]);
            }
        }
    }
    out
}

fn previous_caption(shown: &str, max_cells: usize) -> (String, Range<usize>) {
    let safe = previous_safe_form(shown);
    let chars: Vec<char> = safe.chars().collect();
    let text: Vec<char> = if chars.len() <= max_cells {
        chars
    } else if max_cells == 0 {
        Vec::new()
    } else {
        let keep = max_cells - 1;
        std::iter::once(ELLIPSIS)
            .chain(chars[chars.len() - keep..].iter().copied())
            .collect()
    };
    let name_start = text
        .iter()
        .rposition(|ch| *ch == '/')
        .map_or(0, |at| at + 1)
        .max(usize::from(text.first() == Some(&ELLIPSIS)));
    (text.iter().collect(), name_start..text.len())
}

fn previous_row(
    shown: &str,
    cols: usize,
    palette: impl Into<chrome_band::ChromePalette>,
    seam: bool,
    tracking: bool,
) -> Option<Vec<RenderCell>> {
    let body = cols.saturating_sub(MARGIN * 2);
    if body < MIN_URL_CELLS {
        return None;
    }
    let c = chrome_band::band_colors(palette);
    let mut row = blank_row(cols, c.label, c.bar_bg, seam);
    let lead = if tracking {
        OPENS_IN_ATERM_TRACKED
    } else {
        OPENS_IN_ATERM
    };
    let gap = 1;
    let with_lead = body.saturating_sub(lead.len() + gap);
    let (name_col, name_cells) = if with_lead >= LEAD_IN_URL_CELLS {
        write_str(&mut row, cols, MARGIN, lead, c.label, c.bar_bg, false);
        (MARGIN + lead.len() + gap, with_lead)
    } else {
        (MARGIN, body)
    };
    let (text, emphasis) = previous_caption(shown, name_cells);
    if !text.chars().any(|ch| ch != ELLIPSIS) {
        return None;
    }
    write_str(&mut row, cols, name_col, &text, c.label, c.bar_bg, false);
    if !emphasis.is_empty() {
        let run: String = text
            .chars()
            .skip(emphasis.start)
            .take(emphasis.len())
            .collect();
        write_str(
            &mut row,
            cols,
            name_col + emphasis.start,
            &run,
            c.value,
            c.bar_bg,
            true,
        );
    }
    if seam {
        chrome_band::seal_band_top(&mut row, c.seam);
    }
    Some(row)
}

fn inputs() -> Vec<String> {
    let mut cases = vec![
        String::new(),
        "/".into(),
        "src/foo.rs:42".into(),
        "src/猫.rs:42".into(),
        "src/a\u{202e}sr.exe:1".into(),
        "src/a b%20/é🦀.rs:0".into(),
        "src/trailing/".into(),
        format!("{}/deep/猫.rs:7", "a".repeat(4096)),
    ];
    let alphabet = [
        'a', '/', ':', '%', ' ', '\0', '\x7f', 'é', '猫', '🦀', '\u{202e}',
    ];
    let mut seed = 0x1234_5678u32;
    for len in 0..128 {
        let mut value = String::new();
        for _ in 0..len {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            value.push(alphabet[seed as usize % alphabet.len()]);
        }
        cases.push(value);
    }
    cases
}

#[test]
fn suffix_encoding_and_emphasis_match_the_complete_previous_pipeline() {
    for shown in inputs() {
        for width in (0..=128).chain([256, 8192, usize::MAX]) {
            assert_eq!(
                path_caption(&shown, width),
                previous_caption(&shown, width),
                "shown={shown:?} width={width}"
            );
        }
    }
    // Truncation may split an escape or the encoding of one UTF-8 character.
    assert_eq!(path_caption("é", 3), ("…A9".into(), 1..3));
    assert_eq!(path_caption("x/猫", 5), ("…C%AB".into(), 1..5));
    assert_eq!(path_caption("anything", 0), (String::new(), 0..0));
}

#[test]
fn complete_caption_rows_match_including_style_and_lead_in() {
    for shown in inputs().iter().step_by(7) {
        for cols in [0, 8, 11, 12, 13, 20, 31, 40, 80, 160] {
            for seam in [false, true] {
                for tracking in [false, true] {
                    for theme in [
                        Theme::default(),
                        Theme {
                            fg: 0x202020,
                            bg: 0xf0f0f0,
                            cursor: 0x008000,
                            selection: 0xc0c0e0,
                        },
                    ] {
                        assert_eq!(
                            path_caption_row(shown, cols, theme, seam, tracking),
                            previous_row(shown, cols, theme, seam, tracking),
                            "shown={shown:?} cols={cols} seam={seam} tracking={tracking}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual paired path-caption and whole-row allocation/instruction measurement"]
fn path_caption_cost() {
    use std::hint::black_box;
    type CaptionProbe = fn(&str, usize) -> (String, Range<usize>);
    type RowProbe = fn(&str, usize, Theme, bool, bool) -> Option<Vec<RenderCell>>;
    let long = format!("{}/deep/猫.rs:7", "a".repeat(4096));
    for (name, shown, cols) in [
        ("empty", "", 80),
        ("short", "src/foo.rs:42", 80),
        ("unicode", "src/猫/🦀.rs:42", 80),
        ("hostile", "src/a\u{202e}sr.exe:1", 40),
        ("clipped", "src/long/directory/name/and/file.rs:42", 20),
        ("long", long.as_str(), 80),
        ("zero", long.as_str(), 0),
        ("one", "猫.rs:42", 1),
    ] {
        assert_eq!(path_caption(shown, cols), previous_caption(shown, cols));
        let mut captions: [(&str, CaptionProbe); 2] =
            [("reference", previous_caption), ("candidate", path_caption)];
        let mut rows: [(&str, RowProbe); 2] =
            [("reference", previous_row), ("candidate", path_caption_row)];
        for sample in 0..5 {
            if sample != 0 {
                captions.swap(0, 1);
                rows.swap(0, 1);
            }
            for (variant, probe) in captions {
                for _ in 0..100 {
                    black_box(probe(shown, cols));
                }
                let (_, allocations) = aterm_heap_count::allocations_in(|| probe(shown, cols));
                let before = instructions();
                for _ in 0..2000 {
                    black_box(probe(black_box(shown), black_box(cols)));
                }
                let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "path-caption case={name} scope=caption variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                );
            }
            for (variant, probe) in rows {
                let theme = Theme::default();
                for _ in 0..100 {
                    black_box(probe(shown, cols, theme, true, false));
                }
                let (_, allocations) =
                    aterm_heap_count::allocations_in(|| probe(shown, cols, theme, true, false));
                let before = instructions();
                for _ in 0..2000 {
                    black_box(probe(
                        black_box(shown),
                        black_box(cols),
                        black_box(theme),
                        true,
                        false,
                    ));
                }
                let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "path-caption case={name} scope=row variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                );
            }
        }
    }
}
