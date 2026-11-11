// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn p(s: &[&str]) -> Result<Invocation, ParseError> {
    parse(s.iter().map(|x| x.to_string()))
}

#[test]
fn serde_attrs_takes_any_number_of_paths() {
    assert_eq!(
        p(&["serde-attrs"]).unwrap().cmd,
        Cmd::SerdeAttrs { paths: vec![] }
    );
    assert_eq!(
        p(&["serde-attrs", "crates/aterm-serde", "crates/x"])
            .unwrap()
            .cmd,
        Cmd::SerdeAttrs {
            paths: vec!["crates/aterm-serde".into(), "crates/x".into()]
        }
    );
    assert_eq!(
        p(&["blame", "pkg1", "pkg2"]),
        Err(ParseError::UnexpectedArg("pkg2".into()))
    );
    for verb in ["survey", "budget", "attest", "check"] {
        assert_eq!(
            p(&[verb, "stray"]),
            Err(ParseError::UnexpectedArg("stray".into()))
        );
    }
}

#[test]
fn abrew_help_defaults_to_check_and_import_is_an_explicit_separate_mode() {
    use aterm_forge::abrew_help::{INPUT, Mode, Options};
    assert_eq!(
        p(&["abrew-help"]).unwrap().cmd,
        Cmd::AbrewHelp {
            options: Options {
                input: INPUT.into(),
                mode: Mode::Check
            }
        }
    );
    assert_eq!(
        p(&["abrew-help", "--input", "data/new.json", "--write"])
            .unwrap()
            .cmd,
        Cmd::AbrewHelp {
            options: Options {
                input: "data/new.json".into(),
                mode: Mode::Write
            }
        }
    );
    assert_eq!(
        p(&[
            "abrew-help",
            "import-legacy",
            "--input",
            "/tmp/f15.rs",
            "--output",
            "data/capture.json"
        ])
        .unwrap()
        .cmd,
        Cmd::AbrewHelp {
            options: Options {
                input: "/tmp/f15.rs".into(),
                mode: Mode::ImportLegacy {
                    output: "data/capture.json".into()
                }
            }
        }
    );
    for bad in [
        vec!["loc", "--top", "+10"],
        vec!["abrew-help", "--check", "--write"],
        vec!["abrew-help", "--output", "elsewhere"],
        vec!["abrew-help", "import-legacy", "--input", "capture.rs"],
        vec![
            "abrew-help",
            "import-legacy",
            "--input",
            "capture.rs",
            "--output",
            "capture.json",
            "--write",
        ],
    ] {
        assert!(p(&bad).is_err());
    }
}
