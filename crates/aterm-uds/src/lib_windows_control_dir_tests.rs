// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::windows_control_dir;
use std::path::PathBuf;

/// An elevated process resolves its own directory beside the shared one,
/// under the same temp base, so the two never share `latest`, a token or a
/// socket; the base keeps its order (`%TMP%`, `%TEMP%`, `%LOCALAPPDATA%\Temp`).
#[test]
fn an_elevated_process_gets_a_control_dir_of_its_own() {
    let os = |s: &str| Some(std::ffi::OsString::from(s));
    let env = [os(r"C:\t"), os(r"C:\u"), os(r"C:\l")];
    assert_eq!(
        windows_control_dir(env.clone(), false),
        Some(PathBuf::from(r"C:\t\aterm"))
    );
    assert_eq!(
        windows_control_dir(env, true),
        Some(PathBuf::from(r"C:\t\aterm-elevated"))
    );
    assert_eq!(
        windows_control_dir([os(""), os(r"C:\u"), None], true),
        Some(PathBuf::from(r"C:\u\aterm-elevated"))
    );
    assert_eq!(
        windows_control_dir([None, None, os(r"C:\l")], false),
        Some(PathBuf::from(r"C:\l\Temp\aterm"))
    );
    assert_eq!(windows_control_dir([None, os(""), None], true), None);
}
