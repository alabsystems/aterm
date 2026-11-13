// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A guest lost under a session (krunkit's abort on the GPU instance) cuts it off at the next
/// item, naming the loss and the item, and a later session is not started; while the guest
/// answers, both go on.
#[test]
fn a_lost_guest_cuts_the_session_off_and_starts_no_other() {
    use crate::linux_driver::fake::{Answer, Guest};
    let guest = Guest::new().answer(
        &["vulkaninfo"],
        Answer::Lost("krunkit aborted: virglrenderer's double free".into()),
    );
    let up = linux::fake_up(&guest, false);
    let s = Session {
        gui: Gui {
            up: &up,
            bin: "/b".into(),
            dir: "/d".into(),
            name: "aarch64-xvfb".into(),
            cpu: false,
            deck: false,
        },
        arch: Arch::Aarch64,
        screen: Screen::BASELINE[0],
        bus: String::new(),
        wl: String::new(),
        results: PathBuf::new(),
        rec: Recording::default(),
        roles: Vec::new(),
        end_terminal: None,
        marks: Vec::new(),
    };
    assert_eq!(s.alive("echo"), Ok(()), "the guest answers");
    let baseline = Baseline {
        bus: String::new(),
        wl: String::new(),
        lock: String::new(),
        committed: PathBuf::new(),
        results: PathBuf::new(),
        record: false,
    };
    let _ = s.sh(&["vulkaninfo".into()]);
    let why = s.alive("compose").expect_err("cut off");
    assert!(why.contains("cut off before compose"), "{why}");
    assert!(
        why.ends_with("krunkit aborted: virglrenderer's double free"),
        "{why}"
    );
    let calls = guest.calls().len();
    let row = baseline.session(&up, Arch::Aarch64, Screen::BASELINE[1], "/b");
    assert_eq!(row.verdict, proof::Verdict::NotRun, "{}", row.detail);
    assert!(
        row.detail.contains("lost earlier in this run"),
        "{}",
        row.detail
    );
    assert_eq!(guest.calls().len(), calls, "the next session ran nothing");
}

#[test]
fn a_present_line_names_software_only_for_mesa_s_software_rasterizer() {
    assert_eq!(
        rendering_kind("llvmpipe (LLVM 20.1.2, 128 bits) (Gl)"),
        "software rendering"
    );
    assert_eq!(
        rendering_kind("zink Vulkan 1.4(llvmpipe (LLVM 20.1.2, 128 bits) (MESA_LLVMPIPE)) (Gl)"),
        "software rendering"
    );
    assert_eq!(
        rendering_kind("zink Vulkan 1.3(Virtio-GPU Venus (Apple M4 Pro) (MESA_VENUS)) (Gl)"),
        "GPU rendering"
    );
    assert_eq!(
        rendering_kind("?"),
        "rendering",
        "an unread renderer claims neither"
    );
}

fn rec(lines: &[(Class, &str, &str)]) -> Recording {
    let mut r = Recording::default();
    for (c, k, v) in lines {
        r.push(*c, k, *v);
    }
    r
}

#[test]
fn a_recording_reads_back_what_it_wrote() {
    let mut r = rec(&[
        (Class::Stack, "guest.weston", "13.0.0"),
        (Class::Behaviour, "echo", "typed\nthen shown"),
        (Class::Measured, "keyrepeat.count", "23"),
    ]);
    r.comments.push("a title".into());
    r.behaviour("echo", "again");
    assert_eq!(r.lines[3].key, "echo.2", "a repeated key gets a suffix");
    let text = r.render();
    assert!(text.starts_with("# a title\n@ guest.weston 13.0.0\n= echo typed\\nthen shown\n"));
    assert_eq!(Recording::parse(&text).expect("parses"), r);
    assert!(Recording::parse("= \n").is_err());
    assert!(Recording::parse("! key v\n").is_err());
    assert!(Recording::parse("==key v\n").is_err());
}

#[test]
fn only_behaviour_decides_the_replay() {
    let old = rec(&[
        (Class::Behaviour, "start.dims", "24x80 scale=1.00"),
        (Class::Measured, "keyrepeat.count", "23"),
        (Class::Stack, "crate.x11rb", "0.13.2"),
    ]);
    let same = old.clone();
    assert_eq!(compare(&old, &same), Diff::default());
    let mut moved = old.clone();
    moved.lines[1].value = "22".into();
    moved.lines[2].value = "0.14.0".into();
    let d = compare(&old, &moved);
    assert!(d.same_behaviour(), "{d:?}");
    assert_eq!((d.measured.len(), d.stack.len()), (1, 1));
    let mut broke = old.clone();
    broke.lines[0].value = "24x80 scale=2.00".into();
    assert!(!compare(&old, &broke).same_behaviour());
    let mut gone = old.clone();
    gone.lines.remove(0);
    assert!(!compare(&old, &gone).same_behaviour());
    let mut came = old.clone();
    came.behaviour("not-driven.dnd", "now driven");
    let d = compare(&old, &came);
    assert_eq!(
        d.behaviour[0],
        ("not-driven.dnd".into(), None, Some("now driven".into()))
    );
    let mut reclassed = old.clone();
    reclassed.lines[0].class = Class::Measured;
    assert!(
        !compare(&old, &reclassed).same_behaviour(),
        "a class change is a move"
    );
    let text = render_diff(&compare(&old, &broke), 12);
    assert!(text.contains("behaviour start.dims:\n  - 24x80 scale=1.00\n  + 24x80 scale=2.00"));
}

#[test]
fn every_arch_and_screen_unless_named() {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    let all = parse_baseline(&[]).expect("parses");
    assert_eq!(all.arches, Arch::ALL);
    assert_eq!(all.screens, Screen::BASELINE);
    assert!(!all.record);
    let one = parse_baseline(&s(&[
        "--screen",
        "mutter-scale1.5",
        "--arch",
        "x86_64",
        "--record",
    ]))
    .expect("parses");
    assert_eq!(one.screens[0].name(), "mutter-scale1.5");
    assert_eq!(one.arches, [Arch::X86_64]);
    assert!(one.record);
    assert_eq!(
        parse_baseline(&s(&["--screen", "kwin"]))
            .map_err(|f| f.code)
            .err(),
        Some(2)
    );
    let names: Vec<String> = Screen::BASELINE.iter().map(|s| s.name()).collect();
    assert_eq!(
        names,
        [
            "xvfb",
            "xvfb-dpi144",
            "weston",
            "weston-scale2",
            "mutter",
            "mutter-scale1.5",
            "xwayland",
            "weston-x11"
        ]
    );
    assert_eq!(
        file_name(Arch::Aarch64, Screen::BASELINE[5]),
        "aarch64-mutter-scale1.5.txt"
    );
}

#[test]
fn a_drag_source_hears_the_window_take_and_leave_the_drag() {
    let over = ("text/uri-list", Some("copy"));
    let away = ("none", None);
    let mutter = [
        "target none",
        "action none",
        "target text/uri-list",
        "action copy",
        "target none",
        "target text/uri-list",
        "action copy",
    ];
    assert_eq!(
        source_heard(&mutter[..4], 0, over).as_deref(),
        Some("target text/uri-list, action copy")
    );
    assert_eq!(source_heard(&mutter[..4], 4, away), None, "nothing yet");
    assert_eq!(
        source_heard(&mutter[..5], 4, away).as_deref(),
        Some("target none")
    );
    assert_eq!(
        source_heard(&mutter[..6], 5, over).as_deref(),
        Some("target text/uri-list"),
        "the action wanted is the last one anywhere"
    );
    assert_eq!(
        source_heard(&mutter, 5, over).as_deref(),
        Some("target text/uri-list, action copy")
    );
    let weston = [
        "target text/uri-list",
        "action copy",
        "action none",
        "action copy",
    ];
    assert_eq!(
        source_heard(&weston[..2], 0, over).as_deref(),
        Some("target text/uri-list, action copy")
    );
    assert_eq!(
        source_heard(&weston, 2, away).as_deref(),
        Some("action none"),
        "weston says nothing at a leave: the next client's offer starts at none"
    );
    assert_eq!(
        source_heard(&["action none", "target text/uri-list"], 0, away),
        None,
        "a target past the point is no leave"
    );
    assert_eq!(source_heard(&["action copy"], 0, away), None);
}

#[test]
fn the_slowest_present_is_named_by_the_item_it_ended_in() {
    let metrics = "OK backend=gpu max_present_latency_ms=301.40 max_frame_render_ms=297.10 \
                   max_redraw_total_ms=299.00 max_turn_ms=300.20 max_turn_owner=redraw \
                   max_present_latency_at_ms=5230.00 max_present_latency_gap_ms=410.00 \
                   metrics_now_ms=9000.00";
    let marks = [
        ("settle", 1000.0),
        ("echo", 1200.0),
        ("resizes", 5000.0),
        ("ledger", 6000.0),
    ];
    assert_eq!(
        slowest_present(metrics, &marks),
        "301.40 ms, ending in resizes +230 ms; gap 410.00 ms; max_frame_render_ms=297.10 \
         max_redraw_total_ms=299.00 max_turn_ms=300.20 max_turn_owner=redraw"
    );
    assert!(slowest_present(metrics, &marks[3..]).contains("before the first mark"));
    assert!(slowest_present("OK", &marks).starts_with("? ms, ending in before the first mark"));
}

#[test]
fn the_replies_are_kept_without_what_moves_with_time_or_edits() {
    let dims = "OK 24 80 1120 720 session=0 cell_w=14 cell_h=30 font_px=23.00 scale=1.50 window=0 window_rows=24 window_cols=80 composed_rows=24 grid_w=1120 grid_h=720 frame_w=1156 frame_h=792 surface_w=1157 surface_h=792 offset_x=18 offset_y=54 band_left=0 band_right=0 band_top=0 band_bottom=0 crop_left=0 crop_right=0 crop_top=0 crop_bottom=0 pad=18 pad_top=2 pad_bottom=18 head=34 tab_rows=1 viewers=1 visible_viewers=1 geometry=settled present_retry_state=idle";
    assert_eq!(
        dims_summary(dims),
        "24x80 scale=1.50 cell_w=14 cell_h=30 font_px=23.00 window_rows=24 window_cols=80 grid_w=1120 grid_h=720 frame_w=1156 frame_h=792 surface_w=1157 surface_h=792 offset_x=18 offset_y=54 band_left=0 band_right=0 band_top=0 band_bottom=0 crop_left=0 crop_right=0 crop_top=0 crop_bottom=0 pad=18 pad_top=2 pad_bottom=18 head=34 tab_rows=1"
    );
    assert!(dims_summary("ERR busy").starts_with("refused"));
    assert_eq!(
        resize_line(
            "1 t=480 run=1 ord=1 from=80x24 to=80x22 alt=0 sync=0 trimmed=2 appended=0 restored=0 shift=+0 site=window at=crates/aterm-gui/src/app_config.rs:8062 win=80x22 chrome=1"
        ),
        (
            "1 ord=1 from=80x24 to=80x22 alt=0 sync=0 site=window win=80x22 chrome=1".to_string(),
            "1 run=1 ord=1 from=80x24 to=80x22 alt=0 sync=0 trimmed=2 appended=0 restored=0 shift=+0 site=window win=80x22 chrome=1".to_string()
        ),
        "how the rows moved and the run are measured; the time and source line dropped"
    );
    assert_eq!(
        keep_fields(
            "total=6 runs=5 flaps=1 risks=0 render=ok unledgered=0 lost=0 nudge=idle nudged=0",
            &SUMMARY_BEHAVIOUR
        ),
        "total=6 risks=0 render=ok unledgered=0 lost=0"
    );
    assert_eq!(
        drift_line("OK 1 verdict=clean fidelity=pass against=screen drift_rows=0 runs=1 culprit=- anchor=47.41 seed=primary geom=80x24 ms=0\nrun t=438.6 n=3"),
        (
            "verdict=clean fidelity=pass against=screen drift_rows=0 culprit=- geom=80x24".to_string(),
            "OK 1 verdict=clean fidelity=pass against=screen drift_rows=0 runs=1 culprit=- seed=primary geom=80x24".to_string()
        )
    );
    assert_eq!(
        xset_repeat("  auto repeat delay:  660    repeat rate:  25\n").as_deref(),
        Some("delay=660 rate=25")
    );
    assert_eq!(
        wayland_repeat("interface: 'wl_seat', version: 9, name: 13\n\tname: seat0\n\tcapabilities: pointer keyboard\n\tkeyboard repeat rate: 33\n\tkeyboard repeat delay: 500\n").as_deref(),
        Some("delay=500 rate=33")
    );
    assert_eq!(wayland_repeat("\tcapabilities: pointer\n"), None);
    assert_eq!(
        typed_after_prompt("lane$ echo x\nx\nlane$ aaaaa\n\n", 'a'),
        5
    );
    assert_eq!(typed_after_prompt("no prompt\n", 'a'), 0);
    assert_eq!(
        typed_line("lane$ echo x\nx\nlane$ /tmp/a\\ b.txt\n\n"),
        "/tmp/a\\ b.txt"
    );
    assert_eq!(typed_line("lane$ echo x\nx\nlane$\n"), "");
}

#[test]
fn a_literal_becomes_one_word_of_regex() {
    assert_eq!(regex_literal("echo X-x"), r"echo\sX-x");
    assert_eq!(regex_literal("lane$"), r"lane\$");
    assert_eq!(
        regex_literal("echo LANE-$((6*7))"),
        r"echo\sLANE-\$\(\(6\*7\)\)"
    );
    assert!(!regex_literal("a b c").contains(' '));
}

#[test]
fn text_is_typed_on_the_seat_a_key_each() {
    assert_eq!(
        seat_taps("echo a-1"),
        [
            "tap:e",
            "tap:c",
            "tap:h",
            "tap:o",
            "tap:space",
            "tap:a",
            "tap:-",
            "tap:1"
        ]
    );
    assert!(seat_taps("").is_empty());
}

#[test]
fn the_text_changes_rebuild_the_end_walks_text() {
    let roles = vec![("/t".to_string(), "terminal".to_string())];
    let events = [
        r#"1 7 Object.TextChanged /t detail="insert" d1=6 d2=6 any="echo x""#,
        r#"2 7 Object.TextChanged /t detail="delete" d1=12 d2=1 any="\n""#,
        r#"3 7 Object.TextChanged /t detail="insert" d1=12 d2=8 any="\nx\nlane$""#,
        r#"4 7 Object.TextCaretMoved /t detail="" d1=20 d2=0 any=0"#,
        r#"5 7 Object.BoundsChanged /f detail="" d1=0 d2=0 any=(0,0,1,1)"#,
    ];
    let end = "lane$ echo x\nx\nlane$";
    assert_eq!(
        text_replay(&events, &roles, end, Some("20")),
        "the text changes undo cleanly from the end walk's text; the last caret event names the end walk's caret"
    );
    assert_eq!(
        text_replay(&events, &roles, "lane$ echo y\nx\nlane$", Some("20")),
        "insert at 6 of 6 does not fit the text the later changes leave",
        "an insert must find its own text"
    );
    assert!(
        text_replay(&events, &roles, end, Some("19"))
            .ends_with("the end walk's caret is not the last caret event's")
    );
    assert_eq!(
        text_replay(&events[4..], &roles, end, None),
        "no text change; no caret event"
    );
    assert_eq!(
        quoted_field(
            r#"node role=terminal text="a \"b\"\n\u{e9}" caret=3"#,
            "text"
        )
        .as_deref(),
        Some("a \"b\"\n\u{e9}")
    );
}

#[test]
fn the_text_input_wire_keeps_names_and_the_content_type() {
    let log = r#"[1545378.526] wl_registry@2.global(24, "zwp_text_input_manager_v3", 1)
[1545378.743]  -> wl_registry@2.bind(24, "zwp_text_input_manager_v3", 1, new id [unknown]@19)
[1546509.728]  -> zwp_text_input_manager_v3@19.get_text_input(new id zwp_text_input_v3@79, wl_seat@8)
[1546509.991] wl_keyboard@81.enter(31, wl_surface@22, array[0])
[1546509.994] zwp_text_input_v3@79.enter(wl_surface@22)
[1546510.818]  -> zwp_text_input_v3@79.enable()
[1546510.823]  -> zwp_text_input_v3@79.set_content_type(0, 13)
[1546510.825]  -> zwp_text_input_v3@79.commit()
[1546514.163]  -> zwp_text_input_v3@79.set_cursor_rectangle(66, 36, 9, 20)
[1546514.176]  -> zwp_text_input_v3@79.commit()
[1546716.728]  -> zwp_text_input_v3@79.destroy()
"#;
    assert_eq!(
        text_input_wire(log),
        "binds zwp_text_input_manager_v3 v1; requests zwp_text_input_manager_v3.get_text_input zwp_text_input_v3.enable zwp_text_input_v3.set_content_type(0, 13) zwp_text_input_v3.commit zwp_text_input_v3.set_cursor_rectangle zwp_text_input_v3.destroy; events zwp_text_input_v3.enter"
    );
    assert_eq!(
        text_input_wire("[1.0] wl_keyboard@81.enter(31, wl_surface@22, array[0])\n"),
        "binds none; requests none; events none"
    );
}

#[test]
fn the_touch_wire_keeps_names_and_the_first_down() {
    let log = "[1166175.811]  -> wl_seat@7.get_touch(new id wl_touch@33)
[1176533.604] wl_touch@33.down(57, 1285080, wl_surface@16, 736, 640.00000000, 400.00000000)
[1176533.614] wl_touch@33.frame()
[1176534.035] wl_touch@33.motion(1285080, 736, 650.00000000, 410.00000000)
[1176534.041] wl_touch@33.frame()
[1176534.043] wl_touch@33.up(58, 1285080, 736)
[1177545.446]  -> wl_touch@33.release()
";
    assert_eq!(
        touch_wire(log),
        "requests wl_seat.get_touch wl_touch.release; events wl_touch.down wl_touch.frame wl_touch.motion wl_touch.up"
    );
    assert_eq!(
        touch_down_at(log).as_deref(),
        Some("id 736 at 640.00000000,400.00000000")
    );
    let none = "[1.0] wl_keyboard@81.enter(31, wl_surface@22, array[0])\n";
    assert_eq!(touch_wire(none), "requests none; events none");
    assert_eq!(touch_down_at(none), None);
}

#[test]
fn the_caption_line_reads_back_the_frame_and_its_buttons() {
    let chrome = "toolbar (none)\nstrip-caption frame=client buttons=minimize:68..72,maximize:72..76,close:76..80 hovered=none maximized=false edges=true menu=true\nmenu \"File\": New Tab";
    let c = parse_caption(chrome).expect("a caption line");
    assert!(c.client && c.edges && c.menu && !c.maximized);
    assert_eq!(c.names(), "minimize,maximize,close");
    assert_eq!(c.middle("maximize"), Some(74));
    assert_eq!(c.middle("restore"), None);
    assert_eq!(c.right_end(80), 68);
    let left = parse_caption(
        "strip-caption frame=client buttons=close:0..4,minimize:4..8,maximize:76..80 maximized=false",
    )
    .expect("a caption line");
    assert_eq!(
        left.right_end(80),
        76,
        "the left end's run is not the right end"
    );
    let only_left = parse_caption("strip-caption frame=client buttons=close:0..4 maximized=false")
        .expect("a caption line");
    assert_eq!(only_left.right_end(80), 80);
    let system = parse_caption("strip-caption frame=system").expect("a caption line");
    assert!(!system.client);
    assert_eq!(system.names(), "none");
    assert_eq!(parse_caption("toolbar (none)"), None, "no line");
    assert_eq!(
        parse_caption("strip-caption frame=client buttons=close:9..4 maximized=false"),
        None,
        "a slot that ends before it starts is not read"
    );
    let none = parse_caption(
        "strip-caption frame=client buttons=none hovered=none maximized=true edges=false menu=false",
    )
    .expect("a caption line");
    assert!(none.buttons.is_empty() && none.maximized);
}

#[test]
fn the_ime_line_is_found_among_the_chrome_lines() {
    let chrome = "toolbar (none)\nstrip-caption frame=system\nime preedit=\"ni hao\" caret=6 spot=66,36,9x20\nmenu \"File\": New Tab";
    assert_eq!(
        ime_line(chrome),
        Some("ime preedit=\"ni hao\" caret=6 spot=66,36,9x20")
    );
    assert_eq!(ime_line("toolbar (none)\nmenu \"ime\": x"), None, "no line");
    assert_eq!(
        ime_composition("ime preedit=\"\" caret=- spot=174,76,9x20"),
        ime_composition("ime preedit=\"\" caret=- spot=165,76,9x20"),
        "a spot that moved is no composition"
    );
    assert_ne!(
        ime_composition("ime preedit=\"n\" caret=1 spot=174,76,9x20"),
        ime_composition("ime preedit=\"\" caret=- spot=174,76,9x20"),
    );
}

#[test]
fn the_frame_requests_keep_names_and_the_resize_edge() {
    let log = "[1.0]  -> xdg_toplevel@17.set_title(\"~\")
[2.0]  -> xdg_toplevel@17.move(wl_seat@8, 1234)
[3.0]  -> xdg_toplevel@17.move(wl_seat@8, 1250)
[4.0]  -> xdg_toplevel@17.resize(wl_seat@8, 1260, 8)
[5.0] xdg_toplevel@17.configure(800, 528, array[4])
[6.0]  -> xdg_toplevel@17.show_window_menu(wl_seat@8, 1270, 300, 18)
[7.0]  -> xdg_toplevel@17.set_maximized()
";
    assert_eq!(
        frame_requests(log),
        "move resize(right) show_window_menu set_maximized"
    );
    assert_eq!(frame_requests("[1.0] xdg_toplevel@17.close()\n"), "none");
}

#[test]
fn a_caption_slot_is_34_logical_px_in_whole_cells() {
    assert_eq!(caption_slot_cols(9, 1.0), 4);
    assert_eq!(caption_slot_cols(14, 1.5), 4);
    assert_eq!(caption_slot_cols(18, 2.0), 4);
    assert_eq!(caption_slot_cols(20, 1.0), 2);
    assert_eq!(caption_slot_cols(0, 1.0), 0);
}

#[test]
fn a_slot_with_a_button_reads_drawn_and_bare_band_reads_bare() {
    let band = [48u8, 48, 48, 255];
    let ink = [200u8, 200, 200, 255];
    let mut rgba = Vec::new();
    for y in 0..4 {
        for x in 0..8 {
            let on = (5..8).contains(&x) && (1..4).contains(&y);
            rgba.extend_from_slice(if on { &ink } else { &band });
        }
    }
    let f = Frame { rgba, w: 8, h: 4 };
    assert_eq!(slot_class(&f, 4, 8, 0, 4), ("drawn", 9));
    assert_eq!(slot_class(&f, 0, 4, 0, 4), ("bare", 0));
    assert_eq!(slot_class(&f, 8, 9, 0, 4), ("bare", 0), "past the frame");
    assert_eq!(slot_class(&f, 4, 8, 1, 4), ("drawn", 9));
    assert_eq!(slot_class(&f, 4, 8, 2, 4), ("drawn", 6));
    assert_eq!(slot_class(&f, 4, 8, 4, 4), ("bare", 0), "no rows");
}

#[test]
fn a_frame_change_is_classed_by_how_much_moved() {
    let frame = |px: &[[u8; 4]]| Frame {
        rgba: px.iter().flatten().copied().collect(),
        w: px.len(),
        h: 1,
    };
    let rest = frame(&[[0, 0, 0, 255]; 200]);
    let mut cell = rest.rgba.clone();
    cell[0] = 9;
    let cell = Frame {
        rgba: cell,
        w: 200,
        h: 1,
    };
    assert_eq!(frame_change(&rest, &rest).0, "at most a cell");
    assert_eq!(frame_change(&rest, &cell).0, "at most a cell");
    let washed = frame(&[[9, 9, 9, 255]; 200]);
    assert_eq!(frame_change(&rest, &washed).0, "most pixels");
    assert_eq!(frame_change(&rest, &washed).1, "200 of 200 pixels");
    let mut half = vec![[0, 0, 0, 255]; 150];
    half.extend([[9, 9, 9, 255]; 50]);
    assert_eq!(frame_change(&rest, &frame(&half)).0, "part");
    assert!(
        frame_change(&rest, &frame(&[[0, 0, 0, 255]; 3]))
            .0
            .starts_with("size")
    );
}

#[test]
fn an_event_is_kept_by_kind_and_role() {
    let roles = vec![
        (
            "/org/a11y/atspi/accessible/0/0".to_string(),
            "frame".to_string(),
        ),
        (
            "/org/a11y/atspi/accessible/0/1".to_string(),
            "terminal".to_string(),
        ),
    ];
    let state = "510 77 Object.StateChanged /org/a11y/atspi/accessible/0/1 detail=\"focused\" d1=1 d2=0 any=0";
    assert_eq!(
        event_kind(state, &roles),
        "Object.StateChanged:focused=1@terminal"
    );
    let activate =
        "510 77 Window.Activate /org/a11y/atspi/accessible/0/0 detail=\"\" d1=0 d2=0 any=\"~\"";
    assert_eq!(event_kind(activate, &roles), "Window.Activate@frame");
    let text = "936 77 Object.TextChanged /x detail=\"insert\" d1=126 d2=2 any=\" a\"";
    assert_eq!(event_kind(text, &roles), "Object.TextChanged:insert@?");
    assert_eq!(
        event_body(text),
        "Object.TextChanged /x detail=\"insert\" d1=126 d2=2 any=\" a\""
    );
    assert_eq!(event_parts(text).map(|(pid, _)| pid), Some("77"));
    let node =
        "node depth=2 path=/org/a11y/atspi/accessible/0/1 role=terminal name=\"aterm terminal\"";
    let (path, rest) = node_parts(node).expect("a node");
    assert_eq!(path, "/org/a11y/atspi/accessible/0/1");
    assert_eq!(rest, "depth=2 role=terminal name=\"aterm terminal\"");
}

#[test]
fn the_stack_is_read_from_the_lock_and_the_servers() {
    let lock = "[[package]]\nname = \"x11rb\"\nversion = \"0.13.2\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"winit\"\nversion = \"0.30.13\"\n\n[[package]]\nname = \"serde\"\nversion = \"1.0.0\"\nsource = \"registry+x\"\n";
    assert_eq!(
        lock_stack(lock),
        [
            ("winit".to_string(), "0.30.13 (path)".to_string()),
            ("x11rb".to_string(), "0.13.2".to_string())
        ]
    );
    let info = "interface: 'wl_seat',                                    version:  8, name: 14\n\tname: seat0\ninterface: 'wl_compositor',                              version:  5, name:  1\n";
    assert_eq!(wayland_globals(info), "wl_compositor@5,wl_seat@8");
    let xd = "name of display:    :91\nnumber of extensions:    3\n    XTEST  (opcode: 132)\n    RANDR  (opcode: 140, base event: 89, base error: 147)\n    BIG-REQUESTS\ndefault screen number:    0\n";
    assert_eq!(x_extensions(xd), "BIG-REQUESTS,RANDR,XTEST");
    assert_eq!(
        keymap_summary("rules:      evdev\nmodel:      pc105\nlayout:     us\nvariant:    intl\n"),
        "us intl"
    );
    assert!(guest_packages(Arch::X86_64).contains(&"libx11-6:amd64".to_string()));
    assert!(guest_packages(Arch::Aarch64).contains(&"weston".to_string()));
}

#[test]
fn walk_settled_requires_idle_frame_title_and_trailing_prompt() {
    let snap = |title: &str, text: &str| {
        format!(
            "apps 1\n\
             node depth=0 path=/org/a11y/atspi/accessible/root role=application name=\"aterm-gui\"\n\
             node depth=1 path=/org/a11y/atspi/accessible/0/0 role=frame name=\"{title}\"\n\
             node depth=2 path=/org/a11y/atspi/accessible/0/1 role=terminal name=\"aterm terminal\" text=\"{text}\"\n"
        )
    };
    assert!(walk_settled(&snap(
        "~",
        "lane$ echo LANE-$((6*7))\\nLANE-42\\nlane$\\n\\n\\n"
    )));
    assert!(!walk_settled(&snap(
        "echo LANE-$((6*7))",
        "lane$ echo LANE-$((6*7))\\nLANE-42\\nlane$\\n\\n\\n"
    )));
    assert!(!walk_settled(&snap(
        "~",
        "lane$ echo LANE-$((6*7))\\nLANE-42\\n\\n\\n"
    )));
    assert!(!walk_settled(&snap(
        "~",
        "lane$ echo LANE-$((6*7))\\nLANE-42\\nlane$ echo X-WLCOPY-42 | tr A-Z a-z\\nx-wlcopy-42\\n\\n"
    )));
}
