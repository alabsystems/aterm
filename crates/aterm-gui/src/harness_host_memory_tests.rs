// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MEMORY CEILING OF A HARNESS-LED TAB (HX-15m, D18): its contract 4 harness holding
//! `relaunch` is told `restart cause=memory` through the restart record, and one that does not
//! obey is ended as any unobeyed restart is. The record half runs on the record stream's rig
//! (`control_stream_memory_tests.rs`) with the worker [`worker_over`] builds over the rig's
//! store; the ending half runs here, on a real pty.

use super::*;

/// The window host's worker for tab `sid` of `store`, its agent `agent`: its reads of the tab's
/// scoped claim and its memory restart are the ones the window wires ([`scoped_now`],
/// [`scoped_memory_restart_now`]) over that store, under the default policy; every other act
/// is inert.
pub(crate) fn worker_over(
    store: &crate::session_store::Store,
    sid: &str,
    agent: Program,
) -> WorkerIdle {
    let world = Arc::new(World::default());
    let mut h = hooks(&world, parking_body(&world));
    let (claims, memory) = (store.clone(), store.clone());
    h.acts = Acts {
        scoped: Arc::new(move |sid| scoped_now(&claims, sid)),
        scoped_memory_restart: Arc::new(move |sid| scoped_memory_restart_now(&memory, sid)),
        ..Acts::inert()
    };
    idle_host(sid, agent, switched(&on()), h)
}

/// How many `restart` rows of payload `payload` `ctx`'s timeline holds.
#[cfg(target_os = "macos")]
fn restart_rows(ctx: &crate::SessionCtx, payload: &str) -> usize {
    ctx.timeline
        .lock()
        .unwrap()
        .view_copy(None, 0)
        .rows()
        .iter()
        .filter(|row| row.kind == crate::restart_act::RESTART_EVENT && &*row.payload == payload)
        .count()
}

/// HX-15m: AN UNOBEYED MEMORY RESTART ENDS THE GROUP, THEN THE KEPT CHILD (D18: "an unobeyed one
/// is ended as HX-15e ends any"). A deaf contract 4 harness holding `relaunch` on its tab's real
/// pty keeps a child outside its own process group. The memory ceiling's restart writes one
/// `restart cause=memory` row and is watched for the policy's `stall_term_after_s` (shortened
/// here), as the `restart` verb's is. The harness ignores it: past the bound its group gets
/// `SIGTERM`, then `SIGKILL`, which ends it, and once it is gone by its pid and kernel start the
/// watchdog ends the kept child by its identity, releases the claim at once and reads the tab
/// `unsupervised`. Its negative half (the kept child runs while the restart is only asked, and
/// a restart watched still takes no other beside it) is
/// [`a_watched_memory_restart_takes_no_other_restart_beside_it`], under a bound no read of the
/// test outlasts.
#[cfg(target_os = "macos")]
#[test]
fn an_unobeyed_memory_restart_ends_the_group_then_the_kept_child() {
    use std::os::unix::process::ExitStatusExt as _;
    let agent = RealAgent::spawn(true, 7420);
    let kept = KeptSleep::start(&agent);
    let _ = universal_claim(&agent, Some(&kept), 120);
    let stamps = crate::test_scratch::outlived_dir("aterm-hh-memory-stamps-");
    let _bound = crate::restart_act::stall_bound_for_test(Duration::from_millis(400));
    let _deadline = crate::restart_act::deadline_for_test(Duration::from_secs(2));
    let restart = || {
        crate::harness_host::with_harness_state(stamps.path(), || {
            crate::restart_act::memory_restart(&agent.ctx)
        })
    };
    assert_eq!(restart(), crate::restart_act::MEMORY_TOLD);
    assert_eq!(restart_rows(&agent.ctx, "cause=memory"), 1);

    until("the deaf harness ends", || agent.ended().is_some());
    assert_eq!(
        agent.ended().and_then(|status| status.signal()),
        Some(libc::SIGKILL),
        "its group's SIGTERM ignored, SIGKILL ends it"
    );
    until("its kept child ends", || kept.ended().is_some());
    let (status, claimant_gone) = kept.ended().expect("ended");
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "the watchdog's SIGTERM to the kept child, outside the group, ends it"
    );
    assert!(
        claimant_gone,
        "the kept child is ended only once the claimant is gone"
    );
    until("the tab reads unsupervised", || {
        agent.ctx.meta.lock().unwrap().unsupervised.is_some()
    });
    assert!(
        agent.ctx.meta.lock().unwrap().supervisor.is_none(),
        "its claim was released, no grace kept"
    );
}

/// REVIEW OF HX-15m: A WATCHED MEMORY RESTART TAKES NO OTHER RESTART OF ATERM'S OWN BESIDE IT. A
/// deaf contract 4 harness holding `relaunch` on its real pty, keeping a child outside its
/// process group, is told the memory ceiling's restart, watched for the policy's
/// `stall_term_after_s` (a minute here, which no read below outlasts). The watchdog's beat
/// restart of the same claimant, run on the stream's lane, can come where the memory restart
/// lets go of the act's lock: run there, it writes nothing. A second memory restart answers
/// `wait:restart-watched` and writes nothing, and the kept child runs while the restart is only
/// asked.
/// NEGATIVE HALF: before the fix the memory restart listed its watch only after letting the
/// act's lock go, so the beat restart run there found no watch, wrote `restart cause=beat` and
/// started a second watch under its own bound.
#[cfg(target_os = "macos")]
#[test]
fn a_watched_memory_restart_takes_no_other_restart_beside_it() {
    let agent = RealAgent::spawn(true, 7421);
    let kept = KeptSleep::start(&agent);
    let (_, proven) = universal_claim(&agent, Some(&kept), 121);
    let stamps = crate::test_scratch::outlived_dir("aterm-hh-memory-watched-");
    let _bound = crate::restart_act::stall_bound_for_test(Duration::from_secs(60));
    let restart = || {
        crate::harness_host::with_harness_state(stamps.path(), || {
            crate::restart_act::memory_restart(&agent.ctx)
        })
    };
    let beat = Arc::new(Mutex::new(None));
    let (ctx, wrote) = (Arc::clone(&agent.ctx), Arc::clone(&beat));
    crate::restart_act::between_for_test(Box::new(move || {
        let told = crate::restart_act::watchdog_restart(
            &ctx,
            &proven,
            aterm_agent::harness::restart_stamp::Cause::Beat,
            Some(Duration::from_secs(60)),
            Duration::ZERO,
        );
        *wrote.lock().unwrap() = Some(told);
    }));
    assert_eq!(restart(), crate::restart_act::MEMORY_TOLD);
    assert_eq!(
        *beat.lock().unwrap(),
        Some(false),
        "a beat restart where the memory restart let the act's lock go is refused"
    );
    assert_eq!(restart_rows(&agent.ctx, "cause=memory"), 1);
    assert_eq!(
        restart_rows(&agent.ctx, "cause=beat"),
        0,
        "no second restart"
    );
    assert_eq!(
        restart(),
        crate::restart_act::MEMORY_WATCHED,
        "a restart watched still takes no second one beside it"
    );
    assert_eq!(restart_rows(&agent.ctx, "cause=memory"), 1);
    assert!(
        kept.ended().is_none(),
        "the kept child runs while the restart is asked"
    );
}
