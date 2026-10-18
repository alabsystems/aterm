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

/// Webster's Second International (`web2`), the English word list the dictionary gates
/// walk. Their allowlists (`INTENTIONAL`, `INTENTIONAL_PREFIXES`, `DICTIONARY_EXCUSED`)
/// are curated against it, and `/usr/share/dict/words` is it only where that names web2
/// (macOS): Debian's `wamerican` adds possessives and inflections the lists never saw. So
/// the gates read it by its own name, and a host without it ends COULD NOT RUN: another
/// list's verdict decides nothing about the allowlists, and no list at all asserts nothing.
fn web2_words() -> String {
    const WEB2: &str = "/usr/share/dict/web2";
    std::fs::read_to_string(WEB2).unwrap_or_else(|e| {
        aterm_spec::host::could_not_run(format!(
            "the dictionary gates walk Webster's Second, the list their allowlists are \
             curated against, and {WEB2} does not read here ({e})"
        ))
    })
}

mod animal_multilingual;
mod animal_species;
mod corpus;
mod english_fold_collisions;
mod profanity_reaches_major_languages;
mod serde_corpus;
mod tricks;
