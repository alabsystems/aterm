// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn object_paths() {
    for good in ["/", "/org", "/org/a11y/atspi/accessible/root", "/a_1/B2"] {
        assert!(is_object_path(good), "{good}");
    }
    for bad in ["", "org", "/org/", "//", "/org//a11y", "/org.a11y", "/é"] {
        assert!(!is_object_path(bad), "{bad}");
    }
}

#[test]
fn interfaces_members_and_bus_names() {
    assert!(is_interface("org.a11y.atspi.Event.Object"));
    assert!(is_interface("org.freedesktop.DBus.Error.UnknownMethod"));
    for bad in [
        "org",
        ".org.a11y",
        "org..a11y",
        "org.a11y.",
        "org.1a11y",
        "org.a-11y",
        "",
    ] {
        assert!(!is_interface(bad), "{bad}");
    }
    assert!(!is_interface(&format!("a.{}", "b".repeat(254))));
    assert!(is_member("GetRole") && is_member("_x1"));
    for bad in ["", "1x", "Get.Role", "Get-Role"] {
        assert!(!is_member(bad), "{bad}");
    }
    for good in [
        ":1.42",
        ":1.0",
        "org.a11y.Bus",
        "org.freedesktop.DBus",
        "a-b.c_d",
        ":x-1.2y",
    ] {
        assert!(is_bus_name(good), "{good}");
    }
    for bad in [
        "",
        ":",
        ":1",
        "org",
        "org.",
        "1org.a11y",
        ":1..2",
        "org.a11y/Bus",
    ] {
        assert!(!is_bus_name(bad), "{bad}");
    }
}
