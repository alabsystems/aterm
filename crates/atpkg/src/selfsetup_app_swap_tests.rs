// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::process::ExitStatusExt as _;

fn bundle(path: &Path, marker: &str) {
    std::fs::create_dir_all(path.join("Contents")).unwrap();
    std::fs::write(
        path.join("Contents/Info.plist"),
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
         <key>CFBundleIdentifier</key><string>com.aterm.aterm</string></dict></plist>",
    )
    .unwrap();
    std::fs::write(path.join("marker"), marker).unwrap();
}

fn marker(path: &Path) -> String {
    std::fs::read_to_string(path.join("marker")).unwrap()
}

#[test]
fn app_swap_survives_sigkill_at_both_commit_boundaries() {
    if let Some(root) = std::env::var_os("ATERM_APP_SWAP_TEST_ROOT") {
        let root = PathBuf::from(root);
        if std::env::var("ATERM_APP_SWAP_TEST_CUT").unwrap() == "after" {
            place(&root.join("stage"), &root.join("aterm.app"), &root).unwrap();
        }
        aterm_uds::process::kill_self_now();
    }
    for cut in ["before", "after"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        let installed = root.path().join("aterm.app");
        bundle(&stage, "new");
        bundle(&installed, "old");
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "selfsetup::app_swap::tests::app_swap_survives_sigkill_at_both_commit_boundaries",
                "--nocapture",
            ])
            .env("ATERM_APP_SWAP_TEST_ROOT", root.path())
            .env("ATERM_APP_SWAP_TEST_CUT", cut);
        let result =
            aterm_uds::deadline::output_until(&mut child, Instant::now() + Duration::from_secs(60))
                .unwrap();
        assert_eq!(result.status.signal(), Some(libc::SIGKILL), "{result:?}");
        assert_eq!(
            marker(&installed),
            if cut == "after" { "new" } else { "old" }
        );
        assert_eq!(marker(&stage), if cut == "after" { "old" } else { "new" });
    }
}

#[test]
fn app_swap_promotes_an_absent_destination_and_never_overwrites_a_race() {
    let root = aterm_tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let installed = root.path().join("aterm.app");
    bundle(&stage, "first");
    place(&stage, &installed, root.path()).unwrap();
    assert!(!stage.exists());
    assert_eq!(marker(&installed), "first");
    bundle(&stage, "second");
    assert!(aterm_update_core::app_swap::rename_exclusive(&stage, &installed).is_err());
    assert_eq!(marker(&installed), "first");
    assert_eq!(marker(&stage), "second");
}

#[test]
fn app_swap_refuses_foreign_links_and_aliasing_without_moving_either_app() {
    let root = aterm_tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let installed = root.path().join("aterm.app");
    bundle(&stage, "new");
    assert!(place(&stage, &stage, root.path()).is_err());
    std::os::unix::fs::symlink(&stage, &installed).unwrap();
    assert!(place(&stage, &installed, root.path()).is_err());
    assert!(installed.is_symlink());
    std::fs::remove_file(&installed).unwrap();
    std::fs::create_dir(&installed).unwrap();
    std::fs::write(installed.join("marker"), "foreign").unwrap();
    assert!(place(&stage, &installed, root.path()).is_err());
    assert_eq!(marker(&installed), "foreign");
    assert_eq!(marker(&stage), "new");
}

#[test]
fn app_swap_proves_regular_programs_without_executing_link_targets() {
    let root = aterm_tempfile::tempdir().unwrap();
    let stage = root.path().join("stage");
    let installed = root.path().join("aterm.app");
    bundle(&stage, "new");
    bundle(&installed, "old");
    let programs = installed.join("Contents/MacOS");
    std::fs::create_dir(&programs).unwrap();
    for (program, marker) in [
        (programs.join("aterm"), root.path().join("regular-ran")),
        (root.path().join("outside"), root.path().join("link-ran")),
    ] {
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho ran >'{}'\necho 'aterm 0'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::os::unix::fs::symlink(root.path().join("outside"), programs.join("alias")).unwrap();
    place(&stage, &installed, root.path()).unwrap();
    assert!(root.path().join("regular-ran").exists());
    assert!(!root.path().join("link-ran").exists());
    assert_eq!(marker(&installed), "new");
}
