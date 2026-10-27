// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A killed clone's staging is swept once its owner is gone; a live clone's, a markerless one,
//! a link and a published clone are left.

use super::*;

/// This process's account, at home in `home`.
fn me(home: &Path) -> User {
    let sys = Live::new();
    User {
        name: "me".into(),
        uid: sys.euid(),
        gid: 20,
        home: home.to_string_lossy().into_owned(),
        shell: String::new(),
        admin: false,
    }
}

/// A staging directory as a clone leaves it mid-way: its owner mark, and a partial clone.
fn staging(home: &Path, suffix: &str) -> PathBuf {
    let dir = home.join(format!("{STAGING}{suffix}"));
    std::fs::create_dir_all(dir.join("repo").join(".git")).unwrap();
    std::fs::write(dir.join("repo").join("partial"), b"half a pack").unwrap();
    std::fs::write(dir.join(OWNER), b"").unwrap();
    dir
}

#[test]
fn the_staging_prefix_is_the_one_scratch_names_a_clone_by() {
    let home = aterm_tempfile::tempdir().unwrap();
    let sys = Live::new();
    let mut scratch = Scratch::new_in(&sys, home.path(), "github-clone").unwrap();
    let name = scratch
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with(STAGING), "{name}");
    scratch.close().unwrap();
}

#[test]
fn a_killed_clones_staging_is_swept_and_a_live_ones_stays() {
    let home = aterm_tempfile::tempdir().unwrap();
    let sys = Live::new();
    let who = me(home.path());
    let killed = staging(home.path(), "killed");
    let live = staging(home.path(), "live");
    // The live clone's process holds its mark, as `clone_one_with` does while it runs.
    let held = sys.exclusive(&live.join(OWNER), Duration::ZERO).unwrap();
    let markerless = home.path().join(format!("{STAGING}older"));
    std::fs::create_dir_all(markerless.join("repo")).unwrap();
    let published = home.path().join("aterm");
    std::fs::create_dir_all(published.join(".git")).unwrap();
    std::fs::write(published.join(OWNER), b"").unwrap();
    let elsewhere = aterm_tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join(OWNER), b"").unwrap();
    let link = home.path().join(format!("{STAGING}link"));
    std::os::unix::fs::symlink(elsewhere.path(), &link).unwrap();

    assert_eq!(
        sweep_staging(&sys, &who),
        1,
        "only the killed clone's staging goes"
    );
    assert!(!killed.exists(), "the killed clone's staging is swept");
    assert!(
        live.join("repo").join("partial").exists(),
        "a live clone's staging stays"
    );
    assert!(
        markerless.exists(),
        "a staging with no mark proves no owner gone"
    );
    assert!(
        published.join(".git").exists(),
        "a published clone is never staging"
    );
    assert!(
        elsewhere.path().join(OWNER).exists(),
        "a link is never entered"
    );
    assert!(link.symlink_metadata().is_ok(), "nor removed");

    drop(held);
    assert_eq!(
        sweep_staging(&sys, &who),
        1,
        "once its owner is gone, it goes too"
    );
    assert!(!live.exists());
}

#[test]
fn another_accounts_process_sweeps_nothing() {
    let home = aterm_tempfile::tempdir().unwrap();
    let sys = Live::new();
    let mut who = me(home.path());
    who.uid = who.uid.wrapping_add(1);
    let killed = staging(home.path(), "killed");
    assert_eq!(sweep_staging(&sys, &who), 0);
    assert!(killed.exists());
}
