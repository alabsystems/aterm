// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Platform-neutral UTF-16 command-line quoting (MSVCRT/`CommandLineToArgvW` rules) and
//! `CreateProcessW` environment-block construction.

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;

/// Flatten UTF-16 `argv` slices into one NUL-terminated UTF-16 command line under the MSVCRT
/// quoting rules. `argv[0]` must not contain `'"'` (Windows file names forbid `'"'`, and MSVCRT
/// parses `argv[0]` with literal backslashes via [`append_argv0_units`]); `argv[1..]` escape
/// quotes and pre-quote backslashes via [`append_quoted_units`].
pub(crate) fn build_command_line_units<I, S>(argv: I) -> Vec<u16>
where
    I: IntoIterator<Item = S>,
    S: AsRef<[u16]>,
{
    let mut out: Vec<u16> = Vec::new();
    for (i, arg) in argv.into_iter().enumerate() {
        let units = arg.as_ref();
        if i == 0 {
            append_argv0_units(units, &mut out);
        } else {
            out.push(SPACE);
            append_quoted_units(units, &mut out);
        }
    }
    out.push(0);
    out
}

/// Quote `argv[0]` under MSVCRT/`CommandLineToArgvW`'s program-name rule: backslashes in `argv[0]`
/// are always literal, so trailing backslashes inside quotes must NOT be doubled. Precondition:
/// `units` does not contain `'"'`.
pub(crate) fn append_argv0_units(units: &[u16], out: &mut Vec<u16>) {
    let needs_quotes =
        units.is_empty() || units.iter().any(|&u| u == SPACE || u == TAB || u == QUOTE);
    if !needs_quotes {
        out.extend_from_slice(units);
        return;
    }
    out.push(QUOTE);
    out.extend_from_slice(units);
    out.push(QUOTE);
}

/// Quote `argv[i]` (`i >= 1`) under MSVCRT/`CommandLineToArgvW` argument rules: a run of `n`
/// backslashes before `'"'` becomes `2n + 1` backslashes plus `'"'`, a trailing run inside quotes
/// becomes `2n` backslashes before the closing `'"'`, and other backslashes stay literal.
pub(crate) fn append_quoted_units(units: &[u16], out: &mut Vec<u16>) {
    let needs_quotes =
        units.is_empty() || units.iter().any(|&u| u == SPACE || u == TAB || u == QUOTE);
    if !needs_quotes {
        out.extend_from_slice(units);
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
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes * 2));
        } else if units[i] == QUOTE {
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes * 2 + 1));
            out.push(QUOTE);
            i += 1;
        } else {
            out.extend(std::iter::repeat_n(BACKSLASH, n_backslashes));
            out.push(units[i]);
            i += 1;
        }
    }
    out.push(QUOTE);
}

/// Build the `CreateProcessW` UTF-16 environment block (`KEY=VAL\0 … \0\0`), deduped
/// ASCII-case-insensitively with the last occurrence winning and sorted ASCII-case-insensitively
/// by key.
pub(crate) fn build_env_block_units<K, V>(pairs: &[(K, V)]) -> Vec<u16>
where
    K: AsRef<[u16]>,
    V: AsRef<[u16]>,
{
    let mut kept: Vec<(Vec<u16>, usize)> = Vec::new();
    for (idx, (k, _)) in pairs.iter().enumerate() {
        let folded: Vec<u16> = k.as_ref().iter().copied().map(fold_upper).collect();
        match kept.iter_mut().find(|(f, _)| *f == folded) {
            Some(slot) => slot.1 = idx,
            None => kept.push((folded, idx)),
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block: Vec<u16> = Vec::new();
    for (_, idx) in &kept {
        let (k, v) = &pairs[*idx];
        block.extend_from_slice(k.as_ref());
        block.push(b'=' as u16);
        block.extend_from_slice(v.as_ref());
        block.push(0);
    }
    if kept.is_empty() {
        block.push(0);
    }
    block.push(0);
    block
}

/// Fold ASCII `'a'..='z'` to uppercase in UTF-16.
#[inline]
pub(crate) const fn fold_upper(u: u16) -> u16 {
    if u >= (b'a' as u16) && u <= (b'z' as u16) {
        u - 32
    } else {
        u
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(cmdline: &[u16]) -> String {
        let no_nul = cmdline.strip_suffix(&[0]).expect("trailing NUL");
        String::from_utf16(no_nul).expect("valid UTF-16")
    }

    fn args(v: &[&str]) -> Vec<Vec<u16>> {
        v.iter().map(|s| s.encode_utf16().collect()).collect()
    }

    /// Reference MSVCRT/UCRT command-line parser (`parse_cmdline` / `CommandLineToArgvW`).
    fn crt_parse(cmdline_no_nul: &[u16]) -> Vec<Vec<u16>> {
        if cmdline_no_nul.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Vec<u16>> = Vec::new();
        let mut i = 0usize;

        // `argv[0]`: `"` toggles quote mode; every other unit (including `\`) is literal.
        let mut argv0: Vec<u16> = Vec::new();
        let mut in_quotes = false;
        while i < cmdline_no_nul.len() {
            let u = cmdline_no_nul[i];
            if u == QUOTE {
                in_quotes = !in_quotes;
                i += 1;
            } else if !in_quotes && (u == SPACE || u == TAB) {
                break;
            } else {
                argv0.push(u);
                i += 1;
            }
        }
        out.push(argv0);

        // `argv[1..]`: backslash runs double only before `"`.
        loop {
            while i < cmdline_no_nul.len()
                && (cmdline_no_nul[i] == SPACE || cmdline_no_nul[i] == TAB)
            {
                i += 1;
            }
            if i == cmdline_no_nul.len() {
                break;
            }
            let mut cur: Vec<u16> = Vec::new();
            let mut arg_quotes = false;
            loop {
                let mut slashes = 0usize;
                while i < cmdline_no_nul.len() && cmdline_no_nul[i] == BACKSLASH {
                    slashes += 1;
                    i += 1;
                }
                if i < cmdline_no_nul.len() && cmdline_no_nul[i] == QUOTE {
                    cur.extend(std::iter::repeat_n(BACKSLASH, slashes / 2));
                    if slashes.is_multiple_of(2) {
                        arg_quotes = !arg_quotes;
                    } else {
                        cur.push(QUOTE);
                    }
                    i += 1;
                } else {
                    cur.extend(std::iter::repeat_n(BACKSLASH, slashes));
                    if i == cmdline_no_nul.len()
                        || (!arg_quotes && (cmdline_no_nul[i] == SPACE || cmdline_no_nul[i] == TAB))
                    {
                        break;
                    }
                    cur.push(cmdline_no_nul[i]);
                    i += 1;
                }
            }
            out.push(cur);
        }
        out
    }

    #[test]
    fn plain_args_are_unquoted_and_space_joined() {
        let c = build_command_line_units(args(&["cmd.exe", "/c", "dir"]));
        assert_eq!(decode(&c), "cmd.exe /c dir");
        assert_eq!(c.last(), Some(&0), "NUL-terminated for CreateProcessW");
    }

    #[test]
    fn spaces_tabs_and_empty_args_get_quoted() {
        let c = build_command_line_units(args(&["p.exe", "a b", "", "c\td"]));
        assert_eq!(decode(&c), "p.exe \"a b\" \"\" \"c\td\"");
    }

    #[test]
    fn embedded_quotes_are_backslash_escaped() {
        let c = build_command_line_units(args(&["p.exe", "say \"hi\""]));
        assert_eq!(decode(&c), "p.exe \"say \\\"hi\\\"\"");
    }

    #[test]
    fn backslashes_before_a_quote_double_but_elsewhere_stay_literal() {
        let c = build_command_line_units(args(&["p.exe", "C:\\a dir\\"]));
        assert_eq!(decode(&c), "p.exe \"C:\\a dir\\\\\"");
        let c2 = build_command_line_units(args(&["p.exe", "x\\\"y z"]));
        assert_eq!(decode(&c2), "p.exe \"x\\\\\\\"y z\"");
    }

    #[test]
    fn plain_backslash_paths_stay_verbatim_when_unquoted() {
        let c = build_command_line_units(args(&["C:\\Windows\\System32\\cmd.exe"]));
        assert_eq!(decode(&c), "C:\\Windows\\System32\\cmd.exe");
    }

    /// NEGATIVE CONTROL: applying `append_quoted_units` to `argv[0] = "C:\\a dir\\"` doubles the
    /// trailing backslash (`"\"C:\\a dir\\\\\""`), which `crt_parse` misreads as `"C:\\a dir\\\\"`;
    /// `build_command_line_units` uses `append_argv0_units` on `argv[0]` so every `'"'`-free
    /// `argv[0]` round-trips through `crt_parse`.
    #[test]
    fn argv0_quotes_without_doubling_backslashes_and_round_trips_crt_parse() {
        let argv0_dir = "C:\\a dir\\";
        let mut buggy_out = Vec::new();
        append_quoted_units(
            &argv0_dir.encode_utf16().collect::<Vec<_>>(),
            &mut buggy_out,
        );
        assert_eq!(
            crt_parse(&buggy_out),
            args(&["C:\\a dir\\\\"]),
            "append_quoted_units on argv[0] corrupts trailing backslashes under MSVCRT rules"
        );

        let both = build_command_line_units(args(&[argv0_dir, argv0_dir]));
        assert_eq!(decode(&both), "\"C:\\a dir\\\" \"C:\\a dir\\\\\"");
        assert_eq!(
            crt_parse(both.strip_suffix(&[0]).unwrap()),
            args(&[argv0_dir, argv0_dir])
        );

        let argv0_cases = [
            "",
            "p.exe",
            " p.exe",
            "\tp.exe",
            "C:\\Program Files\\aterm\\aterm.exe",
            "C:\\a dir\\",
            "C:\\two trailing\\\\",
            "a\tb\\c\\",
        ];
        let arg_tail = [
            "",
            "plain",
            "C:\\a dir\\",
            "say \"hi\"",
            "x\\\"y z",
            "trailing\\\\",
        ];
        for p in argv0_cases {
            let mut full = vec![p];
            full.extend_from_slice(&arg_tail);
            let encoded = args(&full);
            let cmd = build_command_line_units(&encoded);
            let parsed = crt_parse(cmd.strip_suffix(&[0]).expect("trailing NUL"));
            assert_eq!(parsed, encoded, "round-trip failed for argv[0]={p:?}");
        }
        let empty: Vec<Vec<u16>> = Vec::new();
        let empty_cmd = build_command_line_units(&empty);
        assert_eq!(empty_cmd, vec![0]);
        assert!(crt_parse(empty_cmd.strip_suffix(&[0]).unwrap()).is_empty());
    }

    fn decode_block(block: &[u16]) -> Vec<String> {
        let mut entries = Vec::new();
        let mut cur: Vec<u16> = Vec::new();
        for &u in block {
            if u == 0 {
                if cur.is_empty() {
                    break;
                }
                entries.push(String::from_utf16(&cur).expect("valid UTF-16"));
                cur.clear();
            } else {
                cur.push(u);
            }
        }
        entries
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(Vec<u16>, Vec<u16>)> {
        v.iter()
            .map(|(k, val)| (k.encode_utf16().collect(), val.encode_utf16().collect()))
            .collect()
    }

    #[test]
    fn env_block_sorts_case_insensitively_and_double_nul_terminates() {
        let block = build_env_block_units(&pairs(&[("b", "2"), ("A", "1"), ("c", "3")]));
        assert_eq!(decode_block(&block), vec!["A=1", "b=2", "c=3"]);
        let n = block.len();
        assert_eq!(&block[n - 2..], &[0, 0], "double-NUL terminator");
    }

    #[test]
    fn env_block_dedupes_case_insensitively_last_wins() {
        let block = build_env_block_units(&pairs(&[
            ("Path", "C:\\old"),
            ("TERM", "dumb"),
            ("PATH", "C:\\new"),
        ]));
        assert_eq!(decode_block(&block), vec!["PATH=C:\\new", "TERM=dumb"]);
    }

    #[test]
    fn empty_env_block_is_still_double_nul() {
        let empty: Vec<(Vec<u16>, Vec<u16>)> = Vec::new();
        assert_eq!(build_env_block_units(&empty), vec![0, 0]);
    }
}
