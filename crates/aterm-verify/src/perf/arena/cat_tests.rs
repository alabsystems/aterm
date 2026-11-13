// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_corpus_is_whole_units_reaching_the_target() {
    for corpus in [Corpus::Ascii, Corpus::Unicode] {
        let mut out = Vec::new();
        let n = generate(corpus.unit().as_bytes(), 1, &mut out).expect("generates");
        let unit = corpus.unit().len() as u64;
        assert_eq!(n, out.len() as u64);
        assert!(
            n >= 1 << 20 && n < (1 << 20) + unit,
            "{}: {n}",
            corpus.name()
        );
        assert_eq!(n % unit, 0, "{}", corpus.name());
        assert!(std::str::from_utf8(&out).is_ok());
    }
}

#[test]
fn the_order_rotates_every_trial() {
    assert_eq!(order(1), [Term::Aterm, Term::Ghostty]);
    assert_eq!(order(2), [Term::Ghostty, Term::Aterm]);
}

#[test]
fn an_aterm_under_test_gets_a_private_home_and_xdg_tree_of_its_own() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = Scratch::new("arena-term-test").expect("scratch");
    let env = private_root(root.path()).expect("private root");
    for key in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_DATA_HOME",
        "TMPDIR",
    ] {
        let (_, dir) = env.iter().find(|(k, _)| *k == key).expect(key);
        assert!(dir.starts_with(root.path()), "{key} {}", dir.display());
        let mode = std::fs::metadata(dir).expect(key).permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{key}");
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("config/aterm/aterm.toml")).expect("config"),
        super::super::start::config()
    );
}

#[test]
fn a_drain_report_is_positive_seconds() {
    assert_eq!(parse_drain("drain 1.25"), Ok(1.25));
    assert!(parse_drain("drain 0").is_err());
    assert!(parse_drain("fail cat exited 1").is_err());
}
