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

/// What the token block asks of the aterm it names, between it and the build: aterm's token
/// wrapper for a launch a person typed (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.4).
pub const TOKEN_VERB: [&str; 5] = ["harness", "accounts", "token-run", "--auto", "--"];

/// The note line ahead of the token block.
const TOKEN_NOTE: &str = "# atpkg: aterm's token route — inside an aterm tab, `aterm harness accounts token-run \
     --auto` starts this launch on its account's vault token where that token is in use on this \
     Mac, and runs it exactly as typed anywhere else (aterm help harness).\n";

/// `body`, `program`'s `sh` twin for the atpkg at `atpkg`, with THE TOKEN BLOCK right before its
/// last line, the plain `exec '<build>' "$@"`: inside an aterm tab (`ATERM_PARENT_SESSION_ID` set), with
/// the harness on for this launch (`ATERM_NO_HARNESS` empty or unset) and the aterm beside
/// `atpkg` there, the launch runs as `<aterm> harness accounts token-run --auto -- <build>
/// <args…>` ([`TOKEN_VERB`]), which serves the account's token or runs the build as typed; with
/// that aterm missing, the last line runs as typed. The block sits after the tap's (in the
/// prelude) and after the twin's exports, so the build the wrapper execs holds them, and its
/// last line stays the plain exec, which `twin_binary` reads the build from. `body` comes back
/// as it is for a program [`of`] does not name, on Windows (whose
/// `.cmd` twin carries no prelude), where `atpkg` names no file, or where its last line is not a
/// plain exec.
#[must_use]
pub fn with_token_block(program: &str, body: String, atpkg: Option<&Path>) -> String {
    if cfg!(windows) || !of(program) {
        return body;
    }
    let Some(aterm) = atpkg.and_then(aterm_beside) else {
        return body;
    };
    let trimmed = body.trim_end_matches('\n');
    let (head, last) = match trimmed.rsplit_once('\n') {
        Some((head, last)) => (head, last),
        None => return body,
    };
    let Some(build) = crate::platform::sh_exec_target(last.trim()) else {
        return body;
    };
    let aterm = crate::platform::sh_quote_str(&aterm.to_string_lossy());
    let build = crate::platform::sh_quote_str(&build.to_string_lossy());
    let mut s = String::with_capacity(body.len() + 512);
    s.push_str(head);
    s.push('\n');
    s.push_str(TOKEN_NOTE);
    s.push_str(
        "if [ -n \"${ATERM_PARENT_SESSION_ID-}\" ] && [ -z \"${ATERM_NO_HARNESS-}\" ] && [ -x ",
    );
    s.push_str(&aterm);
    s.push_str(" ]; then\n  exec ");
    s.push_str(&aterm);
    for word in TOKEN_VERB {
        s.push(' ');
        s.push_str(word);
    }
    s.push(' ');
    s.push_str(&build);
    s.push_str(" \"$@\"\nfi\n");
    s.push_str(last);
    s.push('\n');
    s
}

#[cfg(test)]
#[path = "launch_tap_tests.rs"]
mod tests;
