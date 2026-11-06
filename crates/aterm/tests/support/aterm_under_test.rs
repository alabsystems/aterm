// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resolves the `aterm` executable launched by integration test fixtures.

/// Returns the canonical path of the executable named by `ATERM_TEST_EXECUTABLE`, or of
/// `CARGO_BIN_EXE_aterm` when unset.
///
/// Canonical because aterm names itself by its physical path (`current_exe`, `Image::at`), and a
/// release clone reaches its target through a symlink (`target -> target.noindex`): a test that
/// compared a spelling through the link failed only in a release's verify.
pub fn aterm_executable() -> std::path::PathBuf {
    let Some(selected) = std::env::var_os("ATERM_TEST_EXECUTABLE") else {
        let built = env!("CARGO_BIN_EXE_aterm");
        return std::fs::canonicalize(built)
            .unwrap_or_else(|error| panic!("CARGO_BIN_EXE_aterm {built:?}: {error}"));
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
    std::fs::canonicalize(&selected)
        .unwrap_or_else(|error| panic!("ATERM_TEST_EXECUTABLE {selected:?}: {error}"))
}
