// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The rows a case reads are the ledger's rows about its own folder, and only a row approved
/// with no box read and the launch step's reason is a trust recorded before the launch: a
/// dialog answered on the screen under the same rule, and a row about another folder, are not.
#[test]
fn only_a_row_with_no_box_and_the_launch_steps_reason_is_recorded_before_the_launch() {
    let ledger = concat!(
        r#"{"t_ms":1,"sid":"s-1","rule_id":"trust-any@v1","decision":"approved","command":"/w/repo","reason":"recorded before launch by Claude Code 2.1.323","box_seq":0}"#,
        "\n",
        r#"{"t_ms":2,"sid":"s-1","rule_id":"trust-any@v1","decision":"approved","command":"/w/home","reason":"the folder-trust dialog","box_seq":7}"#,
        "\n",
        "not a row\n",
        r#"{"t_ms":3,"sid":"s-1","rule_id":"trust-any@v1","decision":"escalated","command":"/w/held","reason":"recorded before launch by Claude Code 2.1.323","box_seq":0}"#,
        "\n",
    );
    let repo = rows_for(ledger, "/w/repo");
    assert_eq!(
        repo,
        [Row {
            rule: "trust-any@v1".into(),
            decision: "approved".into(),
            reason: "recorded before launch by Claude Code 2.1.323".into(),
            box_seq: 0,
        }]
    );
    assert!(repo[0].before_launch());
    let home = rows_for(ledger, "/w/home");
    assert_eq!(home.len(), 1);
    assert!(!home[0].before_launch(), "a box was read for it");
    let held = rows_for(ledger, "/w/held");
    assert_eq!(held.len(), 1);
    assert!(!held[0].before_launch(), "not approved");
    assert!(
        rows_for(ledger, "/w").is_empty(),
        "a folder above is no row's subject"
    );
}

#[test]
fn the_options_are_the_answer_file_alone() {
    let args = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
    assert_eq!(Opts::parse(&[]), Ok(Opts::default()));
    assert_eq!(
        Opts::parse(&args(&["--answer", "/tmp/answer.json"])),
        Ok(Opts {
            answer: Some(PathBuf::from("/tmp/answer.json")),
        })
    );
    assert!(Opts::parse(&args(&["--answer"])).is_err());
    assert!(Opts::parse(&args(&["--keep"])).is_err());
}
