// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Integration test suite for `aterm-lexicon`.

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
