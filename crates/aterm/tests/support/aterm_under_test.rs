// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resolves the `aterm` executable launched by integration test fixtures.

/// Returns the executable named by `ATERM_TEST_EXECUTABLE`, or `CARGO_BIN_EXE_aterm` when unset.
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
