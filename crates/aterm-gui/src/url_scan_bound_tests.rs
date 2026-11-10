// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::RenderCell;

// The indexed scanner from main before bounding candidate starts by the query.
fn unbounded_span(
    n: usize,
    col: usize,
    char_at: impl Fn(usize) -> char,
) -> Option<std::ops::Range<usize>> {
    let is_url = |c: char| c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c);
    let mut i = 0;
    while i < n {
        let prefix = if n - i >= 7
            && char_at(i) == 'h'
            && char_at(i + 1) == 't'
            && char_at(i + 2) == 't'
            && char_at(i + 3) == 'p'
        {
            match char_at(i + 4) {
                ':' if char_at(i + 5) == '/' && char_at(i + 6) == '/' => Some(7),
                's' if n - i >= 8
                    && char_at(i + 5) == ':'
                    && char_at(i + 6) == '/'
                    && char_at(i + 7) == '/' =>
                {
                    Some(8)
                }
                _ => None,
            }
        } else {
            None
        };
        if let Some(prefix) = prefix {
            let mut j = i + prefix;
            while j < n && is_url(char_at(j)) {
                j += 1;
            }
            let mut end = j;
            while end > i
                && matches!(
                    char_at(end - 1),
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

fn unbounded_url(
    cells: &[RenderCell],
    col: usize,
    min_contrast: f32,
) -> Option<super::PlainUrl<'_>> {
    let ink = |cell: &RenderCell| {
        let bg = aterm_render::rgb_to_u32(cell.bg);
        let fg = aterm_render::rgb_to_u32(cell.fg);
        (
            aterm_render::floor_min_contrast_fg(fg, bg, min_contrast),
            bg,
        )
    };
    let span = unbounded_span(cells.len(), col, |i| {
        let cell = &cells[i];
        if cell.wide { ' ' } else { cell.ch }
    })?;
    let col = col.checked_sub(span.start)?;
    let cells = &cells[span];
    let span = unbounded_span(cells.len(), col, |i| {
        let cell = &cells[i];
        let (fg, bg) = ink(cell);
        if cell.wide || fg == bg { ' ' } else { cell.ch }
    })?;
    let cells = &cells[span];
    let faint = cells
        .iter()
        .map(ink)
        .any(|(fg, bg)| aterm_render::floor_fg_contrast(fg, bg, super::FAINT_URL_CONTRAST) != fg);
    Some(super::PlainUrl { cells, faint })
}

fn row(text: &str, cols: u16) -> Vec<RenderCell> {
    let mut term = aterm_core::terminal::Terminal::new(4, cols);
    term.process(text.as_bytes());
    term.render_row(0)
}

fn signature(url: Option<super::PlainUrl<'_>>) -> Option<(String, bool)> {
    url.map(|url| (url.to_url_string(), url.faint))
}

#[test]
fn prefix_work_is_bounded_by_the_query_even_on_long_rows() {
    use std::cell::Cell;
    for col in [0, 10, 4096, usize::MAX] {
        let reads = Cell::new(0usize);
        let found = super::find_url_span(4096, col, |_| {
            reads.set(reads.get() + 1);
            'a'
        });
        assert!(found.is_none());
        assert!(
            reads.get() <= 64,
            "col={col}: {} character reads",
            reads.get()
        );
    }
}

#[test]
fn a_query_keeps_the_whole_destination_to_its_right() {
    let cells = row("Docs: https://example.com/path?q=long#tail end", 80);
    for col in 6..14 {
        assert_eq!(
            signature(super::plain_url_at(&cells, col, 1.0)),
            Some(("https://example.com/path?q=long#tail".to_owned(), false))
        );
    }
}

#[test]
fn bounded_queries_match_the_unbounded_pipeline() {
    let long = format!("{} https://late.example/path", "a".repeat(950));
    for text in [
        "",
        "ordinary text",
        "http:// https:// https:/",
        "Docs: https://example.com/x). http://second.example/y end",
        "中 אב https://unicode.example/path",
        "Docs: \x1b[8mhttps://a/\x1b[28mhttps://visible.example/x",
        "Docs: https://good.example\x1b[8m.evil.example/x\x1b[28m",
        long.as_str(),
    ] {
        for cols in [8, 80, 2048] {
            let cells = row(text, cols);
            for contrast in [0.0, 1.0, 4.5] {
                for col in (0..cells.len() + 2).chain([usize::MAX]) {
                    assert_eq!(
                        signature(super::plain_url_at(&cells, col, contrast)),
                        signature(unbounded_url(&cells, col, contrast)),
                        "{text:?} cols={cols} contrast={contrast} col={col}"
                    );
                }
            }
        }
    }
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
#[ignore = "manual paired URL query-bound instruction measurement"]
fn url_query_bound_cost() {
    use std::hint::black_box;
    type Probe = for<'a> fn(&'a [RenderCell], usize, f32) -> Option<super::PlainUrl<'a>>;
    let long_none = "a".repeat(1024);
    let late = format!("{} https://late.example/path", "a".repeat(950));
    for (name, text, cols, col, contrast, min_cells) in [
        (
            "short-before",
            "ordinary text with no destination",
            80,
            0,
            1.0,
            1,
        ),
        ("long-left", long_none.as_str(), 2048, 0, 1.0, 1024),
        ("long-early", long_none.as_str(), 2048, 10, 1.0, 1024),
        ("long-middle", long_none.as_str(), 2048, 512, 1.0, 1024),
        ("long-tail", long_none.as_str(), 2048, 1023, 1.0, 1024),
        ("blank-right", long_none.as_str(), 2048, 1500, 1.0, 1024),
        ("late-before", late.as_str(), 2048, 10, 1.0, 950),
        ("late-inside", late.as_str(), 2048, 960, 1.0, 950),
        ("bright-start", "https://example.com/path", 80, 0, 1.0, 1),
        (
            "faint-start",
            "\x1b[38;2;20;20;20mhttps://example.com/path",
            80,
            0,
            1.0,
            1,
        ),
        (
            "faint-floor",
            "\x1b[38;2;20;20;20mhttps://example.com/path",
            80,
            0,
            4.5,
            1,
        ),
        (
            "nested",
            "Docs: \x1b[8mhttps://a/\x1b[28mhttps://visible.example/x",
            80,
            25,
            1.0,
            1,
        ),
        (
            "out-of-range",
            long_none.as_str(),
            2048,
            usize::MAX,
            1.0,
            1024,
        ),
        ("empty", "", 80, 0, 1.0, 0),
    ] {
        let cells = row(text, cols);
        assert!(cells.len() >= min_cells);
        assert_eq!(
            signature(unbounded_url(&cells, col, contrast)),
            signature(super::plain_url_at(&cells, col, contrast))
        );
        let mut probes: [(&str, Probe); 2] = [
            ("reference", unbounded_url),
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
                    "url-query-bound case={name} cells={} col={col} variant={variant} sample={sample} iterations=5000 allocations={calls} instructions={count:?}",
                    cells.len()
                );
            }
        }
    }
}
