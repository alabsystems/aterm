// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn actual_row_producer_and_event_writer_share_the_observation_contract() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.events");
    let log = EventLog::open(&path).unwrap();
    log.write(&crate::report::row_event("test", "sample", None));
    log.write(&crate::report::row_event(
        "test",
        "sample",
        Some((Duration::from_secs(1), None)),
    ));
    let text = std::fs::read_to_string(&path).unwrap();
    let mut kinds = Vec::new();
    for line in text.lines() {
        let Observation::Known { kind, value } =
            observation(line.strip_prefix("verify-event: ").unwrap())
        else {
            panic!("actual producer/writer record was not recognized");
        };
        assert_eq!(value["row"].as_str(), Some("sample"));
        assert!(!kind.projects_failure());
        kinds.push(kind);
    }
    assert_eq!(kinds, [EventKind::RowStarted, EventKind::RowFinished]);
}

#[test]
fn malformed_producer_payload_is_explicit_loss_not_an_invented_check() {
    for payload in [
        aterm_json::Value::Null,
        aterm_json::json!({"event":"check-finished","outcome":"FAIL"}),
    ] {
        let value = event_value(EventKind::CheckFinished, payload);
        let Observation::Known { kind, value } = observation(&value.to_string()) else {
            panic!("producer loss envelope was not recognized");
        };
        assert_eq!(kind, EventKind::ObservationUnavailable);
        assert!(!kind.projects_failure());
        assert!(value["outcome"].is_null());
    }
}

#[test]
fn private_storage_markers_are_not_execution_observations() {
    for marker in [
        crate::run_log::PRIVATE_EVENTS_HEADER,
        crate::run_log::PRIVATE_HUMAN_HEADER,
    ] {
        let json = marker
            .strip_prefix("verify-event: ")
            .unwrap()
            .trim_end_matches('\n');
        assert!(matches!(observation(json), Observation::StorageMarker));
        assert!(matches!(
            observation(&json.replace("storage", "unknown-storage")),
            Observation::UnknownKind
        ));
    }
}

/// Lines from many threads, each far longer than a pipe writes at once, land whole and one
/// per line, a missing newline added and a present one kept single.
#[test]
fn each_line_lands_whole_whatever_writes_beside_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let at = dir.path().join("06-verify.events");
    let log = EventLog::open(&at).unwrap();
    let long = |t: usize, i: usize| {
        format!(
            "verify-event: {{\"t\":{t},\"i\":{i},\"pad\":\"{}\"}}",
            "x".repeat(20_000)
        )
    };
    std::thread::scope(|s| {
        for t in 0..4 {
            let log = &log;
            s.spawn(move || {
                for i in 0..20 {
                    let line = long(t, i);
                    log.write(&if i % 2 == 0 {
                        line
                    } else {
                        format!("{line}\n")
                    });
                }
            });
        }
    });
    let text = std::fs::read_to_string(&at).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 80);
    for line in lines {
        let json = line.strip_prefix("verify-event: ").expect("a whole line");
        let event: aterm_json::Value = aterm_json::from_str(json).unwrap();
        assert_eq!(event["pad"].as_str().map(str::len), Some(20_000));
    }
}
