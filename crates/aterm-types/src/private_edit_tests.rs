// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_file_in_a_marked_folder_is_private_and_nothing_else_is() {
    assert!(is_private(Path::new(
        "/private/var/folders/x/T/.ubd/att-edit-1_ab.private.noindex/address.tsv"
    )));
    assert!(is_private(Path::new("att-edit-x.private.noindex/a/b.tsv")));
    // The negative controls: an ordinary scratch folder, a file named like the marker, and
    // the bare suffix.
    assert!(!is_private(Path::new(
        "/tmp/.ubd/att-edit-1_ab.noindex/address.tsv"
    )));
    assert!(!is_private(Path::new("/tmp/x/notes.private.noindex.tsv")));
    assert!(!is_private(Path::new("/tmp/.private.noindex/a.tsv")));
}

#[test]
fn a_scratch_folder_names_its_maker() {
    assert_eq!(
        scratch_maker("att-edit-4242_0123abcd_1.private.noindex"),
        Some(4242)
    );
    // The negative controls: no pid, pid 1, another suffix, another prefix, a bare marker.
    assert_eq!(scratch_maker("att-edit-x_1.private.noindex"), None);
    assert_eq!(scratch_maker("att-edit-1_ab.private.noindex"), None);
    assert_eq!(scratch_maker("att-edit-4242_ab.noindex"), None);
    assert_eq!(scratch_maker("other-4242_ab.private.noindex"), None);
    assert_eq!(scratch_maker("att-edit-.private.noindex"), None);
}

/// A dead maker's folder goes, with its file; a live maker's, `keep`, another name and a
/// plain file stay; and where no process can be read as running (this one included),
/// nothing goes.
/// NEGATIVE CONTROL: with `sweep_with` removing regardless of `runs`, the live maker's folder
/// goes too: `` assertion `left == right` failed left: 2 right: 1 ``.
#[test]
fn only_a_dead_makers_scratch_folder_is_swept() {
    let root = crate::test_scratch_dir("edit");
    let folder = |name: &str| {
        let path = root.path().join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("address.tsv"), "pk\tpassword\n").unwrap();
        path
    };
    let own = i32::try_from(std::process::id()).unwrap();
    let dead = folder("att-edit-900001_aa_1.private.noindex");
    let live = folder(&format!("att-edit-{own}_bb_1.private.noindex"));
    let kept = folder("att-edit-900002_cc_1.private.noindex");
    let other = folder("att-edit-900003_dd_1.noindex");
    std::fs::write(root.path().join("att-edit-900004_ee.private.noindex"), "x").unwrap();
    let runs = |pid: i32| pid == own;
    assert_eq!(sweep_with(root.path(), Some(&kept), &|pid| runs(pid)), 1);
    assert!(!dead.exists(), "dead maker's folder swept");
    assert!(live.exists(), "live maker's folder kept");
    assert!(kept.exists() && other.exists());
    assert!(
        root.path()
            .join("att-edit-900004_ee.private.noindex")
            .is_file()
    );
    let blind = folder("att-edit-900005_ff_1.private.noindex");
    assert_eq!(sweep_with(root.path(), None, &|_| false), 0);
    assert!(blind.exists(), "nothing goes where liveness is unknown");
}
