// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Deterministic heap-allocation complexity guard for the terminal engine.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOC_CALLS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOC_CALLS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(l.size() as u64, Ordering::Relaxed);
        }
        // SAFETY: forwarding to System with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarding to System with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn workload(lines: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for i in 0..lines {
        v.extend_from_slice(b"\x1b[1;32m");
        for c in 0..72 {
            v.push(b'a' + ((i + c) % 26) as u8);
        }
        v.extend_from_slice(b"\x1b[0m\r\n");
    }
    v
}

fn unicode_workload(lines: usize) -> Vec<u8> {
    let glyphs = ['中', '文', '日', '本', '한', '😀', 'é', '∑'];
    let mut v = Vec::new();
    let mut buf = [0u8; 4];
    for i in 0..lines {
        for c in 0..36 {
            let g = glyphs[(i + c) % glyphs.len()];
            v.extend_from_slice(g.encode_utf8(&mut buf).as_bytes());
        }
        v.extend_from_slice(b"\r\n");
    }
    v
}

const WARM_LINES: usize = 11_000;

fn steady_state_alloc_calls(measure_input: &[u8]) -> u64 {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(&workload(WARM_LINES)); // fill grid + scrollback ring to capacity
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    t.process(measure_input);
    ACTIVE.store(false, Ordering::Relaxed);
    ALLOC_CALLS.load(Ordering::Relaxed)
}

fn count_allocations(f: impl FnOnce()) -> u64 {
    ALLOC_CALLS.store(0, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    f();
    ACTIVE.store(false, Ordering::Relaxed);
    ALLOC_CALLS.load(Ordering::Relaxed)
}

fn linked_writes_reuse_payload_and_overwrite_storage() {
    use aterm_core::terminal::Terminal;
    use std::hint::black_box;

    for text in ["x".to_owned(), "日".to_owned(), "x".repeat(80)] {
        let mut term = Terminal::new(2, 80);
        term.process(b"\x1b]8;id=steady-link;https://example.com\x1b\\");
        let input = format!("\x1b[H{text}");
        term.process(input.as_bytes());
        ALLOC_BYTES.store(0, Ordering::Relaxed);
        let calls = count_allocations(|| {
            for _ in 0..256 {
                black_box(&mut term).process(black_box(input.as_bytes()));
            }
        });
        let bytes = ALLOC_BYTES.load(Ordering::Relaxed);
        assert_eq!((calls, bytes), (0, 0), "warm linked overwrite {text:?}");
        let width = if text == "日" { 2 } else { text.len() };
        for col in 0..width {
            let extra = term.grid().cell_extra(0, col as u16).unwrap();
            assert_eq!(
                extra.hyperlink().map(AsRef::as_ref),
                Some("https://example.com")
            );
            assert_eq!(extra.hyperlink_id().map(AsRef::as_ref), Some("steady-link"));
        }
    }
}

fn image_history_restore_reuses_layer_storage() {
    use aterm_grid::{Grid, ImageData, ImageFormat, ImageRef, ImageScaling};
    use std::{hint::black_box, sync::Arc};

    for (rows, cols) in [(1_u16, 4_u16), (1, 80), (1, 256), (24, 80)] {
        for layers in [1, 2] {
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
            drop(source);
            let mut grid = Grid::new(rows, cols);
            grid.restore_visible_from_lines(&lines);
            ALLOC_BYTES.store(0, Ordering::Relaxed);
            let calls = count_allocations(|| {
                for _ in 0..64 {
                    black_box(&mut grid).restore_visible_from_lines(black_box(&lines));
                }
            });
            assert_eq!(
                (calls, ALLOC_BYTES.load(Ordering::Relaxed)),
                (0, 0),
                "warm image restore {rows}x{cols}, {layers} layers"
            );
            for row in 0..rows {
                let spans = lines[usize::from(row)].images().unwrap();
                for col in 0..cols {
                    let actual = grid.cell_extra(row, col).unwrap().images();
                    assert_eq!(actual.len(), spans.len());
                    for (actual, span) in actual.iter().zip(spans) {
                        assert!(Arc::ptr_eq(&actual.image, &span.image));
                        assert_eq!((actual.cell_row, actual.cell_col), (row, col));
                    }
                }
            }
            {
                let mut styled = aterm_core::terminal::Terminal::new(rows, cols);
                styled
                    .process(b"\x1b]8;id=styled;https://styled.example\x1b\\\x1b[38;2;12;34;56mx");
                let mut styled_lines = styled.grid().checkpoint_visible_lines();
                for (styled, image_line) in styled_lines.iter_mut().zip(&lines) {
                    styled.set_images(image_line.images().unwrap().to_vec());
                }
                drop(styled);
                grid.restore_visible_from_lines(&styled_lines);
                ALLOC_BYTES.store(0, Ordering::Relaxed);
                let calls = count_allocations(|| {
                    for _ in 0..64 {
                        black_box(&mut grid).restore_visible_from_lines(black_box(&styled_lines));
                    }
                });
                assert_eq!(
                    (calls, ALLOC_BYTES.load(Ordering::Relaxed)),
                    (0, 0),
                    "warm styled image restore {rows}x{cols}, {layers} layers"
                );
                assert_eq!(grid.fg_rgb_at(0, 0), Some([12, 34, 56]));
                assert_eq!(
                    grid.cell_extra(0, 0)
                        .unwrap()
                        .hyperlink_id()
                        .map(AsRef::as_ref),
                    Some("styled")
                );
            }
            let images: Vec<_> = lines
                .iter()
                .flat_map(|line| line.images().unwrap())
                .map(|span| Arc::downgrade(&span.image))
                .collect();
            let mut text = aterm_core::terminal::Terminal::new(rows, cols);
            text.process(
                "\x1b]8;id=new;https://new.example\x1b\\\x1b[38;2;12;34;56me\u{0301}".as_bytes(),
            );
            let text_lines = text.grid().checkpoint_visible_lines();
            grid.restore_visible_from_lines(&text_lines);
            assert_eq!(grid.fg_rgb_at(0, 0), Some([12, 34, 56]));
            assert_eq!(
                grid.cell_extra(0, 0)
                    .unwrap()
                    .hyperlink_id()
                    .map(AsRef::as_ref),
                Some("new")
            );
            assert_eq!(
                grid.checkpoint_visible_lines()[0].as_str(),
                text_lines[0].as_str()
            );
            assert!(!grid.extras().any_image());
            drop(lines);
            assert!(images.iter().all(|image| image.upgrade().is_none()));
            grid.restore_visible_from_lines(&[]);
            assert!(grid.extras().is_empty());
            assert!(!grid.extras().any_image());
        }
    }
}

fn linked_history_restore_reuses_payload_storage() {
    use aterm_core::terminal::Terminal;
    use std::hint::black_box;

    for (rows, cols) in [(1_u16, 4_u16), (1, 80), (1, 256), (24, 80)] {
        let mut source = Terminal::new(rows, cols);
        source.process(b"\x1b]8;id=history-link;https://example.com\x1b\\");
        for row in 0..rows {
            source.process(
                format!("\x1b[{};1H{}", row + 1, "x".repeat(usize::from(cols))).as_bytes(),
            );
        }
        let lines = source.grid().checkpoint_visible_lines();
        drop(source);
        let mut grid = aterm_grid::Grid::new(rows, cols);
        grid.restore_visible_from_lines(&lines);
        ALLOC_BYTES.store(0, Ordering::Relaxed);
        let calls = count_allocations(|| {
            for _ in 0..64 {
                black_box(&mut grid).restore_visible_from_lines(black_box(&lines));
            }
        });
        let bytes = ALLOC_BYTES.load(Ordering::Relaxed);
        assert_eq!(
            (calls, bytes),
            (0, 0),
            "warm linked history restore {rows}x{cols}"
        );
        for row in 0..rows {
            for col in 0..cols {
                let extra = grid.cell_extra(row, col).unwrap();
                assert_eq!(
                    extra.hyperlink().map(AsRef::as_ref),
                    Some("https://example.com")
                );
                assert_eq!(
                    extra.hyperlink_id().map(AsRef::as_ref),
                    Some("history-link")
                );
            }
        }
        grid.cell_extra_mut(0, 0)
            .set_hyperlink_id(Some(std::sync::Arc::from("edited")));
        assert_eq!(
            lines[0].hyperlinks().unwrap()[0].id.as_deref(),
            Some("history-link")
        );
        assert_eq!(
            grid.cell_extra(0, 1)
                .unwrap()
                .hyperlink_id()
                .map(AsRef::as_ref),
            Some("history-link")
        );
    }
}

fn floating_frame_reuses_warm_storage() {
    use aterm_core::render::RenderInput;
    use aterm_core::terminal::Terminal;
    for (rows, cols) in [(4_u16, 16_u16), (24, 80)] {
        let mut term = Terminal::new(rows, cols);
        term.set_cell_pixel_size(1, 1);
        term.process(b"\x1b_Ga=t,f=32,s=1,v=1,i=1,q=2;/wAA/w==\x1b\\\x1b_Ga=t,f=32,s=1,v=1,i=2,q=2;/wAA/w==\x1b\\");
        term.process("\x1b_Ga=p,U=1,i=1,p=7,c=1,r=1,q=2\x1b\\\x1b[1;1H\x1b[38;5;1m\x1b[58;5;7m\u{10EEEE}\u{0305}\u{0305}\x1b[0m".as_bytes());
        term.process(
            format!("\x1b_Ga=p,i=2,p=5,P=1,Q=7,c={cols},r={rows},z=1,q=2\x1b\\").as_bytes(),
        );
        let mut frame = RenderInput::empty();
        term.cell_frame_into(&mut frame, usize::from(rows), usize::from(cols));
        ALLOC_BYTES.store(0, Ordering::Relaxed);
        let calls = count_allocations(|| {
            for _ in 0..64 {
                term.cell_frame_into(&mut frame, usize::from(rows), usize::from(cols));
            }
        });
        let measured = (calls, ALLOC_BYTES.load(Ordering::Relaxed));
        let tiles = frame
            .images
            .iter()
            .flatten()
            .filter(|(_, image)| image.kitty.is_some_and(|tag| tag.image_id == 2))
            .count();
        assert_eq!(tiles, usize::from(rows) * usize::from(cols));
        assert_eq!(
            measured,
            (0, 0),
            "64 warm floating-image refills allocated heap storage"
        );
    }
}

fn render_extras_avoid_temporary_allocations() {
    use aterm_core::render::RenderInput;
    use aterm_core::terminal::Terminal;
    use aterm_grid::{CellDataView, CellExtra};
    use std::hint::black_box;
    use std::sync::Arc;

    let mut extra = CellExtra::default();
    extra.set_complex_char(Some(Arc::from("👩\u{200D}💻")));
    extra.add_combining('\u{0301}');
    extra.add_combining('\u{0323}');
    let data = CellDataView::History(Some(&extra));
    const TAIL: &str = "\u{200D}💻\u{0301}\u{0323}";
    let mut identical = true;
    let borrowed = count_allocations(|| {
        for _ in 0..128 {
            identical &= black_box(data).marks().eq(TAIL.chars());
        }
    });
    assert!(identical, "the history tail lost or reordered marks");
    assert!(
        borrowed <= 2,
        "borrowed history marks allocated {borrowed} times"
    );
    let historical = count_allocations(|| {
        for _ in 0..128 {
            let mut marks: Vec<char> = extra.complex_char().unwrap().chars().skip(1).collect();
            marks.extend_from_slice(extra.combining());
            black_box(marks);
        }
    });
    assert!(
        historical >= 128,
        "the allocating negative control must fire"
    );

    const CLUSTERS: [&str; 5] = [
        "👨\u{200D}👩\u{200D}👧",
        "👍🏽",
        "1\u{FE0F}\u{20E3}",
        "🇺🇸",
        "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}",
    ];
    let mut terminal = Terminal::new(2, 32);
    terminal.process(CLUSTERS.join(" ").as_bytes());
    terminal.process("\x1b[2;1He\u{0301}\u{0323}".as_bytes());
    let mut frame = RenderInput::empty();
    terminal.cell_frame_into(&mut frame, 2, 32);
    assert_eq!(frame.clusters[0].len(), CLUSTERS.len());
    assert!(
        frame.clusters[0]
            .iter()
            .map(|(_, s)| s.as_ref())
            .eq(CLUSTERS)
    );
    let cluster_allocations = count_allocations(|| {
        for _ in 0..16 {
            terminal.cell_frame_into(black_box(&mut frame), 2, 32);
        }
    });
    eprintln!("warm_frame_extras repeats=16 allocations={cluster_allocations}");
    assert_eq!(
        cluster_allocations, 0,
        "unchanged emoji and combining payloads should reuse their owned frame storage"
    );
    let historical_clusters = count_allocations(|| {
        for _ in 0..16 {
            for cluster in CLUSTERS {
                let mut scalars = cluster.chars();
                let base = scalars.next().unwrap();
                let mut text = String::with_capacity(2 + scalars.clone().count());
                text.push(base);
                text.extend(scalars);
                black_box(text.into_boxed_str());
            }
        }
    });
    assert!(
        historical_clusters > cluster_allocations,
        "the scalar-count reservation negative control must allocate more"
    );
}

const CEILING: u64 = 64;

fn selective_truecolor_erase_reuses_warm_storage() {
    use aterm_core::terminal::Terminal;
    let mut worst = 0;
    for (rows, cols) in [(4, 80), (24, 160)] {
        for protected in [false, true] {
            let mut term = Terminal::new(rows, cols);
            term.process(b"\x1b[48;2;10;20;30m");
            if protected {
                term.process(b"\x1b[1\"qP\x1b[0\"q");
            }
            term.process(b"\x1b[?2J");
            let allocations = count_allocations(|| {
                for step in 0..64 {
                    term.process(if step % 2 == 0 {
                        b"\x1b[48;2;40;50;60m\x1b[?2J"
                    } else {
                        b"\x1b[48;2;70;80;90m\x1b[?2J"
                    });
                }
            });
            eprintln!(
                "selective_bce rows={rows} cols={cols} protected={protected} erases=64 allocations={allocations}"
            );
            worst = worst.max(allocations);
            for row in 0..rows {
                for col in 0..cols {
                    let kept = protected && row == 0 && col == 0;
                    assert_eq!(
                        term.grid().bg_rgb_at(row, col),
                        Some(if kept { [10, 20, 30] } else { [70, 80, 90] })
                    );
                    if kept {
                        assert_eq!(term.grid().cell(row, col).unwrap().char(), 'P');
                    }
                }
            }
        }
    }
    assert_eq!(
        worst, 0,
        "warm selective erases should not allocate column scratch"
    );
}

fn hot_scrollback_walks_allocate_only_owned_line_payloads() {
    use aterm_scrollback::{Line, Scrollback, ScrollbackStorage};
    const ROWS: usize = 4_096;
    let long = "x".repeat(256);
    for text in ["inline hot row", long.as_str()] {
        let mut scrollback = Scrollback::new(ROWS * 2, 1 << 20, 64 << 20);
        for i in 0..ROWS {
            let mut line = Line::from(text);
            line.set_wrapped(i % 2 == 1);
            scrollback.push_line(line);
        }
        let direct = count_allocations(|| {
            for (i, line) in scrollback.iter().enumerate() {
                assert_eq!(line.as_bytes(), text.as_bytes());
                assert_eq!(line.is_wrapped(), i % 2 == 1);
                std::hint::black_box(line);
            }
        });
        let storage: ScrollbackStorage = scrollback.into();
        let owning = count_allocations(|| {
            assert_eq!(storage.iter().count(), ROWS);
        });
        let borrowing = count_allocations(|| {
            assert_eq!(storage.dense_from(0).count(), ROWS);
        });
        let payloads = if text.len() > 32 { ROWS as u64 } else { 0 };
        eprintln!(
            "hot_walk bytes_per_row={} direct_allocations={direct} storage_allocations={owning} dense_allocations={borrowing}",
            text.len()
        );
        assert_eq!(direct, payloads);
        assert_eq!(owning, payloads);
        assert_eq!(borrowing, 0);
    }
}

#[test]
fn engine_steady_state_processing_is_allocation_free() {
    image_history_restore_reuses_layer_storage();
    linked_history_restore_reuses_payload_storage();
    linked_writes_reuse_payload_and_overwrite_storage();
    floating_frame_reuses_warm_storage();
    selective_truecolor_erase_reuses_warm_storage();
    hot_scrollback_walks_allocate_only_owned_line_payloads();
    let measure = 2_000;

    let ascii = steady_state_alloc_calls(&workload(measure));
    eprintln!("PERF-BASELINE (ascii):   {measure} lines at steady-state -> {ascii} allocations");
    assert!(
        ascii <= CEILING,
        "steady-state processing of {measure} ascii lines made {ascii} allocations (ceiling \
         {CEILING}) — the engine is allocating per line/cell instead of reusing the ring"
    );

    let u_n = steady_state_alloc_calls(&unicode_workload(measure));
    let u_2n = steady_state_alloc_calls(&unicode_workload(measure * 2));
    eprintln!(
        "PERF-BASELINE (unicode): {measure} lines -> {u_n} allocs, {} lines -> {u_2n} allocs (ratio {:.2}x)",
        measure * 2,
        u_2n as f64 / u_n.max(1) as f64
    );
    assert!(
        u_2n <= u_n.max(1) * 3,
        "unicode allocation scaled super-linearly: {u_n} -> {u_2n} for 2x input (ceiling {}) \
         — the CJK/emoji write path has a quadratic-allocation regression",
        u_n.max(1) * 3
    );

    render_extras_avoid_temporary_allocations();
}
