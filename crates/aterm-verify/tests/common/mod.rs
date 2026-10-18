// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fixture half of the integration suites: the run context over a
//! synthetic repo.

use std::path::Path;

use aterm_verify::checks::Check;
use aterm_verify::{Ctx, EnvSnapshot, Scope};

/// Put an executable `#!/bin/sh` stand-in running `body` at `path`, replacing
/// whatever is there: one file per distinct text, hard-linked into place, so
/// macOS's first-exec assessment is paid once per text and not once per
/// fixture. [`aterm_verify::standin`] says why and how to write the text.
pub fn standin(path: &Path, body: &str) {
    aterm_verify::standin(path, body);
}

/// [`standin`] for a text that cannot be shared (it bakes in a path, or a code):
/// the one constant text `. "$0.body"` is hard-linked into place and `body` is
/// written beside it as a data file the stand-in sources, so every fixture of
/// the contract binary shares one inode, assessed once. [`aterm_verify::standin_body`]
/// says what a sourced body changes (`$0` is the link, `return` and a syntax
/// error read differently) and what it is not for: a stand-in whose changing
/// stamp a tripwire must see, a compiler above all, stays on [`standin`].
pub fn standin_body(path: &Path, body: &str) {
    aterm_verify::standin_body(path, body);
}

/// A run context over the synthetic repo at `root` with the stand-in toolchain
/// `stage2`, the HOME `home` and the checks `checks`.
///
/// THE ENVIRONMENT IS BUILT, NOT CAPTURED: `PATH` from the caller, a `HOME` of
/// the fixture's own, and nothing else, so a variable the developer or the run
/// executing this suite exports cannot change what a fixture decides.
///
/// NO WINDOWS: a fixture's run is a test on a machine a person may be using,
/// so the stages that put a window on the desktop take their `--no-windows`
/// skip; a test that reads one of their rows over stand-in drivers turns it
/// off.
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
