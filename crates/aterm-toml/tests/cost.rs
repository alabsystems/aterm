// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Parse work is linear in the input: no step repeated once per line may
//! re-read text an earlier line wrote.
//!
//! Pinned by input SIZE, never by a clock: each document here is a fraction of
//! a second of linear work and minutes of quadratic work, so a regression is a
//! test that does not finish rather than a timing that a loaded machine could
//! fake.

use aterm_toml::edit::DocumentMut;

/// One header whose name is a megabyte, then a hundred thousand short
/// key-value lines under it.
///
/// Every line resolves the table the header opened. Looked up by name, that
/// hashes the megabyte once per line (10^11 byte-steps, minutes); by position
/// it costs a step per header SEGMENT.
#[test]
fn many_lines_under_a_long_header_cost_one_pass() {
    const NAME: usize = 1 << 20;
    const LINES: usize = 100_000;
    let name = "h".repeat(NAME);
    let mut source = String::with_capacity(NAME + LINES * 12);
    source.push('[');
    source.push_str(&name);
    source.push_str("]\n");
    for line in 0..LINES {
        source.push('k');
        source.push_str(&line.to_string());
        source.push_str(" = 1\n");
    }

    let document: DocumentMut = source.parse().expect("the document is valid TOML");
    let table = document
        .get(&name)
        .and_then(|item| item.as_table())
        .expect("the header's table");
    assert_eq!(table.iter().count(), LINES);
    assert_eq!(document.to_string(), source, "and it round-trips");

    let value: aterm_toml::Value = aterm_toml::from_str(&source).expect("the serde half agrees");
    assert_eq!(
        value[name.as_str()].as_table().map(aterm_toml::Table::len),
        Some(LINES)
    );
}
