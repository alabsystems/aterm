// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::tests::{run, spots};
use super::*;

/// A literal naming the keg, or Homebrew's link to it in its `bin`, is found in shipped
/// and test code alike; prose about it, a scratch keg a test lays under its own
/// directory, another Homebrew path a row lists as a fallback, and this crate's own tables
/// are not.
#[test]
fn no_source_names_homebrews_rustup_keg() {
    let src = "pub const HOMEBREW_RUSTUP: &str = \"/opt/homebrew/opt/rustup/bin/rustup\";\n\
               // Homebrew's keg (/opt/homebrew/opt/rustup) is never looked at.\n\
               const SHIM: &str = \"/usr/local/opt/rustup/bin\";\n\
               let zsh = \"/opt/homebrew/bin/zsh\";\n\
               let link = Path::new(\"/opt/homebrew/bin/rustup\");\n\
               #[cfg(test)]\n\
               mod tests {\n\
                   fn t(tmp: &Path) { tmp.join(\"opt/homebrew/opt/rustup/bin/rustup\"); }\n\
                   fn u() { let c = \"/x/Cellar/rustup/1.0\"; }\n\
               }\n";
    let found = run(
        &RUSTUP_KEG,
        &[
            ("crates/aterm-toolchain/src/rustup.rs", src),
            ("crates/aterm-census/src/guards/rustup_keg.rs", src),
        ],
    );
    assert_eq!(
        spots(&found),
        [
            "crates/aterm-toolchain/src/rustup.rs:1",
            "crates/aterm-toolchain/src/rustup.rs:3",
            "crates/aterm-toolchain/src/rustup.rs:5",
            "crates/aterm-toolchain/src/rustup.rs:9",
        ]
    );
}
