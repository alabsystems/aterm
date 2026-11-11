// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

#[test]
fn first_grant_never_overwrites_a_competing_creator() {
    let root = crate::test_fixtures::scratch_dir("grant-competing-creator");
    let dirs = crate::config::Dirs::under(root.path());
    let existing = "# another editor's file\nvault = true\n";
    crate::test_fixtures::write_config(&dirs, existing);
    let candidate = "vault = true\n[[writers]]\ncaller = 'atpkg-publish'\n\
                     repos = ['alabsystems/aterm']\n";
    let error = super::create_grant_config(&dirs, candidate).expect_err("the competing file wins");
    assert_eq!(error.kind, crate::Kind::Refused);
    assert_eq!(
        std::fs::read_to_string(dirs.config_file()).unwrap(),
        existing
    );
}
