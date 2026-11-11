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
#[path = "argv0_tests.rs"]
mod tests;
