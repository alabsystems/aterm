// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[cfg(unix)]
#[test]
fn automatic_sync_refuses_disabled_or_unread_config_before_recovery_and_fetch() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    let roster = scratch.path().join("accounts.toml");
    let home = scratch.path().join("home");
    let state = scratch.path().join("state");
    let state_bytes = b"not a state directory: recovery must not run";
    std::fs::write(&state, state_bytes).unwrap();
    assert!(!accounts_sync_off(None));
    assert!(!accounts_sync_off(Some(&config)));
    assert!(
        crate::supervise::config::SupervisorConfig::read_path(Some(&config))
            .unwrap()
            .0
            .enabled
    );
    aterm_uds::atomic_file::write(&config, b"[harness]\nenabled = true\n", 0o600).unwrap();
    assert!(!accounts_sync_off(Some(&config)));
    let refuse = || {
        let error = sync_accounts_for_init_with(
            &home,
            &state,
            &roster,
            1_800_000_000,
            || panic!("disabled sync reached its source callback"),
            &|_, _| panic!("disabled sync fetched a source"),
        )
        .expect_err("automatic sync must refuse");
        assert_eq!(
            error.to_string(),
            ACCOUNTS_SYNC_OFF,
            "refuse before state recovery"
        );
        assert_eq!(std::fs::read(&state).unwrap(), state_bytes);
        assert!(!home.exists());
        assert!(!roster.exists());
    };
    for text in [
        "[harness]\nenabled = false\n",
        "[harness]\naccounts = 'off'\n",
    ] {
        aterm_uds::atomic_file::write(&config, text.as_bytes(), 0o600).unwrap();
        assert!(crate::supervise::config::SupervisorConfig::read_path(Some(&config)).is_ok());
        refuse();
    }
    aterm_uds::atomic_file::write(&config, b"[harness]\nenabled = true\n", 0o600).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
    assert!(matches!(
        crate::supervise::config::SupervisorConfig::read_path(Some(&config)),
        Err(aterm_types::dirs::AtermConfigReadError::Refused(_))
    ));
    refuse();
    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir(&config).unwrap();
    assert!(crate::supervise::config::SupervisorConfig::read_path(Some(&config)).is_err());
    refuse();
}
