// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const BASE: &CStr = c"com.aterm.test.state-notify";

/// A home spelled directly and through a link names one notification, and so does a home not
/// made yet beneath either spelling; another home names another. CONTROL: the two spellings
/// differ as paths, so only the resolution joins them.
/// NEGATIVE CONTROL: `name` hashed `home` as spelled in place of `resolved(home)`: "assertion
/// `left == right` failed: a home through a link names the notification it names directly",
/// left `com.aterm.test.state-notify.d61697a08a996224`, right `….7a8b9fbfb4235513` (that run's
/// scratch paths' keys).
#[cfg(unix)]
#[test]
fn two_spellings_of_one_home_name_one_notification() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-state-notify-")
        .tempdir()
        .expect("a scratch directory");
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).expect("the home");
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&home, &link).expect("a link to the home");
    assert_ne!(home, link, "control: two spellings");

    assert_eq!(
        name(BASE, &link),
        name(BASE, &home),
        "a home through a link names the notification it names directly"
    );
    assert_eq!(
        name(BASE, &link.join("state")),
        name(BASE, &home.join("state")),
        "a home not made yet, beneath a link, names what it names beneath the target"
    );
    let made = home.join("state");
    let before = name(BASE, &made);
    std::fs::create_dir(&made).expect("the home made");
    assert_eq!(
        name(BASE, &made),
        before,
        "a home names once made what it named before"
    );
    assert_ne!(
        name(BASE, &tmp.path().join("other")),
        name(BASE, &home),
        "another home names another notification"
    );
}

/// A home not made yet spelled with a `..` over a part not made yet names, once made, what it
/// named before and what its spelling without the step names; so does one whose `..` steps back
/// past every part not made yet. CONTROL: each spelling differs as a path from the one without
/// the step, so only the resolution joins them.
/// NEGATIVE CONTROL: `resolved` walked up by `file_name`, which stops at a `..` and hashes the
/// home as spelled: "assertion `left == right` failed: a home spelled with a step back names once
/// made what it named before", left `com.aterm.test.state-notify.82a3ab7eeb5376da`, right
/// `….830db86a111bce0e` (that run's scratch path's keys).
#[test]
fn a_home_spelled_with_a_step_back_names_once_made_what_it_named_before() {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-state-notify-")
        .tempdir()
        .expect("a scratch directory");
    let home = tmp.path().join("missing").join("..").join("state");
    let plain = tmp.path().join("state");
    assert_ne!(home, plain, "control: two spellings");
    let before = name(BASE, &home);
    std::fs::create_dir_all(&home).expect("the home made");
    assert_eq!(
        name(BASE, &home),
        before,
        "a home spelled with a step back names once made what it named before"
    );
    assert_eq!(
        name(BASE, &plain),
        before,
        "it names what its spelling without the step names"
    );

    let scratch_name = tmp.path().file_name().expect("a named scratch folder");
    let past = tmp
        .path()
        .join("gone")
        .join("..")
        .join("..")
        .join(scratch_name)
        .join("past");
    let plain = tmp.path().join("past");
    assert_ne!(past, plain, "control: two spellings");
    let before = name(BASE, &past);
    std::fs::create_dir_all(&past).expect("the home made");
    assert_eq!(
        name(BASE, &past),
        before,
        "a step back past every part not made yet names once made what it named before"
    );
    assert_eq!(
        name(BASE, &plain),
        before,
        "it names what its spelling without the steps names"
    );
}

/// The name is the base, a dot and 16 lowercase hex digits, whatever the home's length.
#[test]
fn the_name_is_the_base_and_a_fixed_key() {
    let long: PathBuf = std::iter::once("/")
        .chain(std::iter::repeat_n("a-long-folder-name", 200))
        .collect();
    for home in [Path::new("/"), long.as_path()] {
        let named = name(BASE, home);
        let text = named.to_str().expect("ASCII");
        let key = text
            .strip_prefix("com.aterm.test.state-notify.")
            .expect("the base and a dot");
        assert_eq!(key.len(), KEY_LEN - 1, "{text}");
        assert!(
            key.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "{text}"
        );
        assert_eq!(named.count_bytes(), BASE.count_bytes() + KEY_LEN);
    }
}
