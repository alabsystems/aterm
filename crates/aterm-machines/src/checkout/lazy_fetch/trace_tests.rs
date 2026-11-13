// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::checkout::lazy_fetch::OBJECT_NAMED;

fn events(format: &str, attempted: bool) -> Vec<aterm_json::Value> {
    let mut events = vec![
        aterm_json::json!({"event":"version","sid":"root","evt":format}),
        aterm_json::json!({"event":"start","sid":"root","argv":["git","cat-file","-t",OBJECT_NAMED]}),
        aterm_json::json!({"event":"cmd_name","sid":"root","name":"cat-file"}),
    ];
    if attempted {
        events.push(aterm_json::json!({"event":"child_start","sid":"root","argv":["git","fetch"]}));
    }
    events.extend([
        aterm_json::json!({"event":"exit","sid":"root","code":128}),
        aterm_json::json!({"event":"atexit","sid":"root","code":128}),
    ]);
    events
}

fn encode(events: &[aterm_json::Value]) -> Vec<u8> {
    format!(
        "{}\n",
        events
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    )
    .into_bytes()
}

#[test]
fn complete_reads_distinguish_quiet_refusals_from_failed_fetch_attempts() {
    for format in ["3", "4"] {
        for attempted in [false, true] {
            let got = observe(&encode(&events(format, attempted)), OBJECT_NAMED, 128).unwrap();
            assert_eq!(got.attempted, attempted);
            assert_eq!(got.format.to_string(), format);
        }
    }
    let mut exec = events("4", true);
    exec[3]
        .as_object_mut()
        .unwrap()
        .insert("event".into(), "exec".into());
    assert!(
        observe(&encode(&exec), OBJECT_NAMED, 128)
            .unwrap()
            .attempted
    );
}

#[test]
fn every_process_indication_refuses_quiet_even_without_its_start_record() {
    for event in [
        "child_start",
        "child_exit",
        "child_ready",
        "exec",
        "exec_result",
    ] {
        let mut trace = events("4", false);
        trace.insert(3, aterm_json::json!({"event":event,"sid":"root"}));
        assert!(
            observe(&encode(&trace), OBJECT_NAMED, 128)
                .unwrap()
                .attempted,
            "{event}"
        );
    }
    let mut trace = events("4", false);
    trace.insert(
        3,
        aterm_json::json!({"event":"version","sid":"root/child","evt":"4"}),
    );
    assert!(
        observe(&encode(&trace), OBJECT_NAMED, 128)
            .unwrap()
            .attempted
    );
}

#[test]
fn missing_mismatched_and_truncated_evidence_establishes_no_read() {
    let valid = events("4", false);
    for index in 0..valid.len() {
        let mut missing = valid.clone();
        missing.remove(index);
        assert!(
            observe(&encode(&missing), OBJECT_NAMED, 128).is_err(),
            "missing {index}"
        );
    }
    let bytes = encode(&valid);
    assert!(observe(&bytes[..bytes.len() - 1], OBJECT_NAMED, 128).is_err());
    assert!(observe(&bytes, OBJECT_NAMED, 1).is_err());
    for (index, field, value) in [
        (0, "evt", aterm_json::json!("unknown")),
        (
            1,
            "argv",
            aterm_json::json!(["git", "cat-file", "-t", "another-object"]),
        ),
        (2, "sid", aterm_json::json!("unrelated")),
        (2, "name", aterm_json::json!("fetch")),
        (3, "code", aterm_json::json!(0)),
        (4, "code", aterm_json::json!(0)),
    ] {
        let mut changed = valid.clone();
        changed[index]
            .as_object_mut()
            .unwrap()
            .insert(field.into(), value);
        assert!(
            observe(&encode(&changed), OBJECT_NAMED, 128).is_err(),
            "{index}.{field}"
        );
    }
    let mut duplicate = valid.clone();
    duplicate.insert(2, valid[1].clone());
    assert!(observe(&encode(&duplicate), OBJECT_NAMED, 128).is_err());
    let mut late = valid.clone();
    late.push(aterm_json::json!({"event":"child_start","sid":"root"}));
    assert!(observe(&encode(&late), OBJECT_NAMED, 128).is_err());
}
