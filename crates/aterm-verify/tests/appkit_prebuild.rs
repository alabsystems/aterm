// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real Cargo fingerprints for the exact prebuild and bundle target selections.

use aterm_verify::stages::{
    OBJC_DRIVES, REDRAW_CONFORMANCE_BIN, appkit_prebuild_args, objc_drives_build_args,
    redraw_conformance_build_args,
};

#[test]
fn bundle_builds_are_fresh_and_source_changes_invalidate_them() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("examples")).unwrap();
    let mut manifest = format!(
        "[workspace]\n[package]\nname='aterm-gui'\nversion='0.0.0'\nedition='2024'\n\
         [[bin]]\nname='{REDRAW_CONFORMANCE_BIN}'\npath='src/main.rs'\n"
    );
    for drive in OBJC_DRIVES {
        manifest.push_str(&format!(
            "[[example]]\nname='{}'\npath='examples/{}.rs'\n",
            drive.example, drive.example
        ));
        std::fs::write(
            root.join("examples").join(format!("{}.rs", drive.example)),
            "fn main() {}\n",
        )
        .unwrap();
    }
    std::fs::write(root.join("Cargo.toml"), manifest).unwrap();
    let source = root.join("src/main.rs");
    std::fs::write(&source, "fn main() {}\n").unwrap();
    let build = |args: Vec<String>| {
        let mut cmd = aterm_uds::spawnfd::command(env!("CARGO"));
        cmd.current_dir(root)
            .args(args)
            .args(["--offline", "--message-format=json", "--target-dir"])
            .arg(root.join("target.noindex"));
        let began = std::time::Instant::now();
        let output =
            aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60))
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let compiled = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                let event: aterm_json::Value = aterm_json::from_str(line).unwrap();
                event["reason"].as_str() == Some("compiler-artifact")
                    && event["fresh"].as_bool() == Some(false)
            })
            .count();
        (compiled, began.elapsed())
    };
    let before = build(redraw_conformance_build_args());
    assert!(before.0 > 0, "cold negative control must compile");
    // Invalidate the cold control so the production prebuild must do real work.
    std::fs::write(&source, "fn main() { let _ = 1; }\n").unwrap();
    let mut prebuilt = 0;
    for args in appkit_prebuild_args() {
        prebuilt += build(args).0;
    }
    assert!(prebuilt > 0);
    let after = build(redraw_conformance_build_args());
    assert_eq!(after.0, 0, "bundle must reuse the prebuild");
    if cfg!(target_os = "macos") {
        assert_eq!(build(objc_drives_build_args()).0, 0);
    }
    std::fs::write(&source, "fn main() { let _ = 2; }\n").unwrap();
    let changed = build(redraw_conformance_build_args());
    assert!(changed.0 > 0, "changed source must compile again");
    eprintln!("redraw fixture: before={before:?}, after={after:?}, source-change={changed:?}");
}
