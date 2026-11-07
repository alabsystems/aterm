// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only maintained entry for a real cached host/helper integration. No
//! production role or environment variable can select this private-store seam.
use std::{ffi::OsString, process::ExitCode};

/// Dispatch the standalone native fixture before any libtest argument parser.
/// This exists only on the currently implemented host-member platform in tests.
pub fn entry(args: &[OsString]) -> ExitCode {
    if args.first().is_some_and(|v| v == "__resource-member-v3") {
        let image = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .unwrap();
        let path = image
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .unwrap();
        let store = trust_resource_provenance::Store::open_at(path).unwrap();
        return crate::resource_proxy::entry(
            args,
            &mut crate::resource_proxy_backend::NativeBackend::for_store(store),
        )
        .unwrap();
    }
    crate::actor::member_fixture(args)
}
