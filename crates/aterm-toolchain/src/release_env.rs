// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release and its background lane remove the same inherited build overrides.

#[must_use]
pub fn clears(name: &str) -> bool {
    const INERT: [&str; 8] = [
        "CARGO_BUILD_JOBS",
        "CARGO_MAKEFLAGS",
        "CARGO_INCREMENTAL",
        "CARGO_LOG",
        "RUST_BACKTRACE",
        "RUST_LIB_BACKTRACE",
        "RUST_LOG",
        "RUST_TEST_THREADS",
    ];
    const NATIVE: [&str; 8] = [
        "CC", "CXX", "AR", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "ARFLAGS",
    ];
    if name.starts_with("RUST") || name.starts_with("CARGO") {
        return !INERT.contains(&name) && !name.starts_with("CARGO_TERM_");
    }
    if matches!(
        name,
        "MACOSX_DEPLOYMENT_TARGET" | "SDKROOT" | "CRATE_CC_NO_DEFAULTS"
    ) {
        return true;
    }
    let bare = name
        .strip_prefix("TARGET_")
        .or_else(|| name.strip_prefix("HOST_"))
        .unwrap_or(name);
    NATIVE
        .iter()
        .any(|n| bare == *n || bare.strip_prefix(n).is_some_and(|r| r.starts_with('_')))
}

#[must_use]
pub fn launcher_env(name: &str) -> bool {
    matches!(
        name,
        "CARGO" | "CARGO_PRIMARY_PACKAGE" | "TRUST_TARGO_FRONTEND" | "DYLD_FALLBACK_LIBRARY_PATH"
    ) || ["CARGO_MANIFEST_", "CARGO_PKG_", "TARGO_"]
        .iter()
        .any(|p| name.starts_with(p))
}

#[cfg(test)]
#[path = "release_env_tests.rs"]
mod tests;
