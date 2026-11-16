// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{RecordsOptions, records_options};

fn parse(words: &[&str]) -> Result<Option<RecordsOptions>, ()> {
    records_options(
        &words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>(),
    )
}

fn in_each_order(words: &[&str], check: &mut dyn FnMut(&[&str])) {
    fn visit<'a>(
        remaining: &mut Vec<&'a str>,
        ordered: &mut Vec<&'a str>,
        check: &mut dyn FnMut(&[&str]),
    ) {
        if remaining.is_empty() {
            check(ordered);
            return;
        }
        for index in 0..remaining.len() {
            let word = remaining.remove(index);
            ordered.push(word);
            visit(remaining, ordered, check);
            let _ = ordered.pop();
            remaining.insert(index, word);
        }
    }
    visit(&mut words.to_vec(), &mut Vec::new(), check);
}

#[test]
fn records_listing_keeps_the_named_tree_and_seal_choice_in_either_order() {
    for (words, named, show_seal) in [
        (vec![], None, false),
        (vec!["HEAD"], Some("HEAD"), false),
        (vec!["origin/main"], Some("origin/main"), false),
        (vec!["--show-seal"], None, true),
        (vec!["HEAD", "--show-seal"], Some("HEAD"), true),
        (vec!["--show-seal", "HEAD"], Some("HEAD"), true),
    ] {
        match parse(&words) {
            Ok(Some(RecordsOptions::List {
                named: actual,
                show_seal: actual_seal,
            })) => {
                assert_eq!(actual.as_deref(), named, "{words:?}");
                assert_eq!(actual_seal, show_seal, "{words:?}");
            }
            _ => panic!("{words:?} must select listing"),
        }
    }
}

#[test]
fn records_flush_is_an_explicit_standalone_mode() {
    assert!(matches!(
        parse(&["--flush"]),
        Ok(Some(RecordsOptions::Flush))
    ));
    for words in [
        vec!["--flush", "HEAD"],
        vec!["--flush", ""],
        vec!["--flush", "--show-seal"],
        vec!["--flush", "--flush"],
        vec!["--flush", "--unknown"],
        vec!["--flush", "HEAD", "--show-seal"],
    ] {
        in_each_order(&words, &mut |ordered| {
            assert!(parse(ordered).is_err(), "{ordered:?}");
        });
    }
}

#[test]
fn records_help_is_available_for_listing_but_never_combines_with_a_flush() {
    for help in ["--help", "-h"] {
        assert!(matches!(parse(&[help]), Ok(None)), "{help}");
        for words in [vec![help, "HEAD"], vec![help, "HEAD", "--show-seal"]] {
            in_each_order(&words, &mut |ordered| {
                assert!(matches!(parse(ordered), Ok(None)), "{ordered:?}");
            });
        }
        for words in [vec![help, "--flush"], vec![help, "--flush", "HEAD"]] {
            in_each_order(&words, &mut |ordered| {
                assert!(parse(ordered).is_err(), "{ordered:?}");
            });
        }
    }
}

#[test]
fn records_listing_help_needs_no_checkout() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let absent = scratch.path().join("absent-checkout");
    for words in [["HEAD", "--help"], ["--show-seal", "-h"]] {
        let args = words.map(str::to_owned);
        assert_eq!(super::records_cli(&args, &absent), crate::exit::PASS);
    }
    assert!(!absent.exists(), "help created no checkout state");
}

#[test]
fn records_listing_refuses_extra_names_duplicate_flags_and_unknown_options() {
    for words in [
        vec!["HEAD", "origin/main"],
        vec!["--show-seal", "--show-seal"],
        vec!["--unknown"],
        vec!["--unknown", "HEAD"],
        vec!["--show-seal", "--unknown", "HEAD"],
        vec!["--show-seal", "--show-seal", "HEAD"],
    ] {
        in_each_order(&words, &mut |ordered| {
            assert!(parse(ordered).is_err(), "{ordered:?}");
        });
    }
}
