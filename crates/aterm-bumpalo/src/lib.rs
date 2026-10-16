// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `bumpalo`: aterm's first-party facade for the crates.io package of the same
//! name, patched in (`[patch.crates-io]`) under wasm-bindgen-macro-support,
//! its only consumer in the lock.
//!
//! # What the consumer calls
//!
//! Two items, in one place: the `Interner` of
//! `wasm-bindgen-macro-support-0.2.108/src/encode.rs` holds a [`Bump`]
//! (`:44`), builds it with [`Bump::new`] (`:65`) and copies each identifier
//! and string the `#[wasm_bindgen]` encoder emits into it with
//! [`Bump::alloc_str`] (`:81`), only so the copy borrows from the interner
//! rather than from a temporary. Nothing else in bumpalo has a caller.
//!
//! # Not equivalent
//!
//! * There is no arena. Each `alloc_str` is its own heap allocation, leaked
//!   with `Box::leak`, so the crate needs no `unsafe`. Upstream frees the
//!   whole arena when the `Bump` drops; here the strings live until the
//!   process exits. The process is the compiler running the proc macro, and
//!   what leaks is the names and strings of the items one crate annotates
//!   with `#[wasm_bindgen]`: kilobytes per crate compiled. A long-lived
//!   proc-macro server (an editor's) leaks that much per re-expansion.
//! * Only [`Bump::new`] and [`Bump::alloc_str`] exist (with `Default` and
//!   `Debug`). Upstream's feature names are declared and inert; a consumer
//!   that reaches for more API fails to compile rather than run on a
//!   stand-in.
//!
//! What the consumer emits does not change: the interner decides what is
//! encoded, and the arena only decided where the copies lived. The
//! wasm-platform step WP.1 (docs/AUDIT-third-party-2026-10-15.md) held this
//! by building both web modules before and after the switch: the
//! `__wasm_bindgen_unstable` custom sections held the same records (reordered
//! by codegen-unit link order), and the `.js`/`.d.ts` the pinned wasm-bindgen
//! CLI generates from them were identical once the mangled `h<hash>`
//! closure-shim names are masked (raw-identical for `aterm-wasm`).
//!
//! Since WP.7 only aterm-gpu-web's build reaches it: aterm-wasm binds with
//! aterm-wasm-abi and has no wasm-bindgen. The crate retires with
//! wasm-bindgen-macro-support at GW.4 + WP.8.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// The arena handle wasm-bindgen's interner holds. It owns nothing: every
/// allocation is leaked on creation (see the crate documentation).
#[derive(Debug, Default)]
pub struct Bump {
    _private: (),
}

impl Bump {
    /// An empty arena.
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Copy `src` into storage that outlives `self`, and return the copy.
    ///
    /// Upstream's signature: the result borrows `self`. The storage is a
    /// leaked `Box<str>`, so the borrow is shorter than the string's life.
    pub fn alloc_str(&self, src: &str) -> &mut str {
        Box::leak(Box::<str>::from(src))
    }
}
