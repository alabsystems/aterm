// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The explicitly selected macOS CLI, separate from a window's physical bundle.

use std::path::Path;

/// Prefers the installed app selected by `~/.local/bin/aterm` behind the managed reroute and agent directories.
#[must_use]
pub fn selected_path(
    home: Option<&Path>,
    reroute: Option<&str>,
    agents: Option<&str>,
    inherited: Option<&str>,
) -> Option<(String, String)> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = home?.join(".local/bin");
        let link = dir.join("aterm");
        if !std::fs::symlink_metadata(&link)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
        let target = link.canonicalize().ok()?;
        let metadata = target.metadata().ok()?;
        let macos = target.parent()?;
        let app = macos.parent()?.parent()?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || target.file_name()? != "aterm"
            || !macos.ends_with("Contents/MacOS")
            || app.extension()? != "app"
        {
            return None;
        }
        let selected = dir.to_str()?;
        let mut front = Vec::new();
        for entry in [reroute, agents, Some(selected)].into_iter().flatten() {
            if !front.contains(&entry) {
                front.push(entry);
            }
        }
        let mut entries = front.clone();
        entries.extend(
            inherited
                .filter(|path| !path.is_empty())
                .into_iter()
                .flat_map(|path| path.split(':'))
                .filter(|entry| !front.contains(entry)),
        );
        Some(("PATH".into(), entries.join(":")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, reroute, agents, inherited);
        None
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    #[test]
    fn selected_link_runs_the_chosen_app_without_changing_other_path_order() {
        let root = aterm_tempfile::tempdir().unwrap();
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
}
