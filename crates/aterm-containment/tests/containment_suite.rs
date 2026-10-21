// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Combined `aterm-containment` integration test binary.
//!
//! Tests that initialize the process-wide containment mode `OnceLock` run in
//! an isolated child via [`in_fresh_process`] (`--exact <module>::<fn>` with
//! [`FRESH_CHILD`]). `audit_denial_logged` and `image_anchor_live` remain
//! separate `[[test]]` targets in `Cargo.toml` (`autotests = false`).
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

/// Names, in a re-executed child, the one test that child exists to run.
const FRESH_CHILD: &str = "ATERM_CONTAINMENT_FRESH_CHILD";

/// Re-execute this test binary with `--exact <module>::<test> --test-threads=1`
/// and [`FRESH_CHILD`] set so `body` runs in a fresh process with an unset
/// containment mode `OnceLock`, asserting that libtest ran `1 passed`.
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
