// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The original emphasis/slice pipeline and whole URL row, before borrowed runs.

use super::caption_test_support::instructions;
use super::*;

fn previous_emphasis(cap: &Caption) -> Range<usize> {
    let chars: Vec<char> = cap.text.chars().collect();
    previous_emphasis_span(&chars, &cap.host)
}

fn previous_emphasis_span(chars: &[char], host: &Range<usize>) -> Range<usize> {
    let Some(slice) = chars.get(host.start..host.end) else {
        return host.clone();
    };
    if slice
        .iter()
        .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ':' | '[' | ']'))
    {
        return host.clone();
    }
    let mut dots: Vec<usize> = slice
        .iter()
        .enumerate()
        .filter(|(_, ch)| **ch == '.')
        .map(|(i, _)| i)
        .collect();
    if dots.last() == Some(&slice.len().saturating_sub(1)) {
        dots.pop();
    }
    let Some(&last) = dots.last() else {
        return host.clone();
    };
    let n = dots.len();
    if n < 2 {
        return host.clone();
    }
    let second_last_len = last - dots[n - 2] - 1;
    let keep = if second_last_len <= 3 { 3 } else { 2 };
    let start = if n >= keep { dots[n - keep] + 1 } else { 0 };
    host.start + start..host.end
}

fn previous_slice(cap: &Caption, run: &Range<usize>) -> String {
    cap.text.chars().skip(run.start).take(run.len()).collect()
}

fn previous_row(
    url: &str,
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
    let lead = if !crate::is_safe_url(url) {
        BLOCKED
    } else if tracking {
        OPENS_TRACKED
    } else {
        OPENS
    };
    let with_lead = body.saturating_sub(lead.len() + GAP);
    let (url_col, url_cells) = if with_lead >= LEAD_IN_URL_CELLS {
        write_str(&mut row, cols, MARGIN, lead, c.label, c.bar_bg, false);
        (MARGIN + lead.len() + GAP, with_lead)
    } else {
        (MARGIN, body)
    };
    // Caption formatting and host admission are unchanged by this patch.
    let cap = caption(url, url_cells);
    if !cap.text.chars().any(|ch| ch != ELLIPSIS) {
        return None;
    }
    write_str(&mut row, cols, url_col, &cap.text, c.label, c.bar_bg, false);
    let emphasis = previous_emphasis(&cap);
    if !emphasis.is_empty() {
        write_str(
            &mut row,
            cols,
            url_col + emphasis.start,
            &previous_slice(&cap, &emphasis),
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

fn texts() -> Vec<String> {
    let mut cases: Vec<String> = [
        "",
        ".",
        "1.2.3.4",
        "[2001:db8::1]",
        "www.evil.example.",
        "www.example.co.uk",
        "a..b...c.",
        "…猫.🦀.example./x",
        "é.x.猫.rs",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let alphabet = ['a', '.', '1', ':', '[', ']', '猫', '🦀', '…', '\u{202e}'];
    let mut seed = 0x2468_ace0u32;
    for len in 0..64 {
        let mut text = String::new();
        for _ in 0..len {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            text.push(alphabet[seed as usize % alphabet.len()]);
        }
        cases.push(text);
    }
    cases
}

#[test]
fn borrowed_runs_and_constant_space_emphasis_match_original_character_ranges() {
    for text in texts() {
        let width = text.chars().count();
        for start in (0..=width + 1).chain([usize::MAX]) {
            for end in (0..=width + 1).chain([usize::MAX]) {
                let cap = Caption {
                    text: text.clone(),
                    host: start..end,
                    shape: HostShape::Labels,
                };
                assert_eq!(cap.emphasis(), previous_emphasis(&cap), "{cap:?}");
                assert_eq!(
                    cap.slice(&cap.host),
                    previous_slice(&cap, &cap.host),
                    "{cap:?}"
                );
                let range = cap.emphasis();
                assert_eq!(cap.slice(&range), previous_slice(&cap, &range), "{cap:?}");
            }
        }
    }
    for (text, expected) in [
        ("www.evil.example", "evil.example"),
        ("www.example.co.uk", "example.co.uk"),
        ("www.evil.example.", "evil.example."),
        ("1.2.3.4", "1.2.3.4"),
        ("[2001:db8::1]", "[2001:db8::1]"),
    ] {
        let cap = Caption {
            text: text.into(),
            host: 0..text.chars().count(),
            shape: HostShape::Labels,
        };
        assert_eq!(cap.emphasis_text(), expected);
    }
}

fn urls() -> Vec<String> {
    let mut cases: Vec<String> = [
        "",
        "https://evil.example/x",
        "https://www.example.co.uk/x",
        "https://10.0.0.7:8443/x",
        "https://[2001:db8::1]/x",
        "https://www.evil.example./x",
        "https://a..b...c./x",
        "https://google.com@evil.example/x",
        "https://gооgle.com.猫.example/x",
        "https://a\u{202e}b.example/x",
        "file:///etc/passwd",
        "mailto:ops@evil.example",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    for labels in 0..32 {
        cases.push(format!("https://{}evil.example/path", "a.".repeat(labels)));
        cases.push(format!("https://{}example.co.uk/path", "a.".repeat(labels)));
    }
    cases.push(format!("https://{}.evil.example/x", "a".repeat(400)));
    cases.push(format!("https://evil.example/{}", "x".repeat(400)));
    cases
}

#[test]
fn complete_url_rows_match_across_elision_escaping_and_styles() {
    for url in urls() {
        for cols in [0, 8, 11, 12, 16, 20, 31, 40, 80, 160] {
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
                            caption_row(&url, cols, theme, seam, tracking),
                            previous_row(&url, cols, theme, seam, tracking),
                            "url={url:?} cols={cols} seam={seam} tracking={tracking}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual paired URL-emphasis and whole-row allocation/instruction measurement"]
fn url_caption_cost() {
    use std::hint::black_box;
    type EmphasisProbe = fn(&Caption) -> Range<usize>;
    type RowProbe = fn(&str, usize, Theme, bool, bool) -> Option<Vec<RenderCell>>;
    let host = format!("https://{}.evil.example/x", "a".repeat(400));
    let labels = format!("https://{}example.co.uk/x", "a.".repeat(100));
    let path = format!("https://evil.example/{}", "x".repeat(400));
    for (name, url, cols) in [
        ("empty", "", 80),
        ("short", "https://evil.example/x", 80),
        ("short-suffix", "https://www.example.co.uk/x", 80),
        ("ipv4", "https://10.0.0.7:8443/x", 80),
        ("ipv6", "https://[2001:db8::1]/x", 80),
        ("trailing-dot", "https://www.evil.example./x", 80),
        ("unicode", "https://gооgle.com.猫.example/x", 80),
        ("hostile", "https://a\u{202e}b.example/x", 80),
        ("userinfo", "https://google.com@evil.example/x", 80),
        ("long-host", host.as_str(), 80),
        ("many-labels", labels.as_str(), 80),
        ("long-path", path.as_str(), 80),
        ("narrow", "https://www.example.co.uk/x", 20),
        ("zero", path.as_str(), 0),
        ("blocked", "file:///etc/passwd", 80),
    ] {
        let cap = caption(url, cols);
        assert_eq!(cap.emphasis(), previous_emphasis(&cap));
        assert_eq!(
            caption_row(url, cols, Theme::default(), true, false),
            previous_row(url, cols, Theme::default(), true, false)
        );
        let mut emphasis: [(&str, EmphasisProbe); 2] = [
            ("reference", previous_emphasis),
            ("candidate", Caption::emphasis),
        ];
        let mut rows: [(&str, RowProbe); 2] =
            [("reference", previous_row), ("candidate", caption_row)];
        for sample in 0..5 {
            if sample != 0 {
                emphasis.swap(0, 1);
                rows.swap(0, 1);
            }
            for (variant, probe) in emphasis {
                for _ in 0..100 {
                    black_box(probe(&cap));
                }
                let (_, allocations) = aterm_heap_count::allocations_in(|| probe(&cap));
                let before = instructions();
                for _ in 0..2000 {
                    black_box(probe(black_box(&cap)));
                }
                let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "url-caption case={name} scope=emphasis variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                );
            }
            for (variant, probe) in rows {
                let theme = Theme::default();
                for _ in 0..100 {
                    black_box(probe(url, cols, theme, true, false));
                }
                let (_, allocations) =
                    aterm_heap_count::allocations_in(|| probe(url, cols, theme, true, false));
                let before = instructions();
                for _ in 0..2000 {
                    black_box(probe(
                        black_box(url),
                        black_box(cols),
                        black_box(theme),
                        true,
                        false,
                    ));
                }
                let work = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "url-caption case={name} scope=row variant={variant} sample={sample} iterations=2000 allocations={allocations} instructions={work:?}"
                );
            }
        }
    }
}
