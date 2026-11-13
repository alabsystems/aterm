// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::native_path;

#[cfg(windows)]
#[test]
fn drive_letter_uri_path_becomes_a_native_windows_path() {
    assert_eq!(native_path("/C:/Users//x"), "C:\\Users\\x");
    assert_eq!(native_path("/c:/Users//x"), "c:\\Users\\x");
    assert_eq!(native_path("/C|/Users//x"), "C:\\Users\\x");
    assert_eq!(native_path("/C:/"), "C:\\");
    assert_eq!(native_path("/C:"), "C:\\");
}

#[cfg(windows)]
#[test]
fn an_already_native_windows_path_passes_through_unchanged() {
    assert_eq!(native_path("C:\\Users\\x"), "C:\\Users\\x");
    assert_eq!(
        native_path(native_path("/C:/Users//x").as_ref()),
        "C:\\Users\\x"
    );
    assert_eq!(native_path("C:/Users//x"), "C:/Users//x");
}

#[cfg(windows)]
#[test]
fn a_unc_payload_keeps_its_leading_double_slash() {
    assert_eq!(native_path("//server/share"), "\\\\server\\share");
    assert_eq!(native_path("//server/share/dir"), "\\\\server\\share\\dir");
    assert_eq!(native_path("\\\\server\\share"), "\\\\server\\share");
}

#[cfg(windows)]
#[test]
fn non_drive_shaped_paths_are_left_alone_on_windows_too() {
    assert_eq!(native_path("/home/user/proj"), "/home/user/proj");
    assert_eq!(native_path("/C:extra/x"), "/C:extra/x");
    assert_eq!(native_path("/1:/x"), "/1:/x");
    assert_eq!(native_path(""), "");
}

#[cfg(windows)]
#[test]
fn an_msys_drive_path_becomes_the_drive_path_it_names() {
    assert_eq!(native_path("/c/Users//x"), "C:\\Users\\x");
    assert_eq!(native_path("/D/src/a b"), "D:\\src\\a b");
    assert_eq!(native_path("/cygdrive/c/Users//x"), "C:\\Users\\x");
    assert_eq!(native_path("/c"), "C:\\");
    assert_eq!(native_path("/c/"), "C:\\");
    assert_eq!(native_path("/cygdrive/e"), "E:\\");
    assert_eq!(
        native_path(native_path("/c/Users//x").as_ref()),
        "C:\\Users\\x"
    );
}

#[cfg(windows)]
#[test]
fn an_msys_drive_path_is_this_machines_only_when_this_machine_reported_it() {
    use super::{ReportedCwd as _, native_path_reported};
    use aterm_core::terminal::Terminal;
    assert_eq!(native_path_reported("/c/x", || false), "/c/x");
    assert_eq!(
        native_path_reported("/cygdrive/c/x", || false),
        "/cygdrive/c/x"
    );
    assert_eq!(native_path_reported("/c/x", || true), "C:\\x");
    assert_eq!(
        native_path_reported("//srv/share", || panic!("not asked")),
        "\\\\srv\\share"
    );
    assert_eq!(
        native_path_reported("/home/x", || panic!("not asked")),
        "/home/x"
    );
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]7;file://build-box.example/c/Users//x\x07");
    assert_eq!(
        term.native_working_directory().as_deref(),
        Some("/c/Users//x")
    );
    for local in [
        &b"\x1b]7;file:///c/Users//x\x07"[..],
        b"\x1b]7;file://localhost/c/Users//x\x07",
    ] {
        term.process(local);
        assert_eq!(
            term.native_working_directory().as_deref(),
            Some("C:\\Users\\x"),
            "{local:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn a_drive_letter_path_is_this_machines_only_when_this_machine_reported_it() {
    use super::{ReportedCwd as _, native_path_reported};
    use aterm_core::terminal::Terminal;
    assert_eq!(native_path_reported("/C:/x", || false), "/C:/x");
    assert_eq!(native_path_reported("/C|/x", || false), "/C|/x");
    assert!(!std::path::Path::new("/C:/x").is_absolute());
    assert_eq!(native_path_reported("/C:/x", || true), "C:\\x");
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b]7;file://build-box.example/C:/Users//x\x07");
    assert_eq!(
        term.native_working_directory().as_deref(),
        Some("/C:/Users//x")
    );
    for local in [
        &b"\x1b]7;file:///C:/Users//x\x07"[..],
        b"\x1b]7;file://localhost/C:/Users//x\x07",
    ] {
        term.process(local);
        assert_eq!(
            term.native_working_directory().as_deref(),
            Some("C:\\Users\\x"),
            "{local:?}"
        );
    }
}

/// THIS MACHINE BY ANY OF ITS NAMES: a report naming no host, its DNS
/// name (the names the strip knows) or — what aterm's PowerShell
/// integration writes — its NetBIOS name, in any case, is this machine's.
#[test]
fn a_report_is_this_machines_by_any_of_its_names() {
    use super::reported_here;
    let dns = |h: &str| h.eq_ignore_ascii_case("build-machine-of-andrew.lan");
    let netbios = Some("BUILD-MACHINE-O");
    assert!(reported_here(None, dns, netbios));
    assert!(reported_here(
        Some("Build-Machine-Of-Andrew.lan"),
        dns,
        netbios
    ));
    assert!(reported_here(Some("BUILD-MACHINE-O"), dns, netbios));
    assert!(reported_here(Some("build-machine-o"), dns, netbios));
    assert!(!reported_here(Some("other-box"), dns, netbios));
    assert!(!reported_here(Some("BUILD-MACHINE-O"), dns, None));
    assert!(!reported_here(Some("BUILD-MACHINE-O"), dns, Some("  ")));
}

#[cfg(windows)]
#[test]
fn only_a_one_letter_msys_component_is_a_drive() {
    for left in [
        "/usr/bin",
        "/tmp",
        "/cx/y",
        "/c:x",
        "/cygdrive",
        "/cygdrive/",
        "/cygdrive/cx/y",
        "/cygdrivex/c/y",
        "/1/x",
        "/é/x",
    ] {
        assert_eq!(native_path(left), left, "{left}");
        assert!(matches!(native_path(left), std::borrow::Cow::Borrowed(_)));
    }
}

#[cfg(unix)]
#[test]
fn the_drive_rewrite_never_runs_on_unix() {
    assert_eq!(native_path("/C:/foo"), "/C:/foo");
    assert_eq!(native_path("/C:/Users//x"), "/C:/Users//x");
    assert_eq!(native_path("/C|/foo"), "/C|/foo");
    assert_eq!(native_path("/c/x"), "/c/x");
    assert_eq!(native_path("/cygdrive/c/x"), "/cygdrive/c/x");
    assert_eq!(native_path("//server/share"), "//server/share");
    assert_eq!(native_path("/Users//x"), "/Users//x");
}

#[test]
fn conversion_is_borrow_only_for_paths_it_does_not_touch() {
    assert!(matches!(
        native_path("/Users//x"),
        std::borrow::Cow::Borrowed(_)
    ));
    assert!(matches!(native_path(""), std::borrow::Cow::Borrowed(_)));
}
