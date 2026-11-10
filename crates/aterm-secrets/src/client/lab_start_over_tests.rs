// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A16: a lab identity under a release app starts over ([`start_over_as`]). Each test states what
//! the process is through [`crate::Making`] and runs in a scratch home; none reads this Mac's.

use super::*;

/// 2026-11-05T12:00:00Z.
const NOW: u64 = 1_793_880_000;

fn release() -> crate::Making {
    crate::Making::default()
}

fn debug_build() -> crate::Making {
    crate::Making {
        debug_build: true,
        ..crate::Making::default()
    }
}

/// A scratch home whose vault folder holds a lab identity a debug build made, and a file of the
/// lab's own vault state beside it.
fn lab_home() -> (aterm_tempfile::TempDir, Paths, Identity) {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    let lab = machine_identity_as(&paths, debug_build()).expect("a lab identity");
    fsx::write_atomic(&paths.source_file(), b"# the lab's own vault\n", 0o600).expect("source");
    (home, paths, lab)
}

fn siblings(paths: &Paths) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(paths.dir.parent().expect("parent"))
        .expect("read the parent")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// A release-shaped process given a lab-marked identity renames the vault folder to
/// `secrets.lab-<date>`, keeps everything in it, and starts as a new Mac: a new, unmarked
/// identity that no join refuses, and a value-free record of what was set aside.
#[test]
fn a_release_process_sets_a_lab_identity_aside_and_makes_an_unmarked_one() {
    let (_home, paths, lab) = lab_home();
    assert_eq!(
        lab_mark_as(&paths, release()),
        Some(crate::LabMark::DebugBuild)
    );
    assert!(refuse_lab_identity_as(&paths, release()).is_err());

    let set = start_over_as(&paths, release(), NOW)
        .expect("set aside")
        .expect("a lab identity is set aside");
    let kept = paths.dir.with_file_name("secrets.lab-2026-11-05");
    assert_eq!(
        set,
        LabSetAside {
            mark: crate::LabMark::DebugBuild,
            kept: kept.clone(),
        }
    );
    assert_eq!(siblings(&paths), ["secrets", "secrets.lab-2026-11-05"]);

    // Nothing of the lab's state is lost: its folder holds its key and its files as they were.
    let old = Paths {
        home: paths.home.clone(),
        dir: kept.clone(),
    };
    let (key, mark) = read_identity_marked(&old).expect("read").expect("kept");
    assert_eq!(key.recipient(), lab.recipient());
    assert_eq!(mark, Some(crate::LabMark::DebugBuild));
    assert_eq!(
        std::fs::read(old.source_file()).expect("kept source"),
        b"# the lab's own vault\n"
    );

    // The new folder is a new Mac's: an unmarked identity, no vault, nothing refused.
    let (new, mark) = read_identity_marked(&paths).expect("read").expect("made");
    assert_ne!(new.recipient(), lab.recipient());
    assert_eq!(mark, None);
    assert!(config(&paths).expect("config").is_none());
    assert!(!paths.source_file().exists());
    refuse_lab_identity_as(&paths, release()).expect("a new Mac joins the live ALab");
    assert_eq!(
        machine_identity_as(&paths, release())
            .expect("kept")
            .recipient(),
        new.recipient()
    );

    // The record names the mark and the folder, and no key.
    assert_eq!(lab_set_aside(&paths).expect("read"), Some(set));
    let record = std::fs::read_to_string(paths.dir.join(LAB_SET_ASIDE_FILE)).expect("record");
    assert_eq!(
        record,
        format!("{NOW}\tdebug-build\tsecrets.lab-2026-11-05\n")
    );
    for key in [&lab, &new] {
        assert!(!record.contains(key.to_bech32().as_str().unwrap_or("-")));
        assert!(!record.contains(&key.recipient().to_bech32()));
    }

    // Started over once: the next start finds an unmarked identity and changes nothing.
    assert_eq!(start_over_as(&paths, release(), NOW).expect("again"), None);
    assert_eq!(siblings(&paths), ["secrets", "secrets.lab-2026-11-05"]);
}

/// A process that would itself make a lab identity keeps the lab identity it finds: a debug
/// build, a build in a virtual machine, a test binary, a lab lane's process.
///
/// NEGATIVE CONTROL: drop `start_over_as`'s `crate::lab_identity(making)` return: each of these
/// sets the folder aside, and this test fails on the first.
#[test]
fn a_process_that_makes_lab_identities_keeps_the_one_it_finds() {
    for making in [
        debug_build(),
        crate::Making {
            virtual_machine: true,
            ..crate::Making::default()
        },
        crate::Making {
            test_binary: true,
            ..crate::Making::default()
        },
        crate::Making {
            lab_lane: true,
            ..crate::Making::default()
        },
    ] {
        let (_home, paths, lab) = lab_home();
        assert_eq!(
            start_over_as(&paths, making, NOW).expect("kept"),
            None,
            "{making:?}"
        );
        assert_eq!(siblings(&paths), ["secrets"], "{making:?}");
        let (key, mark) = read_identity_marked(&paths).expect("read").expect("kept");
        assert_eq!(key.recipient(), lab.recipient(), "{making:?}");
        assert_eq!(mark, Some(crate::LabMark::DebugBuild), "{making:?}");
        assert!(paths.source_file().exists(), "{making:?}");
        assert_eq!(lab_set_aside(&paths).expect("read"), None);
    }
}

/// A release-shaped process with an unmarked identity, or none, changes nothing: a developer's
/// own Mac keeps its seat, and a new Mac's identity is made by the ordinary path.
#[test]
fn an_unmarked_identity_or_none_is_left_as_it_is() {
    let home = aterm_tempfile::tempdir().expect("owned home");
    let paths = Paths::for_home(home.path());
    assert_eq!(start_over_as(&paths, release(), NOW).expect("none"), None);
    assert!(
        !paths.dir.exists(),
        "a home with no vault folder gains none"
    );
    let made = machine_identity_as(&paths, release()).expect("made");
    assert_eq!(
        start_over_as(&paths, release(), NOW).expect("unmarked"),
        None
    );
    assert_eq!(siblings(&paths), ["secrets"]);
    assert_eq!(
        read_identity(&paths)
            .expect("read")
            .expect("kept")
            .recipient(),
        made.recipient()
    );
}

/// A name already taken beside the vault folder, a folder or a file, is never written over: the
/// set-aside takes the next free one.
#[test]
fn a_set_aside_takes_a_free_name_and_overwrites_nothing() {
    let (_home, paths, _) = lab_home();
    let taken = paths.dir.with_file_name("secrets.lab-2026-11-05");
    std::fs::create_dir(&taken).expect("an earlier set-aside");
    std::fs::write(taken.join("identity"), b"earlier").expect("its file");
    // An empty folder, which a plain rename would replace.
    std::fs::create_dir(paths.dir.with_file_name("secrets.lab-2026-11-05-2")).expect("empty");
    std::fs::write(
        paths.dir.with_file_name("secrets.lab-2026-11-05-3"),
        b"a file",
    )
    .expect("file");

    let set = start_over_as(&paths, release(), NOW)
        .expect("set aside")
        .expect("a lab identity is set aside");
    assert_eq!(
        set.kept,
        paths.dir.with_file_name("secrets.lab-2026-11-05-4")
    );
    assert_eq!(
        std::fs::read(taken.join("identity")).expect("kept"),
        b"earlier"
    );
    assert_eq!(
        std::fs::read_dir(paths.dir.with_file_name("secrets.lab-2026-11-05-2"))
            .expect("still a folder")
            .count(),
        0
    );
    assert_eq!(
        std::fs::read(paths.dir.with_file_name("secrets.lab-2026-11-05-3")).expect("kept"),
        b"a file"
    );
    assert!(set.kept.join("identity").exists());
    assert_eq!(lab_set_aside(&paths).expect("read"), Some(set));
}

/// [`lab_mark`] for a process that is `making`.
fn lab_mark_as(paths: &Paths, making: crate::Making) -> Option<crate::LabMark> {
    lab_standing(paths, making)
        .expect("read")
        .map(|(mark, _)| mark)
}

/// A state lock taken before a set-aside guards the kept folder, not the vault's path: a take
/// that wakes holding it sees that ([`StateLock::guards`]) and takes the new folder's lock, which
/// the stale holder does not hold.
///
/// NEGATIVE CONTROL: make `guards` return `true`: the first assertion fails, and a take waiting
/// on the old file would return a lock on the kept folder.
#[test]
fn a_lock_on_the_set_aside_folder_guards_nothing_at_the_vaults_path() {
    let (_home, paths, _) = lab_home();
    let stale = StateLock::take_at(&paths, &paths.lock_file()).expect("the lab folder's lock");
    assert!(stale.guards(&paths.lock_file()));
    std::fs::rename(
        &paths.dir,
        paths.dir.with_file_name("secrets.lab-2026-11-05"),
    )
    .expect("aside");
    machine_identity_as(&paths, release()).expect("a new Mac's identity");
    assert!(!stale.guards(&paths.lock_file()));
    // The new folder's lock is free though the stale one is still held.
    let fresh = StateLock::try_take(&paths)
        .expect("take")
        .expect("the new folder's lock is not the stale one");
    assert!(fresh.guards(&paths.lock_file()));
    drop(stale);
}

/// A debug build's process that makes a lab identity in the new folder before the set-aside makes
/// its own wins the write: the set-aside says so, naming the kept folder, rather than report a new
/// Mac, and the next start sets that identity aside too.
///
/// NEGATIVE CONTROL: drop the re-read of the new folder's mark after `machine_identity_making`:
/// the first start reports a new Mac whose identity is a lab one.
#[test]
fn a_lab_identity_made_in_the_new_folder_first_is_reported_and_set_aside_next() {
    let (_home, paths, _) = lab_home();
    let refused = start_over_then(
        &paths,
        release(),
        NOW,
        || {},
        || {
            machine_identity_as(&paths, debug_build()).expect("a debug build's identity");
        },
    )
    .expect_err("not a new Mac");
    let kept = paths.dir.with_file_name("secrets.lab-2026-11-05");
    assert!(
        refused.to_string().contains(&kept.display().to_string()),
        "{refused}"
    );
    assert!(refused.to_string().contains("debug-build"), "{refused}");
    assert!(kept.join("identity").exists());

    let set = start_over_as(&paths, release(), NOW)
        .expect("set aside")
        .expect("the second lab identity is set aside");
    assert_eq!(
        set.kept,
        paths.dir.with_file_name("secrets.lab-2026-11-05-2")
    );
    assert_eq!(
        read_identity_marked(&paths).expect("read").expect("made").1,
        None
    );
}

/// A set-aside that waited on a lock file another set-aside moved away with its folder takes the
/// lock at the vault's path before it renames anything ([`StateLock::guards`]): where a debug
/// build's process made a lab identity in a new folder and observes that folder's lock, the
/// set-aside waits for it, renames nothing while it waits, and once the observation ends sets that
/// folder aside in the same call, holding its lock; where nothing at the path holds a lab
/// identity, it sets nothing aside.
///
/// NEGATIVE CONTROLS (2026-11-10): with the set-aside keeping the lock it woke holding, it renames
/// the new folder at once, under the observation (`a set-aside renames only a folder whose lock it
/// holds`); refusing busy where its lock guards nothing, it never takes the new folder's lock (`a
/// set-aside renames only a folder whose lock it holds`); and with no read of the path before it
/// takes the lock again, the set-aside with nothing at the path fails to open a lock there
/// (`nothing to set aside`).
#[test]
fn a_set_aside_that_woke_on_a_moved_lock_renames_only_a_folder_whose_lock_it_holds() {
    use std::{cell::Cell, cell::RefCell, rc::Rc};
    let (_home, paths, _) = lab_home();
    let kept = paths.dir.with_file_name("secrets.lab-2026-11-05");
    let next = paths.dir.with_file_name("secrets.lab-2026-11-05-2");
    let observed: Rc<RefCell<Option<StateLock>>> = Rc::default();
    // Whether the new folder was renamed when the set-aside first met the observation.
    let waited: Rc<Cell<Option<bool>>> = Rc::default();
    let set = aterm_uds::file_lock::with_lock_wait_observer(
        {
            let (observed, waited, next) = (observed.clone(), waited.clone(), next.clone());
            move |_| {
                if waited.get().is_none() {
                    waited.set(Some(next.exists()));
                }
                drop(observed.borrow_mut().take());
            }
        },
        || {
            start_over_then(
                &paths,
                release(),
                NOW,
                || {
                    // Another set-aside moved the folder, and the lock this took, while this
                    // waited; a debug build's process made a lab identity in a new folder, whose
                    // lock it observes.
                    std::fs::rename(&paths.dir, &kept).expect("aside");
                    machine_identity_as(&paths, debug_build()).expect("a debug build's identity");
                    drop(
                        StateLock::try_take(&paths)
                            .expect("take")
                            .expect("the new folder's lock is free"),
                    );
                    *observed.borrow_mut() = Some(
                        StateLock::observe(&paths)
                            .expect("observe")
                            .expect("no writer"),
                    );
                },
                || {},
            )
        },
    );
    assert_eq!(
        waited.get(),
        Some(false),
        "a set-aside renames only a folder whose lock it holds"
    );
    let set = set
        .expect("set aside")
        .expect("the debug build's identity is set aside");
    assert_eq!(set.kept, next);
    assert_eq!(set.mark, crate::LabMark::DebugBuild);
    assert_eq!(
        read_identity_marked(&paths).expect("read").expect("made").1,
        None
    );

    let (_home, paths, _) = lab_home();
    let kept = paths.dir.with_file_name("secrets.lab-2026-11-05");
    let none = start_over_then(
        &paths,
        release(),
        NOW,
        || std::fs::rename(&paths.dir, &kept).expect("aside"),
        || {},
    );
    assert!(matches!(none, Ok(None)), "nothing to set aside: {none:?}");
    assert!(!paths.dir.exists());
    assert!(kept.join("identity").exists());
}

/// `aterm secrets status` names the set-aside and its folder, and no key.
#[test]
fn status_names_what_was_set_aside() {
    let (_home, paths, lab) = lab_home();
    let set = start_over_as(&paths, release(), NOW)
        .expect("set aside")
        .expect("set");
    let line = lab_set_aside(&paths)
        .expect("read")
        .expect("recorded")
        .status_line();
    assert_eq!(
        line,
        format!(
            "set aside: a lab identity (debug-build), kept as {}; this Mac started over as a new one",
            set.kept.display()
        )
    );
    assert!(!line.contains(&lab.recipient().to_bech32()));
}
