// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The modal-resize row's wait passes only on a present whose frame was filled at or after the
/// echo's content clock, never on `none`, an older frame or a reply that is not one.
#[test]
fn a_present_drew_the_echo_only_at_or_past_its_content_clock() {
    assert_eq!(matched_seq("OK match 41\n"), Some(41));
    assert_eq!(matched_seq("OK gone 41\n"), None);
    assert_eq!(matched_seq("ERR timeout\n"), None);
    let dims = |drawn: &str| {
        format!("OK 30 120 960 480 session=0 geometry=window presented_seq={drawn}\n")
    };
    assert!(presented_through(&dims("41"), 41));
    assert!(presented_through(&dims("42"), 41));
    assert!(
        !presented_through(&dims("40"), 41),
        "a frame filled before the echo"
    );
    assert!(
        !presented_through(&dims("none"), 41),
        "no present of this grid"
    );
    assert!(
        !presented_through("OK 30 120 960 480 session=0 geometry=window\n", 41),
        "a build with no presented_seq never passes"
    );
}

/// A `w` x `h` RGBA frame whose pixels all differ, and the RGB readback of a window that shows it.
fn frame_and_window(w: usize, h: usize) -> (Vec<u8>, Vec<u8>) {
    let frame: Vec<u8> = (0..w * h)
        .flat_map(|i| [i as u8, 0x10, 0x20, 0xff])
        .collect();
    let window = frame
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    (frame, window)
}

#[test]
fn a_hold_on_the_screens_part_ignores_the_rows_past_its_edge_and_no_others() {
    let (w, h) = (3, 4);
    let (frame, mut window) = frame_and_window(w, h);
    window[3 * w * 3..].fill(0xff);
    let place =
        desk::parse_place("readback client=3x4 at=10,7 screen=0,0,100,10").expect("a place");
    let on = place.on_monitors(&[place.screen]);
    assert_eq!(
        on,
        vec![desk::Rect {
            l: 0,
            t: 0,
            r: 3,
            b: 3
        }]
    );
    assert!(compare_rgb(&frame, w, h, &(window.clone(), w, h), 0..0).is_err());
    let framed = (frame.clone(), w, h);
    let mut held = window.clone();
    let (put, span) = off_monitors_as_framed(&mut held, (w, h), &on, &framed, (0, 0));
    assert_eq!(
        (put, span),
        (
            3,
            Some(desk::Rect {
                l: 0,
                t: 3,
                r: 3,
                b: 4
            })
        )
    );
    assert_eq!(compare_rgb(&frame, w, h, &(held, w, h), 0..0), Ok(()));
    window[w * 3 + 1] ^= 1;
    let mut held = window;
    off_monitors_as_framed(&mut held, (w, h), &on, &framed, (0, 0));
    assert_eq!(
        compare_rgb(&frame, w, h, &(held, w, h), 0..0),
        Err(
            "1 of 12 pixels differ; first at (0,1): presented [03, 10, 20], server [03, 11, 20]"
                .into()
        )
    );
}

/// The second monitor row's place on 2026-10-25: back on the primary, the client area reached
/// past its right edge and below the smaller second monitor beside it. Scaled down, a 10x10
/// primary with a 4x5 monitor to its right, and a 6x8 client area at (6,1).
#[test]
fn a_hold_ignores_the_pixels_between_the_monitors_and_no_others() {
    let monitors = [
        desk::Rect {
            l: 0,
            t: 0,
            r: 10,
            b: 10,
        },
        desk::Rect {
            l: 10,
            t: 0,
            r: 14,
            b: 5,
        },
    ];
    let place = desk::parse_place("readback client=6x8 at=6,1 screen=0,0,14,10").expect("a place");
    assert_eq!(
        place.on_screen(),
        Some(desk::Rect {
            l: 0,
            t: 0,
            r: 6,
            b: 8
        }),
        "all of it is on the virtual screen"
    );
    let on = place.on_monitors(&monitors);
    assert_eq!(
        on,
        vec![
            desk::Rect {
                l: 0,
                t: 0,
                r: 4,
                b: 8
            },
            desk::Rect {
                l: 4,
                t: 0,
                r: 6,
                b: 4
            }
        ]
    );
    let (w, h) = (6, 8);
    let (frame, mut window) = frame_and_window(w, h);
    let framed = (frame.clone(), w, h);
    // What Windows left between the monitors: the theme's background, not the frame.
    for y in 4..8 {
        for x in 4..6 {
            window[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&[0x11, 0x13, 0x18]);
        }
    }
    assert!(compare_rgb(&frame, w, h, &(window.clone(), w, h), 0..0).is_err());
    let mut held = window.clone();
    let (put, span) = off_monitors_as_framed(&mut held, (w, h), &on, &framed, (0, 0));
    assert_eq!(
        (put, span),
        (
            8,
            Some(desk::Rect {
                l: 4,
                t: 4,
                r: 6,
                b: 8
            })
        )
    );
    assert_eq!(compare_rgb(&frame, w, h, &(held, w, h), 0..0), Ok(()));
    // A wrong pixel beside the gap, on the primary, and one on the second monitor above it.
    for at in [(3, 6), (5, 3)] {
        let mut planted = window.clone();
        planted[(at.1 * w + at.0) * 3] ^= 1;
        off_monitors_as_framed(&mut planted, (w, h), &on, &framed, (0, 0));
        let got = compare_rgb(&frame, w, h, &(planted, w, h), 0..0);
        assert!(
            got.as_ref()
                .is_err_and(|e| e.contains(&format!("first at ({},{})", at.0, at.1))),
            "{at:?}: {got:?}"
        );
    }
}

/// A frame smaller than the client area sits at `dims`' offset and its bands hold its nearest
/// pixels; a pixel on no monitor is put to what that rule expects there, and no other.
#[test]
fn a_hold_off_the_monitors_puts_the_frames_nearest_pixel_at_an_offset() {
    let (w, h) = (4, 6);
    let (cw, ch) = (6, 6);
    let (frame, _) = frame_and_window(w, h);
    let framed = (frame.clone(), w, h);
    let (ox, oy) = (1, 0);
    let nearest = |x: usize, y: usize| {
        let nx = (x as i64 - ox).clamp(0, w as i64 - 1) as usize;
        let ny = (y as i64 - oy).clamp(0, h as i64 - 1) as usize;
        [
            frame[(ny * w + nx) * 4],
            frame[(ny * w + nx) * 4 + 1],
            frame[(ny * w + nx) * 4 + 2],
        ]
    };
    let mut window: Vec<u8> = (0..ch)
        .flat_map(|y| (0..cw).map(move |x| (x, y)))
        .flat_map(|(x, y)| nearest(x, y))
        .collect();
    let on = [
        desk::Rect {
            l: 0,
            t: 0,
            r: 4,
            b: 6,
        },
        desk::Rect {
            l: 4,
            t: 0,
            r: 6,
            b: 3,
        },
    ];
    for y in 3..6 {
        for x in 4..6 {
            window[(y * cw + x) * 3..(y * cw + x) * 3 + 3].fill(0xff);
        }
    }
    let rgba = |px: &[u8]| -> (Vec<u8>, usize, usize) {
        (
            px.as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 0xff])
                .collect(),
            cw,
            ch,
        )
    };
    let view = WestonView {
        x: 0,
        y: 0,
        width: cw,
        height: ch,
        format: "XRGB8888".into(),
    };
    assert!(held_at(&rgba(&window), &framed, ox, oy, 0..0).is_err());
    let mut held = window.clone();
    let (put, span) = off_monitors_as_framed(&mut held, (cw, ch), &on, &framed, (ox, oy));
    assert_eq!(
        (put, span),
        (
            6,
            Some(desk::Rect {
                l: 4,
                t: 3,
                r: 6,
                b: 6
            })
        )
    );
    assert_eq!(held_at(&rgba(&held), &framed, ox, oy, 0..0), Ok((4, 6)));
    assert_eq!(bands_held(&rgba(&held), &framed, &view, (ox, oy)), Ok(12));
    // The band column 0 is on the primary: a wrong pixel there still fails.
    held[(2 * cw) * 3] ^= 1;
    assert!(bands_held(&rgba(&held), &framed, &view, (ox, oy)).is_err());
}

#[test]
fn dims_are_read_from_a_windowed_reply() {
    let r = "OK 23 83 1079 828 session=0 cell_w=13 cell_h=36 font_px=26.00 scale=1.50 window=0 \
             window_rows=23 window_cols=83 composed_rows=24 grid_w=1079 grid_h=828 frame_w=1115 frame_h=996 \
             surface_w=1116 surface_h=996 geometry=window";
    let d = Dims::parse(r).unwrap_or_else(|| unreachable!());
    assert_eq!((d.rows, d.cols), (23, 83));
    assert_eq!((d.scale.as_str(), d.font_px.as_str()), ("1.50", "26.00"));
    assert_eq!((d.window_rows, d.window_cols), (23, 83));
    assert_eq!(d.frame, (1115, 996));
    assert_eq!(d.geometry, "window");
    assert_eq!(Dims::parse("ERR no session"), None);
    assert_eq!(Dims::parse("OK 23 83 scale=1.50"), None);
}

#[test]
fn what_the_rows_type_has_keys_and_never_holds_its_echo() {
    let widths = WIDTH_TAGS.map(width_typed);
    for text in [TYPED, IME_BEFORE, IME_READING, IME_AFTER]
        .into_iter()
        .chain(widths.iter().map(String::as_str))
    {
        assert!(text.chars().all(|c| desk::qcodes(c).is_some()), "{text}");
    }
    assert!(IME_ECHO.chars().any(|c| desk::qcodes(c).is_none()));
    let typed = format!("{IME_BEFORE}{IME_READING}{IME_AFTER}");
    assert!(!typed.contains(IME_ECHO));
    assert!(format!("IME-{IME_READING}") != IME_ECHO);
    assert!(!EXPOSED_TYPED.contains(EXPOSED_ECHO));
    for (tag, typed) in WIDTH_TAGS.iter().zip(&widths) {
        assert!(
            !(0..10).any(|d| typed.contains(&format!("{tag}{d}"))),
            "{typed}"
        );
        for other in WIDTH_TAGS {
            assert!(other == *tag || !other.starts_with(tag), "{tag} {other}");
        }
    }
    for (before, after, typed, echo) in [
        (DEAD_BEFORE, DEAD_AFTER, DEAD_TYPED, DEAD_ECHO),
        (ALTGR_BEFORE, DEAD_AFTER, ALTGR_TYPED, ALTGR_ECHO),
    ] {
        assert!(
            format!("{before}{after}")
                .chars()
                .all(|c| desk::qcodes(c).is_some())
        );
        assert!(!format!("{before}{typed}{after}").contains(echo), "{echo}");
        assert!(echo.ends_with(typed), "{echo}");
    }
    assert!(DEAD_TYPED.chars().any(|c| desk::qcodes(c).is_none()));
    assert_eq!(ALTGR_KEYS.len(), ALTGR_TYPED.chars().count());
    assert!(
        format!("{DROP_BEFORE}{DROP_AFTER}")
            .chars()
            .all(|c| desk::qcodes(c).is_some())
    );
    let pasted = format!("{DROP_BEFORE}\"C:\\lane\\desk\\aarch64-1\\{DROP_FILE}\" {DROP_AFTER}");
    assert!(!pasted.contains(DROP_ECHO));
    assert!(DROP_ECHO.ends_with(DROP_FILE) && DROP_FILE.contains(' '));
    assert!(
        !format!("Write-Output ('{HELD_TAG}' + [Console]::WindowWidth)")
            .contains(&format!("{HELD_TAG}8"))
    );
    assert_eq!(IME_STEPS.len(), IME_READING.chars().count());
    assert!(IME_STEPS.iter().all(|step| !step.contains(IME_CONVERTED)));
    assert!(IME_STEPS.windows(2).all(|w| w[0] != w[1]));
}

#[test]
fn every_echo_is_awaited_as_written() {
    let widths = WIDTH_TAGS.map(|tag| format!("{tag}80"));
    for echo in [
        EXPOSED_ECHO,
        crate::linux_proof::ECHO,
        IME_ECHO,
        DEAD_ECHO,
        ALTGR_ECHO,
        DROP_ECHO,
        &format!("{HELD_TAG}80"),
    ]
    .into_iter()
    .chain(widths.iter().map(String::as_str))
    {
        let pattern = literal(echo);
        assert!(!pattern.contains(char::is_whitespace), "{pattern}");
        let re = aterm_regex::Regex::new(&pattern).unwrap_or_else(|e| panic!("{echo}: {e:?}"));
        assert!(
            re.is_match(&format!("PS C:\\Users\\lane> {echo}")),
            "{echo}"
        );
    }
    // Negative controls: unescaped, the AltGr echo is no expression at all (the lane's
    // `ERR badregex`), escaped alone the drop echo keeps its space (which the wire splits, the
    // lane's next `ERR badregex`), and the drop's `.` matches a byte it does not hold.
    assert!(aterm_regex::Regex::new(ALTGR_ECHO).is_err());
    assert!(aterm_regex::escape(DROP_ECHO).contains(' '));
    let dot = aterm_regex::Regex::new(DROP_ECHO).unwrap_or_else(|e| panic!("{e:?}"));
    let near = DROP_ECHO.replace('.', "x");
    assert!(dot.is_match(&near));
    let escaped = aterm_regex::Regex::new(&literal(DROP_ECHO)).unwrap_or_else(|e| panic!("{e:?}"));
    assert!(!escaped.is_match(&near));
}

#[test]
fn the_drop_is_released_only_on_aterms_copy_under_the_cursor() {
    let aterm = desk::Rect {
        l: 100,
        t: 50,
        r: 1200,
        b: 950,
    };
    let source = desk::Rect {
        l: 1600,
        t: 900,
        r: 1860,
        b: 1060,
    };
    let start = "source at=1600,900,1860,1060\n";
    let crossing = "over effect=None at=1700,950\nover effect=Copy at=1500,700\n";
    let on_aterm = "over effect=Copy at=650,500\n";
    assert_eq!(
        accepted_over(&format!("{start}{crossing}{on_aterm}"), &aterm, &source),
        Ok((650, 500))
    );
    // Negative controls: no answer yet; Copy from a window the path crossed (the desktop);
    // aterm's window refusing; and the source's own label, where it overlaps aterm's.
    assert!(accepted_over(start, &aterm, &source).is_err());
    assert!(accepted_over(&format!("{start}{crossing}"), &aterm, &source).is_err());
    assert!(
        accepted_over(
            &format!("{start}{crossing}over effect=None at=650,500\n"),
            &aterm,
            &source
        )
        .is_err()
    );
    let over_aterm = desk::Rect {
        l: 600,
        t: 450,
        r: 860,
        b: 610,
    };
    assert!(accepted_over(&format!("{start}{on_aterm}"), &aterm, &over_aterm).is_err());
    // The last answer is the one that stands: the cursor left aterm's window after it.
    assert!(
        accepted_over(
            &format!("{start}{on_aterm}over effect=None at=1700,950\n"),
            &aterm,
            &source
        )
        .is_err()
    );
    assert!(desk::HELPER.contains("\"over effect={0} at={1},{2}\""));
}

#[test]
fn the_launch_rows_report_the_style_words_and_dwm() {
    let w = desk::parse_windows(
        "window class=aterm rect=0,0,10,10 client=8x8 dpi=96 foreground=1 awareness=2 layout=0409 ime=0 \
         machine=aa64 conv=0 movesize=0 style=16cf0000 exstyle=00200100 backdrop=0 dark=-",
    );
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(
        styles(&w[0]),
        "style 16cf0000 exstyle 00200100, DWM backdrop 0 dark -"
    );
    // The recorded words hold the window they were read from (the before-run's launch line) and
    // not another: this one's extended style, backdrop and dark mode differ.
    let recorded = desk::parse_windows(
        "window class=aterm rect=0,0,10,10 client=8x8 dpi=96 foreground=1 awareness=2 layout=0409 ime=0 \
         machine=aa64 conv=0 movesize=0 style=16cf0000 exstyle=00040110 backdrop=1 dark=1",
    );
    assert_eq!(Styles::of(&recorded[0]), LAUNCH_STYLES);
    assert_ne!(Styles::of(&w[0]), LAUNCH_STYLES);
    assert_eq!(
        styles_of(LAUNCH_STYLES),
        "style 16cf0000 exstyle 00040110, DWM backdrop 1 dark 1"
    );
}

#[test]
fn window_takes_arches_and_nothing_else() {
    assert_eq!(
        parse_args(&[]).ok(),
        Some(Ask {
            arches: vec![Arch::Aarch64, Arch::X86_64],
            watch: false,
            cpu: false
        })
    );
    assert_eq!(
        parse_args(&["--arch".into(), "x86_64".into(), "--watch".into()]).ok(),
        Some(Ask {
            arches: vec![Arch::X86_64],
            watch: true,
            cpu: false
        })
    );
    assert_eq!(
        parse_args(&["--cpu".into()]).ok(),
        Some(Ask {
            arches: vec![Arch::Aarch64, Arch::X86_64],
            watch: false,
            cpu: true
        })
    );
    assert!(parse_args(&["--arch".into(), "mips".into()]).is_err());
    assert!(parse_args(&["--vnc".into()]).is_err());
    let r = desk::Rect {
        l: 0,
        t: 0,
        r: 1920,
        b: 1200,
    };
    assert!(r.holds((1919, 1199)) && !r.holds((1920, 0)));
}
