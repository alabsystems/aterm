// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn reference_is_one_probe_free_abba_and_unknown_roles_refuse() {
    assert_eq!(
        Plan::Reference.arms(),
        &[(false, false), (true, false), (true, false), (false, false)]
    );
    let focal = json!({"darwin_role":1,"errno":null});
    let inherited = json!({"darwin_role":0,"errno":null});
    assert!(reference_roles(&focal, &inherited));
    assert!(!reference_roles(&json!({"darwin_role":1}), &inherited));
    assert!(!reference_roles(&inherited, &focal));
    assert!(!reference_roles(
        &focal,
        &json!({"darwin_role":null,"errno":1})
    ));
    assert!(!reference_roles(
        &json!({"darwin_role":null,"errno":null}),
        &inherited
    ));
}

#[test]
fn opt_in_readback_is_required_and_never_accepted_in_the_off_arm() {
    assert!(trace_fields("OK enabled=true", false).is_ok());
    assert!(trace_fields("OK enabled=true", true).is_err());
    assert!(trace_fields("OK recording_cost_scope=partial", true).is_err());
    assert!(trace_fields("OK recording_cost_scope=anything", false).is_err());
    let mut complete = String::from(
        "OK recording_cost_scope=recorder_lifetime_completed,inclusive,polling_nonatomic",
    );
    for stage in STAGES {
        for suffix in ["calls", "work", "wall_ns", "cpu_ns", "cpu_unknown_scopes"] {
            complete.push_str(&format!(" recording_cost_{stage}_{suffix}=-"));
        }
    }
    assert!(
        trace_fields(&complete, true).is_ok(),
        "unknown counters remain observable unknowns"
    );
    assert!(
        trace_fields(
            &complete.replace(
                "recording_cost_parse_cpu_ns=-",
                "recording_cost_parse_cpu_ns=broken"
            ),
            true
        )
        .is_err()
    );
}
