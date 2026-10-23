// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The path test against spellings alone: no file system is asked. The walker against
//! trees the tests create is `walk_tests.rs`.

use std::path::{Path, PathBuf};

use aterm_sysprobe::volumes::{Mount, Table};

use crate::refuse::{
    Deny, Refusal, canonical_for, is_mount_point, refuse, unsafe_mounts_in, volume_refusal, within,
};

const HOME: &str = "/Users//a";

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn mount(on: &str, fstype: &str, local: bool, automounted: bool) -> Mount {
    Mount {
        on: p(on),
        from: String::new(),
        fstype: fstype.to_owned(),
        local,
        automounted,
        removable: false,
        read_only: false,
        container: None,
    }
}

fn table() -> Table {
    Table {
        mounts: vec![
            mount("/", "apfs", true, false),
            mount("/System/Volumes/Data", "apfs", true, false),
            mount("/System/Volumes/Data/home", "autofs", true, true),
            mount("/Volumes/Share", "smbfs", false, false),
            mount("/Volumes/Stick", "msdos", true, false),
            mount("/Users//a/fuse", "macfuse", true, false),
            mount("/Users//a/src/nfs", "nfs", false, false),
        ],
        complete: true,
        firmlinks: vec![(p("/Users"), p("/System/Volumes/Data/Users"))],
    }
}

fn deny() -> Deny {
    Deny::for_home(Path::new(HOME), &[], &[]).with_fold_case(true)
}

fn refused(path: &str) -> Option<Refusal> {
    refuse(None, Path::new(path), &table(), &deny())
}

#[test]
fn every_privacy_protected_place_is_refused_before_any_syscall() {
    for path in [
        "/Users//a/Documents",
        "/Users//a/Documents/project",
        "/Users//a/Desktop/x",
        "/Users//a/Downloads",
        "/Users//a/Pictures/Photos Library.photoslibrary",
        "/Users//a/Movies",
        "/Users//a/Music/Music",
        "/Users//a/Library",
        "/Users//a/Library/Mail/V10",
        "/Users//a/Library/Messages/chat.db",
        "/Users//a/Library/Safari",
        "/Users//a/Library/Containers/com.apple.mail",
        "/Users//a/Library/Group Containers/x",
        "/Users//a/Library/CloudStorage/Provider",
        "/Users//a/Library/Mobile Documents/com~apple~CloudDocs",
        "/Users//a/Library/Caches",
        "/Users//a/Library/Caches/com.apple.Safari",
        "/Users//a/Library/Application Support/Google/Chrome",
        "/Users//a/Library/Application Support/AddressBook",
        "/Users//a/.Trash",
        "/Volumes/Stick",
        "/Volumes/Share/x",
        "/System/Volumes/Data/Users//a/Documents/x",
        // APFS folds case, so a shell's `cd ~/documents` is the same folder.
        "/Users//a/documents/x",
        "/USERS/A/DESKTOP",
        "/Users//a/library/mail",
    ] {
        assert_eq!(refused(path), Some(Refusal::Protected), "{path}");
    }
}

#[test]
fn other_users_homes_are_refused() {
    for path in [
        "/Users//b",
        "/Users//b/src",
        "/Users//Shared/x",
        "/System/Volumes/Data/Users//b/x",
    ] {
        assert_eq!(refused(path), Some(Refusal::OtherUser), "{path}");
    }
}

#[test]
fn aterms_own_library_folders_are_walkable() {
    for path in [
        "/Users//a/Library/Caches/aterm",
        "/Users//a/Library/Caches/aterm/cargo-build.noindex/Users//a/w/.cargo-build",
        "/Users//a/Library/Application Support/aterm",
        "/Users//a/Library/Application Support/aterm/atpkg",
    ] {
        assert_eq!(refused(path), None, "{path}");
    }
}

#[test]
fn broad_roots_get_one_level() {
    for path in [
        "/",
        "/Users",
        "/Users//a",
        "/System/Volumes/Data",
        "/System/Volumes/Data/Users",
        "/System/Volumes/Data/Users//a",
        "/System",
        "/System/Volumes",
        "/USERS/A",
    ] {
        assert_eq!(refused(path), Some(Refusal::TooBroad), "{path}");
    }
}

#[test]
fn ordinary_folders_pass() {
    for path in [
        "/Users//a/aterm",
        "/Users//a/aterm/target.noindex/debug",
        "/System/Volumes/Data/Users//a/aterm",
        "/private/tmp/claude-502/p/s",
        "/Users//a/Documents-old",
        "/Users//a/LibraryX",
        "/opt/homebrew",
    ] {
        assert_eq!(refused(path), None, "{path}");
    }
}

#[test]
fn remote_automounted_and_unknown_volumes_are_never_asked() {
    assert_eq!(refused("/Users//a/fuse/x"), Some(Refusal::Remote));
    assert_eq!(refused("/Users//a/src/nfs/y"), Some(Refusal::Remote));
    assert_eq!(
        refused("/System/Volumes/Data/home/a"),
        Some(Refusal::Protected)
    );
    let mut t = table();
    t.mounts.push(mount("/net", "autofs", true, true));
    assert_eq!(
        refuse(None, Path::new("/net/host/x"), &t, &deny()),
        Some(Refusal::Automounted)
    );
    t.complete = false;
    assert_eq!(
        refuse(None, Path::new("/Users//a/aterm"), &t, &deny()),
        Some(Refusal::MountsUnknown)
    );
    assert_eq!(
        volume_refusal(Path::new("/Users//a/aterm"), &table(), true),
        None
    );
}

#[test]
fn the_logical_spelling_is_tested_too() {
    // A shell's `/tmp/x` is the kernel's `/private/tmp/x`.
    assert_eq!(
        canonical_for(Path::new("/tmp/x"), true),
        Some(p("/private/tmp/x"))
    );
    assert_eq!(canonical_for(Path::new("/tmp/x"), false), Some(p("/tmp/x")));
    assert_eq!(
        canonical_for(Path::new("/var/folders/./x/"), true),
        Some(p("/private/var/folders/x"))
    );
    assert_eq!(canonical_for(Path::new("/a/../b"), true), None);
    assert_eq!(canonical_for(Path::new("rel"), true), None);
    // A canonical spelling that passes cannot carry a logical one that does not.
    assert_eq!(
        refuse(
            Some(Path::new("/Users//a/Desktop/w")),
            Path::new("/Users//a/w"),
            &table(),
            &deny()
        ),
        Some(Refusal::Protected)
    );
    assert_eq!(
        refuse(
            Some(Path::new("w")),
            Path::new("/Users//a/w"),
            &table(),
            &deny()
        ),
        Some(Refusal::Relative)
    );
}

#[test]
fn an_opt_in_lifts_a_protected_default_but_never_another_users_home() {
    let d = Deny::for_home(
        Path::new(HOME),
        &[],
        &[
            p("/Users//a/Documents/project"),
            p("/Users//a/Library/Developer"),
            p("/Users//b/src"),
        ],
    )
    .with_fold_case(true);
    let t = table();
    assert_eq!(
        refuse(None, Path::new("/Users//a/Documents/project/x"), &t, &d),
        None
    );
    assert_eq!(
        refuse(None, Path::new("/Users//a/Library/Developer/Xcode"), &t, &d),
        None
    );
    assert_eq!(
        refuse(None, Path::new("/Users//a/Documents/other"), &t, &d),
        Some(Refusal::Protected)
    );
    assert_eq!(
        refuse(None, Path::new("/Users//b/src"), &t, &d),
        Some(Refusal::OtherUser)
    );
}

#[test]
fn an_override_adds_a_protected_root_and_its_ancestors_turn_broad() {
    let d =
        Deny::for_home(Path::new(HOME), &[String::from("~/w/secret")], &[]).with_fold_case(false);
    let t = table();
    assert_eq!(
        refuse(None, Path::new("/Users//a/w/secret/x"), &t, &d),
        Some(Refusal::Protected)
    );
    assert_eq!(
        refuse(None, Path::new("/Users//a/w"), &t, &d),
        Some(Refusal::TooBroad)
    );
    // Case is not folded where the file system does not fold it.
    assert_eq!(refuse(None, Path::new("/Users//a/w/SECRET"), &t, &d), None);
}

#[test]
fn unsafe_mount_points_are_named_for_their_parent() {
    let t = table();
    assert_eq!(
        unsafe_mounts_in(Path::new("/Users//a/src"), &t, true),
        vec!["nfs"]
    );
    assert_eq!(
        unsafe_mounts_in(Path::new("/Users//a"), &t, true),
        vec!["fuse"]
    );
    assert!(unsafe_mounts_in(Path::new("/Users//a/aterm"), &t, true).is_empty());
}

/// Removable volumes are refused at any mount point and skipped when listing their parent.
#[test]
fn a_removable_volume_mounted_anywhere_is_never_asked() {
    let mut t = table();
    t.mounts.push(Mount {
        removable: true,
        ..mount("/Users//a/src/card", "msdos", true, false)
    });
    assert_eq!(
        refuse(None, Path::new("/Users//a/src/card"), &t, &deny()),
        Some(Refusal::TooBroad),
        "a mount point is a volume's whole: broad, as every volume's is"
    );
    assert_eq!(
        refuse(None, Path::new("/Users//a/src/card/DCIM/x"), &t, &deny()),
        Some(Refusal::Removable)
    );
    assert_eq!(
        volume_refusal(Path::new("/users/a/SRC/card/x"), &t, true),
        Some(Refusal::Removable),
        "in any case where the file system folds it"
    );
    let mut skipped = unsafe_mounts_in(Path::new("/Users//a/src"), &t, true);
    skipped.sort();
    assert_eq!(skipped, vec!["card", "nfs"]);
    assert_eq!(
        refuse(None, Path::new("/Users//a/src/other"), &t, &deny()),
        None,
        "its neighbour is walked"
    );
}

/// Case, separator, `.` component, and firmlink prefix spellings of `path`.
fn spellings_of(path: &str) -> Vec<String> {
    let alternating: String = path
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if i % 2 == 0 {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let mut out = Vec::new();
    for cased in [
        path.to_owned(),
        path.to_ascii_lowercase(),
        path.to_ascii_uppercase(),
        alternating,
    ] {
        for prefix in [
            "",
            "/System/Volumes/Data",
            "/system/volumes/data",
            "/SYSTEM/VOLUMES/DATA",
        ] {
            let s = format!("{prefix}{cased}");
            out.push(format!("{s}/"));
            out.push(s.replace('/', "//"));
            out.push(s.replace('/', "/./"));
            out.push(s);
        }
    }
    out
}

/// Every denied path and child path is refused across all canonical and logical spellings.
#[test]
fn a_denied_place_is_refused_in_every_spelling_the_canonicaliser_reads() {
    let d = deny();
    let t = table();
    let mut places: Vec<String> = d
        .roots()
        .iter()
        .filter_map(|r| r.to_str().map(str::to_owned))
        .collect();
    assert!(places.len() >= 10, "the deny set: {places:?}");
    places.extend(
        [
            "/Users//a/Library",
            "/Users//a/Library/Mail",
            "/Users//a/Library/Caches/com.apple.Safari",
            "/Users//b",
            "/Users//b/src",
        ]
        .map(str::to_owned),
    );
    let innocent = Path::new("/Users//a/aterm");
    assert_eq!(refuse(None, innocent, &t, &d), None);
    let mut checked = 0usize;
    for place in &places {
        for target in [place.clone(), format!("{place}/deep/er")] {
            for spelled in spellings_of(&target) {
                let spelled = Path::new(&spelled);
                let canon = canonical_for(spelled, true).expect("absolute, no ..");
                assert!(
                    refuse(None, &canon, &t, &d).is_some(),
                    "{spelled:?} (canonical {canon:?}) passed"
                );
                assert!(
                    refuse(Some(spelled), innocent, &t, &d).is_some(),
                    "the logical {spelled:?} passed beside an innocent canonical form"
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, places.len() * 2 * 64);
}

/// Remote shares and volumes mounted inside them are refused across all spellings.
#[test]
fn a_share_is_refused_in_every_spelling_and_through_a_volume_inside_it() {
    let d = deny();
    let mut t = table();
    t.mounts
        .push(mount("/Users//a/src/nfs/inner", "apfs", true, false));
    for (path, why) in [
        ("/Users//a/src/nfs/y", Refusal::Remote),
        ("/Users//a/src/nfs/inner/z", Refusal::Remote),
        ("/Users//a/fuse/x", Refusal::Remote),
    ] {
        for spelled in spellings_of(path) {
            let canon = canonical_for(Path::new(&spelled), true).expect("absolute");
            assert_eq!(refuse(None, &canon, &t, &d), Some(why), "{spelled}");
        }
    }
    for spelled in spellings_of("/Users//a/src") {
        let canon = canonical_for(Path::new(&spelled), true).expect("absolute");
        assert_eq!(unsafe_mounts_in(&canon, &t, true), vec!["nfs"], "{spelled}");
    }
    // Where the file system keeps case, another case is another folder.
    assert!(unsafe_mounts_in(Path::new("/users/a/src"), &t, false).is_empty());
    assert_eq!(
        volume_refusal(Path::new("/users/a/src/nfs/y"), &t, false),
        None
    );
}

#[test]
fn the_shared_relations_read_every_spelling() {
    let t = table();
    assert!(is_mount_point(Path::new("/users/A/SRC/nfs"), &t, true));
    assert!(is_mount_point(
        Path::new("/System/Volumes/Data/Users//a/src/nfs"),
        &t,
        true
    ));
    assert!(!is_mount_point(Path::new("/users/a/src/nfs"), &t, false));
    assert!(!is_mount_point(Path::new("/Users//a/src"), &t, true));
    assert!(within(Path::new("/Users//a"), Path::new("/USERS"), true));
    assert!(within(
        Path::new("/Users//a"),
        Path::new("/system/volumes/data/Users"),
        true
    ));
    assert!(!within(
        Path::new("/Users//a"),
        Path::new("/Users//a/w"),
        true
    ));
    assert!(!within(Path::new("/Users//a"), Path::new("/USERS"), false));
    // The system volume reads `/TMP` as `/tmp`, a link to `/private/tmp`.
    assert_eq!(
        canonical_for(Path::new("/TMP/x"), true),
        Some(p("/private/TMP/x"))
    );
}
