// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The facade in its consumer's shape: wasm-bindgen-macro-support's
//! `Interner` (src/encode.rs:43-82 at 0.2.108).

use std::cell::RefCell;

struct Interner {
    bump: bumpalo::Bump,
    // A second borrow of `self` while an interned string is alive, as the
    // encoder's file table is.
    files: RefCell<Vec<String>>,
}

impl Interner {
    fn new() -> Interner {
        Interner {
            bump: bumpalo::Bump::new(),
            files: RefCell::new(Vec::new()),
        }
    }

    fn intern_str(&self, s: &str) -> &str {
        self.bump.alloc_str(s)
    }
}

#[test]
fn an_interned_string_is_an_equal_copy_that_outlives_its_source() {
    let i = Interner::new();
    let kept = {
        let temp = String::from("AtermTerminal");
        i.intern_str(&temp)
    };
    i.files.borrow_mut().push("snippet.js".to_string());
    assert_eq!(kept, "AtermTerminal");
    assert_eq!(i.files.borrow().len(), 1);
}

#[test]
fn every_interned_string_keeps_its_own_bytes() {
    let i = Interner::new();
    let inputs = ["", "init", "init_offscreen", "\u{1F600} non-ASCII", "init"];
    let got: Vec<&str> = inputs.iter().map(|s| i.intern_str(s)).collect();
    assert_eq!(got, inputs);
    // Two copies of one string are two allocations, as in upstream's arena.
    assert_ne!(got[1].as_ptr(), got[4].as_ptr());
}

#[test]
fn the_copy_is_mutable_and_independent_of_the_source() {
    let bump = bumpalo::Bump::default();
    let src = String::from("abc");
    let copy = bump.alloc_str(&src);
    copy.make_ascii_uppercase();
    assert_eq!(copy, "ABC");
    assert_eq!(src, "abc");
}
