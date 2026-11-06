// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Manual allocation-traffic comparison for grid row and frame preparation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

fn note(bytes: usize) {
    CALLS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

// SAFETY: every operation forwards the caller's pointer and layout to System unchanged;
// the counters use only nonallocating atomic arithmetic.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        // SAFETY: forwarding the caller's layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        note(size);
        // SAFETY: ptr and layout came from System through this allocator.
        unsafe { System.realloc(ptr, layout, size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr and layout came from System through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

#[cfg(target_os = "macos")]
fn instructions() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    let pid = libc::c_int::try_from(std::process::id()).ok()?;
    // SAFETY: info is writable for the requested rusage_info_v4 flavor.
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    // SAFETY: zeroed at creation, so initialized whatever the call did.
    (rc == 0).then(|| unsafe { info.assume_init() }.ri_instructions)
}

#[cfg(not(target_os = "macos"))]
fn instructions() -> Option<u64> {
    None
}

fn main() {
    inline_image_rows();
    restore_links();
    linked_writes();
    image_free_frames();
    floating_frames();
    restore_rows();
    for (rows, cols, next_cols) in [
        (24, 256, 16),
        (128, 256, 8),
        (256, 256, 1),
        (256, 256, 2),
        (256, 256, 3),
    ] {
        let mut history = Scrollback::new(64, 512, 64_000_000);
        history.set_line_limit(None);
        let mut term = Terminal::with_scrollback(rows, cols, 0, history);
        let line = "x".repeat(usize::from(cols) - 1);
        for row in 0..rows {
            term.process(format!("\x1b[{};1H", row + 1).as_bytes());
            term.process(line.as_bytes());
        }
        let before_instructions = instructions();
        let before_calls = CALLS.load(Ordering::Relaxed);
        let before_bytes = BYTES.load(Ordering::Relaxed);
        let began = Instant::now();
        black_box(&mut term).resize(rows, next_cols);
        let ns = began.elapsed().as_nanos();
        let calls = CALLS.load(Ordering::Relaxed) - before_calls;
        let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
        let retired = instructions()
            .zip(before_instructions)
            .map(|(end, start)| end - start);
        let found = term
            .indexed_search()
            .search_results_opts("x", true, false)
            .unwrap();
        let expected = usize::from(rows) * (usize::from(cols) - 1);
        assert_eq!(
            found.matches.len(),
            expected,
            "reflow must preserve every written glyph"
        );
        assert_eq!(found.cut_short_lines, 0);
        println!(
            "reflow_staging rows={rows} cols={cols} next_cols={next_cols} allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?} glyphs={expected} history_lines={}",
            term.grid().scrollback_lines()
        );
    }
}

fn inline_image_rows() {
    use aterm_grid::{Grid, ImageData, ImageFormat, ImageRef, ImageScaling};
    use std::sync::Arc;

    for (rows, cols, layers) in [(1_u16, 4_u16, 1), (1, 80, 1), (24, 80, 1), (24, 80, 2)] {
        let mut source = Grid::new(rows, cols);
        for layer in 0..layers {
            let image = Arc::new(ImageData {
                bytes: vec![7; 256],
                format: ImageFormat::Png,
                cols,
                rows,
                z_index: layer,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: ImageScaling::Fit,
                source_rect: None,
            });
            for row in 0..rows {
                for col in 0..cols {
                    source.stack_cell_image(
                        row,
                        col,
                        ImageRef {
                            image: Arc::clone(&image),
                            cell_row: row,
                            cell_col: col,
                            kitty: None,
                        },
                    );
                }
            }
        }
        let lines = source.checkpoint_visible_lines();
        let mut restored = Grid::new(rows, cols);
        restored.restore_visible_from_lines(&lines);
        for phase in ["capture", "restore"] {
            let before_instructions = instructions();
            let before_calls = CALLS.load(Ordering::Relaxed);
            let before_bytes = BYTES.load(Ordering::Relaxed);
            let began = Instant::now();
            for _ in 0..64 {
                if phase == "capture" {
                    black_box(black_box(&source).checkpoint_visible_lines());
                } else {
                    black_box(&mut restored).restore_visible_from_lines(black_box(&lines));
                }
            }
            let ns = began.elapsed().as_nanos();
            let calls = CALLS.load(Ordering::Relaxed) - before_calls;
            let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
            let retired = instructions()
                .zip(before_instructions)
                .map(|(end, start)| end - start);
            println!(
                "inline_images rows={rows} cols={cols} layers={layers} phase={phase} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
            );
        }
        for row in 0..rows {
            assert_eq!(
                lines[usize::from(row)].image_count(),
                usize::try_from(layers).unwrap()
            );
            for col in 0..cols {
                let actual = restored.cell_extra(row, col).unwrap().images();
                let expected = source.cell_extra(row, col).unwrap().images();
                assert_eq!(actual.len(), expected.len());
                for (actual, expected) in actual.iter().zip(expected) {
                    assert!(Arc::ptr_eq(&actual.image, &expected.image));
                    assert_eq!((actual.cell_row, actual.cell_col), (row, col));
                }
            }
        }
    }
}

fn restore_links() {
    for (rows, cols, scopes) in [
        (1_u16, 4_u16, 1),
        (1, 80, 1),
        (1, 256, 1),
        (24, 80, 1),
        (1, 80, 2),
    ] {
        let mut source = Terminal::new(rows, cols);
        source.process(b"\x1b]8;id=restore-probe;https://example.com\x1b\\");
        for row in 0..rows {
            source.process(format!("\x1b[{};1H", row + 1).as_bytes());
            if scopes == 1 {
                source.process("x".repeat(usize::from(cols)).as_bytes());
            } else {
                source.process("x".repeat(usize::from(cols / 2)).as_bytes());
                source.process(b"\x1b]8;id=right;https://example.com\x1b\\");
                source.process("x".repeat(usize::from(cols - cols / 2)).as_bytes());
            }
        }
        let capture_calls_before = CALLS.load(Ordering::Relaxed);
        let capture_bytes_before = BYTES.load(Ordering::Relaxed);
        let lines = source.grid().checkpoint_visible_lines();
        let capture_calls = CALLS.load(Ordering::Relaxed) - capture_calls_before;
        let capture_bytes = BYTES.load(Ordering::Relaxed) - capture_bytes_before;
        let line_charge_bytes = lines.iter().map(|line| line.memory_used()).sum::<usize>();
        drop(source);
        let url_owners = std::sync::Arc::strong_count(&lines[0].hyperlinks().unwrap()[0].url);
        let mut grid = aterm_grid::Grid::new(rows, cols);
        grid.restore_visible_from_lines(&lines);
        let before_instructions = instructions();
        let before_calls = CALLS.load(Ordering::Relaxed);
        let before_bytes = BYTES.load(Ordering::Relaxed);
        let began = Instant::now();
        for _ in 0..64 {
            black_box(&mut grid).restore_visible_from_lines(black_box(&lines));
        }
        let ns = began.elapsed().as_nanos();
        let calls = CALLS.load(Ordering::Relaxed) - before_calls;
        let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
        let retired = instructions()
            .zip(before_instructions)
            .map(|(end, start)| end - start);
        for row in 0..rows {
            for col in 0..cols {
                let extra = grid.cell_extra(row, col).unwrap();
                assert_eq!(
                    extra.hyperlink().map(AsRef::as_ref),
                    Some("https://example.com")
                );
                let id = if scopes == 2 && col >= cols / 2 {
                    "right"
                } else {
                    "restore-probe"
                };
                assert_eq!(extra.hyperlink_id().map(AsRef::as_ref), Some(id));
            }
        }
        println!(
            "restore_links rows={rows} cols={cols} scopes={scopes} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?} capture_calls={capture_calls} capture_bytes={capture_bytes} line_charge_bytes={line_charge_bytes} url_owners={url_owners}"
        );
    }
}

fn linked_writes() {
    for (label, linked, input) in [
        ("cursor", false, &b"\x1b[H"[..]),
        ("plain_ascii", false, &b"\x1b[Hx"[..]),
        ("linked_ascii", true, &b"\x1b[Hx"[..]),
        ("linked_unicode", true, "\x1b[H日".as_bytes()),
    ] {
        let mut term = Terminal::new(2, 80);
        if linked {
            term.process(b"\x1b]8;id=write-probe;https://example.com\x1b\\");
        }
        term.process(input);
        let before_instructions = instructions();
        let before_calls = CALLS.load(Ordering::Relaxed);
        let before_bytes = BYTES.load(Ordering::Relaxed);
        let began = Instant::now();
        for _ in 0..256 {
            black_box(&mut term).process(black_box(input));
        }
        let ns = began.elapsed().as_nanos();
        let calls = CALLS.load(Ordering::Relaxed) - before_calls;
        let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
        let retired = instructions()
            .zip(before_instructions)
            .map(|(end, start)| end - start);
        if linked {
            let extra = term.grid().cell_extra(0, 0).unwrap();
            assert_eq!(
                extra.hyperlink().map(AsRef::as_ref),
                Some("https://example.com")
            );
            assert_eq!(extra.hyperlink_id().map(AsRef::as_ref), Some("write-probe"));
        }
        println!(
            "linked_write phase={label} iterations=256 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
        );
    }
}

fn image_free_frames() {
    for (rows, cols) in [(24_u16, 80_u16), (40, 120)] {
        let mut term = Terminal::new(rows, cols);
        term.process(b"\x1b]8;;https://example.com\x1b\\");
        let text = "x".repeat(usize::from(cols) - 1);
        for row in 0..rows {
            term.process(format!("\x1b[{};1H{text}", row + 1).as_bytes());
        }
        assert_eq!(term.grid().extras().len(), usize::from(rows) * text.len());
        let mut frame = aterm_core::render::RenderInput::empty();
        term.cell_frame_into(&mut frame, usize::from(rows), usize::from(cols));
        for phase in ["images", "frame", "write_frame"] {
            let before_instructions = instructions();
            let before_calls = CALLS.load(Ordering::Relaxed);
            let before_bytes = BYTES.load(Ordering::Relaxed);
            let began = Instant::now();
            for _ in 0..256 {
                if phase == "images" {
                    assert!(
                        black_box(&term)
                            .images_frame(usize::from(rows))
                            .iter()
                            .all(Vec::is_empty)
                    );
                } else {
                    if phase == "write_frame" {
                        term.process(b"\x1b[Hx");
                    }
                    black_box(&mut term).cell_frame_into(
                        &mut frame,
                        usize::from(rows),
                        usize::from(cols),
                    );
                }
            }
            let ns = began.elapsed().as_nanos();
            let calls = CALLS.load(Ordering::Relaxed) - before_calls;
            let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
            let retired = instructions()
                .zip(before_instructions)
                .map(|(end, start)| end - start);
            assert!(frame.images.iter().all(Vec::is_empty));
            println!(
                "image_free rows={rows} cols={cols} phase={phase} iterations=256 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
            );
        }
    }
}

fn floating_frames() {
    for (rows, cols) in [(4_u16, 16_u16), (24, 80)] {
        let mut term = Terminal::new(rows, cols);
        term.set_cell_pixel_size(1, 1);
        term.process(b"\x1b_Ga=t,f=32,s=1,v=1,i=1,q=2;/wAA/w==\x1b\\\x1b_Ga=t,f=32,s=1,v=1,i=2,q=2;/wAA/w==\x1b\\");
        term.process("\x1b_Ga=p,U=1,i=1,p=7,c=1,r=1,q=2\x1b\\\x1b[1;1H\x1b[38;5;1m\x1b[58;5;7m\u{10EEEE}\u{0305}\u{0305}\x1b[0m".as_bytes());
        term.process(
            format!("\x1b_Ga=p,i=2,p=5,P=1,Q=7,c={cols},r={rows},z=1,q=2\x1b\\").as_bytes(),
        );
        let mut frame = aterm_core::render::RenderInput::empty();
        term.cell_frame_into(&mut frame, usize::from(rows), usize::from(cols));
        let before_instructions = instructions();
        let before_calls = CALLS.load(Ordering::Relaxed);
        let before_bytes = BYTES.load(Ordering::Relaxed);
        let began = Instant::now();
        for _ in 0..64 {
            black_box(&mut term).cell_frame_into(&mut frame, usize::from(rows), usize::from(cols));
        }
        let ns = began.elapsed().as_nanos();
        let calls = CALLS.load(Ordering::Relaxed) - before_calls;
        let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
        let retired = instructions()
            .zip(before_instructions)
            .map(|(end, start)| end - start);
        let mut found = 0;
        for (row, images) in frame.images.iter().enumerate() {
            for (col, image) in images {
                if image.kitty.is_some_and(|tag| tag.image_id == 2) {
                    assert_eq!(usize::from(image.cell_row), row);
                    assert_eq!(usize::from(image.cell_col), *col);
                    found += 1;
                }
            }
        }
        assert_eq!(found, usize::from(rows) * usize::from(cols));
        println!(
            "floating_frame rows={rows} cols={cols} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?} tiles={found}"
        );
    }
}

fn restore_rows() {
    for (rows, cols) in [(1_u16, 4_u16), (1, 80), (1, 256), (24, 80), (128, 80)] {
        let mut source = Terminal::new(rows, cols);
        source.process(b"\x1b[38;2;12;34;56m\x1b[48;2;65;43;21m");
        for row in 0..rows {
            source.process(format!("\x1b[{};1H", row + 1).as_bytes());
            source.process("x".repeat(usize::from(cols)).as_bytes());
        }
        let lines = source.grid().checkpoint_visible_lines();
        let mut grid = aterm_grid::Grid::new(rows, cols);
        grid.restore_visible_from_lines(&lines);
        let before_instructions = instructions();
        let before_calls = CALLS.load(Ordering::Relaxed);
        let before_bytes = BYTES.load(Ordering::Relaxed);
        let began = Instant::now();
        for _ in 0..64 {
            black_box(&mut grid).restore_visible_from_lines(black_box(&lines));
        }
        let ns = began.elapsed().as_nanos();
        let calls = CALLS.load(Ordering::Relaxed) - before_calls;
        let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
        let retired = instructions()
            .zip(before_instructions)
            .map(|(end, start)| end - start);
        let restored = grid.checkpoint_visible_lines();
        for (actual_line, expected_line) in restored.iter().zip(&lines) {
            assert_eq!(actual_line.as_str(), expected_line.as_str());
            let mut actual = actual_line.attr_cursor();
            let mut expected = expected_line.attr_cursor();
            for col in 0..usize::from(cols) {
                assert_eq!(actual.attr_at(col), expected.attr_at(col));
            }
        }
        println!(
            "restore_rows rows={rows} cols={cols} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
        );
    }
}
