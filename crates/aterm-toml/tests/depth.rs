// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nesting is bounded, and the bound is tested with real deep input.
//!
//! Recursion on untrusted data that has no depth limit is not a bug that
//! returns an error — it aborts the process on a guard-page hit, from inside a
//! config load, with no chance to fall back to defaults. aterm parses TOML that
//! arrives over the update channel and out of package manifests, so "the file
//! was too deep" has to be an `Err` a caller can handle.
//!
//! Four shapes recurse and are therefore all covered here: arrays, inline
//! tables, dotted keys (the parse is a loop, but the TREE it builds is walked
//! recursively by `Drop`, by the encoder, and by the deserializer), and table
//! headers.

/// Deliberately far past the limit, so the test proves an ERROR rather than a
/// slightly-larger limit.
const FAR_TOO_DEEP: usize = 10_000;

fn deep_arrays(levels: usize) -> String {
    format!("a = {}1{}\n", "[".repeat(levels), "]".repeat(levels))
}

fn deep_inline_tables(levels: usize) -> String {
    // Built end-to-end rather than by repeated `format!` wrapping, which would
    // be quadratic at the 50,000-level probe.
    let mut out = String::with_capacity(levels * 8 + 8);
    out.push_str("a = ");
    for _ in 0..levels {
        out.push_str("{ k = ");
    }
    out.push('1');
    for _ in 0..levels {
        out.push_str(" }");
    }
    out.push('\n');
    out
}

fn deep_dotted_key(levels: usize) -> String {
    let path: Vec<String> = (0..levels).map(|i| format!("k{i}")).collect();
    format!("{} = 1\n", path.join("."))
}

fn deep_header(levels: usize) -> String {
    let path: Vec<String> = (0..levels).map(|i| format!("k{i}")).collect();
    format!("[{}]\nx = 1\n", path.join("."))
}

#[test]
fn deeply_nested_input_returns_an_error_instead_of_overflowing_the_stack() {
    for (name, source) in [
        ("arrays", deep_arrays(FAR_TOO_DEEP)),
        ("inline tables", deep_inline_tables(FAR_TOO_DEEP)),
        ("dotted keys", deep_dotted_key(FAR_TOO_DEEP)),
        ("table headers", deep_header(FAR_TOO_DEEP)),
    ] {
        let error = aterm_toml::from_str::<aterm_toml::Value>(&source)
            .err()
            .unwrap_or_else(|| panic!("{name}: {FAR_TOO_DEEP} levels were accepted"));
        assert!(
            error.to_string().contains("nesting") || error.to_string().contains("limit"),
            "{name}: the error should name the limit, got {error}"
        );

        // The document half is the other entry point and gets the same bound.
        assert!(
            source.parse::<aterm_toml::edit::DocumentMut>().is_err(),
            "{name}: the document half accepted {FAR_TOO_DEEP} levels"
        );
    }
}

/// The bound has to be generous enough that no real document trips it. 32
/// levels is far past anything a config or a manifest contains.
#[test]
fn ordinary_nesting_is_untouched() {
    for (name, source) in [
        ("arrays", deep_arrays(32)),
        ("inline tables", deep_inline_tables(32)),
        ("dotted keys", deep_dotted_key(32)),
        ("table headers", deep_header(32)),
    ] {
        aterm_toml::from_str::<aterm_toml::Value>(&source)
            .unwrap_or_else(|e| panic!("{name}: 32 levels should be fine: {e}"));
        let document: aterm_toml::edit::DocumentMut = source
            .parse()
            .unwrap_or_else(|e| panic!("{name}: 32 levels should be fine: {e}"));
        assert_eq!(
            document.to_string(),
            source,
            "{name}: deep-but-legal must round-trip"
        );
    }
}

/// A rejected deep document must not leave a half-built tree that overflows on
/// the way out — the error path drops everything it allocated.
#[test]
fn the_rejection_path_itself_is_safe() {
    for levels in [200usize, 1_000, FAR_TOO_DEEP, 50_000] {
        assert!(aterm_toml::from_str::<aterm_toml::Value>(&deep_arrays(levels)).is_err());
        assert!(aterm_toml::from_str::<aterm_toml::Value>(&deep_inline_tables(levels)).is_err());
        assert!(aterm_toml::from_str::<aterm_toml::Value>(&deep_dotted_key(levels)).is_err());
    }
}
