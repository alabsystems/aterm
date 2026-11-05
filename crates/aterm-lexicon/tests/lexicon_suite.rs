// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration test suite for `aterm-lexicon`.

/// Webster's Second from the first of this host's places aterm_uds's `web2` row names that
/// reads. It is a word list, not a program, so it is read where it lies rather than resolved
/// as a runnable tool (which wants an execute bit macOS's mode-0444 copy has not).
fn web2_words() -> String {
    let row = aterm_spec::host::tool("web2");
    let places: Vec<&str> = row.places().collect();
    places
        .iter()
        .find_map(|at| std::fs::read_to_string(at).ok())
        .unwrap_or_else(|| {
            aterm_spec::host::could_not_run(format!(
                "the dictionary gates walk Webster's Second, the list their allowlists are \
                 curated against, and none reads here (looked at {}): {}",
                places.join(" and "),
                row.remedy()
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
