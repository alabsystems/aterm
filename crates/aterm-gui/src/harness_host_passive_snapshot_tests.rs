// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[derive(Clone, Copy)]
enum Initial {
    Absent,
    Unnamed,
    Current,
}

struct Passive {
    sid: &'static str,
    world: Arc<Acting>,
    host: HostHandle,
    copy: self_claimed::Copy,
    reads: Arc<AtomicUsize>,
}

impl Passive {
    fn new(sid: &'static str, initial: Initial) -> Self {
        let world = Arc::new(Acting::default());
        let line = vec![
            "/opt/codex".to_string(),
            "resume".to_string(),
            "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77".to_string(),
        ];
        let mut record = standing_record();
        if let self_claimed::Seen::Present(record) = &mut record {
            record.resume_argv = Some(line.clone());
        }
        let copy = self_copy(&world, sid, record, Ok(line));
        let holder = format!("codex-alab@{}", copy.pid);
        world.open.lock().unwrap().insert(sid.to_string());
        world
            .claims
            .lock()
            .unwrap()
            .insert(sid.to_string(), holder.clone());
        world.proven.lock().unwrap().insert(sid.to_string());
        world
            .unreadable
            .store(matches!(initial, Initial::Absent), Ordering::SeqCst);
        *world.session_now.lock().unwrap() = Some(None);
        if matches!(initial, Initial::Current) {
            Self::admit(&world, sid, &copy);
        }
        world.set(&[(sid, Program::Codex)]);
        let mut hooks = world.hooks();
        let read = Arc::clone(&hooks.acts.snapshot);
        let reads = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&reads);
        hooks.acts.snapshot = Arc::new(move |sid| {
            observed.fetch_add(1, Ordering::SeqCst);
            read(sid)
        });
        let host = HostHandle::start(on(), false, false, hooks);
        until("the passive Codex worker attached", || {
            world
                .beside_ran
                .lock()
                .unwrap()
                .iter()
                .any(|(tab, beside)| tab == sid && beside.as_deref() == Some(holder.as_str()))
        });
        Self {
            sid,
            world,
            host,
            copy,
            reads,
        }
    }

    fn admit(world: &Acting, sid: &str, copy: &self_claimed::Copy) {
        *world.session_now.lock().unwrap() = Some(Some(
            copy.resume
                .as_ref()
                .expect("validated primary line")
                .session
                .clone(),
        ));
        world.unreadable.store(false, Ordering::SeqCst);
        world
            .self_copies
            .lock()
            .unwrap()
            .insert(sid.to_string(), copy.clone());
        crate::self_claim::set_copy_for_test(sid, Some(copy.clone()));
    }

    fn admit_primary(&self) {
        Self::admit(&self.world, self.sid, &self.copy);
        self_claimed(self.sid, None);
        self.world.passes();
        // A completed pass may have started the existing asynchronous point read.
        // Drain it before judging the published result, rather than imposing a latency.
        let read = self.host.shared.kept.lock().unwrap()[self.sid]
            .lock()
            .unwrap()
            .point_read
            .clone();
        if let Some(read) = read {
            assert!(
                read.wait(aterm_spec::wait::MUST_HAPPEN),
                "snapshot read ended"
            );
        }
    }

    fn untouched(&self) {
        assert!(self.world.stepped_for.lock().unwrap().is_empty());
        assert!(self.world.restarts.lock().unwrap().is_empty());
        assert!(self.world.relaunched.lock().unwrap().is_empty());
        assert!(self.world.self_relaunched.lock().unwrap().is_empty());
        assert_eq!(self.world.carried.load(Ordering::SeqCst), 0);
        assert!(self.world.idle_said.lock().unwrap().is_empty());
    }
}

impl Drop for Passive {
    fn drop(&mut self) {
        self.host.shutdown_and_join();
        publish_snapshot_for_test(self.sid, None);
        crate::self_claim::set_copy_for_test(self.sid, None);
    }
}

fn current_route_guards(sid: &'static str) {
    let current = Passive::new(sid, Initial::Current);
    assert!(crate::restart_act::offered_for(sid, current.copy.pid));
    let published = published_snapshot(sid).expect("the attached current route");
    current.host.shutdown_and_join();
    // Judge the real guard on this fixed publication, without a concurrent follower
    // replacing it as the deliberately invalid copies below are supplied.
    publish_snapshot_for_test(sid, Some(&published));
    assert!(!crate::restart_act::offered_for(sid, current.copy.pid + 1));
    assert!(!crate::restart_act::offered_for(
        "s-passive-other-tab",
        current.copy.pid
    ));
    let mut wrong = current.copy.clone();
    wrong.start.push_str(" another birth");
    crate::self_claim::set_copy_for_test(sid, Some(wrong));
    assert!(!crate::restart_act::offered_for(sid, current.copy.pid));
    let mut wrong = current.copy.clone();
    wrong.resume = Err("foreign-argv");
    crate::self_claim::set_copy_for_test(sid, Some(wrong));
    assert!(!crate::restart_act::offered_for(sid, current.copy.pid));
    crate::self_claim::set_copy_for_test(sid, Some(current.copy.clone()));
    assert!(crate::restart_act::offered_for(sid, current.copy.pid));
    current.untouched();
}

fn retired_worker_stays_unpublished(sid: &'static str) {
    let retired = Passive::new(sid, Initial::Absent);
    let mut off = on();
    off.off.push(Program::Codex);
    retired.host.set_config(off);
    until("the passive worker was revoked", || {
        retired.host.live().is_empty()
    });
    let reads = retired.reads.load(Ordering::SeqCst);
    retired.admit_primary();
    assert!(
        published_snapshot(sid).is_none(),
        "a revoked worker publishes nothing"
    );
    assert_eq!(retired.reads.load(Ordering::SeqCst), reads);
    retired.untouched();
}

fn recovers(sid: &'static str, initial: Initial) {
    let passive = Passive::new(sid, initial);
    assert!(!crate::restart_act::offered_for(sid, passive.copy.pid));
    match initial {
        Initial::Absent => assert!(published_snapshot(sid).is_none()),
        Initial::Unnamed => {
            assert!(published_snapshot(sid).is_some_and(|snap| snap.session.is_none()))
        }
        Initial::Current => unreachable!(),
    }
    passive.admit_primary();
    passive.untouched();
    let published = published_snapshot(sid);
    assert!(
        published.as_ref().is_some_and(|snap| {
            snap.pid == passive.copy.pid
                && snap.start == passive.copy.start
                && snap.tab == sid
                && relaunch::same_self_claim(snap, &passive.copy)
        }) && crate::restart_act::offered_for(sid, passive.copy.pid),
        "ordinary passive observation must publish its now-valid exact restart route: {published:?}"
    );
}

#[test]
fn an_initially_unread_passive_codex_snapshot_recovers_without_a_host_step() {
    current_route_guards("s-passive-current-missing");
    retired_worker_stays_unpublished("s-passive-retired-missing");
    recovers("s-passive-read-missing", Initial::Absent);
}

#[test]
fn an_initially_unnamed_passive_codex_snapshot_learns_its_valid_primary() {
    current_route_guards("s-passive-current-unnamed");
    retired_worker_stays_unpublished("s-passive-retired-unnamed");
    recovers("s-passive-read-unnamed", Initial::Unnamed);
}

#[test]
fn a_late_passive_read_cannot_publish_for_a_replacement_worker() {
    const SID: &str = "s-passive-late-generation";
    let world = Arc::new(Acting::default());
    let line = vec![
        "/opt/codex".to_string(),
        "resume".to_string(),
        "0199f2a4-6c1e-7a52-9d3b-2f6c8e1a4b77".to_string(),
    ];
    let mut record = standing_record();
    if let self_claimed::Seen::Present(record) = &mut record {
        record.resume_argv = Some(line.clone());
    }
    let copy = self_copy(&world, SID, record, Ok(line));
    world.open.lock().unwrap().insert(SID.to_string());
    world
        .claims
        .lock()
        .unwrap()
        .insert(SID.to_string(), format!("codex-alab@{}", copy.pid));
    world.proven.lock().unwrap().insert(SID.to_string());
    world.unreadable.store(true, Ordering::SeqCst);
    world.set(&[(SID, Program::Codex)]);
    let mut hooks = world.hooks();
    let read = Arc::clone(&hooks.acts.snapshot);
    let held = Arc::new(GatedRead::default());
    let gate = held.act("the captured snapshot is returned after this gate");
    let hold_once = AtomicBool::new(true);
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&reads);
    hooks.acts.snapshot = Arc::new(move |sid| {
        observed.fetch_add(1, Ordering::SeqCst);
        let captured = read(sid);
        if captured.is_some() && hold_once.swap(false, Ordering::SeqCst) {
            gate(sid);
        }
        captured
    });
    let host = HostHandle::start(on(), false, false, hooks);
    until("the initially unread worker is passive", || {
        !world.beside_ran.lock().unwrap().is_empty()
    });
    let passive = Passive {
        sid: SID,
        world,
        host,
        copy,
        reads,
    };
    Passive::admit(&passive.world, SID, &passive.copy);
    self_claimed(SID, None);
    until("the actual passive repair captured its snapshot", || {
        held.began.load(Ordering::SeqCst) == 1
    });
    let kept = Arc::clone(&passive.host.shared.kept.lock().unwrap()[SID]);
    let read = kept.lock().unwrap().point_read.clone().unwrap();
    passive.world.passes();
    assert_eq!(held.began.load(Ordering::SeqCst), 1);
    assert!(published_snapshot(SID).is_none());

    // Keep the replacement's independent reads unsuccessful until the old result is
    // judged. Its live primary remains identical, so only worker custody rejects it.
    passive.world.unreadable.store(true, Ordering::SeqCst);
    let mut off = on();
    off.off.push(Program::Codex);
    passive.host.set_config(off);
    until("the read's worker was revoked", || {
        passive.host.live().is_empty()
    });
    let runs = passive.world.runs.load(Ordering::SeqCst);
    passive.host.set_config(on());
    until(
        "its replacement attached to the same still-unnamed keep",
        || passive.world.runs.load(Ordering::SeqCst) > runs,
    );
    assert!(Arc::ptr_eq(
        &kept,
        &passive.host.shared.kept.lock().unwrap()[SID]
    ));
    assert!(kept.lock().unwrap().snapshot.is_none());
    assert_eq!(
        PassivePrimary::current(SID, &passive.host.hooks)
            .unwrap()
            .pid,
        passive.copy.pid
    );
    held.let_go();
    assert!(read.wait(aterm_spec::wait::MUST_HAPPEN));
    assert!(
        published_snapshot(SID).is_none(),
        "the old generation cannot land into an unchanged None snapshot"
    );
    passive.world.unreadable.store(false, Ordering::SeqCst);
    ring();
    until(
        "the replacement's own paced read publishes its route",
        || crate::restart_act::offered_for(SID, passive.copy.pid),
    );
    passive.untouched();
}
