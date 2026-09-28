// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `tools/verify.sh`'s toolchain pin, driven as the shell script it is.
//!
//! A checkout with NO `rust-toolchain.toml` is refused (exit 3, COULD NOT RUN)
//! before any toolchain is adopted — decided 2026-09-25. Until then it printed
//! "NO TOOLCHAIN PIN … UNAUTHENTICATED" and ran anyway: with no pin the
//! candidate check answers "may be used" for every directory holding a
//! `targo`, so the gate was built and run by whichever one it met first and its
//! verdict was attributed to a pin nobody read.
//!
//! Each fixture is a scratch root holding a copy of the real script and a
//! recording `targo` on `PATH`. The script's discovery also probes the rustup
//! home and the atpkg store under `$HOME`, so `HOME` and `RUSTUP_HOME` point
//! into the scratch root: nothing on the developer's machine is reachable.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/aterm-verify sits two levels under the root")
        .to_path_buf()
}

struct Fixture {
    root: PathBuf,
    record: PathBuf,
}

impl Fixture {
    fn new(tag: &str, pin: Option<&str>) -> Self {
        let root = aterm_verify::mktemp_dir(tag).expect("scratch root");
        fs::create_dir_all(root.join("tools")).expect("tools dir");
        fs::copy(
            workspace_root().join("tools/verify.sh"),
            root.join("tools/verify.sh"),
        )
        .expect("copy verify.sh");
        if let Some(body) = pin {
            fs::write(root.join("rust-toolchain.toml"), body).expect("pin");
        }
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("bin dir");
        fs::create_dir_all(root.join("home")).expect("home dir");
        let record = root.join("targo-was-run");
        // Records that it ran, then fails the gate build — which the script
        // reports as COULD NOT RUN too. Only the record tells the two apart.
        let targo = bin.join("targo");
        fs::write(
            &targo,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", record.display()),
        )
        .expect("targo");
        fs::set_permissions(&targo, fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { root, record }
    }

    fn run(&self) -> (i32, String) {
        let out = Command::new("/bin/bash")
            .arg(self.root.join("tools/verify.sh"))
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("HOME", self.root.join("home"))
            .env("RUSTUP_HOME", self.root.join("home/.rustup"))
            .output()
            .expect("bash runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn targo_ran(&self) -> bool {
        self.record.exists()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_checkout_with_no_toolchain_pin_is_refused_before_any_toolchain_runs() {
    let f = Fixture::new("verify-sh-no-pin", None);
    let (code, stderr) = f.run();
    assert_eq!(code, 3, "a pinless checkout is COULD NOT RUN:\n{stderr}");
    assert!(
        stderr.contains("NO TOOLCHAIN PIN") && stderr.contains("COULD NOT RUN"),
        "the refusal names its reason:\n{stderr}"
    );
    assert!(
        !f.targo_ran(),
        "no toolchain may be adopted and run when there is no pin to authenticate it:\n{stderr}"
    );
}

/// The negative control: the same fixture with a pin DOES reach the recording
/// `targo`, so the assertion above is about the pin and not about a fixture
/// whose `targo` could never have been found.
#[test]
fn the_same_fixture_with_a_pin_reaches_its_toolchain() {
    let f = Fixture::new(
        "verify-sh-stable-pin",
        Some("[toolchain]\nchannel = \"stable\"\n"),
    );
    let (code, stderr) = f.run();
    assert!(
        f.targo_ran(),
        "a pinned upstream channel adopts the targo on PATH:\n{stderr}"
    );
    assert!(!stderr.contains("NO TOOLCHAIN PIN"), "{stderr}");
    assert_eq!(
        code, 3,
        "the recording targo fails the gate build:\n{stderr}"
    );
}

/// Its neighbour, unchanged: a pin FILE that names no channel was already
/// refused, and still is, before any toolchain runs.
#[test]
fn a_pin_file_that_names_no_channel_is_refused_before_any_toolchain_runs() {
    let f = Fixture::new("verify-sh-empty-pin", Some("[toolchain]\n"));
    let (code, stderr) = f.run();
    assert_eq!(code, 3, "{stderr}");
    assert!(stderr.contains("names no channel"), "{stderr}");
    assert!(!f.targo_ran(), "{stderr}");
}
