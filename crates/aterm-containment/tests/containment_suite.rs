// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Combined `aterm-containment` integration test binary.
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

const FRESH_CHILD: &str = "ATERM_CONTAINMENT_FRESH_CHILD";

fn in_fresh_process(module: &str, test: &str, body: impl FnOnce()) {
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
