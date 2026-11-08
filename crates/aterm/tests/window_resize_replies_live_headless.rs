// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A WINDOW'S SIZE VERBS SAY WHAT THEY DID WITH SETTINGS IN FRONT: `resize` under Settings'
//! least view gives the least and says so, `dims` reads the window's own size, `status` names the
//! verbs that read what is there, and a resize of the terminal behind says how long its grid
//! lasts. Each answered a bare `OK` or `ERR no active terminal` before.

#![cfg(unix)]

use crate::headless_boot::{self, Instance, field};

/// What `aterm ctl <args>` printed, its stdout then its stderr, and whether it exited 0.
fn answer(inst: &Instance, args: &[&str]) -> (bool, String) {
    let out = headless_boot::ctl(inst, args);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The `(rows, cols)` a `dims` line opens with.
fn grid_of(dims: &str) -> (u16, u16) {
    let mut words = dims.split_whitespace().skip(1);
    let mut next = || {
        words
            .next()
            .and_then(|word| word.parse().ok())
            .unwrap_or_else(|| panic!("a grid in `{dims}`"))
    };
    (next(), next())
}

#[test]
fn a_window_holding_settings_says_the_size_each_resize_gave_it() {
    let inst = headless_boot::boot("atwmr", &["--lines", "34", "--columns", "100"]);
    let (local, _) = headless_boot::boot_session(&inst);
    let terminal = format!("@{local}");
    headless_boot::ctl_ok(&inst, &["open", "app", "settings", "/home"]);

    let held = headless_boot::ctl_ok(&inst, &["resize", "6", "20"]);
    assert!(
        held.starts_with("OK ") && held.contains(" held=least asked_rows=6 asked_cols=20"),
        "a grid under Settings' least view is held to it, and the reply says so: {held}"
    );
    let given: (u16, u16) = (
        field(&held, "rows")
            .and_then(|n| n.parse().ok())
            .expect("rows="),
        field(&held, "cols")
            .and_then(|n| n.parse().ok())
            .expect("cols="),
    );
    assert!(given.0 > 6 && given.1 > 20, "{held}");

    let (read, dims) = answer(&inst, &["dims"]);
    assert!(
        read && dims.contains(" session=none "),
        "with Settings in front `dims` reads the window: {dims}"
    );
    assert_eq!(
        grid_of(&dims),
        given,
        "the size the resize said it gave: {dims}"
    );
    let (read, status) = answer(&inst, &["status"]);
    assert!(
        !read && status.contains("`dims` reads the window's size"),
        "`status` has no session to report and names the verbs that read what is there: {status}"
    );

    assert_eq!(
        headless_boot::ctl_ok(&inst, &["resize", "34", "100"]),
        "OK rows=34 cols=100\n",
        "a grid over the least is given as asked"
    );
    let behind = headless_boot::ctl_ok(&inst, &[terminal.as_str(), "resize", "30", "30"]);
    assert_eq!(
        behind, "OK rows=30 cols=30 until=front window=0 front_rows=34 front_cols=100\n",
        "a grid set behind Settings lasts until the terminal's tab comes to the front"
    );
    headless_boot::ctl_ok(&inst, &["tab", "1"]);
    let front = headless_boot::ctl_ok(&inst, &["dims"]);
    assert_eq!(
        grid_of(&front),
        (34, 100),
        "in front, the window gives the terminal the grid the reply named: {front}"
    );
}
