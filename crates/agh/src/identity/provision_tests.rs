// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

use aterm_uds::private_file::{ELEVATED, PrivateDir, judge_private_dir};

use super::*;

/// An elevated process (as agh takes this thread to be) refuses to make agh's config folder
/// where the folder above it is missing, and makes nothing on the way: what it made would be
/// Administrators', which the user's own processes refuse. Unelevated, the same call makes the
/// folder private; once it is there, the elevated call finds it and makes nothing.
#[test]
fn an_elevated_process_makes_no_folder_above_aghs_config() {
    let root = crate::test_fixtures::scratch_dir("elevated-config");
    let dirs = Dirs::under(root.path());
    let above = root.path().join("config");
    let refused = config::as_elevated_for_tests(|| config_dir(&dirs)).expect_err("refused");
    assert!(refused.why.contains(ELEVATED), "{}", refused.why);
    assert!(!above.exists(), "nothing is made above agh's folder");
    config_dir(&dirs).expect("made unelevated");
    assert_eq!(
        judge_private_dir(&dirs.config).expect("judged"),
        PrivateDir::Private
    );
    config::as_elevated_for_tests(|| config_dir(&dirs)).expect("a private folder that is there");
}
