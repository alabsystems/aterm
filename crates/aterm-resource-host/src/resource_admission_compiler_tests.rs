// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn selected_sdk_precedes_global_tools_and_pairs_both_compilers() {
    let mut environment = vec![
        ("PATH".into(), "/global/bin:/usr/bin".into()),
        ("KEEP".into(), "value".into()),
    ];
    paired_environment(Path::new("/signed/project/sdk"), &mut environment).unwrap();
    for (key, value) in [
        ("RUSTC", "/signed/project/sdk/bin/trustc"),
        ("RUSTDOC", "/signed/project/sdk/bin/trustdoc"),
        ("PATH", "/signed/project/sdk/bin:/global/bin:/usr/bin"),
        ("KEEP", "value"),
    ] {
        assert_eq!(
            environment
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, v)| v.as_os_str()),
            Some(std::ffi::OsStr::new(value))
        );
    }
}
#[test]
fn caller_loader_code_is_not_a_signed_native_recipe() {
    for argument in [
        "--config",
        "--config=build.rustc=foreign",
        "-C",
        "-C/foreign",
    ] {
        assert!(no_cli_overrides(&[argument.as_bytes().to_vec()]).is_err());
    }
    assert!(no_cli_overrides(&[b"--unverified".to_vec(), b"check".to_vec()]).is_ok());
    for key in [
        b"LD_PRELOAD".as_slice(),
        b"LD_LIBRARY_PATH",
        b"DYLD_INSERT_LIBRARIES",
        b"DYLD_LIBRARY_PATH",
    ] {
        assert!(no_loader_overrides(&[(key.to_vec(), b"/foreign/code".to_vec())]).is_err());
        assert!(no_loader_overrides(&[(key.to_vec(), Vec::new())]).is_err());
    }
    assert!(no_loader_overrides(&[(b"ORDINARY_PATH".to_vec(), b"/ordinary".to_vec())]).is_ok());
}
