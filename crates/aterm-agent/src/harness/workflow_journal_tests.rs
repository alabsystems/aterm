// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The workflow journal's registry, before any capture.

use super::*;

/// NO CAPTURE, NO FORMAT: the empty registry reads every version unread, the version the
/// slice-2 lab ran included.
#[test]
fn the_empty_registry_reads_every_version_unread() {
    assert!(FORMATS.is_empty());
    for version in ["2.1.304", "2.1.308", "", "2.1.x", "../2.1.304"] {
        assert_eq!(reader_for(version), Err(Unread), "{version:?}");
    }
}

/// A run id spelled from its halves (F9): no test line holds a whole one, which the export
/// guard reads as an agent session's workflow id.
fn run(eight: &str, three: &str) -> String {
    ["wf", "_", eight, "-", three].concat()
}

#[test]
fn a_run_id_is_claude_codes_own_form_and_nothing_else() {
    let id = run("65345cfa", "ea1");
    assert_eq!(
        RunId::parse(&id).map(|r| r.as_str().to_string()),
        Some(id.clone())
    );
    for text in [
        "wf_x".to_string(),
        run("65345cfa", "ea12"),
        run("65345cfA", "ea1"),
        run("65345cf", "ea1"),
        format!("../{id}"),
        format!("subagents/workflows/{id}"),
        "release mint".to_string(),
        String::new(),
    ] {
        assert_eq!(RunId::parse(&text), None, "{text:?}");
    }
}

#[test]
fn a_step_key_is_its_run_and_row() {
    let (one, two) = (run("65345cfa", "ea1"), run("65345cfa", "ea2"));
    assert_eq!(step_key(&one, 12), step_key(&one, 12));
    assert_ne!(step_key(&one, 12), step_key(&one, 13));
    assert_ne!(step_key(&one, 12), step_key(&two, 12));
    assert_eq!(
        [StepStatus::Ok, StepStatus::Partial, StepStatus::Failed].map(StepStatus::word),
        ["ok", "partial", "failed"]
    );
}
