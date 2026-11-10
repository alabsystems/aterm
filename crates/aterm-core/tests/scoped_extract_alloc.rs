// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Damage-scoped refill allocates nothing once warm and stays on the scoped arm.

use aterm_bench::allocator::{Observed, Observer};
use std::alloc::System;
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOC_CALLS: Cell<u64> = const { Cell::new(0) };
    static ALLOC_BYTES: Cell<u64> = const { Cell::new(0) };
    static RETAINED_DELTA: Cell<i128> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: constant, drop-free thread-locals and wrapping arithmetic neither allocate nor unwind.
unsafe impl Observer for Counting {
    fn retained(&self, old: usize, new: usize) {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = RETAINED_DELTA.try_with(|n| {
                    n.set(n.get().wrapping_add(new as i128).wrapping_sub(old as i128));
                });
            }
        });
    }

    fn requested(&self, size: usize) {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = ALLOC_CALLS.try_with(|n| n.set(n.get().wrapping_add(1)));
                let _ = ALLOC_BYTES.try_with(|n| n.set(n.get().wrapping_add(size as u64)));
            }
        });
    }
}

#[global_allocator]
static ALLOC: Observed<System, Counting> = Observed::new(System, Counting);

fn allocs_in<R>(f: impl FnOnce() -> R) -> (R, u64) {
    ALLOC_CALLS.with(|n| n.set(0));
    ALLOC_BYTES.with(|n| n.set(0));
    RETAINED_DELTA.with(|n| n.set(0));
    ACTIVE.with(|a| a.set(true));
    let r = f();
    ACTIVE.with(|a| a.set(false));
    (r, ALLOC_CALLS.with(Cell::get))
}

fn echo(i: usize) -> &'static [u8] {
    const LETTERS: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
    if i % 70 == 69 {
        b"\r\x1b[K"
    } else {
        let k = i % 26;
        &LETTERS[k..=k]
    }
}

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::uninit();
    // SAFETY: the output has the size/alignment of RUSAGE_INFO_V4; only read on success.
    let result = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as i32,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast(),
        )
    };
    // SAFETY: proc_pid_rusage initialized info when it returned success.
    (result == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

#[test]
#[ignore = "manual before/after live row extraction measurement; run alone"]
fn live_row_extraction_cost() {
    use aterm_core::terminal::{SearchRows, Terminal};
    // Separate records from libtest's unfinished status line under --nocapture.
    eprintln!();
    const REPEATS: usize = 256;
    for cols in [80u16, 4096] {
        for shape in [
            "empty",
            "short",
            "short-space",
            "ascii",
            "cjk",
            "combining",
            "dense-mark",
            "dense-mark-padded",
            "dense-mark-full",
            "mixed-mark-suffix",
            "blank",
            "padded",
            "wide-padded",
            "emoji-padded",
            "space-mark",
            "space-vs",
            "ideographic-space",
            "nbsp",
        ] {
            let text = match shape {
                "short" => "x".to_string(),
                "short-space" => "x ".to_string(),
                "ascii" => "x".repeat(usize::from(cols)),
                "cjk" => "日".repeat(usize::from(cols) / 2),
                "combining" => "e\u{301}".repeat(usize::from(cols)),
                "dense-mark" => format!("x{}", "\u{301}".repeat(16)),
                "dense-mark-padded" => format!(
                    "x{}{}",
                    "\u{301}".repeat(16),
                    " ".repeat(usize::from(cols) - 1)
                ),
                "dense-mark-full" => format!("x{}", "\u{301}".repeat(16)).repeat(usize::from(cols)),
                "mixed-mark-suffix" => {
                    format!("x{}{}y", "\u{1d185}".repeat(14), "\u{20dd}".repeat(2))
                }
                "blank" => " ".repeat(usize::from(cols)),
                "padded" => format!("x{}", " ".repeat(usize::from(cols) - 1)),
                "wide-padded" => format!("日{}", " ".repeat(usize::from(cols) - 2)),
                "emoji-padded" => format!("🦀{}", " ".repeat(usize::from(cols) - 2)),
                "space-mark" => format!(" \u{301}{}", " ".repeat(usize::from(cols) - 1)),
                "space-vs" => format!(" \u{fe0f}{}", " ".repeat(usize::from(cols) - 1)),
                "ideographic-space" => "\u{3000}".repeat(usize::from(cols) / 2),
                "nbsp" => "\u{a0}".repeat(usize::from(cols)),
                _ => String::new(),
            };
            let mut terminal = Terminal::new(24, cols);
            terminal.process(text.as_bytes());
            let expected = terminal.written_line_cells(0).unwrap();
            assert_eq!(expected.0, text);
            for trim in [false, true] {
                let before = instructions();
                let (_, calls) = allocs_in(|| {
                    for _ in 0..REPEATS {
                        if trim {
                            std::hint::black_box(terminal.get_line_text(0, None).unwrap());
                        } else {
                            std::hint::black_box(terminal.written_line_cells(0).unwrap());
                        }
                    }
                });
                let bytes = ALLOC_BYTES.with(Cell::get);
                let retired = before.zip(instructions()).map(|(a, b)| b - a);
                assert_eq!(terminal.get_line_text(0, None).unwrap(), text.trim_end());
                eprintln!(
                    "live_row cols={cols} shape={shape} trim={trim} repeats={REPEATS} instructions={retired:?} allocation_calls={calls} allocated_bytes={bytes}"
                );
            }
            let ansi = terminal.grid().row_ansi_text_screen(0).unwrap();
            let before = instructions();
            let (_, calls) = allocs_in(|| {
                for _ in 0..REPEATS {
                    std::hint::black_box(terminal.grid().row_ansi_text_screen(0).unwrap());
                }
            });
            let bytes = ALLOC_BYTES.with(Cell::get);
            let retired = before.zip(instructions()).map(|(a, b)| b - a);
            assert_eq!(terminal.grid().row_ansi_text_screen(0).unwrap(), ansi);
            eprintln!(
                "ansi_row cols={cols} shape={shape} repeats={REPEATS} instructions={retired:?} allocation_calls={calls} allocated_bytes={bytes}"
            );
            for history in [false, true] {
                if history {
                    terminal.process("\r\n".repeat(24).as_bytes());
                    terminal.scroll_to_top();
                }
                let expected = terminal.row_text(0).unwrap();
                assert_eq!(expected.trim_end(), text.trim_end());
                for reused in [false, true] {
                    let mut out = expected.clone();
                    if !history && !reused && matches!(shape, "short" | "short-space") {
                        let before = instructions();
                        let (_, calls) = allocs_in(|| {
                            for _ in 0..REPEATS {
                                let mut control = String::with_capacity(8);
                                control.push_str(&expected);
                                std::hint::black_box(control);
                            }
                        });
                        let bytes = ALLOC_BYTES.with(Cell::get);
                        let retired = before.zip(instructions()).map(|(a, b)| b - a);
                        eprintln!(
                            "allocator_control cols={cols} shape={shape} repeats={REPEATS} instructions={retired:?} allocation_calls={calls} allocated_bytes={bytes}"
                        );
                    }
                    let before = instructions();
                    let (_, calls) = allocs_in(|| {
                        for _ in 0..REPEATS {
                            if reused {
                                assert!(terminal.row_text_into(0, &mut out));
                                std::hint::black_box(&out);
                            } else {
                                std::hint::black_box(terminal.row_text(0).unwrap());
                            }
                        }
                    });
                    let bytes = ALLOC_BYTES.with(Cell::get);
                    let retired = before.zip(instructions()).map(|(a, b)| b - a);
                    assert_eq!(out, expected);
                    eprintln!(
                        "viewport_row cols={cols} shape={shape} history={history} reused={reused} repeats={REPEATS} instructions={retired:?} allocation_calls={calls} allocated_bytes={bytes}"
                    );
                }
            }
        }
        let terminal = Terminal::new(24, cols);
        let mut rows = SearchRows::with_capacity(24);
        let before = instructions();
        let (_, calls) = allocs_in(|| terminal.read_search_rows(0, 24, &mut rows));
        let bytes = ALLOC_BYTES.with(Cell::get);
        let retired = before.zip(instructions()).map(|(a, b)| b - a);
        assert!(rows.text.iter().all(String::is_empty));
        eprintln!(
            "live_snapshot cols={cols} rows=24 instructions={retired:?} allocation_calls={calls} allocated_bytes={bytes}"
        );
    }
}

#[test]
#[ignore = "manual styled ANSI allocation/instruction measurement; run alone"]
fn styled_ansi_emission_cost() {
    use aterm_core::terminal::Terminal;
    // Separate records from libtest's unfinished status line under --nocapture.
    eprintln!();
    const REPEATS: usize = 16;
    for cols in [80u16, 4096] {
        for rows in [1u16, 24] {
            for shape in [
                "plain",
                "uniform",
                "indexed",
                "rgb",
                "attributes",
                "underline",
                "wide-mark",
            ] {
                let mut input = String::new();
                for row in 0..rows {
                    if row > 0 {
                        input.push_str("\r\n");
                    }
                    let width = if shape == "wide-mark" { 2 } else { 1 };
                    for col in 0..cols / width {
                        let odd = col % 2 == 1;
                        input.push_str(match shape {
                            "uniform" => "\x1b[1;38;5;17m",
                            "indexed" | "wide-mark" if odd => "\x1b[38;5;255;48;5;9m",
                            "indexed" | "wide-mark" => "\x1b[38;5;2;48;5;18m",
                            "rgb" if odd => "\x1b[38;2;255;128;64;48;2;1;2;3m",
                            "rgb" => "\x1b[38;2;12;34;56;48;2;78;90;123m",
                            "attributes" if odd => "\x1b[0;1;3;4:3;5;7;9;53m",
                            "attributes" => "\x1b[0;2;4:2;8;73m",
                            "underline" if odd => "\x1b[4:3;58:2::255:128:64m",
                            "underline" => "\x1b[4:2;58:5:9m",
                            _ => "",
                        });
                        input.push_str(if shape == "wide-mark" {
                            "日\u{301}"
                        } else {
                            "x"
                        });
                    }
                }
                let mut terminal = Terminal::new(rows, cols);
                terminal.process(input.as_bytes());
                let expected: Vec<_> = (0..rows)
                    .map(|row| terminal.grid().row_ansi_text_screen(row).unwrap())
                    .collect();
                for mode in ["row", "frame", "restore"] {
                    let emit = || match mode {
                        "row" => terminal.grid().row_ansi_text_screen(0).unwrap(),
                        "frame" => terminal.serialize_ansi(usize::from(rows), Some(0)),
                        _ => terminal.serialize_restore_ansi(),
                    };
                    let before = instructions();
                    let (_, calls) = allocs_in(|| {
                        for _ in 0..REPEATS {
                            std::hint::black_box(emit());
                        }
                    });
                    let bytes = ALLOC_BYTES.with(Cell::get);
                    let retired = before.zip(instructions()).map(|(a, b)| b - a);
                    if mode != "row" {
                        let mut replay = Terminal::new(rows, cols);
                        replay.process(emit().as_bytes());
                        assert_same_styled_cells(&terminal, &replay, rows, cols);
                        for row in 0..rows {
                            assert_eq!(
                                replay.grid().row_ansi_text_screen(row).unwrap(),
                                expected[usize::from(row)]
                            );
                        }
                        if mode == "restore" {
                            terminal.process(b"\r\nQ");
                            replay.process(b"\r\nQ");
                            assert_same_styled_cells(&terminal, &replay, rows, cols);
                        }
                    }
                    eprintln!(
                        "styled_ansi cols={cols} rows={rows} shape={shape} mode={mode} repeats={REPEATS} instructions={retired:?} allocation_calls={calls} allocated_bytes={bytes}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "manual numeric SGR formatting measurement; run alone"]
fn sgr_byte_formatting_cost() {
    use std::fmt::Write as _;
    eprintln!();
    const REPEATS: usize = 4096;
    let expected: String = (0..=u8::MAX).map(|value| value.to_string()).collect();
    for mode in ["fmt", "byte"] {
        let mut out = String::with_capacity(768);
        let before = instructions();
        let (_, calls) = allocs_in(|| {
            for _ in 0..REPEATS {
                out.clear();
                if mode == "fmt" {
                    for value in 0..=u8::MAX {
                        let _ = write!(out, "{}", std::hint::black_box(value));
                    }
                } else {
                    for value in 0..=u8::MAX {
                        aterm_grid::push_sgr_byte(&mut out, std::hint::black_box(value));
                    }
                }
                std::hint::black_box(&out);
            }
        });
        let bytes = ALLOC_BYTES.with(Cell::get);
        let retired = before.zip(instructions()).map(|(a, b)| b - a);
        assert_eq!(out, expected);
        assert_eq!(calls, 0);
        eprintln!(
            "sgr_number mode={mode} values=256 repeats={REPEATS} instructions={retired:?} allocation_calls={calls} allocated_bytes={bytes}"
        );
    }
}

#[test]
fn every_sgr_byte_appends_without_allocating_into_reserved_text() {
    let prefix = "日prefix  ";
    let mut out = String::with_capacity(32);
    out.push_str(prefix);
    for value in 0..=u8::MAX {
        out.truncate(prefix.len());
        let (_, calls) = allocs_in(|| aterm_grid::push_sgr_byte(&mut out, value));
        assert_eq!(calls, 0, "SGR byte {value}");
        assert_eq!(out, format!("{prefix}{value}"));
    }
}

fn assert_same_styled_cells(
    original: &aterm_core::terminal::Terminal,
    replay: &aterm_core::terminal::Terminal,
    rows: u16,
    cols: u16,
) {
    for row in 0..rows {
        for col in 0..cols {
            let a = original.grid().cell(row, col).unwrap();
            let b = replay.grid().cell(row, col).unwrap();
            assert_eq!(a.flags(), b.flags(), "flags at {row}:{col}");
            assert_eq!(a.colors(), b.colors(), "colors at {row}:{col}");
            assert_eq!(
                original.grid().fg_rgb_at(row, col),
                replay.grid().fg_rgb_at(row, col)
            );
            assert_eq!(
                original.grid().bg_rgb_at(row, col),
                replay.grid().bg_rgb_at(row, col)
            );
            assert_eq!(
                original
                    .grid()
                    .cell_extra(row, col)
                    .and_then(|extra| extra.underline_color_u32()),
                replay
                    .grid()
                    .cell_extra(row, col)
                    .and_then(|extra| extra.underline_color_u32()),
                "underline color at {row}:{col}",
            );
            assert_eq!(
                original.cell_grapheme(usize::from(row), usize::from(col)),
                replay.cell_grapheme(usize::from(row), usize::from(col))
            );
        }
    }
}

#[test]
fn ten_thousand_one_row_echoes_extract_with_zero_allocations_after_warm_up() {
    use aterm_core::render::{FrameRefill, RenderInput};
    use aterm_core::terminal::Terminal;

    let (rows, cols) = (24usize, 80usize);
    let mut term = Terminal::new(24, 80);
    let mut scratch = RenderInput::empty();
    term.process(b"$ ");
    let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
    for i in 0..140 {
        term.process(echo(i));
        let _ = term.cell_frame_damage_scoped_into(&mut scratch, rows, cols);
    }

    let mut scoped = 0usize;
    let mut extraction_allocs = 0u64;
    for i in 140..140 + 10_000 {
        term.process(echo(i));
        let (refill, n) =
            allocs_in(|| term.cell_frame_damage_scoped_into(&mut scratch, rows, cols));
        extraction_allocs += n;
        match refill {
            FrameRefill::Scoped { rows_refilled } => {
                assert!(
                    rows_refilled <= 1,
                    "a one-row echo refilled {rows_refilled} rows at keystroke {i}"
                );
                scoped += 1;
            }
            FrameRefill::Full { cause } => {
                panic!("a one-row echo fell back to a full refill ({cause:?}) at keystroke {i}")
            }
        }
    }
    assert_eq!(scoped, 10_000, "every echo frame must take the scoped arm");
    assert_eq!(
        extraction_allocs, 0,
        "10,000 one-row echo extractions made {extraction_allocs} heap allocations after \
         warm-up — the damage-scoped refill must reuse its row mask and row buffers"
    );
}

#[test]
fn empty_written_rows_and_snapshots_allocate_nothing() {
    use aterm_core::terminal::{SearchRows, Terminal};
    for cols in [80, 4096] {
        let terminal = Terminal::new(24, cols);
        let mut rows = SearchRows::with_capacity(24);
        let (row, calls) = allocs_in(|| terminal.written_line_cells(0).unwrap());
        assert!(row.0.is_empty());
        assert_eq!(calls, 0, "empty row at width {cols}");
        let (text, calls) = allocs_in(|| terminal.get_line_text(0, None).unwrap());
        assert!(text.is_empty());
        assert_eq!(calls, 0, "empty trimmed row at width {cols}");
        let (_, calls) = allocs_in(|| terminal.read_search_rows(0, 24, &mut rows));
        assert!(rows.text.iter().all(String::is_empty));
        assert_eq!(calls, 0, "empty snapshot at width {cols}");
    }
}

#[test]
fn trimming_printed_plain_blanks_allocates_nothing_but_written_keeps_them() {
    use aterm_core::terminal::Terminal;
    for cols in [80, 4096] {
        for blank in [" ", "\u{a0}", "\u{3000}"] {
            let width = if blank == "\u{3000}" { 2 } else { 1 };
            let text = blank.repeat(usize::from(cols) / width);
            let mut terminal = Terminal::new(24, cols);
            terminal.process(text.as_bytes());
            let (trimmed, calls) = allocs_in(|| terminal.get_line_text(0, None).unwrap());
            assert!(trimmed.is_empty());
            assert_eq!(calls, 0, "{blank:?} width={cols}");
            assert_eq!(terminal.written_line_cells(0).unwrap().0, text);
        }
    }
}

#[test]
fn a_dense_combining_cell_grows_its_text_at_most_once_after_initial_reservation() {
    use aterm_core::terminal::Terminal;
    for mark in ['\u{301}', '\u{20dd}', '\u{1d185}'] {
        for prefix in [0, 1, 7, 15, 31, 63] {
            for count in 1..=16 {
                let text = format!("{}e{}", "x".repeat(prefix), mark.to_string().repeat(count));
                let mut terminal = Terminal::new(24, 80);
                terminal.process(text.as_bytes());
                let (actual, calls) = allocs_in(|| terminal.written_line_text(0).unwrap());
                assert_eq!(actual, text);
                assert!(
                    calls <= 2,
                    "{mark:?} prefix={prefix} count={count}: {calls} allocations"
                );
                let (actual, calls) = allocs_in(|| terminal.get_line_text(0, None).unwrap());
                assert_eq!(actual, text);
                assert!(
                    calls <= 2,
                    "trim {mark:?} prefix={prefix} count={count}: {calls} allocations"
                );
                let (actual, calls) = allocs_in(|| terminal.row_text(0).unwrap());
                assert_eq!(actual, text);
                assert!(
                    calls <= 2,
                    "viewport {mark:?} prefix={prefix} count={count}: {calls} allocations"
                );
                let mut reused = actual;
                let (valid, calls) = allocs_in(|| terminal.row_text_into(0, &mut reused));
                assert!(valid);
                assert_eq!(reused, text);
                assert_eq!(calls, 0, "reused viewport text");
            }
        }
    }
    let text = format!("x{}{}y", "\u{1d185}".repeat(14), "\u{20dd}".repeat(2));
    let mut terminal = Terminal::new(24, 80);
    terminal.process(text.as_bytes());
    let (actual, calls) = allocs_in(|| terminal.written_line_text(0).unwrap());
    assert_eq!(actual, text);
    assert!(
        calls <= 2,
        "mixed marks followed by ASCII: {calls} allocations"
    );
}

#[test]
fn styled_ansi_grows_only_its_output_and_appends_without_temporary_allocations() {
    use aterm_core::terminal::Terminal;
    for cols in [80u16, 4096] {
        let mut input = String::new();
        for col in 0..cols {
            input.push_str(if col % 2 == 0 {
                "\x1b[0;1;3;4:3;38;2;255;128;64;48;5;17;58:5:9m"
            } else {
                "\x1b[0;2;4:2;8;73;38;2;12;34;56;48;5;255;58:2::1:2:3m"
            });
            input.push_str("e\u{301}");
        }
        let mut terminal = Terminal::new(24, cols);
        terminal.process(input.as_bytes());
        let (expected, calls) = allocs_in(|| terminal.grid().row_ansi_text_screen(0).unwrap());
        assert!(calls <= 12, "styled row width {cols}: {calls} allocations");
        let mut replay = Terminal::new(24, cols);
        replay.process(expected.as_bytes());
        assert_same_styled_cells(&terminal, &replay, 1, cols);
        let mut out = String::with_capacity(expected.len() + 8);
        out.push_str("prefix  ");
        let (valid, calls) = allocs_in(|| terminal.grid().push_row_ansi_text_screen(0, &mut out));
        assert!(valid);
        assert_eq!(calls, 0, "reserved styled append");
        assert_eq!(out.strip_prefix("prefix  "), Some(expected.as_str()));
        let (valid, calls) = allocs_in(|| {
            terminal
                .grid()
                .push_row_ansi_text_screen(u16::MAX, &mut out)
        });
        assert!(!valid);
        assert_eq!(calls, 0, "absent row append");
        assert_eq!(out.strip_prefix("prefix  "), Some(expected.as_str()));
        let mut restored = Terminal::new(24, cols);
        restored.process(terminal.serialize_restore_ansi().as_bytes());
        terminal.process(b"\r\nQ");
        restored.process(b"\r\nQ");
        assert_same_styled_cells(&terminal, &restored, 2, cols);
    }
}

/// Local component measurement; opt-test timings are not pointer-to-glass latency.
#[test]
#[ignore = "manual BiDi pointer mapping allocation and CPU measurement"]
fn bidi_pointer_mapping_cost() {
    use aterm_core::terminal::Terminal;
    use std::hint::black_box;
    use std::time::Instant;

    for (name, text) in [
        ("latin", "plain ASCII row"),
        ("mixed", "abc אב 12 ג def"),
        ("wide", "中 אב (12) 🦀 xyz"),
        ("arabic", "abc مرحبا 123 xyz"),
    ] {
        let mut term = Terminal::new(4, 80);
        term.process(text.as_bytes());
        let (_, calls) = allocs_in(|| black_box(term.viewport_cell_logical(0, 5)));
        for sample in 0..5 {
            let before = instructions();
            #[expect(
                clippy::disallowed_methods,
                reason = "manual benchmark wall-clock boundary"
            )]
            let start = Instant::now();
            for i in 0..20_000u16 {
                black_box(term.viewport_cell_logical(0, black_box(i % 80)));
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "manual benchmark wall-clock boundary"
            )]
            let elapsed = start.elapsed();
            let instructions = before
                .zip(instructions())
                .map(|(before, after)| after.saturating_sub(before));
            println!(
                "bidi-pointer case={name} sample={sample} calls={calls} iterations=20000 instructions={instructions:?} ms={:.6}",
                elapsed.as_secs_f64() * 1000.0
            );
        }
    }
}

#[test]
fn bidi_pointer_mapping_does_not_allocate_two_resolvers() {
    use aterm_core::terminal::Terminal;

    for text in ["abc אב 12 ג def", "中 אב (12) 🦀 xyz", "abc مرحبا 123 xyz"] {
        let mut term = Terminal::new(4, 80);
        term.process(text.as_bytes());
        let (_, calls) = allocs_in(|| term.viewport_cell_logical(0, 5));
        // One resolution needs at most 33 calls here; the duplicate-resolution
        // implementation needs at least 47. Leave room for allocator growth changes.
        assert!(calls < 40, "{text:?}: {calls} allocations");
    }
}

fn check_bidi_map_allocations(enforce: bool) {
    use aterm_bidi::{
        BaseDirection, Scratch, bidi_class, bracket_of, reorder_cells_with_classes_into,
    };

    for n in [0, 1, 4, 5, 80, 257] {
        for paired in [false, true] {
            let chars: Vec<_> = (0..n)
                .map(|i| ['א', 'ב', ' ', '(', '1', '2', ')', '中'][i % 8])
                .collect();
            let classes: Vec<_> = chars.iter().copied().map(bidi_class).collect();
            let brackets: Vec<_> = chars.iter().copied().map(bracket_of).collect();
            let wide: Vec<_> = (0..n).map(|i| paired && i % 2 == 1).collect();
            let mut scratch = Scratch::default();
            let mut expected = Vec::new();
            reorder_cells_with_classes_into(
                &classes,
                &brackets,
                &wide,
                BaseDirection::Auto,
                &mut scratch,
                &mut expected,
            );
            let (mut out, cold) = allocs_in(|| {
                let mut out = Vec::new();
                reorder_cells_with_classes_into(
                    &classes,
                    &brackets,
                    &wide,
                    BaseDirection::Auto,
                    &mut scratch,
                    &mut out,
                );
                out
            });
            let bytes = ALLOC_BYTES.with(Cell::get);
            assert_eq!(out, expected);
            let (_, warm) = allocs_in(|| {
                reorder_cells_with_classes_into(
                    &classes,
                    &brackets,
                    &wide,
                    BaseDirection::Auto,
                    &mut scratch,
                    &mut out,
                )
            });
            assert_eq!(out, expected);
            let mut seen = out.clone();
            seen.sort_unstable();
            assert_eq!(
                seen,
                (0..n).collect::<Vec<_>>(),
                "every input cell survives once"
            );
            println!(
                "bidi-map cells={n} paired={paired} cold={cold} warm={warm} requested_bytes={bytes} capacity={}",
                out.capacity()
            );
            if enforce {
                assert!(
                    cold <= 1,
                    "cells={n} paired={paired}: output map grew {cold} times"
                );
                assert_eq!(warm, 0, "reused output and scratch");
            }
        }
    }
}

#[test]
fn bidi_map_allocates_once_for_a_known_cell_count() {
    check_bidi_map_allocations(true);
}

#[test]
#[ignore = "manual cold and warm BiDi output allocation measurement"]
fn bidi_map_allocation_measurement() {
    check_bidi_map_allocations(false);
}

#[test]
#[ignore = "manual BiDi output capacity measurement across changing row widths"]
fn bidi_map_resize_measurement() {
    use aterm_bidi::{BaseDirection, BidiClass, Bracket, Scratch, reorder_cells_with_classes_into};

    let classes = vec![BidiClass::R; 257];
    let brackets = vec![Bracket::None; 257];
    let wide = vec![false; 257];
    let mut scratch = Scratch::default();
    let mut initial = Vec::new();
    reorder_cells_with_classes_into(
        &classes,
        &brackets,
        &wide,
        BaseDirection::Auto,
        &mut scratch,
        &mut initial,
    );
    let mut out = Vec::new();
    for n in [80, 100, 80, 257, 80] {
        let (_, calls) = allocs_in(|| {
            reorder_cells_with_classes_into(
                &classes[..n],
                &brackets[..n],
                &wide[..n],
                BaseDirection::Auto,
                &mut scratch,
                &mut out,
            )
        });
        let bytes = ALLOC_BYTES.with(Cell::get);
        assert_eq!(out.len(), n);
        assert!(out.capacity() >= n);
        println!(
            "bidi-map-resize cells={n} calls={calls} requested_bytes={bytes} capacity={}",
            out.capacity()
        );
    }
}

#[test]
#[ignore = "manual BiDi scratch allocations, retained layouts and optimized instruction measurement"]
fn bidi_scratch_cost() {
    use aterm_bidi::{
        BaseDirection, Scratch, bidi_class, bracket_of, reorder_cells_with_classes_into,
    };
    use std::hint::black_box;

    for n in [80, 257] {
        for paired in [false, true] {
            let chars: Vec<_> = (0..n)
                .map(|i| ['א', 'ב', ' ', '(', '1', '2', ')', 'ג'][i % 8])
                .collect();
            let classes: Vec<_> = chars.iter().copied().map(bidi_class).collect();
            let brackets: Vec<_> = chars.iter().copied().map(bracket_of).collect();
            let wide: Vec<_> = (0..n).map(|i| paired && i % 2 == 1).collect();
            let ((mut scratch, mut out), cold) = allocs_in(|| {
                let mut scratch = Scratch::default();
                let mut out = Vec::new();
                reorder_cells_with_classes_into(
                    &classes,
                    &brackets,
                    &wide,
                    BaseDirection::Auto,
                    &mut scratch,
                    &mut out,
                );
                (scratch, out)
            });
            let bytes = ALLOC_BYTES.with(Cell::get);
            let retained = RETAINED_DELTA.with(Cell::get);
            assert!(retained >= 0);
            assert_eq!(out.len(), n);
            for _ in 0..100 {
                reorder_cells_with_classes_into(
                    &classes,
                    &brackets,
                    &wide,
                    BaseDirection::Auto,
                    &mut scratch,
                    &mut out,
                );
                for &cell in &out {
                    black_box(scratch.cell_is_rtl(black_box(cell)));
                }
            }
            let (_, warm) = allocs_in(|| {
                reorder_cells_with_classes_into(
                    &classes,
                    &brackets,
                    &wide,
                    BaseDirection::Auto,
                    &mut scratch,
                    &mut out,
                )
            });
            for sample in 0..5 {
                let before = instructions();
                for _ in 0..1000 {
                    reorder_cells_with_classes_into(
                        black_box(&classes),
                        black_box(&brackets),
                        black_box(&wide),
                        BaseDirection::Auto,
                        &mut scratch,
                        &mut out,
                    );
                    for &cell in &out {
                        black_box(scratch.cell_is_rtl(black_box(cell)));
                    }
                }
                let instructions = before
                    .zip(instructions())
                    .map(|(before, after)| after.saturating_sub(before));
                println!(
                    "bidi-scratch cells={n} paired={paired} sample={sample} iterations=1000 cold={cold} warm={warm} requested_bytes={bytes} retained_layout_bytes={retained} instructions={instructions:?}"
                );
            }
        }
    }
}

#[test]
fn single_cell_bidi_rows_avoid_redundant_scratch_storage() {
    use aterm_bidi::{
        BaseDirection, Scratch, bidi_class, bracket_of, reorder_cells_with_classes_into,
    };

    let chars: Vec<_> = (0..80)
        .map(|i| ['א', 'ב', ' ', '(', '1', '2', ')', 'ג'][i % 8])
        .collect();
    let classes: Vec<_> = chars.iter().copied().map(bidi_class).collect();
    let brackets: Vec<_> = chars.iter().copied().map(bracket_of).collect();
    let ((scratch, out), calls) = allocs_in(|| {
        let mut scratch = Scratch::default();
        let mut out = Vec::new();
        reorder_cells_with_classes_into(
            &classes,
            &brackets,
            &[],
            BaseDirection::Auto,
            &mut scratch,
            &mut out,
        );
        (scratch, out)
    });
    let retained = RETAINED_DELTA.with(Cell::get);
    println!("bidi-scratch-budget cold={calls} retained_layout_bytes={retained}");
    // The folded baseline uses 40 requests and 6,444 live layout bytes. The
    // direct path uses 17 and 3,500; leave space without admitting the old path.
    assert!(calls <= 20, "redundant cold scratch allocations: {calls}");
    assert!(
        (0..=4000).contains(&retained),
        "redundant retained scratch: {retained}"
    );
    assert_eq!(out.len(), 80);
    assert!(out.iter().any(|&cell| scratch.cell_is_rtl(cell)));
}

#[test]
#[ignore = "manual retained BiDi scratch layouts across width and glyph-shape changes"]
fn bidi_scratch_transition_layouts() {
    use aterm_bidi::{
        BaseDirection, Scratch, bidi_class, bracket_of, reorder_cells_with_classes_into,
    };

    let mut scratch = Scratch::default();
    let mut out = Vec::new();
    let mut live = 0i128;
    for (n, paired) in [(80, false), (258, true), (300, true), (80, false)] {
        let chars: Vec<_> = (0..n)
            .map(|i| ['א', 'ב', ' ', '(', '1', '2', ')', 'ג'][i % 8])
            .collect();
        let classes: Vec<_> = chars.iter().copied().map(bidi_class).collect();
        let brackets: Vec<_> = chars.iter().copied().map(bracket_of).collect();
        let wide: Vec<_> = (0..n).map(|i| paired && i % 2 == 1).collect();
        let (_, calls) = allocs_in(|| {
            reorder_cells_with_classes_into(
                &classes,
                &brackets,
                &wide,
                BaseDirection::Auto,
                &mut scratch,
                &mut out,
            )
        });
        live += RETAINED_DELTA.with(Cell::get);
        assert!(live >= 0);
        assert_eq!(out.len(), n);
        println!(
            "bidi-scratch-transition cells={n} paired={paired} calls={calls} retained_layout_bytes={live}"
        );
    }
}

#[test]
#[ignore = "manual map-only BiDi entry point instruction and allocation comparison"]
fn bidi_map_only_cost() {
    use aterm_bidi::{BaseDirection, bidi_class, bracket_of, reorder_cells_with_classes};
    use std::hint::black_box;

    for n in [80, 257] {
        for paired in [false, true] {
            let chars: Vec<_> = (0..n)
                .map(|i| ['א', 'ב', ' ', '(', '1', '2', ')', 'ג'][i % 8])
                .collect();
            let classes: Vec<_> = chars.iter().copied().map(bidi_class).collect();
            let brackets: Vec<_> = chars.iter().copied().map(bracket_of).collect();
            let wide: Vec<_> = (0..n).map(|i| paired && i % 2 == 1).collect();
            let (out, cold) = allocs_in(|| {
                reorder_cells_with_classes(&classes, &brackets, &wide, BaseDirection::Auto)
            });
            assert_eq!(out.len(), n);
            for _ in 0..100 {
                black_box(reorder_cells_with_classes(
                    &classes,
                    &brackets,
                    &wide,
                    BaseDirection::Auto,
                ));
            }
            for sample in 0..5 {
                let before = instructions();
                for _ in 0..1000 {
                    black_box(reorder_cells_with_classes(
                        black_box(&classes),
                        black_box(&brackets),
                        black_box(&wide),
                        BaseDirection::Auto,
                    ));
                }
                let instructions = before
                    .zip(instructions())
                    .map(|(before, after)| after.saturating_sub(before));
                println!(
                    "bidi-map-only cells={n} paired={paired} sample={sample} iterations=1000 cold={cold} instructions={instructions:?}"
                );
            }
        }
    }
}
