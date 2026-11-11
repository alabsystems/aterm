// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::{PermissionsExt as _, symlink};

#[test]
fn selected_link_runs_the_chosen_app_without_changing_other_path_order() {
    let root = crate::test_scratch_dir("cli-path");
    let home = root.path().join("home");
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let release = root.path().join("aterm.app/Contents/MacOS");
    let dev = root.path().join("aterm (dev).app/Contents/MacOS");
    for (dir, word) in [(&release, "release"), (&dev, "dev")] {
        std::fs::create_dir_all(dir).unwrap();
        let image = dir.join("aterm");
        std::fs::write(&image, format!("#!/bin/sh\nprintf '%s\\n' '{word}'\n")).unwrap();
        std::fs::set_permissions(&image, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let link = bin.join("aterm");
    symlink(dev.join("aterm"), &link).unwrap();
    let reroute = root.path().join("package/reroute");
    let agents = root.path().join("package/agents");
    let reroute = reroute.to_str().unwrap();
    let agents = agents.to_str().unwrap();
    let inherited = format!("{}:/usr/bin::/bin:{}", release.display(), bin.display());
    let (_, path) =
        selected_path(Some(&home), Some(reroute), Some(agents), Some(&inherited)).unwrap();
    assert_eq!(
        path,
        format!(
            "{reroute}:{agents}:{}:{}:/usr/bin::/bin",
            bin.display(),
            release.display()
        )
    );
    let run = |path: &str| {
        let mut child = aterm_uds::spawnfd::command("/bin/sh");
        child
            .args(["-c", "aterm --version"])
            .env_clear()
            .env("PATH", path);
        let output = aterm_uds::deadline::output_until(
            &mut child,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(
        run(&inherited),
        "release\n",
        "negative control: inherited bundle wins"
    );
    assert_eq!(run(&path), "dev\n");
    assert_eq!(
        selected_path(Some(&home), None, None, Some(&inherited))
            .unwrap()
            .1,
        format!("{}:{}:/usr/bin::/bin", bin.display(), release.display()),
        "the upstream Rust reroute escape does not change CLI selection"
    );
    assert_eq!(
        selected_path(Some(&home), Some(reroute), Some(agents), Some(&path))
            .unwrap()
            .1,
        path
    );
    std::fs::remove_file(&link).unwrap();
    symlink(release.join("aterm"), &link).unwrap();
    assert_eq!(
        run(&path),
        "release\n",
        "selection is followed at execution time"
    );
    std::fs::remove_file(&link).unwrap();
    symlink(home.join("missing"), &link).unwrap();
    assert!(selected_path(Some(&home), None, None, Some(&inherited)).is_none());
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, b"not an installed selection").unwrap();
    assert!(selected_path(Some(&home), None, None, Some(&inherited)).is_none());
}
