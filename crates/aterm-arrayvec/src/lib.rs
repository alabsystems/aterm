// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `arrayvec` facade re-exporting [`aterm_alloc::ArrayVec`],
//! [`IntoIter`](aterm_alloc::ArrayVecIntoIter), [`Drain`](aterm_alloc::ArrayVecDrain),
//! and [`CapacityError`](aterm_alloc::CapacityError).
//!
//! Published under package name `arrayvec` in `crates/aterm-arrayvec` so
//! `[patch.crates-io]` redirects `naga`, `wgpu`, `wgpu-core`, and `wgpu-hal`
//! to `aterm_alloc` (`#![no_std]`).
//!
//! ## Verification
//!
//! * `tests/consumer_forms.rs` compiles and runs every `ArrayVec` invocation
//!   form across all four consumers, including SPIR-V, DX12, GLES, and Vulkan
//!   sites that are `cfg`'d off in macOS host builds.
//! * `crates/aterm-alloc/tests/arrayvec_differential.rs` checks operations
//!   and deterministic LCG fuzz sequences against a `Vec`-backed reference model.
//!
//! ## Intentional Divergences from Upstream
//!
//! * **Overflow panic message**: `extend`/`from_iter` panics with
//!   `"ArrayVec overflow: capacity exceeded"` (matching `aterm-alloc`'s
//!   `#[trust::contract_panic]` annotation).
//! * **Length field width**: stores `len` as `usize` rather than `u32`
//!   (identical size for 8-aligned `T` such as `Arc<TextureView>`).
//! * **`Drain` drop/forget safety**: removes elements as they are yielded
//!   rather than truncating up front, avoiding element loss on `mem::forget`
//!   or leaks on panicking destructors.
//! * **`IntoIter` move cost**: pops from the front via `remove(0)` (`O(n²)`
//!   moves, with max consumer capacity `<= 32` in setup paths) so `IntoIter`
//!   wraps `ArrayVec` directly without manual drop-range bookkeeping.
//! * **`ArrayString`**: omitted because no crate in the dependency graph uses it.

#![no_std]

// The type itself, and the two iterator types its `IntoIterator` / `drain`
// produce. Renamed back to upstream's spellings here — this is the one place
// the names have to match somebody else's API.
pub use aterm_alloc::{
    ArrayVec, ArrayVecDrain as Drain, ArrayVecIntoIter as IntoIter, CapacityError,
};
