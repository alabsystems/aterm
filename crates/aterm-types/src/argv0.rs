// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The argv0 aliases of the one `aterm` binary.

/// The argv0 aliases every bundle carries in `Contents/MacOS`, each a symlink onto the one `aterm`
/// binary, which dispatches on the name it is run as.
pub const ALIASES: [&str; 12] = [
    "aterm-cli",
    "aterm-ctl",
    "atpkg",
    "abrew",
    "atpkg-abrew",
    "agh",
    "aterm-fleet",
    "aterm-drive",
    "aterm-askpass",
    "aterm-link",
    "aterm-gui",
    "amacs",
];

#[cfg(test)]
mod tests {
    use super::ALIASES;

    /// Each alias is a file name an installer can lay beside `aterm` on every platform: not
    /// empty, not `aterm` itself, no path separator, no `.exe` (Windows appends it), and no
    /// name twice (a second link would fail or replace the first).
    #[test]
    fn aliases_are_distinct_plain_names() {
        for (i, alias) in ALIASES.iter().enumerate() {
            assert!(!alias.is_empty(), "alias {i} is empty");
            assert_ne!(*alias, "aterm", "the binary is not its own alias");
            assert!(
                !alias.contains('/') && !alias.contains('\\'),
                "{alias} names a path, not a file name"
            );
            assert!(
                !alias.to_ascii_lowercase().ends_with(".exe"),
                "{alias} carries a Windows suffix"
            );
            assert!(!ALIASES[..i].contains(alias), "{alias} is listed twice");
        }
    }
}
