// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fixture helpers and run context for `aterm-verify` integration tests.

use std::path::Path;

use aterm_verify::checks::Check;
use aterm_verify::{Ctx, EnvSnapshot, Scope};

/// Put an executable `#!/bin/sh` stand-in running `body` at `path`.
pub fn standin(path: &Path, body: &str) {
    aterm_verify::standin(path, body);
}

/// Put a stand-in at `path` that sources `body` from an adjacent `.body` file.
pub fn standin_body(path: &Path, body: &str) {
    aterm_verify::standin_body(path, body);
}

/// Build a run context over the synthetic repo at `root`.
pub fn fixture_ctx(
    root: &Path,
    stage2: &Path,
    scratch: &Path,
    home: &Path,
    checks: &[Check],
    scope: Scope,
) -> Ctx {
    let env = EnvSnapshot {
        path: std::env::var_os("PATH").unwrap_or_default(),
        home: home.to_path_buf(),
        trust_stage2_bin: Some(stage2.to_path_buf()),
        ..EnvSnapshot::default()
    };
    Ctx::new(root.to_path_buf(), scope, env, scratch.to_path_buf())
        .with_checks(checks)
        .with_no_windows(true)
}
