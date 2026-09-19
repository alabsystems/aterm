// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The release version lives in THREE places and only one of them is bumped by
//! `pub bump`: `Cargo.toml`'s `[workspace.package] version`, `publish/config.sh`'s
//! `VERSION_DEFAULT`, and the `aterm X.Y.0` assertion inside that file's
//! `CHECK_CMD_DEFAULT`.
//!
//! `publish/version-sites.sh` has owned all three since 2026-08-26 — but it is
//! RUN by the export transform, which is to say at `pub stage` time, which is to
//! say at the start of a release. A stale pair committed today is not reported
//! until the day somebody tries to ship, and then it costs a failed stage (and,
//! for site 3, a failed stage discovered only after the slow anonymous
//! public-clone build).
//!
//! So the check moves to where the mistake is made. This test runs the same
//! script, in check mode, on this checkout: a version bump that forgets
//! `publish/config.sh` is red in the ordinary suite, in the commit that made it,
//! not in the release that trips over it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root is two levels above crates/aterm-release")
        .to_path_buf()
}

#[test]
fn the_three_version_sites_agree() {
    let root = repo_root();
    let script = root.join("publish/version-sites.sh");
    assert!(
        script.is_file(),
        "publish/version-sites.sh is missing — the three version sites then have no owner at all"
    );
    let out = Command::new("bash")
        .arg(&script)
        .arg("--quiet")
        .arg(&root)
        .output()
        .expect("run publish/version-sites.sh");
    assert!(
        out.status.success(),
        "the release version disagrees across its sites:\n{}{}\n\
         fix:  publish/version-sites.sh --fix   (commit publish/config.sh WITH Cargo.toml)",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
