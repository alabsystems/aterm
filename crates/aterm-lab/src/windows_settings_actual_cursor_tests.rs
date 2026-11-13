// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn requested_point_is_not_actual_cursor_ownership() {
    let raw = "target-owned=1 left_down=0 cursor=100,200 cursor-owned=1 foreground=1";
    assert!(actual_cursor_button(raw, (100, 200), false));
    assert!(!actual_cursor_button(raw, (101, 200), false));
    assert!(!actual_cursor_button(raw, (100, 200), true));
    assert_eq!(cursor_ready_to_press(raw, (100, 200)), Ok(true));
    assert_eq!(cursor_ready_to_press(raw, (101, 200)), Ok(false));
    for bad in [
        raw.replace("left_down=0", "left_down=1"),
        raw.replace("left_down=0", "left_down=unknown"),
        raw.replace("target-owned=1", "target-owned=0"),
        raw.replace("cursor-owned=1", "cursor-owned=0"),
        raw.replace("foreground=1", "foreground=0"),
        raw.replace("cursor=100,200", "cursor=unknown"),
        raw.replace("cursor=100,200", "cursor=100,broken"),
        format!("{raw} left_down=0"),
    ] {
        assert!(cursor_ready_to_press(&bad, (100, 200)).is_err(), "{bad}");
    }
    for bad in [
        raw.replace("cursor=100,200", "cursor=unknown"),
        raw.replace("cursor-owned=1", "cursor-owned=0"),
        raw.replace("foreground=1", "foreground=0"),
        format!("{raw} cursor=100,200"),
        format!("{raw} left_down=1"),
        format!("{raw} target-owned=0"),
        "target-owned=1 left_down=0".into(),
    ] {
        assert!(!actual_cursor_button(&bad, (100, 200), false));
    }
    let monitor = desk::Rect {
        l: 800,
        t: 0,
        r: 2720,
        b: 1200,
    };
    assert!(monitor_contains(
        monitor,
        desk::Rect {
            l: 816,
            t: 16,
            r: 1950,
            b: 820
        }
    ));
    assert!(!monitor_contains(
        monitor,
        desk::Rect {
            l: 26,
            t: 26,
            r: 1160,
            b: 830
        }
    ));
    assert!(!monitor_contains(
        monitor,
        desk::Rect {
            l: 816,
            t: 16,
            r: 1950,
            b: 1220
        }
    ));
}
