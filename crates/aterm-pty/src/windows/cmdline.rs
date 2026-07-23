// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PURE helpers for the ConPTY spawn: MSVCRT-canonical command-line quoting and
//! the `CreateProcessW` environment block. No FFI, unit-tested in place.
//!
//! SECURITY NOTE (quoting is an injection surface): Windows flattens argv to ONE
//! string, and the child's CRT re-parses it. The quoting below is the canonical
//! MSVCRT algorithm (quote when an arg contains space/tab/quote or is empty;
//! double every backslash run that precedes a quote; escape embedded quotes as
//! `\"`), so a round-trip through a conforming CRT reproduces the argv exactly.
//! `cmd.exe` ADDITIONALLY interprets its own metacharacters (`&`, `|`, `^`, `%`)
//! inside `/C`/`/K` arguments — a `-e` command routed through cmd.exe is
//! shell-interpreted, unlike Unix `execve` (documented caller-facing caveat).

use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;

/// Encode `s` as UTF-16 with a trailing NUL (the shape every `*W` API wants).
pub(crate) fn wide_nul(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Flatten `argv` into a single NUL-terminated UTF-16 command line using the
/// canonical MSVCRT quoting rules (see module docs). The trailing NUL is
/// included because `CreateProcessW` takes a mutable NUL-terminated buffer.
pub(crate) fn build_command_line(argv: &[OsString]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    for (i, arg) in argv.iter().enumerate() {
        if i > 0 {
            out.push(SPACE);
        }
        append_quoted(arg, &mut out);
    }
    out.push(0);
    out
}

/// Append one argument, quoted iff needed (empty, or contains space/tab/quote).
/// Inside quotes: a run of N backslashes before an embedded `"` becomes 2N+1
/// backslashes + the quote; a run of N trailing backslashes (before the closing
/// quote we add) becomes 2N backslashes; backslashes elsewhere pass verbatim.
fn append_quoted(arg: &OsStr, out: &mut Vec<u16>) {
    let units: Vec<u16> = arg.encode_wide().collect();
    let needs_quotes =
        units.is_empty() || units.iter().any(|&u| u == SPACE || u == TAB || u == QUOTE);
    if !needs_quotes {
        out.extend_from_slice(&units);
        return;
    }
    out.push(QUOTE);
    let mut i = 0;
    while i < units.len() {
        let mut n_backslashes = 0usize;
        while i < units.len() && units[i] == BACKSLASH {
            n_backslashes += 1;
            i += 1;
        }
        if i == units.len() {
            // Trailing run: double it so the closing quote stays a real quote.
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes * 2));
        } else if units[i] == QUOTE {
            // Escape the run AND the quote itself.
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes * 2 + 1));
            out.push(QUOTE);
            i += 1;
        } else {
            // Backslashes not before a quote are literal.
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes));
            out.push(units[i]);
            i += 1;
        }
    }
    out.push(QUOTE);
}

/// Build the `CreateProcessW` UTF-16 environment block from `build_child_env`'s
/// output: `KEY=VAL\0 … \0\0`, deduped CASE-INSENSITIVELY (last occurrence wins
/// — Windows env names are case-insensitive, so an inherited `Path` plus an
/// injected `PATH` must yield ONE entry, the injected one, or child lookups are
/// undefined) and sorted case-insensitively by key, as the `CreateProcessW`
/// docs require for a unicode environment block.
pub(crate) fn build_env_block(pairs: &[(OsString, OsString)]) -> Vec<u16> {
    // (folded key, index into `pairs` of the LAST occurrence)
    let mut kept: Vec<(Vec<u16>, usize)> = Vec::new();
    for (idx, (k, _)) in pairs.iter().enumerate() {
        let folded: Vec<u16> = k.encode_wide().map(fold_upper).collect();
        match kept.iter_mut().find(|(f, _)| *f == folded) {
            Some(slot) => slot.1 = idx, // case-insensitive dedupe: last wins
            None => kept.push((folded, idx)),
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block: Vec<u16> = Vec::new();
    for (_, idx) in &kept {
        let (k, v) = &pairs[*idx];
        block.extend(k.encode_wide());
        block.push(b'=' as u16);
        block.extend(v.encode_wide());
        block.push(0);
    }
    if kept.is_empty() {
        block.push(0); // an empty block is still double-NUL terminated
    }
    block.push(0);
    block
}

/// ASCII-uppercase fold of one UTF-16 unit — the ordinal, locale-free key fold
/// used for env-name dedupe/sort (env names are ASCII in practice; non-ASCII
/// units compare ordinally, which is deterministic and order-stable).
fn fold_upper(u: u16) -> u16 {
    if (u16::from(b'a')..=u16::from(b'z')).contains(&u) {
        u - 32
    } else {
        u
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a NUL-terminated UTF-16 command line back to a String for asserts.
    fn decode(cmdline: &[u16]) -> String {
        let no_nul = cmdline.strip_suffix(&[0]).expect("trailing NUL");
        String::from_utf16(no_nul).expect("valid UTF-16")
    }

    fn args(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn plain_args_are_unquoted_and_space_joined() {
        let c = build_command_line(&args(&["cmd.exe", "/c", "dir"]));
        assert_eq!(decode(&c), "cmd.exe /c dir");
        assert_eq!(c.last(), Some(&0), "NUL-terminated for CreateProcessW");
    }

    #[test]
    fn spaces_tabs_and_empty_args_get_quoted() {
        let c = build_command_line(&args(&["p.exe", "a b", "", "c\td"]));
        assert_eq!(decode(&c), "p.exe \"a b\" \"\" \"c\td\"");
    }

    #[test]
    fn embedded_quotes_are_backslash_escaped() {
        let c = build_command_line(&args(&["p.exe", "say \"hi\""]));
        assert_eq!(decode(&c), "p.exe \"say \\\"hi\\\"\"");
    }

    #[test]
    fn backslashes_before_a_quote_double_but_elsewhere_stay_literal() {
        // C:\dir\ as an arg with a space: the TRAILING run doubles; the interior
        // backslash is literal. An embedded \" becomes \\\" + the escaped quote.
        let c = build_command_line(&args(&["p.exe", "C:\\a dir\\"]));
        assert_eq!(decode(&c), "p.exe \"C:\\a dir\\\\\"");
        let c2 = build_command_line(&args(&["p.exe", "x\\\"y z"]));
        // arg is: x\"y z  -> quoted as "x\\\"y z"
        assert_eq!(decode(&c2), "p.exe \"x\\\\\\\"y z\"");
    }

    #[test]
    fn plain_backslash_paths_stay_verbatim_when_unquoted() {
        let c = build_command_line(&args(&["C:\\Windows\\System32\\cmd.exe"]));
        assert_eq!(decode(&c), "C:\\Windows\\System32\\cmd.exe");
    }

    /// Decode an env block into its `KEY=VAL` entries for asserts.
    fn decode_block(block: &[u16]) -> Vec<String> {
        let mut entries = Vec::new();
        let mut cur: Vec<u16> = Vec::new();
        for &u in block {
            if u == 0 {
                if cur.is_empty() {
                    break; // second NUL of the double-NUL terminator
                }
                entries.push(String::from_utf16(&cur).expect("valid UTF-16"));
                cur.clear();
            } else {
                cur.push(u);
            }
        }
        entries
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        v.iter()
            .map(|(k, val)| (OsString::from(k), OsString::from(val)))
            .collect()
    }

    #[test]
    fn env_block_sorts_case_insensitively_and_double_nul_terminates() {
        let block = build_env_block(&pairs(&[("b", "2"), ("A", "1"), ("c", "3")]));
        assert_eq!(decode_block(&block), vec!["A=1", "b=2", "c=3"]);
        let n = block.len();
        assert_eq!(&block[n - 2..], &[0, 0], "double-NUL terminator");
    }

    #[test]
    fn env_block_dedupes_case_insensitively_last_wins() {
        // The inherited-'Path'-plus-injected-'PATH' hazard: exactly ONE entry
        // survives, and it is the LAST occurrence (env_add overrides).
        let block = build_env_block(&pairs(&[
            ("Path", "C:\\old"),
            ("TERM", "dumb"),
            ("PATH", "C:\\new"),
        ]));
        assert_eq!(decode_block(&block), vec!["PATH=C:\\new", "TERM=dumb"]);
    }

    #[test]
    fn empty_env_block_is_still_double_nul() {
        assert_eq!(build_env_block(&[]), vec![0, 0]);
    }
}
