// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::askpass_env;

const BUNDLE: &str = "/Applications/aterm.app/Contents/MacOS";

#[test]
fn sets_both_when_unset_in_a_bundle() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let env = askpass_env(Some(BUNDLE), false, None, None);
    assert!(
        env.iter().any(|(k, v)| k == "SUDO_ASKPASS"
            && v == "/Applications/aterm.app/Contents/MacOS/aterm-askpass")
    );
    assert!(env.iter().any(|(k, v)| k == "DISPLAY" && v.is_empty()));
}

#[test]
fn keeps_the_users_own_values() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let env = askpass_env(Some(BUNDLE), false, Some("/opt/my/askpass"), Some(":0"));
    assert!(env.iter().all(|(k, _)| k != "SUDO_ASKPASS"), "{env:?}");
    assert!(env.iter().all(|(k, _)| k != "DISPLAY"), "{env:?}");
}

#[test]
fn replaces_an_enclosing_aterms_helper() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let env = askpass_env(
        Some(BUNDLE),
        false,
        Some("/old/aterm.app/Contents/MacOS/aterm-askpass"),
        Some(":0"),
    );
    assert_eq!(
        env.iter()
            .find(|(k, _)| k == "SUDO_ASKPASS")
            .map(|(_, v)| v.as_str()),
        Some("/Applications/aterm.app/Contents/MacOS/aterm-askpass")
    );
}

#[test]
fn nothing_without_a_bundle_or_when_contained() {
    assert!(askpass_env(None, false, None, None).is_empty());
    assert!(askpass_env(Some(BUNDLE), true, None, None).is_empty());
}
