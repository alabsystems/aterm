// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::RenderCell;

fn row(text: &str, cols: u16) -> Vec<RenderCell> {
    let mut term = aterm_core::terminal::Terminal::new(4, cols);
    term.process(text.as_bytes());
    term.render_row(0)
}

// The scanner and owned-buffer pipeline from main before the borrowed-view change.
fn original_span(chars: &[char], col: usize) -> Option<std::ops::Range<usize>> {
    let n = chars.len();
    let is_url = |c: char| c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c);
    let mut i = 0;
    while i < n {
        let s = &chars[i..];
        if s.starts_with(&['h', 't', 't', 'p', ':', '/', '/'])
            || s.starts_with(&['h', 't', 't', 'p', 's', ':', '/', '/'])
        {
            let mut j = i;
            while j < n && is_url(chars[j]) {
                j += 1;
            }
            let mut end = j;
            while end > i
                && matches!(
                    chars[end - 1],
                    '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '\'' | '"'
                )
            {
                end -= 1;
            }
            if (i..end).contains(&col) {
                return Some(i..end);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    None
}

fn original(cells: &[RenderCell], col: usize, contrast: f32) -> Option<(String, bool)> {
    let ink = |cell: &RenderCell| {
        let bg = aterm_render::rgb_to_u32(cell.bg);
        let fg = aterm_render::rgb_to_u32(cell.fg);
        (aterm_render::floor_min_contrast_fg(fg, bg, contrast), bg)
    };
    let mut chars: Vec<char> = cells
        .iter()
        .map(|c| if c.wide { ' ' } else { c.ch })
        .collect();
    let span = original_span(&chars, col)?;
    for (ch, cell) in chars[span.clone()].iter_mut().zip(&cells[span]) {
        let (fg, bg) = ink(cell);
        if fg == bg {
            *ch = ' ';
        }
    }
    let span = original_span(&chars, col)?;
    let url = chars[span.clone()].iter().collect();
    let faint = cells[span]
        .iter()
        .map(ink)
        .any(|(fg, bg)| aterm_render::floor_fg_contrast(fg, bg, super::FAINT_URL_CONTRAST) != fg);
    Some((url, faint))
}

fn candidate(cells: &[RenderCell], col: usize, contrast: f32) -> Option<(String, bool)> {
    super::plain_url_at(cells, col, contrast).map(|url| (url.to_url_string(), url.faint))
}

#[test]
fn borrowed_url_matches_original_across_ink_widths_and_boundaries() {
    let texts = [
        "Docs: https://example.com/path?q=1#frag end",
        "(http://a.b/p). https://second.example/x?!",
        "http:// https:// https:/ hhttp://a/b httpsx://a",
        "中 אב https://example.com/a[]()'.,;:!? xyz",
        "https://a.example/https://nested.example/x",
        "prefix: \x1b[8mhttps://hidden.example/\x1b[28mhttps://visible.example/x",
        "prefix: https://good.example\x1b[8m.evil.example/x\x1b[28m end",
        "\x1b#6http://wide.example/x",
        "ordinary text with 中 and no destination",
    ];
    for text in texts {
        for cols in [8, 24, 80, 128] {
            let original_cells = row(text, cols);
            for mutation in 0..5 {
                let mut cells = original_cells.clone();
                for (i, cell) in cells.iter_mut().enumerate() {
                    match mutation {
                        1 if i.is_multiple_of(7) => cell.fg = cell.bg,
                        2 if i.is_multiple_of(3) => cell.fg = [20, 20, 20],
                        3 if i.is_multiple_of(11) => cell.wide = true,
                        4 if i.is_multiple_of(5) => cell.fg = [0, 0, 0],
                        _ => {}
                    }
                }
                for contrast in [0.0, 1.0, 4.5, 21.0, f32::NAN, f32::INFINITY] {
                    for col in (0..cells.len() + 2).chain([usize::MAX]) {
                        assert_eq!(
                            candidate(&cells, col, contrast),
                            original(&cells, col, contrast),
                            "{text:?} cols={cols} mutation={mutation} contrast={contrast} col={col}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_hidden_prefix_can_expose_a_later_url_in_the_same_raw_token() {
    let cells = row(
        "Docs: \x1b[8mhttps://a/\x1b[28mhttps://visible.example/x",
        80,
    );
    assert_eq!(
        candidate(&cells, 25, 1.0),
        Some(("https://visible.example/x".into(), false))
    );
    assert_eq!(candidate(&cells, 25, 1.0), original(&cells, 25, 1.0));
}

type Probe = fn(&[RenderCell], usize, f32, bool) -> (Option<String>, bool);

fn old_probe(cells: &[RenderCell], col: usize, contrast: f32, own: bool) -> (Option<String>, bool) {
    original(cells, col, contrast).map_or((None, false), |(url, faint)| {
        ((own || faint).then_some(url), true)
    })
}

fn new_probe(cells: &[RenderCell], col: usize, contrast: f32, own: bool) -> (Option<String>, bool) {
    super::plain_url_at(cells, col, contrast).map_or((None, false), |url| {
        ((own || url.faint).then(|| url.to_url_string()), true)
    })
}

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: the output has the size/alignment of RUSAGE_INFO_V4; read only on success.
    let result = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: a successful proc_pid_rusage initialized the output.
    (result == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

#[test]
#[ignore = "manual paired URL-view allocation and instruction measurement"]
fn plain_url_view_cost() {
    use std::hint::black_box;
    let long = format!("Docs: https://example.com/{}", "abcd".repeat(100));
    for (name, text, cols, col, contrast, own) in [
        (
            "bright-hover",
            "Docs: https://example.com/path",
            80,
            10,
            1.0,
            false,
        ),
        (
            "bright-click",
            "Docs: https://example.com/path",
            80,
            10,
            1.0,
            true,
        ),
        (
            "faint-hover",
            "Docs: \x1b[38;2;20;20;20mhttps://example.com/path",
            80,
            10,
            1.0,
            false,
        ),
        (
            "faint-floor",
            "Docs: \x1b[38;2;20;20;20mhttps://example.com/path",
            80,
            10,
            4.5,
            false,
        ),
        (
            "concealed",
            "Docs: \x1b[8mhttps://example.com/path",
            80,
            10,
            1.0,
            false,
        ),
        (
            "none-80",
            "ordinary text with no destination",
            80,
            10,
            1.0,
            false,
        ),
        (
            "none-1024",
            "ordinary text with no destination",
            1024,
            10,
            1.0,
            false,
        ),
        ("long-hover", long.as_str(), 1024, 100, 1.0, false),
        ("long-click", long.as_str(), 1024, 100, 1.0, true),
        (
            "nested-visible",
            "Docs: \x1b[8mhttps://a/\x1b[28mhttps://visible.example/x",
            80,
            25,
            1.0,
            false,
        ),
    ] {
        let cells = row(text, cols);
        let mut probes: [(&str, Probe); 2] = [("reference", old_probe), ("candidate", new_probe)];
        assert_eq!(
            old_probe(&cells, col, contrast, own),
            new_probe(&cells, col, contrast, own)
        );
        for (_, probe) in probes {
            for _ in 0..100 {
                black_box(probe(&cells, col, contrast, own));
            }
        }
        for sample in 0..5 {
            if sample != 0 {
                probes.swap(0, 1);
            }
            for (variant, probe) in probes {
                let (_, calls) =
                    aterm_heap_count::allocations_in(|| probe(&cells, col, contrast, own));
                let before = instructions();
                for _ in 0..5_000 {
                    black_box(probe(
                        black_box(&cells),
                        black_box(col),
                        black_box(contrast),
                        black_box(own),
                    ));
                }
                let count = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "url-view case={name} variant={variant} sample={sample} iterations=5000 allocations={calls} instructions={count:?}"
                );
            }
        }
    }
}
