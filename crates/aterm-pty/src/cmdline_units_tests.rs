// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
        while i < cmdline_no_nul.len() && (cmdline_no_nul[i] == SPACE || cmdline_no_nul[i] == TAB) {
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
