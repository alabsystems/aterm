// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn temp_layout(label: &str) -> crate::testdir::ScratchLayout {
    crate::testdir::layout(&format!("atpkg-landing-{label}-"))
}

#[test]
fn the_sweep_removes_every_marker_and_the_directory() {
    let l = temp_layout("sweep");
    let dir = l.landing_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let live = format!(
        "atpkg-landing-v1 build=2026091702 pid={} from=2026091601 version=2.1.274 \
         from_version=2.1.273\n",
        std::process::id()
    );
    std::fs::write(dir.join("claude"), live).unwrap();
    std::fs::write(dir.join("codex"), b"junk\n").unwrap();
    std::fs::write(dir.join(".claude.tmp-4242"), b"").unwrap();
    let optin = l.prefix.join("optin");
    std::fs::create_dir_all(&optin).unwrap();
    std::fs::write(optin.join("vendorx"), b"# opted in\n").unwrap();
    sweep(&l);
    assert!(!dir.exists(), "landing/ is gone");
    assert!(!optin.exists(), "optin/ is gone");
    sweep(&l);
    assert!(!dir.exists());
    let _ = std::fs::remove_dir_all(&l.prefix);
}

#[test]
fn the_sweep_never_follows_a_link_at_landing() {
    let l = temp_layout("sweep-link");
    let elsewhere = l.prefix.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("claude"), b"keep me\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, l.landing_dir()).unwrap();
    sweep(&l);
    assert!(
        std::fs::symlink_metadata(l.landing_dir()).is_err(),
        "unlinked"
    );
    assert_eq!(
        std::fs::read(elsewhere.join("claude")).unwrap(),
        b"keep me\n",
        "the link's target is not ours"
    );
    let _ = std::fs::remove_dir_all(&l.prefix);
}
