// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::valid_control_reply;
use aterm_types::app_inspection::{InspectionEnvelope, InspectionSubject};

#[test]
fn structured_inspection_does_not_relax_mutation_acknowledgements() {
    let response = InspectionEnvelope {
        revision: 3,
        subject: InspectionSubject::Tabs,
    }
    .header_line();
    assert!(valid_control_reply(
        &["inspect", "app/v1", "tabs"],
        &response
    ));
    assert!(!valid_control_reply(
        &["inspect", "app/v1", "network"],
        &response
    ));
    assert!(!valid_control_reply(&["inspect", "app/v1", "tabs"], "OK"));
    assert!(!valid_control_reply(
        &["inspect", "app/v1", "tabs"],
        "ERR refused"
    ));
    assert!(!valid_control_reply(
        &["open", "app", "settings", "/"],
        &response
    ));
    assert!(!valid_control_reply(
        &["open", "app", "settings", "/"],
        "ERR refused"
    ));
    assert!(valid_control_reply(
        &["open", "app", "settings", "/"],
        "OK opened"
    ));
}
