// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nesting is bounded, tested with real deep input. Unbounded recursion on untrusted input
//! overflows the stack and kills the process inside a config load, so "too deep" must be
//! an `Err` a caller can handle. Arrays, inline tables, dotted keys and table headers all
//! deepen the tree that `Drop`, the encoder and the deserializer walk recursively, so all
//! four are covered.

/// Far past the limit, so the test proves an error rather than a slightly larger limit.
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

/// No real document trips the bound: 32 levels is far past any config or manifest.
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

/// Inline tables nested 128 deep, each entry a dotted key as long as the budget left at
/// its depth allows (`k = { a.a.….a = { a.….a = { … } } }`): every construct fits the
/// limit on its own, but the tree is 128 + 127 + … + 1 = 8,256 tables deep.
fn dotted_staircase(limit: usize) -> String {
    let mut out = String::from("k = ");
    for depth in 0..limit {
        out.push_str("{ ");
        out.push_str(&vec!["a"; limit - depth].join("."));
        out.push_str(" = ");
    }
    out.push('1');
    out.push_str(&" }".repeat(limit));
    out.push('\n');
    out
}

/// The bound is on the tree's depth, not on each construct alone: header segments,
/// dotted-key segments and array/inline-table nesting all spend from one budget.
#[test]
fn constructs_that_each_fit_the_limit_cannot_stack_past_it() {
    let path = (0..64)
        .map(|i| format!("k{i}"))
        .collect::<Vec<_>>()
        .join(".");
    for (name, source) in [
        ("dotted keys in nested inline tables", dotted_staircase(128)),
        (
            "a header, a dotted key, then nested arrays",
            format!(
                "[{path}]\n{path} = {}1{}\n",
                "[".repeat(100),
                "]".repeat(100)
            ),
        ),
    ] {
        let parsed = source.parse::<aterm_toml::edit::DocumentMut>();
        let refused = parsed.is_err();
        // An accepted tree is thousands of levels deep and dropping it recurses, so it
        // is leaked: the assertion, not a stack overflow, reports the failure.
        core::mem::forget(parsed);
        assert!(refused, "{name}: the document half accepted it");
        let error = aterm_toml::from_str::<aterm_toml::Value>(&source)
            .err()
            .unwrap_or_else(|| panic!("{name}: from_str accepted it"));
        assert!(
            error.to_string().contains("limit"),
            "{name}: the error should name the limit, got {error}"
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
