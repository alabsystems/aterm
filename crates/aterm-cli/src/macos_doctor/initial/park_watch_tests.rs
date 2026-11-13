// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::atomic::AtomicUsize;

static CHECKOUT_FIRED: AtomicUsize = AtomicUsize::new(0);

fn checkout_fired() {
    CHECKOUT_FIRED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn selected_checkout_arrival_and_in_place_git_repair_rearm_the_existing_queue() {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().canonicalize().unwrap();
    let uid = std::fs::metadata(&home).unwrap().uid();
    let root = home.join("code");
    std::fs::create_dir(&root).unwrap();
    let admin = root.join(".git");
    let config = admin.join("config");
    let paths = vec![root, admin.clone(), config.clone()];
    let targets = paths
        .iter()
        .map(|path| FolderTarget {
            target: path.clone(),
            floor: home.clone(),
            follow_links: false,
            owner: Some(uid),
        })
        .collect();
    let scoped = paths
        .iter()
        .map(|path| (path.clone(), home.clone(), uid))
        .collect();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_paths_scoped(
        targets,
        paths,
        scoped,
        Arc::clone(&stop),
        Arc::clone(&lost),
        checkout_fired,
    )
    .unwrap();
    for path in [&admin, &config] {
        let before = CHECKOUT_FIRED.load(Ordering::SeqCst);
        if path == &admin {
            std::fs::create_dir(path).unwrap()
        } else {
            std::fs::write(path, b"origin01").unwrap()
        }
        until("the selected checkout boundary was not heard", || {
            CHECKOUT_FIRED.load(Ordering::SeqCst) > before
        });
    }
    let before = CHECKOUT_FIRED.load(Ordering::SeqCst);
    let inode = std::fs::metadata(&config).unwrap().ino();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&config)
        .unwrap();
    file.write_all(b"origin02").unwrap();
    until("in-place Git metadata repair was not heard", || {
        CHECKOUT_FIRED.load(Ordering::SeqCst) > before
    });
    assert_eq!(std::fs::metadata(&config).unwrap().ino(), inode);
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

#[test]
fn a_checkout_parent_replaced_by_a_link_cannot_enlist_its_destination() {
    use std::os::unix::fs::MetadataExt as _;

    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().canonicalize().unwrap();
    let uid = std::fs::metadata(&home).unwrap().uid();
    let root = home.join("code");
    let foreign = home.join("unselected");
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(foreign.join(".git")).unwrap();
    std::os::unix::fs::symlink(&foreign, root.join("linked")).unwrap();
    let target = FolderTarget {
        target: root.join("linked/.git/config"),
        floor: home.clone(),
        follow_links: false,
        owner: Some(uid),
    };
    assert_eq!(target.nearest(), Some(root.clone()));
    assert!(!owned_directory(&root.join("linked/.git"), &home, uid));
    std::fs::rename(&root, home.join("saved-code")).unwrap();
    std::os::unix::fs::symlink(&foreign, &root).unwrap();
    assert_eq!(target.nearest(), Some(home));
    let watcher = atpkg::dirwatch::Watcher::new().unwrap();
    // A swap after the component check is also refused at the actual kernel open.
    assert!(watcher.add_dir_scoped(&root.join(".git")).is_err());
    std::fs::write(foreign.join(".git/config"), b"unselected config").unwrap();
    assert!(watcher.add_file_scoped(&root.join(".git/config")).is_err());
    assert!(
        watcher.add_dir(&root.join(".git")).is_ok(),
        "the old caller policy remains unchanged"
    );
}

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// Wait, as a one-minute hang detector, until `done`.
fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + HANG;
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A target's watch sits on the deepest folder there, never above its floor, and moves
/// deeper once the next folder on the way appears.
#[test]
fn a_watch_moves_deeper_as_folders_appear() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let floor = home.path().join(".config");
    let target = floor.join("aterm/secrets");
    let watcher = atpkg::dirwatch::Watcher::new().unwrap();
    let mut folders = Folders::new(vec![(target.clone(), floor.clone()).into()]);
    let on = |folders: &Folders| {
        folders.watched[0]
            .as_ref()
            .map(|(folder, _)| folder.clone())
    };
    folders.arm(&watcher).unwrap();
    assert_eq!(
        on(&folders),
        None,
        "nothing at or below the floor to watch yet"
    );
    std::fs::create_dir_all(&floor).unwrap();
    folders.arm(&watcher).unwrap();
    assert_eq!(on(&folders), Some(floor));
    std::fs::create_dir_all(&target).unwrap();
    folders.arm(&watcher).unwrap();
    assert_eq!(on(&folders), Some(target));
}

static FIRED: AtomicUsize = AtomicUsize::new(0);

fn fired() {
    FIRED.fetch_add(1, Ordering::SeqCst);
}

/// The subscriber writing its state where a park watches tells the park to look again.
#[test]
fn a_write_in_a_watched_folder_tells_the_park() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let floor = home.path().join(".config");
    let target = floor.join("aterm/secrets");
    std::fs::create_dir_all(&target).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_folders(
        vec![(target.clone(), floor).into()],
        Arc::clone(&stop),
        Arc::clone(&lost),
        fired,
    )
    .unwrap();
    std::fs::write(target.join("state.toml"), "seq = 1\n").unwrap();
    until("the watch never fired", || FIRED.load(Ordering::SeqCst) > 0);
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

static MATERIALIZED_FIRED: AtomicUsize = AtomicUsize::new(0);

static GIT_FIRED: AtomicUsize = AtomicUsize::new(0);

static REPOSITORY_INPUT_FIRED: AtomicUsize = AtomicUsize::new(0);

fn repository_input_fired() {
    REPOSITORY_INPUT_FIRED.fetch_add(1, Ordering::SeqCst);
}

static DANGLING_HELPER_FIRED: AtomicUsize = AtomicUsize::new(0);

fn dangling_helper_fired() {
    DANGLING_HELPER_FIRED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn a_dangling_helper_target_arriving_rebinds_its_file_watch() {
    use std::io::Write as _;

    for parent_link in [false, true] {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let base = root.path().canonicalize().unwrap();
        let home = base.join("home");
        let launcher = home.join(".local/bin/aterm");
        let target = base.join("package/bin/aterm");
        let declaration = if parent_link {
            std::fs::create_dir_all(home.join(".local")).unwrap();
            let link = home.join(".local/bin");
            std::os::unix::fs::symlink(target.parent().unwrap(), &link).unwrap();
            link
        } else {
            std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, &launcher).unwrap();
            launcher.clone()
        };
        assert!(launcher.canonicalize().is_err(), "the helper is absent");
        let original = std::fs::read_link(&declaration).unwrap();
        let files = vec![launcher.clone(), target.clone()];
        let mut folders = repository_input_folders(&files);
        for folder in &mut folders {
            folder.floor = base.clone();
        }
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let watcher = watch_paths(
            folders,
            files,
            Arc::clone(&stop),
            Arc::clone(&lost),
            dangling_helper_fired,
        )
        .unwrap();
        if parent_link {
            let before = DANGLING_HELPER_FIRED.load(Ordering::SeqCst);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            until("the missing helper parent was not heard", || {
                DANGLING_HELPER_FIRED.load(Ordering::SeqCst) > before
            });
        }
        let before = DANGLING_HELPER_FIRED.load(Ordering::SeqCst);
        std::fs::write(&target, "helper01").unwrap();
        until("the dangling helper target's arrival was not heard", || {
            DANGLING_HELPER_FIRED.load(Ordering::SeqCst) > before
        });
        assert_eq!(launcher.canonicalize().unwrap(), target);
        assert_eq!(std::fs::read_link(&declaration).unwrap(), original);

        let before = DANGLING_HELPER_FIRED.load(Ordering::SeqCst);
        let mut helper = std::fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .unwrap();
        helper.write_all(b"helper02").unwrap();
        drop(helper);
        until("the arriving helper's file watch was not rebound", || {
            DANGLING_HELPER_FIRED.load(Ordering::SeqCst) > before
        });
        assert!(!lost.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        watcher.wake().unwrap();
        let ended = Arc::downgrade(&watcher);
        drop(watcher);
        until("the helper watch outlived its park", || {
            ended.upgrade().is_none()
        });
    }
}

#[test]
fn selected_inputs_and_helper_wake_for_in_place_writes_and_replacement() {
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;

    let root = aterm_tempfile::TempDir::new().unwrap();
    let home = root.path().join("home");
    let profile = home.join(".config/aterm/init.toml");
    let token = root.path().join("team/private-token");
    let helper = root.path().join("application/Contents/MacOS/aterm");
    let launcher = home.join(".local/bin/aterm");
    for file in [&profile, &token, &helper, &launcher] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    }
    for file in [&profile, &token, &helper] {
        std::fs::write(file, "before01").unwrap();
    }
    std::os::unix::fs::symlink(&helper, &launcher).unwrap();
    let files = vec![profile.clone(), token.clone(), launcher, helper.clone()];
    let mut folders = repository_input_folders(&files);
    // The production watch follows parent chains to root; this isolated tree is the
    // test's namespace, so unrelated tests cannot supply its notification.
    for target in &mut folders {
        target.floor = root.path().to_path_buf();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_paths(
        folders,
        files,
        Arc::clone(&stop),
        Arc::clone(&lost),
        repository_input_fired,
    )
    .unwrap();
    let rewrite = |path: &Path, bytes: &[u8]| {
        let before = REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst);
        let inode = std::fs::metadata(path).unwrap().ino();
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.write_all(bytes).unwrap();
        drop(file);
        assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
        until("the in-place repository input change was not heard", || {
            REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst) > before
        });
    };
    rewrite(&profile, b"profile2");
    rewrite(&token, b"token002");
    rewrite(&helper, b"helper02");

    let before = REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst);
    let replacement = token.with_extension("replacement");
    std::fs::write(&replacement, "replaced").unwrap();
    std::fs::rename(replacement, &token).unwrap();
    until("the token replacement was not heard", || {
        REPOSITORY_INPUT_FIRED.load(Ordering::SeqCst) > before
    });
    rewrite(&token, b"token003");
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
    let ended = Arc::downgrade(&watcher);
    drop(watcher);
    until("the input watch outlived its park", || {
        ended.upgrade().is_none()
    });
}

fn git_fired() {
    GIT_FIRED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn packaged_git_activation_wakes_a_park_waiting_for_git() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let prefix = home.path().join("pkg");
    std::fs::create_dir_all(prefix.join("store/git")).unwrap();
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let current = prefix.join("store/git/current");
    let next = prefix.join("store/git/current.next");
    std::os::unix::fs::symlink("41", &current).unwrap();
    std::os::unix::fs::symlink("42", &next).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_folders(
        packaged_git_folders(&prefix),
        Arc::clone(&stop),
        Arc::clone(&lost),
        git_fired,
    )
    .unwrap();
    let before = GIT_FIRED.load(Ordering::SeqCst);
    std::fs::rename(next, current).unwrap();
    until("the managed Git activation link never woke setup", || {
        GIT_FIRED.load(Ordering::SeqCst) > before
    });
    let before = GIT_FIRED.load(Ordering::SeqCst);
    std::fs::write(prefix.join("store/git/42.ready"), "ready").unwrap();
    until("the managed Git activation never woke setup", || {
        GIT_FIRED.load(Ordering::SeqCst) > before
    });
    let before = GIT_FIRED.load(Ordering::SeqCst);
    std::fs::write(prefix.join("bin/git"), "shim").unwrap();
    until("the managed Git shim never woke setup", || {
        GIT_FIRED.load(Ordering::SeqCst) > before
    });
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

fn materialized_fired() {
    MATERIALIZED_FIRED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn a_materialized_file_repair_wakes_without_a_vault_state_write() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let state_dir = home.path().join(".config/aterm/secrets");
    let output = home.path().join(".secrets");
    std::fs::create_dir_all(&state_dir).unwrap();
    std::fs::create_dir(&output).unwrap();
    let state = state_dir.join("state.toml");
    std::fs::write(&state, "seq = 7\n").unwrap();
    let credential = output.join("test_credential");
    std::fs::write(&credential, "before").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_folders(
        secrets_folders(home.path()),
        Arc::clone(&stop),
        Arc::clone(&lost),
        materialized_fired,
    )
    .unwrap();
    let replacement = output.join("replacement");
    std::fs::write(&replacement, "after").unwrap();
    std::fs::rename(replacement, credential).unwrap();
    until("the materialized repair was never heard", || {
        MATERIALIZED_FIRED.load(Ordering::SeqCst) > 0
    });
    assert_eq!(std::fs::read_to_string(state).unwrap(), "seq = 7\n");
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

#[test]
fn a_materialized_root_symlink_is_watched_at_its_parent() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let elsewhere = aterm_tempfile::TempDir::new().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), home.path().join(".secrets")).unwrap();
    let watcher = atpkg::dirwatch::Watcher::new().unwrap();
    let mut folders = Folders::new(secrets_folders(home.path()));
    folders.arm(&watcher).unwrap();
    assert_eq!(
        folders.watched[1].as_ref().map(|(path, _)| path.as_path()),
        Some(home.path()),
        "materialization refuses the symlink; only its replacement can make progress"
    );
}

static MEMBERSHIP_FIRED: AtomicUsize = AtomicUsize::new(0);

fn membership_fired() {
    MEMBERSHIP_FIRED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn a_membership_permission_repair_is_heard_without_replacing_the_state_file() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let home = aterm_tempfile::TempDir::new().unwrap();
    let paths = aterm_secrets::client::Paths::locate_for_home(home.path());
    let state = paths
        .standing_files()
        .into_iter()
        .find(|path| path.file_name().is_some_and(|name| name == "state.toml"))
        .expect("the canonical standing inputs include the saved client state");
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::write(&state, "inert membership fixture").unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o644)).unwrap();
    let inode = std::fs::metadata(&state).unwrap().ino();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_paths(
        secrets_folders(home.path()),
        secrets_input_files(home.path()),
        Arc::clone(&stop),
        Arc::clone(&lost),
        membership_fired,
    )
    .unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o600)).unwrap();
    until("the in-place membership repair was never heard", || {
        MEMBERSHIP_FIRED.load(Ordering::SeqCst) > 0
    });
    assert_eq!(std::fs::metadata(&state).unwrap().ino(), inode);
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

static RECORDS_FIRED: AtomicUsize = AtomicUsize::new(0);

fn records_fired() {
    RECORDS_FIRED.fetch_add(1, Ordering::SeqCst);
}

fn append(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
}

/// The standing inputs name a records layout's header and data file whatever the records entry
/// is. While it is a plain file those two are named under a file, which is nothing to watch, and
/// the watch starts and hears a record put on the end of the plain file; once a directory
/// replaces the file, a record put on the end of its data file is heard.
///
/// NEGATIVE CONTROL (2026-11-11): with `arm_files` answering a path named under a file as an
/// error, the watch does not start.
#[test]
fn a_record_appended_in_place_is_heard_under_a_plain_records_file_and_under_a_layout() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let paths = aterm_secrets::client::Paths::locate_for_home(home.path());
    let files = paths.standing_files();
    let named = |name: &str| {
        files
            .iter()
            .find(|path| path.file_name().is_some_and(|file| file == name))
            .unwrap_or_else(|| panic!("the standing inputs name {name}"))
            .clone()
    };
    let (entry, data) = (named("records"), named("current"));
    assert_eq!(data.parent(), Some(entry.as_path()));
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::write(&entry, "one\n").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_paths(
        secrets_folders(home.path()),
        secrets_input_files(home.path()),
        Arc::clone(&stop),
        Arc::clone(&lost),
        records_fired,
    )
    .expect("a path named under the plain records file is not there, not a failure");
    append(&entry, b"two\n");
    until(
        "the record on the end of the plain file was never heard",
        || RECORDS_FIRED.load(Ordering::SeqCst) > 0,
    );

    let before = RECORDS_FIRED.load(Ordering::SeqCst);
    let next = paths.dir.join("records.next");
    std::fs::create_dir(&next).unwrap();
    std::fs::write(next.join("current"), "one\ntwo\n").unwrap();
    std::fs::remove_file(&entry).unwrap();
    std::fs::rename(&next, &entry).unwrap();
    until(
        "the layout that replaced the plain file was never heard",
        || RECORDS_FIRED.load(Ordering::SeqCst) > before,
    );
    let before = RECORDS_FIRED.load(Ordering::SeqCst);
    append(&data, b"three\n");
    until(
        "the record on the end of the layout's data file was never heard",
        || RECORDS_FIRED.load(Ordering::SeqCst) > before,
    );
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

static REPLACED_FIRED: AtomicUsize = AtomicUsize::new(0);

fn replaced_fired() {
    REPLACED_FIRED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn a_replaced_directory_keeps_hearing_its_new_contents() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let target = home.path().join("watched");
    std::fs::create_dir(&target).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_folders(
        vec![(target.clone(), home.path().to_path_buf()).into()],
        Arc::clone(&stop),
        Arc::clone(&lost),
        replaced_fired,
    )
    .unwrap();
    std::fs::rename(&target, home.path().join("previous")).unwrap();
    std::fs::create_dir(&target).unwrap();
    until("the directory replacement was never heard", || {
        REPLACED_FIRED.load(Ordering::SeqCst) > 0
    });
    assert!(!lost.load(Ordering::Acquire));
    let before = REPLACED_FIRED.load(Ordering::SeqCst);
    std::fs::write(target.join("state.toml"), "seq = 2\n").unwrap();
    until("the watch stayed on the renamed old directory", || {
        REPLACED_FIRED.load(Ordering::SeqCst) > before
    });
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

static RETARGET_FIRED: AtomicUsize = AtomicUsize::new(0);

fn retarget_fired() {
    RETARGET_FIRED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn a_retargeted_config_link_keeps_hearing_the_new_vault() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let first = aterm_tempfile::TempDir::new().unwrap();
    let second = aterm_tempfile::TempDir::new().unwrap();
    for root in [first.path(), second.path()] {
        std::fs::create_dir_all(root.join("aterm/secrets")).unwrap();
    }
    std::fs::create_dir(home.path().join(".secrets")).unwrap();
    let config = home.path().join(".config");
    std::os::unix::fs::symlink(first.path(), &config).unwrap();
    let replacement = home.path().join("next-config");
    std::os::unix::fs::symlink(second.path(), &replacement).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_folders(
        secrets_folders(home.path()),
        Arc::clone(&stop),
        Arc::clone(&lost),
        retarget_fired,
    )
    .unwrap();
    std::fs::rename(replacement, config).unwrap();
    until("the config link retarget was never heard", || {
        RETARGET_FIRED.load(Ordering::SeqCst) > 0
    });
    let before = RETARGET_FIRED.load(Ordering::SeqCst);
    std::fs::write(second.path().join("aterm/secrets/state.toml"), "seq = 2\n").unwrap();
    until(
        "the watch stayed on the previous config destination",
        || RETARGET_FIRED.load(Ordering::SeqCst) > before,
    );
    assert!(!lost.load(Ordering::Acquire));
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

#[test]
fn a_parent_replacement_rebinds_the_descendant_watch() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let parent = home.path().join("config");
    let target = parent.join("aterm/secrets");
    std::fs::create_dir_all(&target).unwrap();
    let watcher = atpkg::dirwatch::Watcher::new().unwrap();
    let mut folders = Folders::new(vec![(target.clone(), home.path().to_path_buf()).into()]);
    folders.arm(&watcher).unwrap();
    std::fs::rename(parent, home.path().join("previous-config")).unwrap();
    std::fs::create_dir_all(&target).unwrap();
    let fired: BTreeSet<_> = watcher
        .wait(Some(Duration::from_secs(60)))
        .unwrap()
        .into_iter()
        .collect();
    assert!(!fired.is_empty(), "a parent must hear the namespace change");
    folders.arm_after(&watcher, &fired).unwrap();
    let current = folders.watched[0].as_ref().unwrap().1;
    std::fs::write(target.join("state.toml"), "seq = 3\n").unwrap();
    until("the descendant watch stayed on the old parent", || {
        watcher
            .wait(Some(Duration::ZERO))
            .unwrap()
            .contains(&current)
    });
}

static FLOOR_LOST_TOLD: AtomicUsize = AtomicUsize::new(0);

fn floor_lost_told() {
    FLOOR_LOST_TOLD.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn a_missing_watch_floor_returns_the_fallback_timer() {
    let home = aterm_tempfile::TempDir::new().unwrap();
    let floor = home.path().join("floor");
    std::fs::create_dir(&floor).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_folders(
        vec![(floor.clone(), floor.clone()).into()],
        Arc::clone(&stop),
        Arc::clone(&lost),
        floor_lost_told,
    )
    .unwrap();
    std::fs::remove_dir(floor).unwrap();
    until(
        "a park with no remaining watch must regain its timer",
        || lost.load(Ordering::Acquire) && FLOOR_LOST_TOLD.load(Ordering::SeqCst) > 0,
    );
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
}

static LOST_TOLD: AtomicUsize = AtomicUsize::new(0);

fn lost_told() {
    LOST_TOLD.fetch_add(1, Ordering::SeqCst);
}

/// A folder that appears where no watch can follow (this login cannot read it) leaves the
/// watch lost and tells the park, which takes its timer back rather than wait unheard.
#[test]
fn a_folder_the_watch_cannot_follow_gives_the_park_its_timer_back() {
    use std::os::unix::fs::PermissionsExt;
    let home = aterm_tempfile::TempDir::new().unwrap();
    let floor = home.path().join(".config");
    std::fs::create_dir_all(&floor).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watcher = watch_folders(
        vec![(floor.join("aterm/secrets"), floor.clone()).into()],
        Arc::clone(&stop),
        Arc::clone(&lost),
        lost_told,
    )
    .unwrap();
    let unreadable = floor.join("aterm");
    std::fs::create_dir(&unreadable).unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    until("the watch never said it was lost", || {
        lost.load(Ordering::Acquire) && LOST_TOLD.load(Ordering::SeqCst) > 0
    });
    stop.store(true, Ordering::Release);
    watcher.wake().unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// The network watch tells the park once a burst settles, by ten seconds into one that never
/// does, and never within a minute of its last tell.
#[test]
fn the_network_watch_tells_once_a_burst_settles_and_at_most_once_a_minute() {
    let start = Instant::now();
    let at = |seconds| start + Duration::from_secs(seconds);
    assert_eq!(
        network_tell_at(start, at(3), None),
        at(5),
        "a join: the link, then its addresses"
    );
    assert_eq!(network_tell_at(start, at(30), None), at(10), "never quiet");
    assert_eq!(
        network_tell_at(at(5), at(5), Some(start)),
        at(60),
        "a burst soon after a tell waits out the minute"
    );
    assert_eq!(network_tell_at(at(90), at(90), Some(start)), at(92));
}

/// A park whose network watch cannot open its socket fails to start with the kernel's
/// reason, which the park's log line carries (`setup: a parked run reads its facts each
/// minute: …`). Here no descriptor is free, in a child of this test binary, so no other test
/// meets the limit.
#[test]
fn a_network_watch_that_cannot_open_says_why() {
    use std::os::fd::AsRawFd as _;
    const CHILD: &str = "ATERM_TEST_PARK_WITH_NO_DESCRIPTOR_FREE";
    if std::env::var_os(CHILD).is_none() {
        let name = format!(
            "{}::a_network_watch_that_cannot_open_says_why",
            module_path!().split_once("::").map_or("", |(_, rest)| rest)
        );
        let mut child =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
        child
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1");
        let out = aterm_uds::deadline::output_until(&mut child, Instant::now() + HANG)
            .expect("the child ran");
        let said = String::from_utf8_lossy(&out.stdout);
        let refused = format!(
            "park refused: {}",
            io::Error::from_raw_os_error(libc::EMFILE)
        );
        // The line the test harness began with its test's name.
        assert!(
            said.lines().any(|line| line.ends_with(&refused)),
            "the park's refusal does not say why:\n{said}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes the struct this frame owns.
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
        0
    );
    // The lowest descriptor free, as the limit: no descriptor opens after.
    let lowest = std::fs::File::open("/dev/null")
        .expect("a descriptor free")
        .as_raw_fd();
    limit.rlim_cur = libc::rlim_t::try_from(lowest).expect("a descriptor's number");
    // SAFETY: `setrlimit` reads the struct this frame owns: a soft limit under the hard one.
    assert_eq!(
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) },
        0
    );
    let network = Watches {
        network: true,
        ..Watches::default()
    };
    match ParkWatch::start(network, || {}) {
        Ok(_) => println!("park started"),
        Err(why) => println!("park refused: {why}"),
    }
}

static DROP_TOLD: AtomicUsize = AtomicUsize::new(0);

fn drop_told() {
    DROP_TOLD.fetch_add(1, Ordering::SeqCst);
}

/// Dropping a park's network watch ends its thread by the pipe's hang-up, and is neither a
/// tell nor a loss. `stop` is never set here and the stand-in socket never speaks, so only
/// the hang-up can end the wait, as only it ends one on a kernel event socket, which takes
/// no `shutdown(2)`.
#[test]
fn a_dropped_network_watch_ends_its_thread_by_its_pipe() {
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let lost = Arc::new(AtomicBool::new(false));
    let watch = watch_network_over(
        aterm_uds::network::RouteChanges::over(heard.into()),
        Arc::new(AtomicBool::new(false)),
        Arc::clone(&lost),
        drop_told,
    )
    .expect("a network watch");
    let routes = watch.routes.clone();
    drop(watch);
    until("the network thread outlived its watch", || {
        routes.upgrade().is_none()
    });
    assert!(!lost.load(Ordering::Acquire), "an end is no loss");
    assert_eq!(DROP_TOLD.load(Ordering::SeqCst), 0, "an end tells nothing");
    drop(kernel);
}

static PARK_TOLD: AtomicUsize = AtomicUsize::new(0);

fn park_told() {
    PARK_TOLD.fetch_add(1, Ordering::SeqCst);
}

/// A park that watches the network starts a network watch on the kernel's socket, and the
/// park's end (`stop`, then the pipe's hang-up) ends that watch's thread, as neither a tell
/// nor a loss. Which of the two ends it is where the thread was when the park dropped; that
/// the hang-up alone ends a wait is `a_dropped_network_watch_ends_its_thread_by_its_pipe`'s.
#[test]
fn a_park_watching_the_network_ends_its_network_thread_with_it() {
    let watch = ParkWatch::start(
        Watches {
            network: true,
            ..Watches::default()
        },
        park_told,
    )
    .expect("a park's watches");
    let routes = watch
        .network
        .as_ref()
        .expect("a network watch")
        .routes
        .clone();
    let lost = Arc::clone(&watch.lost);
    drop(watch);
    until("the network thread outlived its park", || {
        routes.upgrade().is_none()
    });
    assert!(!lost.load(Ordering::Acquire), "an end is no loss");
    assert_eq!(PARK_TOLD.load(Ordering::SeqCst), 0, "an end tells nothing");
}

/// One kernel event message as `PF_SYSTEM` delivers it: Apple's network class, its
/// `subclass` and event `code`.
fn kernel_event(subclass: u32, code: u32) -> Vec<u8> {
    [24_u32, 1, 1, subclass, 37, code]
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect()
}

/// When the park was told of each change ([`change_told`]).
static CHANGES_TOLD: std::sync::Mutex<Vec<Instant>> = std::sync::Mutex::new(Vec::new());

fn change_told() {
    CHANGES_TOLD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Instant::now());
}

/// When the park was told of its `nth` change, once it was.
fn nth_change_told(nth: usize) -> Instant {
    let told = || {
        CHANGES_TOLD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(nth)
            .copied()
    };
    until("a change of network was never told", || told().is_some());
    told().expect("a change told")
}

/// Every park's last tell of a network change ([`super::NETWORK_TOLD`]).
fn last_network_tell() -> std::sync::MutexGuard<'static, Option<Instant>> {
    super::NETWORK_TOLD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A change of network (an IPv4 address added) is told the park no sooner than its burst
/// settles ([`NETWORK_SETTLE`]), and no sooner than [`NETWORK_SPACING`] after the last tell,
/// which the thread reads and records ([`super::NETWORK_TOLD`]); neither is a loss. Each is
/// a lower bound on when the tell came, which no load can break, and which a thread that
/// tells at once, or never reads or records the last tell, fails. Which events name a change
/// is `RouteChanges::wait_beside`'s (aterm-uds' `names_a_change` laws), and the ten-second
/// bound on a settle is [`network_tell_at`]'s arithmetic. The one law that tells through
/// [`super::NETWORK_TOLD`], so it alone sets it.
#[test]
fn a_network_change_is_told_no_sooner_than_it_settles_or_a_minute_after_the_last_tell() {
    let (heard, kernel) = std::os::unix::net::UnixDatagram::pair().expect("a socket pair");
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    *last_network_tell() = None;
    let watch = watch_network_over(
        aterm_uds::network::RouteChanges::over(heard.into()),
        Arc::clone(&stop),
        Arc::clone(&lost),
        change_told,
    )
    .expect("a network watch");
    let sent = Instant::now();
    kernel.send(&kernel_event(1, 1)).expect("an address added");
    let told = nth_change_told(0);
    assert!(
        told >= sent + NETWORK_SETTLE,
        "told {:?} after the change, before its burst settled",
        told - sent
    );
    let recorded = *last_network_tell();
    assert!(
        recorded.is_some_and(|at| at >= sent + NETWORK_SETTLE && at <= told),
        "the tell was not recorded for the next one's spacing: {recorded:?}"
    );
    // A last tell all but twice the settle a minute ago: the spacing says when, not the
    // settle.
    let last = Instant::now()
        .checked_sub(NETWORK_SPACING - 2 * NETWORK_SETTLE)
        .expect("a clock past its first minute");
    *last_network_tell() = Some(last);
    kernel
        .send(&kernel_event(1, 1))
        .expect("another address added");
    let told = nth_change_told(1);
    assert!(
        told >= last + NETWORK_SPACING,
        "told {:?} short of a minute after the last tell",
        (last + NETWORK_SPACING).duration_since(told)
    );
    assert!(!lost.load(Ordering::Acquire), "a change is no loss");
    stop.store(true, Ordering::Release);
    drop(watch);
}

static LOSS_TOLD: AtomicUsize = AtomicUsize::new(0);

fn loss_told() {
    LOSS_TOLD.fetch_add(1, Ordering::SeqCst);
}

/// A route socket that fails (its peer gone) is a loss, which tells the park to look again
/// and take its timer back.
#[test]
fn a_failed_route_socket_is_a_loss_the_park_is_told() {
    let (heard, peer) = std::os::unix::net::UnixStream::pair().expect("a stream pair");
    drop(peer);
    let stop = Arc::new(AtomicBool::new(false));
    let lost = Arc::new(AtomicBool::new(false));
    let watch = watch_network_over(
        aterm_uds::network::RouteChanges::over(heard.into()),
        Arc::clone(&stop),
        Arc::clone(&lost),
        loss_told,
    )
    .expect("a network watch");
    until("a failed route socket was never a loss", || {
        lost.load(Ordering::Acquire) && LOSS_TOLD.load(Ordering::SeqCst) == 1
    });
    stop.store(true, Ordering::Release);
    drop(watch);
}
