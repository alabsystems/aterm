// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared live-conformance artifact resolution.

use std::path::PathBuf;

/// The release `aterm` a measuring row times, or `None` (said on stderr) when
/// none was handed over.
///
/// The MEASURE tier's `measuring tests` stage passes the release bundle's
/// `Contents/MacOS/aterm` it was handed (`tools/verify.sh --measure <APP>`) in
/// `ATERM_PAINT_BIN` / `ATERM_SPIN_BIN` (`aterm-verify`'s `measuring_cmd`). A
/// plain `targo test` names neither and its rows do not run, since they time a
/// live window against a release build. By hand, name a release `aterm` in the
/// variable.
pub(crate) fn release_bin(overrides: &[&str]) -> Option<PathBuf> {
    for var in overrides {
        if let Some(path) = std::env::var_os(var).map(PathBuf::from) {
            assert!(path.is_file(), "{var}={} does not exist", path.display());
            return Some(path);
        }
    }
    eprintln!(
        "not measured: no {} — this row times the release aterm the MEASURE tier \
         (tools/verify.sh --measure <APP>) hands it",
        overrides.join(" or ")
    );
    None
}
