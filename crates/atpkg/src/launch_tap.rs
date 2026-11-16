// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! aterm's usage tap at launch: the block a managed agent program's twin and `bin/` shim carry,
//! so a Claude Code started inside an aterm tab carries aterm's status-line usage tap from its
//! start, with no relaunch and nothing for the person to do
//! (`docs/DESIGN-claude-accounts-v2-2026-10-15.md`). The judgment is aterm's own
//! (`aterm harness tap --launch`, which prints the `--settings` value or nothing); the block only
//! asks it and puts what it printed ahead of the launch's own arguments. The same verb first
//! settles the launch's folder trust (aterm's `harness::launch_trust`), so the block is also the
//! one step a launch may wait on: for a folder Claude Code holds no trust record for, and for
//! seconds at most.

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
/// exits 0. It returns at once where the folder's trust is recorded or is not aterm's to settle;
/// otherwise it waits, within a bound of its own, while Claude Code records it.
pub const VERB: [&str; 4] = ["harness", "tap", "--launch", "--"];

/// The variable the block sets for that one command: `1` where the launch's standard output is a
/// terminal, `0` where it is not (a pipe, a file, a tool's shell). aterm settles no folder trust
/// for a launch that says `0`: Claude Code prints and exits there and draws no dialog.
pub const STDOUT_TTY_ENV: &str = "ATERM_LAUNCH_STDOUT_TTY";

/// The note line ahead of the block.
const NOTE: &str = "# atpkg: aterm's usage tap — inside an aterm tab, `aterm harness tap --launch` \
     names the `--settings` value that hands this launch's usage figures to aterm's footer, or \
     nothing, and a launch it names none for runs as typed; before that, where aterm's policy \
     trusts this folder and Claude Code holds no trust record for it, it has Claude Code record \
     one, within a bound of seconds (aterm help harness).\n";

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
/// launch's arguments, its stdin `/dev/null`, its stderr dropped and [`STDOUT_TTY_ENV`] saying
/// whether the launch's own standard output is a terminal; where that printed a value,
/// the launch runs with `--settings <value>` ahead of its own arguments, and where it printed
/// nothing, failed or is not there, the launch runs as typed. The launch waits for that aterm to
/// exit, which is bounded by aterm's own deadline for the folder's trust (the module's). It sits after the `--yolo` block,
/// which reads `--settings` as a posture of the person's own, and ends with the renderer block
/// ([`RENDERER_BLOCK`]).
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
    s.push_str(" ]; then\n  if [ -t 1 ]; then __atpkg_tty=1; else __atpkg_tty=0; fi\n");
    s.push_str("  __atpkg_tap=$(");
    s.push_str(STDOUT_TTY_ENV);
    s.push_str("=$__atpkg_tty ");
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
    s.push_str(RENDERER_BLOCK);
    s
}

/// THE RENDERER BLOCK, the last part of [`sh_prelude`]: inside an aterm tab with the harness on
/// for this launch, a Claude Code launch that names no renderer runs fullscreen
/// (`CLAUDE_CODE_NO_FLICKER=1`, which starts it ahead of every settings file). Claude Code
/// otherwise picks its renderer from a server flag it caches only after a start, so the first launch on a new Claude home drew its classic renderer
/// inline under the last command's output (no clear, no blank top row) and every later one
/// drew fullscreen (measured on 2.1.321, 2026-11-09). A launch names a renderer, and the block
/// leaves it alone, by setting `CLAUDE_CODE_NO_FLICKER` or
/// `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN` in its environment (to any value, empty included) or by a `"tui":` key in the person's own
/// `settings.json` under `$CLAUDE_CONFIG_DIR`, else `$HOME/.claude` (where `/tui` saves the
/// choice, and its relaunch drops the variable); a `tui` only a project's or a managed settings
/// file holds is overridden in a tab.
const RENDERER_BLOCK: &str = "# atpkg: Claude Code's renderer — inside an aterm tab, a launch \
     that names none (CLAUDE_CODE_NO_FLICKER and CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN unset, no \
     \"tui\" in your settings.json) runs fullscreen from its first start (aterm help pkg).\n\
     if [ -n \"${ATERM_PARENT_SESSION_ID-}\" ] && [ -z \"${ATERM_NO_HARNESS-}\" ] \
     && [ -z \"${CLAUDE_CODE_NO_FLICKER+x}\" ] && [ -z \"${CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN+x}\" ] \
     && ! grep -qs '\"tui\"[[:space:]]*:' \"${CLAUDE_CONFIG_DIR:-${HOME-}/.claude}/settings.json\"; then\n  \
     CLAUDE_CODE_NO_FLICKER=1; export CLAUDE_CODE_NO_FLICKER\n\
     fi\n";

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
