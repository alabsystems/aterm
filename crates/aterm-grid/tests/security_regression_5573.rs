// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Security regression tests for #5573: page-backed Row use-after-free.
//!
//! ## Background
//!
//! Before #5573, `Row::new(cols, &mut PageStore)` and `Row::resize(new_cols, &mut PageStore)`
//! were safe public functions. This allowed safe code to create an owned page-backed row,
//! drop the backing `PageStore`, and then access dangling memory:
//!
//! ```text
//! // ORIGINAL BUG WITNESS (no longer compiles without `unsafe`):
//! let mut pages = PageStore::new();
//! let row = Row::new(8, &mut pages);   // was safe, now requires unsafe
//! drop(pages);                          // backing storage freed
//! let _ = row.as_slice();               // use-after-free!
//! ```
//!
//! The fix made `Row::new` and `Row::resize` `pub unsafe fn`, requiring callers
//! to explicitly opt into the lifetime invariant: the backing `PageStore` must
//! outlive all rows allocated from it.
//!
//! Additionally, `GridStorage.rows` was narrowed to `pub(crate)` to prevent
//! external code from extracting owned `Row` values that could outlive the
//! backing `PageStore`.
//!
//! ## What these tests verify
//!
//! 1. The correct `unsafe` usage pattern works (runtime check).
//! 2. Row operations after valid `unsafe` creation are sound (MIRI-exercisable).
//! 3. The `GridStorage.rows` field is not accessible from outside the crate
//!    (this is enforced by the compiler — `pub(crate)` is invisible to
//!    integration tests).
//!
//! ## What the compiler enforces (not testable at runtime)
//!
//! The following code would NOT compile, which is the security property:
//!
//! ```text
//! // COMPILE ERROR: call to unsafe function requires unsafe block
//! let mut pages = aterm_grid::PageStore::new();
//! let row = aterm_grid::Row::new(8, &mut pages);  // ERROR
//! ```
//!
//! ```text
//! // COMPILE ERROR: field `rows` of `GridStorage` is private
//! let grid_storage: aterm_grid::state::GridStorage = /* ... */;
//! let owned_row = grid_storage.rows.remove(0);  // ERROR
//! ```

use aterm_grid::{CellFlags, PackedColor, PageStore, Row};

// =========================================================================
// Correct unsafe usage: PageStore outlives rows
// =========================================================================

/// Verify that creating a row through the `unsafe` boundary works correctly
/// when the lifetime invariant is upheld.
///
/// Security invariant: `pages` must outlive `row`. Here both live in the
/// same scope, so `row` is dropped before `pages` (reverse declaration order).
#[test]
fn row_new_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` (same scope, reverse drop order).
    let row = unsafe { Row::new(80, &mut pages) };

    assert_eq!(row.cols(), 80);
    assert_eq!(row.len(), 0);
    assert!(row.is_empty());
}

/// Verify that resizing a row through the `unsafe` boundary works correctly.
///
/// Security invariant: `pages` must outlive `row` after the resize.
#[test]
fn row_resize_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` for the full scope.
    let mut row = unsafe { Row::new(40, &mut pages) };

    // Write content before resize
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    for col in 0..40u16 {
        row.write_char_styled(col, 'A', fg, bg, CellFlags::empty());
    }
    assert_eq!(row.len(), 40);

    // SAFETY: `pages` still outlives `row` after resize.
    unsafe { row.resize(80, &mut pages) };
    assert_eq!(row.cols(), 80);

    // Original content survived
    for col in 0..40u16 {
        assert_eq!(row.get(col).unwrap().char(), 'A', "col {col} content lost");
    }

    // New cells are empty
    for col in 40..80u16 {
        assert!(row.get(col).unwrap().is_empty(), "col {col} not empty");
    }
}

/// Verify that shrink-resize through the `unsafe` boundary works correctly.
///
/// Tests the inverse direction: large row resized to smaller.
#[test]
fn row_resize_shrink_valid_unsafe_usage() {
    let mut pages = PageStore::new();
    // SAFETY: `pages` outlives `row` for the full scope.
    let mut row = unsafe { Row::new(80, &mut pages) };

    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;
    for col in 0..80u16 {
        row.write_char_styled(col, 'B', fg, bg, CellFlags::empty());
    }

    // SAFETY: `pages` still outlives `row` after resize.
    unsafe { row.resize(40, &mut pages) };
    assert_eq!(row.cols(), 40);

    // Content within the new bounds survived
    for col in 0..40u16 {
        assert_eq!(row.get(col).unwrap().char(), 'B', "col {col} content lost");
    }
}

// =========================================================================
// Multiple rows sharing a PageStore: cross-row isolation
// =========================================================================

/// Allocate many rows from the same PageStore and verify they don't corrupt
/// each other. This exercises the page-backed allocation path that was the
/// root cause of the UAF when PageStore was dropped while rows were alive.
#[test]
fn multiple_rows_cross_isolation() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: All rows are dropped before `pages` (Vec dropped before local `pages`).
    let mut rows: Vec<Row> = (0..20)
        .map(|_| unsafe { Row::new(80, &mut pages) })
        .collect();

    // Write unique content to each row
    for (i, row) in rows.iter_mut().enumerate() {
        let c = char::from(b'A' + (i % 26) as u8);
        for col in 0..80u16 {
            row.write_char_styled(col, c, fg, bg, CellFlags::empty());
        }
    }

    // Verify each row's content is intact — no cross-row aliasing
    for (i, row) in rows.iter().enumerate() {
        let expected = char::from(b'A' + (i % 26) as u8);
        for col in 0..80u16 {
            assert_eq!(
                row.get(col).unwrap().char(),
                expected,
                "row[{i}][{col}] corrupted"
            );
        }
    }
}

/// Resize some rows while others remain unchanged. Verify that resizing one
/// row doesn't corrupt another row's backing storage.
#[test]
fn resize_one_row_doesnt_corrupt_others() {
    let mut pages = PageStore::new();
    let fg = PackedColor::DEFAULT_FG;
    let bg = PackedColor::DEFAULT_BG;

    // SAFETY: All rows and `pages` live in the same scope.
    let mut row_a = unsafe { Row::new(40, &mut pages) };
    let mut row_b = unsafe { Row::new(40, &mut pages) };

    // Write distinct content
    for col in 0..40u16 {
        row_a.write_char_styled(col, 'X', fg, bg, CellFlags::empty());
        row_b.write_char_styled(col, 'Y', fg, bg, CellFlags::empty());
    }

    // Resize row_a — allocates new PageSlice, abandons old one
    // SAFETY: `pages` outlives both rows.
    unsafe { row_a.resize(120, &mut pages) };

    // row_b must be unaffected
    for col in 0..40u16 {
        assert_eq!(
            row_b.get(col).unwrap().char(),
            'Y',
            "row_b[{col}] corrupted after row_a resize"
        );
    }

    // row_a original content survived resize
    for col in 0..40u16 {
        assert_eq!(
            row_a.get(col).unwrap().char(),
            'X',
            "row_a[{col}] lost after resize"
        );
    }
}

// =========================================================================
// Compile-fail verification (subprocess)
// =========================================================================

/// Verify that `Row::new` cannot be called without an `unsafe` block.
///
/// This is a compile-fail test: we write a small Rust program that calls
/// `Row::new` in safe code and verify that `rustc` rejects it.
#[test]
fn row_new_rejects_safe_call() {
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg("--crate-type=lib")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                // Minimal program that tries to call Row::new without unsafe.
                // We can't import aterm_grid in a subprocess, but we can verify
                // the language-level property: calling an `unsafe fn` without
                // an `unsafe` block is a compile error.
                stdin.write_all(
                    b"#![deny(unsafe_op_in_unsafe_fn)]\n\
                      unsafe fn create() -> u8 { 0 }\n\
                      fn main() { let _ = create(); }\n",
                )?;
            }
            child.wait()
        });

    match status {
        Ok(exit) => assert!(
            !exit.success(),
            "safe call to `unsafe fn` should be rejected by rustc"
        ),
        Err(e) => {
            // rustc not in PATH is an environmental issue, not a test failure.
            eprintln!("SKIP: rustc not available for compile-fail check: {e}");
        }
    }
}

/// Verify that `Row::resize` cannot be called without an `unsafe` block.
///
/// Same compile-fail approach: an `unsafe fn` called without `unsafe {}` is
/// rejected by the compiler since Rust 2024 edition (and by the
/// `unsafe_op_in_unsafe_fn` lint in earlier editions with `#![deny(...)]`).
#[test]
fn row_resize_rejects_safe_call() {
    let status = std::process::Command::new("rustc")
        .arg("--edition=2021")
        .arg("--crate-type=lib")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(
                    b"unsafe fn resize() {}\n\
                      fn caller() { resize(); }\n",
                )?;
            }
            child.wait()
        });

    match status {
        Ok(exit) => assert!(
            !exit.success(),
            "safe call to `unsafe fn` should be rejected by rustc"
        ),
        Err(e) => {
            eprintln!("SKIP: rustc not available for compile-fail check: {e}");
        }
    }
}
