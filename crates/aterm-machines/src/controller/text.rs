// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Text from a host or the LAN, made safe to print on the owner's terminal.
//!
//! A report row's values, a program's output under `run`, an advertised Bonjour name: each is
//! whatever a lab Mac (or anyone on it) chose, so an escape sequence in it could set the
//! owner's clipboard (OSC 52), retitle the window or forge a link. Every such string goes
//! through [`shown`] at the point it is printed.

/// `text` safe to print to a terminal: each control character (an escape sequence's ESC, a
/// carriage return, a line feed) and each bidirectional override is shown as its `\u{…}`
/// escape rather than sent.
#[must_use]
pub fn shown(text: &str) -> String {
    let inert = |c: char| {
        c.is_control()
            || matches!(
                c,
                '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    };
    if !text.chars().any(inert) {
        return text.to_owned();
    }
    text.chars()
        .map(|c| {
            if inert(c) {
                c.escape_unicode().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_text_reaches_the_terminal_without_its_control_characters() {
        assert_eq!(
            shown("Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)"),
            "Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)"
        );
        assert_eq!(
            shown("lab\u{1b}]52;c;aGk=\u{7}\r"),
            "lab\\u{1b}]52;c;aGk=\\u{7}\\u{d}"
        );
        assert_eq!(shown("a\u{202e}b"), "a\\u{202e}b");
    }
}
