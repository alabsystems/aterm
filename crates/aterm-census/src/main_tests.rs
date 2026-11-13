// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn names(args: &[&str]) -> Vec<&'static str> {
    let parsed = parse(args.iter().map(|a| (*a).to_string()))
        .expect("parses")
        .expect("not --help");
    parsed.selected.iter().map(|c| c.name).collect()
}

#[test]
fn bare_and_all_select_every_census_once_in_table_order() {
    let every: Vec<&str> = CENSUSES.iter().map(|c| c.name).collect();
    assert_eq!(names(&[]), every);
    assert_eq!(names(&["--all"]), every);
    assert_eq!(names(&["--locks", "--all", "--scope"]), every);
}

#[test]
fn census_flags_combine_instead_of_the_last_one_winning() {
    assert_eq!(
        names(&["--scope", "--locks", "--scope"]),
        ["locks", "scope"]
    );
}

#[test]
fn a_root_and_flags_mix_in_any_order() {
    let parsed = parse(["/x".to_string(), "--wasm".to_string()])
        .expect("parses")
        .expect("not --help");
    assert_eq!(parsed.root.as_deref(), Some(Path::new("/x")));
    assert_eq!(parsed.selected.len(), 1);
}

#[test]
fn memo_takes_the_next_word_as_its_file_not_as_the_root() {
    for args in [&["--memo", "/m", "/x"][..], &["--memo=/m", "/x"]] {
        let parsed = parse(args.iter().map(|a| (*a).to_string()))
            .expect("parses")
            .expect("not --help");
        assert_eq!(parsed.memo.as_deref(), Some(Path::new("/m")));
        assert_eq!(parsed.root.as_deref(), Some(Path::new("/x")));
    }
}

#[test]
fn verbose_is_a_flag_not_a_root() {
    for flag in ["-v", "--verbose"] {
        let parsed = parse([flag.to_string()])
            .expect("parses")
            .expect("not --help");
        assert!(parsed.verbose && parsed.root.is_none());
    }
}

#[test]
fn a_root_with_shell_metacharacters_is_quoted_for_the_rerun_line() {
    assert_eq!(shell_word(Path::new("/a/b-c.d")), "/a/b-c.d");
    assert_eq!(shell_word(Path::new("/a b/it's")), r"'/a b/it'\''s'");
}

#[test]
fn unknown_flags_and_a_second_root_are_usage_errors() {
    assert!(parse(["--lock".to_string()]).is_err());
    assert!(parse(["-x".to_string()]).is_err());
    assert!(parse(["a".to_string(), "b".to_string()]).is_err());
    assert!(parse(["--memo".to_string()]).is_err());
    assert!(matches!(parse(["--help".to_string()]), Ok(None)));
}
