// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

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
