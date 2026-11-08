// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pure process/watch controls. Disappearance, positive kernel death, and group quiescence
//! are independent inputs, so a census/PID-absence implementation cannot pass these cases.

use super::*;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU32};

#[derive(Debug, Default)]
struct WatchCounts {
    live: AtomicUsize,
    peak: AtomicUsize,
    opened: AtomicUsize,
}

#[derive(Debug)]
struct FakeDeath {
    state: Arc<AtomicU8>,
    counts: Arc<WatchCounts>,
    checks: Option<Arc<LockChecks>>,
}

impl DeathWitness for FakeDeath {
    fn exited(&self) -> Option<bool> {
        match self.state.load(Ordering::Acquire) {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

impl Drop for FakeDeath {
    fn drop(&mut self) {
        if let Some(checks) = &self.checks
            && (checks.predicate.try_lock().is_err()
                || checks
                    .store
                    .upgrade()
                    .is_some_and(|store| store.try_lock().is_err()))
        {
            checks.bad_disposal.store(true, Ordering::Release);
        }
        self.counts.live.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Facts {
    processes: Mutex<BTreeMap<u32, Process>>,
    death: Mutex<BTreeMap<u32, Arc<AtomicU8>>>,
    foreground: AtomicU32,
    group_gone: Mutex<Option<bool>>,
    watches: Arc<WatchCounts>,
    no_watch: AtomicBool,
    change_during_watch: AtomicBool,
    checks: Option<Arc<LockChecks>>,
}

#[derive(Debug)]
struct LockChecks {
    store: Weak<Mutex<Store>>,
    predicate: Arc<Mutex<()>>,
    bad_disposal: AtomicBool,
}

impl Facts {
    fn set_process(&self, process: Process) {
        self.death.lock().unwrap().entry(process.pid).or_default();
        self.processes.lock().unwrap().insert(process.pid, process);
    }

    fn death(&self, pid: u32, state: u8) {
        self.death.lock().unwrap()[&pid].store(state, Ordering::Release);
    }

    fn remove(&self, pid: u32) {
        self.processes.lock().unwrap().remove(&pid);
    }

    fn crash(&self, registration: &Registration) {
        self.remove(registration.vendor.pid);
        self.death(registration.vendor.pid, 1);
        self.foreground
            .store(registration.shell.group, Ordering::Release);
        *self.group_gone.lock().unwrap() = Some(true);
    }
}

impl Kernel for Facts {
    fn process(&self, pid: u32) -> Option<Process> {
        self.processes.lock().unwrap().get(&pid).cloned()
    }

    fn foreground(&self, _: i32) -> Option<u32> {
        Some(self.foreground.load(Ordering::Acquire))
    }

    fn watch(&self, process: &Process) -> Option<Box<dyn DeathWitness>> {
        if let Some(checks) = &self.checks {
            assert!(
                checks.predicate.try_lock().is_ok(),
                "watch constructor outside metadata/native predicate"
            );
            assert!(
                checks.store.upgrade().unwrap().try_lock().is_ok(),
                "watch constructor outside store lock"
            );
        }
        if self.no_watch.load(Ordering::Acquire) {
            return None;
        }
        let state = Arc::clone(self.death.lock().unwrap().get(&process.pid)?);
        let live = self.watches.live.fetch_add(1, Ordering::AcqRel) + 1;
        self.watches.peak.fetch_max(live, Ordering::AcqRel);
        self.watches.opened.fetch_add(1, Ordering::AcqRel);
        if self.change_during_watch.swap(false, Ordering::AcqRel) {
            self.processes.lock().unwrap().get_mut(&process.pid)?.birth += 1;
        }
        Some(Box::new(FakeDeath {
            state,
            counts: Arc::clone(&self.watches),
            checks: self.checks.clone(),
        }))
    }

    fn group_gone(&self, _: u32) -> Option<bool> {
        *self.group_gone.lock().unwrap()
    }
}

fn process(pid: u32, parent: u32, group: u32, session: u32) -> Process {
    Process {
        pid,
        birth: u64::from(pid) * 100,
        parent,
        group,
        session,
        image: aterm_agent::harness::native_act::Image {
            path: format!("/private/image-{pid}").into_bytes(),
            device: 7,
            inode: u64::from(pid),
        },
    }
}

fn setup() -> (Registration, Context, Facts) {
    let actor = process(111, 1, 111, 111);
    let shell = process(222, 1, 222, 222);
    let vendor = process(333, shell.pid, 333, shell.session);
    let registration = Registration {
        id: "1".repeat(32),
        host_epoch: "2".repeat(32),
        vendor,
        shell,
    };
    let context = Context {
        session: aterm_session::SessionId::new("s-abcdef"),
        nonce: aterm_session::LaunchNonce::from_bytes([3; 16]),
        host_epoch: registration.host_epoch.clone(),
        caller: SupervisorInputCaller {
            principal: crate::control::Principal::Harness,
            connection: Some(1),
            peer: Some(actor.pid),
            proof: None,
        },
        actor,
        authority: crate::session_timeline::SessionMeta::default().supervisor_authority(0),
        human_seq: 4,
        conversation: "host-verified-private-record/conversation".into(),
    };
    let facts = Facts {
        processes: Mutex::default(),
        death: Mutex::default(),
        foreground: AtomicU32::new(registration.vendor.group),
        group_gone: Mutex::new(Some(false)),
        watches: Arc::default(),
        no_watch: AtomicBool::new(false),
        change_during_watch: AtomicBool::new(false),
        checks: None,
    };
    for process in [&context.actor, &registration.shell, &registration.vendor] {
        facts.set_process(process.clone());
    }
    (registration, context, facts)
}

fn outcome_slot(store: &mut Store, context: &Context, now: u64, id: char) -> OutcomeSlot {
    let (slot, retired) = store
        .reserve_outcome_with(context, now, || {
            Some((id.to_string().repeat(32), "8".repeat(64)))
        })
        .into_parts();
    drop(retired);
    slot.expect("available future passive slot")
}

#[derive(Debug)]
struct HeldDeath {
    // The descriptor closes before its capacity hold drops.
    watch: Box<dyn DeathWitness>,
    hold: OutcomeHold,
}

impl DeathWitness for HeldDeath {
    fn exited(&self) -> Option<bool> {
        self.watch.exited()
    }
    fn outcome_hold(&self) -> Option<&OutcomeHold> {
        Some(&self.hold)
    }
}

fn outcome_watch(slot: &OutcomeSlot, facts: &Facts, vendor: &Process) -> Arc<dyn DeathWitness> {
    Arc::new(HeldDeath {
        watch: facts.watch(vendor).unwrap(),
        hold: slot.hold(),
    })
}

#[test]
fn future_outcome_slots_share_the_live_bound_without_opening_any_watch() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let future = outcome_slot(&mut store, &context, 10, 'a');
    let (live, retired) = store.reserve(&registration, &context, 10).into_parts();
    drop(retired);
    assert!(live.is_some());
    assert_eq!(store.slots.load(Ordering::Acquire), MAX_RECORDS);
    let (third, retired) = store
        .reserve_outcome_with(&context, 11, || Some(("b".repeat(32), "8".repeat(64))))
        .into_parts();
    assert!(third.is_none());
    drop(retired);
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 0);
    drop(future);
    let replacement = outcome_slot(&mut store, &context, 12, 'b');
    assert_eq!(store.slots.load(Ordering::Acquire), MAX_RECORDS);
    drop((replacement, live));
    assert_eq!(store.slots.load(Ordering::Acquire), 0);
}

#[test]
fn a_mapped_outcome_hands_the_same_watch_through_death_before_publication() {
    let (registration, context, facts, store, checks) = locked_setup();
    let slot = outcome_slot(&mut store.lock().unwrap(), &context, 10, 'a');
    let native = outcome_watch(&slot, &facts, &registration.vendor);
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 1);
    let capture = {
        let _predicate = checks.predicate.lock().unwrap();
        slot.capture_with(
            &registration.vendor,
            &registration.shell,
            &context,
            9,
            11,
            Arc::clone(&native),
            &facts,
        )
        .unwrap()
    };
    // The native completion/history can drop its owner now. The future observation owns the
    // SAME descriptor across the exact crash; no new watch is opened after the child died.
    facts.crash(&registration);
    let publication = {
        let _predicate = checks.predicate.lock().unwrap();
        store
            .lock()
            .unwrap()
            .publish_outcome_with(capture, &context, 9, 12, &facts)
    };
    let (published, retired) = publication.into_parts();
    let published = published.expect("retained capture survives later death");
    drop(retired);
    assert_eq!(published.registration.vendor, registration.vendor);
    assert_eq!(published.registration.shell, registration.shell);
    assert_eq!(published.binding.id, "a".repeat(32));
    assert_eq!(published.binding.capability, "8".repeat(64));
    let evidence = store
        .lock()
        .unwrap()
        .prove_with(&published.binding, &context, 9, 13, &facts)
        .expect("positive retained death and exact cohort quiescence");
    assert!(Arc::ptr_eq(&native, &evidence.death));
    drop(native);
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 1);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
    assert_eq!(
        evidence.expires_us.load(Ordering::Acquire),
        10 + LIFETIME_US
    );
    // A late refresh cannot revive or destroy this retained crash evidence.
    assert_eq!(
        store
            .lock()
            .unwrap()
            .refresh_with(&published.binding, &context, 9, 14, &facts),
        Reply::Unknown
    );
    assert!(evidence.reprove_with(&context, 9, 15, &facts));
    drop(store);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
    drop(evidence);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
    assert!(!checks.bad_disposal.load(Ordering::Acquire));
}

#[test]
fn outcome_capture_refusals_return_the_slot_and_existing_watch_outside_predicates() {
    for change in 0..6 {
        let (registration, context, facts, store, checks) = locked_setup();
        let slot = outcome_slot(&mut store.lock().unwrap(), &context, 10, 'a');
        let native = outcome_watch(&slot, &facts, &registration.vendor);
        let mut fresh = context.clone();
        let now = match change {
            0 => {
                fresh.conversation.push_str("/different-conversation");
                11
            }
            1 => {
                facts.death(registration.vendor.pid, 1);
                11
            }
            2 => {
                facts.death(registration.vendor.pid, 2);
                11
            }
            3 => {
                let mut reused = registration.vendor.clone();
                reused.birth += 1;
                facts.set_process(reused);
                11
            }
            4 => {
                facts
                    .foreground
                    .store(registration.shell.group, Ordering::Release);
                11
            }
            5 => 10 + LIFETIME_US,
            _ => unreachable!(),
        };
        let refusal = {
            let _predicate = checks.predicate.lock().unwrap();
            let refusal = slot
                .capture_with(
                    &registration.vendor,
                    &registration.shell,
                    &fresh,
                    9,
                    now,
                    native,
                    &facts,
                )
                .unwrap_err();
            assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
            assert_eq!(store.lock().unwrap().slots.load(Ordering::Acquire), 1);
            assert_eq!(refusal.slot.reserved.id, "a".repeat(32));
            assert!(Arc::strong_count(&refusal.death) >= 1);
            refusal
        };
        drop(refusal);
        assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
        assert_eq!(store.lock().unwrap().slots.load(Ordering::Acquire), 0);
        assert!(!checks.bad_disposal.load(Ordering::Acquire));
    }
}

#[test]
fn outcome_publication_rechecks_nonce_authority_human_epoch_conversation_and_original_ttl() {
    for change in 0..7 {
        let (registration, context, facts, store, checks) = locked_setup();
        let slot = outcome_slot(&mut store.lock().unwrap(), &context, 10, 'a');
        let native = outcome_watch(&slot, &facts, &registration.vendor);
        let capture = slot
            .capture_with(
                &registration.vendor,
                &registration.shell,
                &context,
                9,
                11,
                native,
                &facts,
            )
            .unwrap();
        let mut fresh = context.clone();
        let now = match change {
            0 => {
                fresh.human_seq += 1;
                12
            }
            1 => {
                fresh.nonce = aterm_session::LaunchNonce::from_bytes([4; 16]);
                12
            }
            2 => {
                fresh.host_epoch = "3".repeat(32);
                12
            }
            3 => {
                fresh.conversation.push_str("/new-turn");
                12
            }
            4 => {
                fresh.actor.birth += 1;
                12
            }
            5 => {
                fresh.authority = crate::session_timeline::SessionMeta {
                    supervisor_revision: context.authority.revision().wrapping_add(1),
                    builtin_relaunch_epoch: crate::session_timeline::RelaunchEpoch::for_test(1),
                    ..crate::session_timeline::SessionMeta::default()
                }
                .supervisor_authority(0);
                12
            }
            6 => 10 + LIFETIME_US,
            _ => unreachable!(),
        };
        let publication = {
            let _predicate = checks.predicate.lock().unwrap();
            let mut table = store.lock().unwrap();
            let publication = table.publish_outcome_with(capture, &fresh, 9, now, &facts);
            assert!(publication.outcome.is_none());
            assert!(table.entries.is_empty());
            assert!(table.outcomes.is_empty());
            assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
            assert_eq!(table.slots.load(Ordering::Acquire), 1);
            publication
        };
        let (_, retired) = publication.into_parts();
        assert!(!retired.is_empty());
        drop(retired);
        assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
        assert_eq!(store.lock().unwrap().slots.load(Ordering::Acquire), 0);
        assert!(!checks.bad_disposal.load(Ordering::Acquire));
        let (revived, retired) = store
            .lock()
            .unwrap()
            .reserve_outcome_with(&context, now, || Some(("a".repeat(32), "8".repeat(64))))
            .into_parts();
        assert!(revived.is_none(), "refused capture id stays retired");
        drop(retired);
    }
}

#[test]
fn an_outcome_observation_is_no_death_proof_until_its_exact_witness_and_group_agree() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let slot = outcome_slot(&mut store, &context, 10, 'a');
    let native = outcome_watch(&slot, &facts, &registration.vendor);
    let capture = slot
        .capture_with(
            &registration.vendor,
            &registration.shell,
            &context,
            9,
            11,
            native,
            &facts,
        )
        .unwrap();
    let (published, retired) = store
        .publish_outcome_with(capture, &context, 9, 12, &facts)
        .into_parts();
    drop(retired);
    let published = published.unwrap();
    facts.remove(registration.vendor.pid);
    facts
        .foreground
        .store(registration.shell.group, Ordering::Release);
    *facts.group_gone.lock().unwrap() = Some(true);
    assert!(
        store
            .prove_with(&published.binding, &context, 9, 13, &facts)
            .is_none(),
        "a missing PID is not the retained watch's positive death"
    );
    facts.death(registration.vendor.pid, 1);
    *facts.group_gone.lock().unwrap() = None;
    assert!(
        store
            .prove_with(&published.binding, &context, 9, 14, &facts)
            .is_none()
    );
    *facts.group_gone.lock().unwrap() = Some(false);
    assert!(
        store
            .prove_with(&published.binding, &context, 9, 14, &facts)
            .is_none()
    );
    *facts.group_gone.lock().unwrap() = Some(true);
    assert!(
        store
            .prove_with(&published.binding, &context, 9, 15, &facts)
            .is_some()
    );
}

#[test]
fn a_shared_native_watch_pins_capacity_after_its_passive_index_is_retired() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let slot = outcome_slot(&mut store, &context, 10, 'a');
    let native = outcome_watch(&slot, &facts, &registration.vendor);
    let capture = slot
        .capture_with(
            &registration.vendor,
            &registration.shell,
            &context,
            9,
            11,
            Arc::clone(&native),
            &facts,
        )
        .unwrap();
    let (published, retired) = store
        .publish_outcome_with(capture, &context, 9, 12, &facts)
        .into_parts();
    assert!(published.is_some());
    drop(retired);
    let second = outcome_slot(&mut store, &context, 13, 'b');
    let expired = 10 + LIFETIME_US;
    let reserve = |store: &mut Store| {
        store
            .reserve_outcome_with(&context, expired, || Some(("c".repeat(32), "8".repeat(64))))
            .into_parts()
    };
    let (third, retired) = reserve(&mut store);
    assert!(third.is_none());
    assert!(
        store.entries.is_empty(),
        "expired passive index was retired"
    );
    drop(retired);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
    assert_eq!(
        store.slots.load(Ordering::Acquire),
        MAX_RECORDS,
        "the remaining native Arc itself must pin the original slot"
    );
    let (third, retired) = reserve(&mut store);
    assert!(
        third.is_none(),
        "retiring the passive index cannot overallocate a live FD"
    );
    drop(retired);
    drop(native);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
    assert_eq!(store.slots.load(Ordering::Acquire), 1);
    let (third, retired) = reserve(&mut store);
    assert!(third.is_some());
    drop(retired);
    drop((third, second));
    assert_eq!(store.slots.load(Ordering::Acquire), 0);
}

#[test]
fn outcome_capture_binds_fresh_person_and_authority_without_changing_stable_context_or_expiry() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let slot = outcome_slot(&mut store, &context, 10, 'a');
    let native = outcome_watch(&slot, &facts, &registration.vendor);
    let mut fresh = context.clone();
    fresh.human_seq += 1;
    fresh.authority = crate::session_timeline::SessionMeta {
        supervisor_revision: context.authority.revision().wrapping_add(1),
        ..crate::session_timeline::SessionMeta::default()
    }
    .supervisor_authority(0);
    fresh.caller.connection = Some(99);
    fresh.actor.parent += 1; // Diagnostic reparenting leaves the exact actor birth/image intact.
    facts.set_process(fresh.actor.clone());
    let capture = slot
        .capture_with(
            &registration.vendor,
            &registration.shell,
            &fresh,
            9,
            11,
            native,
            &facts,
        )
        .expect("capacity reservation is no stale person/authority veto");
    facts.crash(&registration);
    let (published, retired) = store
        .publish_outcome_with(capture, &fresh, 9, 12, &facts)
        .into_parts();
    drop(retired);
    let published = published.unwrap();
    let evidence = store
        .prove_with(&published.binding, &fresh, 9, 13, &facts)
        .unwrap();
    assert_eq!(evidence.context.human_seq, fresh.human_seq);
    assert_eq!(evidence.context.authority, fresh.authority);
    assert_eq!(
        evidence.expires_us.load(Ordering::Acquire),
        10 + LIFETIME_US
    );
    assert!(
        store
            .prove_with(&published.binding, &context, 9, 14, &facts)
            .is_none(),
        "publication uses the captured fresh context, never the older reservation snapshot"
    );
}

#[test]
fn a_native_watch_cannot_be_handed_to_a_different_future_slot() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let first = outcome_slot(&mut store, &context, 10, 'a');
    let second = outcome_slot(&mut store, &context, 10, 'b');
    let native = outcome_watch(&second, &facts, &registration.vendor);
    let refusal = first
        .capture_with(
            &registration.vendor,
            &registration.shell,
            &context,
            9,
            11,
            native,
            &facts,
        )
        .unwrap_err();
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
    assert_eq!(store.slots.load(Ordering::Acquire), MAX_RECORDS);
    drop(refusal);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
    assert_eq!(store.slots.load(Ordering::Acquire), 1);
    drop(second);
    assert_eq!(store.slots.load(Ordering::Acquire), 0);
}

fn register(
    store: &mut Store,
    registration: &Registration,
    context: &Context,
    facts: &Facts,
    now: u64,
) -> ObservationBinding {
    let Reply::Registered(binding) =
        store.register_with(registration, context, 9, now, facts, || {
            Some("5".repeat(64))
        })
    else {
        panic!("fresh exact live observation must register")
    };
    binding
}

#[test]
fn lost_registration_ack_and_authenticated_reconnect_reuse_one_watch() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let binding = register(&mut store, &registration, &context, &facts, 100);
    let mut reconnected = context.clone();
    reconnected.caller.connection = Some(2);
    reconnected.actor.parent = 77;
    facts.set_process(reconnected.actor.clone());
    assert_eq!(
        store.register_with(&registration, &reconnected, 9, 200, &facts, || panic!(
            "replay must not mint"
        )),
        Reply::Registered(binding.clone())
    );
    assert_eq!(
        store.refresh_with(&binding, &reconnected, 9, 300, &facts),
        Reply::Registered(binding.clone())
    );
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 1);
    facts.crash(&registration);
    let proof = store
        .prove_with(&binding, &reconnected, 9, 400, &facts)
        .unwrap();
    assert_eq!(proof.vendor(), &registration.vendor);
    assert_eq!(proof.shell(), &registration.shell);
    assert_eq!(proof.binding(), &binding);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
}

#[test]
fn missing_pid_and_unknown_census_never_substitute_for_retained_death() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    facts.remove(registration.vendor.pid);
    assert_eq!(
        store.register_with(&registration, &context, 9, 1, &facts, || Some(
            "5".repeat(64)
        )),
        Reply::Unknown
    );
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 0);
    facts.set_process(registration.vendor.clone());
    facts.no_watch.store(true, Ordering::Release);
    assert_eq!(
        store.register_with(&registration, &context, 9, 1, &facts, || Some(
            "5".repeat(64)
        )),
        Reply::Unknown
    );
    facts.no_watch.store(false, Ordering::Release);
    let binding = register(&mut store, &registration, &context, &facts, 1);
    facts.remove(registration.vendor.pid);
    facts
        .foreground
        .store(registration.shell.group, Ordering::Release);
    *facts.group_gone.lock().unwrap() = Some(true);
    assert!(
        store.prove_with(&binding, &context, 9, 2, &facts).is_none(),
        "absence is not death"
    );
    facts.death(registration.vendor.pid, 2);
    assert!(
        store.prove_with(&binding, &context, 9, 3, &facts).is_none(),
        "unknown watch is not death"
    );
    facts.death(registration.vendor.pid, 1);
    for group in [Some(false), None] {
        *facts.group_gone.lock().unwrap() = group;
        assert!(
            store.prove_with(&binding, &context, 9, 4, &facts).is_none(),
            "death alone does not retire its named cohort"
        );
    }
    *facts.group_gone.lock().unwrap() = Some(true);
    // A PID reused outside the old cohort cannot change the identity of the retained watch.
    let mut reused = registration.vendor.clone();
    reused.birth += 1;
    reused.group += 1;
    facts.set_process(reused);
    assert!(store.prove_with(&binding, &context, 9, 5, &facts).is_some());
}

#[test]
fn disappearance_freezes_refresh_but_preserves_later_positive_crash_proof() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let binding = register(&mut store, &registration, &context, &facts, 10);
    facts.remove(registration.vendor.pid);
    assert_eq!(
        store.refresh_with(&binding, &context, 9, 20, &facts),
        Reply::Unknown
    );
    facts.set_process(registration.vendor.clone());
    assert_eq!(
        store.refresh_with(&binding, &context, 9, 30, &facts),
        Reply::Unknown,
        "a later live lookup cannot revive refresh"
    );
    facts.crash(&registration);
    assert!(
        store
            .prove_with(&binding, &context, 9, LIFETIME_US + 9, &facts)
            .is_some()
    );
    assert!(
        store
            .prove_with(&binding, &context, 9, LIFETIME_US + 10, &facts)
            .is_none()
    );
    assert_eq!(
        store.refresh_with(&binding, &context, 9, LIFETIME_US + 11, &facts),
        Reply::Unknown
    );
}

#[test]
fn live_refresh_extends_only_observation_retention_and_never_a_dead_record() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let binding = register(&mut store, &registration, &context, &facts, 10);
    assert_eq!(
        store.refresh_with(&binding, &context, 9, 20, &facts),
        Reply::Registered(binding.clone())
    );
    facts.crash(&registration);
    assert_eq!(
        store.refresh_with(&binding, &context, 9, 30, &facts),
        Reply::Unknown
    );
    let proof = store
        .prove_with(&binding, &context, 9, LIFETIME_US + 19, &facts)
        .unwrap();
    assert!(!proof.reprove_with(&context, 9, LIFETIME_US + 20, &facts));
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 1);
}

#[test]
fn passive_death_proof_survives_builtin_claim_revision_changes() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let binding = register(&mut store, &registration, &context, &facts, 1);
    let mut current = context.clone();
    current.authority = crate::session_timeline::SessionMeta {
        supervisor_revision: 3,
        ..Default::default()
    }
    .supervisor_authority(0);
    assert_ne!(context.authority, current.authority);
    assert_eq!(
        store.refresh_with(&binding, &current, 9, 2, &facts),
        Reply::Registered(binding.clone())
    );
    facts.crash(&registration);
    let proof = store.prove_with(&binding, &current, 9, 3, &facts).unwrap();
    assert!(proof.reprove_with(&current, 9, 4, &facts));
}

#[test]
fn actor_or_session_guard_change_invalidates_without_revival() {
    for change in 0..9 {
        let (registration, context, facts) = setup();
        let mut store = Store::default();
        let binding = register(&mut store, &registration, &context, &facts, 1);
        facts.crash(&registration);
        let proof = store.prove_with(&binding, &context, 9, 2, &facts).unwrap();
        let mut changed = context.clone();
        match change {
            0 => changed.human_seq += 1,
            1 => changed.conversation.push('x'),
            2 => changed.nonce = aterm_session::LaunchNonce::from_bytes([9; 16]),
            3 => changed.host_epoch = "9".repeat(32),
            4 => changed.caller.principal = crate::control::Principal::Instance,
            5 => changed.actor.birth += 1,
            6 => changed.session = aterm_session::SessionId::new("s-other"),
            7 => changed.caller.peer = Some(777),
            8 => {
                let meta = crate::session_timeline::SessionMeta {
                    supervisor_revision: context.authority.revision().wrapping_add(1),
                    builtin_relaunch_epoch: crate::session_timeline::RelaunchEpoch::for_test(1),
                    ..Default::default()
                };
                changed.authority = meta.supervisor_authority(0);
            }
            _ => unreachable!(),
        }
        assert!(!proof.reprove_with(&changed, 9, 3, &facts));
        assert!(
            !proof.reprove_with(&context, 9, 4, &facts),
            "restoring a guard cannot revive its old observation"
        );
    }
}

#[test]
fn shell_foreground_and_birth_are_rechecked_after_predecessor_drain() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    let binding = register(&mut store, &registration, &context, &facts, 1);
    facts.crash(&registration);
    let proof = store.prove_with(&binding, &context, 9, 2, &facts).unwrap();
    facts.foreground.store(999, Ordering::Release);
    assert!(!proof.reprove_with(&context, 9, 3, &facts));
    facts
        .foreground
        .store(registration.shell.group, Ordering::Release);
    let mut shell = registration.shell.clone();
    shell.birth += 1;
    facts.set_process(shell);
    assert!(!proof.reprove_with(&context, 9, 4, &facts));
}

#[test]
fn watch_registration_brackets_complete_birth_and_drops_refused_witness() {
    let (registration, context, facts) = setup();
    let mut store = Store::default();
    facts.change_during_watch.store(true, Ordering::Release);
    assert_eq!(
        store.register_with(&registration, &context, 9, 1, &facts, || Some(
            "5".repeat(64)
        )),
        Reply::Unknown
    );
    assert!(store.entries.is_empty());
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 1);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
}

#[test]
fn two_witness_bound_includes_pinned_evidence_and_bounded_eviction_history() {
    let (first, context, facts) = setup();
    let mut store = Store::default();
    let a = register(&mut store, &first, &context, &facts, 1);
    let mut second = first.clone();
    second.id = "6".repeat(32);
    let b = register(&mut store, &second, &context, &facts, 1);
    facts.crash(&first);
    let pinned_a = store.prove_with(&a, &context, 9, 2, &facts).unwrap();
    let pinned_b = store.prove_with(&b, &context, 9, 2, &facts).unwrap();
    let mut third = first.clone();
    third.id = "7".repeat(32);
    third.vendor.pid += 1;
    third.vendor.group += 1;
    third.vendor.birth += 1;
    facts.set_process(third.vendor.clone());
    facts
        .foreground
        .store(third.vendor.group, Ordering::Release);
    assert_eq!(
        store.register_with(&third, &context, 9, 3, &facts, || Some("5".repeat(64))),
        Reply::Unknown
    );
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 2);
    drop(pinned_a);
    register(&mut store, &third, &context, &facts, 4);
    assert_eq!(facts.watches.peak.load(Ordering::Acquire), MAX_RECORDS);
    assert!(store.prove_with(&a, &context, 9, 5, &facts).is_none());
    assert_eq!(
        store.register_with(&first, &context, 9, 5, &facts, || Some("5".repeat(64))),
        Reply::Unknown
    );
    drop(pinned_b);
    for index in 0..MAX_HISTORY + 4 {
        third.id = format!("{index:032x}");
        register(&mut store, &third, &context, &facts, 6);
    }
    assert_eq!(store.entries.len(), MAX_RECORDS);
    assert_eq!(store.retired.len(), MAX_HISTORY);
    assert_eq!(facts.watches.peak.load(Ordering::Acquire), MAX_RECORDS);
    drop(store);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
}

#[test]
fn an_evicted_index_cannot_hide_an_outliving_witness_from_capacity() {
    let (first, context, facts) = setup();
    let mut store = Store::default();
    register(&mut store, &first, &context, &facts, 1);
    let mut second = first.clone();
    second.id = "6".repeat(32);
    register(&mut store, &second, &context, &facts, 1);

    // State possible when a previously handed-out Weak upgrades during index retirement.
    // The slot travels with the actual witness, independently of the index/strong-count hint.
    let outliving = Arc::clone(&store.entries[0]);
    let mut retired = Retired::default();
    store.retire(0, &mut retired);
    drop(retired);
    assert_eq!(store.entries.len(), 1);
    assert_eq!(store.slots.load(Ordering::Acquire), MAX_RECORDS);
    let mut third = first.clone();
    third.id = "7".repeat(32);
    assert_eq!(
        store.register_with(&third, &context, 9, 2, &facts, || Some("5".repeat(64))),
        Reply::Unknown
    );
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), MAX_RECORDS);
    drop(outliving);
    register(&mut store, &third, &context, &facts, 3);
    assert_eq!(facts.watches.peak.load(Ordering::Acquire), MAX_RECORDS);
}

fn locked_setup() -> (
    Registration,
    Context,
    Facts,
    Arc<Mutex<Store>>,
    Arc<LockChecks>,
) {
    let (registration, context, mut facts) = setup();
    let store = Arc::new(Mutex::new(Store::default()));
    let checks = Arc::new(LockChecks {
        store: Arc::downgrade(&store),
        predicate: Arc::new(Mutex::new(())),
        bad_disposal: AtomicBool::new(false),
    });
    facts.checks = Some(Arc::clone(&checks));
    (registration, context, facts, store, checks)
}

fn locked_register(
    store: &Mutex<Store>,
    checks: &LockChecks,
    registration: &Registration,
    context: &Context,
    facts: &Facts,
    now: u64,
) -> Reply {
    for _ in 0..2 {
        let (reservation, retired) = store
            .lock()
            .unwrap()
            .reserve(registration, context, now)
            .into_parts();
        let reclaimed = !retired.is_empty();
        drop(retired); // Both mutex scopes ended before descriptor disposal.
        let Some(reservation) = reservation else {
            if reclaimed {
                continue;
            }
            return Reply::Unknown;
        };
        let Some(prepared) = reservation.arm_with(9, facts, || Some("5".repeat(64))) else {
            return Reply::Unknown;
        };
        let publication = {
            let _predicate = checks.predicate.lock().unwrap();
            let mut store = store.lock().unwrap();
            store.publish_with(prepared, context, 9, now, facts)
        };
        let (reply, retired) = publication.into_parts();
        drop(retired);
        return reply;
    }
    Reply::Unknown
}

#[test]
fn overlapping_prearms_reserve_only_two_slots_and_construct_outside_all_guards() {
    let (first, context, facts, store, checks) = locked_setup();
    let mut second = first.clone();
    second.id = "6".repeat(32);
    let mut third = first.clone();
    third.id = "7".repeat(32);
    let (one, retired) = store
        .lock()
        .unwrap()
        .reserve(&first, &context, 1)
        .into_parts();
    drop(retired);
    let (two, retired) = store
        .lock()
        .unwrap()
        .reserve(&second, &context, 1)
        .into_parts();
    drop(retired);
    for refused in [&first, &third] {
        let (reservation, retired) = store
            .lock()
            .unwrap()
            .reserve(refused, &context, 1)
            .into_parts();
        assert!(
            reservation.is_none(),
            "pending id and third proposed slot both refuse"
        );
        drop(retired);
    }
    assert_eq!(
        store.lock().unwrap().slots.load(Ordering::Acquire),
        MAX_RECORDS
    );
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 0);
    let one = one
        .unwrap()
        .arm_with(9, &facts, || Some("5".repeat(64)))
        .unwrap();
    let two = two
        .unwrap()
        .arm_with(9, &facts, || Some("5".repeat(64)))
        .unwrap();
    assert_eq!(facts.watches.live.load(Ordering::Acquire), MAX_RECORDS);
    let publication = {
        let _predicate = checks.predicate.lock().unwrap();
        store
            .lock()
            .unwrap()
            .publish_with(one, &context, 9, 2, &facts)
    };
    let (reply, retired) = publication.into_parts();
    assert!(matches!(reply, Reply::Registered(_)));
    drop(retired);
    drop(two); // Cancelling an armed proposal also returns its own capacity, outside guards.
    assert_eq!(store.lock().unwrap().slots.load(Ordering::Acquire), 1);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
    drop(store);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
    assert!(!checks.bad_disposal.load(Ordering::Acquire));
}

#[test]
fn publication_rechecks_guard_kernel_and_original_ttl_and_defers_refused_watch_disposal() {
    for change in 0..3 {
        let (registration, context, facts, store, checks) = locked_setup();
        let (reservation, retired) = store
            .lock()
            .unwrap()
            .reserve(&registration, &context, 10)
            .into_parts();
        drop(retired);
        let prepared = reservation
            .unwrap()
            .arm_with(9, &facts, || Some("5".repeat(64)))
            .unwrap();
        let mut fresh = context.clone();
        let now = match change {
            0 => {
                fresh.human_seq += 1;
                11
            }
            1 => {
                let mut changed = registration.vendor.clone();
                changed.birth += 1;
                facts.set_process(changed);
                11
            }
            2 => 10 + LIFETIME_US,
            _ => unreachable!(),
        };
        let publication = {
            let _predicate = checks.predicate.lock().unwrap();
            let mut table = store.lock().unwrap();
            let publication = table.publish_with(prepared, &fresh, 9, now, &facts);
            assert_eq!(publication.reply, Reply::Unknown);
            assert!(table.entries.is_empty());
            assert!(table.pending.is_empty());
            assert_eq!(
                facts.watches.live.load(Ordering::Acquire),
                1,
                "refusal must return custody, not close under predicates"
            );
            assert_eq!(table.slots.load(Ordering::Acquire), 1);
            publication
        };
        let (_, retired) = publication.into_parts();
        assert!(!retired.is_empty());
        drop(retired);
        assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
        assert_eq!(store.lock().unwrap().slots.load(Ordering::Acquire), 0);
        assert!(!checks.bad_disposal.load(Ordering::Acquire));
        let (retry, retired) = store
            .lock()
            .unwrap()
            .reserve(&registration, &context, now)
            .into_parts();
        assert!(
            retry.is_none(),
            "refused publication cannot revive the retired id"
        );
        drop(retired);
    }
}

#[test]
fn eviction_and_expiry_return_watch_ownership_before_a_replacement_can_arm() {
    let (first, context, facts, store, checks) = locked_setup();
    let mut second = first.clone();
    second.id = "6".repeat(32);
    let mut third = first.clone();
    third.id = "7".repeat(32);
    for registration in [&first, &second] {
        assert!(matches!(
            locked_register(&store, &checks, registration, &context, &facts, 1),
            Reply::Registered(_)
        ));
    }
    let (reservation, retired) = store
        .lock()
        .unwrap()
        .reserve(&third, &context, 2)
        .into_parts();
    assert!(
        reservation.is_none(),
        "old witness still owns its slot until retirement is disposed"
    );
    assert_eq!(facts.watches.live.load(Ordering::Acquire), MAX_RECORDS);
    drop(retired);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 1);
    assert!(matches!(
        locked_register(&store, &checks, &third, &context, &facts, 2),
        Reply::Registered(_)
    ));
    let mut after_expiry = first.clone();
    after_expiry.id = "8".repeat(32);
    assert!(matches!(
        locked_register(
            &store,
            &checks,
            &after_expiry,
            &context,
            &facts,
            LIFETIME_US + 3
        ),
        Reply::Registered(_)
    ));
    assert_eq!(facts.watches.peak.load(Ordering::Acquire), MAX_RECORDS);
    assert!(!checks.bad_disposal.load(Ordering::Acquire));
    drop(store);
    assert_eq!(facts.watches.live.load(Ordering::Acquire), 0);
}

#[test]
fn sealed_native_outcomes_keep_exact_foreign_authority_without_granting_passive_capture() {
    let (registration, mut context, facts) = setup();
    let mut meta = crate::session_timeline::SessionMeta {
        supervisor: Some(crate::session_timeline::SupervisorClaim {
            holder: "foreign-relaunch-owner".into(),
            conn: Some(77),
            expires_us: None,
            proven: None,
            scoped: None,
        }),
        supervisor_revision: 7,
        ..Default::default()
    };
    let foreign = meta.supervisor.clone();
    context.authority = meta.supervisor_authority(10);
    assert!(context.valid());
    assert!(!context.passive_valid());
    let mut store = Store::default();
    let (passive, retired) = store.reserve(&registration, &context, 10).into_parts();
    assert!(
        passive.is_none(),
        "foreign ownership cannot admit a fresh passive watch"
    );
    drop(retired);
    assert_eq!(facts.watches.opened.load(Ordering::Acquire), 0);
    let slot = outcome_slot(&mut store, &context, 10, 'a');
    let native = outcome_watch(&slot, &facts, &registration.vendor);
    let capture = slot
        .capture_with(
            &registration.vendor,
            &registration.shell,
            &context,
            9,
            11,
            native,
            &facts,
        )
        .unwrap();
    let (published, retired) = store
        .publish_outcome_with(capture, &context, 9, 12, &facts)
        .into_parts();
    drop(retired);
    let published = published.expect("the independently authorized exact outcome can be retained");
    facts.remove(registration.vendor.pid);
    facts
        .foreground
        .store(registration.shell.group, Ordering::Release);
    *facts.group_gone.lock().unwrap() = Some(true);
    facts.death(registration.vendor.pid, 1);
    assert!(
        store
            .prove_with(&published.binding, &context, 9, 13, &facts)
            .is_some()
    );
    // Even unchanged holder text cannot waive a new revision or a foreign release/ABA.
    meta.supervisor_revision += 1;
    let mut changed = context.clone();
    changed.authority = meta.supervisor_authority(14);
    assert!(!context.outcome_matches(&changed));
    assert!(
        store
            .prove_with(&published.binding, &changed, 9, 14, &facts)
            .is_none()
    );
    meta.supervisor = None;
    meta.supervisor_revision += 1;
    changed.authority = meta.supervisor_authority(15);
    assert!(!context.outcome_matches(&changed));
    assert!(
        store
            .prove_with(&published.binding, &changed, 9, 15, &facts)
            .is_none()
    );
    meta.supervisor = foreign;
    meta.supervisor_revision += 1;
    changed.authority = meta.supervisor_authority(16);
    assert!(!context.outcome_matches(&changed));
    assert!(
        store
            .prove_with(&published.binding, &changed, 9, 16, &facts)
            .is_none()
    );
}

#[test]
fn only_an_orphaned_system_keep_awake_outlives_its_group() {
    let keep_awake = Some(std::path::Path::new(KEEP_AWAKE));
    assert!(
        !keeps_group(1, keep_awake),
        "launchd adopted it: its runner died"
    );
    assert!(keeps_group(4242, keep_awake), "its runner lives");
    for other in [
        "/bin/sleep",
        "/private/tmp/caffeinate",
        "/usr/bin/caffeinate2",
    ] {
        assert!(keeps_group(1, Some(std::path::Path::new(other))), "{other}");
    }
    assert!(keeps_group(1, None), "an unread image keeps the group");
}

/// A group whose leader exited, leaving `leftover` in it adopted by launchd.
#[cfg(target_os = "macos")]
fn orphaned_in_a_group(leftover: &str) -> (u32, i32) {
    let out = aterm_uds::deadline::output_until(
        aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg(format!(
                "{leftover} </dev/null >/dev/null 2>&1 & echo $$ $!"
            ))
            .process_group(0),
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    )
    .expect("the group's leader runs and exits");
    let out = String::from_utf8(out.stdout).expect("utf-8");
    let mut pids = out
        .split_whitespace()
        .map(|n| n.parse::<i32>().expect("a pid"));
    let (leader, left) = (pids.next().expect("leader"), pids.next().expect("leftover"));
    // `$!` names the forked shell, which runs the leftover's own image only once its exec lands.
    let program = leftover.split_whitespace().next().expect("a program");
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the leftover's exec", || {
        (aterm_uds::process::read_process_path(left).as_deref()
            == Some(std::path::Path::new(program)))
        .then_some(())
    });
    assert_eq!(
        aterm_uds::process::read_short_info(left).map(|info| (info.pbsi_pgid, info.pbsi_ppid)),
        Some((leader.unsigned_abs(), 1)),
        "{leftover} is left in its group, adopted by launchd"
    );
    (leader.unsigned_abs(), left)
}

#[cfg(target_os = "macos")]
#[test]
fn a_group_left_holding_only_its_keep_awake_is_gone_and_one_left_working_is_not() {
    struct Kill(i32);
    impl Drop for Kill {
        fn drop(&mut self) {
            // SAFETY: a positive pid this test started; SIGKILL ends only that process.
            unsafe { libc::kill(self.0, libc::SIGKILL) };
        }
    }
    let (group, left) = orphaned_in_a_group("/usr/bin/caffeinate -i -t 60");
    let _keep_awake = Kill(left);
    assert_eq!(
        cohort_gone(group),
        Some(true),
        "its keep-awake runs nothing"
    );
    let (group, left) = orphaned_in_a_group("/bin/sleep 60");
    let _sleep = Kill(left);
    assert_eq!(
        cohort_gone(group),
        Some(false),
        "a sleep is work the group still runs"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_member_that_forks_and_goes_while_its_group_is_read_leaves_the_group_live() {
    struct Kill(Option<i32>);
    impl Drop for Kill {
        fn drop(&mut self) {
            if let Some(pid) = self.0 {
                // SAFETY: a positive pid this test started; SIGKILL ends only that process.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }
    let (reader, mut writer) = std::io::pipe().expect("a pipe");
    // The leader exits at once, leaving its member blocked on a line from the pipe (fd 3: an
    // asynchronous list's stdin is /dev/null); given one, the member starts a sleep in the group
    // and exits.
    let out = aterm_uds::deadline::output_until_from(
        aterm_uds::spawnfd::command("/bin/sh")
            .arg("-c")
            .arg(
                "exec 3<&0; { read _ <&3; /bin/sleep 60 </dev/null >/dev/null 2>&1 & } \
                 </dev/null >/dev/null 2>&1 & echo $$ $!",
            )
            .process_group(0),
        aterm_uds::spawnfd::Stdio::from(std::os::fd::OwnedFd::from(reader)),
        std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("the group's leader runs and exits");
    let out = String::from_utf8(out.stdout).expect("utf-8");
    let mut pids = out
        .split_whitespace()
        .map(|n| n.parse::<i32>().expect("a pid"));
    let (group, member) = (pids.next().expect("leader"), pids.next().expect("member"));
    let mut sleep = Kill(None);
    // What a probe descheduled between its listing and its reads sees: the listed member forks
    // and is reaped by launchd before it is read.
    let mut between = || {
        std::io::Write::write_all(&mut writer, b"\n").expect("the member's line");
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the member's reap", || {
            (aterm_uds::process::process_presence(member)
                == aterm_uds::process::ProcessPresence::Absent)
                .then_some(())
        });
        sleep.0 = Some(aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the member's sleep in the group",
            || {
                crate::foreground_handback::darwin_group_members(group)?
                    .into_iter()
                    .find(|&pid| {
                        aterm_uds::process::read_process_path(pid).as_deref()
                            == Some(std::path::Path::new("/bin/sleep"))
                    })
            },
        ));
    };
    assert_eq!(
        darwin_cohort_idle(group, &mut between),
        Some(false),
        "the member's sleep is still in its group"
    );
}
