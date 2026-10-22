// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one `aterm` every launch fixture runs (`launch_isolation`, the fabric
//! harness): a site naming `CARGO_BIN_EXE_aterm` itself skips a collector's pick.

/// The executable chosen by a private external test collector
/// (`ATERM_TEST_EXECUTABLE`), else this package's own `aterm`. An explicit
/// artifact must be absolute and a regular file; an invalid selection is a
/// broken fixture, never permission to launch a different build. The collector
/// attests the owned artifact's hash before and after the test. The variable is
/// test-only: `launch_isolation::apply` strips it from children.
pub fn aterm_executable() -> std::path::PathBuf {
    let Some(selected) = std::env::var_os("ATERM_TEST_EXECUTABLE") else {
        return std::path::PathBuf::from(env!("CARGO_BIN_EXE_aterm"));
    };
    let selected = std::path::PathBuf::from(selected);
    assert!(
        selected.is_absolute(),
        "ATERM_TEST_EXECUTABLE must name an absolute owned artifact: {selected:?}"
    );
    let metadata = std::fs::metadata(&selected)
        .unwrap_or_else(|error| panic!("ATERM_TEST_EXECUTABLE {selected:?}: {error}"));
    assert!(
        metadata.is_file(),
        "ATERM_TEST_EXECUTABLE must name a regular file: {selected:?}"
    );
    selected
}
