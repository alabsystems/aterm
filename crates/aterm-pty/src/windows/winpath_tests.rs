// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::union_paths;
use std::ffi::OsString;

fn os(s: &str) -> OsString {
    OsString::from(s)
}

#[test]
fn union_dedupes_and_preserves_order() {
    let reg = os(r"C:\Windows\system32;C:\Windows;C:\tools");
    let inherited = os(r"c:\windows\system32\;C:\session-only;C:\TOOLS");
    let merged = union_paths(&reg, Some(&inherited))
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        merged,
        r"C:\Windows\system32;C:\Windows;C:\tools;C:\session-only"
    );
}

#[test]
fn union_keeps_the_inherited_lead_in_front() {
    let reg = os(r"C:\Windows\system32;C:\Users\u\AppData\Local\Programs\aterm");
    let inherited = os(
        r"C:\Users\u\pkg\agents;D:\scratch\aterm;;C:\WINDOWS\system32;C:\session-only;D:\scratch\aterm",
    );
    assert_eq!(
        union_paths(&reg, Some(&inherited)).to_string_lossy(),
        r"C:\Users\u\pkg\agents;D:\scratch\aterm;C:\Windows\system32;C:\Users\u\AppData\Local\Programs\aterm;C:\session-only"
    );
    assert_eq!(
        union_paths(&reg, Some(&os(r"D:\only"))).to_string_lossy(),
        r"D:\only;C:\Windows\system32;C:\Users\u\AppData\Local\Programs\aterm"
    );
}

#[test]
fn union_registry_only() {
    let reg = os(r"C:\a;;C:\b;");
    assert_eq!(union_paths(&reg, None).to_string_lossy(), r"C:\a;C:\b");
}
