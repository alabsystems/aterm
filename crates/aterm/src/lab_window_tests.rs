// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::{
    io::{Read as _, Write as _},
    os::{fd::AsRawFd as _, unix::ffi::OsStringExt as _},
};
fn words(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn development_guard_adoption_requires_an_explicit_private_window() {
    let good = words(&[
        "/private/build/aterm",
        "--window",
        "--control-sock",
        "/private/lab.sock",
        "--columns",
        "120",
    ]);
    assert!(window_context(&good).is_ok());
    assert!(
        window_context(&words(&["aterm", "--window", "--control-sock", ""])).is_ok(),
        "ordinary private windows retain their default-socket Commons route"
    );
    for bad in [
        words(&["aterm", "--control-sock", "/private/lab.sock"]),
        words(&[
            "aterm-cli",
            "--window",
            "--control-sock",
            "/private/lab.sock",
        ]),
        words(&[
            "aterm-gui",
            "--window",
            "--control-sock",
            "/private/lab.sock",
        ]),
        words(&[
            "aterm",
            "ctl",
            "--window",
            "--control-sock",
            "/private/lab.sock",
        ]),
        words(&["aterm", "--window"]),
        words(&["aterm", "--window", "--control-sock", "off"]),
        words(&["aterm", "--window", "--control-sock", "OFF"]),
        words(&["aterm", "--window", "--control-sock", "Off"]),
        words(&["aterm", "--control-sock", "--window"]),
        words(&["aterm", "--window", "--control-sock", "relative/socket"]),
        words(&[
            "aterm",
            "--window",
            "--control-sock",
            "/private/a",
            "--control-sock",
            "/private/b",
        ]),
    ] {
        assert!(window_context(&bad).is_err(), "{bad:?}");
    }
    for forbidden in [
        "--session",
        "--headless",
        "--diagnose",
        "--help",
        "--version",
        "--no-control-sock",
    ] {
        let mut bad = good.clone();
        bad.push(forbidden.into());
        assert!(window_context(&bad).is_err(), "{bad:?}");
    }
}

#[test]
fn claim_operands_are_opaque_to_window_adoption_and_keep_their_live_descriptors() {
    let (mut read, mut write) = aterm_uds::pipe().unwrap();
    let flock = aterm_tempfile::tempfile().unwrap();
    for prefix in [vec!["aterm", "pkg", "claim"], vec!["atpkg", "claim"]] {
        let mut argv = words(&prefix);
        argv.extend(words(&[
            "--what",
            "--lab-window-guard",
            "--key",
            "--lab-window-memory-guard=opaque",
            "--checkout",
        ]));
        argv.push(OsString::from_vec(b"/private/owned/opaque-\xff".to_vec()));
        argv.extend(words(&[
            "--rank",
            "ordinary",
            "--cores",
            "1",
            "--status-fd",
        ]));
        argv.push(write.as_raw_fd().to_string().into());
        argv.push("--flock-fd".into());
        argv.push(flock.as_raw_fd().to_string().into());
        argv.extend(words(&["--", "/bin/echo", "--lab-window-guard", "7"]));
        argv.push(OsString::from_vec(b"payload-\xff".to_vec()));
        let original = argv.clone();
        assert!(!enter(&mut argv).unwrap().is_present());
        assert_eq!(argv, original);
        write.write_all(b"held").unwrap();
        let mut bytes = [0; 4];
        read.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"held");
        assert!(flock.metadata().unwrap().is_file());
    }
}

#[test]
fn actual_window_offer_keeps_its_private_grammar_and_context_refusals() {
    let mut argv = words(&[
        "aterm",
        "--window",
        "--control-sock",
        "/private/lab.sock",
        "--lab-window-guard",
        "7",
    ]);
    let offered = take_window_offer(&mut argv).unwrap().unwrap();
    assert_eq!(offered.lane, 7);
    assert!(window_context(&argv).is_ok());
    let mut malformed = words(&[
        "aterm",
        "--window",
        "--control-sock",
        "/private/lab.sock",
        "--lab-window-guard=7",
    ]);
    let original = malformed.clone();
    assert!(take_window_offer(&mut malformed).is_err());
    assert_eq!(malformed, original);
    let mut forbidden = words(&[
        "aterm",
        "--window",
        "--control-sock",
        "/private/lab.sock",
        "--headless",
        "--lab-window-guard",
        "7",
    ]);
    assert!(
        enter(&mut forbidden).is_err(),
        "no FD is adopted before window policy admits it"
    );
}

#[test]
fn package_alias_owns_even_a_leading_flag_shaped_word() {
    let mut argv = words(&["atpkg", "--lab-window-guard", "7"]);
    let original = argv.clone();
    assert!(take_window_offer(&mut argv).unwrap().is_none());
    assert_eq!(argv, original);
    assert!(
        window_context(&argv).is_err(),
        "the package alias cannot become a private window"
    );
}
