// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! aterm's usage tap at launch: the block a managed agent program's twin and `bin/` shim carry,
//! so a Claude Code started inside an aterm tab carries aterm's status-line usage tap from its
//! start, with no relaunch and nothing for the person to do (owner ruling R-12 of
//! `docs/DESIGN-claude-accounts-v2-2026-10-15.md`). The judgment is aterm's own
//! (`aterm harness tap --launch`, which prints the `--settings` value or nothing); the block only
//! asks it and puts what it printed ahead of the launch's own arguments.

use std::path::Path;

/// The programs whose twin carries the block ([`of`]).
const PROGRAMS: [&str; 1] = ["claude"];

/// Whether `program`'s twin carries the usage tap's block ([`sh_prelude`]).
#[must_use]
pub fn of(program: &str) -> bool {
    PROGRAMS.contains(&program)
}

/// What the block asks of the aterm it names, ahead of the launch's own arguments: aterm's
/// `harness tap --launch --`, which reads no stdin, prints the `--settings` value or nothing, and
/// exits 0.
pub const VERB: [&str; 4] = ["harness", "tap", "--launch", "--"];

/// The note line ahead of the block.
const NOTE: &str = "# atpkg: aterm's usage tap — inside an aterm tab, `aterm harness tap --launch` \
     names the `--settings` value that hands this launch's usage figures to aterm's footer, or \
     nothing, and a launch it names none for runs as typed (aterm help harness).\n";

/// The aterm executable beside `atpkg` (`atpkg` and `aterm` are one program under two names, laid
/// side by side), or `None` where `atpkg` names no file (an empty path).
#[must_use]
pub fn aterm_beside(atpkg: &Path) -> Option<std::path::PathBuf> {
    atpkg
        .file_name()
        .is_some_and(|name| !name.is_empty())
        .then(|| atpkg.with_file_name(if cfg!(windows) { "aterm.exe" } else { "aterm" }))
}

/// THE BLOCK of `program`'s twin for the atpkg at `atpkg` (`None`, or a program [`of`] does not
/// name, or Windows, whose `.cmd` twin carries no prelude: none). Inside an aterm tab
/// (`ATERM_PARENT_SESSION_ID` set) with the harness on for this launch (`ATERM_NO_HARNESS`
/// empty or unset: `claude --no-harness` sets it), it runs the aterm beside `atpkg` on the
/// launch's arguments, its stdin `/dev/null` and its stderr dropped; where that printed a value,
/// the launch runs with `--settings <value>` ahead of its own arguments, and where it printed
/// nothing, failed or is not there, the launch runs as typed. It sits after the `--yolo` block,
/// which reads `--settings` as a posture of the person's own.
#[must_use]
pub fn sh_prelude(program: &str, atpkg: Option<&Path>) -> String {
    if cfg!(windows) || !of(program) {
        return String::new();
    }
    let Some(aterm) = atpkg.and_then(aterm_beside) else {
        return String::new();
    };
    let aterm = crate::platform::sh_quote_str(&aterm.to_string_lossy());
    let mut s = String::from(NOTE);
    s.push_str(
        "if [ -n \"${ATERM_PARENT_SESSION_ID-}\" ] && [ -z \"${ATERM_NO_HARNESS-}\" ] && [ -x ",
    );
    s.push_str(&aterm);
    s.push_str(" ]; then\n  __atpkg_tap=$(");
    s.push_str(&aterm);
    for word in VERB {
        s.push(' ');
        s.push_str(word);
    }
    s.push_str(" \"$@\" </dev/null 2>/dev/null) || __atpkg_tap=\n");
    s.push_str(
        "  if [ -n \"$__atpkg_tap\" ]; then set -- --settings \"$__atpkg_tap\" \"$@\"; fi\n",
    );
    s.push_str("fi\n");
    s
}

#[cfg(test)]
#[path = "launch_tap_tests.rs"]
mod tests;
