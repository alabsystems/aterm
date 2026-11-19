// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::time::{Duration, Instant};

const DIMS: &str = "OK 24 80 720 480 scale=1.00 surface_w=1014 surface_h=528 \
    frame_w=744 frame_h=528 offset_x=270 offset_y=0 cell_w=9 cell_h=20 \
    pad=12 pad_top=2 head=14 tab_rows=1 pad_bottom=12 grid_w=720 grid_h=480";

fn tree(client: &str) -> String {
    format!(r#"{{"nodes":[{{"nodes":[{{"floating_nodes":[{client}]}}]}}]}}"#)
}

fn client(pid: u32, x: i32, y: i32, inset: i32) -> String {
    format!(
        r#"{{"app_id":"aterm","pid":{pid},"visible":true,
        "rect":{{"x":{x},"y":{y},"width":{},"height":528}},
        "window_rect":{{"x":{inset},"y":0,"width":1014,"height":528}}}}"#,
        1014 + inset * 2,
    )
}

#[test]
fn client_placement_comes_from_the_exact_pid_and_excludes_sways_decorations() {
    let geometry = NativeGeometry::parse(DIMS).unwrap();
    let ssd = client(42, 0, 25, 2);
    let csd = client(42, 131, 122, 0);
    assert_eq!(
        client_origin(&tree(&ssd), 42, geometry).unwrap(),
        Placement::Ready((2, 25))
    );
    assert_eq!(
        client_origin(&tree(&csd), 42, geometry).unwrap(),
        Placement::Ready((131, 122))
    );
    let decoy = client(41, 900, 900, 0);
    assert_eq!(
        client_origin(&tree(&format!("{decoy},{csd}")), 42, geometry).unwrap(),
        Placement::Ready((131, 122))
    );
    // A relayed GUI's app_id survives, but its compositor peer is the proxy.
    let proxied = tree(&client(77, 131, 122, 0));
    assert!(matches!(
        client_origin(&proxied, 42, geometry),
        Ok(Placement::Pending(_))
    ));
    assert_eq!(
        client_origin(&proxied, 77, geometry).unwrap(),
        Placement::Ready((131, 122))
    );
    for bad in [
        tree(&format!("{csd},{csd}")),
        tree(&csd.replace("\"visible\":true", "\"ignored\":false")),
        tree(&ssd.replace("\"width\":1014", "\"width\":1019")),
        tree(&csd.replace("\"pid\":42", "\"pid\":42,\"pid\":41")),
        tree(&csd.replace("\"pid\":42", "\"pid\":42.0")),
        tree(&client(42, i32::MAX, 0, 2)),
        "not JSON".into(),
    ] {
        assert!(client_origin(&bad, 42, geometry).is_err(), "{bad}");
    }
    let scaled = NativeGeometry::parse(&DIMS.replace("scale=1.00", "scale=2.00")).unwrap();
    assert!(client_origin(&tree(&csd), 42, scaled).is_err());
}

#[test]
fn mapping_and_a_pending_configure_do_not_supply_pointer_coordinates() {
    let launch = NativeGeometry::parse(DIMS).unwrap();
    let tiled = NativeGeometry::parse(&DIMS.replace("surface_w=1014", "surface_w=1276")).unwrap();
    let node = client(42, 0, 25, 2)
        .replace("1018", "1280")
        .replace("1014", "1276");
    let mapped = tree(&node);
    for (tree, geometry) in [
        (tree(""), launch),
        (
            tree(&node.replace("\"visible\":true", "\"visible\":false")),
            tiled,
        ),
        (mapped.clone(), launch),
    ] {
        assert!(matches!(
            client_origin(&tree, 42, geometry),
            Ok(Placement::Pending(_))
        ));
    }
    assert_eq!(
        client_origin(&mapped, 42, tiled).unwrap(),
        Placement::Ready((2, 25))
    );
}

#[test]
fn a_grid_target_is_pure_window_geometry_and_differs_from_the_native_seed() {
    let geometry = NativeGeometry::parse(DIMS).unwrap();
    assert_eq!(grid_cell(DIMS, 2, 2).unwrap(), (304, 86));
    assert_eq!(geometry.grid_midpoint(DIMS).unwrap(), (642, 276));
    let warmup = grid_cell(DIMS, 1, 1).unwrap();
    assert_eq!(warmup, (295, 66));
    let seed = strip_point(DIMS).unwrap();
    assert_eq!(seed, (286, 26));
    assert_ne!(warmup, geometry.grid_midpoint(DIMS).unwrap());
    assert_ne!(warmup, seed);
    assert_ne!(geometry.grid_midpoint(DIMS).unwrap(), seed);
    assert_ne!(geometry.right_midpoint(-2).unwrap(), seed);
    assert_eq!(grid_cell(DIMS, 23, 79).unwrap(), (997, 506));
    for (row, col) in [(-1, 0), (0, -1), (24, 0), (0, 80)] {
        assert!(grid_cell(DIMS, row, col).is_err());
    }
    assert!(grid_cell(&DIMS.replace("grid_h=480", "grid_h=9999"), 2, 2).is_err());
}

#[test]
fn a_native_strip_seed_requires_visible_uncropped_chrome() {
    for bad in [
        DIMS.replace("tab_rows=1", "tab_rows=0"),
        DIMS.replace("tab_rows=1", "tab_rows=-1"),
        DIMS.replace("tab_rows=1", "tab_rows=2147483647"),
        DIMS.replace("cell_w=9", "cell_w=0"),
        DIMS.replace("cell_h=20", "cell_h=0"),
        DIMS.replace("pad=12", "pad=-1"),
        DIMS.replace("pad_top=2", "pad_top=-1"),
        DIMS.replace("head=14", "head=-1"),
        DIMS.replace("offset_x=270", "offset_x=999"),
        DIMS.replace("offset_y=0", "offset_y=502"),
        format!("{DIMS} tab_rows=2"),
    ] {
        assert!(strip_point(&bad).is_err(), "{bad}");
    }
    assert_eq!(
        strip_point(&DIMS.replace("offset_y=0", "offset_y=7")).unwrap(),
        (286, 33)
    );
}

const PREFIX: &str = "\
[1] -> xdg_wm_base@6.get_xdg_surface(new id xdg_surface@18, wl_surface@17)
[2] -> wp_cursor_shape_manager_v1@32.get_pointer(new id [unknown]@33, wl_pointer@49)
[3] wl_pointer@49.enter(1, wl_surface@17, 642.00000000, 276.00000000)
[3.1] wl_pointer@49.frame()
[4] -> wp_cursor_shape_device_v1@33.set_shape(1, 1)
[4.1] wl_pointer@49.motion(99, 1.00000000, 264.00000000)
[4.15] wl_pointer@49.frame()
[4.2] -> wp_cursor_shape_device_v1@33.set_shape(1, 1)
";
const MOTION: &str =
    "[5] wl_pointer@49.motion(100, 642.00000000, 276.00000000)\n[5.1] wl_pointer@49.frame()\n";
const RESPONSE: &str = "[6] -> wp_cursor_shape_device_v1@33.set_shape(1, 9)\n";

#[test]
fn warmup_needs_a_fresh_native_arrival_and_frame_after_its_wire_checkpoint() {
    let old = PREFIX.lines().take(5).collect::<Vec<_>>().join("\n") + "\n";
    let target = (642, 276);
    assert!(!arrived(&old, &old, target).unwrap());
    for suffix in [RESPONSE, "[5] wl_pointer@49.frame()\n"] {
        assert!(!arrived(&format!("{old}{suffix}"), &old, target).unwrap());
    }
    let motion = "[5] wl_pointer@49.motion(100, 642.00000000, 276.00000000)\n";
    assert!(!arrived(&format!("{old}{motion}"), &old, target).unwrap());
    let fresh = format!("{old}{MOTION}");
    assert!(arrived(&fresh, &old, target).unwrap());
    assert!(!handled(&fresh, target, Cursor::Shape(9)));
    for suffix in [
        MOTION.replace("wl_pointer@49", "wl_pointer@50"),
        "[5] wl_pointer@49.enter(2, wl_surface@117, 642.00000000, 276.00000000)\n[6] wl_pointer@49.frame()\n".into(),
        // The observed failure: coalesced moves enter at the seed, never warm up.
        "[5] wl_pointer@49.enter(2, wl_surface@17, 1.00000000, 264.00000000)\n[6] wl_pointer@49.frame()\n".into(),
    ] {
        assert!(!arrived(&format!("{old}{suffix}"), &old, target).unwrap());
    }
    let entry = format!(
        "{old}[5] wl_pointer@49.enter(2, wl_surface@17, 642.00000000, 276.00000000)\n[6] wl_pointer@49.frame()\n"
    );
    assert!(arrived(&entry, &old, target).unwrap());
    let partial = old.len() + 20;
    assert!(!arrived(&fresh, &fresh[..partial], target).unwrap());
    assert!(arrived(&format!("{fresh}{MOTION}"), &fresh[..partial], target).unwrap());
    assert!(arrived("replaced", &old, target).is_err());
    assert!(arrived(&old[..old.len() - 1], &old, target).is_err());
}

#[test]
fn native_fractional_positions_keep_identity_without_rounding_target_pixels() {
    let entered = "\
[1] -> xdg_wm_base@6.get_xdg_surface(new id xdg_surface@19, wl_surface@18)
[2] wl_pointer@25.enter(36, wl_surface@18, 1012.00000000, 252.50000000)
[3] wl_pointer@25.frame()
";
    let motion =
        "[4] wl_pointer@25.motion(1, 295.00000000, 66.00000000)\n[5] wl_pointer@25.frame()\n";
    let cursors = "\
[6] -> wl_pointer@25.set_cursor(36, wl_surface@31, 3, 1)
[7] -> wl_pointer@25.set_cursor(36, wl_surface@31, 11, 12)
";
    let complete = format!("{entered}{motion}{cursors}");
    assert!(arrived(&complete, "", (295, 66)).unwrap());
    assert!(handled(&complete, (295, 66), Cursor::Surface));
    assert!(!arrived(entered, "", (1012, 252)).unwrap());
    let fractional = complete.replace("295.00000000", "295.50000000");
    assert!(!arrived(&fractional, "", (295, 66)).unwrap());
    assert!(!handled(&fractional, (295, 66), Cursor::Surface));
    let recovered =
        format!("{fractional}{motion}[8] -> wl_pointer@25.set_cursor(36, wl_surface@31, 11, 12)\n");
    assert!(handled(&recovered, (295, 66), Cursor::Surface));
    for nonfinite in ["NaN", "inf", "-inf", "1e999"] {
        for bad in [
            complete.replace("1012.00000000", nonfinite),
            complete.replace("295.00000000", nonfinite),
        ] {
            assert!(!arrived(&bad, "", (295, 66)).unwrap(), "{bad}");
            assert!(!handled(&bad, (295, 66), Cursor::Surface), "{bad}");
        }
    }
}

#[test]
fn a_pointer_target_requires_native_arrival_and_a_later_response_on_that_pointer() {
    let target = (642, 276);
    assert!(handled(PREFIX, (1, 264), Cursor::Any));
    let entered = PREFIX.lines().take(5).collect::<Vec<_>>().join("\n");
    assert!(!handled(&entered, target, Cursor::Any));
    assert!(!handled(
        &entered.replace("set_shape(1, 1)", "set_shape(1, 9)"),
        target,
        Cursor::Shape(9)
    ));
    assert!(!handled(PREFIX, target, Cursor::Shape(9)));
    assert!(!handled(
        &format!("{PREFIX}{MOTION}"),
        target,
        Cursor::Shape(9)
    ));
    assert!(!handled(
        &format!("{PREFIX}{RESPONSE}{MOTION}"),
        target,
        Cursor::Shape(9)
    ));
    let complete = format!("{PREFIX}{MOTION}{RESPONSE}");
    assert!(handled(&complete, target, Cursor::Shape(9)));
    assert!(!handled(&complete, target, Cursor::Shape(18)));
    for bad in [
        complete.replace("[5.1] wl_pointer@49.frame()\n", ""),
        complete.replace("set_shape(1, 9)", "set_shape(2, 9)"),
        complete.replace(
            "[6] -> wp_cursor_shape_device_v1@33",
            "[6] -> wp_cursor_shape_device_v1@34",
        ),
        complete.replace("wl_pointer@49.motion", "wl_pointer@50.motion"),
        complete.replace("wl_surface@17, 642", "wl_surface@117, 642"),
        complete.replace("642.00000000", "642.50000000"),
        format!("{complete}[7] wl_pointer@49.leave(2, wl_surface@17)\n"),
        format!("{complete}[7] wl_pointer@49.motion(101, 1.00000000, 264.00000000)\n"),
    ] {
        assert!(!handled(&bad, target, Cursor::Shape(9)), "{bad}");
    }
    let surface =
        format!("{PREFIX}{MOTION}[6] -> wl_pointer@49.set_cursor(1, wl_surface@52, 3, 1)\n");
    assert!(handled(&surface, target, Cursor::Surface));
    assert!(!handled(
        &surface.replace("wl_surface@52", "nil"),
        target,
        Cursor::Surface
    ));
    assert!(!handled(
        &surface.replace("[6] -> wl_pointer@49", "[6] -> wl_pointer@50"),
        target,
        Cursor::Surface
    ));
}

#[test]
fn an_enter_replay_after_a_batched_motion_and_frame_is_not_a_gui_acknowledgment() {
    let opening = PREFIX.lines().take(2).collect::<Vec<_>>().join("\n");
    let batch = format!(
        "{opening}\n[3] wl_pointer@49.enter(1, wl_surface@17, 642.00000000, 276.00000000)\n[4] wl_pointer@49.motion(99, 1.00000000, 264.00000000)\n[5] wl_pointer@49.frame()\n"
    );
    let replay = format!("{batch}[6] -> wp_cursor_shape_device_v1@33.set_shape(1, 1)\n");
    assert!(!handled(&replay, (1, 264), Cursor::Any));
    let consumed = format!("{replay}[7] -> wp_cursor_shape_device_v1@33.set_shape(1, 25)\n");
    assert!(handled(&consumed, (1, 264), Cursor::Shape(25)));
    let stale = format!(
        "{batch}[6] -> wp_cursor_shape_device_v1@33.set_shape(2, 1)\n[7] -> wp_cursor_shape_device_v1@33.set_shape(1, 25)\n"
    );
    assert!(!handled(&stale, (1, 264), Cursor::Any));
    assert!(handled(
        &format!("{stale}[8] -> wp_cursor_shape_device_v1@33.set_shape(1, 25)\n"),
        (1, 264),
        Cursor::Shape(25)
    ));
    let legacy_replay = format!("{batch}[6] -> wl_pointer@49.set_cursor(1, wl_surface@52, 3, 1)\n");
    assert!(!handled(&legacy_replay, (1, 264), Cursor::Surface));
    assert!(!handled(
        &legacy_replay.replace("set_cursor(1,", "set_cursor(2,"),
        (1, 264),
        Cursor::Surface
    ));
    let consumed =
        format!("{legacy_replay}[7] -> wl_pointer@49.set_cursor(1, wl_surface@52, 11, 12)\n");
    assert!(handled(&consumed, (1, 264), Cursor::Surface));
}

fn stat(pid: u32, birth: u64, cpu: u64) -> String {
    let mut fields = vec!["0".to_string(); 20];
    fields[0] = "S".into();
    fields[11] = cpu.to_string();
    fields[17] = "1".into();
    fields[19] = birth.to_string();
    format!("{pid} (a gui (name)) {}", fields.join(" "))
}

fn sample(pid: u32, birth: u64, cpu: u64) -> String {
    let stat = stat(pid, birth, cpu);
    format!(
        "{stat}\nrchar: 10\nwchar: 20\nsyscr: 3\nsyscw: 4\nread_bytes: 5\nwrite_bytes: 6\ncancelled_write_bytes: 0\n{stat}\n"
    )
}

#[test]
fn guest_work_is_bound_to_the_same_process_on_both_sides_of_the_io_read() {
    let text = sample(42, 7, 10);
    assert!(matches!(
        reading(42, &text),
        Reading::Work(Work { birth: 7, .. })
    ));
    assert!(matches!(reading(41, &text), Reading::Unknown(None)));
    assert!(matches!(
        reading(42, &text.replace("syscw: 4", "syscw: unread")),
        Reading::Unknown(Some(7))
    ));
    let changed = text.replacen(&stat(42, 7, 10), &stat(42, 8, 10), 1);
    assert!(matches!(reading(42, &changed), Reading::Unknown(Some(7))));
    assert!(matches!(
        reading(42, &text.replace(") S ", ") Z ")),
        Reading::Ended
    ));
}

#[test]
fn native_work_extends_the_existing_window_and_unknown_samples_cannot_close_it() {
    let start = Instant::now();
    let mut work = Progress::new(&[42, 43]);
    let mut window = aterm_uds::deadline::ProgressWindow::new(Duration::from_secs(60), start);
    let mut observe = |seconds, cpu, unknown| {
        window.observe(start + Duration::from_secs(seconds), 0, || {
            work.observe(|pid| {
                if unknown && pid == 42 {
                    Reading::Unknown(Some(7))
                } else {
                    reading(pid, &sample(pid, 7, cpu))
                }
            })
            .unwrap()
        })
    };
    assert!(!observe(0, 10, false).is_zero());
    assert!(!observe(1, 10, false).is_zero());
    assert!(!observe(60, 11, false).is_zero());
    assert!(!observe(119, 11, false).is_zero());
    assert!(!observe(120, 11, true).is_zero());
    assert!(!observe(180, 11, false).is_zero());
    assert!(!observe(181, 11, false).is_zero());
    assert!(observe(241, 11, false).is_zero());
    assert!(!observe(242, 9, false).is_zero());
    assert!(!observe(243, 9, false).is_zero());
    assert!(observe(303, 9, false).is_zero());
    for reading in [Reading::Unknown(Some(8)), Reading::Ended] {
        assert!(
            work.observe(|_| match reading {
                Reading::Unknown(birth) => Reading::Unknown(birth),
                _ => Reading::Ended,
            })
            .is_err()
        );
    }
}

#[test]
fn answering_placement_queries_does_not_extend_the_hang_detector() {
    let mut work = Progress::new(&[42]);
    let at = |cpu| move |pid| reading(pid, &sample(pid, 7, cpu));
    work.after_probe(at(4)).unwrap();
    assert_eq!(work.observe(at(4)).unwrap(), Some(0));
    work.after_probe(at(8)).unwrap();
    assert_eq!(work.observe(at(8)).unwrap(), Some(0));
    assert_eq!(work.observe(at(9)).unwrap(), Some(1));
    assert!(
        work.after_probe(|pid| reading(pid, &sample(pid, 8, 9)))
            .is_err()
    );
}
