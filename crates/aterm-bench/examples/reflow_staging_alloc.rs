// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Manual allocation-traffic comparison for grid row and frame preparation.
//! Fixture entry points stay separate so inlining into one main does not couple sibling measurements.

use std::alloc::System;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use aterm_bench::allocator::{Observed, Observer};
use aterm_core::scrollback::Scrollback;
use aterm_core::terminal::Terminal;

static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;

// SAFETY: the callback uses only nonallocating, nonpanicking atomic arithmetic.
unsafe impl Observer for Counting {
    fn requested(&self, bytes: usize) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOC: Observed<System, Counting> = Observed::new(System, Counting);

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

fn main() -> Result<(), String> {
    if let Some(fixture) = std::env::args().nth(1) {
        match fixture.as_str() {
            "history-unscroll" => history_unscroll(),
            "wide-restores" => wide_restores(),
            "mixed-images" => mixed_image_rows(),
            "inline-images" => inline_image_rows(),
            "restore-links" => restore_links(),
            "linked-writes" => linked_writes(),
            "image-free" => image_free_frames(),
            "floating" => floating_frames(),
            "snapshot-copies" => snapshot_copies(),
            "restore-rows" => restore_rows(),
            "reflow-staging" => reflow_staging(),
            _ => {
                return Err(format!(
                    "unknown fixture {fixture:?}; use history-unscroll, wide-restores, mixed-images, inline-images, restore-links, linked-writes, image-free, floating, snapshot-copies, restore-rows or reflow-staging"
                ));
            }
        }
        return Ok(());
    }
    history_unscroll();
    wide_restores();
    mixed_image_rows();
    inline_image_rows();
    restore_links();
    linked_writes();
    image_free_frames();
    floating_frames();
    snapshot_copies();
    restore_rows();
    reflow_staging();
    Ok(())
}

#[inline(never)]
fn reflow_staging() {
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

#[inline(never)]
fn history_unscroll() {
    use aterm_grid::{Grid, ImageData, ImageFormat, ImageRef, ImageScaling};
    use std::sync::Arc;
    for rows in [16_u16, 64, 128] {
        // Stay below the intentional 10,000-cell hyperlink retention budget.
        let cols = 64;
        let pull = rows / 2;
        for style in ["plain", "linked", "image_link"] {
            let mut source = Terminal::new(1, cols);
            if style != "plain" {
                source.process(b"\x1b]8;id=pull;https://pull.example\x1b\\\x1b[38;2;12;34;56m\x1b[48;2;65;43;21m\x1b[4;58;5;7m");
            }
            let text = "x".repeat(usize::from(cols));
            source.process(text.as_bytes());
            let mut line = source.grid().checkpoint_visible_lines().remove(0);
            if style == "image_link" {
                let image = Arc::new(ImageData {
                    bytes: vec![7; 256],
                    format: ImageFormat::Png,
                    cols,
                    rows: 1,
                    z_index: 0,
                    band_lift_px: 0,
                    cell_offset_px: (0, 0),
                    scaling: ImageScaling::Fit,
                    source_rect: None,
                });
                let mut pictures = Grid::new(1, cols);
                for col in 0..cols {
                    pictures.set_cell_image(
                        0,
                        col,
                        ImageRef {
                            image: Arc::clone(&image),
                            cell_row: 0,
                            cell_col: col,
                            kitty: None,
                        },
                    );
                }
                line.set_images(
                    pictures.checkpoint_visible_lines()[0]
                        .images()
                        .unwrap()
                        .to_vec(),
                );
            }
            let history = vec![line.clone(); usize::from(pull)];
            let visible = vec![line; usize::from(rows)];
            let mut grids: Vec<_> = (0..16)
                .map(|_| {
                    let mut grid = Grid::with_scrollback(rows, cols, usize::from(rows) * 2);
                    grid.scroll_in_history(&history);
                    grid.restore_visible_from_lines(&visible);
                    grid
                })
                .collect();
            let before_instructions = instructions();
            let before_calls = CALLS.load(Ordering::Relaxed);
            let before_bytes = BYTES.load(Ordering::Relaxed);
            let began = Instant::now();
            let mut total = 0;
            for grid in &mut grids {
                total += black_box(grid).unscroll_from_scrollback(black_box(usize::from(pull)));
            }
            let ns = began.elapsed().as_nanos();
            let calls = CALLS.load(Ordering::Relaxed) - before_calls;
            let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
            let retired = instructions()
                .zip(before_instructions)
                .map(|(end, start)| end - start);
            assert_eq!(total, 16 * usize::from(pull));
            for grid in &grids {
                for row in 0..rows {
                    assert_eq!(grid.row_text(row).as_deref(), Some(text.as_str()));
                    if style != "plain" {
                        assert_eq!(grid.fg_rgb_at(row, 0), Some([12, 34, 56]));
                        assert_eq!(
                            grid.cell_extra(row, 0)
                                .unwrap()
                                .hyperlink_id()
                                .map(AsRef::as_ref),
                            Some("pull")
                        );
                    }
                    if style == "image_link" {
                        assert_eq!(
                            grid.cell_extra(row, cols - 1)
                                .unwrap()
                                .image()
                                .unwrap()
                                .cell_col,
                            cols - 1
                        );
                    }
                }
            }
            println!(
                "history_unscroll rows={rows} cols={cols} pull={pull} style={style} iterations=16 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
            );
        }
    }
}

#[inline(never)]
fn wide_restores() {
    use aterm_grid::{Grid, ImageData, ImageFormat, ImageRef, ImageScaling};
    use std::sync::Arc;
    for (glyph, unit, width) in [
        ("cjk", "日", 2),
        ("emoji", "😀", 2),
        ("nonbmp_narrow", "𐐀", 1),
        ("zwj", "👩‍💻", 2),
        ("combining", "e\u{301}", 1),
        ("combining_long", "e\u{301}\u{302}\u{303}\u{304}\u{305}", 1),
    ] {
        for (rows, cols) in [(1_u16, 4_u16), (1, 16), (1, 80), (24, 80), (1, 256)] {
            for style in ["plain", "rgb", "underline", "rgb_image", "rgb_sparse"] {
                let mut source = Terminal::new(rows, cols);
                if style != "plain" {
                    source.process(b"\x1b[4;58;5;7m");
                }
                if style == "rgb" || style == "rgb_image" || style == "rgb_sparse" {
                    source.process(b"\x1b]8;id=wide;https://wide.example\x1b\\\x1b[38;2;12;34;56m\x1b[48;2;65;43;21m");
                }
                let text = if style == "rgb_sparse" {
                    format!("{unit}{}", "x".repeat(usize::from(cols - width)))
                } else {
                    unit.repeat(usize::from(cols / width))
                };
                for row in 0..rows {
                    source.process(format!("\x1b[{};1H{text}", row + 1).as_bytes());
                }
                let mut lines = source.grid().checkpoint_visible_lines();
                drop(source);
                let image = Arc::new(ImageData {
                    bytes: vec![7; 256],
                    format: ImageFormat::Png,
                    cols,
                    rows,
                    z_index: 0,
                    band_lift_px: 0,
                    cell_offset_px: (0, 0),
                    scaling: ImageScaling::Fit,
                    source_rect: None,
                });
                if style == "rgb_image" {
                    let mut pictures = Grid::new(rows, cols);
                    for row in 0..rows {
                        for col in 0..cols {
                            pictures.set_cell_image(
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
                    let image_lines = pictures.checkpoint_visible_lines();
                    for (line, image_line) in lines.iter_mut().zip(&image_lines) {
                        line.set_images(image_line.images().unwrap().to_vec());
                    }
                }
                let mut grid = Grid::new(rows, cols);
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
                    assert_eq!(grid.row_text(row).as_deref(), Some(text.as_str()));
                    for col in 0..cols {
                        assert_eq!(
                            grid.row(row).unwrap().get(col).unwrap().is_wide(),
                            width == 2
                                && if style == "rgb_sparse" {
                                    col == 0
                                } else {
                                    col % 2 == 0
                                }
                        );
                        if style != "plain" {
                            assert_eq!(
                                grid.cell_extra(row, col).unwrap().underline_color_u32(),
                                Some(0x02000007)
                            );
                        }
                        if style == "rgb" || style == "rgb_image" || style == "rgb_sparse" {
                            assert_eq!(grid.fg_rgb_at(row, col), Some([12, 34, 56]));
                            assert_eq!(grid.bg_rgb_at(row, col), Some([65, 43, 21]));
                            assert_eq!(
                                grid.cell_extra(row, col)
                                    .unwrap()
                                    .hyperlink_id()
                                    .map(AsRef::as_ref),
                                Some("wide")
                            );
                        }
                        if style == "rgb_image" {
                            let actual = grid.cell_extra(row, col).unwrap().image().unwrap();
                            assert!(Arc::ptr_eq(&actual.image, &image));
                            assert_eq!((actual.cell_row, actual.cell_col), (row, col));
                        }
                    }
                }
                println!(
                    "wide_restore rows={rows} cols={cols} style={style} glyph={glyph} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
                );
            }
        }
    }
}

#[inline(never)]
fn mixed_image_rows() {
    use aterm_grid::{Grid, ImageData, ImageFormat, ImageRef, ImageScaling};
    use std::sync::Arc;
    let (rows, cols) = (24_u16, 80_u16);
    for pattern in ["none", "tiny", "half", "checker", "full"] {
        let selected = |row: u16, col: u16| match pattern {
            "none" => false,
            "tiny" => row == 0 && col < 4,
            "half" => row < rows / 2,
            "checker" => (row + col) % 16 == 0,
            "full" => true,
            _ => unreachable!(),
        };
        let image = Arc::new(ImageData {
            bytes: vec![7; 256],
            format: ImageFormat::Png,
            cols,
            rows,
            z_index: 0,
            band_lift_px: 0,
            cell_offset_px: (0, 0),
            scaling: ImageScaling::Fit,
            source_rect: None,
        });
        let mut pictures = Grid::new(rows, cols);
        let mut cells = 0;
        for row in 0..rows {
            for col in 0..cols {
                if selected(row, col) {
                    pictures.set_cell_image(
                        row,
                        col,
                        ImageRef {
                            image: Arc::clone(&image),
                            cell_row: row,
                            cell_col: col,
                            kitty: None,
                        },
                    );
                    cells += 1;
                }
            }
        }
        let image_lines = pictures.checkpoint_visible_lines();
        let mut text = Terminal::new(rows, cols);
        text.process(
            b"\x1b]8;id=mixed;https://mixed.example\x1b\\\x1b[38;2;12;34;56m\x1b[48;2;65;43;21m",
        );
        for row in 0..rows {
            text.process(
                format!("\x1b[{};1H{}", row + 1, "x".repeat(usize::from(cols))).as_bytes(),
            );
        }
        let mut lines = text.grid().checkpoint_visible_lines();
        for (line, images) in lines.iter_mut().zip(&image_lines) {
            if let Some(spans) = images.images() {
                line.set_images(spans.to_vec());
            }
        }
        drop(text);
        drop(pictures);
        drop(image_lines);
        let mut restored = Grid::new(rows, cols);
        restored.restore_visible_from_lines(&lines);
        let before_instructions = instructions();
        let before_calls = CALLS.load(Ordering::Relaxed);
        let before_bytes = BYTES.load(Ordering::Relaxed);
        let began = Instant::now();
        for _ in 0..64 {
            black_box(&mut restored).restore_visible_from_lines(black_box(&lines));
        }
        let ns = began.elapsed().as_nanos();
        let calls = CALLS.load(Ordering::Relaxed) - before_calls;
        let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
        let retired = instructions()
            .zip(before_instructions)
            .map(|(end, start)| end - start);
        for row in 0..rows {
            for col in 0..cols {
                let extra = restored.cell_extra(row, col).unwrap();
                assert_eq!(extra.images().len(), usize::from(selected(row, col)));
                if let Some(actual) = extra.image() {
                    assert!(Arc::ptr_eq(&actual.image, &image));
                    assert_eq!((actual.cell_row, actual.cell_col), (row, col));
                }
                assert_eq!(extra.hyperlink_id().map(AsRef::as_ref), Some("mixed"));
                assert_eq!(restored.fg_rgb_at(row, col), Some([12, 34, 56]));
                assert_eq!(restored.bg_rgb_at(row, col), Some([65, 43, 21]));
            }
        }
        println!(
            "mixed_images pattern={pattern} rows={rows} cols={cols} cells={cells} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
        );
    }
}

#[inline(never)]
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

#[inline(never)]
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

#[inline(never)]
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

#[inline(never)]
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

#[inline(never)]
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
        image_reference_traffic(&frame.images, rows, cols);
        clone_image_frame(&frame, rows, cols, "floating");
    }
}

#[inline(never)]
fn image_reference_traffic(source: &[Vec<(usize, aterm_grid::ImageRef)>], rows: u16, cols: u16) {
    for reuse in [false, true] {
        let mut target = source.to_vec();
        let before_instructions = instructions();
        let before_calls = CALLS.load(Ordering::Relaxed);
        let before_bytes = BYTES.load(Ordering::Relaxed);
        let began = Instant::now();
        for _ in 0..64 {
            if reuse {
                black_box(&mut target).clone_from_slice(black_box(source));
            } else {
                for (target, source) in black_box(&mut target).iter_mut().zip(black_box(source)) {
                    target.clear();
                    target.extend(source.iter().cloned());
                }
            }
        }
        let ns = began.elapsed().as_nanos();
        let calls = CALLS.load(Ordering::Relaxed) - before_calls;
        let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
        let retired = instructions()
            .zip(before_instructions)
            .map(|(end, start)| end - start);
        assert_eq!(target, source);
        println!(
            "image_reference_traffic rows={rows} cols={cols} reuse={reuse} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
        );
    }
}

#[inline(never)]
fn clone_image_frame(frame: &aterm_core::render::RenderInput, rows: u16, cols: u16, glyph: &str) {
    let mut target = frame.clone();
    let before_instructions = instructions();
    let before_calls = CALLS.load(Ordering::Relaxed);
    let before_bytes = BYTES.load(Ordering::Relaxed);
    let began = Instant::now();
    for _ in 0..64 {
        black_box(&mut target).clone_from(black_box(frame));
    }
    let ns = began.elapsed().as_nanos();
    let calls = CALLS.load(Ordering::Relaxed) - before_calls;
    let bytes = BYTES.load(Ordering::Relaxed) - before_bytes;
    let retired = instructions()
        .zip(before_instructions)
        .map(|(end, start)| end - start);
    assert_eq!(target, *frame);
    println!(
        "image_frame_copy rows={rows} cols={cols} glyph={glyph} iterations=64 allocation_calls={calls} allocated_bytes={bytes} ns={ns} instructions={retired:?}"
    );
}

#[inline(never)]
fn snapshot_copies() {
    for (glyph, text) in [
        ("zwj", "👩‍💻"),
        ("combining", "e\u{301}\u{302}\u{303}\u{304}\u{305}"),
    ] {
        for (rows, cols) in [(1, 8), (24, 80)] {
            let mut term = Terminal::new(rows, cols);
            for row in 0..rows {
                term.process(
                    format!("\x1b[{};1H{}", row + 1, text.repeat(usize::from(cols) / 2)).as_bytes(),
                );
            }
            let frame = term.cell_frame(usize::from(rows), usize::from(cols));
            clone_image_frame(&frame, rows, cols, glyph);
        }
    }
}

#[inline(never)]
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
