// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{FAINT_URL_CONTRAST, PlainUrl, find_url_span};
use crate::RenderCell;

// The complete URL helper before lookup-local color reuse.
fn uncached_url(cells: &[RenderCell], col: usize, min_contrast: f32) -> Option<PlainUrl<'_>> {
    let ink = |cell: &RenderCell| {
        let bg = aterm_render::rgb_to_u32(cell.bg);
        let fg = aterm_render::rgb_to_u32(cell.fg);
        let painted = aterm_render::floor_min_contrast_fg(fg, bg, min_contrast);
        (painted, bg)
    };
    let span = find_url_span(cells.len(), col, |i| {
        let cell = &cells[i];
        if cell.wide { ' ' } else { cell.ch }
    })?;
    let col = col.checked_sub(span.start)?;
    let cells = &cells[span];
    let span = find_url_span(cells.len(), col, |i| {
        let cell = &cells[i];
        let (fg, bg) = ink(cell);
        if cell.wide || fg == bg { ' ' } else { cell.ch }
    })?;
    let cells = &cells[span];
    let faint = cells
        .iter()
        .map(ink)
        .any(|(fg, bg)| aterm_render::floor_fg_contrast(fg, bg, FAINT_URL_CONTRAST) != fg);
    Some(PlainUrl { cells, faint })
}

fn row(text: &str, cols: u16) -> Vec<RenderCell> {
    let mut term = aterm_core::terminal::Terminal::new(4, cols);
    term.process(text.as_bytes());
    term.render_row(0)
}

fn signature(url: Option<PlainUrl<'_>>) -> Option<(String, bool)> {
    url.map(|url| (url.to_url_string(), url.faint))
}

fn recolor(cells: &mut [RenderCell], pattern: usize) {
    for (i, cell) in cells.iter_mut().enumerate() {
        match pattern {
            1 => cell.fg = [20, 20, 20],
            2 => {
                cell.fg = if i.is_multiple_of(2) {
                    [180, 180, 180]
                } else {
                    [230, 230, 230]
                }
            }
            3 => {
                cell.fg = [20, 20, 20];
                cell.bg = if i.is_multiple_of(2) {
                    [0, 0, 0]
                } else {
                    [20, 20, 20]
                };
            }
            4 => {
                cell.fg = [
                    (i * 19 % 256) as u8,
                    (i * 37 % 256) as u8,
                    (i * 71 % 256) as u8,
                ];
                cell.bg = [(i * 11 % 256) as u8, 0, 0];
            }
            5 if i.is_multiple_of(5) => cell.fg = cell.bg,
            6 if i.is_multiple_of(7) => cell.wide = true,
            _ => {}
        }
    }
}

#[test]
fn lookup_local_ink_reuse_preserves_each_color_boundary() {
    for text in [
        "Docs: https://example.com/path?q=1#tail end",
        "https://a/https://nested.example/path",
        "中 אב http://first.example/a https://second.example/y).",
        "Docs: \x1b[8mhttps://a/\x1b[28mhttps://visible.example/path",
        "no destination",
    ] {
        for cols in [8, 80, 512] {
            let base = row(text, cols);
            for pattern in 0..7 {
                let mut cells = base.clone();
                recolor(&mut cells, pattern);
                for contrast in [0.0, 1.0, 3.0, 4.5, 21.0, f32::NAN, f32::INFINITY] {
                    for col in (0..cells.len() + 2).chain([usize::MAX]) {
                        assert_eq!(
                            signature(super::plain_url_at(&cells, col, contrast)),
                            signature(uncached_url(&cells, col, contrast)),
                            "{text:?} cols={cols} pattern={pattern} contrast={contrast} col={col}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn equal_foregrounds_do_not_reuse_another_backgrounds_floor() {
    let mut cells = row("https://same.example/path", 80);
    for cell in &mut cells {
        cell.fg = [20, 20, 20];
        cell.bg = [0, 0, 0];
    }
    cells[8].bg = cells[8].fg;
    assert!(super::plain_url_at(&cells, 8, 4.5).is_none());
    cells[8].bg = [100, 100, 100];
    let found = super::plain_url_at(&cells, 8, 4.5).expect("visible URL");
    assert!(!found.faint, "each background needs its own contrast floor");
}

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: the output has the size/alignment of RUSAGE_INFO_V4; read only on success.
    let rc = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: a successful proc_pid_rusage initialized the output.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

#[test]
#[ignore = "manual paired URL ink instruction measurement"]
fn url_ink_cost() {
    use std::hint::black_box;
    type Probe = for<'a> fn(&'a [RenderCell], usize, f32) -> Option<PlainUrl<'a>>;
    let long = format!("https://example.com/{}", "x".repeat(400));
    for (name, text, pattern, col, contrast) in [
        ("bright", "https://example.com/path", 0, 0, 1.0),
        ("faint", "https://example.com/path", 1, 0, 1.0),
        ("floor", "https://example.com/path", 1, 0, 4.5),
        ("alternating-bright", "https://example.com/path", 2, 0, 1.0),
        ("alternating-floor", "https://example.com/path", 2, 0, 4.5),
        ("alternating-hidden", "https://example.com/path", 3, 0, 4.5),
        ("varied", "https://example.com/path", 4, 0, 1.0),
        ("varied-floor", "https://example.com/path", 4, 0, 4.5),
        ("long-bright", long.as_str(), 0, 10, 1.0),
        ("long-floor", long.as_str(), 1, 10, 4.5),
        ("none", "ordinary text without a destination", 0, 10, 1.0),
        ("outside", long.as_str(), 0, usize::MAX, 4.5),
        (
            "nested",
            "Docs: \x1b[8mhttps://a/\x1b[28mhttps://visible.example/path",
            0,
            25,
            1.0,
        ),
        ("infinite", "https://example.com/path", 1, 0, f32::INFINITY),
    ] {
        let mut cells = row(text, 1024);
        recolor(&mut cells, pattern);
        assert_eq!(
            signature(uncached_url(&cells, col, contrast)),
            signature(super::plain_url_at(&cells, col, contrast))
        );
        let mut probes: [(&str, Probe); 2] = [
            ("reference", uncached_url),
            ("candidate", super::plain_url_at),
        ];
        for (_, probe) in probes {
            for _ in 0..100 {
                black_box(probe(&cells, col, contrast));
            }
        }
        for sample in 0..5 {
            if sample != 0 {
                probes.swap(0, 1);
            }
            for (variant, probe) in probes {
                let (_, calls) = aterm_heap_count::allocations_in(|| probe(&cells, col, contrast));
                let before = instructions();
                for _ in 0..5_000 {
                    black_box(probe(
                        black_box(&cells),
                        black_box(col),
                        black_box(contrast),
                    ));
                }
                let count = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "url-ink case={name} cells={} col={col} variant={variant} sample={sample} iterations=5000 allocations={calls} instructions={count:?}",
                    cells.len()
                );
            }
        }
    }
}
