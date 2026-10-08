// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE aterm-containment INTEGRATION SUITE: every integration file but two, as
//! modules of ONE test binary.
//!
//! Most of these files exist to test the crate's ONE-SHOT process state — the
//! containment mode is a `OnceLock` — so each used to be its own binary to get
//! a fresh process. They still get one: [`in_fresh_process`] re-executes THIS
//! binary filtered to exactly the calling test, with [`FRESH_CHILD`] naming it,
//! and the body runs only in that child. The parent never touches the mode, so
//! `claimant_census`, which needs no fresh process, runs in it beside them.
//!
//! Two files stay their own `[[test]]` (see `Cargo.toml`): `audit_denial_logged`
//! installs the process-global logger and counts its records exactly, and
//! `image_anchor_live` copies its own binary and re-enters it by `--exact`.
//!
//! `autotests = false` in `Cargo.toml`: a `tests/*.rs` file that is neither a
//! `mod` line here nor a `[[test]]` of its own NEVER COMPILES.

/// Names, in a re-executed child, the one test that child exists to run.
const FRESH_CHILD: &str = "ATERM_CONTAINMENT_FRESH_CHILD";

/// Run `body` in a FRESH PROCESS: the mode `OnceLock` unset and no sibling
/// test in the process.
///
/// Called from a `#[test]` as `in_fresh_process(module_path!(), "<fn name>", ||
/// …)`. In the parent it re-executes this binary as `--exact <module>::<fn>
/// --test-threads=1` with [`FRESH_CHILD`] set to that name, and passes only if
/// the child REPORTS exactly one passed test by that name — so a misspelt name
/// (which libtest answers with a green "0 passed") fails here instead of
/// passing without running anything. In the child the variable matches and the
/// body runs.
fn in_fresh_process(module: &str, test: &str, body: impl FnOnce()) {
    // libtest names a test by its module path WITHOUT the crate segment.
    let module = module.split_once("::").map_or("", |(_, rest)| rest);
    let name = if module.is_empty() {
        test.to_string()
    } else {
        format!("{module}::{test}")
    };
    if std::env::var(FRESH_CHILD).as_deref() == Ok(name.as_str()) {
        body();
        return;
    }
    let exe = std::env::current_exe().expect("the test binary's own path");
    let out = std::process::Command::new(exe)
        .args(["--exact", &name, "--test-threads=1"])
        .env(FRESH_CHILD, &name)
        .output()
        .expect("re-execute the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "{name} failed in its fresh process:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.contains(&format!("test {name} ... ok"))
            && stdout.contains("test result: ok. 1 passed; 0 failed"),
        "the fresh process did not run exactly `{name}` — a misnamed --exact \
         filter passes vacuously:\n{stdout}\n{stderr}"
    );
}

mod claimant_census;
mod init_from_flag_default;
mod init_from_flag_rejects_unparsable;
mod init_from_flag_with_value;
mod init_mode_is_write_once;
mod uninitialized_mode;
