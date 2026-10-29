// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nesting depth bounds on TOML values and documents.

const FAR_TOO_DEEP: usize = 10_000;

fn deep_arrays(levels: usize) -> String {
    format!("a = {}1{}\n", "[".repeat(levels), "]".repeat(levels))
}

fn deep_inline_tables(levels: usize) -> String {
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
            error.to_string().contains("limit"),
            "{name}: the error should name the limit, got {error}"
        );

        assert!(
            source.parse::<aterm_toml::edit::DocumentMut>().is_err(),
            "{name}: the document half accepted {FAR_TOO_DEEP} levels"
        );
    }
}

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

#[test]
fn exact_depth_limit_boundary_is_128_below_root() {
    for (name, at_limit, past_limit) in [
        ("arrays", deep_arrays(127), deep_arrays(128)),
        (
            "inline tables",
            deep_inline_tables(127),
            deep_inline_tables(128),
        ),
        ("dotted keys", deep_dotted_key(128), deep_dotted_key(129)),
        ("table headers", deep_header(127), deep_header(128)),
    ] {
        let doc: aterm_toml::edit::DocumentMut = at_limit
            .parse()
            .unwrap_or_else(|e| panic!("{name}: depth 128 must succeed: {e}"));
        assert_eq!(doc.to_string(), at_limit, "{name}: depth 128 round-trip");
        aterm_toml::from_str::<aterm_toml::Value>(&at_limit)
            .unwrap_or_else(|e| panic!("{name}: Value depth 128 must succeed: {e}"));

        assert!(
            past_limit.parse::<aterm_toml::edit::DocumentMut>().is_err(),
            "{name}: depth 129 must fail on DocumentMut"
        );
        let err = aterm_toml::from_str::<aterm_toml::Value>(&past_limit)
            .err()
            .unwrap_or_else(|| panic!("{name}: depth 129 must fail on Value"));
        assert!(
            err.to_string().contains("limit"),
            "{name}: expected limit error, got {err}"
        );
    }

    let standalone_ok = format!("{}1{}", "[".repeat(128), "]".repeat(128));
    let standalone_err = format!("{}1{}", "[".repeat(129), "]".repeat(129));
    standalone_ok
        .parse::<aterm_toml::edit::Value>()
        .expect("standalone depth 128 must succeed");
    assert!(standalone_err.parse::<aterm_toml::edit::Value>().is_err());
}

#[test]
fn the_rejection_path_itself_is_safe() {
    for levels in [200usize, 1_000, FAR_TOO_DEEP, 50_000] {
        assert!(aterm_toml::from_str::<aterm_toml::Value>(&deep_arrays(levels)).is_err());
        assert!(aterm_toml::from_str::<aterm_toml::Value>(&deep_inline_tables(levels)).is_err());
        assert!(aterm_toml::from_str::<aterm_toml::Value>(&deep_dotted_key(levels)).is_err());
    }
}
