// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The argv0 aliases of the one `aterm` binary: the names an install lays beside it, each a
//! link onto it, which it dispatches on when run under that name.
//!
//! [`ALIASES`] is the one list every Rust reader shares: the release bundle lays it in
//! `Contents/MacOS` (`aterm-release`'s `bundle::ARGV0_ALIASES` re-exports it, and the dev
//! bundle reads it there). Programs that cannot import a Rust constant keep hand copies, and
//! a change here is made in each of them too:
//!
//! - `tools/install.sh`: `STORE_ALIASES` (line 2403) and the loop that links it (3738);
//! - `apps/aterm-win/build.ps1:217` and `apps/aterm-win/install.ps1:91`, spelled with
//!   `.exe`; Windows lays the CLI names only, so `aterm-askpass` and `aterm-gui` (there the
//!   windowed image, a file of its own) are not in them;
//! - the `Get-Process` lists at `apps/aterm-win/install.ps1:225` and
//!   `apps/aterm-win/uninstall.ps1:146`;
//! - the prose in `apps/aterm-win/build.ps1`: the dist layout's `argv0 alias` rows in its
//!   header (lines 33-40) and the README it writes into the dist (462-470);
//! - the symlink sentence at `README.md:174`.
//!
//! The lists among them (`STORE_ALIASES`, both `$aliases`, both `Get-Process` lists and the
//! README sentence) each carry a comment naming this constant.

/// The argv0 aliases every bundle carries in `Contents/MacOS`, each a symlink onto the
/// one `aterm` binary, which dispatches on the name it is run as. `aterm-link` is spawned
/// BY NAME out of `[fabric] command`, so a shipped install must carry it too.
///
/// Plain names: no directory and no `.exe` (a Windows reader appends it).
pub const ALIASES: [&str; 11] = [
    "aterm-cli",
    "aterm-ctl",
    "atpkg",
    // abrew — aterm's Homebrew (`docs/DESIGN-abrew-2026-10-19.md`): both spellings,
    // the short one shells and the reroute stub name, the long one the atpkg bin name.
    "abrew",
    "atpkg-abrew",
    "aterm-fleet",
    "aterm-drive",
    // The `SUDO_ASKPASS` helper `sudo` runs with no tty
    // (`docs/DESIGN-sudo-askpass-2026-10-15.md`): an argv0 alias because `sudo`
    // execs it as `[path, prompt]` with no room for a verb.
    "aterm-askpass",
    "aterm-link",
    "aterm-gui",
    // amacs, aterm's editor (`aterm edit`, `docs/DESIGN-amacs-2026-10-19.md` §8.1): an
    // argv0 alias because `$EDITOR` names one executable, run with the file and no verb.
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
