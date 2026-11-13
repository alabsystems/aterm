// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests::{run, spots};
use super::*;

/// Every compile-time read of a crate version or an identity input is found, in shipped
/// code, tests, build scripts and examples alike, however it is spelled or wrapped; the
/// record's own crate and writer, other variables, run-time reads, comments and literals
/// are not.
#[test]
fn a_compiled_in_identity_is_found_wherever_it_is_read() {
    let reads = "pub const V: &str = env!(\"CARGO_PKG_VERSION\");\n\
                 let ua = concat!(\"ua/\", env!(\"CARGO_PKG_VERSION\"));\n\
                 let hint = option_env!(\"ATERM_STAMP_INDEX_HINT\");\n\
                 let major = std::env!(\"CARGO_PKG_VERSION_MAJOR\");\n\
                 let epoch = env![ \"SOURCE_DATE_EPOCH\" ];\n\
                 let commit = env!(\n    \"ATERM_BUILD_GIT_COMMIT\",\n);\n\
                 #[cfg(test)]\n\
                 mod tests { const R: bool = option_env!(\"ATERM_RELEASE_BUILD\").is_some(); }\n";
    let quiet = "let dir = env!(\"CARGO_MANIFEST_DIR\");\n\
                 let tmp = env!(\"CARGO_TARGET_TMPDIR\");\n\
                 let host = env!(\"ATERM_COMPILER_HOST\");\n\
                 let v = std::env::var(\"CARGO_PKG_VERSION\");\n\
                 // env!(\"CARGO_PKG_VERSION\")\n\
                 let s = \"env!(\\\"CARGO_PKG_VERSION\\\")\";\n";
    let found = run(
        &IDENTITY_READ,
        &[
            ("crates/x/src/lib.rs", reads),
            ("crates/x/tests/t.rs", "fn t() { env!(\"ATERM_STAMP\"); }\n"),
            (
                "crates/y/build.rs",
                "fn main() { option_env!(\"ATERM_RELEASE_BUILD\"); }\n",
            ),
            ("crates/y/src/lib.rs", quiet),
            ("crates/aterm-stamp/src/lib.rs", reads),
            ("crates/aterm/build.rs", reads),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/x/src/lib.rs:1",
            "crates/x/src/lib.rs:2",
            "crates/x/src/lib.rs:3",
            "crates/x/src/lib.rs:4",
            "crates/x/src/lib.rs:5",
            "crates/x/src/lib.rs:6",
            "crates/x/src/lib.rs:10",
            "crates/x/tests/t.rs:1",
            "crates/y/build.rs:1",
        ]
    );
}

/// A member that inherits the product version is found, in either spelling; the `aterm` bin
/// package, a constant version, a dependency taken from the workspace and a commented-out
/// line are not.
#[test]
fn a_member_inheriting_the_product_version_is_found() {
    let found = run(
        &IDENTITY_READ,
        &[
            (
                "crates/a/Cargo.toml",
                "[package]\nname = \"a\"\nversion.workspace = true\n",
            ),
            (
                "crates/b/Cargo.toml",
                "[package]\nname = \"b\"\nversion = { workspace = true }\n",
            ),
            (
                "crates/aterm/Cargo.toml",
                "[package]\nname = \"aterm\"\nversion.workspace = true\n",
            ),
            (
                "crates/c/Cargo.toml",
                "[package]\nname = \"c\"\nversion = \"0.0.0\"\n\
                 # version.workspace = true\nedition.workspace = true\n\
                 [dependencies]\nd.workspace = true\n",
            ),
        ],
    );
    assert_eq!(
        spots(&found),
        ["crates/a/Cargo.toml:3", "crates/b/Cargo.toml:3"]
    );
}
