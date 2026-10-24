// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The harnesses' build stamp, which `aos_gpu` prints and `lab aos-gpu-bench` gates on: the commit `ATERM_BENCH_GIT_COMMIT` (12 hex digits, `-dirty` on a dirty tree, `unknown` without git), the toolchain line `ATERM_BENCH_TOOLCHAIN` (the first line of `$RUSTC -vV`) and the build profile `ATERM_BENCH_PROFILE` (cargo's `PROFILE` and `OPT_LEVEL`).

#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: a build script"
)]

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Only the `aos-gpu` harness reads the stamp: without the feature nothing is stamped, so a moved HEAD or an edit rebuilds no aterm-bench dependent.
    if std::env::var_os("CARGO_FEATURE_AOS_GPU").is_none() {
        return;
    }
    println!("cargo:rerun-if-env-changed=ATERM_BUILD_GIT_COMMIT");
    let pinned = std::env::var("ATERM_BUILD_GIT_COMMIT")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let commit = match pinned {
        Some(commit) => commit,
        None => {
            // Re-stamp when HEAD moves or a tracked file changes, so the dirty flag is that of the source compiled.
            for path in git_watch_paths().into_iter().chain(tracked_files()) {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            let commit = run("git", &["rev-parse", "--short=12", "HEAD"])
                .unwrap_or_else(|| "unknown".into());
            let dirty = run("git", &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            if commit != "unknown" && dirty {
                format!("{commit}-dirty")
            } else {
                commit
            }
        }
    };
    println!("cargo:rustc-env=ATERM_BENCH_GIT_COMMIT={commit}");
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let toolchain = run(&rustc, &["-vV"])
        .and_then(|v| v.lines().next().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ATERM_BENCH_TOOLCHAIN={toolchain}");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    let opt = std::env::var("OPT_LEVEL").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=ATERM_BENCH_PROFILE={profile} opt-level={opt}");
}

/// Run `cmd args…`, its trimmed stdout when it succeeded with some, else `None`.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A path git prints, made absolute: an older git without `--path-format=absolute` answers relative to this script's working directory (the package root).
fn git_abs_path(args: &[&str]) -> Option<PathBuf> {
    let mut with_format = vec!["rev-parse", "--path-format=absolute"];
    with_format.extend_from_slice(args);
    let value = run("git", &with_format).or_else(|| {
        let mut plain = vec!["rev-parse"];
        plain.extend_from_slice(args);
        run("git", &plain)
    })?;
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

/// The existing git files whose change may stale the commit stamp (cargo reruns a build script on every build while a watched path is missing), resolved through git so a linked worktree and the main checkout both work.
fn git_watch_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Some(git_dir) = run("git", &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) else {
        return paths;
    };
    let common_dir = git_abs_path(&["--git-common-dir"]).unwrap_or_else(|| git_dir.clone());

    paths.push(git_dir.join("HEAD"));

    // Watch the loose ref HEAD names or, while it is packed and has no loose file, the nearest existing directory below `refs/`, which the commit that writes it changes.
    if let Some(reference) = run("git", &["symbolic-ref", "--quiet", "HEAD"])
        && let (Some(ref_path), Some(refs_root)) = (
            git_abs_path(&["--git-path", &reference]),
            git_abs_path(&["--git-path", "refs"]),
        )
        && let Some(watch) = ref_path
            .ancestors()
            .take_while(|p| p.starts_with(&refs_root))
            .find(|p| p.exists())
    {
        paths.push(watch.to_path_buf());
    }

    // A packed ref changes in `packed-refs`, and a reftable-backend repo keeps every ref below `reftable/`.
    paths.push(common_dir.join("packed-refs"));
    paths.push(common_dir.join("reftable"));

    paths.retain(|p| p.exists());
    paths.sort();
    paths.dedup();
    paths
}

/// Every existing regular file git tracks in this checkout and its submodules, absolute and with no newline in its name, so an edit to any of them reruns the script.
fn tracked_files() -> Vec<PathBuf> {
    let Some(top) = run("git", &["rev-parse", "--show-toplevel"]).map(PathBuf::from) else {
        return Vec::new();
    };
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(&top)
        .args(["ls-files", "-z", "--recurse-submodules"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|name| !name.is_empty() && !name.contains('\n'))
        .map(|name| top.join(name))
        .filter(|path| path.is_file())
        .collect()
}
