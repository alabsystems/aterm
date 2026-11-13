// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_helpers_windows_and_monitors_are_read_back() {
    let out = "window class=aterm rect=208,82,1346,1134 client=1116x996 dpi=144 foreground=1 awareness=2 layout=0411 ime=1 machine=8664 conv=19 movesize=1 style=16cf0000 exstyle=00200100 backdrop=0 dark=1\r\n\
               window class=Winit_Thread_Event_Target rect=0,0,16,16 client=0x0 dpi=96 foreground=0 awareness=2 layout=0409 ime=-1 machine=- conv=- movesize=0 style=94000000 exstyle=00000080 backdrop=- dark=-\r\n\
               window class=broken rect=1,2,3 client=1x1 dpi=96 foreground=0 awareness=2 layout=0409 ime=0 machine=aa64 conv=0 movesize=0 style=0 exstyle=0 backdrop=0 dark=0\r\n\
               window class=aterm rect=0,0,9,9 client=9x9 dpi=96 foreground=0 awareness=2 layout=0407 ime=0 machine=aa64 conv=0 movesize=0\r\n";
    let w = parse_windows(out);
    assert_eq!(w.len(), 2, "{w:?}");
    assert_eq!(w[0].class, ATERM_CLASS);
    assert_eq!(
        w[0].rect,
        Rect {
            l: 208,
            t: 82,
            r: 1346,
            b: 1134
        }
    );
    assert_eq!(
        (w[0].client, w[0].dpi, w[0].foreground),
        ((1116, 996), 144, true)
    );
    assert_eq!(
        (w[0].awareness, w[0].layout.as_str(), w[0].ime_open),
        (2, "0411", Some(true))
    );
    assert_eq!(w[1].ime_open, None);
    assert_eq!(
        (w[0].machine.as_str(), w[1].machine.as_str()),
        ("8664", "-")
    );
    assert_eq!((w[0].move_size, w[1].move_size), (true, false));
    assert_eq!(
        (w[0].style, w[0].exstyle, w[0].backdrop, w[0].dark),
        (0x16cf_0000, 0x0020_0100, Some(0), Some(1))
    );
    assert_eq!((w[1].backdrop, w[1].dark), (None, None));
    assert_eq!(w[0].rect.centre(), (777, 608));
    assert!(w[0].rect.holds((208, 82)) && !w[0].rect.holds((1346, 500)));

    let mons = "monitor name=\\\\.\\DISPLAY2 rect=0,0,1920,1200 dpi=96 primary=1 scale=0/0/4 adapter=Red Hat VirtIO GPU DOD controller\r\n\
                monitor name=\\\\.\\DISPLAY1 rect=1920,0,2720,600 dpi=96 primary=0 scale=- adapter=Microsoft Basic Display Adapter\r\n\
                set=0\r\n";
    let m = parse_monitors(mons);
    assert_eq!(m.len(), 2, "{m:?}");
    assert_eq!(m[0].name, "\\\\.\\DISPLAY2");
    assert!(m[0].primary && m[0].adapter.contains(VIRTIO_ADAPTER));
    assert_eq!(m[0].scale, Some((0, 0, 4)));
    assert_eq!(m[1].scale, None);
    assert_eq!(m[1].rect.width(), 800);
    assert_eq!(m[1].adapter, "Microsoft Basic Display Adapter");

    assert_eq!(parse_place("error=no-window\r\n"), None);
    let cut = parse_place("readback client=1076x939 at=239,273 screen=0,0,2720,1200\r\n")
        .expect("a place");
    assert_eq!(cut.client, (1076, 939));
    assert_eq!(cut.at, (239, 273));
    assert_eq!(
        cut.on_screen(),
        Some(Rect {
            l: 0,
            t: 0,
            r: 1076,
            b: 927
        })
    );
    let whole =
        parse_place("place client=1076x939 at=239,261 screen=0,0,2720,1200\r\n").expect("a place");
    assert_eq!(
        whole.on_screen(),
        Some(Rect {
            l: 0,
            t: 0,
            r: 1076,
            b: 939
        })
    );
    let left = parse_place("place client=100x50 at=-30,-10 screen=0,0,1920,1200")
        .and_then(|p| p.on_screen());
    assert_eq!(
        left,
        Some(Rect {
            l: 30,
            t: 10,
            r: 100,
            b: 50
        })
    );
    assert_eq!(
        parse_place("place client=100x50 at=3000,0 screen=0,0,1920,1200")
            .and_then(|p| p.on_screen()),
        None
    );
    assert_eq!(parse_place("readback client=1116x996\r\n"), None);

    // The lane's monitors, and a 150 % client area back on the primary but reaching past its
    // right edge: on the virtual screen whole, on the monitors but for the part below the
    // second one.
    let monitors: Vec<Rect> = m.iter().map(|m| m.rect).collect();
    let across =
        parse_place("readback client=1080x903 at=1204,45 screen=0,0,2720,1200").expect("a place");
    assert_eq!(
        across.on_screen(),
        Some(Rect {
            l: 0,
            t: 0,
            r: 1080,
            b: 903
        })
    );
    assert_eq!(
        across.on_monitors(&monitors),
        vec![
            Rect {
                l: 0,
                t: 0,
                r: 716,
                b: 903
            },
            Rect {
                l: 716,
                t: 0,
                r: 1080,
                b: 555
            }
        ]
    );
    assert_eq!(
        cut.on_monitors(&monitors),
        vec![cut.on_screen().expect("a part")]
    );
    assert_eq!(
        parse_place("place client=100x50 at=2000,700 screen=0,0,2720,1200")
            .expect("a place")
            .on_monitors(&monitors),
        Vec::new(),
        "below the second monitor, beside the primary"
    );
}

#[test]
fn every_character_typed_has_its_us_key_and_others_have_none() {
    assert_eq!(qcodes('a'), Some(vec!["a"]));
    assert_eq!(qcodes('W'), Some(vec!["shift", "w"]));
    assert_eq!(qcodes('7'), Some(vec!["7"]));
    assert_eq!(qcodes('('), Some(vec!["shift", "9"]));
    assert_eq!(qcodes('\''), Some(vec!["apostrophe"]));
    assert_eq!(qcodes('\n'), Some(vec!["ret"]));
    assert_eq!(qcodes('日'), None);
    assert_eq!(qcodes('é'), None);
    for c in "Write-Output ('IME-' + [Console]::WindowWidth*7)".chars() {
        assert!(qcodes(c).is_some(), "{c:?}");
    }
}

#[test]
fn the_qmp_commands_are_json_qemu_reads() {
    let k = key_command(&["meta_l", "spc"], 60);
    assert_eq!(
        k,
        "{\"execute\":\"send-key\",\"arguments\":{\"keys\":[{\"type\":\"qcode\",\"data\":\"meta_l\"},\
         {\"type\":\"qcode\",\"data\":\"spc\"}],\"hold-time\":60}}"
    );
    for c in [
        k,
        tablet_move(959, 599, 1920, 1200),
        tablet_button(true),
        screendump_command("/tmp/a \"b\".png", Some("gpu1")),
        screendump_command("/tmp/a.png", None),
    ] {
        assert!(aterm_json::from_str::<aterm_json::Value>(&c).is_ok(), "{c}");
    }
    assert!(tablet_move(0, 0, 1920, 1200).contains("\"value\":0}"));
    assert!(tablet_move(1919, 1199, 1920, 1200).contains(&format!("\"value\":{TABLET_MAX}}}")));
    assert!(tablet_move(5000, -3, 1920, 1200).contains(&format!("\"x\",\"value\":{TABLET_MAX}}}")));
    assert!(screendump_command("/x.png", Some("gpu1")).contains(",\"device\":\"gpu1\""));
    assert!(!screendump_command("/x.png", None).contains("device"));
}

#[test]
fn the_helper_command_keeps_its_path_and_quotes_its_words() {
    let line = helper_line(&["scale", "\\\\.\\DISPLAY2", "2"]);
    assert!(line.starts_with("powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File C:\\lane\\desk\\desk.ps1 scale "));
    assert!(line.ends_with(" 2 2>&1"));
    assert_eq!(
        instance_dir(guest::Arch::X86_64, 2),
        "C:\\lane\\desk\\x86_64-2"
    );
}
