// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The external lock and resource grant are one ledger decision. Failed/no-wait
//! attempts keep no grant or guard; retry order is distinct from actual laying.

use super::*;
use crate::{Fixed, Reading};
use std::cell::Cell;
use std::rc::Rc;

fn ledger() -> (aterm_tempfile::TempDir, Ledger, Fixed) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let ledger = Ledger::at(dir.path().join("ledger"));
    (dir, ledger, Fixed::new(Reading::of_gib(36, 30, 14)))
}

struct Guard(Rc<Cell<usize>>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn since_orders_an_ordinary_waiter_without_backdating_actual_entry() {
    let (_dir, ledger, machine) = ledger();
    let before = now_ms();
    let since = before.saturating_sub(100_000);
    let later = Waiter::enter_stand_in(
        &ledger,
        &machine,
        Need::new("later arrival").cores(14),
        Some(std::process::id() + 1),
        before - 1_000,
    )
    .unwrap();
    let fresh = Waiter::enter(&ledger, &machine, Need::new("fresh arrival").cores(2)).unwrap();
    assert!(
        matches!(fresh.poll(&machine), Polled::Waiting(_, _)),
        "a new arrival is behind the existing core-only reservation"
    );
    let waiter =
        Waiter::enter_since(&ledger, &machine, Need::new("retry").cores(2), since).unwrap();
    assert_eq!(waiter.record().start_ms, since);
    assert!(waiter.record().laid_ms() >= before);
    assert!(waiter.record().need.heavy.is_none());
    let record = ledger
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.order() == waiter.record().order())
        .unwrap();
    assert_eq!(record.start_ms, since);
    assert_eq!(record.laid_ms(), waiter.record().laid_ms());
    match waiter.poll(&machine) {
        Polled::Admitted(claim) => {
            assert_eq!(claim.asked_ms(), record.laid_ms());
            assert!(claim.waited() < Duration::from_secs(10));
        }
        other => panic!("expected admission: {other:?}"),
    }
    drop(later);
    let future = Waiter::enter_since(&ledger, &machine, Need::new("future"), u64::MAX).unwrap();
    assert!(future.record().start_ms >= before);
    assert_eq!(future.record().start_ms, future.record().laid_ms());
}

#[test]
fn guarded_poll_keeps_queue_order_and_only_transfers_a_successful_guard() {
    let (_dir, ledger, machine) = ledger();
    let drops = Rc::new(Cell::new(0));
    let since = now_ms().saturating_sub(100_000);
    let waiter =
        Waiter::enter_since(&ledger, &machine, Need::new("guarded").cores(2), since).unwrap();
    let order = waiter.record().order();
    let mut calls = 0;
    let (polled, guard) = waiter.poll_until_guarded(&machine, &|| false, &mut || {
        calls += 1;
        assert!(
            matches!(ledger.try_turn().unwrap(), Err(_)),
            "same admission turn remains held"
        );
        Ok(None::<Guard>)
    });
    assert_eq!(calls, 1);
    assert!(guard.is_none());
    let waiter = match polled {
        Polled::Waiting(w, short) => {
            assert!(short.shorts.contains(&Short::ExternalLock));
            w
        }
        other => panic!("expected external lock wait: {other:?}"),
    };
    assert_eq!(waiter.record().order(), order);
    assert!(waiter.record().held.is_none());
    let (polled, guard) =
        waiter.poll_until_guarded(&machine, &|| false, &mut || Ok(Some(Guard(drops.clone()))));
    let claim = match polled {
        Polled::Admitted(c) => c,
        other => panic!("{other:?}"),
    };
    assert!(guard.is_some());
    assert_eq!(claim.record().order(), order);
    assert_eq!(drops.get(), 0);
    drop((claim, guard));
    assert_eq!(drops.get(), 1);
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn no_room_error_and_cancellation_never_leave_a_guard_or_grant() {
    let (_dir, ledger, machine) = ledger();
    let held = Waiter::enter(
        &ledger,
        &machine,
        Need::new("all cores").cores(14).rank(crate::Rank::Release),
    )
    .unwrap()
    .poll(&machine);
    let claim = match held {
        Polled::Admitted(c) => c,
        other => panic!("{other:?}"),
    };
    let waiter = Waiter::enter(&ledger, &machine, Need::new("blocked").cores(1)).unwrap();
    let (polled, guard) =
        waiter.poll_until_guarded(&machine, &|| false, &mut || -> io::Result<Option<Guard>> {
            panic!("no callback when resources do not fit")
        });
    assert!(matches!(&polled, Polled::Waiting(_, _)));
    assert!(guard.is_none());
    drop((polled, claim));
    let waiter = Waiter::enter(&ledger, &machine, Need::new("error").cores(1)).unwrap();
    let (polled, guard) =
        waiter.poll_until_guarded(&machine, &|| false, &mut || -> io::Result<Option<Guard>> {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        });
    assert!(matches!(polled, Polled::Gone(e) if e.kind() == io::ErrorKind::PermissionDenied));
    assert!(guard.is_none());
    let stopped = Cell::new(false);
    let drops = Rc::new(Cell::new(0));
    let waiter = Waiter::enter(&ledger, &machine, Need::new("cancel after lock").cores(1)).unwrap();
    let (polled, guard) = waiter.poll_until_guarded(&machine, &|| stopped.get(), &mut || {
        stopped.set(true);
        Ok(Some(Guard(drops.clone())))
    });
    assert!(matches!(polled, Polled::Gone(e) if e.kind() == io::ErrorKind::Interrupted));
    assert!(guard.is_none());
    assert_eq!(drops.get(), 1);
    assert!(ledger.list().unwrap().is_empty());
}

struct OldPool {
    fixed: Fixed,
    held: crate::build_slot_wait::held::HeldSlots,
    lineage: Vec<aterm_sysprobe::slots::Holder>,
}
impl Machine for OldPool {
    fn read(&self, path: Option<&Path>) -> Reading {
        self.fixed.read(path)
    }
    fn procs(&self) -> Option<Procs> {
        self.fixed.procs()
    }
    fn legacy_slots(
        &self,
        _ledger: &Ledger,
        _cores: u32,
    ) -> crate::build_slot_wait::held::HeldSlots {
        self.held.clone()
    }
    fn legacy_lineage(&self, _pid: u32) -> Vec<aterm_sysprobe::slots::Holder> {
        self.lineage.clone()
    }
}

#[test]
fn old_holders_bound_all_grants_and_alone_but_verified_nested_work_can_finish() {
    use crate::build_slot_wait::held::{Held, HeldSlots};
    let (_dir, ledger, fixed) = ledger();
    let parent = aterm_sysprobe::slots::Holder {
        pid: 42,
        birth_us: 100,
    };
    let mut machine = OldPool {
        fixed,
        held: HeldSlots {
            holders: vec![Held::verified(parent, 4)],
        },
        lineage: vec![],
    };
    let need = || Need::new("new work").cores(14).rank(crate::Rank::Release);
    let claim = match Waiter::enter(&ledger, &machine, need())
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(claim.cores(), 10, "old reservation is held cores");
    drop(claim);
    let alone = Waiter::enter(&ledger, &machine, need().alone()).unwrap();
    assert!(
        matches!(alone.force(&machine).unwrap(), Forced::Turn(_, _)),
        "even --admit now cannot bypass old ALONE exclusion"
    );
    machine.lineage.push(parent);
    let claim = match Waiter::enter(&ledger, &machine, need())
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(c) => c,
        other => panic!("nested old-slot work must not wait on itself: {other:?}"),
    };
    assert_eq!(
        claim.cores(),
        4,
        "nested work retains the old grant ceiling"
    );
    drop(claim);
    machine.lineage[0].birth_us += 1;
    let claim = match Waiter::enter(&ledger, &machine, need())
        .unwrap()
        .poll(&machine)
    {
        Polled::Admitted(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(claim.cores(), 10, "PID reuse supplies no exemption");
    drop(claim);
    machine.held.holders[0] = Held::unmeasured(None, 14);
    let waiter = Waiter::enter(&ledger, &machine, need()).unwrap();
    let (polled, guard) =
        waiter.poll_until_guarded(&machine, &|| false, &mut || -> io::Result<Option<Guard>> {
            panic!("unknown held old slot cannot admit")
        });
    assert!(matches!(&polled, Polled::Waiting(_, _)));
    assert!(guard.is_none());
}

#[test]
fn grant_publication_failure_drops_the_acquired_guard() {
    let (_dir, ledger, machine) = ledger();
    let waiter = Waiter::enter(&ledger, &machine, Need::new("lost waiter").cores(1)).unwrap();
    let path = waiter.laid.path().to_path_buf();
    let drops = Rc::new(Cell::new(0));
    let (polled, guard) = waiter.poll_until_guarded(&machine, &|| false, &mut || {
        std::fs::remove_file(&path)?;
        Ok(Some(Guard(drops.clone())))
    });
    assert!(!matches!(&polled, Polled::Admitted(_)));
    assert!(guard.is_none());
    assert_eq!(drops.get(), 1);
    drop(polled);
    assert!(ledger.list().unwrap().is_empty());
}

#[test]
fn sibling_ledger_claims_share_one_old_parent_and_outsiders_count_the_union() {
    use crate::build_slot_wait::held::{Held, HeldSlots};
    use aterm_sysprobe::slots::Holder;
    struct Family {
        fixed: Fixed,
        parent: Holder,
        children: Vec<u32>,
        cores: u32,
    }
    impl Machine for Family {
        fn read(&self, path: Option<&Path>) -> Reading {
            self.fixed.read(path)
        }
        fn procs(&self) -> Option<Procs> {
            self.fixed.procs()
        }
        fn legacy_slots(&self, _ledger: &Ledger, _cores: u32) -> HeldSlots {
            HeldSlots {
                holders: vec![Held::verified(self.parent, self.cores)],
            }
        }
        fn legacy_lineage(&self, pid: u32) -> Vec<Holder> {
            if self.children.contains(&pid) {
                vec![
                    Holder {
                        pid,
                        birth_us: 200 + u64::from(pid),
                    },
                    self.parent,
                ]
            } else {
                vec![Holder {
                    pid,
                    birth_us: 200 + u64::from(pid),
                }]
            }
        }
    }
    let (_dir, ledger, fixed) = ledger();
    let child1 = std::process::id() + 1;
    let child2 = child1 + 1;
    let outsider = child2 + 1;
    let mut machine = Family {
        fixed,
        parent: Holder {
            pid: child1 + 100,
            birth_us: 100,
        },
        children: vec![child1, child2],
        cores: 14,
    };
    let ask = |what| Need::new(what).cores(14).rank(crate::Rank::Release);
    let first = Waiter::enter_stand_in(
        &ledger,
        &machine,
        ask("first old child"),
        Some(child1),
        now_ms() - 2,
    )
    .unwrap();
    let first = match first.poll(&machine) {
        Polled::Admitted(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(first.cores(), 14);
    let second = Waiter::enter_stand_in(
        &ledger,
        &machine,
        ask("second old child"),
        Some(child2),
        now_ms() - 1,
    )
    .unwrap();
    let second = match second.poll(&machine) {
        Polled::Waiting(w, _) => w,
        other => panic!("siblings cannot multiply the old budget: {other:?}"),
    };
    let outside =
        Waiter::enter_stand_in(&ledger, &machine, ask("outsider"), Some(outsider), now_ms())
            .unwrap();
    let outside = match outside.poll(&machine) {
        Polled::Waiting(w, _) => w,
        other => panic!("outside work cannot overlap the full old reservation: {other:?}"),
    };
    drop(first);
    let second = match second.poll(&machine) {
        Polled::Admitted(c) => c,
        other => panic!("next sibling must make progress: {other:?}"),
    };
    assert_eq!(second.cores(), 14);
    drop((second, outside));
    machine.cores = 4;
    let child = Waiter::enter_stand_in(
        &ledger,
        &machine,
        ask("old4 child"),
        Some(child1),
        now_ms() - 1,
    )
    .unwrap();
    let child = match child.poll(&machine) {
        Polled::Admitted(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(child.cores(), 4);
    let outside = Waiter::enter_stand_in(
        &ledger,
        &machine,
        ask("outside old4"),
        Some(outsider),
        now_ms(),
    )
    .unwrap();
    let outside = match outside.poll(&machine) {
        Polled::Admitted(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        outside.cores(),
        10,
        "the old4 reservation and its child4 are one physical reservation, not eight cores"
    );
    drop((child, outside));

    // A smaller old reservation with a ledger child must count as 4, not 8.
    let owner = Holder {
        pid: child1 + 100,
        birth_us: 100,
    };
    let old = HeldSlots {
        holders: vec![Held::verified(owner, 4)],
    };
    assert_eq!(
        old.budget_with(
            &[],
            &[(
                vec![
                    Holder {
                        pid: child1,
                        birth_us: 200
                    },
                    owner
                ],
                4
            )]
        ),
        (0, None)
    );
}

#[test]
fn private_live_ledgers_and_pure_disk_reads_do_not_census_public_slots() {
    let (_dir, ledger, _machine) = ledger();
    assert!(crate::Live.legacy_slots(&ledger, 14).holders.is_empty());
    struct DiskOnly(Fixed);
    impl Machine for DiskOnly {
        fn read(&self, path: Option<&Path>) -> Reading {
            self.0.read(path)
        }
        fn procs(&self) -> Option<Procs> {
            None
        }
        fn legacy_slots(&self, _: &Ledger, _: u32) -> crate::build_slot_wait::held::HeldSlots {
            panic!("a pure disk/quiet observation cannot reserve old CPU slots")
        }
    }
    let machine = DiskOnly(Fixed::new(Reading::of_gib(36, 30, 14)));
    assert!(matches!(
        Waiter::enter(&ledger, &machine, Need::new("quiet only").quiet(100.0))
            .unwrap()
            .poll(&machine),
        Polled::Admitted(_)
    ));
}
