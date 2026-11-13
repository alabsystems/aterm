// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

#[test]
fn a_doc_comment_does_not_end_mid_sentence() {
    let rust = "//! A module that\n\
                //! does one thing.\n\
                \n\
                /// Reads the vault and\n\
                fn cut() {}\n\
                /// The current index,\n\
                struct Comma;\n\
                /// Reads the vault.\n\
                fn whole() {}\n\
                /// Each cell, except that\n\
                ///\n\
                /// * a zero-width one joins the cell before it.\n\
                fn list() {}\n\
                /// # Where it comes from\n\
                fn heading() {}\n\
                // A plain comment ends on the\n\
                fn plain() { let x = 1; // a trailing one is code's\n\
                }\n\
                const S: &str = \"\n\
                /// a string's text and\n\
                \";\n\
                /// The band's base\n\
                fn noun() {}\n\
                /// Something can be zoomed in\n\
                const ZOOM_IN: u32 = 0x21;\n\
                /// Built in the\n\
                //! module doc after an item doc\n\
                fn mixed() {}\n\
                /// Reads the vault and\n\
                /// # Errors\n\
                /// Each cell, except that\n\
                ///\n\
                /// When it cannot.\n\
                fn errs() {}\n\
                /// ```\n\
                /// let x = f(1,\n\
                /// # 2);\n\
                /// ```\n\
                fn fenced() {}\n\
                /// Phase 1 of\n\
                /// #7877.\n\
                fn issue() {}\n\
                /// Human semver, e.g.\n\
                const V: &str = \"0.2.0\";\n\
                //! A model re-implemented with\n\
                #![cfg_attr(\n\
                    unix,\n\
                    doc = \"[`pump`]\"\n\
                )]\n\
                //! on every desktop.\n\
                /// Read by the\n\
                #[doc = \"loop.\"]\n\
                fn spelled() {}\n\
                /// Reads the ledger and\n\
                #[inline]\n\
                fn attributed() {}\n\
                /// Each lane, checked by:\n\
                fn listless() {}\n\
                /// Each lane, checked by:\n\
                ///\n\
                /// * its own baseline.\n\
                fn listed() {}\n\
                /// The steps:\n\
                /// # Errors\n\
                /// When it cannot.\n\
                fn above_heading() {}\n\
                // The layout:\n\
                fn plain_colon() {}\n\
                /// # Example:\n\
                fn heading_colon() {}\n\
                /// ```\n\
                /// use aterm_x::f;\n\
                /// ```\n\
                /// use aterm_x::f;\n\
                fn echoed_fence() {}\n";
    let found = run(&DOC_SENTENCE, &[("crates/aterm-x/src/lib.rs", rust)]);
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-x/src/lib.rs:4",
            "crates/aterm-x/src/lib.rs:6",
            "crates/aterm-x/src/lib.rs:16",
            "crates/aterm-x/src/lib.rs:26",
            "crates/aterm-x/src/lib.rs:29",
            "crates/aterm-x/src/lib.rs:43",
            "crates/aterm-x/src/lib.rs:54",
            "crates/aterm-x/src/lib.rs:57",
            "crates/aterm-x/src/lib.rs:63",
            "crates/aterm-x/src/lib.rs:74",
        ]
    );
    assert_eq!(
        found[0].detail,
        "a doc comment ends mid-sentence: `…Reads the vault and`"
    );
    assert_eq!(
        found[7].detail,
        "a doc comment ends on a colon with nothing after it: `…Each lane, checked by:`"
    );
}

#[test]
fn a_paragraph_or_safety_note_is_not_left_open() {
    let rust = "/// Whether a run writes beside its loop (its journal.\n\
                fn paren() {}\n\
                /// Encode `` into a 20-byte snapshot.\n\
                fn emptied() {}\n\
                /// The answer to \"a build is staged.\n\
                fn quote() {}\n\
                /// Exit (the cure), or.\n\
                enum OnPoison {}\n\
                /// Encode `(self, a)` into a snapshot (`u32`, then `u64`).\n\
                fn closed() {}\n\
                /// The run `[from, to)`; a literal \\` stays prose.\n\
                fn range() {}\n\
                /// German Shift+2 types '\"' and Shift+9 types '('.\n\
                fn quoted() {}\n\
                /// A first paragraph (cut here.\n\
                ///\n\
                /// A second one.\n\
                fn two() {}\n\
                fn f() {\n\
                // SAFETY: `hwnd` is live and\n\
                unsafe { g() };\n\
                // SAFETY: the out-pointers name locals (whose lengths.\n\
                unsafe { g() };\n\
                // SAFETY: both pointers are live; the call is synchronous.\n\
                unsafe { g() };\n\
                // SAFETY: `-setContentMinSize:`.\n\
                unsafe { g() };\n\
                // SAFETY: `-frame` is `{CGRect}@:`.\n\
                unsafe { g() };\n\
                // A plain note (cut here.\n\
                g();\n\
                // The ids in `(lo, head]`, then `ESC ( B`.\n\
                g();\n\
                // 127.0.0.1:9\".into()\n\
                }\n\
                /// The keys of the half-open run (t, next].\n\
                fn half_open() {}\n\
                /// A cut with a list after it (t, [next].\n\
                fn bracketed() {}\n\
                /// Hashed with FNV-1a.\n\
                fn hashed() {}\n\
                /// Named pool-a.\n\
                fn pooled() {}\n\
                /// Whether it ends on the.\n\
                fn article() {}\n\
                fn h() {\n\
                // SAFETY: the pointer outlives the.\n\
                unsafe { g() };\n\
                }\n";
    let found = run(&DOC_SENTENCE, &[("crates/aterm-x/src/lib.rs", rust)]);
    assert_eq!(
        spots(&found),
        [1, 3, 5, 7, 15, 20, 22, 26, 30, 34, 38, 44, 47]
            .map(|no| format!("crates/aterm-x/src/lib.rs:{no}"))
    );
    assert_eq!(
        found[5].detail,
        "a SAFETY note ends mid-sentence: `…SAFETY: `hwnd` is live and`"
    );
    assert_eq!(
        found[7].detail,
        "a SAFETY note names only code: `…SAFETY: `-setContentMinSize:`.`"
    );
    assert_eq!(
        found[9].detail,
        "a comment ends mid-sentence: `…127.0.0.1:9\".into()`"
    );
}

/// A comment's own line may open on any white space, ASCII or not, and is read; a line of
/// a literal that only looks like one is not.
#[test]
fn a_comment_indented_by_unicode_space_is_read() {
    let at = "crates/x/src/lib.rs";
    let cut = "fn a() {}\n\u{3000}// The vault and\nfn b() {}\n";
    assert_eq!(
        spots(&run(&DOC_SENTENCE, &[(at, cut)])),
        [format!("{at}:2")]
    );
    let whole = "fn a() {}\n\u{3000}// The vault.\nfn b() {}\n";
    assert!(run(&DOC_SENTENCE, &[(at, whole)]).is_empty());
    let literal = "const S: &str = \"\n\u{3000}// The vault and\n\";\n";
    assert!(run(&DOC_SENTENCE, &[(at, literal)]).is_empty());
}
