// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

fn world() -> aterm_tempfile::TempDir {
    let dir = aterm_tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

fn slot_file(root: &Path) -> File {
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .mode(0o600)
        .open(root.join("slot-0"))
        .unwrap()
}

#[test]
fn old_slot_probe_never_waits_or_counts_shared_epoch_fences() {
    let root = world();
    drop(slot_file(root.path()));
    let path = root.path().join("slot-0");
    let lock = aterm_uds::file_lock::LockFile::share(
        &path,
        std::time::Instant::now(),
        "shared fixture slot",
    )
    .unwrap();
    assert!(!exclusive(&File::open(root.path().join("slot-0")).unwrap()).unwrap());
    assert!(
        read(root.path(), 14).holders.is_empty(),
        "SH fences are not CPU reservations"
    );
    drop(lock);
    let lock = aterm_uds::file_lock::LockFile::take_existing(
        &path,
        std::time::Instant::now(),
        "exclusive fixture slot",
    )
    .unwrap();
    assert!(exclusive(&File::open(root.path().join("slot-0")).unwrap()).unwrap());
    assert_eq!(
        read(root.path(), 14),
        HeldSlots::unknown(14),
        "held empty/unreadable metadata is conservative"
    );
    drop(lock);
    std::fs::write(root.path().join("slot-0"), br#"{"pid":999999,"started":1}"#).unwrap();
    assert!(
        read(root.path(), 14).holders.is_empty(),
        "dead metadata without a lock holds nothing"
    );
    assert!(read(&root.path().join("absent"), 14).holders.is_empty());
}

#[test]
fn both_pool_names_and_untrusted_named_files_remain_accounted() {
    let root = world();
    let runtime = root.path().join("runtime-v2");
    std::fs::create_dir(&runtime).unwrap();
    drop(slot_file(&runtime));
    let lock = aterm_uds::file_lock::LockFile::take_existing(
        &runtime.join("slot-0"),
        std::time::Instant::now(),
        "exclusive fixture slot",
    )
    .unwrap();
    assert_eq!(read(root.path(), 14), HeldSlots::unknown(14));
    drop(lock);
    let slot = root.path().join("slot-0");
    std::os::unix::fs::symlink("missing", &slot).unwrap();
    assert_eq!(
        read(root.path(), 14),
        HeldSlots::unknown(14),
        "a named symlink is never followed or free"
    );
}

#[test]
fn verified_budgets_deduplicate_only_the_same_complete_lease() {
    let owner = Holder {
        pid: 42,
        birth_us: 100,
    };
    let value = aterm_json::json!({"pid":42,"started":1,"cpu_mode":"fixed-v1","cpu_budget":4,"pool_slots":6,"lease_id":"0123456789abcdef0123456789abcdef"});
    let (row, lease) = reservation(&value, owner, 36).unwrap();
    assert_eq!(row.cores, 4);
    let mut held = HeldSlots::default();
    let mut seen = BTreeMap::new();
    insert(&mut held, &mut seen, row.clone(), lease.clone()).unwrap();
    insert(&mut held, &mut seen, row.clone(), lease.clone()).unwrap();
    assert_eq!(
        held.holders,
        [row.clone()],
        "duplicate descriptors/pools do not multiply a lease"
    );
    let mut changed = lease.unwrap();
    changed.owner.birth_us += 1;
    assert!(
        insert(&mut held, &mut seen, row, Some(changed)).is_none(),
        "same text cannot merge different native owners"
    );
    assert!(
        reservation(
            &value,
            Holder {
                pid: 43,
                birth_us: 100
            },
            36
        )
        .is_none()
    );
    let old = aterm_json::json!({"pid":42,"started":1,"pool_slots":6});
    let (row, no_lease) = reservation(&old, owner, 36).unwrap();
    assert_eq!(row.cores, 6);
    assert!(no_lease.is_none());
    let historic = aterm_json::json!({"pid":42,"started":1,"pool_slots":6,"cpu_mode":"fixed-v1","lease_id":"0123456789abcdef0123456789abcdef"});
    assert_eq!(reservation(&historic, owner, 36), Some((row, None)));
    let bad_id = aterm_json::json!({"pid":42,"started":1,"pool_slots":6,"cpu_mode":"fixed-v1","lease_id":"malformed"});
    assert!(reservation(&bad_id, owner, 36).is_none());
    let no_width = aterm_json::json!({"pid":42,"started":1});
    assert_eq!(
        reservation(&no_width, owner, 36),
        Some((Held::unmeasured(Some(owner), 36), None))
    );
    let one_slot = aterm_json::json!({"pid":42,"started":1,"pool_slots":1});
    assert_eq!(
        reservation(&one_slot, owner, 36),
        Some((Held::verified(owner, 36), None))
    );
    let budget_only = aterm_json::json!({"pid":42,"started":1,"cpu_budget":3});
    assert_eq!(
        reservation(&budget_only, owner, 36),
        Some((Held::verified(owner, 3), None))
    );
    let partial = aterm_json::json!({"pid":42,"started":1,"cpu_budget":3,"cpu_mode":"fixed-v1","lease_id":"0123456789abcdef0123456789abcdef"});
    assert!(reservation(&partial, owner, 36).is_none());
    let malformed = aterm_json::json!({"pid":42,"started":1,"cpu_budget":0});
    assert!(reservation(&malformed, owner, 36).is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn real_native_holder_and_process_bound_adoption_are_counted() {
    use std::io::{Seek as _, SeekFrom, Write as _};
    let root = world();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let mut file = slot_file(&root);
    let pid = std::process::id();
    let owner = crate::build_slot_wait::held::lineage(pid)
        .into_iter()
        .find(|h| h.pid == pid)
        .unwrap();
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let data = aterm_json::json!({"pid":pid,"started":started,"cpu_budget":4,"pool_slots":6,"cpu_mode":"fixed-v1","lease_id":"0123456789abcdef0123456789abcdef"});
    file.write_all(&aterm_json::to_vec(&data).unwrap()).unwrap();
    let lock = aterm_uds::file_lock::LockFile::take_existing(
        &root.join("slot-0"),
        std::time::Instant::now(),
        "exclusive fixture slot",
    )
    .unwrap();
    assert_eq!(read(&root, 36).holders, [Held::verified(owner, 4)]);
    let duplicate = format!(
        "{{\"pid\":{pid},\"started\":{started},\"cpu_budget\":4,\"cpu_budget\":1,\"pool_slots\":6}}"
    );
    file.set_len(0).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(duplicate.as_bytes()).unwrap();
    assert_eq!(
        read(&root, 36),
        HeldSlots::unknown(36),
        "duplicate budget authority cannot reduce a held reservation"
    );
    file.set_len(0).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(&aterm_json::to_vec(&data).unwrap()).unwrap();
    drop(lock);
    assert!(read(&root, 36).holders.is_empty());
    let adopted = root.join("adopted");
    std::fs::create_dir(&adopted).unwrap();
    std::fs::set_permissions(&adopted, std::fs::Permissions::from_mode(0o700)).unwrap();
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let record = aterm_json::json!({
        "schema":1,"adopted":true,"pid":pid,"pool":root.to_str().unwrap(),"worktree":null,
        "identity":{"pid":pid,"uid":uid,"executable":"fixture","birth":format!("darwin:{}:{}", owner.birth_us / 1_000_000, owner.birth_us % 1_000_000)},
        "leases":[root.join("slot-0").to_str().unwrap()]
    });
    let mut journal = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(adopted.join("owner.json"))
        .unwrap();
    journal
        .write_all(&aterm_json::to_vec(&record).unwrap())
        .unwrap();
    assert_eq!(
        read(&root, 36).holders,
        [Held::unmeasured(Some(owner), 36)],
        "a verified adoption holds without a kernel EX lock"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn native_runtime_hex_lease_counts_one_actual_executing_unit() {
    use std::io::{Seek as _, SeekFrom, Write as _};
    let root = world();
    let runtime = root.path().join("runtime-v2");
    std::fs::create_dir(&runtime).unwrap();
    let owner = crate::build_cpu::process_cpu::read(std::process::id())
        .unwrap()
        .owner;
    let record = crate::build_cpu::Record { slot: 10, owner };
    let named = runtime.join("slot-0000000a");
    let mut file = OpenOptions::new()
        .write(true)
        .read(true)
        .create_new(true)
        .mode(0o600)
        .open(&named)
        .unwrap();
    file.write_all(record.encode().as_bytes()).unwrap();
    let lock = aterm_uds::file_lock::LockFile::take_existing(
        &named,
        std::time::Instant::now(),
        "exclusive fixture slot",
    )
    .unwrap();
    let held = read(root.path(), 36);
    assert_eq!(held.holders.len(), 1);
    assert_eq!(held.holders[0].cores, 1);
    assert_eq!(held.holders[0].process.unwrap().pid, std::process::id());
    let mut stale = record;
    stale.owner.birth = owner.birth.map(|b| b + 1);
    file.set_len(0).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(stale.encode().as_bytes()).unwrap();
    assert_eq!(
        read(root.path(), 36),
        HeldSlots::unknown(36),
        "a reused native identity cannot lower the held bound"
    );
    drop(lock);
    assert!(
        read(root.path(), 36).holders.is_empty(),
        "stale native bytes without EX hold reserve nothing"
    );
}

#[test]
fn adopted_slot_binding_refuses_conflicting_native_owner_unknown_and_replacement() {
    let owner = Holder {
        pid: 42,
        birth_us: 100,
    };
    let file = (7, 9);
    let mut adoption = slots::AdoptedSlot {
        file,
        claim: slots::AdoptionClaim::Held(owner),
    };
    assert_eq!(
        adoption_owner(Some(&adoption), file, None),
        Some(Some(owner))
    );
    assert_eq!(
        adoption_owner(Some(&adoption), file, Some(owner)),
        Some(Some(owner))
    );
    assert_eq!(
        adoption_owner(
            Some(&adoption),
            file,
            Some(Holder {
                pid: 43,
                birth_us: 101
            })
        ),
        None
    );
    assert_eq!(adoption_owner(Some(&adoption), (7, 10), Some(owner)), None);
    adoption.claim = slots::AdoptionClaim::Unknown;
    assert_eq!(adoption_owner(Some(&adoption), file, None), None);
}

#[cfg(target_os = "macos")]
#[test]
fn complete_adoption_census_includes_a_slot_beyond_the_diagnostic_limit() {
    use std::io::Write as _;
    let world = world();
    let root = std::fs::canonicalize(world.path()).unwrap();
    for index in 0..=slots::MAX_SLOTS {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(format!("slot-{index}")))
            .unwrap();
    }
    let target = root.join("slot-999");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&target)
        .unwrap();
    let owner = crate::build_slot_wait::held::lineage(std::process::id())[0];
    let adopted = root.join("adopted");
    std::fs::create_dir(&adopted).unwrap();
    std::fs::set_permissions(&adopted, std::fs::Permissions::from_mode(0o700)).unwrap();
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let record = aterm_json::json!({
        "schema":1,"adopted":true,"pid":owner.pid,"pool":root.to_str().unwrap(),"worktree":null,
        "identity":{"pid":owner.pid,"uid":uid,"executable":"fixture","birth":format!("darwin:{}:{}", owner.birth_us / 1_000_000, owner.birth_us % 1_000_000)},
        "leases":[target.to_str().unwrap()]
    });
    let path = adopted.join("owner.json");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(&aterm_json::to_vec(&record).unwrap())
        .unwrap();
    let snapshot = slots::adoptions_for_pool(&adopted, &root).unwrap();
    assert_eq!(snapshot.len(), 1);
    assert!(snapshot.contains_key(&target));
    assert_eq!(
        read(&root, 36).holders,
        [Held::unmeasured(Some(owner), 36)],
        "valid adoption past MAX_SLOTS is retained even though its physical file has no writer metadata"
    );
    let other = root.join("other-pool");
    std::fs::create_dir(&other).unwrap();
    assert!(
        slots::adoptions_for_pool(&adopted, &other)
            .unwrap()
            .is_empty(),
        "adoption does not leak into another pool"
    );
    let mut changed = record.clone();
    let identity = changed
        .as_object_mut()
        .unwrap()
        .get_mut("identity")
        .unwrap()
        .as_object_mut()
        .unwrap();
    identity.insert(
        "birth".into(),
        Value::from(format!(
            "darwin:{}:{}",
            (owner.birth_us + 1) / 1_000_000,
            (owner.birth_us + 1) % 1_000_000
        )),
    );
    std::fs::write(&path, aterm_json::to_vec(&changed).unwrap()).unwrap();
    assert!(
        read(&root, 36).holders.is_empty(),
        "a dead/reused native adoption identity reserves nothing without a lock"
    );
    let identity = changed
        .as_object_mut()
        .unwrap()
        .get_mut("identity")
        .unwrap()
        .as_object_mut()
        .unwrap();
    identity.insert("birth".into(), Value::from("unavailable-native-birth"));
    std::fs::write(&path, aterm_json::to_vec(&changed).unwrap()).unwrap();
    assert_eq!(
        read(&root, 36),
        HeldSlots::unknown(36),
        "unprovable live adoption is unknown, never silently omitted"
    );
}
