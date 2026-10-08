// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The press guard: bind an approving keystroke to the box that was judged.
//!
//! `key if=<re> <key>` presses only while some visible row matches `<re>`,
//! checked under the same terminal lock as the write (aterm-gui
//! `control_input.rs`, `input_if_row_matches`). [`row_guard`] builds the
//! pattern from the row that was classified (the command row, indentation and
//! all, anchored at both ends), so the press lands only while that row is
//! still drawn, never on a swapped box or a transcript copy.
//!
//! The wire splits a control line on whitespace and nothing quotes, so the
//! pattern holds no whitespace: a space is `\x20`, and only regex
//! metacharacters are escaped (`aterm_regex` reads `\<` and `\>` as word
//! boundaries, so `<` and `>` stay bare). A row past [`MAX_GUARD_BYTES`] is
//! guarded by its anchored prefix.
//!
//! Where the server can fence a press on the judged read's screen generation
//! (`key if-gen=<epoch>.<seq>`, answered `OK skipped reason=changed` when the
//! screen moved), [`key_args`] adds that fence before the row guard;
//! [`server_fences_gen`] reads the server's `help` to decide. The generation,
//! not `seq=`, which repeats after an alternate-screen re-entry. `if-fp=` is
//! not sent: within one generation the screen cannot change.

/// The longest pattern the server compiles for a guard
/// (`aterm_observe::row_matcher`'s `MAX_REGEX_PATTERN_LEN`).
pub const MAX_GUARD_BYTES: usize = 1024;

/// How every row guard ends: whatever whitespace the row trails, to the
/// row's end ([`row_guard`]).
pub const ROW_END: &str = "\\s*$";

/// An anchored, whitespace-free pattern that matches `row` exactly (its
/// leading indentation, its text, and any trailing whitespace the grid pads
/// it with) and nothing else. A row whose pattern would exceed
/// [`MAX_GUARD_BYTES`] is matched by the longest prefix that fits, still
/// anchored at the start.
///
/// The trailing run is [`ROW_END`], any whitespace (`\s`), not only spaces:
/// `text` trims what `str::trim_end` trims, while the server tests the row as
/// drawn, and Claude Code draws its empty composer as `❯` and a no-break
/// space.
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

/// One character as a literal in the server's pattern syntax.
fn escape_char(c: char) -> String {
    if c.is_ascii_alphanumeric() || c == '_' {
        c.to_string()
    } else if c == ' ' {
        "\\x20".to_string()
    } else if c.is_ascii_control() || c.is_ascii_whitespace() {
        format!("\\x{:02X}", c as u32)
    } else if "\\.+*?()|[]{}^$".contains(c) {
        // The metacharacters only: `\<` and `\>` are word-boundary
        // assertions in this engine, so the rest of ASCII is written bare.
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
/// `key` takes the `if-gen=<epoch>.<seq>` generation fence. An entry is the
/// row naming its verb in column 0 and the indented rows under it, and the
/// fence is named in the entry's FENCES paragraph, never its first row. The
/// `if-seq=` spelling is not the fence: a server that knows `if-gen=` names
/// `if-seq=` only to refuse it.
pub fn server_fences_gen(help: &str) -> bool {
    entry_names_gen(help, "key")
}

/// Whether the server's `help` text says `send` takes the `if-gen=` fence —
/// [`server_fences_gen`]'s reading of `send`'s entry. An older `send` types
/// a leading `if-gen=` into its body as TEXT, so a fenced write is sent
/// only where this is true.
pub fn server_fences_send(help: &str) -> bool {
    entry_names_gen(help, "send")
}

/// Whether the help entry of `verb` names `if-gen=`.
fn entry_names_gen(help: &str, verb: &str) -> bool {
    let mut in_entry = false;
    for line in help.lines() {
        if !line.starts_with(char::is_whitespace) {
            in_entry = line.split_whitespace().next() == Some(verb);
        }
        if in_entry && line.contains("if-gen=") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_phase::prompt::fixtures;

    /// The SHIPPED matcher: what the server compiles a `key if=<re>` with.
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
        // Grid padding on the right still matches.
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

    /// Claude Code 2.1.281's empty composer as the server matches it: the
    /// caret and a no-break space, which `text` reads trimmed to `❯`. The
    /// guard built from the read matches the drawn row. Negative controls: a
    /// draft, and a caret indented as an option row is.
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
        // The server's own answers: `help key` (the one entry, wrapped over
        // many rows, the fence on a continuation row) and the full catalog
        // (one row per verb). Each names if-gen= as the fence and
        // if-seq= only as a refusal.
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
        // NEGATIVE CONTROLS: the retired spelling alone is not the fence — a
        // host that names only `if-seq=` answers `if-gen=` as a usage error —
        // and a mention of if-gen under another verb is not key's.
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
}
