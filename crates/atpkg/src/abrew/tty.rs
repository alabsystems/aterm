// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! Brew's terminal styling (`Utils::Tty` + `Formatter`): colour is
//! emitted only when the destination stream styles (a terminal, or
//! `HOMEBREW_COLOR`), and never under `HOMEBREW_NO_COLOR`. Every
//! painter returns its plain text unchanged when colour is off, so
//! piped output is byte-identical either way.

#[cfg(not(test))]
use std::io::IsTerminal as _;

/// Whether a stream styles, from brew's env rules alone.
fn env_color() -> Option<bool> {
    if super::env_set("HOMEBREW_NO_COLOR") {
        Some(false)
    } else if super::env_set("HOMEBREW_COLOR") {
        Some(true)
    } else {
        None
    }
}

/// Whether stdout receives colour escapes.
pub(super) fn stdout_color() -> bool {
    env_color().unwrap_or_else(stdout_tty)
}

/// Whether stderr receives colour escapes.
pub(super) fn stderr_color() -> bool {
    env_color().unwrap_or_else(stderr_tty)
}

/// Whether stdout is a terminal (brew gates some glyphs on this,
/// not on colour). Under `#[cfg(test)]`, the libtest runner's own
/// stdout may or may not be a PTY depending on how tests are invoked,
/// so default to non-TTY unless `HOMEBREW_COLOR` is explicitly set.
pub(super) fn stdout_tty() -> bool {
    #[cfg(test)]
    {
        false
    }
    #[cfg(not(test))]
    {
        std::io::stdout().is_terminal()
    }
}

fn stderr_tty() -> bool {
    #[cfg(test)]
    {
        false
    }
    #[cfg(not(test))]
    {
        std::io::stderr().is_terminal()
    }
}

const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const DEFAULT: &str = "\x1b[39m";
const UNDERLINE: &str = "\x1b[4m";
const NO_UNDERLINE: &str = "\x1b[24m";

/// Wrap in `codes…reset` when colour is on.
fn paint(on: bool, codes: &str, text: &str) -> String {
    if on {
        format!("{codes}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Brew `ohai_title`: a blue `==>` and a bold title, for stdout.
pub(super) fn ohai(title: &str) -> String {
    headline(stdout_color(), BLUE, title)
}

/// Brew `ohai_title` for a stderr line.
pub(super) fn ohai_err(title: &str) -> String {
    headline(stderr_color(), BLUE, title)
}

/// Brew `oh1_title`: a green `==>` and a bold headline, for stdout.
/// The title may itself contain painted spans (see [`ident`]).
pub(super) fn oh1(title: &str) -> String {
    headline(stdout_color(), GREEN, title)
}

fn headline(on: bool, arrow_color: &str, title: &str) -> String {
    if on {
        format!("{arrow_color}==>{RESET} {BOLD}{title}{RESET}")
    } else {
        format!("==> {title}")
    }
}

/// Brew `Formatter.identifier`: a name in green (stdout).
pub(super) fn ident(name: &str) -> String {
    if stdout_color() {
        format!("{GREEN}{name}{DEFAULT}")
    } else {
        name.to_string()
    }
}

/// Brew `Tty.bold` around a block of text (stderr), reset at
/// the end — doctor's warning preamble.
pub(super) fn bold_err(text: &str) -> String {
    paint(stderr_color(), BOLD, text)
}

/// Brew `Formatter.url`: an underlined URL (stdout).
pub(super) fn url(text: &str) -> String {
    if stdout_color() {
        format!("{UNDERLINE}{text}{NO_UNDERLINE}")
    } else {
        text.to_string()
    }
}

/// Brew `Formatter.success` (green, stdout).
pub(super) fn success(text: &str) -> String {
    paint(stdout_color(), GREEN, text)
}

/// Brew `Formatter.error` (red, stdout).
pub(super) fn error(text: &str) -> String {
    paint(stdout_color(), RED, text)
}

/// Brew `Formatter.warning` (yellow, stdout).
pub(super) fn warning(text: &str) -> String {
    paint(stdout_color(), YELLOW, text)
}

/// Print a `Warning:`-labelled message to stderr with brew's
/// yellow label (brew's `opoo` body; the label stays in the text).
macro_rules! opoo {
    ($($t:tt)*) => {
        eprintln!("{}", $crate::abrew::tty::warning_line(&format!($($t)*)))
    };
}
pub(super) use opoo;

/// Print an `Error:`-labelled message to stderr with brew's red
/// label (brew's `onoe` body; the label stays in the text).
macro_rules! onoe {
    ($($t:tt)*) => {
        eprintln!("{}", $crate::abrew::tty::error_line(&format!($($t)*)))
    };
}
pub(super) use onoe;

/// The display width of a possibly-styled string: brew pads by
/// `Tty.strip_ansi(text).length`.
pub(super) fn visible_len(text: &str) -> usize {
    let mut count = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // Skip a CSI sequence: ESC [ ... letter.
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
        }
        count += 1;
    }
    count
}

/// A stderr `Error:`-labelled message with the label in red, as
/// brew's `onoe` prints it; messages without the label pass through.
pub(super) fn error_line(message: &str) -> String {
    label_line(stderr_color(), RED, "Error:", message)
}

/// A stderr `Warning:`-labelled message with the label in yellow.
pub(super) fn warning_line(message: &str) -> String {
    label_line(stderr_color(), YELLOW, "Warning:", message)
}

fn label_line(on: bool, color: &str, label: &str, message: &str) -> String {
    if on && let Some(rest) = message.strip_prefix(label) {
        format!("{color}{label}{RESET}{rest}")
    } else {
        message.to_string()
    }
}

/// Brew's `pretty_install_status` for one display name on stdout:
/// status glyph and weight follow the install state, and the
/// `HOMEBREW_NO_EMOJI` spellings are brew's. Off a terminal every
/// form is the plain string, as in brew. (`warning` is brew's
/// missing-library mark, which abrew does not compute; callers
/// pass false.)
#[allow(clippy::too_many_arguments)]
pub(super) fn pretty_install_status(
    name: &str,
    installed: bool,
    outdated: bool,
    deprecated: bool,
    disabled: bool,
    warn: bool,
    can_install: bool,
    mark_uninstalled: bool,
    bold: bool,
) -> String {
    if !stdout_tty() {
        return name.to_string();
    }
    let no_emoji = super::env_set("HOMEBREW_NO_EMOJI");
    let on = stdout_color();
    let weight = if bold && on { BOLD } else { "" };
    let reset = if on { RESET } else { "" };
    let mut status = if warn {
        if no_emoji {
            warning(&format!("{weight}{name} (warning){reset}"))
        } else {
            format!("{weight}{name} {}{reset}", warning("⚠"))
        }
    } else if installed && outdated {
        if no_emoji {
            format!("{weight}{name} (upgradable){reset}")
        } else {
            format!("{weight}{name} {}{reset}", success("↑"))
        }
    } else if installed {
        if no_emoji {
            success(&format!(
                "{}{name} (installed){reset}",
                if on { BOLD } else { "" }
            ))
        } else {
            format!(
                "{}{name} {}{reset}",
                if on { BOLD } else { "" },
                success("✔")
            )
        }
    } else if !can_install {
        if no_emoji {
            error(&format!("{weight}{name} (can't be installed){reset}"))
        } else {
            format!("{weight}{name} {}{reset}", error("⊘"))
        }
    } else if mark_uninstalled {
        if no_emoji {
            error(&format!("{weight}{name} (uninstalled){reset}"))
        } else {
            format!("{weight}{name} {}{reset}", error("✘"))
        }
    } else if bold {
        format!("{weight}{name}{reset}")
    } else {
        name.to_string()
    };
    if disabled {
        status = format!("{status} {}", error("(disabled)"));
    } else if deprecated {
        status = format!("{status} {}", warning("(deprecated)"));
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_pass_through_without_the_label() {
        assert_eq!(label_line(true, RED, "Error:", "plain"), "plain");
        assert_eq!(
            label_line(true, RED, "Error:", "Error: boom"),
            "\x1b[31mError:\x1b[0m boom"
        );
        assert_eq!(
            label_line(false, RED, "Error:", "Error: boom"),
            "Error: boom"
        );
    }

    #[test]
    fn headline_shapes_match_brew() {
        assert_eq!(
            headline(true, BLUE, "Caveats"),
            "\x1b[34m==>\x1b[0m \x1b[1mCaveats\x1b[0m"
        );
        assert_eq!(headline(false, BLUE, "Caveats"), "==> Caveats");
    }
}
