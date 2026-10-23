// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The press guard: bind an approving keystroke to the box that was judged.

/// The longest pattern the server compiles for a guard
/// (`aterm_observe::row_matcher`'s `MAX_REGEX_PATTERN_LEN`).
pub const MAX_GUARD_BYTES: usize = 1024;

/// How every row guard ends: whatever whitespace the row trails, to the
/// row's end ([`row_guard`]).
pub const ROW_END: &str = "\\s*$";

/// An anchored, whitespace-free pattern that matches `row` exactly (its
/// indentation, its text, and any trailing whitespace the grid pads it with)
/// and nothing else; past [`MAX_GUARD_BYTES`], the longest anchored prefix
/// that fits.
pub fn row_guard(row: &str) -> String {
    let body = row.trim_end();
    let indent = body.chars().take_while(|c| *c == ' ').count();
    let text = &body[indent..];
    let mut out = String::from("^");
    if indent > 0 {
        out.push_str(&format!("\\x20{{{indent}}}"));
    }
    const END: &str = ROW_END;
    for c in text.chars() {
        let piece = escape_char(c);
        if out.len() + piece.len() + END.len() > MAX_GUARD_BYTES {
            return out;
        }
        out.push_str(&piece);
    }
    out.push_str(END);
    out
}

/// `text` as a literal in the server's pattern syntax, as [`row_guard`]
/// writes each character of a row: what a tape's scrub takes a name in
/// within a press guard (`tape::corpus::Scrub`).
pub(crate) fn literal(text: &str) -> String {
    text.chars().map(escape_char).collect()
}

/// One character as a literal in the server's pattern syntax.
fn escape_char(c: char) -> String {
    if c.is_ascii_alphanumeric() || c == '_' {
        c.to_string()
    } else if c == ' ' {
        "\\x20".to_string()
    } else if c.is_ascii_control() || c.is_ascii_whitespace() {
        format!("\\x{:02X}", c as u32)
    } else if "\\.+*?()|[]{}^$".contains(c) {
        format!("\\{c}")
    } else if c.is_whitespace() || c.is_control() {
        format!("\\x{{{:X}}}", c as u32)
    } else {
        c.to_string()
    }
}

/// The arguments of the `key` verb that press `key` under `guard`, fenced on
/// the judged read's screen generation when the server supports it:
/// `if-gen=<epoch>.<seq> if=<guard> <key>`, else `if=<guard> <key>`.
pub fn key_args(guard: &str, key: &str, gen_fence: Option<&str>) -> String {
    match gen_fence {
        Some(generation) => format!("if-gen={generation} if={guard} {key}"),
        None => format!("if={guard} {key}"),
    }
}

/// Whether the server's `help` text (the whole catalog, or `help key`) says
/// `key` takes the `if-gen=<epoch>.<seq>` fence.
pub fn server_fences_gen(help: &str) -> bool {
    entry_names_gen(help, "key")
}

/// Whether the server's `help` text says `send` takes the `if-gen=` fence.
pub fn server_fences_send(help: &str) -> bool {
    entry_names_gen(help, "send")
}

/// Whether the server's `help` text says `key` takes `act=`:
/// what a press means for the box it dismisses.
pub fn server_takes_act(help: &str) -> bool {
    entry_names(help, "key", "act=approve")
}

/// What a dismissing key means for the box it dismisses:
/// the leading `act=<word>` the server's `key` takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAct {
    /// The box's command is approved.
    Approve,
    /// The box is declined.
    Deny,
    /// The question is answered.
    Answer,
}

impl KeyAct {
    /// The leading option, `act=<word>`.
    #[must_use]
    pub const fn option(self) -> &'static str {
        match self {
            KeyAct::Approve => "act=approve",
            KeyAct::Deny => "act=deny",
            KeyAct::Answer => "act=answer",
        }
    }
}

/// Whether the help entry of `verb` names `if-gen=`.
fn entry_names_gen(help: &str, verb: &str) -> bool {
    entry_names(help, verb, "if-gen=")
}

/// Whether the help entry of `verb` names `token`.
fn entry_names(help: &str, verb: &str, token: &str) -> bool {
    let mut in_entry = false;
    for line in help.lines() {
        if !line.starts_with(char::is_whitespace) {
            in_entry = line.split_whitespace().next() == Some(verb);
        }
        if in_entry && line.contains(token) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_phase::prompt::fixtures;

    /// The shipped matcher: what the server compiles a `key if=<re>` with.
    fn matcher(pattern: &str) -> impl Fn(&str) -> bool + use<> {
        let m = aterm_observe::row_matcher(pattern).expect("the server compiles the guard");
        move |row| m.matches(row)
    }

    /// The guard compiles on the server's engine, is one wire token, and
    /// matches the judged row and no other row of the recorded box.
    #[test]
    fn a_row_guard_matches_its_row_and_nothing_else_on_the_box() {
        let rows = fixtures::bash_one_row();
        let cmd_row = rows
            .iter()
            .position(|r| r == "   git log --oneline -5")
            .expect("the command row");
        let g = row_guard(&rows[cmd_row]);
        assert_eq!(g, "^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$");
        assert!(!g.contains(char::is_whitespace));
        let m = matcher(&g);
        for (k, row) in rows.iter().enumerate() {
            assert_eq!(m(row), k == cmd_row, "row {k}: {row:?}");
        }
        assert!(m("   git log --oneline -5      "));
    }

    /// Negative controls: the same command in a transcript copy, a longer
    /// command that starts the same, another indentation, a swapped box.
    #[test]
    fn a_copy_or_a_swapped_box_does_not_satisfy_the_guard() {
        let g = row_guard("   touch x");
        let m = matcher(&g);
        assert!(m("   touch x"));
        assert!(!m("  ⎿  $ touch x"));
        assert!(!m("   touch x; rm -rf y"));
        assert!(!m("    touch x"));
        assert!(!m("   touch y"));
        assert!(!m(" Do you want to proceed?"));
    }

    /// Every ASCII punctuation mark, a `│` bar and a non-ASCII space are
    /// literals, never regex syntax.
    #[test]
    fn punctuation_and_bars_are_literal() {
        let row = "   │ S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1 [x] (y) {z} ^a|b.c*+?\\ '\"#&~`<>=,!@%";
        let g = row_guard(row);
        assert!(!g.contains(char::is_whitespace), "{g}");
        let m = matcher(&g);
        assert!(m(row));
        assert!(!m(&row.replace("$S", "$T")));
        let nbsp = "   a\u{a0}b";
        let g = row_guard(nbsp);
        let m = matcher(&g);
        assert!(m(nbsp));
        assert!(!m("   a b"));
    }

    /// Claude Code's empty composer is drawn as the caret and a no-break space,
    /// which `text` reads trimmed to `❯`.
    #[test]
    fn the_guard_of_a_trimmed_read_matches_the_row_the_server_tests() {
        let g = row_guard("❯");
        assert_eq!(g, "^❯\\s*$");
        let m = matcher(&g);
        assert!(m("❯\u{a0}"));
        assert!(m("❯\u{a0}   "));
        assert!(m("❯"));
        assert!(!m("❯\u{a0}keep going"));
        assert!(!m("  ❯ 1. Yes"));
    }

    #[test]
    fn a_long_row_is_guarded_by_an_anchored_prefix_that_fits() {
        let row = format!("   {}", "a.b ".repeat(400));
        let g = row_guard(&row);
        assert!(g.len() <= MAX_GUARD_BYTES, "{}", g.len());
        assert!(!g.ends_with('$'));
        let m = matcher(&g);
        assert!(m(&row));
        assert!(!m(&format!(" x{row}")));
    }

    #[test]
    fn the_gen_fence_is_added_only_where_the_server_says_it_can() {
        assert_eq!(key_args("^x$", "1", None), "if=^x$ 1");
        assert_eq!(key_args("^x$", "1", Some("2.15")), "if-gen=2.15 if=^x$ 1");
        assert!(!server_fences_gen(
            "key [id=<key>] [if=<re>] <name>: send a named key"
        ));
        let help_key = aterm_types::control_verbs::spec("key")
            .expect("the key verb")
            .entry_lines()
            .join("\n");
        assert!(!help_key.lines().next().unwrap().contains("if-gen="));
        assert!(server_fences_gen(&help_key), "{help_key}");
        let shipped = aterm_types::control_verbs::catalog_lines_full()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(server_fences_gen(&shipped));
        assert!(!server_fences_gen(
            "status: …\nkey [id=<key>] [if-seq=<n>] [if=<re>] <name>: send a named key"
        ));
        assert!(!server_fences_gen(
            "status … gen=<e.s> … (key if-gen=)\nkey [if=<re>] <name>\n  a named key"
        ));
        assert!(!server_fences_gen(
            "key [if=<re>] <name>\n  a named key\nsend [if-gen=<g>] <text>\n  gen fenced"
        ));
    }

    /// `act=` is read off the `help key`
    /// reply the fence probe already asks for: the shipped `key` entry and
    /// catalog name it; an older entry, or `act=` under another verb, does
    /// not.
    #[test]
    fn the_act_word_is_sent_only_where_help_key_names_it() {
        let help_key = aterm_types::control_verbs::spec("key")
            .expect("the key verb")
            .entry_lines()
            .join("\n");
        assert!(server_takes_act(&help_key), "{help_key}");
        let shipped = aterm_types::control_verbs::catalog_lines_full()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(server_takes_act(&shipped));
        assert!(!server_takes_act(
            "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key"
        ));
        assert!(!server_takes_act(
            "key [if=<re>] <name>\n  a named key\nsend [act=approve] <text>\n  acted"
        ));
        assert_eq!(
            [KeyAct::Approve, KeyAct::Deny, KeyAct::Answer].map(KeyAct::option),
            ["act=approve", "act=deny", "act=answer"]
        );
    }
}
