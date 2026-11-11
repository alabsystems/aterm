// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn typed_outcomes_do_not_turn_idle_or_failure_into_publication() {
    let flags = RunFlags::default();
    for (result, status, exit, index) in [
        (
            Ok(Some(linux::Outcome {
                idle: true,
                ..linux::Outcome::default()
            })),
            "idle",
            0,
            None,
        ),
        (
            Ok(Some(linux::Outcome {
                covered: true,
                ..linux::Outcome::default()
            })),
            "covered",
            0,
            None,
        ),
        (
            Ok(Some(linux::Outcome {
                published: Some(118),
                ..linux::Outcome::default()
            })),
            "published",
            0,
            Some(118),
        ),
        (
            Err(Error::refused("signer unavailable")),
            "refused",
            1,
            None,
        ),
    ] {
        let encoded = aterm_json::to_string(&outcome_report(&flags, &result, vec![])).unwrap();
        let value: aterm_json::Value = aterm_json::from_str(&encoded).unwrap();
        assert_eq!(value["status"].as_str(), Some(status));
        assert_eq!(value["exit"].as_u64(), Some(exit));
        assert_eq!(value["published_index"].as_u64(), index);
    }
}
