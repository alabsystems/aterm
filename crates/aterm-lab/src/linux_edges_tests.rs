// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn with_run(guest: &crate::linux_driver::fake::Guest, results: &Path, test: impl FnOnce(&Run<'_>)) {
    let up = linux::fake_up(guest, false);
    let cell = Cell {
        up: &up,
        arch: guest::Arch::X86_64,
        tag: "x86_64".into(),
        repo: "/repo".into(),
        env: Vec::new(),
        bin: "/b".into(),
        results,
    };
    let row = Run {
        cell: &cell,
        gui: Gui {
            up: &up,
            bin: "/b".into(),
            dir: "/d".into(),
            name: "x86_64-edge-padding-0".into(),
            cpu: true,
            deck: false,
        },
        lane_wl: Ok("/b/lane-wl"),
        invocation: RefCell::new(None),
    };
    test(&row);
}

#[test]
fn failed_edges_retain_the_original_launch_evidence_before_stopping_its_unit() {
    use crate::linux_driver::fake::{Answer, Call, Guest};
    let id = "0123456789abcdef0123456789abcdef";
    let journal_id = format!("_SYSTEMD_INVOCATION_ID={id}");
    let guest = Guest::new()
        .answer(
            &["--property=InvocationID", "--value"],
            Answer::Exited(0, id.into()),
        )
        .answer(&["ready", "60000"], Answer::Exited(0, "OK ready".into()))
        .answer(
            &["await", "match"],
            Answer::Exited(1, "connect: Connection refused".into()),
        )
        .answer(
            &["cat", "/d/gui.status"],
            Answer::Exited(
                0,
                format!("schema=1\ninvocation={id}\ncode=dumped\nstatus=SEGV\nresult=core-dump\n"),
            ),
        )
        .answer(
            &["cat", "/d/gui.stderr"],
            Answer::Exited(0, "GUI's native failure\n".into()),
        )
        .answer(
            &["journalctl", &journal_id],
            Answer::Exited(0, "this launch's journal\n".into()),
        )
        .answer(
            &["find", "/d/home"],
            Answer::Exited(0, "/d/home/crash-1.log\n".into()),
        )
        .answer(
            &["cat", "/d/home/crash-1.log"],
            Answer::Exited(0, "original crash\n".into()),
        );
    let top = aterm_tempfile::tempdir().unwrap();
    with_run(&guest, top.path(), |row| {
        row.launch(
            &[("WAYLAND_DISPLAY".into(), "owned-display".into())],
            &[("WAYLAND_DEBUG".into(), "client".into())],
        )
        .unwrap();
        let original = row
            .ctl(&["await", "match", "LAST-ROW", "timeout=60000"])
            .unwrap_err();
        let (verdict, detail) = row.finish((Verdict::Fail, original.clone()));
        assert_eq!(verdict, Verdict::Fail);
        assert!(detail.starts_with(&original));
        let evidence =
            std::fs::read_to_string(top.path().join("x86_64-edge-padding-0-failure.txt")).unwrap();
        assert!(evidence.contains(id));
        assert!(evidence.contains("code=dumped status=SEGV result=core-dump"));
        assert!(evidence.contains("GUI's native failure"));
        assert!(evidence.contains("this launch's journal"));
        assert!(
            std::fs::read_to_string(top.path().join("x86_64-edge-padding-0-crash-1.log"))
                .unwrap()
                .contains("original crash")
        );
    });
    let calls = guest.calls();
    let stop = calls.iter().position(|call| matches!(call, Call::Shell { argv, .. } if argv.iter().any(|v| v == "systemctl") && argv.iter().any(|v| v == "stop"))).unwrap();
    for required in [
        "/d/gui.status",
        "/d/gui.stderr",
        &journal_id,
        "/d/home/crash-1.log",
    ] {
        assert!(calls.iter().take(stop).any(|call| matches!(call, Call::Shell { argv, .. } if argv.iter().any(|v| v == required))), "{required} was not retained before cleanup");
    }
    assert!(calls.iter().any(|call| matches!(call, Call::Shell { argv, .. } if argv.iter().any(|v| v == "--setenv=WAYLAND_DEBUG=client") && argv.iter().any(|v| v == "--setenv=WAYLAND_DISPLAY=owned-display") && argv.iter().any(|v| v == "--setenv=ATERM_LANE_EXIT_RECORD=/d/gui.status") && argv.iter().any(|v| v.starts_with("--property=ExecStopPost=")))));
}

#[test]
fn unavailable_or_replaced_unit_evidence_cannot_replace_the_edge_failure() {
    use crate::linux_driver::fake::{Answer, Call, Guest};
    for (id, state_code, state_id) in [
        ("", 0, "ffffffffffffffffffffffffffffffff"),
        ("malformed", 0, "ffffffffffffffffffffffffffffffff"),
        (
            "0123456789abcdef0123456789abcdef",
            0,
            "ffffffffffffffffffffffffffffffff",
        ),
        (
            "0123456789abcdef0123456789abcdef",
            1,
            "0123456789abcdef0123456789abcdef",
        ),
    ] {
        let guest = Guest::new()
            .answer(&["--property=InvocationID", "--value"], Answer::Exited(0, id.into()))
            .answer(&["cat", "/d/gui.status"], Answer::Exited(1, "status unavailable".into()))
            .answer(&["cat", "/d/gui.stderr"], Answer::Exited(1, "stderr unreadable".into()))
            .answer(&["--property=InvocationID,LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,ExecStopPost,MainPID"], Answer::Exited(state_code, format!("InvocationID={state_id}\nResult=success\n")))
            .answer(&["find"], Answer::Exited(1, "crash inventory unreadable".into()));
        let top = aterm_tempfile::tempdir().unwrap();
        with_run(&guest, top.path(), |row| {
            row.start(&[], &[]).unwrap();
            let (verdict, detail) = row.finish((Verdict::Fail, "original socket failure".into()));
            assert_eq!(verdict, Verdict::Fail);
            assert!(detail.starts_with("original socket failure"));
            let evidence =
                std::fs::read_to_string(top.path().join("x86_64-edge-padding-0-failure.txt"))
                    .unwrap();
            assert!(evidence.contains("status unavailable"));
            assert!(evidence.contains("stderr unreadable"));
            assert!(evidence.contains("different invocation"));
            assert!(evidence.contains("crash inventory incomplete"));
            let journals: Vec<_> = guest
                .calls()
                .into_iter()
                .filter_map(|call| match call {
                    Call::Shell { argv, .. } if argv.iter().any(|v| v == "journalctl") => {
                        Some(argv)
                    }
                    _ => None,
                })
                .collect();
            if id.len() == 32 {
                assert_eq!(journals.len(), 1);
                assert!(journals[0].contains(&format!("_SYSTEMD_INVOCATION_ID={id}")));
            } else {
                assert!(journals.is_empty());
                assert!(evidence.contains("no unit-name journal was substituted"));
            }
        });
    }
}

#[test]
fn main_exit_records_require_a_whole_matching_manager_record_without_hiding_unit_failure() {
    let id = "0123456789abcdef0123456789abcdef";
    let whole = format!("schema=1\ninvocation={id}\ncode=exited\nstatus=37\nresult=exit-code\n");
    assert_eq!(
        main_exit(&whole, id).unwrap(),
        "code=exited status=37 result=exit-code"
    );
    assert_eq!(
        main_exit(
            &whole.replace("37", "0").replace("exit-code", "success"),
            id
        )
        .unwrap(),
        "code=exited status=0 result=success"
    );
    for bad in [
        whole.trim_end().to_string(),
        whole.replace("schema=1", "schema=2"),
        whole.replace("code=exited", "code="),
        whole.replace("status=37", "status=037"),
        whole.replace("status=37", "status=256"),
        whole.replace("result=exit-code", "result="),
        whole.replace(id, "ffffffffffffffffffffffffffffffff"),
        format!("{whole}status=0\n"),
    ] {
        assert!(main_exit(&bad, id).is_err(), "{bad}");
    }
    assert!(main_exit(&whole, "").is_err());
    // The main's record and the hook/unit result are distinct observations: neither can
    // turn an existing socket failure into PASS, even when the main itself exited zero.
    use crate::linux_driver::fake::{Answer, Guest};
    let guest = Guest::new()
        .answer(&["--property=InvocationID", "--value"], Answer::Exited(0, id.into()))
        .answer(&["cat", "/d/gui.status"], Answer::Exited(0, whole.replace("status=37", "status=0").replace("result=exit-code", "result=success")))
        .answer(&["--property=InvocationID,LoadState,ActiveState,SubState,Result,ExecMainCode,ExecMainStatus,ExecStopPost,MainPID"], Answer::Exited(0, format!("InvocationID={id}\nResult=protocol\nExecMainStatus=0\n")));
    let top = aterm_tempfile::tempdir().unwrap();
    with_run(&guest, top.path(), |row| {
        row.start(&[], &[]).unwrap();
        let (verdict, detail) = row.finish((Verdict::Fail, "original socket failure".into()));
        assert_eq!(verdict, Verdict::Fail);
        assert!(detail.starts_with("original socket failure"));
        let text =
            std::fs::read_to_string(top.path().join("x86_64-edge-padding-0-failure.txt")).unwrap();
        assert!(text.contains("Result=protocol"));
        assert!(text.contains("code=exited status=0 result=success"));
    });
}

#[test]
fn failed_edge_logs_and_crash_inventory_are_bounded_and_report_truncation() {
    use crate::linux_driver::fake::{Answer, Guest};
    let long = format!("discarded-prefix{}retained-tail", "x".repeat(100_000));
    let files = (0..10)
        .map(|i| format!("/d/home/crash-{i}.log\n"))
        .collect::<String>();
    let guest = Guest::new()
        .answer(&["cat", "/d/gui.stderr"], Answer::Exited(0, long.clone()))
        .answer(&["find", "/d/home"], Answer::Exited(0, files))
        .answer(&["cat"], Answer::Exited(0, long));
    let top = aterm_tempfile::tempdir().unwrap();
    with_run(&guest, top.path(), |row| {
        row.start(&[], &[]).unwrap();
        let (verdict, _) = row.finish((Verdict::Fail, "original failure".into()));
        assert_eq!(verdict, Verdict::Fail);
        let evidence =
            std::fs::read_to_string(top.path().join("x86_64-edge-padding-0-failure.txt")).unwrap();
        assert!(evidence.len() < 140_000);
        assert!(evidence.contains("truncated:"));
        assert!(evidence.contains("retained-tail"));
        assert!(!evidence.contains("discarded-prefix"));
        assert!(evidence.contains("retaining at most eight"));
        for i in 0..8 {
            let crash = std::fs::read_to_string(
                top.path()
                    .join(format!("x86_64-edge-padding-0-crash-{i}.log")),
            )
            .unwrap();
            assert!(crash.len() < 66_000);
            assert!(crash.contains("truncated:"));
            assert!(crash.ends_with("retained-tail"));
        }
        assert!(
            !top.path()
                .join("x86_64-edge-padding-0-crash-8.log")
                .exists()
        );
    });
}

#[test]
fn every_row_has_a_name_it_is_found_by() {
    for e in Edge::ALL {
        assert_eq!(Edge::parse(e.slug()), Some(e));
    }
    assert_eq!(Edge::parse("nope"), None);
}

#[test]
fn a_window_is_found_in_swaymsg_tree_however_it_is_spaced() {
    let empty = r#"{ "id": 1, "name": "root", "type": "root", "nodes": [ { "id": 3, "name": "HEADLESS-1", "type": "output", "nodes": [ { "id": 4, "name": "1", "type": "workspace", "nodes": [ ], "floating_nodes": [ ] } ] } ] }"#;
    assert!(!sway_shows(empty, "aterm"), "an empty workspace");
    let pretty = "{\n  \"nodes\": [\n    {\n      \"type\": \"con\",\n      \"app_id\": \"aterm\",\n      \"pid\": 4242\n    }\n  ]\n}";
    assert!(sway_shows(pretty, "aterm"));
    assert!(sway_shows(
        r#"{"nodes":[{"app_id":"aterm","pid":1}]}"#,
        "aterm"
    ));
    assert!(!sway_shows(
        r#"{"nodes":[{"app_id":"aterm-other"}]}"#,
        "aterm"
    ));
}

#[test]
fn the_wire_log_is_read_for_shapes_requests_and_bindings() {
    let log = "\
[1.0]  -> wl_registry@2.bind(2, \"wl_compositor\", 5, new id [unknown]@3)
[1.1]  -> wp_cursor_shape_device_v1@33.set_shape(9, 1)
[1.2]  -> wp_cursor_shape_device_v1@33.set_shape(9, 9)
[1.3]  -> wp_cursor_shape_device_v1@33.set_shape(10, 9)
[1.4]  -> wp_cursor_shape_device_v1@33.set_shape(11, 18)
[1.5] wl_seat@8.capabilities(0)
[1.6]  -> wl_pointer@31.release()
[1.7]  -> wl_pointer@40.set_cursor(12, wl_surface@41, 3, 3)
[1.8] wl_registry@2.global_remove(46)
[1.9] wl_surface@18.preferred_buffer_scale(2)
";
    assert_eq!(shapes_set(log), ["default", "text", "e_resize"]);
    assert_eq!(requests_of(log, "wl_pointer", "release"), 1);
    assert_eq!(requests_of(log, "wl_pointer", "set_cursor"), 1);
    assert_eq!(requests_of(log, "wl_seat", "capabilities"), 0);
    assert_eq!(events_of(log, "wl_registry", "global_remove"), 1);
    assert_eq!(events_of(log, "wl_surface", "preferred_buffer_scale"), 1);
    assert_eq!(events_of(log, "wl_pointer", "release"), 0);
    assert_eq!(bound_version(log, "wl_compositor"), Some(5));
    assert_eq!(bound_version(log, "wl_shm"), None);
}

#[test]
fn the_seats_bound_are_read_with_their_globals_and_objects() {
    let log = "\
[2355583.982] wl_registry@2.global(42, \"wl_seat\", 8)
[2355584.025]  -> wl_registry@2.bind(42, \"wl_seat\", 7, new id [unknown]@9)
[2355584.030]  -> wl_registry@2.bind(43, \"wl_shm\", 1, new id [unknown]@10)
[2356443.875] wl_registry@2.global(46, \"wl_seat\", 8)
[2356443.961]  -> wl_registry@2.bind(46, \"wl_seat\", 7, new id [unknown]@33)
";
    assert_eq!(
        seat_binds(log),
        [
            (42, "wl_seat@9".to_string()),
            (46, "wl_seat@33".to_string())
        ]
    );
    assert!(seat_binds("[1.0] wl_registry@2.global(42, \"wl_seat\", 8)").is_empty());
}

#[test]
fn the_preferred_scale_is_the_last_one_sent_to_the_window_s_surface() {
    let log = "\
[156408.709]  -> wl_registry@2.bind(30, \"wp_fractional_scale_manager_v1\", 1, new id [unknown]@12)
[156415.590]  -> xdg_wm_base@6.get_xdg_surface(new id xdg_surface@20, wl_surface@19)
[156415.599]  -> wp_fractional_scale_manager_v1@12.get_fractional_scale(new id wp_fractional_scale_v1@24, wl_surface@19)
[156415.600]  -> wp_fractional_scale_manager_v1@12.get_fractional_scale(new id wp_fractional_scale_v1@25, wl_surface@191)
[156636.041] wl_surface@19.enter(wl_output@8)
[156636.047] wp_fractional_scale_v1@24.preferred_scale(120)
[156780.941] wp_fractional_scale_v1@24.preferred_scale(240)
[156780.942] wp_fractional_scale_v1@25.preferred_scale(360)
[156843.811] wl_surface@19.enter(wl_output@8)
[156843.832] wl_registry@2.global_remove(46)
";
    assert_eq!(window_surface(log).as_deref(), Some("wl_surface@19"));
    assert_eq!(preferred_scale(log, "wl_surface@19"), Some(240));
    assert_eq!(preferred_scale(log, "wl_surface@191"), Some(360));
    assert_eq!(preferred_scale(log, "wl_surface@1"), None);
    let before = log.lines().take(6).collect::<Vec<_>>().join("\n");
    assert_eq!(preferred_scale(&before, "wl_surface@19"), Some(120));
}

#[test]
fn the_wire_log_says_which_surface_each_cursor_was_set_on() {
    let log = "\
[2284390.180]  -> xdg_wm_base@6.get_xdg_surface(new id xdg_surface@18, wl_surface@17)
[2285062.528] wl_pointer@49.enter(1, wl_surface@17, 373.00000000, 310.00000000)
[2285065.678]  -> wl_pointer@49.set_cursor(1, wl_surface@52, 3, 1)
[2285065.866]  -> wl_pointer@49.set_cursor(1, wl_surface@52, 11, 12)
[2285675.737] wl_pointer@49.leave(2, wl_surface@17)
[2285675.771] wl_pointer@49.enter(3, wl_surface@27, 6.00000000, 264.00000000)
[2285675.827]  -> wl_pointer@49.set_cursor(3, wl_surface@52, 19, 13)
[2286081.354]  -> xdg_toplevel@19.resize(wl_seat@8, 4, 8)
[2286785.496] wl_pointer@49.leave(9, wl_surface@27)
[2286785.600]  -> wl_pointer@49.set_cursor(9, nil, 0, 0)
";
    assert_eq!(window_surface(log).as_deref(), Some("wl_surface@17"));
    assert_eq!(
        cursors_set(log),
        [
            ("wl_surface@17".to_string(), (3, 1)),
            ("wl_surface@17".to_string(), (11, 12)),
            ("wl_surface@27".to_string(), (19, 13)),
            (String::new(), (0, 0)),
        ]
    );
    assert_eq!(frame_requests(log), "resize(right)");
}
