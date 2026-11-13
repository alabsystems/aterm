// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Absent `[swarm]` and `[swarm] enabled = true` leave swarm participation on;
/// `[swarm] enabled = false` turns it off; malformed sections or keys fail rather than
/// silently defaulting on.
/// NEGATIVE CONTROL: ignoring `[swarm]` would return `Ok(true)` for `[swarm] enabled = false`.
#[test]
fn swarm_table_parses_default_true_explicit_false_and_rejects_bad_shapes() {
    assert_eq!(enabled_in_toml(""), Ok(true));
    assert_eq!(enabled_in_toml("[commons]\nenabled = true\n"), Ok(true));
    assert_eq!(enabled_in_toml("[swarm]\n"), Ok(true));
    assert_eq!(enabled_in_toml("[swarm]\nenabled = true\n"), Ok(true));
    assert_eq!(enabled_in_toml("[swarm]\nenabled = false\n"), Ok(false));
    assert!(enabled_in_toml("swarm = false\n").is_err());
    assert!(enabled_in_toml("[swarm]\nenabled = \"off\"\n").is_err());
    assert!(enabled_in_toml("[swarm]\nother = true\n").is_err());
}

#[cfg(unix)]
#[test]
fn enabled_at_reads_missing_as_on_and_follows_a_symlink_to_an_owned_file() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let config = scratch.path().join("aterm.toml");
    assert_eq!(enabled_at(&config), Ok(true), "missing file defaults on");
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = false\n");
    assert_eq!(enabled_at(&config), Ok(false));
    let link = scratch.path().join("linked.toml");
    std::os::unix::fs::symlink(&config, &link).unwrap();
    assert_eq!(enabled_at(&link), Ok(false), "a symlink is followed");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
    let open = enabled_at(&link).expect_err("a file others can write is refused");
    assert!(unreadable(&open).starts_with("aterm cannot read its settings file ("));
}

/// A symlinked `aterm.toml` (a dotfiles checkout, home-manager) reads as the window reads it:
/// the link is followed to a file of this user's, so `[swarm] enabled` reads what the window
/// reads, and Commons, the vault subscriber and its approver follow it. An edit of the file
/// the link names moves the switch's stamp, so a loop hears it.
/// NEGATIVE CONTROL: the `O_NOFOLLOW` read refuses the link, and every swarm reader in
/// aterm-secrets holds Commons and the vault for good while the window asks macOS.
#[cfg(unix)]
#[test]
fn a_symlinked_aterm_toml_reads_as_the_window_reads_it() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join(".config").join("aterm");
    std::fs::create_dir_all(&dir).unwrap();
    let dotfiles = scratch.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let target = dotfiles.join("aterm.toml");
    crate::fsx::write_owned_in_place(&target, "[swarm]\nenabled = true\n");
    std::os::unix::fs::symlink(&target, dir.join("aterm.toml")).unwrap();
    let mut switch = Switch::new(&home);
    assert_eq!(switch.enabled(), Ok(true), "the window reads sharing on");
    crate::fsx::write_owned_in_place(&target, "[swarm]\nenabled = false # turned off\n");
    assert_eq!(
        switch.enabled(),
        Ok(false),
        "an edit of the linked file is heard"
    );
}

/// A switch that cannot read `aterm.toml` says so in plain words once per change, never once
/// per turn of the loop that asks it.
#[cfg(unix)]
#[test]
fn an_unreadable_switch_says_so_once_per_change() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join(".config").join("aterm");
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("aterm.toml");
    let said = std::cell::RefCell::new(Vec::<String>::new());
    let log = |line: &str| said.borrow_mut().push(line.to_owned());
    let mut switch = Switch::new(&home);
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = 1\n");
    for _ in 0..5 {
        assert!(switch.enabled_saying(&log).is_err());
    }
    crate::fsx::write_owned_in_place(&config, "[swarm]\nother = true\n");
    for _ in 0..5 {
        assert!(switch.enabled_saying(&log).is_err());
    }
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = true\n");
    assert_eq!(switch.enabled_saying(&log), Ok(true));
    let said = said.into_inner();
    assert_eq!(said.len(), 2, "once per change: {said:?}");
    assert!(
        said.iter()
            .all(|line| line.starts_with("aterm cannot read its settings file (")),
        "{said:?}"
    );
}

/// A loop's switch follows each edit, replace and removal of `aterm.toml`, and says so where
/// it cannot read `[swarm]`.
/// NEGATIVE CONTROL: a switch that keeps its first answer misses the edit to `false`.
#[cfg(unix)]
#[test]
fn a_loops_switch_follows_each_change_of_the_file() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join(".config").join("aterm");
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("aterm.toml");
    let mut switch = Switch::new(&home);
    assert_eq!(switch.enabled(), Ok(true), "no file: on");
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = false\n");
    assert_eq!(switch.enabled(), Ok(false), "written off");
    assert_eq!(switch.enabled(), Ok(false), "unchanged");
    let replaced = dir.join("aterm.toml.new");
    crate::fsx::write_owned_in_place(&replaced, "[swarm]\nenabled = true\n");
    std::fs::rename(&replaced, &config).unwrap();
    assert_eq!(switch.enabled(), Ok(true), "replaced on");
    crate::fsx::write_owned_in_place(&config, "[swarm]\nenabled = 1\n");
    assert!(switch.enabled().is_err(), "unreadable says so");
    std::fs::remove_file(&config).unwrap();
    assert_eq!(switch.enabled(), Ok(true), "removed: on");
}

/// home-manager links `~/.config/aterm/aterm.toml` to a read-only file in the Nix store that
/// root owns (mode 0444). The window reads it by the one rule every reader holds (this user's
/// or root's, no group or other write: `aterm_types::dirs::aterm_config_refusal`), so the
/// switch hears what the window hears. A root-owned, read-only, empty file stands in for the
/// store file (`/private/etc/hosts.equiv` on macOS); a Mac without one has nothing to read.
/// NEGATIVE CONTROL: hold `fsx::read_aterm_config` to this user's own files alone.
#[cfg(unix)]
#[test]
fn a_root_owned_read_only_linked_aterm_toml_reads_as_the_window_reads_it() {
    use std::os::unix::fs::MetadataExt;
    let store = std::path::Path::new("/private/etc/hosts.equiv");
    let Ok(meta) = std::fs::metadata(store) else {
        return;
    };
    if meta.uid() != 0 || meta.len() != 0 || meta.mode() & 0o022 != 0 {
        return;
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join(".config").join("aterm");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(store, dir.join("aterm.toml")).unwrap();
    assert_eq!(
        Switch::new(&home).enabled(),
        Ok(true),
        "the window reads this file as the defaults (sharing on), and only root can write it"
    );
}

/// A file another user may write is refused by every reader alike.
#[cfg(unix)]
#[test]
fn a_group_writable_aterm_toml_is_read_by_no_reader() {
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let dir = home.join(".config").join("aterm");
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("aterm.toml");
    std::fs::write(&config, "[swarm]\nenabled = true\n").unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
    let refused = enabled_at(&config).expect_err("its group may write it");
    assert!(
        refused.to_string().contains("lets other users write it"),
        "{refused}"
    );
}

#[cfg(unix)]
#[test]
fn enabled_in_home_reads_scratch_home_config() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let cfg_dir = home.join(".config").join("aterm");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    assert_eq!(enabled_in_home(&home), Ok(true));
    crate::fsx::write_owned_in_place(cfg_dir.join("aterm.toml"), "[swarm]\nenabled = false\n");
    assert_eq!(enabled_in_home(&home), Ok(false));
}
