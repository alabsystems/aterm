// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pages and lines of the scenes `trail-gallery` and `trail-film` share, written once.
//!
//! `trail-gallery`'s line fixture (a program in a real aterm's tab) and `aterm-render`'s
//! `trail_film` (a replay's events; it mounts this file by path) both lay their pages and
//! repaint their rows with these, so a scene is one page in a gallery frame and a film frame.
//! Nothing here reads a clock, a terminal or either crate: bytes out of sizes and text.

/// The line the two resident scenes (`bottom-prompt`, `composer`) and `jump` type: 48 keys,
/// descenders among them.
pub const RESIDENT_TEXT: &str = "echo the quick brown fox jumps over the lazy dog";
const _: () = assert!(RESIDENT_TEXT.len() == 48);

/// How long a scene on a resident page waits before its first key, ms: a cold instance's pet
/// has come to the caret by then (it leaps at about 1 s).
pub const RESIDENT_SETTLE_MS: u64 = 2_000;

/// `enter-long`'s command, 40 cells.
pub const ENTER_LONG_TEXT: &str = "echo the quick brown fox jumps over dogs";
const _: () = assert!(ENTER_LONG_TEXT.len() == 40);

/// `coloured-prompt`'s command, and the suggestion fish shows the rest of.
pub const COLOURED_PROMPT_TEXT: &str = "git commit -m 'quick brown fox jumps'";

/// `verdict`'s commands that end by themselves: the frame label, the command, how long it runs
/// (ms) and its exit code.
pub const VERDICT_COMMANDS: [(&str, &str, u64, u8); 4] = [
    ("true", "true", 0, 0),
    ("false", "false", 0, 1),
    ("sleep", "sleep 3", 3_000, 0),
    ("sleep-false", "sleep 3; false", 3_000, 1),
];

/// `verdict`'s last command, which Ctrl-C ends [`VERDICT_INTERRUPT_MS`] in (exit 130).
pub const VERDICT_INTERRUPTED: &str = "sleep 9";
/// See [`VERDICT_INTERRUPTED`].
pub const VERDICT_INTERRUPT_MS: u64 = 1_200;

/// `long-build`'s command: [`BUILD_LINES`] lines of output ([`build_line`]) [`BUILD_LINE_MS`]
/// apart, then [`BUILD_FINISHED`] and exit 0.
pub const BUILD_COMMAND: &str = "targo --unverified build";
/// See [`BUILD_COMMAND`].
pub const BUILD_LINES: u64 = 40;
/// See [`BUILD_COMMAND`].
pub const BUILD_LINE_MS: u64 = 100;
/// See [`BUILD_COMMAND`].
pub const BUILD_FINISHED: &str =
    "    Finished `dev` profile [unoptimized + debuginfo] in 4.10s\r\n";

/// Line `n` of [`BUILD_COMMAND`]'s output.
#[must_use]
pub fn build_line(n: u64) -> String {
    format!("   Compiling aterm-crate-{n:02} (crates/aterm-crate-{n:02})\r\n")
}

/// The rows of a shell's session [`shell_page`] lays over its prompt, the last of them the row
/// over the prompt: a command's output, 65 cells of it.
pub const SHELL_ROWS: [&str; 13] = [
    "$ git status --short",
    " M crates/aterm-effects/src/rainbow_kitty/ribbon.rs",
    " M crates/aterm-lab/src/trail_gallery.rs",
    "?? notes/typing-row.md",
    "$ targo --unverified test -p aterm-effects",
    "   Compiling aterm-effects (crates/aterm-effects)",
    "    Finished `test` profile [unoptimized + debuginfo] in 42.18s",
    "     Running unittests src/lib.rs",
    "running 3 tests",
    "test ribbon::lays_the_foot_rail ... ok",
    "test stardust::glints_once_a_key ... ok",
    "test kitty_pet::keeps_off_the_typed_line ... ok",
    "test result: ok. 3 passed; 0 failed; 0 ignored; finished in 0.42s",
];

/// The shell-integration marks a shell's prompt sits between (OSC 133 A and B), and the mark
/// that a command runs (C); [`exit_mark`] is the one that it ended.
pub const MARK_PROMPT: &str = "\x1b]133;A\x07";
/// See [`MARK_PROMPT`].
pub const MARK_INPUT: &str = "\x1b]133;B\x07";
/// See [`MARK_PROMPT`].
pub const MARK_RUNS: &str = "\x1b]133;C\x07";

/// The mark that a command ended with exit `code` (OSC 133 D).
#[must_use]
pub fn exit_mark(code: u8) -> String {
    format!("\x1b]133;D;{code}\x07")
}

/// A full page of a shell's session on `rows` by `cols`, the caret left on the last row, which
/// it leaves blank: [`SHELL_ROWS`] from the row over that up, cycled on a taller grid and cut at
/// the grid's last column.
#[must_use]
pub fn shell_page(rows: u16, cols: u16) -> String {
    let over = usize::from(rows).saturating_sub(1);
    let mut page = String::from("\x1b[H\x1b[2J");
    for k in 0..over {
        let from_foot = (over - 1 - k) % SHELL_ROWS.len();
        page.extend(
            SHELL_ROWS[SHELL_ROWS.len() - 1 - from_foot]
                .chars()
                .take(usize::from(cols)),
        );
        page.push_str("\r\n");
    }
    page
}

/// The transcript [`composer_page`] lays over its composer.
pub const COMPOSER_TRANSCRIPT: [&str; 10] = [
    "\u{23fa} The foot rail is drawn in ribbon.rs. A row with letters under it,",
    "  or the pane's last row, keeps its body inside the row.",
    "",
    "\u{23fa} Read(crates/aterm-effects/src/rainbow_kitty/ribbon.rs)",
    "  \u{23bf}  Read 212 lines",
    "",
    "\u{23fa} Both typing rows are in the gallery now: a shell's last row and",
    "  this box. Type a line into either to judge the trail on it.",
    "",
    "\u{273b} Worked for 42s",
];

/// The composer's prompt: `❯` and a no-break space, as Claude Code draws it.
pub const COMPOSER_PROMPT: &str = "\u{276f}\u{a0}";

/// The ink of the composer's two rules, and of the rows under its box, as a Claude Code in an
/// aterm tab draws them (read off one with `aterm ctl cell`, 2026-11-11: the rules `#888888`,
/// the hint row `#999999`, on aterm's dark page): SGR foreground parameters.
pub const COMPOSER_RULE_INK: &str = "38;2;136;136;136";
/// See [`COMPOSER_RULE_INK`].
pub const COMPOSER_FOOT_INK: &str = "38;2;153;153;153";

/// The rows under the composer's box: its hint row and its mode row. A Claude Code draws two to
/// six there (the hint or usage, the mode, a row per background task).
pub const COMPOSER_FOOT: [&str; 2] = ["  ? for shortcuts", "  \u{23f5}\u{23f5} accept edits on"];

/// The rows [`composer_page`] keeps from the prompt's row down: it, the rule under it and
/// [`COMPOSER_FOOT`].
const COMPOSER_UNDER: u16 = 4;
const _: () = assert!(COMPOSER_FOOT.len() + 2 == COMPOSER_UNDER as usize);

/// The composer's row on a grid of `rows`, counted from 1 as a CUP names it.
#[must_use]
pub const fn composer_row_of(rows: u16) -> u16 {
    let row = rows.saturating_sub(COMPOSER_UNDER - 1);
    if row == 0 { 1 } else { row }
}

/// Claude Code's screen with an empty composer on `rows` by `cols`: [`COMPOSER_TRANSCRIPT`]'s
/// last rows, a blank row, a rule (`─` in every cell, [`COMPOSER_RULE_INK`]), the prompt's row
/// ([`composer_row_of`]), a rule and [`COMPOSER_FOOT`] in [`COMPOSER_FOOT_INK`]. The prompt's
/// row is left for [`composer_row`].
#[must_use]
pub fn composer_page(rows: u16, cols: u16) -> String {
    let cols = usize::from(cols);
    let rule = "\u{2500}".repeat(cols);
    let prompt = composer_row_of(rows);
    // The rule over the prompt's row and the blank row over that are not the transcript's.
    let top = usize::from(prompt).saturating_sub(3);
    let mut page = String::from("\x1b[H\x1b[2J");
    for row in COMPOSER_TRANSCRIPT
        .iter()
        .skip(COMPOSER_TRANSCRIPT.len().saturating_sub(top))
    {
        page.extend(row.chars().take(cols));
        page.push_str("\r\n");
    }
    let over = prompt.saturating_sub(1).max(1);
    page.push_str(&format!(
        "\x1b[{over};1H\x1b[{COMPOSER_RULE_INK}m{rule}\x1b[0m\r\n\r\n\x1b[{COMPOSER_RULE_INK}m{rule}\x1b[0m"
    ));
    for row in COMPOSER_FOOT {
        let row: String = row.chars().take(cols).collect();
        page.push_str(&format!("\r\n\x1b[{COMPOSER_FOOT_INK}m{row}\x1b[0m"));
    }
    page
}

/// The composer's repaint of its row `row` (from 1) holding `text`: one synchronized update,
/// the caret hidden through it and left past the text's end, as Claude Code repaints.
#[must_use]
pub fn composer_row(row: u16, text: &str) -> String {
    format!(
        "\x1b[?2026h\x1b[?25l\x1b[{row};1H\x1b[2K{COMPOSER_PROMPT}{text}\x1b[{row};{col}H\x1b[?25h\x1b[?2026l",
        col = COMPOSER_PROMPT.chars().count() + text.chars().count() + 1
    )
}

/// A shell's prompt.
pub const SHELL_PROMPT: &str = "$ ";

/// `text` as fish's highlighter inks it: the command, then each word an option or parameter,
/// or part of a quoted string (fish's default `fish_color_command`, `_param` and `_quote`).
#[must_use]
pub fn fish_inked(text: &str) -> String {
    let mut out = String::new();
    let mut quoted = false;
    for (n, word) in text.split(' ').enumerate() {
        if n > 0 {
            out.push(' ');
        }
        if word.is_empty() {
            continue;
        }
        let opens = word.starts_with('\'');
        let ink = if n == 0 {
            "38;5;26"
        } else if quoted || opens {
            "38;5;100"
        } else {
            "38;5;39"
        };
        quoted = (quoted || opens) && !(word.len() > 1 && word.ends_with('\''));
        out.push_str(&format!("\x1b[{ink}m{word}\x1b[0m"));
    }
    out
}

/// fish's repaint of its line on row `row` (from 1) with `typed` on it: the caret hidden
/// through it, the prompt, the typed part inked ([`fish_inked`]), the rest of
/// [`COLOURED_PROMPT_TEXT`] in bright black past the caret (none once `typed` leaves it).
#[must_use]
pub fn fish_row(row: u16, typed: &str) -> String {
    let rest = COLOURED_PROMPT_TEXT.strip_prefix(typed).unwrap_or("");
    format!(
        "\x1b[?25l\x1b[{row};1H\x1b[K{SHELL_PROMPT}{}\x1b[90m{rest}\x1b[0m\x1b[{row};{col}H\x1b[?25h",
        fish_inked(typed),
        col = SHELL_PROMPT.len() + typed.len() + 1
    )
}
