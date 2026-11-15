// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const BOOT: &str = "7bd6dc21fbac480eabe228c34320639d";

/// A process table: `(pid, parent, start)`.
type Table = &'static [(u32, u32, u64)];

/// agh (700) under the engine's python (600) under the cutter (500) under a shell (400).
const CUT: Table = &[(700, 600, 70), (600, 500, 60), (500, 400, 50), (400, 1, 40)];

fn read(table: Table, registry: &Path, own: u32, boot: Option<&str>) -> bool {
    let row = move |pid: u32| table.iter().find(|row| row.0 == pid).copied();
    let boot = boot.map(str::to_owned);
    in_cut_tree_with(
        registry,
        own,
        &Kernel {
            boot_id: &|| boot.clone(),
            start_of: &|pid| row(pid).map(|row| row.2),
            parent_of: &|pid| row(pid).map(|row| row.1),
            uid: aterm_uds::peer::our_uid(),
        },
    )
}

/// A registry holding one record, named as a holder of `named` names it, saying `holder`.
fn registry(named: (u32, u64), holder: (u32, &str, u64)) -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().unwrap();
    let name = aterm_ctl::release_holder::Identity {
        pid: named.0,
        boot: BOOT.into(),
        start: named.1,
    }
    .record_name("0.200.0");
    let record = aterm_json::json!({
        "holder": {"pid": holder.0, "boot": holder.1, "start": holder.2, "node": "n", "host": "h"},
        "step": "publish",
    });
    std::fs::write(dir.path().join(name), record.to_string()).unwrap();
    dir
}

#[test]
fn a_process_below_a_recorded_live_holder_is_in_the_cut_and_one_beside_it_is_not() {
    let cut = registry((500, 50), (500, BOOT, 50));
    assert!(read(CUT, cut.path(), 700, Some(BOOT)), "the engine's agh");
    assert!(read(CUT, cut.path(), 500, Some(BOOT)), "the cutter itself");
    // Negative controls: the shell the cut was started from, and a process of another tree,
    // read the same record and are no part of the cut.
    assert!(!read(CUT, cut.path(), 400, Some(BOOT)));
    const BESIDE: Table = &[(800, 400, 80), (500, 400, 50), (400, 1, 40)];
    assert!(!read(BESIDE, cut.path(), 800, Some(BOOT)));
}

#[test]
fn a_stale_or_forged_record_proves_nothing() {
    // The pid changed hands: the record's holder started at 50, the ancestor now there at 51.
    const REUSED: Table = &[(700, 500, 70), (500, 400, 51), (400, 1, 40)];
    let cut = registry((500, 50), (500, BOOT, 50));
    assert!(!read(REUSED, cut.path(), 700, Some(BOOT)));
    // Another boot's record of the same pid and start, and a boot that does not read.
    assert!(!read(
        CUT,
        cut.path(),
        700,
        Some("00000000000000000000000000000000")
    ));
    assert!(!read(CUT, cut.path(), 700, None));
    // A file named for the ancestor that says another holder, and one whose boot differs.
    for holder in [
        (501, BOOT, 50),
        (500, BOOT, 49),
        (500, "00000000000000000000000000000000", 50),
    ] {
        let forged = registry((500, 50), holder);
        assert!(!read(CUT, forged.path(), 700, Some(BOOT)), "{holder:?}");
    }
    // A record that names the ancestor inside, under a name that is not its holder's.
    let misnamed = registry((999, 50), (500, BOOT, 50));
    assert!(!read(CUT, misnamed.path(), 700, Some(BOOT)));
    // Not a record at all, and no registry.
    let junk = aterm_tempfile::tempdir().unwrap();
    std::fs::write(junk.path().join("0.200.0-500-50.json"), b"cutter").unwrap();
    assert!(!read(CUT, junk.path(), 700, Some(BOOT)));
    assert!(!read(CUT, &junk.path().join("absent"), 700, Some(BOOT)));
    // Positive control for the forgeries above: the true record in the same shape reads.
    assert!(read(CUT, cut.path(), 700, Some(BOOT)));
}

#[test]
fn an_unread_process_ends_the_line_below_the_holder() {
    // 600's parent does not read, so 500 is not proven an ancestor of 700.
    const CUT_OFF: Table = &[(700, 600, 70), (500, 400, 50), (400, 1, 40)];
    let cut = registry((500, 50), (500, BOOT, 50));
    assert!(!read(CUT_OFF, cut.path(), 700, Some(BOOT)));
    // A parent loop ends the walk instead of spinning.
    const LOOP: Table = &[(700, 600, 70), (600, 700, 60)];
    assert!(!read(LOOP, cut.path(), 700, Some(BOOT)));
}

/// The live kernel: a record of this test's own parent, the real ancestor that started it.
/// NEGATIVE CONTROLS: a live process that is no ancestor (a child), and the parent's pid with
/// another start.
#[test]
fn the_kernel_proves_a_real_ancestor_and_refuses_a_child_or_another_start() {
    use aterm_uds::process::{boot_id, read_process_parent, read_process_start};
    let own = i32::try_from(std::process::id()).unwrap();
    let parent = read_process_parent(own).expect("this test has a parent");
    let start = |pid: u32| read_process_start(i32::try_from(pid).unwrap()).map(|s| s.boot.0);
    let boot = boot_id().expect("this host names its boot");
    let record = |pid: u32, start: u64| {
        let dir = aterm_tempfile::tempdir().unwrap();
        let identity = aterm_ctl::release_holder::Identity {
            pid,
            boot: boot.clone(),
            start,
        };
        let body = aterm_json::json!({"holder": {"pid": pid, "boot": boot, "start": start}});
        std::fs::write(
            dir.path().join(identity.record_name("0.200.0")),
            body.to_string(),
        )
        .unwrap();
        dir
    };
    let parent_start = start(parent).expect("the parent's start reads");
    assert!(in_cut_tree(record(parent, parent_start).path()));
    assert!(!in_cut_tree(
        record(parent, parent_start.wrapping_add(1)).path()
    ));
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let beside = start(child.id()).map(|start| in_cut_tree(record(child.id(), start).path()));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(beside, Some(false), "a live child is no ancestor");
}
