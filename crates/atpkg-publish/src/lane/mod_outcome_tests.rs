// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn typed_outcomes_do_not_turn_idle_or_failure_into_publication() {
    let flags = RunFlags::default();
    for (outcome, result, status, exit, index) in [
        (
            Some(Summary {
                idle: true,
                ..Summary::default()
            }),
            Ok(()),
            "idle",
            0,
            None,
        ),
        (
            Some(Summary {
                covered: true,
                ..Summary::default()
            }),
            Ok(()),
            "covered",
            0,
            None,
        ),
        (
            Some(Summary {
                published: Some(118),
                ..Summary::default()
            }),
            Ok(()),
            "published",
            0,
            Some(118),
        ),
        (
            None,
            Err(Error::refused("signer unavailable")),
            "refused",
            1,
            None,
        ),
    ] {
        let encoded =
            aterm_json::to_string(&outcome_report(&flags, outcome.as_ref(), &result, vec![]))
                .unwrap();
        let value: aterm_json::Value = aterm_json::from_str(&encoded).unwrap();
        assert_eq!(value["status"].as_str(), Some(status));
        assert_eq!(value["exit"].as_u64(), Some(exit));
        assert_eq!(value["published_index"].as_u64(), index);
    }
}

#[test]
fn a_run_that_ends_red_still_reports_what_it_queued_and_holds() {
    let outcome = Summary {
        head: Some(5),
        ready: vec!["ay=5".into()],
        held: vec!["nn=4: its repository declares no qualify list".into()],
        ..Summary::default()
    };
    let result = Err(Error::refused("no qualify list in the repository of nn"));
    let report = outcome_report(&RunFlags::default(), Some(&outcome), &result, vec![]);
    let value: aterm_json::Value =
        aterm_json::from_str(&aterm_json::to_string(&report).unwrap()).unwrap();
    assert_eq!(value["status"].as_str(), Some("refused"));
    assert_eq!(value["exit"].as_u64(), Some(1));
    assert_eq!(value["ready"][0].as_str(), Some("ay=5"));
    assert_eq!(
        value["held"][0].as_str(),
        Some("nn=4: its repository declares no qualify list")
    );
    assert!(
        value["error"]
            .as_str()
            .is_some_and(|e| e.contains("repository of nn"))
    );
}
