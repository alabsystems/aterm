// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn a_scheduled_record_writer_does_not_spend_the_automatic_ask() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let started = std::time::Instant::now() - std::time::Duration::from_secs(3);
    drop(lock_started(&path, started).unwrap());
    assert!(claim(&Live, &path, "owner").unwrap());
    let held = Live.lock(&path).unwrap();
    assert!(matches!(
        lock_started(&path, std::time::Instant::now() - std::time::Duration::from_secs(61)),
        Err(error) if error.kind() == io::ErrorKind::TimedOut
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "opened owner\n");
    drop(held);
    assert!(!claim(&Live, &path, "later").unwrap());
    forget(&Live, &path, "later").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "opened owner\n");
    forget(&Live, &path, "owner").unwrap();
    assert!(claim(&Live, &path, "later").unwrap());
}

/// A consumed episode survives updates and historical anonymous/multi-owner records.
/// Only its exact owner's proven preprompt release can remove a line; unreadable records
/// remain held and unchanged.
#[test]
fn a_password_episode_survives_updates_and_only_its_owner_can_release_it() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let text = || std::fs::read_to_string(&path).unwrap();
    assert!(!spent(&Live, &path));
    assert!(claim(&Live, &path, "release").unwrap());
    for build in ["release", "dev", "update"] {
        assert!(spent(&Live, &path));
        assert!(!claim(&Live, &path, build).unwrap());
    }
    forget(&Live, &path, "update").unwrap();
    assert_eq!(text(), "opened release\n");
    forget(&Live, &path, "release").unwrap();
    assert!(!spent(&Live, &path));
    assert!(!path.exists());

    for historical in [
        "opened\n",
        "opened release\nopened dev\n",
        "opened\nopened release\n",
    ] {
        std::fs::write(&path, historical).unwrap();
        assert!(spent(&Live, &path));
        assert!(!claim(&Live, &path, "update").unwrap());
        assert_eq!(text(), historical);
    }
    forget(&Live, &path, "release").unwrap();
    assert_eq!(
        text(),
        "opened\n",
        "anonymous historical hold survives exact release"
    );

    let too_many = "opened again\n".repeat(LINES + 1);
    for malformed in [
        "",
        "\n",
        "opened two words\n",
        "closed\n",
        too_many.as_str(),
    ] {
        std::fs::write(&path, malformed).unwrap();
        assert!(spent(&Live, &path), "{malformed:?} holds");
        assert!(claim(&Live, &path, "release").is_err());
        assert!(forget(&Live, &path, "release").is_err());
        assert_eq!(text(), malformed);
    }
    assert!(spent(&Live, dir.path()));
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, "opened dev\n").unwrap();
    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(&target, &linked).unwrap();
    assert!(spent(&Live, &linked));
    assert!(claim(&Live, &linked, "release").is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "opened dev\n");
    assert!(
        std::fs::symlink_metadata(&linked)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        claim(
            &Live,
            &dir.path().join("none"),
            &"x".repeat(BUILD_BYTES + 1)
        )
        .is_err()
    );
}

/// A staged copy an earlier process of this pid left as it died mid-write (killed during the
/// flush, before its rename) holds no later change, a claim's or a forget's, and a link left
/// at that name goes without its target being touched.
/// NEGATIVE CONTROL: a writer that opens the staged name without clearing it first fails every
/// such change with `AlreadyExists`, the line it should add or take back left as it was.
#[test]
fn a_staged_copy_a_dead_process_of_this_pid_left_holds_no_change() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let staged = dir
        .path()
        .join(format!("{FILE}.tmp-{}", std::process::id()));
    let text = || std::fs::read_to_string(&path).unwrap();
    std::fs::write(&staged, "opened hal").unwrap();
    assert!(claim(&Live, &path, "release").unwrap());
    assert_eq!(text(), "opened release\n");
    assert!(!staged.exists(), "the staged copy went with the rename");

    std::fs::write(&path, "opened release\nopened dev\n").unwrap();
    std::fs::write(&staged, "opened hal").unwrap();
    forget(&Live, &path, "release").unwrap();
    assert_eq!(text(), "opened dev\n", "the forget took its line back");

    let target = dir.path().join("elsewhere");
    std::fs::write(&target, "kept\n").unwrap();
    std::os::unix::fs::symlink(&target, &staged).unwrap();
    forget(&Live, &path, "dev").unwrap();
    assert!(claim(&Live, &path, "release").unwrap());
    assert_eq!(text(), "opened release\n");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "kept\n");
    assert!(std::fs::symlink_metadata(&staged).is_err());
}

/// Valid historical records and unreadable records both hold, with distinct diagnostics.
#[test]
fn the_reading_distinguishes_spent_available_and_unread() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    assert!(matches!(reading(&Live, &path), Asked::Available));
    for record in ["opened\n", "opened dev\n"] {
        std::fs::write(&path, record).unwrap();
        assert!(matches!(reading(&Live, &path), Asked::Spent));
    }
    std::fs::write(&path, "closed\n").unwrap();
    assert!(
        matches!(reading(&Live, &path), Asked::Unread(error) if error.kind() == io::ErrorKind::InvalidData)
    );
    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(dir.path().join("elsewhere"), &linked).unwrap();
    for unread in [linked.as_path(), dir.path()] {
        assert!(matches!(reading(&Live, unread), Asked::Unread(_)));
    }
}

/// A claim reads the record under its lock: a claim made while another asker holds the lock
/// waits for it, then finds the line that asker added and adds none (`false`), the record left
/// as that asker wrote it; and a claim of a build the record names already rewrites nothing,
/// so its order stays.
/// NEGATIVE CONTROL: with no other asker, the same claim adds the line (`true`).
#[test]
fn of_two_builds_claiming_one_episode_only_one_asks() {
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join(FILE);
    let other = std::rc::Rc::new(std::cell::RefCell::new(Some(Live.lock(&path).unwrap())));
    let waited = std::rc::Rc::new(std::cell::Cell::new(false));
    let claimed = aterm_uds::file_lock::with_lock_wait_observer(
        {
            let (other, waited, path) = (other.clone(), waited.clone(), path.clone());
            move |_| {
                if let Some(held) = other.borrow_mut().take() {
                    // The other asker adds the line while it holds the lock, then lets go.
                    Live.write(&path, "opened release\n").unwrap();
                    drop(held);
                    waited.set(true);
                }
            }
        },
        || claim(&Live, &path, "update").unwrap(),
    );
    assert!(waited.get(), "the claim waited for the other asker's lock");
    assert!(!claimed, "the other asker added it first");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "opened release\n");

    std::fs::write(&path, "opened release\nopened dev\n").unwrap();
    assert!(!claim(&Live, &path, "release").unwrap());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "opened release\nopened dev\n",
        "a lost claim rewrites nothing"
    );

    let alone = dir.path().join("alone");
    assert!(claim(&Live, &alone, "release").unwrap());
}

/// A first record makes the folder aterm's state lives in when there is none yet (a package
/// install before the window ever launched), and is written whole, readable by this account
/// alone, beside its lock file.
#[test]
fn a_first_record_makes_its_folder() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = crate::test_scratch_dir("clt-mark");
    let path = dir.path().join("state").join("aterm").join(FILE);
    assert!(claim(&Live, &path, "release").unwrap());
    assert!(spent(&Live, &path));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [FILE.to_owned(), format!("{FILE}.lock")],
        "no staged file left beside it"
    );
}

/// The window (launchd gives it no `XDG_CONFIG_HOME`) and a terminal's `aterm init` (whose
/// shell may export one) find one record: its place is this account's aterm state, which no
/// `XDG_CONFIG_HOME` moves. This test binary runs itself twice over one state root (a lab
/// instance's `ATERM_STATE_HOME`; each test process has its own root otherwise), with the
/// variable set and with it unset, and both runs name the same file in that root.
/// NEGATIVE CONTROL: the config folder the run with the variable set reads is the variable's.
#[test]
fn one_record_whether_or_not_a_shell_exports_xdg_config_home() {
    const SAY: &str = "ATERM_TYPES_CLT_MARK_SAY_PATH";
    if std::env::var_os(SAY).is_some() {
        println!(
            "record={}",
            path().expect("a test binary's state root").display()
        );
        println!(
            "config={}",
            crate::dirs::aterm_config_dir()
                .expect("a test binary's config folder")
                .display()
        );
        return;
    }
    let dir = crate::test_scratch_dir("clt-mark");
    let state = dir.path().join("state");
    let xdg = dir.path().join("xdg");
    let test = format!(
        "{}::one_record_whether_or_not_a_shell_exports_xdg_config_home",
        module_path!().split_once("::").map_or("", |(_, rest)| rest)
    );
    let said = |xdg: Option<&Path>| {
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env(SAY, "1")
            .env("ATERM_STATE_HOME", &state)
            .stdin(aterm_uds::spawnfd::Stdio::null());
        match xdg {
            Some(xdg) => child.env("XDG_CONFIG_HOME", xdg),
            None => child.env_remove("XDG_CONFIG_HOME"),
        };
        let out = child.output().expect("re-exec the test binary");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "the re-exec failed:\n{text}");
        let line = |key: &str| {
            // The harness writes the test's name ahead of its first line.
            text.lines()
                .find_map(|line| line.split_once(key).map(|(_, value)| value))
                .map(PathBuf::from)
                .unwrap_or_else(|| panic!("the re-exec named no {key}:\n{text}"))
        };
        (line("record="), line("config="))
    };
    let (set, config) = said(Some(&xdg));
    let (unset, _) = said(None);
    assert_eq!(set, state.join(FILE), "in the account's aterm state");
    assert_eq!(unset, set, "one file for both environments");
    assert_eq!(
        config,
        xdg.join("aterm"),
        "the config folder is the variable's"
    );
}
