// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Portable fleet display-name vocabulary, independent of network transport.

/// Whether a primary fleet name is a lowercase, bounded human label. The `node-`
/// namespace belongs permanently to automatically assigned temporary aliases.
#[must_use]
pub fn valid_primary_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=63).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        && !name.starts_with("node-")
        && !matches!(name, "localhost" | "self" | "all")
}

/// The permanent temporary alias of a node, retained after any primary-name change.
#[must_use]
pub fn temporary_name(node: &str) -> String {
    format!("node-{node}")
}
