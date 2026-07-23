// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Engine MEMORY efficiency: aterm vs alacritty_terminal, apples-to-apples.
//!
//! The "more efficient" half of the goal needs evidence too. This measures the
//! net heap a terminal engine RETAINS (allocations minus deallocations) for an
//! identical 24x80 screen — a fresh engine, then one with the screen filled —
//! via a counting global allocator. An integration test is its own crate, so the
//! allocator here does NOT affect the throughput benches.
//!
//! Honest scope: this is the ENGINE's retained heap (grid + state), not RSS, not
//! rendering, and one competitor's engine — not "all competitors". Run:
//!   cargo test -p aterm-bench --test memory -- --nocapture

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, Ordering};

static NET: AtomicI64 = AtomicI64::new(0);

/// System allocator that tracks net live bytes (alloc - dealloc).
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        NET.fetch_add(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        NET.fetch_sub(l.size() as i64, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn net() -> i64 {
    NET.load(Ordering::Relaxed)
}

struct Dims;
impl alacritty_terminal::grid::Dimensions for Dims {
    fn total_lines(&self) -> usize {
        24
    }
    fn screen_lines(&self) -> usize {
        24
    }
    fn columns(&self) -> usize {
        80
    }
}

/// 24 full rows of mixed ASCII+SGR — fills the visible screen without scrolling
/// far past it, so the comparison is dominated by the GRID, not scrollback config.
fn screen_fill() -> Vec<u8> {
    let mut v = Vec::new();
    for r in 0..24 {
        v.extend_from_slice(format!("\x1b[{};1H", r + 1).as_bytes());
        v.extend_from_slice(b"\x1b[1;38;5;202m");
        for c in 0..80 {
            v.push(b'a' + ((r + c) % 26) as u8);
        }
        v.extend_from_slice(b"\x1b[0m");
    }
    v
}

/// Net heap retained while `f`'s result is alive (the engine's grid + state).
fn retained<T>(f: impl FnOnce() -> T) -> (i64, T) {
    let before = net();
    let v = f();
    let held = net() - before;
    (held, v)
}

#[test]
fn engine_memory_comparison() {
    let corpus = screen_fill(); // allocated before measurement; stays alive

    // --- fresh engines (the base grid allocation) ---
    let (aterm_fresh, aterm_t) = retained(|| aterm_core::terminal::Terminal::new(24, 80));
    let (alac_fresh, (alac_t, alac_p)) = retained(|| {
        use alacritty_terminal::Term;
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::term::Config;
        use alacritty_terminal::vte::ansi::Processor;
        let t = Term::new(Config::default(), &Dims, VoidListener);
        let p: Processor = Processor::new();
        (t, p)
    });
    drop(aterm_t);
    drop((alac_t, alac_p));

    // --- engines with the 24x80 screen filled ---
    let (aterm_full, aterm_t) = retained(|| {
        let mut t = aterm_core::terminal::Terminal::new(24, 80);
        t.process(&corpus);
        t
    });
    let (alac_full, holder) = retained(|| {
        use alacritty_terminal::Term;
        use alacritty_terminal::event::VoidListener;
        use alacritty_terminal::term::Config;
        use alacritty_terminal::vte::ansi::Processor;
        let mut t = Term::new(Config::default(), &Dims, VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut t, &corpus);
        (t, p)
    });
    drop(aterm_t);
    drop(holder);

    eprintln!("engine retained heap (24x80, identical input), bytes:");
    eprintln!("  fresh   aterm={aterm_fresh:>9}  alacritty={alac_fresh:>9}");
    eprintln!("  filled  aterm={aterm_full:>9}  alacritty={alac_full:>9}");
    let ratio = |a: i64, b: i64| if b > 0 { a as f64 / b as f64 } else { f64::NAN };
    eprintln!(
        "  aterm/alacritty: fresh={:.2}x  filled={:.2}x  (<1 = aterm more memory-efficient)",
        ratio(aterm_fresh, alac_fresh),
        ratio(aterm_full, alac_full)
    );

    // Sanity only: both engines genuinely retained heap for a real grid.
    assert!(
        aterm_fresh > 0 && alac_fresh > 0,
        "both engines should allocate a grid"
    );
    assert!(
        aterm_full > 0 && alac_full > 0,
        "both engines should retain a filled grid"
    );
}
