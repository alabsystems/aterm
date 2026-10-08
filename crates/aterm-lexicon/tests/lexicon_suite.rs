// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The aterm-lexicon integration files as modules of ONE test binary: each file
//! stays at its own path and keeps its own `#[test]`s, and none installs a
//! global or touches the environment. The two exact-allocation pins
//! (`scan_scratch_alloc`, `tricks_alloc`) are the separate `lexicon_alloc`
//! binary, which owns the counting `#[global_allocator]`.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

mod animal_multilingual;
mod animal_species;
mod corpus;
mod english_fold_collisions;
mod profanity_reaches_major_languages;
mod tricks;
