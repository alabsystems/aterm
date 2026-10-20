// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `[fabric] command`'s two spellings, read alike by every reader: the
//! instance's bridge launcher (`aterm-gui`'s `fabric_launch`) and `aterm
//! fabric` status, on, off and doctor (`aterm-link`). Neither crate depends on
//! the other, so the rule is defined once, here.
//!
//! ```toml
//! [fabric]
//! # A command LINE: split on whitespace, with no quoting grammar and no shell.
//! command = "aterm link serve --fleet lab --broker /tmp/f/bus.sock"
//! # An ARGV: each element is one word, passed as written — never split, never
//! # shell-interpreted — so a path with a space in it stays one word.
//! command = ["/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm", "link", "serve"]
//! ```
//!
//! A blank line, or an empty array, names no command: the fabric is off. An
//! array element that is EMPTY or holds a NUL is refused by name, never
//! dropped: no argv word can carry a NUL (`execve` ends the word there), and an
//! empty word would reach the bridge as an argument nobody meant.
//!
//! `aterm fabric on` ALWAYS writes the line form, and refuses an argv the line
//! does not spell ([`line_spells`]) rather than write the array: an older
//! aterm, which knows only the line, rejects a whole aterm.toml that holds an
//! array, and the file is shared by every aterm on the machine. It runs an
//! aterm path with a space in it through a link in the fabric root instead,
//! so the line still names it as one word. The array is for a person to write.

/// What `[fabric] command` may be, for a refusal of any other TOML type.
pub const EXPECTED: &str = "a command line (one string, split on whitespace) or an argv (an array \
                            of strings, one word each)";

/// The LINE form: `line` split on whitespace, with no quoting grammar and no
/// shell, so a metacharacter is one more word and never a second command.
/// Empty for a blank line, which names no command.
#[must_use]
pub fn split_line(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

/// The ARGV form's check on one element, `index` counted from 0: `Err` names
/// the element (counted from 1) and what is wrong with it.
///
/// # Errors
///
/// An empty element, or one that holds a NUL.
pub fn check_word(index: usize, word: &str) -> Result<(), String> {
    let n = index.saturating_add(1);
    if word.is_empty() {
        return Err(format!(
            "`[fabric] command` element {n} is empty: each element is one argv word, passed as \
             written, and an empty word is never meant"
        ));
    }
    if word.contains('\0') {
        return Err(format!(
            "`[fabric] command` element {n} holds a NUL byte, which no argv word can carry"
        ));
    }
    Ok(())
}

/// Whether the LINE form spells `argv` exactly — every word non-empty and free
/// of whitespace, so [`split_line`] of the words joined by spaces gives them
/// back. The only argv `aterm fabric on` writes; any other one only the array
/// spells.
#[must_use]
pub fn line_spells(argv: &[String]) -> bool {
    !argv.is_empty()
        && argv
            .iter()
            .all(|w| !w.is_empty() && !w.chars().any(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|w| (*w).to_string()).collect()
    }

    /// The line form still splits on whitespace — a path with a space in it
    /// is two words there, and a metacharacter is a word — while the array
    /// form is what keeps such a path whole; `line_spells` tells the two
    /// apart exactly where the split would lose a word.
    #[test]
    fn the_line_splits_and_only_a_plain_argv_is_spelled_by_it() {
        assert_eq!(
            split_line("  /a b/aterm link\tserve ; rm\n"),
            words(&["/a", "b/aterm", "link", "serve", ";", "rm"])
        );
        assert!(split_line(" \t ").is_empty(), "a blank line names nothing");
        let plain = words(&[
            "/usr/bin/aterm",
            "link",
            "serve",
            "--state",
            "/Users//u/Caf\u{e9}",
        ]);
        assert!(line_spells(&plain));
        assert_eq!(split_line(&plain.join(" ")), plain);
        // Negative controls: each of these is an argv the line cannot spell,
        // and the split of its join is a different argv.
        for spaced in [
            words(&[
                "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm",
                "link",
            ]),
            words(&["aterm", "tab\there"]),
            words(&["aterm", "nb\u{a0}sp"]),
            words(&["aterm", ""]),
        ] {
            assert!(!line_spells(&spaced), "{spaced:?}");
            assert_ne!(split_line(&spaced.join(" ")), spaced, "{spaced:?}");
        }
        assert!(!line_spells(&[]), "no words is no command");
    }

    /// An element that is empty or holds a NUL is refused, naming the element
    /// from 1; a word with a space, a quote or a non-ASCII letter is not.
    #[test]
    fn an_empty_or_nul_element_is_refused_by_name() {
        for ok in ["aterm", "/a b/aterm", "it's", "\"q\"", "Caf\u{e9}", " "] {
            assert_eq!(check_word(0, ok), Ok(()), "{ok:?}");
        }
        let empty = check_word(2, "").expect_err("an empty word");
        assert!(
            empty.contains("element 3") && empty.contains("empty"),
            "{empty}"
        );
        let nul = check_word(0, "a\0b").expect_err("a NUL");
        assert!(nul.contains("element 1") && nul.contains("NUL"), "{nul}");
    }
}
