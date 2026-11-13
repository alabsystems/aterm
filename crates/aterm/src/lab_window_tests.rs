// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
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
