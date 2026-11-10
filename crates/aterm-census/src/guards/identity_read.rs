// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! identity-read: no first-party code compiles the product's identity in, and no member
//! inherits the product version.
//!
//! The version, commit, build number, build time, dev-commit count, index hint and release mark
//! are one record the `aterm` binary carries and installs (`aterm_stamp`); everything else reads
//! it at run time. A compile-time read of any of them (`env!`, `option_env!`) records the
//! variable in the reading unit's dep-info, so each bump, commit or index publication rebuilds
//! that unit and everything above it. A member manifest that inherits `[workspace.package]
//! version` makes every bump a change of its metadata, and so of every unit that depends on it:
//! only the `aterm` bin package, the leaf that writes the record and that nothing depends on,
//! inherits it.

use super::rules::PRODUCT;
use super::{Bound, Finding, Rule, Tree, hits, literal, open_delim, token};

/// The record itself, and its one writer, read the record's inputs by name.
const STAMP_CRATE: &str = "crates/aterm-stamp/";
const STAMP_WRITER: &str = "crates/aterm/build.rs";
/// The writer's package, the one member that inherits the product version.
const STAMP_PACKAGE: &str = "crates/aterm/Cargo.toml";

pub(super) const IDENTITY_READ: Rule = Rule {
    id: "identity-read",
    why: "a compiled-in product identity (a crate version, the commit, build number, build time, \
          dev commits, index hint or release mark) makes every bump, commit or index publication \
          an input of the unit that reads it and of everything above it; read \
          `aterm_stamp::current()` at run time, and give a member its own constant version",
    scope: &[STAMP_CRATE, STAMP_WRITER],
    check: identity_read,
};

/// The cheap prefilter: a file that names none of these reads none of them.
const NEEDLES: &[&str] = &["CARGO_PKG_VERSION", "ATERM_", "SOURCE_DATE_EPOCH"];

/// A variable that names the product's identity: a crate version (whose every first-party
/// value is the constant `0.0.0`), or an input of the identity record.
fn names_identity(name: &str) -> bool {
    name.starts_with("CARGO_PKG_VERSION") || aterm_stamp::input::ALL.contains(&name)
}

fn identity_read(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.rel.starts_with(STAMP_CRATE) || file.rel == STAMP_WRITER || !file.mentions(NEEDLES)
        {
            continue;
        }
        let lexed = file.lexed();
        let (code, text) = (lexed.code.as_str(), lexed.text.as_str());
        for mac in ["env", "option_env"] {
            for at in hits(code, mac, Bound::Word) {
                let Some((name, _)) = token(code, at + mac.len(), "!")
                    .and_then(|bang| open_delim(code, bang))
                    .and_then(|(open, _)| literal(code, text, open))
                else {
                    continue;
                };
                if names_identity(name) {
                    out.push(file.finding(
                        IDENTITY_READ.id,
                        code[..at].matches('\n').count() + 1,
                        format!(
                            "`{mac}!(\"{name}\")` compiles the product's identity in: read \
                             `aterm_stamp::current()`"
                        ),
                    ));
                }
            }
        }
    }
    for (rel, manifest) in tree.corpus.manifests() {
        if rel == STAMP_PACKAGE {
            continue;
        }
        for (index, line) in manifest.lines().enumerate() {
            let key: String = line
                .split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            if key == "version.workspace=true" || key == "version={workspace=true}" {
                out.push(Finding {
                    rule: IDENTITY_READ.id,
                    path: rel.clone(),
                    line: index + 1,
                    detail: "inherits `[workspace.package] version`: give the member its own \
                             constant version"
                        .to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}
