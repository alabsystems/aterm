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

/// A judgment's output keeps everything but its event lines, whatever pieces the writes come
/// in: a line that begins like an event and turns out not to be one passes whole, and a
/// partial line still held when the writer drops is written then.
/// NEGATIVE CONTROL: the same lines, each with one byte of its event prefix changed, pass the
/// same filter whole, in the same pieces.
#[test]
fn a_run_kept_off_the_console_drops_only_its_event_lines() {
    let text = "verify: fmt ok\n\
                verify-event: {\"event\":\"check-finished\"}\n\
                verify-events are not this\n\
                \n\
                verify-event: {\"event\":\"run-finished\"}\n\
                verify: 1 passed\n\
                verif";
    let want = "verify: fmt ok\nverify-events are not this\n\nverify: 1 passed\nverif";
    let filtered = |text: &str, piece: usize| {
        let mut out = Vec::new();
        {
            let mut filtered = WithoutEvents::new(&mut out);
            for chunk in text.as_bytes().chunks(piece) {
                filtered.write_all(chunk).unwrap();
            }
        }
        String::from_utf8(out).unwrap()
    };
    let near = text.replace("verify-event: ", "verify-event; ");
    for piece in [1, 2, 3, 7, 14, 15, text.len()] {
        assert_eq!(filtered(text, piece), want, "pieces of {piece}");
        assert_eq!(filtered(&near, piece), near, "pieces of {piece}");
    }
}

/// A run's reds are the failed cases its events name, each with its finding's id and the line it
/// starts on, else the line its panic names; a failed check's reds are its findings, less the NOT
/// RUN rows folded among them. NEGATIVE CONTROL: a passed binary and a check that passed name
/// no red.
#[test]
fn a_judgments_reds_name_each_failed_case_its_id_and_where() {
    let case = |extra: &str| {
        format!(
            "verify-event: {{\"schema\":1,\"event\":\"test-case-failed\",\"outcome\":\"FAIL\",\
             \"test\":\"-p a --lib\",\"case\":\"tests::adds\"{extra}}}"
        )
    };
    let declared = Red::of_event(&case(
        ",\"source_path\":\"crates/a/src/lib_tests.rs\",\"start_line\":12",
    ));
    assert_eq!(
        declared,
        [Red {
            what: "-p a --lib: tests::adds".into(),
            id: Some("-p a --lib -- tests::adds".into()),
            at: Some("crates/a/src/lib_tests.rs:12".into()),
        }]
    );
    let panicked = Red::of_event(&case(
        ",\"output\":\"thread 'tests::adds' panicked at crates/a/src/x.rs:40:9:\\nboom\"",
    ));
    assert_eq!(panicked[0].at.as_deref(), Some("crates/a/src/x.rs:40"));
    let check = Red::of_event(
        "verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"test\",\
         \"outcome\":\"FAIL\",\"findings\":[\"-p a --lib -- t\",\"NOT RUN -p z --lib — stopped\"]}",
    );
    assert_eq!(
        check,
        [Red {
            what: "test: -p a --lib -- t".into(),
            id: Some("-p a --lib -- t".into()),
            at: None,
        }]
    );
    for passed in [
        "verify-event: {\"schema\":1,\"event\":\"test-binary-finished\",\"outcome\":\"ok\",\
         \"test\":\"-p a --lib\"}",
        "verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"lint\",\
         \"outcome\":\"ok\",\"findings\":[]}",
    ] {
        assert!(Red::of_event(passed).is_empty(), "{passed}");
    }
    assert_eq!(panicked_at("no panic here"), None);
    assert_eq!(
        panicked_at("panicked at src/a.rs:7:"),
        Some("src/a.rs:7".to_owned())
    );
}

/// A judgment's record names where its run kept its log and events, its reds in the order the
/// events came, each check's time, and whether the stop kept it from running.
#[test]
fn a_judgments_record_names_its_log_its_reds_and_what_the_halt_stopped() {
    let text = "verify-event: {\"schema\":1,\"event\":\"invocation-started\",\
                \"log\":\"/logs/run.log\",\"events\":\"/logs/run.log.events\"}\n\
                verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"lint\",\
                \"outcome\":\"FAIL\",\"seconds\":12.5,\"findings\":[\"-p a: x\"],\
                \"findings_total\":1,\"stopped\":false}\n\
                verify-event: {\"schema\":1,\"event\":\"check-finished\",\"check\":\"test\",\
                \"outcome\":\"NOT RUN\",\"seconds\":0.0,\"findings\":[],\"findings_total\":0,\
                \"stopped\":true}\n";
    let judged = Judged::from_events(text, crate::exit::FAILED, 13);
    assert_eq!(
        judged.kept,
        Some(Kept {
            log: PathBuf::from("/logs/run.log"),
            events: Some(PathBuf::from("/logs/run.log.events")),
        })
    );
    assert_eq!(
        judged
            .reds
            .iter()
            .map(|red| red.what.as_str())
            .collect::<Vec<_>>(),
        ["lint: -p a: x"]
    );
    assert_eq!(judged.checks[0].took, Some(Duration::from_millis(12_500)));
    assert_eq!(
        judged
            .checks
            .iter()
            .map(|row| row.stopped)
            .collect::<Vec<_>>(),
        [false, true]
    );
    // A run that keeps no log names none.
    let unkept = Judged::from_events(
        "verify-event: {\"schema\":1,\"event\":\"invocation-started\"}\n",
        0,
        0,
    );
    assert_eq!(unkept.kept, None);
}
