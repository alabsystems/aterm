// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

/// NEGATIVE CONTROL: each value read fails, whatever its variable, macro, bracket or depth;
/// the same names inside an include, a string, a comment, or a run-time read do not.
#[test]
fn a_build_path_is_read_at_run_time_or_named_in_an_include() {
    let src = r##"
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures");
fn helper() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_aterm-lab"))
}
fn tmp() -> &'static str { env!["CARGO_TARGET_TMPDIR"] }
fn out() -> Option<&'static str> { option_env!("OUT_DIR") }
fn raw() -> &'static str { std::env!(r#"CARGO_MANIFEST_PATH"#) }
fn wrapped() -> String { format!("{}/x", env!("CARGO_MANIFEST_DIR")) }
static TEXT: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/data.txt"));
static BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data.bin"
));
include!(concat!(env!("OUT_DIR"), "/generated.rs"));
#[doc = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))]
struct Documented;
const NOTE: &str = "env!(\"CARGO_MANIFEST_DIR\") in a string is no read";
// env!("CARGO_BIN_EXE_x") in a comment is no read
fn runtime() -> std::path::PathBuf { aterm_tempfile::manifest_dir() }
fn other() -> &'static str { env!("CARGO_PKG_NAME") }
fn lookalike() -> &'static str { my_env!("CARGO_MANIFEST_DIR") }
"##;
    let found = run(&BUILD_PATH_VALUE, &[("crates/x/src/lib.rs", src)]);
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/lib.rs:2",
            "crates/x/src/lib.rs:4",
            "crates/x/src/lib.rs:6",
            "crates/x/src/lib.rs:7",
            "crates/x/src/lib.rs:8",
            "crates/x/src/lib.rs:9",
        ],
        "{found:#?}"
    );
    assert!(found[0].detail.contains("env!(\"CARGO_MANIFEST_DIR\")"));
    assert!(found[3].detail.contains("option_env!(\"OUT_DIR\")"));
}

/// Test code, a test target, an example and a build script bake the path exactly as shipped
/// code does, so the rule reads them all.
#[test]
fn test_code_and_dev_targets_are_read_too() {
    let read = "fn f() -> &'static str { env!(\"CARGO_MANIFEST_DIR\") }\n";
    let found = run(
        &BUILD_PATH_VALUE,
        &[
            ("crates/x/src/a_tests.rs", read),
            ("crates/x/tests/it.rs", read),
            ("crates/x/examples/e.rs", read),
            ("crates/x/build.rs", read),
            ("apps/y/src/main.rs", read),
        ],
    );
    assert_eq!(found.len(), 5, "{found:#?}");
}

#[test]
fn the_enclosing_macro_is_read_from_its_bracket() {
    let code = "let a = include_str!(concat!(x, y));";
    let open = code.find("concat").unwrap() + "concat!".len();
    assert_eq!(macro_before(code, open), "concat");
    let outer = code.find("include_str").unwrap() + "include_str!".len();
    assert_eq!(macro_before(code, outer), "include_str");
    assert_eq!(macro_before("f(", 1), "");
    assert!(inside_include(code, code.find('x').unwrap()));
    assert!(!inside_include("let a = concat!(x);", 16));
}
