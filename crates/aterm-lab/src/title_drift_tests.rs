// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn session(sid: &str, window: &str, active: bool) -> Session {
    Session {
        local: u64::from(sid.as_bytes().last().copied().unwrap_or(b'a') - b'a'),
        sid: sid.into(),
        window: window.into(),
        title: "cat".into(),
        active,
        program: "cat".into(),
        focused: window == "1",
    }
}

#[test]
fn native_focus_requires_the_actual_typing_window_and_all_native_facts() {
    let valid = "OK available=1 window=27 app_active=1 window_key=1 key=1 posted_keys=8";
    assert!(native_focus_ok(valid, "27"));
    assert!(!native_focus_ok(valid, "28"));
    for fact in ["available", "app_active", "window_key", "key"] {
        let fault = valid.replace(&format!("{fact}=1"), &format!("{fact}=0"));
        assert!(!native_focus_ok(&fault, "27"), "{fact}");
    }
    assert!(!native_focus_ok("OK posted=500 key=1", "27"));
}

#[test]
fn fixture_focus_waits_for_observed_completion_but_the_measurement_check_does_not() {
    use std::io::{BufRead, BufReader, Write};

    let sid = "t0000000001";
    let typing = Session {
        local: 0,
        sid: sid.into(),
        window: "0".into(),
        title: "cat".into(),
        active: true,
        program: "cat".into(),
        focused: true,
    };
    let roster = |focused| {
        format!("OK 1\n0 {sid} 0 alive cat window=0 active=1 wfocus={focused} program=cat\n")
    };
    let native =
        |key| format!("OK available=1 window=27 app_active=1 window_key={key} key={key}\n");
    let replies = [
        ("sessions".into(), roster(0)),
        (format!("raise {sid}"), "OK raising\n".into()),
        ("sessions".into(), roster(0)),
        ("sessions".into(), roster(1)),
        ("native-focus".into(), native(0)),
        ("sessions".into(), roster(1)),
        ("native-focus".into(), native(1)),
        ("sessions".into(), roster(1)),
        ("sessions".into(), roster(0)),
        ("protocol-end".into(), "OK done\n".into()),
    ];
    let (ours, peer) = aterm_uds::CtlStream::pair().unwrap();
    peer.set_read_timeout(Some(HANG)).unwrap();
    peer.set_write_timeout(Some(HANG)).unwrap();
    let server = std::thread::spawn(move || {
        let mut peer = BufReader::new(peer);
        for (wanted, reply) in replies {
            let mut request = String::new();
            assert_ne!(peer.read_line(&mut request).unwrap(), 0);
            assert_eq!(request.trim_end(), wanted);
            peer.get_mut().write_all(reply.as_bytes()).unwrap();
        }
    });
    let mut wire = aterm_agent::RelayClient::new(ours).bounded(HANG);
    assert!(focus(&mut wire, &typing, None).is_err());
    assert!(native_focus_ok(
        &raise_for_setup(&mut wire, &typing).unwrap(),
        "27"
    ));
    assert!(focus(&mut wire, &typing, Some("27")).is_err());
    assert_eq!(ask(&mut wire, "protocol-end").unwrap(), "OK done");
    server.join().unwrap();
}

#[test]
fn successful_posts_do_not_substitute_for_complete_input_booking() {
    let reply = "OK n_input=500 input_p50_ms=2 input_p95_ms=4 input_p99_ms=8 \
                 n_key_queue=500 key_queue_unavailable=0 input_edges_dropped=0 input_predicted=0";
    assert!(metrics(reply, 500).is_ok());
    for fault in [
        reply.replace("n_input=500", "n_input=499"),
        reply.replace("n_input=500", "n_input=501"),
        reply.replace("input_p95_ms=4", "input_p95_ms=NaN"),
        reply.replace("input_p99_ms=8", "input_p99_ms=1"),
    ] {
        assert!(metrics(&fault, 500).is_err());
    }
}

#[test]
fn complete_dispatch_histograms_do_not_substitute_for_native_clock_evidence() {
    let reply = "OK n_input=500 input_p50_ms=2 input_p95_ms=4 input_p99_ms=8 \
                 n_key_queue=500 key_queue_unavailable=0 input_edges_dropped=0 input_predicted=0";
    assert!(metrics(reply, 500).is_ok());
    for (field, value, fault) in [
        ("n_key_queue", "500", "499"),
        ("n_key_queue", "500", "501"),
        ("key_queue_unavailable", "0", "1"),
        ("input_edges_dropped", "0", "1"),
        ("input_predicted", "0", "1"),
    ] {
        let original = format!("{field}={value}");
        assert!(
            metrics(&reply.replace(&original, &format!("{field}={fault}")), 500).is_err(),
            "{field}"
        );
        assert!(
            metrics(&reply.replace(&original, ""), 500).is_err(),
            "{field}"
        );
    }
    let dispatch_only = "OK n_input=500 input_p50_ms=2 input_p95_ms=4 input_p99_ms=8 \
                         n_key_queue=0 key_queue_unavailable=500 input_edges_dropped=0 input_predicted=0";
    assert!(metrics(dispatch_only, 500).is_err());
}

#[test]
fn topology_comparison_tracks_identity_placement_title_and_selection() {
    let source = session("s-a", "0", true);
    let typing = session("s-b", "1", true);
    let parked = session("s-c", "1", false);
    let before = [source.clone(), typing.clone(), parked.clone()];
    assert!(validate_roster(&before, &[parked, typing.clone(), source], &typing).is_ok());
    for fault in ["identity", "placement", "title", "selection", "missing"] {
        let mut after = before.to_vec();
        match fault {
            "identity" => after[2].sid = "s-d".into(),
            "placement" => after[2].window = "0".into(),
            "title" => after[2].title = "changed".into(),
            "selection" => after[1].active = false,
            "missing" => {
                after.pop();
            }
            _ => unreachable!(),
        }
        assert!(
            validate_roster(&before, &after, &typing).is_err(),
            "{fault}"
        );
    }
}

const TWO_WINDOWS: &str = "app-inspection version=app/v1 revision=1 subject=tabs
window id=0 focused=false tabs=1 active=1 rows=32 cols=110
  tab id=1 index=1 kind=terminal title=\"source\" state=clean active=true parked=false
    leaf path=/ view=1 rect_px=0,0,100,100 focused=true kind=terminal session=0
window id=1 focused=true tabs=2 active=2 rows=32 cols=110
  tab id=2 index=1 kind=terminal title=\"typing\" state=clean active=true parked=false
    leaf path=/ view=2 rect_px=0,0,100,100 focused=true kind=terminal session=1
  tab id=3 index=2 kind=terminal title=\"idle\" state=clean active=false parked=false
    leaf path=/ view=3 rect_px=0,0,100,100 focused=true kind=terminal session=2";

#[test]
fn matching_session_counts_do_not_prove_the_requested_tabs_or_leaf_placement() {
    let source = session("s-a", "0", true);
    let typing = session("s-b", "1", true);
    let idle = session("s-c", "1", false);
    let rows = [source.clone(), typing.clone(), idle];
    assert!(topology(TWO_WINDOWS, Case::Tabs(2), &rows, &source, &typing).is_ok());
    let one_split = "window id=0 focused=false tabs=1 active=1
  tab id=1 index=1 kind=terminal title=\"source\" state=clean active=true parked=false
    leaf path=/ view=1 focused=true kind=terminal session=0
window id=1 focused=true tabs=1 active=2
  tab id=2 index=1 kind=mixed title=\"typing and idle\" state=clean active=true parked=false
    split path=/ axis=horizontal
      leaf path=/0 view=2 focused=true kind=terminal session=1
      leaf path=/1 view=3 focused=false kind=terminal session=2";
    assert!(
        validate_roster(&rows, &rows, &typing).is_ok(),
        "the old roster-only check cannot see the wrong tab distribution"
    );
    assert!(topology(one_split, Case::Tabs(2), &rows, &source, &typing).is_err());
    for invalid in [
        TWO_WINDOWS.replace("view=3", "view=2"),
        TWO_WINDOWS.replace(
            "focused=true kind=terminal session=1",
            "focused=false kind=terminal session=1",
        ),
        TWO_WINDOWS.replace("session=2", "session=0"),
        TWO_WINDOWS.replace("kind=terminal session=2", "kind=app app=settings"),
        format!(
            "{TWO_WINDOWS}\n  tab id=4 index=3 kind=app title=\"Settings\" state=clean active=false parked=false\n    leaf path=/ view=4 focused=true kind=app app=settings"
        ),
    ] {
        assert!(topology(&invalid, Case::Tabs(2), &rows, &source, &typing).is_err());
    }
}

#[test]
fn split_control_requires_one_tab_with_exact_source_and_focused_typing_leaves() {
    let source = session("s-a", "1", false);
    let typing = session("s-b", "1", true);
    let rows = [source.clone(), typing.clone()];
    let split = "window id=1 focused=true tabs=1 active=1
  tab id=1 index=1 kind=mixed title=\"cat\" state=clean active=true parked=false
    split path=/ axis=horizontal
      leaf path=/0 view=1 focused=false kind=terminal session=0
      leaf path=/1 view=2 focused=true kind=terminal session=1";
    assert!(topology(split, Case::Split, &rows, &source, &typing).is_ok());
    assert!(
        topology(
            &split.replace("axis=horizontal", "axis=vertical"),
            Case::Split,
            &rows,
            &source,
            &typing
        )
        .is_err()
    );
    assert!(
        topology(
            &split.replace("session=1", "session=0"),
            Case::Split,
            &rows,
            &source,
            &typing
        )
        .is_err()
    );
    assert!(
        topology(
            &split.replace("path=/1 view=2", "path=/0 view=2"),
            Case::Split,
            &rows,
            &source,
            &typing
        )
        .is_err()
    );
    assert!(
        topology(
            &split.replace(
                "focused=true kind=terminal session=1",
                "focused=false kind=terminal session=1"
            ),
            Case::Split,
            &rows,
            &source,
            &typing
        )
        .is_err()
    );
}

fn snapshot_fixture() -> Snapshot {
    Snapshot {
        status: "OK rows=32 cols=110 render=ok resizes=1 flaps=0".into(),
        dims: "OK 32 110 880 512 window=1 window_rows=32 window_cols=110".into(),
        resizes: "OK 1 total=1 runs=1 flaps=0 risks=0 render=ok\nresize 1 from=110x32 to=110x32"
            .into(),
        drift: "OK 0 verdict=clean fidelity=pass".into(),
        title: "OK \n".into(),
    }
}

#[test]
fn unchanged_grid_does_not_hide_a_resize_excursion_or_unfaithful_replay() {
    let before = snapshot_fixture();
    assert!(stable_snapshot(&before, &before).is_ok());
    let mut excursion = before.clone();
    excursion
        .resizes
        .push_str("\nresize 2 from=110x32 to=80x24\nresize 3 from=80x24 to=110x32");
    assert!(stable_snapshot(&before, &excursion).is_err());
    let mut unfaithful = before.clone();
    unfaithful.drift = "OK 0 verdict=clean fidelity=fail".into();
    assert!(stable_snapshot(&before, &unfaithful).is_err());
    let mut lost_title = before.clone();
    lost_title.title = "OK new title\n".into();
    assert!(stable_snapshot(&before, &lost_title).is_err());
}

#[test]
fn default_workload_is_fixed_and_quick_workload_keeps_the_same_judgement() {
    let work = Work {
        profile: "agent".into(),
        duration_ms: 20000,
        typing_ms: 16000,
        block_bytes: 256,
        output_ms: 20,
        key_ms: 40,
    };
    assert!(work.validate().is_ok());
    assert_eq!((work.blocks(), work.keys()), (1000, 400));
    let (plain, final_block) = work.payloads();
    assert_eq!(plain.len(), 256);
    assert_eq!(final_block.len(), 256);
    assert!(std::str::from_utf8(&final_block).unwrap().contains(MARKER));
    let quick = Work {
        duration_ms: 400,
        typing_ms: 320,
        ..work.clone()
    };
    assert!(quick.validate().is_ok());
    assert_eq!((quick.blocks(), quick.keys()), (20, 8));
    assert!(
        Work {
            key_ms: 0,
            ..work.clone()
        }
        .validate()
        .is_err()
    );
    assert!(
        Work {
            duration_ms: 401,
            ..work
        }
        .validate()
        .is_err()
    );
}

#[test]
fn producer_receipts_refuse_missing_bytes_catch_up_bursts_and_incomplete_intervals() {
    let work = Work {
        profile: "agent".into(),
        duration_ms: 80,
        typing_ms: 40,
        block_bytes: 256,
        output_ms: 20,
        key_ms: 40,
    };
    let valid = aterm_json::json!({
        "version": 1, "blocks": 4, "bytes": 1024, "elapsed_ns": 80_000_000,
        "clock": "host_CLOCK_MONOTONIC_ns", "start_monotonic_ns": 1_000_000_000,
        "end_monotonic_ns": 1_080_000_000,
        "sent_ns": [1_000_000_000, 1_020_000_000, 1_040_000_000, 1_060_000_000],
    });
    assert!(producer::validate(&valid.to_string(), &work).is_ok());
    for (key, value) in [
        ("bytes", aterm_json::json!(1023)),
        ("blocks", aterm_json::json!(3)),
        ("elapsed_ns", aterm_json::json!(60_000_000)),
        (
            "sent_ns",
            aterm_json::json!([1_000_000_000, 1_020_000_000, 1_020_000_001, 1_060_000_000]),
        ),
        (
            "sent_ns",
            aterm_json::json!([1_000_000_000, 1_020_000_000, 1_040_000_000, 1_090_000_000]),
        ),
    ] {
        let mut fault = valid.clone();
        fault.as_object_mut().unwrap().insert(key.into(), value);
        assert!(
            producer::validate(&fault.to_string(), &work).is_err(),
            "{key}"
        );
    }
}

#[test]
fn both_finished_does_not_substitute_for_typing_entirely_during_output() {
    assert!(enclosed(100, 500, 150, 450).is_ok());
    assert!(enclosed(100, 500, 100, 500).is_ok());
    for (first, last, start, end) in [
        (100, 500, 50, 450),
        (100, 500, 150, 550),
        (600, 900, 150, 450),
        (0, 500, 150, 450),
    ] {
        assert!(enclosed(first, last, start, end).is_err());
    }
}

#[test]
fn a_debug_seam_feature_cannot_stand_in_for_a_debug_compiler_record() {
    let digest = "ab".repeat(32);
    let valid = aterm_json::json!({
        "reason": "compiler-artifact", "target": {"name": "aterm", "kind": ["bin"]},
        "trust_executable_sha256": digest, "trust_compile_mode": "build", "trust_compile_kind": "host",
        "profile": {"debug_assertions": true, "test": false}, "features": [],
    });
    assert!(build_record(&valid.to_string(), &digest).is_ok());
    assert!(build_record(&valid.to_string(), &"cd".repeat(32)).is_err());
    let mut release = valid.clone();
    release.as_object_mut().unwrap().insert(
        "profile".into(),
        aterm_json::json!({"debug_assertions": false, "test": false}),
    );
    release
        .as_object_mut()
        .unwrap()
        .insert("features".into(), aterm_json::json!(["dev-seams"]));
    assert!(build_record(&release.to_string(), &digest).is_err());
    let mut test = valid;
    test.as_object_mut().unwrap().insert(
        "profile".into(),
        aterm_json::json!({"debug_assertions": true, "test": true}),
    );
    assert!(build_record(&test.to_string(), &digest).is_err());
}

#[test]
fn cast_coverage_and_distribution_reject_deferred_or_missing_output() {
    let work = Work {
        profile: "agent".into(),
        duration_ms: 80,
        typing_ms: 40,
        block_bytes: 256,
        output_ms: 20,
        key_ms: 40,
    };
    let (block, last) = work.payloads();
    let header = aterm_json::json!({"version": 2, "width": 110, "height": 32});
    let cast = |header: &aterm_json::Value, rows: &[(f64, &[u8])]| {
        let mut text = format!("{header}\n");
        for (at, bytes) in rows {
            let event = aterm_json::json!([at, "o", std::str::from_utf8(bytes).unwrap()]);
            text.push_str(&format!("{event}\n"));
        }
        text
    };
    let complete = cast(
        &header,
        &[(0.0, &block), (0.02, &block), (0.04, &block), (0.06, &last)],
    );
    let evidence = cast_evidence(&complete, &work).unwrap();
    assert_eq!(evidence.bytes, 1024);
    assert!(!evidence.truncated);
    let all = [&block[..], &block, &block, &last].concat();
    assert!(cast_evidence(&cast(&header, &[(0.0, &all)]), &work).is_err());
    assert!(cast_evidence(&cast(&header, &[(0.0, &block), (0.06, &last)]), &work).is_err());
    let truncated = aterm_json::json!({"version": 2, "width": 110, "height": 32, "aterm_truncated": {"evicted_events": 2}});
    let tail = cast_evidence(&cast(&truncated, &[(0.0, &block), (0.02, &last)]), &work).unwrap();
    assert_eq!(tail.bytes, 512);
    assert!(tail.truncated);
}

#[test]
fn an_unanswered_control_request_reaches_cleanup_and_keeps_no_result() {
    let (ours, _unanswering_peer) = aterm_uds::CtlStream::pair().unwrap();
    // An intentionally silent peer, not a speed requirement for an answering one.
    let mut wire = aterm_agent::RelayClient::new(ours).bounded(Duration::ZERO);
    let result = ask(&mut wire, "status");
    assert!(result.is_err());
    let mut cleaned = false;
    let result = finish(
        result,
        || {
            cleaned = true;
            Ok(())
        },
        &mut Raw::default(),
    );
    assert!(cleaned);
    assert!(result.is_err());
    assert!(ask(&mut wire, "status").is_err());
}

#[test]
fn setup_waits_for_actual_producer_metadata_before_freezing_the_roster() {
    let source = session("s-a", "0", true);
    let typing = session("s-b", "1", true);
    let idle = session("s-c", "1", false);
    let expected = vec![source.clone(), typing.clone(), idle];
    let mut current = expected.clone();
    // A ready-file receipt does not mean the roster has observed the exec yet.
    current[0].program = "sh".into();
    let producer = "frozen-measure-driver";
    assert!(!setup_roster_ready(
        &expected, &current, &source, &typing, producer
    ));
    current[0].program = producer.into();
    assert!(setup_roster_ready(
        &expected, &current, &source, &typing, producer
    ));
    for fault in [
        "missing", "sid", "local", "window", "producer", "idle", "typing",
    ] {
        let mut bad = current.clone();
        match fault {
            "missing" => {
                bad.pop();
            }
            "sid" => bad[0].sid = "s-other".into(),
            "local" => bad[0].local += 1,
            "window" => bad[0].window = "1".into(),
            "producer" => bad[0].program = "sh".into(),
            "idle" => bad[2].program = "sh".into(),
            "typing" => bad[1].active = false,
            _ => unreachable!(),
        }
        assert!(
            !setup_roster_ready(&expected, &bad, &source, &typing, producer),
            "{fault}"
        );
    }
    // Roster ordering is immaterial; all exact identities and programs still match.
    current.reverse();
    assert!(setup_roster_ready(
        &expected, &current, &source, &typing, producer
    ));
}

/// Independent wire receipts for the cohosted workload and unrelated 30-tab windows.
fn real_window_receipt(windows: usize) -> (String, Vec<Session>) {
    let mut text = String::new();
    let mut rows = Vec::new();
    let mut session_id = 0;
    for window in 0..windows {
        let tabs = if window == 0 { 2 } else { 30 };
        let active = if window == 0 { 1 } else { 0 };
        let first = session_id;
        writeln!(
            text,
            "window id={window} focused={} tabs={tabs} active={}",
            window == 0,
            first + active + 1
        )
        .unwrap();
        for index in 0..tabs {
            let chosen = index == active;
            writeln!(text, "  tab id={} index={} kind=terminal title=\"cat\" state=clean active={chosen} parked=false\n    leaf path=/ view={} focused=true kind=terminal session={session_id}", session_id + 1, index + 1, session_id + 1).unwrap();
            rows.push(Session {
                local: session_id as u64,
                sid: format!("session-{session_id}"),
                window: window.to_string(),
                title: "cat".into(),
                active: chosen,
                program: "cat".into(),
                focused: window == 0,
            });
            session_id += 1;
        }
    }
    (text, rows)
}

#[test]
fn real_window_cases_refuse_tab_counts_as_window_evidence_and_wrong_workload_placement() {
    for (name, count) in [("windows1", 1), ("windows2", 2), ("windows8", 8)] {
        let case = Case::parse(name).unwrap();
        let (text, rows) = real_window_receipt(count);
        let check = |text: &str, rows: &[Session]| topology(text, case, rows, &rows[0], &rows[1]);
        assert!(check(&text, &rows).is_ok(), "{name}");
        assert_eq!(case.receipt()["os_windows"].as_u64(), Some(count as u64));
        assert_eq!(
            case.receipt()["terminal_sessions"].as_u64(),
            Some(rows.len() as u64)
        );
        assert!(topology(&text, Case::Tabs(30), &rows, &rows[0], &rows[1]).is_err());
        assert!(check(&text.replace("tabs=2", "tabs=1"), &rows).is_err());
        assert!(check(&text.replace("active=2", "active=1"), &rows).is_err());
        assert!(check(&text.replace("view=2 ", "view=1 "), &rows).is_err());
        if count > 1 {
            let (one, one_rows) = real_window_receipt(1);
            assert!(
                check(&one, &one_rows).is_err(),
                "one logical window is not many OS windows"
            );
            assert!(check(&text.replace("tabs=30", "tabs=29"), &rows).is_err());
            let mut moved = rows.clone();
            moved[2].window = "0".into();
            assert!(check(&text, &moved).is_err());
        }
        let binding = case.receipt().to_string();
        assert_ne!(binding, Case::Tabs(30).receipt().to_string());
    }
    for bad in ["windows0", "windows30", "windows200", "window1"] {
        assert!(Case::parse(bad).is_err());
    }
}

#[test]
fn every_logical_window_needs_its_own_native_identity_not_a_repeated_focus_receipt() {
    let (_, rows) = real_window_receipt(8);
    let good: BTreeMap<_, _> = (0..8)
        .map(|n| (n.to_string(), (100 + n).to_string()))
        .collect();
    validate_native_windows(&rows, &good).unwrap();
    let mut bad = good.clone();
    bad.insert("7".into(), "100".into());
    assert!(validate_native_windows(&rows, &bad).is_err());
    for value in ["0", "unknown", "00100"] {
        let mut bad = good.clone();
        bad.insert("7".into(), value.into());
        assert!(validate_native_windows(&rows, &bad).is_err());
    }
    let mut bad = good.clone();
    bad.remove("7");
    assert!(validate_native_windows(&rows, &bad).is_err());
    let mut bad = good;
    bad.insert("extra".into(), "108".into());
    assert!(validate_native_windows(&rows, &bad).is_err());
}

#[test]
fn global_present_diagnostics_preserve_counts_without_claiming_hidden_source_latency() {
    let reply = "OK n_present=17 present_p50_ms=1 present_p95_ms=2 present_p99_ms=3";
    assert_eq!(present_metrics(reply).unwrap().n, 17);
    for bad in [
        reply.replace("n_present=17", ""),
        reply.replace("present_p95_ms=2", "present_p95_ms=NaN"),
        reply.replace("present_p99_ms=3", "present_p99_ms=0"),
    ] {
        assert!(present_metrics(&bad).is_err());
    }
}

#[test]
fn production_session_placement_creates_real_new_windows_and_keeps_the_workload_cohosted() {
    use std::io::{BufRead, BufReader, Write};
    for name in ["windows1", "windows2", "windows8"] {
        let case = Case::parse(name).unwrap();
        let (ours, peer) = aterm_uds::CtlStream::pair().unwrap();
        peer.set_read_timeout(Some(HANG)).unwrap();
        peer.set_write_timeout(Some(HANG)).unwrap();
        let server = std::thread::spawn(move || {
            let mut peer = BufReader::new(peer);
            // Model the real protocol distinction: spawn adds a tab to its named
            // existing window; NewWindow creates a distinct window with one tab.
            let mut windows = vec![vec![0]];
            let mut next = 1;
            loop {
                let mut request = String::new();
                assert_ne!(peer.read_line(&mut request).unwrap(), 0);
                let request = request.trim_end();
                let reply = if request == "sessions" {
                    let mut reply = format!("OK {next}\n");
                    for (window, sessions) in windows.iter().enumerate() {
                        for local in sessions {
                            writeln!(reply, "{local} {} 0 alive cat window={window} active={} wfocus={} program=cat", aterm_types::tab_id::TabId::from_bits(*local as u64), usize::from(Some(local) == sessions.last()), usize::from(window == 0)).unwrap();
                        }
                    }
                    reply
                } else if request == "invoke NewWindow" {
                    windows.push(vec![next]);
                    next += 1;
                    "OK created\n".into()
                } else if let Some(target) = request.strip_prefix("spawn window=") {
                    let target: usize = target
                        .strip_suffix(" raise=false")
                        .unwrap()
                        .parse()
                        .unwrap();
                    windows[target].push(next);
                    next += 1;
                    "OK spawned\n".into()
                } else {
                    assert_eq!(request, "protocol-end");
                    peer.get_mut().write_all(b"OK done\n").unwrap();
                    return windows;
                };
                peer.get_mut().write_all(reply.as_bytes()).unwrap();
            }
        });
        let mut wire = aterm_agent::RelayClient::new(ours).bounded(HANG);
        let (source, typing, rows) = place_sessions(&mut wire, case).unwrap();
        assert_ne!(source.sid, typing.sid);
        assert_eq!(source.window, typing.window);
        assert_eq!(source.window, "0");
        assert_eq!(
            rows.len() as u64,
            case.receipt()["terminal_sessions"].as_u64().unwrap()
        );
        ask(&mut wire, "protocol-end").unwrap();
        let observed = server.join().unwrap();
        assert_eq!(
            observed.len() as u64,
            case.receipt()["os_windows"].as_u64().unwrap()
        );
        assert_eq!(observed[0], [0, 1]);
        assert!(observed.iter().skip(1).all(|tabs| tabs.len() == 30));
    }
}
