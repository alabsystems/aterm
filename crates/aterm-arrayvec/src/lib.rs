// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `arrayvec` — aterm's first-party replacement for the upstream `arrayvec`
//! crate. It contains no implementation: every item is a re-export of
//! [`aterm_alloc`], which has carried `ArrayVec` since long before this shim
//! existed.
//!
//! This crate is published into the build under the package name `arrayvec`
//! (the directory is `crates/aterm-arrayvec`; see the manifest for why the two
//! names differ) so that `[patch.crates-io]` can redirect the six third-party
//! consumers in aterm's graph — `naga`, `wgpu`, `wgpu-core`, `wgpu-hal`,
//! `tiny-skia` (Linux only, under `sctk-adwaita`) and `vte` (dev edges only) —
//! at code we own.
//!
//! # Why this replacement is safe, and how that was established
//!
//! The claim is narrower and more checkable than "our ArrayVec is as good as
//! theirs". It is: **every item the six consumers name resolves, with the same
//! signature and the same behaviour.** Three things back it up.
//!
//! 1. **The surface was enumerated from the consumers' source, not guessed.**
//!    62 distinct `ArrayVec<…>` uses across the six crates were read out of the
//!    registry copies and reduced to a list of methods, trait impls and syntactic
//!    forms. Seven impls were MISSING from `aterm_alloc::ArrayVec` and were
//!    written for this swap: `Hash`, `Extend`, `IntoIterator` by value, by `&`
//!    and by `&mut`, `into_inner` and `drain`. Two more differed in SIGNATURE
//!    (`try_push`'s error type, `retain`'s closure bound) and were changed to
//!    upstream's. They all live in `crates/aterm-alloc/src/array_vec.rs`,
//!    because the orphan rule puts them there: a foreign trait on a foreign
//!    type cannot be implemented here.
//!
//! 2. **`tests/consumer_forms.rs` compiles every invocation form verbatim.**
//!    Each block is commented with the crate, file and line it was copied from.
//!    Compiling that file is the proof that the surface accepts what the real
//!    consumers write — and it has to be, because no cell compiles all of what
//!    they write, and this one compiles least of all. On macOS the four wgpu-
//!    family crates do build, but with the feature sets that switch most of
//!    their `ArrayVec` sites off; `tiny-skia` never enters the graph at all
//!    (it arrives under Linux's `sctk-adwaita`); and `vte`'s single use is
//!    `cfg`'d out by the `std` feature it resolves with. See the per-cell note
//!    below for exactly which impls that leaves untested here.
//!
//! 3. **A differential oracle runs this type and the real crates.io `arrayvec`
//!    through the same operation scripts and asserts they agree**, element by
//!    element, hash by hash, panic by panic — including a deterministic-LCG
//!    op-sequence fuzz. It lives at
//!    `crates/aterm-alloc/tests/arrayvec_differential.rs` rather than here,
//!    because a dev-dependency on the registry `arrayvec` declared by the
//!    package that is itself *named* `arrayvec` makes `cargo test -p arrayvec`
//!    ambiguous; the manifests on both sides carry the measurement. The oracle
//!    is a dev-dependency either way, so it is invisible to the shipped graph.
//!
//!    Re-check the whole claim with:
//!
//!    ```text
//!    cargo test -p arrayvec                    # the consumer forms
//!    cargo test -p aterm-alloc                 # the differential + unit tests
//!    ```
//!
//!    Both in one command needs the disambiguated spec —
//!    `cargo test -p arrayvec@0.7.8 -p aterm-alloc` — because the oracle keeps
//!    a second `arrayvec` in the graph. Bare `-p arrayvec` on its own is
//!    unambiguous; bare `-p arrayvec -p aterm-alloc` is not. Measured both
//!    ways.
//!
//! ## The per-cell scope, stated plainly
//!
//! `cargo test -p arrayvec` on macOS does NOT compile the consumers, and a
//! `cargo check -p aterm-gui` on macOS compiles only a *fraction* of the
//! surface: naga resolves with `default,msl-out,wgsl-in` (so its SPIR-V writer
//! is `cfg`'d off) and wgpu-hal with `metal,portable-atomic` (and the Metal
//! backend contains zero `ArrayVec` uses). That cell exercises `Hash`,
//! `Extend`, by-value `IntoIterator` and `into_inner`; it does NOT exercise
//! `drain`, by-`&`/`&mut` `IntoIterator`, `new_const` or `clone_from`. Windows
//! and Linux (`dx12,gles,vulkan` + naga's `spv-out`, plus tiny-skia on Linux)
//! are the cells that cover the whole surface. `tests/consumer_forms.rs` exists
//! precisely to close that gap here, on any cell.
//!
//! # Where the replacement is NOT exactly upstream
//!
//! Four divergences, all deliberate, none reachable by anything in the graph:
//!
//! * **Capacity-panic message.** Upstream's `extend`/`from_iter` overflow says
//!   `ArrayVec: capacity exceeded in extend/from_iter`; ours says
//!   `ArrayVec overflow: capacity exceeded`, because that literal is what
//!   `crates/aterm-alloc`'s `#[trust::contract_panic(message_contains = …)]`
//!   annotation binds. Same panic, same place, different words. Nothing in the
//!   graph matches on the text.
//! * **`size_of`.** Upstream stores the length as a `u32`; `aterm_alloc` stores
//!   a `usize`. That is a divergence, but a smaller one than it sounds, and the
//!   numbers are measured rather than reasoned (aarch64-apple-darwin, ours vs
//!   upstream): `ArrayVec<u8, 4>` 16 vs 8, `ArrayVec<u32, 8>` 40 vs 36,
//!   `ArrayVec<u64, 4>` 40 vs 40, `ArrayVec<Arc<u32>, 3>` 32 vs 32. Once `T` is
//!   8-aligned, upstream's `u32` is padded to 8 anyway and the two are
//!   IDENTICAL — which covers the `Arc<TextureView>` / handle-carrying vectors
//!   that dominate the wgpu side. The growth is real only for small, weakly
//!   aligned `T`. No consumer puts an `ArrayVec` in a `#[repr(C)]` type either,
//!   so this is not an ABI break. `crates/aterm-alloc/tests/arrayvec_differential.rs`
//!   pins the relation so a future layout change cannot widen it unnoticed.
//! * **`Drain` under `mem::forget`.** Upstream shortens the vector up front and
//!   restores the tail in `Drop`, so forgetting a `Drain` loses the un-drained
//!   elements; ours removes as it yields, so forgetting one keeps them. Ours is
//!   the safer answer and nothing in the graph forgets a `Drain`.
//! * **`IntoIter` cost.** Ours is O(n²) in element moves rather than O(n),
//!   because it moves each element out with `remove(0)` instead of an index
//!   cursor plus a hand-written `Drop`. The largest capacity anywhere in the
//!   graph is 32 and every by-value iteration site is pipeline/attachment
//!   setup rather than per-frame work. The reason for the trade is written out
//!   on `aterm_alloc::ArrayVecIntoIter`: holding the `ArrayVec` makes the
//!   "leak an `Arc` on every early return" and "double-free the yielded
//!   elements" failure modes unrepresentable instead of merely untested.
//!
//! And one property that is NOT yet what the manifest's `std` feature implies:
//!
//! * **`#![no_std]` is true of this crate and not yet of the graph below it.**
//!   `crates/aterm-alloc/src/array_vec.rs` names only `core` items (re-check:
//!   `grep -n 'std::' crates/aterm-alloc/src/array_vec.rs` — every hit is
//!   inside its `#[cfg(test)]` module, bar the line that says so), but
//!   `aterm-alloc` itself is not
//!   `#![no_std]`, because its *other* module, `small_vec.rs`, has a heap
//!   fallback that names `Vec`. So a consumer taking this crate with
//!   `default-features = false` — which all six do — still links `std`
//!   transitively. That changes nothing for aterm: all four cells are std
//!   targets and `naga`/`tiny-skia`/`vte` are compiled into a std binary
//!   regardless. The remaining work to make the claim unconditional is
//!   `extern crate alloc` in `aterm-alloc` plus `Vec`/`String` imports in
//!   `small_vec.rs` and the two test modules; it is not done here because it
//!   touches a crate on aterm's hot parser path for no measurable gain.
//!
//! # Surface
//!
//! Upstream's three public exports are [`ArrayVec`], `ArrayString` and
//! [`CapacityError`]. `ArrayString` is deliberately absent: no crate in aterm's
//! graph names it, and a half-written one would be worse than none. The two
//! iterator types [`IntoIter`] and [`Drain`] are re-exported under upstream's
//! names (they are `ArrayVecIntoIter` / `ArrayVecDrain` inside `aterm_alloc`,
//! where the flat `IntoIter` spelling already belongs to `SmallVec`).

#![no_std]

// The type itself, and the two iterator types its `IntoIterator` / `drain`
// produce. Renamed back to upstream's spellings here — this is the one place
// the names have to match somebody else's API.
pub use aterm_alloc::{
    ArrayVec, ArrayVecDrain as Drain, ArrayVecIntoIter as IntoIter, CapacityError,
};
