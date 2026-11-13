// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn shell_text_and_arbitrary_arguments_never_enter_roles() {
    let words = |args: &[&str]| {
        args.iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        role("bash", &words(&["bash", "-c", "sensitive payload"])),
        "bash"
    );
    assert_eq!(
        role("agh", &words(&["agh", "api", "sensitive payload"])),
        "agh api"
    );
    assert_eq!(
        role("aterm", &words(&["aterm", "ship", "release"])),
        "aterm ship release"
    );
}
