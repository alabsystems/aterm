// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn returned_native_coordinates_supply_both_transform_directions() {
    let raw = "OK at=native view=3 x=425.0 y=126.0 key=settings/control/flag px=425,164";
    let mapped = native_mapping(raw, "3", "settings/control/flag", 1.0).unwrap();
    assert_eq!(mapped.origin, (0, 38));
    assert_eq!(mapped.screen((20, 40)), Ok((445, 204)));
    assert_eq!(mapped.inverse((445, 204), (20, 40)), Some((425, 126)));
    let other = native_mapping(
        &raw.replace("px=425,164", "px=437,193"),
        "3",
        "settings/control/flag",
        1.0,
    )
    .unwrap();
    assert_eq!(other.origin, (12, 67));
    assert_eq!(
        other.inverse(other.screen((7, 9)).unwrap(), (7, 9)),
        Some((425, 126))
    );
    for bad in [
        raw.replace("at=native", "at=terminal"),
        raw.replace("OK ", "ERR "),
        format!("{raw} at=native"),
        format!("{raw} key=settings/control/flag"),
        format!("{raw} px=425,164"),
        raw.replace("view=3", "view=4"),
        raw.replace("key=settings/control/flag", "key=settings/control/other"),
        raw.replace("x=425.0", "x=NaN"),
        raw.replace("x=425.0", "x=inf"),
        raw.replace("x=425.0", "x=2147483648.0"),
        raw.replace("x=425.0", "x=-2147483648.0")
            .replace("px=425,164", "px=2147483647,164"),
        raw.replace("y=126.0", "y=126.5"),
        raw.replace("px=425,164", "px=unknown"),
        raw.replace("px=425,164", ""),
        format!("{raw} view=3"),
        format!("{raw} x=425.0"),
        format!("{raw}\n{raw}"),
    ] {
        assert!(
            native_mapping(&bad, "3", "settings/control/flag", 1.0).is_err(),
            "{bad}"
        );
    }
    assert!(native_mapping(raw, "3", "settings/control/flag", 2.0).is_err());
    assert!(mapped.screen((i32::MAX, i32::MAX)).is_err());
    assert_eq!(
        mapped.inverse((i32::MIN, i32::MIN), (i32::MAX, i32::MAX)),
        None
    );
    assert!(
        native_mapping(
            &raw.replace("view=3", "view=0"),
            "0",
            "settings/control/flag",
            1.0
        )
        .is_err()
    );

    let up = "target-owned=1 foreground=1 left_down=0 owned-hwnd=ab12 cursor=10,20 cursor-owned=0";
    assert!(released_owned_target(up));
    for bad in [
        up.replace("left_down=0", "left_down=1"),
        up.replace("owned-hwnd=ab12", "owned-hwnd=0"),
        format!("{up} left_down=0"),
        up.replace("foreground=1", "foreground=0"),
    ] {
        assert!(!released_owned_target(&bad));
    }
}
