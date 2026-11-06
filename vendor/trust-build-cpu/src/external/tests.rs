use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};

use super::*;

fn record(dir: &Path, name: &str, text: &str, lock: bool) -> File {
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join(name))
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
    if lock {
        file.lock().unwrap();
    }
    file
}

fn row(mode: &str, budget: u32, lease: char) -> String {
    serde_json::json!({"cpu_mode":mode,"cpu_budget":budget,"pool_slots":4,
        "lease_id":lease.to_string().repeat(32)})
    .to_string()
}

#[test]
fn fixed_leases_deduplicate_and_cooperating_workers_are_separate() {
    let dir = tempfile::tempdir().unwrap();
    let _first = record(dir.path(), "slot-0", &row("fixed-v1", 14, 'a'), true);
    let _second = record(dir.path(), "slot-1", &row("fixed-v1", 14, 'a'), true);
    let _worker = record(dir.path(), "slot-2", &row("workers-v1", 7, 'b'), true);
    let _stale = record(dir.path(), "slot-3", "not json", false);
    assert_eq!(reserved_cpus(dir.path(), 28, 4).unwrap(), 14);
    drop(_first);
    drop(_second);
    assert_eq!(reserved_cpus(dir.path(), 28, 4).unwrap(), 0);
}

#[test]
fn exclusive_fence_and_unknown_live_metadata_veto_new_workers() {
    let dir = tempfile::tempdir().unwrap();
    let fence = record(dir.path(), "admission.lock", "{}", true);
    assert_eq!(reserved_cpus(dir.path(), 28, 4).unwrap(), 28);
    drop(fence);
    let _unknown = record(dir.path(), "slot-0", "bad", true);
    assert_eq!(reserved_cpus(dir.path(), 28, 4).unwrap(), 28);
}

#[test]
fn inherited_duplicate_stays_reserved_and_legacy_has_a_fixed_share() {
    let dir = tempfile::tempdir().unwrap();
    let lease = record(
        dir.path(),
        "slot-0",
        r#"{"pid":4,"cmd":"legacy","cwd":"/fixture","started":0}"#,
        true,
    );
    let inherited = lease.try_clone().unwrap();
    drop(lease);
    assert_eq!(reserved_cpus(dir.path(), 28, 4).unwrap(), 7);
    drop(inherited);
    assert_eq!(reserved_cpus(dir.path(), 28, 4).unwrap(), 0);
}

#[test]
fn unsafe_files_and_symlinks_are_not_treated_as_free_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let original = record(dir.path(), "original", "{}", true);
    symlink(dir.path().join("original"), dir.path().join("slot-0")).unwrap();
    assert!(reserved_cpus(dir.path(), 28, 4).is_err());
    std::fs::remove_file(dir.path().join("slot-0")).unwrap();
    drop(original);
    let _unsafe = record(dir.path(), "slot-0", "{}", false);
    std::fs::set_permissions(dir.path().join("slot-0"), std::fs::Permissions::from_mode(0o666))
        .unwrap();
    assert!(reserved_cpus(dir.path(), 28, 4).is_err());
}
