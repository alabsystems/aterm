// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Damage-scoped refill allocates nothing once warm and stays on the scoped arm.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static ALLOC_CALLS: Cell<u64> = const { Cell::new(0) };
    static ALLOC_BYTES: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every method forwards to `System` with the caller's layout and pointer; the thread-local
// counter never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = ALLOC_CALLS.try_with(|n| n.set(n.get() + 1));
                let _ = ALLOC_BYTES.try_with(|n| n.set(n.get() + l.size() as u64));
            }
        });
        // SAFETY: forwarding to System with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let _ = ACTIVE.try_with(|active| {
            if active.get() {
                let _ = ALLOC_CALLS.try_with(|n| n.set(n.get() + 1));
                let _ = ALLOC_BYTES.try_with(|n| n.set(n.get() + new_size as u64));
            }
        });
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.realloc(p, l, new_size) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn allocs_in<R>(f: impl FnOnce() -> R) -> (R, u64) {
    ALLOC_CALLS.with(|n| n.set(0));
    ALLOC_BYTES.with(|n| n.set(0));
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
