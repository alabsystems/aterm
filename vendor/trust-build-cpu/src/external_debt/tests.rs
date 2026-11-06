use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Barrier};

use super::*;

fn width(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).unwrap()
}
fn owner() -> Owner {
    Owner::current(Some(77))
}

#[test]
fn observation_absence_is_read_only_and_busy_has_no_counts() {
    let root = tempfile::tempdir().unwrap();
    let native = Pool::open_in(root.path(), width(2)).unwrap();
    let _lease = native.try_acquire(owner()).unwrap().unwrap();
    let (slots, external) = ExternalDebt::observe(&native, None).unwrap();
    assert_eq!(slots.iter().filter(|slot| matches!(slot, SlotState::Held { .. })).count(), 1);
    assert!(external.is_none());
    assert!(!native.path().join(ANCHOR).exists());
    assert!(!native.path().join(DOMAIN).exists());
    let lock = manifest(&native).unwrap();
    lock.lock().unwrap();
    assert_eq!(ExternalDebt::observe(&native, None).unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn observation_distinguishes_live_retained_and_settled_external_units() {
    let Some((_root, native, debt)) = setup() else { return };
    let _worker = debt.try_acquire_native(owner(), 4).unwrap().unwrap();
    let mut external = debt.try_reserve(owner(), width(2), 4).unwrap().unwrap();
    let anchor = native.path().join(ANCHOR);
    let before = std::fs::read(&anchor).unwrap();
    let observe = || {
        let (slots, external) = ExternalDebt::observe(&native, Some(&debt.retirement)).unwrap();
        let external = external.unwrap();
        (
            slots.iter().filter(|slot| matches!(slot, SlotState::Held { .. })).count(),
            external.iter().filter(|slot| matches!(slot, RetainedSlotState::Held { .. })).count(),
            external
                .iter()
                .filter(|slot| matches!(slot, RetainedSlotState::Retained { .. }))
                .count(),
        )
    };
    assert_eq!(observe(), (1, 2, 0));
    external.pop().unwrap().settle().unwrap();
    assert_eq!(observe(), (1, 1, 0));
    drop(external);
    assert_eq!(observe(), (1, 0, 1));
    assert_eq!(std::fs::read(&anchor).unwrap(), before);
}

#[test]
fn observation_refuses_partial_or_malformed_state_without_repair() {
    let Some((root, native, debt)) = setup() else { return };
    assert!(ExternalDebt::observe(&native, None).is_err());
    let anchor = native.path().join(ANCHOR);
    std::fs::write(&anchor, b"partial").unwrap();
    assert!(ExternalDebt::observe(&native, Some(&debt.retirement)).is_err());
    assert_eq!(std::fs::read(&anchor).unwrap(), b"partial");
    std::fs::remove_file(&anchor).unwrap();
    assert!(ExternalDebt::observe(&native, Some(&debt.retirement)).is_err());
    std::fs::rename(native.path().join(DOMAIN), root.path().join("detached-debt")).unwrap();
    std::fs::write(&anchor, b"partial").unwrap();
    assert!(ExternalDebt::observe(&native, Some(&debt.retirement)).is_err());
    assert!(!native.path().join(DOMAIN).exists());
}
struct Fixture {
    root: tempfile::TempDir,
    legacy: std::path::PathBuf,
}
impl Fixture {
    fn path(&self) -> &Path {
        self.root.path()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.legacy, std::fs::Permissions::from_mode(0o700));
    }
}
fn setup() -> Option<(Fixture, Pool, ExternalDebt)> {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o700)).unwrap();
    let retired = LegacyRetirement::retire(&legacy, 1);
    // Permission retirement is deliberately unavailable to root.
    // SAFETY: geteuid only reads process identity.
    if unsafe { libc::geteuid() } == 0 {
        assert!(retired.is_err());
        return None;
    }
    let retired = retired.unwrap();
    let native = Pool::open_in(root.path(), width(4)).unwrap();
    let debt = ExternalDebt::activate(&native, &retired).unwrap();
    Some((Fixture { root, legacy }, native, debt))
}

#[test]
fn native_and_retained_external_width_share_one_capacity() {
    let Some((_root, native, debt)) = setup() else {
        return;
    };
    let worker = debt.try_acquire_native(owner(), 4).unwrap().unwrap();
    let external = debt.try_reserve(owner(), width(3), 4).unwrap().unwrap();
    assert_eq!(debt.reserved().unwrap(), 3);
    assert!(debt.try_reserve(owner(), width(1), 4).unwrap().is_none());
    // The policy already reports native-only headroom; reserve subtracts from
    // physical width independently, never from that already-adjusted value.
    assert_eq!(debt.native_limit(2).unwrap(), 1);
    drop(external);
    assert_eq!(debt.reserved().unwrap(), 3, "closing custody cannot erase debt");
    drop(worker);
    let reopened = ExternalDebt::open(&native, &debt.retirement).unwrap().unwrap();
    assert_eq!(reopened.reserved().unwrap(), 3);
    assert_eq!(reopened.native_limit(1).unwrap(), 1);
    assert_eq!(reopened.native_limit(0).unwrap(), 0);
}

#[test]
fn settled_bundle_returns_exact_width_and_small_limit_never_partially_grants() {
    let Some((_root, _native, debt)) = setup() else {
        return;
    };
    assert!(debt.try_reserve(owner(), width(3), 2).unwrap().is_none());
    assert_eq!(debt.reserved().unwrap(), 0);
    let bundle = debt.try_reserve(owner(), width(2), 3).unwrap().unwrap();
    assert_eq!(bundle.len(), 2);
    assert!(debt.try_reserve(owner(), width(2), 3).unwrap().is_none());
    for lease in bundle {
        lease.settle().unwrap();
    }
    assert_eq!(debt.reserved().unwrap(), 0);
    assert_eq!(debt.native_limit(2).unwrap(), 2);
}

#[test]
fn actual_native_manifest_lock_prevents_external_admission() {
    let Some((_root, native, debt)) = setup() else {
        return;
    };
    let lock = manifest(&native).unwrap();
    lock.lock().unwrap();
    assert!(debt.try_reserve(owner(), width(1), 4).unwrap().is_none());
    assert_eq!(debt.reserved().unwrap(), 0);
    lock.unlock().unwrap();
    assert!(debt.try_reserve(owner(), width(1), 4).unwrap().is_some());
}

#[test]
fn competing_external_grants_do_not_overcommit() {
    let Some((_root, _native, debt)) = setup() else {
        return;
    };
    let barrier = Arc::new(Barrier::new(9));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let debt = debt.clone();
            std::thread::spawn(move || {
                barrier.wait();
                debt.try_reserve(owner(), width(2), 4).unwrap()
            })
        })
        .collect();
    barrier.wait();
    let bundles: Vec<_> = handles.into_iter().filter_map(|worker| worker.join().unwrap()).collect();
    assert!(!bundles.is_empty());
    assert!(bundles.len() <= 2);
    assert_eq!(debt.reserved().unwrap(), (bundles.len() * 2) as u32);
    for bundle in bundles {
        for lease in bundle {
            lease.settle().unwrap();
        }
    }
    assert_eq!(debt.reserved().unwrap(), 0);
}

#[test]
fn incomplete_activation_and_missing_or_replaced_bound_domain_refuse() {
    let Some((root, native, debt)) = setup() else {
        return;
    };
    let bound = native.path().join(DOMAIN);
    std::fs::rename(&bound, root.path().join("old-bound")).unwrap();
    assert!(debt.reserved().is_err());
    assert!(ExternalDebt::open(&native, &debt.retirement).is_err());
    RetainedPool::create_private(&bound, width(4)).unwrap();
    assert!(ExternalDebt::open(&native, &debt.retirement).is_err());
    std::fs::remove_file(native.path().join(ANCHOR)).unwrap();
    assert!(ExternalDebt::open(&native, &debt.retirement).is_err());

    let root = tempfile::tempdir().unwrap();
    let native = Pool::open_in(root.path(), width(4)).unwrap();
    assert!(ExternalDebt::open(&native, &debt.retirement).unwrap().is_none());
    std::fs::write(native.path().join(ANCHOR), format!("{VERSION} initializing=1\n")).unwrap();
    assert!(ExternalDebt::open(&native, &debt.retirement).is_err());
    assert!(ExternalDebt::activate(&native, &debt.retirement).is_err());
}

#[test]
fn malformed_anchor_is_never_read_as_zero_debt() {
    let Some((_root, native, debt)) = setup() else {
        return;
    };
    let anchor = native.path().join(ANCHOR);
    for bytes in [b"".as_slice(), b"partial", &[b'x'; 1024]] {
        std::fs::write(&anchor, bytes).unwrap();
        assert!(debt.reserved().is_err());
        assert!(ExternalDebt::open(&native, &debt.retirement).is_err());
    }
}

#[test]
fn concurrent_native_and_external_v3_clients_share_the_same_transaction() {
    let Some((_root, _native, debt)) = setup() else {
        return;
    };
    let barrier = Arc::new(Barrier::new(9));
    let workers: Vec<_> = (0..8)
        .map(|index| {
            let barrier = Arc::clone(&barrier);
            let debt = debt.clone();
            std::thread::spawn(move || {
                barrier.wait();
                if index % 2 == 0 {
                    (debt.try_acquire_native(owner(), 4).unwrap(), None)
                } else {
                    (None, debt.try_reserve(owner(), width(1), 4).unwrap())
                }
            })
        })
        .collect();
    barrier.wait();
    let granted: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    let native = granted.iter().filter(|(lease, _)| lease.is_some()).count();
    let external = granted.iter().filter(|(_, leases)| leases.is_some()).count();
    assert!(native + external > 0);
    assert!(native + external <= 4);
    for (_, leases) in granted {
        if let Some(leases) = leases {
            for lease in leases {
                lease.settle().unwrap();
            }
        }
    }
}

#[test]
fn prepared_grant_preserves_refused_plan_then_publishes_exact_prejournaled_nonces() {
    let Some((_root, _native, debt)) = setup() else {
        return;
    };
    let mut prepared = PreparedReservation::new(width(3)).unwrap();
    let persisted = prepared.nonces().unwrap().to_vec();
    assert!(debt.try_reserve_prepared(owner(), &mut prepared, 2).unwrap().is_none());
    assert_eq!(prepared.nonces().unwrap(), persisted);
    assert_eq!(debt.reserved().unwrap(), 0);
    let bundle = debt.try_reserve_prepared(owner(), &mut prepared, 4).unwrap().unwrap();
    let actual = bundle.iter().map(|lease| lease.identity().unwrap().nonce).collect::<Vec<_>>();
    assert_eq!(actual, persisted);
    assert!(prepared.nonces().is_err());
    assert!(debt.try_reserve_prepared(owner(), &mut prepared, 4).is_err());
    drop(bundle);
    assert_eq!(debt.reserved().unwrap(), 3, "controller loss retains every published intent nonce");
}

fn recovery_store(root: &Fixture) -> std::path::PathBuf {
    let path = root.path().join("authority");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}
#[test]
fn private_original_authority_restores_exact_debt_and_never_reissues() {
    let Some((root, _native, debt)) = setup() else {
        return;
    };
    let store = recovery_store(&root);
    let mut prepared = debt
        .prepare_recoverable(owner(), width(2), [42; 32])
        .unwrap()
        .persist(&store, "grant")
        .unwrap();
    let nonces = prepared.nonces().unwrap();
    assert_eq!(debt.reserved().unwrap(), 0, "persistence is not a grant");
    assert!(debt.try_reserve_recoverable(&mut prepared, 1).unwrap().is_none());
    assert_eq!(prepared.nonces().unwrap(), nonces);
    let bundle = debt.try_reserve_recoverable(&mut prepared, 4).unwrap().unwrap();
    let identities = bundle.iter().map(|lease| lease.identity().unwrap()).collect::<Vec<_>>();
    assert_eq!(identities.iter().map(|id| id.nonce).collect::<Vec<_>>(), nonces);
    assert!(debt.try_reserve_recoverable(&mut prepared, 4).is_err());
    drop(bundle);
    assert_eq!(debt.reserved().unwrap(), 2);
    assert!(
        debt.restore_recovery(&store, "grant").is_err(),
        "original backing lock still owns authority"
    );
    drop(prepared);
    let restored = debt.restore_recovery(&store, "grant").unwrap();
    assert_eq!(restored.context(), [42; 32]);
    assert_eq!(restored.retained_units(), 2);
    assert_eq!(restored.identities().unwrap(), identities);
    // This fixture never dispatches any target: original no-work closure is
    // known independently of secret possession and the observations above.
    restored.settle().unwrap();
    assert_eq!(debt.reserved().unwrap(), 0);
    let old = debt.restore_recovery(&store, "grant").unwrap();
    assert_eq!(old.retained_units(), 0, "reopening cannot reissue settled units");
}
#[test]
fn public_nonces_or_foreign_domain_cannot_recover_original_authority() {
    let Some((root, _native, debt)) = setup() else {
        return;
    };
    let Some((_foreign, _native2, debt2)) = setup() else {
        return;
    };
    let store = recovery_store(&root);
    let mut prepared = debt
        .prepare_recoverable(owner(), width(1), [12; 32])
        .unwrap()
        .persist(&store, "grant")
        .unwrap();
    let public = prepared.nonces().unwrap();
    let bundle = debt.try_reserve_recoverable(&mut prepared, 4).unwrap().unwrap();
    drop(bundle);
    drop(prepared);
    let fake = store.join("public-only");
    std::fs::write(&fake, format!("{public:?}")).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(debt.restore_recovery(&store, "public-only").is_err());
    assert!(debt2.restore_recovery(&store, "grant").is_err());
    assert_eq!(debt.reserved().unwrap(), 1);
    assert_eq!(debt2.reserved().unwrap(), 0);
    let recovered = debt.restore_recovery(&store, "grant").unwrap();
    assert_eq!(recovered.retained_units(), 1);
    recovered.settle().unwrap(); // no target ever dispatched in this fixture
}
#[test]
fn changed_recovery_commitment_never_becomes_settlement_authority() {
    let Some((root, _native, debt)) = setup() else {
        return;
    };
    let store = recovery_store(&root);
    let mut prepared = debt
        .prepare_recoverable(owner(), width(1), [9; 32])
        .unwrap()
        .persist(&store, "grant")
        .unwrap();
    let bundle = debt.try_reserve_recoverable(&mut prepared, 4).unwrap().unwrap();
    let identity = bundle[0].identity().unwrap();
    let path = debt
        .retained
        .path()
        .join(crate::RETAINED_DIRECTORY_NAME)
        .join(crate::unix::slot_name(identity.slot));
    let mut bytes = std::fs::read(&path).unwrap();
    let start =
        bytes.windows(b"commitment=".len()).position(|window| window == b"commitment=").unwrap()
            + b"commitment=".len();
    bytes[start] = if bytes[start] == b'0' { b'1' } else { b'0' };
    std::fs::write(&path, &bytes).unwrap();
    assert!(bundle[0].identity().is_err());
    drop(bundle);
    drop(prepared);
    assert!(debt.restore_recovery(&store, "grant").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(debt.reserved().unwrap(), 1);
}

#[test]
fn native_manifest_observation_revalidates_the_actual_bound_domain() {
    let Some((_root, native, debt)) = setup() else {
        return;
    };
    let meta = manifest(&native).unwrap().metadata().unwrap();
    assert_eq!(debt.native_manifest_identity().unwrap(), (meta.dev(), meta.ino()));
    std::fs::write(native.path().join(ANCHOR), b"unknown").unwrap();
    assert!(debt.native_manifest_identity().is_err());
}

#[test]
fn raw_native_acquisition_refuses_activated_and_partial_debt() {
    let Some((_root, native, debt)) = setup() else { return };
    assert!(native.try_acquire(owner()).is_err());
    assert!(debt.try_acquire_native(owner(), 4).unwrap().is_some());
    let anchor = native.path().join(ANCHOR);
    std::fs::write(&anchor, b"partial").unwrap();
    assert!(native.try_acquire(owner()).is_err());
    assert_eq!(std::fs::read(&anchor).unwrap(), b"partial");
    assert!(debt.try_acquire_native(owner(), 4).is_err());
}

#[cfg(feature = "jobserver")]
#[test]
fn shared_worker_returns_local_tokens_and_counts_crash_retained_debt() {
    use std::os::unix::ffi::OsStrExt;

    use crate::WorkerClient;
    let _serial = crate::tests::kernel_fixture();
    let Some((root, native, debt)) = setup() else { return };
    let implicit = Arc::new(debt.try_acquire_native(owner(), 4).unwrap().unwrap());
    let mut external = debt.try_reserve(owner(), width(3), 4).unwrap().unwrap();
    let fifo = root.path().join("local-credits");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated name is in this private fixture's directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let local = jobserver::Client::from_fifo(&fifo).unwrap();
    for _ in 0..3 {
        local.release_raw().unwrap();
    }
    let debt = Arc::new(debt);
    let foreign = tempfile::tempdir().unwrap();
    let foreign_pool = Pool::open_in(foreign.path(), width(1)).unwrap();
    let foreign_credit = Arc::new(foreign_pool.try_acquire(owner()).unwrap().unwrap());
    assert!(
        WorkerClient::new_shared(
            local.clone(),
            Arc::clone(&debt),
            foreign_credit,
            Arc::new(|_| 4),
            owner()
        )
        .is_err()
    );
    assert_eq!(local.available().unwrap(), 3);
    let client =
        WorkerClient::new_shared(local.clone(), debt, implicit, Arc::new(|_| 4), owner()).unwrap();
    assert!(client.try_acquire().unwrap().is_none());
    assert_eq!(local.available().unwrap(), 3);
    external.pop().unwrap().settle().unwrap();
    drop(external); // two units remain durable debt, not newly free capacity
    let credit = client.try_acquire().unwrap().unwrap();
    assert_eq!(local.available().unwrap(), 2);
    assert!(client.try_acquire().unwrap().is_none());
    assert_eq!(local.available().unwrap(), 2);
    drop(credit);
    assert_eq!(local.available().unwrap(), 3);
    assert_eq!(
        native
            .snapshot()
            .unwrap()
            .iter()
            .filter(|slot| matches!(slot, SlotState::Held { .. }))
            .count(),
        1
    );
}

#[test]
fn inheritance_release_race_cannot_interleave_a_retained_grant_or_expand_entry_occupancy() {
    let _serial = crate::tests::kernel_fixture();
    let Some((_root, native, debt)) = setup() else { return };
    let mut original = Some(debt.try_acquire_native(owner(), 4).unwrap().unwrap());
    let slot = original.as_ref().unwrap().slot();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(native.path().join(format!("slot-{slot:08}")))
        .unwrap();
    assert!(native.try_adopt_inherited(&file, |_| 4).is_err(), "raw activated inheritance refuses");
    let adopted = debt
        .adopt_with(
            &file,
            |_| 4,
            || {
                // Deterministically reproduce the old held-probe/release/lock race.
                drop(original.take());
                assert!(
                    debt.try_reserve(owner(), width(4), 4).unwrap().is_none(),
                    "same manifest excludes a new retained grant"
                );
                assert!(debt.try_acquire_native(owner(), 4).unwrap().is_none());
            },
        )
        .unwrap()
        .unwrap();
    drop(file);
    let held =
        native.snapshot().unwrap().iter().filter(|s| matches!(s, SlotState::Held { .. })).count();
    assert_eq!(held, 1, "replacement cannot increase the entry occupancy");
    assert_eq!(debt.reserved().unwrap(), 0);
    assert!(debt.try_reserve(owner(), width(4), 4).unwrap().is_none());
    let external = debt.try_reserve(owner(), width(3), 4).unwrap().unwrap();
    assert_eq!(debt.reserved().unwrap() + held as u32, 4);
    drop(adopted);
    for lease in external {
        lease.settle().unwrap();
    }
}
