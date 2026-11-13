// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{DEVTOOLS_USAGE, devtools_reply, parse_devtools};
use crate::devtools_helper::{HandOver, Refusal};

#[test]
fn the_grammar_is_hand_over_with_an_optional_now() {
    assert_eq!(parse_devtools("hand-over"), Ok(false));
    assert_eq!(parse_devtools("  hand-over  "), Ok(false));
    assert_eq!(parse_devtools("hand-over now"), Ok(true));
    assert_eq!(parse_devtools("hand-over   now\t"), Ok(true));
    for junk in [
        "",
        "   ",
        "handover",
        "hand-over later",
        "hand-over now now",
        "now",
        "now hand-over",
        "HAND-OVER",
        "hand-over NOW",
        "open",
    ] {
        assert_eq!(parse_devtools(junk), Err(DEVTOOLS_USAGE), "{junk:?}");
    }
}

#[test]
fn every_outcome_has_one_answer_and_the_help_names_the_verbs_own() {
    let asked = [
        (HandOver::Queued, "OK queued\n"),
        (HandOver::Refused(Refusal::Granted), "OK granted\n"),
        (HandOver::Refused(Refusal::Unknown), "OK unknown\n"),
        (HandOver::Refused(Refusal::Declined), "OK declined\n"),
        (HandOver::Refused(Refusal::Unstable), "OK unstable\n"),
        (HandOver::Refused(Refusal::Recent), "OK recent\n"),
        (HandOver::Refused(Refusal::Listed), "OK listed\n"),
    ];
    let help = aterm_types::control_verbs::spec("devtools")
        .expect("devtools is in the table")
        .help_line();
    for (outcome, want) in asked {
        assert_eq!(devtools_reply(outcome), want);
        let words = format!("`{}`", want.trim_end());
        assert!(help.contains(&words), "the help names {words}\n{help}");
    }
    let headless = devtools_reply(HandOver::Headless);
    assert!(headless.starts_with("ERR headless: "), "{headless}");
    assert!(help.contains("`ERR headless: \u{2026}`"), "{help}");
    // `now` answers as `permissions walk now devtools` does.
    assert!(help.contains("`permissions walk now devtools`"), "{help}");
    assert_eq!(devtools_reply(HandOver::Opened), "OK opened\n");
    assert_eq!(devtools_reply(HandOver::Listing), "OK listing\n");
    assert!(devtools_reply(HandOver::OpenFailed).starts_with("ERR "));
}
