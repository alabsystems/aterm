// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_preflight_refusal_does_not_write_identity_needs() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let config = home.join(".config");
    let state = home.join(".state");
    let dirs = agh::config::Dirs::resolve(&home, Some(&config), Some(&state)).unwrap();
    std::fs::create_dir_all(dirs.config.parent().unwrap()).unwrap();
    aterm_uds::private_file::create_private_dir(&dirs.config).unwrap();
    aterm_uds::private_file::write_private(&dirs.config, "agh.toml", b"bogus = true\n").unwrap();
    let before = std::fs::read(dirs.config_file()).unwrap();
    assert!(
        Ctx::from_home_hosts_mode(
            &home,
            Some(&config),
            Some(&state),
            0,
            HostChoice::Agh,
            false
        )
        .is_err()
    );
    assert_eq!(before, std::fs::read(dirs.config_file()).unwrap());
    assert!(!state.exists(), "a view must not raise or clear a need");
    assert_eq!(std::fs::read_dir(&dirs.config).unwrap().count(), 1);
}
