// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The macro in the forms the graph and upstream's documentation use it.

// wgpu's and wgpu-core's one call: an inner doc attribute after the crate's
// own prose (here on a module).
mod consumer {
    //! Crate documentation, then the feature list.
    #![doc = document_features::document_features!()]
}

#[test]
fn the_expansion_is_an_empty_string_literal() {
    const BARE: &str = document_features::document_features!();
    const LABELLED: &str = document_features::document_features!(feature_label = "**`{feature}`**");
    assert_eq!(BARE, "");
    assert_eq!(LABELLED, "");
}
