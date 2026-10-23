// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE SCREEN CANNOT SHOW OF A BASH BOX'S COMMAND
//! (`docs/measured/breaker-transcript-and-zsh-2026-10-22.md` §B).
//!
//! Claude Code draws a Bash command longer than 80 cells, or with a newline in
//! it, in barred rows (` │ …`), and its renderer wraps a long line onto
//! barred rows of the same look. So the screen cannot tell a wrap from a
//! newline, and the read-only checks judge every join of the rows
//! ([`aterm_phase::PromptV2::readings`]).
//!
//! Two commands can also draw the same rows and still be read otherwise by
//! the shell. The screen draws a tab as the blanks to its stop and shows no
//! blank at a row's end, and a row break looks the same over a newline, a
//! blank or nothing. Outside quotes a run of blanks and tabs is one
//! separator, but in a here-document and after a backslash it is not
//! (MEASURED under `/bin/bash` 3.2). So no rule proves a reading of the
//! screen that holds such a form ([`blanks_read_as_drawn`], applied to every
//! reading of every Bash box by the policy's `blanks_shown`). What stays open
//! is the screen's own: at a row's break the command may have a newline, a
//! blank or nothing (the readings join with a newline or a blank), and inside
//! quotes a tab and its blanks differ as data.
//!
//! No rule reads the agent's transcript for a Bash box. breaker-4's
//! `breaker-read-only@v4` added the pending call's command as one more
//! reading where it drew as the box, which never turned a refusal into an
//! approval and cost every too-many box its lag retries and a 1 s settle;
//! `breaker-read-only@v6` judges the screen's readings alone
//! (`docs/measured/box-notes-2026-10-22.md`).

/// `Err` naming a form of `text`, a reading of a Bash box's rows, in which
/// the shell may read a blank the screen does not show. The screen draws a
/// tab as the blanks to its stop and shows no blank at a row's end, so a
/// command that differs from
/// `text` only there draws as the same rows. Outside quotes the shell reads a
/// run of blanks and tabs as one separator wherever it stands; these are the
/// forms in which it does not (MEASURED 2026-10-22 under `/bin/bash` 3.2, the
/// measured note's §B: in each pair the command drawn as `text` runs a line
/// that `text` holds as data or as part of a word):
///
/// * a here-document (`<<`, `<<-`; a here-string, `<<<`, is a word): its
///   lines are matched blank for blank, so `EOF ` ends nothing, and `<<-`
///   drops a line's leading tabs but not the blanks they draw as;
/// * a backslash at a line's end or `text`'s end: drawn so, the command may
///   hold a blank after it, which the backslash escapes, so the line ends
///   there and the next runs alone, where `text` joins the two lines;
/// * a backslash before a tab, before two blanks, or before a blank that
///   ends a line: the first blank may be a tab drawn as several, which the
///   shell reads escaped, one word with what follows it (a `#` there starts
///   no comment), where in `text` the blank after an escaped one separates.
///
/// A backslash before one blank and a word (`My\ File`) is read alike
/// whether that blank is a blank or a tab drawn as one. Stricter than the
/// shell where a backslash is itself escaped (`\\`).
///
/// # Errors
/// The form found.
pub fn blanks_read_as_drawn(text: &str) -> Result<(), String> {
    let chars: Vec<char> = text.chars().collect();
    let mut k = 0;
    while k < chars.len() {
        let run = chars[k..].iter().take_while(|c| **c == '<').count();
        if run >= 2 && run != 3 {
            return Err(
                "it holds a here-document (`<<`), whose lines the shell matches blank for blank"
                    .to_string(),
            );
        }
        k += run.max(1);
    }
    let blank = |at: usize| matches!(chars.get(at), Some(' ' | '\t'));
    let ends = |at: usize| matches!(chars.get(at), None | Some('\n'));
    for (k, c) in chars.iter().enumerate() {
        if *c != '\\' {
            continue;
        }
        if ends(k + 1) {
            return Err(
                "a backslash ends a line of it, where a blank the screen does not show \
                        would end the line instead of joining the next"
                    .to_string(),
            );
        }
        if chars.get(k + 1) == Some(&'\t') || (blank(k + 1) && (blank(k + 2) || ends(k + 2))) {
            return Err(
                "a backslash stands before blanks that may be a tab, which the shell \
                        reads escaped as one word with what follows"
                    .to_string(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "box_command_tests.rs"]
pub(crate) mod tests;
