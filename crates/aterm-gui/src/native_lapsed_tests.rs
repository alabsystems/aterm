// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's `lapsed` answer to a refused restart prepare (`historical_prepare`), and the
//! admission of a prepare's boot and custody stamps.

use super::*;

const EPOCH_CHANGED: &str = "ERR native host epoch changed\n";

/// A cold restart prepare an earlier host epoch of this boot minted, its deadline ahead.
struct Old {
    handle: crate::session_store::SessionHandle,
    facts: Facts,
    request: Prepare,
    actor: SupervisorInputCaller,
}

impl Old {
    fn new(local: u64) -> Self {
        let (handle, facts, mut request, actor) = closed_restart::fixture(local);
        cold_restart(&mut request);
        request.host_epoch = "34".repeat(16);
        assert_ne!(host_epoch(), Some(request.host_epoch.as_str()));
        Self {
            handle,
            facts,
            request,
            actor,
        }
    }

    fn send(&self) -> String {
        self.send_as(&self.request, &self.actor)
    }

    fn send_as(&self, request: &Prepare, actor: &SupervisorInputCaller) -> String {
        run(
            &self.handle.ctx,
            &self.facts,
            Request::prepare(request.clone()),
            actor.clone(),
        )
    }

    fn lapsed(&self) -> String {
        aterm_agent::harness::native_act::lapsed_reply(&self.request.id).unwrap()
    }

    fn binding(&self) -> Binding {
        Binding {
            id: self.request.id.clone(),
            capability: "cd".repeat(32),
        }
    }

    /// Record `events` as the old epoch's issuer did, for this exact prepare.
    fn record(&self, events: &[JournalEvent]) {
        let (prepare, binding) = (&self.request, self.binding());
        for event in events {
            assert!(match event {
                JournalEvent::Prepare => self.facts.record_prepared(prepare, &binding),
                JournalEvent::MayExecute => self.facts.record_may_execute(prepare, &binding),
                JournalEvent::NoExec(phase) => {
                    self.facts.record_noexec(prepare, &binding, *phase)
                }
                JournalEvent::Seal => self.facts.record_sealed(prepare, &binding),
            });
        }
    }

    fn expire(&self) {
        self.facts
            .clock
            .store(self.request.deadline_us, Ordering::SeqCst);
    }

    fn another_boot(&mut self) {
        let mut stamp = self.facts.boot().unwrap();
        stamp.boot = "e7".repeat(16);
        self.request.boot = Some(stamp);
    }

    /// Everything the host could write or hold for one answer: its operation table, the
    /// fake journal and its histories, and the effects a live admission would start.
    fn footprint(&self) -> (usize, usize, usize, usize, usize, u32, u32, u32) {
        let table = self.handle.ctx.fenced_writes.native.lock().unwrap();
        assert!(table.cancelled.is_empty() && table.receipt_flights.is_empty());
        (
            table.by_id.len() + table.reservations.len(),
            self.facts.journal.lock().unwrap().len(),
            self.facts.prepared_history.lock().unwrap().len(),
            self.facts.sealed_history.lock().unwrap().len(),
            self.facts.receipt_history.lock().unwrap().len(),
            self.facts.watch_count.load(Ordering::SeqCst),
            self.facts.signals.load(Ordering::SeqCst),
            self.facts.grant_journal_calls.load(Ordering::SeqCst),
        )
    }
}

#[test]
fn a_stamped_absence_lapses_only_past_its_deadline_and_writes_nothing() {
    let mut old = Old::new(8301);
    old.request.custody = old.facts.custody();
    let before = old.footprint();
    assert_eq!(before, (0, 0, 0, 0, 0, 0, 0, 0));
    assert_eq!(old.send(), EPOCH_CHANGED);
    old.expire();
    // Every replay gets it again, and none of them writes, reserves or starts anything.
    for _ in 0..2 {
        assert_eq!(old.send(), old.lapsed());
        assert_eq!(old.footprint(), before);
    }
    // An unstamped absence proves nothing: an issuer older than the journal granted
    // without recording. It keeps the refusal however late it is read.
    let mut unstamped = old.request.clone();
    unstamped.custody = None;
    assert_eq!(old.send_as(&unstamped, &old.actor), EPOCH_CHANGED);
    old.facts.clock.fetch_add(86_400_000_000, Ordering::SeqCst);
    assert_eq!(old.send_as(&unstamped, &old.actor), EPOCH_CHANGED);
    // Nor is it read from a journal that names no custody: no stamp matches no custody.
    let kept = old.facts.custody.lock().unwrap().take();
    assert_eq!(old.send_as(&unstamped, &old.actor), EPOCH_CHANGED);
    *old.facts.custody.lock().unwrap() = kept;
    // A replaced custody's absence is not the stamped custody's.
    let stamped = old.facts.custody.lock().unwrap().replace("d6".repeat(32));
    assert_eq!(old.send(), EPOCH_CHANGED);
    *old.facts.custody.lock().unwrap() = stamped;
    assert_eq!(old.send(), old.lapsed());
    assert_eq!(old.footprint(), before);
}

#[test]
fn an_exact_record_lapses_by_its_own_state_on_this_boot() {
    use JournalEvent::{MayExecute, NoExec, Prepare, Seal};
    let sealed_open = [Prepare, Seal];
    let cases: [(&[JournalEvent], bool, bool); 5] = [
        // (events, lapsed before the deadline, lapsed after it)
        (&[Prepare], false, true),
        (&sealed_open, true, true),
        (&[Prepare, MayExecute], false, false),
        (&[Prepare, MayExecute, Seal], false, false),
        (&[Prepare, NoExec(ImagePhase::Forward)], false, false),
    ];
    for (case, (events, early, late)) in cases.into_iter().enumerate() {
        // An exact record proves an issuer that records, so it needs no stamp.
        let old = Old::new(8310 + case as u64);
        old.record(events);
        let before = old.footprint();
        let noexec = events.contains(&NoExec(ImagePhase::Forward));
        for (expired, lapses) in [(false, early), (true, late)] {
            if expired {
                old.expire();
            }
            let expected = if noexec {
                NOEXEC.to_string()
            } else if lapses {
                old.lapsed()
            } else {
                EPOCH_CHANGED.to_string()
            };
            assert_eq!(old.send(), expected, "{events:?} expired={expired}");
            assert_eq!(old.footprint(), before, "{events:?}");
        }
    }
}

/// A NoExec the receipt read missed (appended between it and the journal read, or after that
/// read erred) is still served as one, never as `lapsed`, on this boot or another.
#[test]
fn a_noexec_record_only_the_journal_read_finds_is_still_noexec() {
    for (case, other_boot) in [false, true].into_iter().enumerate() {
        let mut old = Old::new(8380 + case as u64);
        if other_boot {
            old.another_boot();
            old.request.custody = old.facts.custody();
        }
        old.facts.journal.lock().unwrap().insert(
            old.request.id.clone(),
            JournalEntry {
                prepare: old.request.clone(),
                binding: old.binding(),
                state: JournalState::NoExec,
                predecessor: None,
                successor: None,
                closed_phase: Some(ImagePhase::Forward),
                account_phase: None,
            },
        );
        assert!(old.facts.receipt_history.lock().unwrap().is_empty());
        old.expire();
        assert_eq!(old.send(), NOEXEC, "other_boot={other_boot}");
    }
}

#[test]
fn another_boot_of_this_machine_lapses_every_record_but_noexec() {
    use JournalEvent::{MayExecute, NoExec, Prepare, Seal};
    let records: [&[JournalEvent]; 4] = [
        &[Prepare],
        &[Prepare, MayExecute],
        &[Prepare, MayExecute, Seal],
        &[Prepare, NoExec(ImagePhase::Forward)],
    ];
    for (case, events) in records.into_iter().enumerate() {
        let mut old = Old::new(8320 + case as u64);
        old.another_boot();
        old.record(events);
        // Before the deadline: every process of that boot has ended.
        let expected = if events.contains(&NoExec(ImagePhase::Forward)) {
            NOEXEC.to_string()
        } else {
            old.lapsed()
        };
        assert_eq!(old.send(), expected, "{events:?}");
        // The same possible execution stamped with this boot, or with another machine's,
        // still holds.
        if events.contains(&MayExecute) {
            for (n, host) in [None, Some(format!("darwin:{}", "a9".repeat(16)))]
                .into_iter()
                .enumerate()
            {
                let mut other = Old::new(8330 + 2 * case as u64 + n as u64);
                let mut stamp = other.facts.boot().unwrap();
                if let Some(host) = host {
                    stamp.host = host;
                    stamp.boot = "e7".repeat(16);
                }
                other.request.boot = Some(stamp);
                other.record(events);
                assert_eq!(other.send(), EPOCH_CHANGED, "{events:?}");
            }
        }
    }
    // An absence from another boot lapses only from the custody its stamp names.
    let mut absent = Old::new(8340);
    absent.another_boot();
    assert_eq!(absent.send(), EPOCH_CHANGED);
    absent.request.custody = absent.facts.custody();
    assert_eq!(absent.send(), absent.lapsed());
    *absent.facts.custody.lock().unwrap() = Some("d6".repeat(32));
    assert_eq!(absent.send(), EPOCH_CHANGED);
    assert_eq!(absent.footprint(), (0, 0, 0, 0, 0, 0, 0, 0));
}

#[test]
fn only_a_cold_or_supported_asked_restart_from_a_live_builtin_harness_can_lapse() {
    // Each variant has a sealed record with no possible execution, which lapses at any clock
    // once every gate holds; one failed gate keeps the refusal.
    let refused = |old: &Old, request: &Prepare| {
        let binding = Binding {
            id: request.id.clone(),
            capability: "cd".repeat(32),
        };
        assert!(old.facts.record_prepared(request, &binding));
        assert!(old.facts.record_sealed(request, &binding));
        old.send_as(request, &old.actor)
    };
    let old = Old::new(8350);
    old.record(&[JournalEvent::Prepare, JournalEvent::Seal]);
    assert_eq!(old.send(), old.lapsed());

    // A private restart (and the Codex renewal built on it) keeps the refusal.
    let private = Old::new(8351);
    let mut request = private.request.clone();
    let NativePlan::Restart { cold, .. } = &mut request.plan else {
        unreachable!()
    };
    *cold = None;
    assert_eq!(request.constructor(), Constructor::PrivateRestart);
    assert_eq!(refused(&private, &request), EPOCH_CHANGED);

    // A supported asked restart may lapse; an unsupported asked shape may not.
    for (local, supported) in [(8352, true), (8353, false)] {
        let asked = Old::new(local);
        let mut request = asked.request.clone();
        let NativePlan::Restart { cold, .. } = &mut request.plan else {
            unreachable!()
        };
        *cold = None;
        request.asked = Some(AskedIntent::Upgrade(
            aterm_agent::harness::native_act::AskedUpgradeIntent {
                predecessor: "de".repeat(16),
            },
        ));
        request.duty = if supported {
            Scope::Upgrade
        } else {
            Scope::Relaunch
        };
        assert_eq!(request.asked_restart_shape(), supported);
        let reply = refused(&asked, &request);
        if supported {
            assert_eq!(reply, asked.lapsed());
        } else {
            assert_eq!(reply, EPOCH_CHANGED);
        }
    }

    // A self-exec plan writes no issuer record: its stamped absence proves nothing.
    let own = Old::new(8354);
    let mut request = plan(&own.handle.ctx, &own.facts);
    request.host_epoch = own.request.host_epoch.clone();
    request.custody = own.facts.custody();
    own.facts.clock.store(request.deadline_us, Ordering::SeqCst);
    assert_eq!(own.send_as(&request, &own.actor), EPOCH_CHANGED);

    // An account-linked record reads as another record.
    let account = Old::new(8355);
    account.facts.journal.lock().unwrap().insert(
        account.request.id.clone(),
        JournalEntry {
            prepare: account.request.clone(),
            binding: account.binding(),
            state: JournalState::Sealed { may_execute: false },
            predecessor: None,
            successor: None,
            closed_phase: None,
            account_phase: Some(ImagePhase::Forward),
        },
    );
    assert_eq!(account.send(), EPOCH_CHANGED);

    // Neither an instance nor a harness whose peer has gone reads history.
    let instance = SupervisorInputCaller {
        principal: Principal::Instance,
        ..old.actor.clone()
    };
    assert_eq!(old.send_as(&old.request, &instance), EPOCH_CHANGED);
    let peer = old.facts.processes.lock().unwrap().remove(&900).unwrap();
    assert_eq!(old.send(), EPOCH_CHANGED);
    old.facts.processes.lock().unwrap().insert(900, peer);

    // An unreadable journal, and a reservation or seal of the id here, keep the refusal.
    old.facts.receipt_readable.store(false, Ordering::SeqCst);
    assert_eq!(old.send(), EPOCH_CHANGED);
    old.facts.receipt_readable.store(true, Ordering::SeqCst);
    let id = old.request.id.clone();
    let table = || old.handle.ctx.fenced_writes.native.lock().unwrap();
    table().reservations.insert(id.clone(), None);
    assert_eq!(old.send(), EPOCH_CHANGED);
    table().reservations.remove(&id);
    table().cancelled.insert(
        id.clone(),
        CancelledPrepare {
            request: old.request.clone(),
            principal: Principal::Harness,
            actor: old.facts.process(4242).unwrap(),
        },
    );
    assert_eq!(old.send(), EPOCH_CHANGED);
    table().cancelled.remove(&id);
    assert_eq!(old.send(), old.lapsed());
}

#[test]
fn admission_refuses_a_stamp_that_is_not_this_hosts_own() {
    let (handle, facts, mut request, actor) = closed_restart::fixture(8360);
    cold_restart(&mut request);
    let send = |request: &Prepare| {
        run(
            &handle.ctx,
            &facts,
            Request::prepare(request.clone()),
            actor.clone(),
        )
    };
    let untouched = || {
        let table = handle.ctx.fenced_writes.native.lock().unwrap();
        table.by_id.is_empty()
            && table.reservations.is_empty()
            && facts.prepared_history.lock().unwrap().is_empty()
            && facts.watch_count.load(Ordering::SeqCst) == 0
    };
    let own = facts.boot().unwrap();
    let mut foreign_custody = request.clone();
    foreign_custody.custody = Some("d6".repeat(32));
    let mut foreign_machine = request.clone();
    foreign_machine.boot = Some(BootStamp {
        host: format!("darwin:{}", "a9".repeat(16)),
        boot: own.boot.clone(),
    });
    let mut foreign_boot = request.clone();
    foreign_boot.boot = Some(BootStamp {
        host: own.host.clone(),
        boot: "e7".repeat(16),
    });
    for refused in [&foreign_custody, &foreign_machine, &foreign_boot] {
        assert_eq!(send(refused), DENIED);
        assert!(untouched());
    }
    // No host of this boot admits a prepare stamped with another one: with the custody it
    // names, its absence is already final.
    foreign_boot.custody = facts.custody();
    assert_eq!(
        send(&foreign_boot),
        aterm_agent::harness::native_act::lapsed_reply(&request.id).unwrap()
    );
    assert!(untouched());
    // A stamp this host cannot read is not its own.
    let mut stamped = request.clone();
    stamped.boot = Some(own);
    stamped.custody = facts.custody();
    let boot = facts.boot.lock().unwrap().take();
    assert_eq!(send(&stamped), DENIED);
    *facts.boot.lock().unwrap() = boot;
    let custody = facts.custody.lock().unwrap().take();
    assert_eq!(send(&stamped), DENIED);
    *facts.custody.lock().unwrap() = custody;
    assert!(untouched());
    // Its own stamps are admitted, and recorded with the prepare they are part of.
    admitted_binding(&send(&stamped));
    assert_eq!(
        facts.prepared_history.lock().unwrap()[0].0,
        stamped,
        "the record names the stamped prepare"
    );
}

#[test]
fn a_restart_the_sweep_sealed_unpublished_and_reclaimed_lapses_past_its_deadline() {
    let (handle, facts, mut request, actor) = closed_restart::fixture(8370);
    cold_restart(&mut request);
    let ctx = &handle.ctx;
    let send = |actor: &SupervisorInputCaller| {
        run(
            ctx,
            &facts,
            Request::prepare(request.clone()),
            actor.clone(),
        )
    };
    admitted_binding(&send(&actor));
    facts.clock.store(request.deadline_us, Ordering::SeqCst);
    // While the operation is unfinished here, another live harness peer reads no history.
    let other = process(901, 1, 901, 400, 20);
    facts.processes.lock().unwrap().insert(other.pid, other);
    let rival = SupervisorInputCaller {
        peer: Some(901),
        ..actor.clone()
    };
    assert_eq!(send(&rival), DENIED);
    // The sweep seals the expired operation without a reply, and reclaim forgets it.
    reconcile_extinct_by(ctx, &facts);
    let operation = ctx.fenced_writes.native.lock().unwrap().by_id[&request.id].clone();
    assert!(matches!(
        operation.lock().unwrap().phase,
        Phase::Completed(Decision::ProvenNotCommittedRollbackSafe)
    ));
    drop(operation);
    let retired = ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .reclaim(request.deadline_us);
    drop(retired);
    assert!(ctx.fenced_writes.native.lock().unwrap().by_id.is_empty());
    // Its journal record never left Prepared, and it is read after the deadline.
    assert!(facts.sealed_history.lock().unwrap().is_empty());
    assert_eq!(
        send(&actor),
        aterm_agent::harness::native_act::lapsed_reply(&request.id).unwrap()
    );
    assert!(facts.sealed_history.lock().unwrap().is_empty());
    assert_eq!(facts.signals.load(Ordering::SeqCst), 0);
    assert_eq!(facts.grant_journal_calls.load(Ordering::SeqCst), 0);
}
