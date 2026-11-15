// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn selection_is_native_session_only_and_effort_can_keep_the_model() {
    assert_eq!(
        Selection::parse("set model=gpt-6.1-sol effort=ultra")
            .unwrap()
            .codex_command(),
        "/model gpt-6.1-sol --reasoning=ultra"
    );
    assert_eq!(
        Selection::parse("set effort=ultra")
            .unwrap()
            .codex_command(),
        "/model --reasoning=ultra"
    );
    assert_eq!(
        Selection::parse("set model=gpt-6.1-sol")
            .unwrap()
            .codex_command(),
        "/model gpt-6.1-sol"
    );
}

#[test]
fn selection_refuses_injection_duplicates_and_unknown_settings() {
    for invalid in [
        "set",
        "set model=",
        "set model=-bad",
        "set model=x;quit",
        "set model=a model=b",
        "set effort=ultra effort=low",
        "set effort=unknown",
        "set profile=admin",
        "set model=a\nset effort=high",
        "set model=a\t",
        "set effort=HIGH",
    ] {
        assert!(Selection::parse(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn remote_metadata_is_autopilot_only() {
    for valid in [
        "set autopilot cruise",
        "set autopilot land",
        "set autopilot off",
        "unset autopilot",
        "set autopilot cruise until=+2h",
    ] {
        assert!(autopilot_write(valid), "{valid}");
    }
    for invalid in [
        "",
        "set supervisor me",
        "unset supervisor",
        "set autopilot cruise supervisor=me",
        "set autopilot cruise\nset supervisor me",
        "unset autopilot anything",
        "set autopilot land until=+2h",
        "set autopilot cruise until=",
    ] {
        assert!(!autopilot_write(invalid), "{invalid:?}");
    }
}
