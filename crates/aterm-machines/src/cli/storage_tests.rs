// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::ffi::OsStringExt as _;

#[test]
fn storage_route_preserves_exact_arguments_before_live_controller_dispatch() {
    let path = OsString::from_vec(b"/explicit/bytes\xff/.git".to_vec());
    let args = vec![
        "storage".into(),
        "plan".into(),
        "--git-common-dir".into(),
        path,
    ];
    assert_eq!(storage_route(&args), Some(StorageRoute::Run(&args[1..])));
    assert_eq!(
        storage_route(&["help".into(), "storage".into()]),
        Some(StorageRoute::Help)
    );
    for args in [
        vec![],
        vec!["ls".into()],
        vec!["plan".into(), "--all".into()],
        vec!["--controller".into(), "/controller".into(), "ls".into()],
    ] {
        assert_eq!(storage_route(&args), None);
    }
}

#[test]
fn storage_with_controller_is_refused_before_controller_resolution() {
    let args = vec![
        "--controller".into(),
        "/must-not-be-read/controller".into(),
        "storage".into(),
        "plan".into(),
    ];
    assert_eq!(storage_route(&args), Some(StorageRoute::Controller));
    assert_eq!(main_entry(args), ExitCode::from(2));
}

#[test]
fn storage_help_uses_local_engine_without_controller_setup() {
    assert_eq!(
        main_entry(vec!["storage".into(), "--help".into()]),
        ExitCode::SUCCESS
    );
    assert_eq!(
        main_entry(vec!["help".into(), "storage".into()]),
        ExitCode::SUCCESS
    );
}

#[test]
fn storage_exit_status_keeps_usage_and_failure_distinct() {
    assert_eq!(storage_exit(0), ExitCode::SUCCESS);
    assert_eq!(storage_exit(2), ExitCode::from(2));
    for code in [-1, 1, 3, 256] {
        assert_eq!(storage_exit(code), ExitCode::FAILURE);
    }
}
