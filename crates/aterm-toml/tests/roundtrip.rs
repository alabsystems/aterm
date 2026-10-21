// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Every `.toml` file in the repository parses, and parse then print reproduces it byte for
//! byte: a rewrite of one key (the Preferences window, `vendor/forge.toml` tooling) must
//! leave the rest of the file untouched.

use crate::common;

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
        if let Err(e) = aterm_toml::from_str::<aterm_toml::Value>(&text) {
            failures.push(format!("{}: value parse failed: {e}", path.display()));
        }
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

/// Formatting the crate must preserve, spelled out so a regression names itself instead
/// of pointing into a large corpus file.
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
