// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::RenderCell;

// Original parser and full-row visibility pipeline from main before indexed lookup.
fn original_parser(chars: &[char], col: usize) -> Option<PathSpan> {
    let at = *chars.get(col)?;
    let in_token = |ch: char| ch == WIDE_CONTINUATION || !(is_delimiter(ch) || ch.is_control());
    if !in_token(at) {
        return None;
    }
    let mut start = col;
    while start > 0 && in_token(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && in_token(chars[end]) {
        end += 1;
    }
    if end - start > MAX_TOKEN_CELLS {
        return None;
    }
    while start < end && chars[start] == WIDE_CONTINUATION {
        start += 1;
    }
    while end > start && is_trailing_punct(chars[end - 1]) {
        end -= 1;
    }
    if !(start..end).contains(&col) {
        return None;
    }
    let token: String = chars[start..end]
        .iter()
        .filter(|ch| **ch != WIDE_CONTINUATION)
        .collect();
    if token.contains("://") {
        return None;
    }
    let (path, line, col_no) = split_suffix(&token);
    if !looks_like_path(path) {
        return None;
    }
    Some(PathSpan {
        path: path.to_string(),
        line,
        col: col_no,
        start,
        end: end - 1,
    })
}

fn original_lookup(cells: &[RenderCell], col: usize, min_contrast: f32) -> Option<super::PathSpan> {
    let chars: Vec<char> = cells
        .iter()
        .map(|cell| {
            if cell.wide {
                return super::WIDE_CONTINUATION;
            }
            let bg = aterm_render::rgb_to_u32(cell.bg);
            let fg = aterm_render::rgb_to_u32(cell.fg);
            if aterm_render::floor_min_contrast_fg(fg, bg, min_contrast) == bg {
                ' '
            } else {
                cell.ch
            }
        })
        .collect();
    original_parser(&chars, col)
}

fn row(text: &str, cols: u16) -> Vec<RenderCell> {
    let mut term = aterm_core::terminal::Terminal::new(4, cols);
    term.process(text.as_bytes());
    term.render_row(0)
}

#[test]
fn indexed_lookup_matches_the_original_visible_pipeline() {
    for text in [
        "Docs: src/main.rs:12:3 end",
        "(資料/日本語.rs#L12C3-L15C6).",
        "src/one.rs:0 src/two.rs:4294967296",
        "https://example.com/src/main.rs",
        "src/good.rs\x1b[8m.evil.rs\x1b[28m",
        "\x1b[8msrc/hidden.rs\x1b[28m src/visible.rs:2",
        "ordinary text | [src/main.rs:12]",
        " \x1b#6src/wide.rs:3 ",
    ] {
        for cols in [8, 80, 512] {
            let base = row(text, cols);
            for pattern in 0..5 {
                let mut cells = base.clone();
                for (i, cell) in cells.iter_mut().enumerate() {
                    match pattern {
                        1 if i.is_multiple_of(5) => cell.fg = cell.bg,
                        2 => cell.fg = [20, 20, 20],
                        3 if i.is_multiple_of(7) => cell.wide = true,
                        4 => {
                            cell.fg = [(i * 19 % 256) as u8, (i * 37 % 256) as u8, 20];
                            cell.bg = [(i * 11 % 256) as u8, 0, 0];
                        }
                        _ => {}
                    }
                }
                let found: Vec<_> = (0..cells.len() + 2)
                    .chain([usize::MAX])
                    .map(|col| crate::plain_path_at(&cells, col))
                    .collect();
                for contrast in [0.0, 1.0, 3.0, 4.5, 21.0, f32::NAN, f32::INFINITY] {
                    for (col, expected) in (0..cells.len() + 2).chain([usize::MAX]).zip(&found) {
                        assert_eq!(
                            expected,
                            &original_lookup(&cells, col, contrast),
                            "{text:?} cols={cols} pattern={pattern} contrast={contrast} col={col}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn contrast_floor_preserves_ink_equality() {
    let mut state = 0x9e37_79b9u32;
    for i in 0..4096 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        let fg = state & 0xff_ffff;
        let bg = match i % 4 {
            0 => fg,
            1 => 0,
            2 => 0xff_ffff,
            _ => state.rotate_left(11) & 0xff_ffff,
        };
        for contrast in [
            f32::NEG_INFINITY,
            -1.0,
            0.0,
            1.0,
            1.01,
            3.0,
            4.5,
            21.0,
            f32::NAN,
            f32::INFINITY,
        ] {
            assert_eq!(
                fg == bg,
                aterm_render::floor_min_contrast_fg(fg, bg, contrast) == bg,
                "fg={fg:x} bg={bg:x} contrast={contrast}"
            );
        }
    }
}

#[test]
fn token_limit_applies_before_wide_and_punctuation_trimming() {
    for n in [1023, 1024, 1025, 4096] {
        let text = format!("{}.rs", "x".repeat(n - 3));
        let base: Vec<char> = text.chars().collect();
        for mutation in 0..3 {
            let mut chars = base.clone();
            match mutation {
                1 => chars[0] = WIDE_CONTINUATION,
                2 => chars[n - 1] = '.',
                _ => {}
            }
            for col in [0, 1, n / 2, n - 1, n, usize::MAX] {
                assert_eq!(
                    find_path_span(chars.len(), col, |i| chars[i]),
                    original_parser(&chars, col),
                    "n={n} mutation={mutation} col={col}"
                );
            }
        }
    }
}

#[test]
fn oversized_tokens_stop_after_a_bounded_number_of_cell_reads() {
    use std::cell::Cell;
    for col in [0, 10, 500_000, 999_999, 1_000_000, usize::MAX] {
        let reads = Cell::new(0usize);
        assert!(
            find_path_span(1_000_000, col, |_| {
                reads.set(reads.get() + 1);
                'x'
            })
            .is_none()
        );
        assert!(reads.get() <= 1030, "col={col}: {} reads", reads.get());
    }
}

#[test]
fn successful_lookup_has_one_buffer_and_rejections_have_none() {
    for (text, col, max, expected) in [
        ("src/example.rs", 2, 1, Some("src/example.rs")),
        ("src/example.rs:12:3", 2, 2, Some("src/example.rs")),
        ("資料/日本語.rs:12", 5, 2, Some("資料/日本語.rs")),
        (
            "src/x.rs#L1-abcdefghijklmnopqrstuvwxyz",
            2,
            2,
            Some("src/x.rs"),
        ),
        ("ordinary text", 8, 0, None),
        ("ordinary text", usize::MAX, 0, None),
    ] {
        let cells = row(text, 80);
        let (found, calls) = aterm_heap_count::allocations_in(|| crate::plain_path_at(&cells, col));
        assert!(calls <= max, "{text:?}: {calls} allocation requests");
        assert_eq!(found.as_ref().map(|span| span.path.as_str()), expected);
        if let Some(found) = found {
            assert_eq!(
                found.path.capacity(),
                found.path.len(),
                "suffix storage must not remain in the returned path"
            );
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

fn candidate(cells: &[RenderCell], col: usize, _contrast: f32) -> Option<PathSpan> {
    crate::plain_path_at(cells, col)
}

#[test]
#[ignore = "manual paired indexed path lookup instruction measurement"]
fn path_lookup_cost() {
    use std::hint::black_box;
    type Probe = fn(&[RenderCell], usize, f32) -> Option<PathSpan>;
    let long = format!("src/{}.rs:9", "x".repeat(500));
    let unicode = format!("資料/{}.rs:9", "名".repeat(200));
    let huge = "x".repeat(4096);
    let far = format!("{}src/main.rs:12", "a ".repeat(1800));
    let suffix = format!("x.rs#L1-{}", "x".repeat(900));
    for (name, text, col, contrast, faint) in [
        ("plain", "src/example.rs", 2, 1.0, false),
        ("line-col", "src/main.rs:12:3", 4, 1.0, false),
        ("unicode", "資料/日本語.rs:12", 5, 1.0, false),
        ("quoted", "(src/main.rs:12:3).", 4, 1.0, false),
        ("hash", "src/main.rs#L12C3-L15C6", 3, 1.0, false),
        ("long-ascii", long.as_str(), 250, 1.0, false),
        ("long-unicode", unicode.as_str(), 250, 1.0, false),
        ("blank", "src/main.rs:12:3 tail", 16, 1.0, false),
        ("word", "ordinary", 2, 1.0, false),
        ("outside", huge.as_str(), 5000, 4.5, false),
        ("huge-word", huge.as_str(), 2000, 1.0, false),
        ("far-path", far.as_str(), 3604, 1.0, false),
        ("floor", "src/main.rs:12:3", 4, 4.5, true),
        (
            "hidden-suffix",
            "src/good.rs\x1b[8m.evil.rs\x1b[28m",
            8,
            1.0,
            false,
        ),
        ("url-reject", "https://example.com/a.rs", 5, 1.0, false),
        ("suffix-slack", suffix.as_str(), 0, 1.0, false),
    ] {
        let mut cells = row(text, 8192);
        if faint {
            for cell in &mut cells {
                cell.fg = [20, 20, 20];
            }
        }
        assert_eq!(
            original_lookup(&cells, col, contrast),
            candidate(&cells, col, contrast)
        );
        let mut probes: [(&str, Probe); 2] =
            [("reference", original_lookup), ("candidate", candidate)];
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
                let (found, calls) =
                    aterm_heap_count::allocations_in(|| probe(&cells, col, contrast));
                let (path_len, capacity) = found
                    .as_ref()
                    .map_or((0, 0), |s| (s.path.len(), s.path.capacity()));
                let before = instructions();
                for _ in 0..1000 {
                    black_box(probe(
                        black_box(&cells),
                        black_box(col),
                        black_box(contrast),
                    ));
                }
                let count = before.zip(instructions()).map(|(a, b)| b.saturating_sub(a));
                println!(
                    "path-lookup case={name} cells={} col={col} variant={variant} sample={sample} iterations=1000 allocations={calls} path_len={path_len} capacity={capacity} instructions={count:?}",
                    cells.len()
                );
            }
        }
    }
}
