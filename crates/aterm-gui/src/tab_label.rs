// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Drop the part of an OSC title that names the machine the user is already on.
//!
//! ## The defect this exists to prevent
//!
//! The default prompt on Debian, Ubuntu, Fedora and macOS sets the terminal title
//! to some spelling of `user@host: cwd` — bash's stock `PROMPT_COMMAND` writes
//! `\u@\h: \w` and the others differ only in punctuation. Every tab of a local
//! session therefore opens with the SAME twenty-odd characters, and they are the
//! characters a tab has least room for.
//!
//! Measured on this machine at 0.61.0: two tabs in `~/aterm` both labelled
//! `user@m17-tower: ~/aterm · Ready`. Byte-identical, so [`crate::tab_bar`]'s
//! distinctness pass correctly concluded there was nothing to tell apart and fell
//! back to painting the bare tab ordinals `1` and `2`. With seven tabs open the
//! whole strip was numbers. The strip was not wrong — its input carried no
//! information, because the only distinguishing rung (the directory) sat past the
//! width the shared prefix had already spent.
//!
//! ## What is dropped, and what is never dropped
//!
//! `user@host:` is removed only when `host` names THIS machine, where the user
//! gains nothing from being told where they are. A host that does NOT match is
//! the case where the prefix is the most valuable text in the tab — an ssh
//! session into `prod-db-01` must keep saying so — and it is left untouched.
//!
//! ## The second shape: the console's own program path (Windows)
//!
//! ConPTY reports the console title to the terminal as OSC 0/2, and a fresh
//! console's title is the FULL PATH of the program it was created for —
//! measured on this machine at 0.90.0: a new pane's first title was
//! `C:\Program Files\WindowsApps\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\pwsh.exe`,
//! which the strip painted whole and `ls` printed percent-encoded, until the
//! prompt's own title (`~\aterm`) replaced it a moment later. A shell that sets
//! no title (stock `cmd.exe`, `pwsh` without a prompt hook) keeps that path for
//! its whole life. The path names only the program the user just launched, so
//! it is dropped the same way the local identity is
//! ([`without_console_program_path`]) and the cwd rung labels the tab.
//! `cmd.exe` appends the running command to its title (`C:\…\cmd.exe - ping x`);
//! that suffix IS information and is kept.
//!
//! The title is display-only chrome. The terminal's OSC 0/2 title remains
//! authoritative identity and is not rewritten: `ctl title` and `meta` still
//! report exactly what the program set (the roster's `ls` title drops only
//! the console program path, keeping `user@host:` — see
//! `control_session::roster_title`). A title a person or a program
//! actually chose (`vim README.md`, a TUI's own name) matches none of the
//! shapes below and survives in full.

use std::sync::OnceLock;

/// This machine's names, resolved once: the hostname as configured, plus its
/// short form (the label before the first dot, which is what bash's `\h` writes).
fn local_host_names() -> &'static (String, String) {
    static NAMES: OnceLock<(String, String)> = OnceLock::new();
    NAMES.get_or_init(|| {
        let full = resolve_hostname().unwrap_or_default();
        let short = full
            .split_once('.')
            .map_or(full.as_str(), |(label, _)| label)
            .to_owned();
        (full, short)
    })
}

/// Read the host's own name without spawning anything on the platforms that can
/// answer from the filesystem. `$HOSTNAME` is consulted first because a user who
/// overrides it is describing the same machine the prompt will name.
fn resolve_hostname() -> Option<String> {
    if let Some(name) = std::env::var_os("HOSTNAME")
        && let Some(name) = name.to_str()
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_owned());
    }
    #[cfg(target_os = "linux")]
    if let Ok(name) = std::fs::read_to_string("/proc/sys/kernel/hostname")
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_owned());
    }
    let out = std::process::Command::new("hostname").output().ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// The title with a `user@host:` prefix naming this machine removed.
///
/// Returns the input unchanged when the title is not of that shape, or names a
/// different host. An `""` result means the title said nothing BUT the local
/// identity, which is the caller's signal to fall through to its next rung (the
/// working directory) — see [`crate::app_tabs::resolved_terminal_title_rung`].
#[must_use]
pub(crate) fn without_local_identity(title: &str) -> &str {
    let (full, short) = local_host_names();
    without_identity_of(title, full, short)
}

/// The title with everything that names only what the user already knows
/// removed: a `user@host:` prefix naming this machine
/// ([`without_local_identity`]) and the console's own program path
/// ([`without_console_program_path`]). THE strip's rung, used by both label
/// paths (`app_tabs::resolved_terminal_title_rung` and the per-frame refill),
/// so the painted strip and the chrome push cannot label one tab differently.
/// `""` is the fall-through signal to the cwd rung, as for either shed alone.
#[must_use]
pub(crate) fn informative(title: &str) -> &str {
    without_console_program_path(without_local_identity(title))
}

/// The title with a bare Windows executable path removed — ConPTY's title for
/// a console whose program set none (see the module docs for the measurement).
///
/// The shape is an ABSOLUTE Windows path (`X:\…` or a UNC `\\…`) whose first
/// `.exe` (ASCII case-insensitive) either ends the title, which yields `""`
/// (the caller's fall-through to the cwd rung), or is followed by `cmd.exe`'s
/// ` - <command>` convention, which yields the command alone. Any other title
/// is returned unchanged: a relative or POSIX path, an `.exe` mid-path
/// (`C:\apps\foo.exe.bak\bar.txt`), or prose that merely contains one. The
/// shape cannot occur on Unix, so that platform's labels are untouched.
///
/// No allocation: the per-frame refill calls this on every terminal tab.
#[must_use]
pub(crate) fn without_console_program_path(title: &str) -> &str {
    let trimmed = title.trim();
    let bytes = trimmed.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    if !drive && !trimmed.starts_with("\\\\") {
        return title;
    }
    for (at, _) in trimmed.match_indices('.') {
        let Some(ext) = trimmed.get(at..at + 4) else {
            continue;
        };
        if !ext.eq_ignore_ascii_case(".exe") {
            continue;
        }
        let rest = &trimmed[at + 4..];
        if rest.is_empty() {
            return "";
        }
        // ` - <command>`. The outer trim has already taken the space after a
        // dash with nothing behind it, so a bare ` -` is the same shape.
        if let Some(command) = rest.strip_prefix(" -")
            && (command.is_empty() || command.starts_with(' '))
        {
            return command.trim();
        }
    }
    title
}

/// [`without_local_identity`] with this machine's names handed in.
///
/// The decision is the whole subject and it turns on which host the title names,
/// so a test drives it against hosts it chooses rather than against whatever
/// machine happens to be running the suite.
#[must_use]
fn without_identity_of<'a>(title: &'a str, local_full: &str, local_short: &str) -> &'a str {
    let trimmed = title.trim();
    // `user@host: rest`, or `user@host` alone at the end of the title. Splitting
    // on the FIRST colon is what keeps `make: *** No rule` out of this: its head
    // (`make`) carries no `@`, so the shape check below rejects it outright.
    let (head, rest) = match trimmed.split_once(':') {
        Some((head, rest)) => (head, rest.trim_start()),
        None => (trimmed, ""),
    };
    let Some((user, host)) = head.split_once('@') else {
        return title;
    };
    // No default prompt puts whitespace in either token, and requiring that is
    // what stops a sentence that merely CONTAINS an address — `sent to
    // user@m17-tower: ok` — from being read as a prompt's identity.
    if user.is_empty()
        || host.is_empty()
        || head.contains(char::is_whitespace)
        || !host_is_local(host, local_full, local_short)
    {
        return title;
    }
    rest
}

/// Whether `host` names this machine, comparing the short forms too because a
/// prompt may write either (`\h` short, `\H` fully qualified) while the machine
/// reports the other.
fn host_is_local(host: &str, local_full: &str, local_short: &str) -> bool {
    if local_full.is_empty() && local_short.is_empty() {
        return false;
    }
    let host_short = host.split_once('.').map_or(host, |(label, _)| label);
    let eq = |a: &str, b: &str| !a.is_empty() && a.eq_ignore_ascii_case(b);
    eq(host, local_full) || eq(host, local_short) || eq(host_short, local_short)
}

#[cfg(test)]
mod tests {
    use super::{host_is_local, informative, without_console_program_path, without_identity_of};

    const FULL: &str = "m17-tower.local";
    const SHORT: &str = "m17-tower";

    fn strip(title: &str) -> &str {
        without_identity_of(title, FULL, SHORT)
    }

    #[test]
    fn the_stock_prompt_title_keeps_only_the_directory() {
        // The measured defect: bash's default `\u@\h: \w`.
        assert_eq!(strip("user@m17-tower: ~/aterm"), "~/aterm");
        // Fully-qualified (`\H`) and a no-space spelling reach the same rung.
        assert_eq!(strip("user@m17-tower.local: ~/aterm"), "~/aterm");
        assert_eq!(strip("user@m17-tower:~/aterm"), "~/aterm");
    }

    #[test]
    fn a_title_that_is_only_this_machine_leaves_nothing_to_show() {
        // Empty is the caller's fall-through signal, NOT a label: the cwd rung
        // renders the same fact with the part worth reading still in it.
        assert_eq!(strip("user@m17-tower"), "");
        assert_eq!(strip("user@m17-tower:"), "");
        assert_eq!(strip("  user@m17-tower:   "), "");
    }

    #[test]
    fn another_machine_keeps_the_identity_that_is_the_point_of_the_tab() {
        // The case the prefix EARNS its width: this tab is not local.
        assert_eq!(
            strip("root@prod-db-01: /var/log"),
            "root@prod-db-01: /var/log"
        );
        assert_eq!(
            strip("user@m17-tower-2: ~/aterm"),
            "user@m17-tower-2: ~/aterm"
        );
    }

    #[test]
    fn a_title_a_program_chose_is_never_touched() {
        for title in [
            "vim README.md",
            "make: *** No rule to make target 'all'",
            "cargo test -p aterm-gui",
            "~/aterm",
            "",
            // Contains an address but is prose, not a prompt's identity.
            "sent to user@m17-tower: ok",
            // Shape-like but degenerate on one side.
            "@m17-tower: ~/aterm",
            "user@: ~/aterm",
        ] {
            assert_eq!(strip(title), title, "rewrote a title it does not own");
        }
    }

    #[test]
    fn an_unknown_local_name_matches_nothing() {
        // A host that cannot name itself must not start eating every prefix.
        assert_eq!(
            without_identity_of("user@m17-tower: ~/aterm", "", ""),
            "user@m17-tower: ~/aterm"
        );
        assert!(!host_is_local("m17-tower", "", ""));
    }

    #[test]
    fn case_and_domain_spellings_of_this_machine_all_match() {
        assert!(host_is_local("M17-Tower", FULL, SHORT));
        assert!(host_is_local("m17-tower.lan", FULL, SHORT));
        assert!(!host_is_local("m17-towerx", FULL, SHORT));
    }

    /// The measured defect: ConPTY's title for a fresh console is the program's
    /// own path, which names nothing the user did not just choose. Empty is the
    /// fall-through to the cwd rung, exactly as for the local identity.
    #[test]
    fn a_console_program_path_leaves_nothing_to_show() {
        for path in [
            "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\\pwsh.exe",
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            "C:\\Windows\\system32\\cmd.exe",
            "c:/windows/system32/CMD.EXE",
            "\\\\build-01\\tools\\shell.exe",
            "  C:\\Windows\\system32\\cmd.exe  ",
        ] {
            assert_eq!(without_console_program_path(path), "", "{path:?}");
            assert_eq!(informative(path), "", "{path:?}");
        }
    }

    /// `cmd.exe` writes `<path> - <command>` while a command runs: the path
    /// goes, the command — the half that says what the tab is doing — stays.
    #[test]
    fn a_command_after_the_program_path_is_the_part_worth_reading() {
        assert_eq!(
            without_console_program_path("C:\\Windows\\system32\\cmd.exe - ping  build-01"),
            "ping  build-01"
        );
        assert_eq!(
            without_console_program_path("C:\\Windows\\system32\\cmd.exe - C:\\tools\\run.exe"),
            "C:\\tools\\run.exe"
        );
        // A dash with nothing after it says nothing either.
        assert_eq!(
            without_console_program_path("C:\\Windows\\system32\\cmd.exe - "),
            ""
        );
    }

    /// Only the bare console shape is dropped: every title that is not an
    /// absolute Windows path to an `.exe` — including ones that CONTAIN one —
    /// is returned byte-identical, so nothing a program chose is rewritten and
    /// Unix labels (which never take the shape) cannot change at all.
    #[test]
    fn a_title_that_is_not_a_bare_program_path_is_never_touched() {
        for title in [
            "~\\aterm",
            "~/aterm",
            "vim README.md",
            "/usr/bin/bash",
            "pwsh.exe",
            "bin\\tool.exe",
            "C:\\apps\\foo.exe.bak\\notes.txt",
            "C:\\Users\\x\\aterm",
            "running C:\\tools\\run.exe now",
            "C:\\tools\\run.exe.",
            "C:\\tools\\run.exe -v",
            "C:",
            "",
            "user@m17-tower: ~/aterm",
        ] {
            assert_eq!(
                without_console_program_path(title),
                title,
                "rewrote {title:?}"
            );
        }
        // The two sheds compose: identity first, then the path shape.
        assert_eq!(informative("vim README.md"), "vim README.md");
    }
}
