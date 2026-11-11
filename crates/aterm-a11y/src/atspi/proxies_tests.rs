// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn only_status_and_launcher_signals_count() {
    let status = Message::signal(A11Y_BUS_PATH, PROPERTIES, "PropertiesChanged")
        .unwrap()
        .with_body(vec![STATUS.into()]);
    assert!(status_may_have_changed(&status));
    let other = Message::signal(A11Y_BUS_PATH, PROPERTIES, "PropertiesChanged")
        .unwrap()
        .with_body(vec!["org.a11y.Bus".into()]);
    assert!(!status_may_have_changed(&other));
    let owner = Message::signal(
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameOwnerChanged",
    )
    .unwrap()
    .with_body(vec![A11Y_BUS.into(), "".into(), ":1.9".into()]);
    assert!(status_may_have_changed(&owner));
    let call = Message::method_call(None, A11Y_BUS_PATH, Some(PROPERTIES), "PropertiesChanged")
        .unwrap()
        .with_body(vec![STATUS.into()]);
    assert!(!status_may_have_changed(&call));
}
