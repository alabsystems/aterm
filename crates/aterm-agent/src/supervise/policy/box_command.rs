// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHAT THE SCREEN CANNOT SHOW OF A BASH BOX'S COMMAND.

/// Reject `text` from a Bash box when the shell may read a blank the screen does not show.
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
