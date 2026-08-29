// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The round-trip gate: parse then print must reproduce the source BYTE FOR
//! BYTE, for every `.toml` file in this repository.
//!
//! Comment and formatting preservation is the entire contract of the document
//! half — `cargo forge` asserts that rewriting `vendor/forge.toml` does not
//! move a single byte, and the Preferences window promises a user that saving a
//! font size will not reflow their config. Anything less than byte equality on
//! an unmodified document breaks both.

mod common;

use common::corpus;

#[test]
fn every_toml_file_in_the_repository_round_trips_byte_for_byte() {
    let mut checked = 0usize;
    let mut bytes = 0usize;
    let mut failures = Vec::new();

    for path in corpus() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let document: aterm_toml::edit::DocumentMut = match text.parse() {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{}: parse failed: {e}", path.display()));
                continue;
            }
        };
        let printed = document.to_string();
        if printed != text {
            failures.push(format!(
                "{}: round-trip differs\n  first difference at byte {}",
                path.display(),
                first_difference(&text, &printed)
            ));
        }
        checked += 1;
        bytes += text.len();
    }

    assert!(
        failures.is_empty(),
        "{} files failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("round-trip: {checked} files / {bytes} bytes reproduced exactly");
}

fn first_difference(a: &str, b: &str) -> String {
    let offset = a
        .bytes()
        .zip(b.bytes())
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let window = |s: &str| {
        let start = offset.saturating_sub(40).min(s.len());
        let end = (offset + 40).min(s.len());
        s[start..end].to_string()
    };
    format!(
        "{offset}\n  ours:   {:?}\n  source: {:?}",
        window(b),
        window(a)
    )
}

/// Formatting the crate must preserve, spelled out so a regression names itself
/// instead of pointing at a 3,000-line manifest.
#[test]
fn formatting_details_survive() {
    for source in [
        "# leading comment\n\nkey = 1 # trailing\n\n[table]  # header comment\nx = 'lit'\n",
        "a.b.c = 1\n# between\na.b.d = 2\n",
        "[ spaced . header ]\nk=1\n",
        "arr = [\n  1, # one\n  2,\n]\n",
        "inline = { a = 1, b = { c = 2 } }\n",
        "empty_inline = {}\nspaced_inline = {   }\n",
        "ml = \"\"\"\nline\\\n  continued\"\"\"\n",
        "lit = '''\nno escapes \\n here'''\n",
        "\"quoted key\" = 1\n'literal key' = 2\n",
        "[[aot]]\nx = 1\n\n[[aot]]\nx = 2\n",
        "[a.b]\nx = 1\n[a]\ny = 2\n",
        "no_trailing_newline = 1",
        "[header_only]",
        "\r\nwindows = 1\r\n",
        "nums = [0x1F, 0o777, 0b1010, 1_000_000, +3.0e2, -0.5, inf, -inf, nan]\n",
        "dates = [1979-05-27T07:32:00Z, 1979-05-27 07:32:00-07:00, 1979-05-27, 07:32:00.999999]\n",
        "# only a comment\n",
        "",
        "\n\n\n",
    ] {
        let document: aterm_toml::edit::DocumentMut = source
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse {source:?}: {e}"));
        assert_eq!(
            document.to_string(),
            source,
            "round-trip changed {source:?}"
        );
    }
}

/// The oracle round-trips these too. Agreeing with `toml_edit` here is what
/// makes the swap safe for `cargo forge`, whose own test asserts byte equality
/// through the crate being replaced.
#[test]
fn the_oracle_round_trips_the_same_corpus() {
    for path in corpus() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(theirs) = text.parse::<toml_edit::DocumentMut>() else {
            continue;
        };
        let ours: aterm_toml::edit::DocumentMut = text
            .parse()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            ours.to_string(),
            theirs.to_string(),
            "{}: our rendering differs from toml_edit's",
            path.display()
        );
    }
}
