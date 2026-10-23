// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-claim verification tests ([`crate::self_claim`]).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use aterm_uds::CtlStream;

#[cfg(target_os = "macos")]
use super::ServeDisposition;
#[cfg(target_os = "macos")]
use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes};
use super::{Scope, ServingFd, serving_peer_pid};
use crate::self_claim::Proven;
use crate::session_timeline::{
    ClaimRule, claim_rule, claim_supervisor_as, release_proven_if_gone, release_supervisor,
};

/// The proof of a process that runs ([`crate::self_claim::stand_in`]).
fn running() -> Proven {
    crate::self_claim::stand_in::running()
}

/// The pid of the process [`running`] proves.
fn stand_in() -> u32 {
    crate::self_claim::stand_in::pid()
}

/// A proof whose process is gone: this pid under a start it never had.
fn gone() -> Proven {
    Proven {
        start: "Thu Jan  1 00:00:00 1970".to_string(),
        ..running()
    }
}

/// The supervisor fields of a session's meta now: `(holder, proven)`.
fn shown(ctx: &crate::SessionCtx) -> (Option<String>, bool) {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let now = crate::metrics::now_us();
    (
        meta.live_supervisor(now).map(str::to_owned),
        meta.supervisor_proven(now),
    )
}

/// The `meta-change` payloads the session's timeline recorded, oldest first.
fn meta_changes(ctx: &crate::SessionCtx) -> Vec<String> {
    let reply = super::control_session::cmd_timeline(ctx, "");
    reply
        .lines()
        .filter_map(|l| {
            l.split_once(" kind=meta-change ")
                .map(|(_, p)| p.to_string())
        })
        .collect()
}

#[test]
fn the_claim_rule_lets_only_a_proof_replace_and_only_a_proof_renew_a_proof() {
    let host = "aterm-harness@77";
    let me = "codex-alab@500";
    assert_eq!(claim_rule(None, false, me, false), ClaimRule::Take);
    assert_eq!(claim_rule(None, false, me, true), ClaimRule::Take);
    assert_eq!(
        claim_rule(Some((host, false)), false, me, true),
        ClaimRule::Replace
    );
    assert_eq!(
        claim_rule(Some(("codex-alab@499", true)), true, me, true),
        ClaimRule::Replace
    );
    assert_eq!(
        claim_rule(Some((me, true)), false, me, true),
        ClaimRule::Renew
    );
    assert_eq!(
        claim_rule(Some((host, false)), false, me, false),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some(("codex-alab@499", true)), true, me, false),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some((me, true)), false, me, false),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some(("watcher@elsewhere", false)), false, me, true),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some(("codex-alab@499", true)), false, me, true),
        ClaimRule::Busy
    );
    assert_eq!(
        claim_rule(Some(("watcher", false)), false, "watcher", false),
        ClaimRule::Renew
    );
}

#[test]
fn a_proven_claim_replaces_the_hosts_and_a_dead_proofs_and_leaves_with_its_process() {
    let h = crate::session_store::test_handle_on(7501, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let host = format!("aterm-harness@{}", std::process::id());
    assert!(claim_supervisor_as(ctx, &host, None, None, None, now, aterm_slog::Actor::Ctl).is_ok());
    let me = format!("codex-alab@{}", std::process::id());
    assert_eq!(
        claim_supervisor_as(ctx, &me, Some(1), None, None, now, aterm_slog::Actor::Ctl),
        Err(host.clone())
    );
    assert_eq!(shown(ctx), (Some(host.clone()), false));
    let claimed = claim_supervisor_as(
        ctx,
        &me,
        Some(1),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof replaces the host's claim");
    assert!(claimed.changed);
    assert_eq!(claimed.replaced.as_deref(), Some(host.as_str()));
    assert_eq!(shown(ctx), (Some(me.clone()), true));
    assert_eq!(
        claim_supervisor_as(ctx, &me, Some(2), None, None, now, aterm_slog::Actor::Ctl),
        Err(me.clone())
    );
    assert_eq!(shown(ctx), (Some(me.clone()), true));
    assert_eq!(
        claim_supervisor_as(ctx, &host, None, None, None, now, aterm_slog::Actor::Ctl),
        Err(me.clone())
    );
    let pid = i32::try_from(stand_in()).expect("a pid");
    assert!(!release_proven_if_gone(ctx, pid, now));
    assert_eq!(shown(ctx), (Some(me.clone()), true));
    assert!(release_proven_if_gone(ctx, pid + 1, now));
    assert_eq!(shown(ctx), (None, false));
    assert_eq!(
        meta_changes(ctx).last().map(String::as_str),
        Some("field=supervisor value=-")
    );
    assert!(
        claim_supervisor_as(
            ctx,
            &me,
            Some(3),
            None,
            Some(gone()),
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    assert!(release_proven_if_gone(ctx, pid, now), "its process is gone");
    assert!(
        claim_supervisor_as(
            ctx,
            &me,
            Some(4),
            None,
            Some(gone()),
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    let other = "codex-alab@4242";
    let claimed = claim_supervisor_as(
        ctx,
        other,
        Some(5),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof replaces a dead proof");
    assert_eq!(claimed.replaced.as_deref(), Some(me.as_str()));
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    assert!(
        claim_supervisor_as(
            ctx,
            "watcher",
            None,
            None,
            None,
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    assert!(!release_proven_if_gone(ctx, pid + 1, now));
    assert_eq!(shown(ctx), (Some("watcher".to_string()), false));
}

/// THE FENCE (§2): a write fenced `if-supervisor=<holder>` passes only while
/// that holder holds the live claim — the window host's, until a proven
/// self-claim replaces it; then the host's is refused naming the new holder,
/// and the self-claim's passes.
#[test]
fn a_fenced_write_lands_only_under_its_holders_claim() {
    use crate::session_timeline::supervisor_fence;
    let h = crate::session_store::test_handle_on(7504, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let host = "aterm-harness@1";
    assert_eq!(
        supervisor_fence(ctx, host, now).as_deref(),
        Some("ERR busy supervisor=-\n")
    );
    assert!(claim_supervisor_as(ctx, host, None, None, None, now, aterm_slog::Actor::Ctl).is_ok());
    assert_eq!(supervisor_fence(ctx, host, now), None);
    let me = format!("codex-alab@{}", std::process::id());
    assert!(
        claim_supervisor_as(
            ctx,
            &me,
            Some(1),
            None,
            Some(running()),
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    assert_eq!(
        supervisor_fence(ctx, host, now),
        Some(format!("ERR busy supervisor={me}\n")),
        "the replaced loop's write is refused"
    );
    assert_eq!(supervisor_fence(ctx, &me, now), None);
}

/// THE FENCE HOLDS ACROSS THE WRITE (§2): a write the host's fence admitted
/// and that is still in flight keeps a proven replacement from being
/// answered until it lands, and once the replacement is answered every
/// write under the host's fence is refused.
#[test]
fn a_replacement_waits_for_the_writes_the_replaced_fence_admitted() {
    use crate::session_timeline::FencedWrite;
    let h = crate::session_store::test_handle_on(7506, -1);
    let ctx = Arc::clone(&h.ctx);
    let now = crate::metrics::now_us();
    let host = "aterm-harness@1";
    let me = format!("codex-alab@{}", std::process::id());
    assert!(claim_supervisor_as(&ctx, host, None, None, None, now, aterm_slog::Actor::Ctl).is_ok());
    let writing = FencedWrite::enter(&ctx, host, now).expect("the host holds the claim");
    let (answered_tx, answered) = std::sync::mpsc::channel();
    let claimant = {
        let ctx = Arc::clone(&ctx);
        let me = me.clone();
        std::thread::spawn(move || {
            let claimed = claim_supervisor_as(
                &ctx,
                &me,
                Some(1),
                None,
                Some(running()),
                now,
                aterm_slog::Actor::Ctl,
            );
            answered_tx.send(claimed.map(|c| c.replaced)).expect("send");
        })
    };
    let moved = Instant::now();
    while shown(&ctx).0.as_deref() != Some(me.as_str()) {
        assert!(
            moved.elapsed() < std::time::Duration::from_secs(60),
            "hang: the claim never moved"
        );
        std::thread::yield_now();
    }
    assert!(FencedWrite::enter(&ctx, host, now).is_err());
    assert!(
        answered.try_recv().is_err(),
        "answered under a write in flight"
    );
    drop(writing);
    assert_eq!(
        answered
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("hang: never answered"),
        Ok(Some(host.to_string()))
    );
    claimant.join().expect("claimant");
    assert_eq!(
        FencedWrite::enter(&ctx, host, now).err().as_deref(),
        Some(format!("ERR busy supervisor={me}\n").as_str())
    );
    assert!(release_supervisor(&ctx, None, now, aterm_slog::Actor::Ctl));
    assert!(claim_supervisor_as(&ctx, host, None, None, None, now, aterm_slog::Actor::Ctl).is_ok());
    assert!(
        claim_supervisor_as(
            &ctx,
            &me,
            Some(2),
            None,
            Some(running()),
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
}

/// `meta set harness_act <act>` (§5): only the live PROVEN self-claim's
/// holder records an act, on its claim's connection or on one the kernel
/// proves again; it is one `harness-act` row and `status last_act=`.
#[test]
fn bounded_harness_acts_prepare_and_publish_through_real_meta_dispatch() {
    use aterm_agent::harness::acts::TaskFace;
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7510, -1);
    store.write().unwrap().register(h.clone());
    crate::term_lock(&h.term).process(b"\x1b]7501;state=idle:app=codex\x1b\\");
    let holder = format!("codex-alab@{}", std::process::id());
    claim_supervisor_as(
        &h.ctx,
        &holder,
        Some(2),
        None,
        Some(running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a proven claim");
    let serve =
        |rest: &str, conn| super::meta_wakes_for_test(&store, &h.term, 7510, &h.ctx, rest, conn);
    let (reply, wakes) = serve("prepare harness_act", Some(3));
    assert_eq!(reply, "ERR denied\n");
    assert!(wakes.is_empty());
    let (reply, wakes) = serve("prepare harness_act", Some(2));
    assert!(reply.starts_with("OK until_us="), "{reply}");
    let deadline = reply
        .split_whitespace()
        .find_map(|word| word.strip_prefix("until_us="))
        .and_then(|word| word.parse::<u64>().ok())
        .expect("server-monotonic deadline");
    assert!(deadline > 0);
    assert!(reply.split_whitespace().any(|word| word == "lease_ms=5000"));
    assert!(wakes.is_empty(), "preparing records no task face");
    assert!(h.ctx.timeline.lock().unwrap().last_act().is_none());
    let mark = crate::session_timeline::LandingMark {
        program_turns: crate::term_lock(&h.term).program_state().turns,
        busy_entries: h.ctx.timeline.lock().unwrap().agent().busy_entries,
    };
    let (reply, wakes) = serve(
        &format!("set harness_act left-to-person@v1 until_us={deadline}"),
        Some(2),
    );
    assert_eq!(reply, "OK\n");
    assert!(matches!(
        wakes.as_slice(),
        [
            crate::Wake::MetaChanged { session: 7510 },
            crate::Wake::HarnessFact {
                fact: crate::HarnessFact::SelfTask(TaskFace::Theirs),
                ..
            }
        ]
    ));
    {
        let mut timeline = h.ctx.timeline.lock().unwrap();
        assert_eq!(timeline.last_act(), Some("left-to-person@v1"));
        assert_eq!(timeline.landing_mark(), Some(mark));
        assert_eq!(timeline.take_self_tasks(), [TaskFace::Theirs]);
    }
    let (reply, wakes) = serve("set harness_act continue@v1", Some(2));
    assert_eq!(reply, "OK\n");
    assert!(matches!(
        wakes.as_slice(),
        [
            crate::Wake::MetaChanged { session: 7510 },
            crate::Wake::HarnessFact {
                fact: crate::HarnessFact::SelfTask(TaskFace::Open),
                ..
            }
        ]
    ));
}

#[test]
fn bounded_harness_acts_reject_expiry_and_stale_proof_without_publication() {
    use super::control_session::cmd_meta_harness_act_with_clock;
    use aterm_agent::harness::acts::TaskFace;
    use std::cell::Cell;
    let h = crate::session_store::test_handle_on(7511, -1);
    let ctx = h.ctx.as_ref();
    let base = crate::metrics::now_us();
    let clock = Cell::new(base);
    let now = || clock.get();
    let holder = format!("codex-alab@{}", std::process::id());
    let mark = crate::session_timeline::LandingMark {
        program_turns: 7,
        busy_entries: 9,
    };
    let proof = |_: &str| Some(running());
    let no_proof = |_: &str| None;
    let serve = |rest: &str, conn, prove: &dyn Fn(&str) -> Option<Proven>| {
        cmd_meta_harness_act_with_clock(
            ctx,
            Some(super::control_session::HarnessActSnapshot {
                mark,
                left_at_rest: true,
            }),
            conn,
            aterm_slog::Actor::Ctl,
            prove,
            rest,
            &now,
        )
        .expect("a harness-act command")
    };
    let empty = || {
        let mut timeline = ctx.timeline.lock().unwrap();
        assert!(timeline.last_act().is_none());
        assert!(timeline.landing_mark().is_none());
        assert!(timeline.take_self_tasks().is_empty());
        assert!(
            !timeline
                .since(None)
                .any(|row| row.kind == crate::session_timeline::HARNESS_ACT)
        );
    };
    let bounded = format!("set harness_act left-to-person@v1 until_us={}", base + 100);
    let denied = ("ERR denied\n".to_string(), false);
    for proven in [false, true] {
        if proven {
            claim_supervisor_as(
                ctx,
                &holder,
                Some(2),
                None,
                None,
                base,
                aterm_slog::Actor::Ctl,
            )
            .expect("unproven holder");
        }
        for rest in ["prepare harness_act", bounded.as_str()] {
            assert_eq!(serve(rest, Some(2), &proof), denied);
        }
        empty();
    }
    assert!(release_supervisor(ctx, None, base, aterm_slog::Actor::Ctl));
    claim_supervisor_as(
        ctx,
        &holder,
        Some(2),
        None,
        Some(running()),
        base,
        aterm_slog::Actor::Ctl,
    )
    .expect("proven holder");
    let reused = |_: &str| Some(gone());
    let replaced = |_: &str| {
        assert!(release_supervisor(ctx, None, base, aterm_slog::Actor::Ctl));
        claim_supervisor_as(
            ctx,
            &holder,
            Some(2),
            None,
            Some(running()),
            base,
            aterm_slog::Actor::Ctl,
        )
        .expect("same identity under a new claim revision");
        Some(running())
    };
    for prove in [
        &no_proof as &dyn Fn(&str) -> Option<Proven>,
        &reused,
        &replaced,
    ] {
        for rest in ["prepare harness_act", bounded.as_str()] {
            assert_eq!(serve(rest, Some(3), prove), denied);
        }
        empty();
    }
    for rest in [
        "prepare harness_act extra",
        "set harness_act left-to-person@v1 until_us=",
        "set harness_act left-to-person@v1 until_us=-1",
        "set harness_act left-to-person@v1 until_us=+1",
        "set harness_act left-to-person@v1 until_us=1.5",
        "set harness_act left-to-person@v1 until_us=18446744073709551616",
        "set harness_act left-to-person@v1 until_us=1 extra",
        "set harness_act left-to-person@v1 until_us=1 until_us=2",
        "set harness_act left-to-person@v1 lease_ms=5000",
    ] {
        let (reply, changed) = serve(rest, Some(2), &proof);
        assert!(reply.starts_with("ERR usage:"), "{rest}: {reply}");
        assert!(!changed);
        assert!(super::control_session::harness_act_face(rest).is_none());
        empty();
    }
    for deadline in [0, base, base + 5_000_001, u64::MAX] {
        assert_eq!(
            serve(
                &format!("set harness_act left-to-person@v1 until_us={deadline}"),
                Some(2),
                &proof
            ),
            if deadline <= base {
                ("ERR expired harness_act\n".to_string(), false)
            } else {
                ("ERR usage: meta prepare harness_act | meta set harness_act <act> [until_us=<n>]\n".to_string(), false)
            }
        );
        empty();
    }
    clock.set(u64::MAX);
    assert_eq!(
        serve("prepare harness_act", Some(2), &proof),
        ("ERR clock unavailable\n".to_string(), false)
    );
    clock.set(base);
    let expired = |_: &str| {
        clock.set(base + 100);
        Some(running())
    };
    assert_eq!(
        serve(&bounded, Some(3), &expired),
        ("ERR expired harness_act\n".to_string(), false)
    );
    empty();
    clock.set(base);
    let fresh = |_: &str| {
        clock.set(base + 99);
        Some(running())
    };
    assert_eq!(serve(&bounded, Some(3), &fresh), ("OK\n".to_string(), true));
    let mut timeline = ctx.timeline.lock().unwrap();
    assert_eq!(timeline.last_act(), Some("left-to-person@v1"));
    assert_eq!(timeline.landing_mark(), Some(mark));
    assert_eq!(timeline.take_self_tasks(), [TaskFace::Theirs]);
}

#[test]
fn a_harness_act_rechecks_claim_expiry_at_the_timeline_commit() {
    use super::control_session::cmd_meta_harness_act_with_clock;
    for bounded in [false, true] {
        let h = crate::session_store::test_handle_on(7512, -1);
        let ctx = h.ctx.as_ref();
        let base = crate::metrics::now_us();
        let expires = base + 100;
        claim_supervisor_as(
            ctx,
            &format!("codex-alab@{}", std::process::id()),
            Some(2),
            Some(expires),
            Some(running()),
            base,
            aterm_slog::Actor::Ctl,
        )
        .expect("standing proven claim");
        let clock = || {
            if ctx.timeline.try_lock().is_err() {
                expires
            } else {
                base
            }
        };
        let rest = if bounded {
            format!(
                "set harness_act left-to-person@v1 until_us={}",
                base + 5_000_000
            )
        } else {
            "set harness_act left-to-person@v1".to_string()
        };
        assert_eq!(
            cmd_meta_harness_act_with_clock(
                ctx,
                None,
                Some(2),
                aterm_slog::Actor::Ctl,
                &|_| Some(running()),
                &rest,
                &clock
            ),
            Some(("ERR denied\n".to_string(), false))
        );
        let mut timeline = ctx.timeline.lock().unwrap();
        assert!(timeline.last_act().is_none());
        assert!(timeline.landing_mark().is_none());
        assert!(timeline.take_self_tasks().is_empty());
    }
}

#[test]
fn only_a_proven_claims_holder_records_an_act() {
    use super::control_session::{
        HarnessActSnapshot, cmd_meta_harness_act, cmd_meta_harness_act_with_clock,
    };
    let h = crate::session_store::test_handle_on(7505, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let no_proof = |_: &str| -> Option<Proven> { None };
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    let acts = |ctx: &crate::SessionCtx| {
        let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        let rows: Vec<String> = timeline
            .since(None)
            .filter(|e| e.kind == crate::session_timeline::HARNESS_ACT)
            .map(|e| e.payload.to_string())
            .collect();
        (rows, timeline.last_act().map(str::to_owned))
    };
    let denied = Some(("ERR denied\n".to_string(), false));
    let ok = Some(("OK\n".to_string(), true));
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(1),
            aterm_slog::Actor::Ctl,
            &proof,
            "set title x"
        ),
        None
    );
    assert_eq!(
        cmd_meta_harness_act(ctx, None, Some(1), aterm_slog::Actor::Ctl, &proof, ""),
        None
    );
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(1),
            aterm_slog::Actor::Ctl,
            &proof,
            "set harness_act continue"
        ),
        denied
    );
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(1),
            aterm_slog::Actor::Ctl,
            &proof,
            "prepare harness_act"
        ),
        denied
    );
    let me = format!("codex-alab@{}", std::process::id());
    assert!(
        claim_supervisor_as(ctx, &me, Some(1), None, None, now, aterm_slog::Actor::Ctl).is_ok()
    );
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(1),
            aterm_slog::Actor::Ctl,
            &proof,
            "set harness_act continue"
        ),
        denied
    );
    assert_eq!(acts(ctx), (Vec::new(), None));
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(1),
            aterm_slog::Actor::Ctl,
            &proof,
            "prepare harness_act"
        ),
        denied
    );
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    assert!(
        claim_supervisor_as(
            ctx,
            &me,
            Some(2),
            None,
            Some(running()),
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(2),
            aterm_slog::Actor::Ctl,
            &no_proof,
            "set harness_act continue"
        ),
        ok
    );
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(3),
            aterm_slog::Actor::Ctl,
            &no_proof,
            "set harness_act approve"
        ),
        denied
    );
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(3),
            aterm_slog::Actor::Ctl,
            &proof,
            "set harness_act approve"
        ),
        ok
    );
    assert_eq!(
        acts(ctx),
        (
            vec![
                format!("act=continue holder={me}"),
                format!("act=approve holder={me}")
            ],
            Some("approve".to_string())
        )
    );
    let before = acts(ctx);
    let reused = |_: &str| Some(gone());
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(3),
            aterm_slog::Actor::Ctl,
            &reused,
            "set harness_act continue"
        ),
        denied
    );
    let moved = |_: &str| {
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        assert!(
            claim_supervisor_as(
                ctx,
                &me,
                Some(2),
                None,
                Some(running()),
                now,
                aterm_slog::Actor::Ctl
            )
            .is_ok()
        );
        Some(running())
    };
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(3),
            aterm_slog::Actor::Ctl,
            &moved,
            "set harness_act continue"
        ),
        denied
    );
    assert_eq!(acts(ctx), before, "a stale act records nothing");
    let usage = Some((
        "ERR usage: meta prepare harness_act | meta set harness_act <act> [until_us=<n>]\n"
            .to_string(),
        false,
    ));
    for rest in [
        "set harness_act",
        "set harness_act a b",
        "set harness_act a=b?",
        "unset harness_act",
        "prepare harness_act extra",
        "set harness_act continue until_us=bad",
        "set harness_act continue until_us=-1",
        "set harness_act continue until_us=18446744073709551616",
        "set harness_act continue until_us=1 until_us=2",
    ] {
        assert_eq!(
            cmd_meta_harness_act(ctx, None, Some(2), aterm_slog::Actor::Ctl, &proof, rest),
            usage,
            "{rest}"
        );
    }
    let landing = |program_turns, busy_entries| crate::session_timeline::LandingMark {
        program_turns,
        busy_entries,
    };
    let snapshot = |mark| HarnessActSnapshot {
        mark,
        left_at_rest: true,
    };
    let mark = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .landing_mark()
    };
    for (conn, next) in [(2, landing(3, 5)), (3, landing(4, 6))] {
        assert_eq!(
            cmd_meta_harness_act(
                ctx,
                Some(snapshot(next)),
                Some(conn),
                aterm_slog::Actor::Ctl,
                &proof,
                "set harness_act landed@v1"
            ),
            ok
        );
        assert_eq!(acts(ctx).1.as_deref(), Some("landed@v1"));
        assert_eq!(mark(), Some(next), "the admitted act committed its mark");
    }
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            Some(snapshot(landing(8, 9))),
            Some(2),
            aterm_slog::Actor::Ctl,
            &proof,
            "set harness_act continue"
        ),
        ok
    );
    assert_eq!(
        mark(),
        Some(landing(4, 6)),
        "an ordinary act marks no landing"
    );
    let before = acts(ctx);
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            Some(snapshot(landing(8, 9))),
            Some(3),
            aterm_slog::Actor::Ctl,
            &no_proof,
            "set harness_act landed@v1"
        ),
        denied
    );
    assert_eq!(acts(ctx), before, "a refused landing records nothing");
    assert_eq!(
        mark(),
        Some(landing(4, 6)),
        "nor replaces the admitted mark"
    );

    let _ = ctx.timeline.lock().unwrap().take_self_tasks();
    let before = acts(ctx);
    let base = 100;
    let until = base + 5_000_000;
    assert_eq!(
        cmd_meta_harness_act_with_clock(
            ctx,
            None,
            Some(2),
            aterm_slog::Actor::Ctl,
            &no_proof,
            "prepare harness_act",
            &|| base
        ),
        Some((format!("OK until_us={until} lease_ms=5000\n"), false))
    );
    assert_eq!(acts(ctx), before);
    assert_eq!(mark(), Some(landing(4, 6)));
    assert!(ctx.timeline.lock().unwrap().take_self_tasks().is_empty());

    let request = format!("set harness_act left-to-person@v1 until_us={until}");
    let clock = std::cell::Cell::new(base);
    let guarded_read = std::cell::Cell::new(false);
    let guarded_clock = || {
        if ctx.meta.try_lock().is_err() && ctx.timeline.try_lock().is_err() {
            guarded_read.set(true);
        }
        clock.get()
    };
    for admitted in [base, until - 1] {
        clock.set(admitted);
        guarded_read.set(false);
        assert_eq!(
            cmd_meta_harness_act_with_clock(
                ctx,
                Some(snapshot(landing(7, 9))),
                Some(2),
                aterm_slog::Actor::Ctl,
                &no_proof,
                &request,
                &guarded_clock
            ),
            ok
        );
        assert!(guarded_read.get(), "deadline read under both commit guards");
        assert_eq!(mark(), Some(landing(7, 9)));
        assert_eq!(
            ctx.timeline.lock().unwrap().take_self_tasks(),
            std::collections::VecDeque::from([aterm_agent::harness::acts::TaskFace::Theirs])
        );
    }
    let before = acts(ctx);
    for expired in [until, until + 1] {
        clock.set(base);
        guarded_read.set(false);
        let delayed_proof = |_: &str| {
            clock.set(expired);
            Some(running())
        };
        assert_eq!(
            cmd_meta_harness_act_with_clock(
                ctx,
                Some(snapshot(landing(20, 30))),
                Some(3),
                aterm_slog::Actor::Ctl,
                &delayed_proof,
                &request,
                &guarded_clock
            ),
            Some(("ERR expired harness_act\n".to_string(), false))
        );
        assert!(
            guarded_read.get(),
            "expiry checked under both commit guards"
        );
        assert_eq!(acts(ctx), before, "an expired act records no row");
        assert_eq!(
            mark(),
            Some(landing(7, 9)),
            "nor replaces the captured mark"
        );
        assert!(ctx.timeline.lock().unwrap().take_self_tasks().is_empty());
    }
    assert_eq!(
        cmd_meta_harness_act_with_clock(
            ctx,
            None,
            Some(2),
            aterm_slog::Actor::Ctl,
            &no_proof,
            &format!("set harness_act continue until_us={}", until + 1),
            &|| base
        ),
        usage
    );
    assert_eq!(
        cmd_meta_harness_act_with_clock(
            ctx,
            None,
            Some(2),
            aterm_slog::Actor::Ctl,
            &no_proof,
            "prepare harness_act",
            &|| u64::MAX
        ),
        Some(("ERR clock unavailable\n".to_string(), false))
    );
    assert_eq!(
        cmd_meta_harness_act_with_clock(
            ctx,
            None,
            Some(3),
            aterm_slog::Actor::Ctl,
            &no_proof,
            "prepare harness_act",
            &|| base
        ),
        denied
    );
    assert_eq!(acts(ctx), before);
    assert!(ctx.timeline.lock().unwrap().take_self_tasks().is_empty());
}

/// A PROVEN HOLDER'S ACT IS ONE ACT MARK: in
/// the hold that records the `harness-act` row, an act the log has a word
/// for writes `mark act word=<w> rule=<act, @ written.> by=<the
/// connection's actor> src=mark conf=claim`, and its wake is taken in that
/// hold (the caller's change wake notifies).
#[test]
fn a_proven_holders_act_is_one_act_mark_by_the_connection() {
    use super::control_session::cmd_meta_harness_act;
    use aterm_slog::{Actor, Conf, Key, Kind, Node, Op, Src};
    let h = crate::session_store::test_handle_on(7506, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    let no_proof = |_: &str| -> Option<Proven> { None };
    let marks = |ctx: &crate::SessionCtx| {
        let timeline = ctx.timeline.lock().unwrap_or_else(PoisonError::into_inner);
        let rows = timeline
            .since(None)
            .filter(|e| e.kind == crate::session_timeline::HARNESS_ACT)
            .count();
        let acts: Vec<(String, String, String, Actor, Src, Conf, Node)> = timeline
            .slog()
            .log()
            .window()
            .filter(|r| r.op == Op::Mark && r.k == Kind::Act)
            .map(|r| {
                (
                    r.f.get_word(Key::Word).unwrap_or("-").to_string(),
                    r.f.get_str(Key::Rule).unwrap_or("-").to_string(),
                    r.f.get_str(Key::HarnessAct).unwrap_or("-").to_string(),
                    r.by,
                    r.src,
                    r.conf,
                    r.node,
                )
            })
            .collect();
        (rows, acts)
    };
    let me = format!("codex-alab@{}", std::process::id());
    assert!(claim_supervisor_as(ctx, &me, Some(2), None, Some(running()), now, Actor::Ctl).is_ok());
    let ok = Some(("OK\n".to_string(), true));
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(2),
            Actor::Ctl,
            &proof,
            "set harness_act approve-all@v1"
        ),
        ok
    );
    assert!(
        !ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .slog_take_wake(),
        "the act's wake was taken in its own hold"
    );
    let step = super::tests::with_open_approval(&h);
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(2),
            Actor::Harness,
            &proof,
            "set harness_act upgrade@v1"
        ),
        ok
    );
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(2),
            Actor::Ctl,
            &proof,
            "set harness_act finished@v1"
        ),
        ok
    );
    let claim = (Src::Mark, Conf::Claim);
    let want = vec![
        (
            "approve".to_string(),
            "approve-all.v1".to_string(),
            "approve-all@v1".to_string(),
            Actor::Ctl,
            claim.0,
            claim.1,
            Node::SESSION,
        ),
        (
            "relaunch".to_string(),
            "upgrade.v1".to_string(),
            "upgrade@v1".to_string(),
            Actor::Harness,
            claim.0,
            claim.1,
            step,
        ),
        (
            "unknown".to_string(),
            "-".to_string(),
            "finished@v1".to_string(),
            Actor::Ctl,
            claim.0,
            claim.1,
            step,
        ),
    ];
    assert_eq!(marks(ctx), (3, want.clone()));
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            None,
            Some(3),
            Actor::Ctl,
            &no_proof,
            "set harness_act continue@v1"
        ),
        Some(("ERR denied\n".to_string(), false))
    );
    assert_eq!(marks(ctx), (3, want));
}

#[test]
fn a_proven_claim_is_never_carried_across_an_update() {
    let h = crate::session_store::test_handle_on(7502, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let me = format!("codex-alab@{}", std::process::id());
    let ttl = Some(now + 60_000_000);
    assert!(
        claim_supervisor_as(
            ctx,
            &me,
            None,
            ttl,
            Some(running()),
            now,
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    let meta = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        meta.handoff_supervisor(now, "aterm-harness@1"),
        (None, false)
    );
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    assert!(claim_supervisor_as(ctx, &me, None, ttl, None, now, aterm_slog::Actor::Ctl).is_ok());
    let meta = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert!(meta.handoff_supervisor(now, "aterm-harness@1").0.is_some());
}

/// The peer pid is read only on a direct connection: the lanes mark theirs,
/// the inherited bridge's `socketpair` is never one.
#[test]
fn the_peer_pid_is_read_only_on_a_direct_connection() {
    let (_client, server) = CtlStream::pair().expect("socket pair");
    let serving = ServingFd::enter(&server);
    assert_eq!(
        serving_peer_pid(),
        None,
        "an undirected connection proves nothing"
    );
    let direct = super::DirectPeer::enter();
    assert_eq!(serving_peer_pid(), Some(std::process::id()));
    let nested = std::panic::catch_unwind(|| {
        let (_nested_client, nested_server) = CtlStream::pair().expect("nested pair");
        let _serving = ServingFd::enter(&nested_server);
        let _direct = super::DirectPeer::enter();
        assert_eq!(serving_peer_pid(), Some(std::process::id()));
        panic!("the nested request unwound");
    });
    assert!(nested.is_err());
    assert_eq!(serving_peer_pid(), Some(std::process::id()));
    drop(direct);
    drop(serving);
    assert_eq!(serving_peer_pid(), None);
}

/// A lane service that serves `meta …` against one session exactly as the
/// dispatch's `meta` arm does: [`super::control_session::cmd_meta_supervisor`]
/// with the proof read from the connection and the session's PTY master
/// ([`super::prove_self_claim_by`]), and every other `meta` as a bare read.
#[cfg(target_os = "macos")]
struct MetaService {
    ctx: Arc<crate::SessionCtx>,
    master: i32,
    alab_image: fn(u32) -> bool,
    replies: Mutex<Vec<String>>,
}

#[cfg(target_os = "macos")]
impl LaneService for MetaService {
    fn authenticate(
        &self,
        _stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, crate::control::Principal, Option<String>)> {
        (super::read_request_line(reader)? == "AUTH ok").then_some((
            Scope::Owner,
            crate::control::Principal::Instance,
            None,
        ))
    }

    fn serve_line(
        &self,
        request: super::Request,
        scope: Scope,
        _stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut super::control_lanes::Outbox,
    ) -> Option<ServeDisposition> {
        let rest = request.line().strip_prefix("meta ").unwrap_or("");
        let master = self.master;
        let alab_image = self.alab_image;
        let prove = |holder: &str| super::prove_self_claim_by(holder, master, &alab_image);
        let reply = super::control_session::cmd_meta_supervisor(
            &self.ctx,
            scope,
            super::serving_connection(),
            &prove,
            rest,
        )
        .map_or_else(|| "ERR usage\n".to_string(), |(reply, _)| reply);
        self.replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(reply.clone());
        out.extend_from_slice(reply.as_bytes());
        None
    }

    fn is_wait(&self, _request: &super::Request, _scope: Scope) -> bool {
        false
    }

    fn subscribe(&self, _request: super::Request, _scope: Scope, _stream: CtlStream) {}

    fn has_live_claim(&self, id: u64) -> bool {
        self.ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .live_claim(crate::metrics::now_us())
            .is_some_and(|claim| claim.conn == Some(id))
    }

    fn release(&self, id: u64) {
        let _ = release_supervisor(
            &self.ctx,
            Some(id),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        );
    }
}

/// The claimant on a real pty: `perl`, its own session leader (so it leads
/// the pty's foreground group), connecting to `sock` and claiming
/// `codex-alab@<pid>` — its own pid (`self`, and `ttl` with a lease), or,
/// from a FORKED child (a descendant, as a model's tool process is), its
/// parent's (`child`).
#[cfg(target_os = "macos")]
const CLAIMANT: &str = r#"
use IO::Socket::UNIX;
my ($sock, $mode) = @ARGV;
my $pid = $$;
my $ttl = $mode eq "ttl" ? " ttl=60000" : "";
my $s;
my $claim = sub {
    $s = IO::Socket::UNIX->new(Peer => $sock) or die "connect: $!";
    print $s "AUTH ok\nmeta set supervisor codex-alab\@$pid$ttl\n";
    my $reply = <$s>;
    print "REPLY $reply";
};
if ($mode eq "child") {
    my $kid = fork();
    if ($kid == 0) { $claim->(); print "DONE\n"; my $hold = <STDIN>; exit 0; }
    waitpid($kid, 0);
} else {
    $claim->();
    print "DONE\n";
    my $hold = <STDIN>;
}
"#;

/// Own the claimant's process group and master, including on an assertion's
/// unwind.
#[cfg(target_os = "macos")]
struct Claimant {
    child: std::process::Child,
    master: Option<std::os::fd::OwnedFd>,
    cleaned: bool,
}

#[cfg(target_os = "macos")]
impl Claimant {
    fn kill_group(&self) {
        // SAFETY: `pre_exec` made this child its own session and group leader.
        if let Ok(pid) = i32::try_from(self.child.id()) {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
    }

    fn kill_and_wait(&mut self) {
        self.kill_group();
        self.master.take();
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the killed claimant exits",
            || self.child.try_wait().expect("wait for the claimant"),
        );
        self.cleaned = true;
    }
}

#[cfg(target_os = "macos")]
impl Drop for Claimant {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        self.kill_group();
        self.master.take();
        let _ = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(10),
            || match self.child.try_wait() {
                Ok(None) => None,
                Ok(Some(_)) | Err(_) => Some(()),
            },
        );
    }
}

/// A claimant spawned on a fresh pty ([`CLAIMANT`]).
#[cfg(target_os = "macos")]
fn claimant(sock: &std::path::Path, mode: &str) -> Claimant {
    use aterm_uds::spawnfd::InheritNothing as _;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: Both descriptors are fresh and owned by this fixture.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    use std::os::fd::AsRawFd as _;
    aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("nonblocking master");
    let stdio = || std::process::Stdio::from(slave.try_clone().expect("dup slave"));
    let mut program = std::process::Command::new("/usr/bin/perl");
    program
        .inherit_nothing()
        .env_clear()
        .env("HOME", sock.parent().expect("scratch socket directory"))
        .args(["-e", CLAIMANT])
        .arg(sock)
        .arg(mode)
        .stdin(stdio())
        .stdout(stdio())
        .stderr(stdio());
    // SAFETY: Runs in the forked child before exec, after stdio is in place.
    unsafe {
        program.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY.into(), 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = program.spawn().expect("spawn the claimant");
    drop(slave);
    Claimant {
        child,
        master: Some(master),
        cleaned: false,
    }
}

/// Read the pty until `DONE` (the claimant printed its reply), with a hang
/// detector; the text read.
#[cfg(target_os = "macos")]
fn read_until_done(master: i32) -> String {
    let mut text = Vec::new();
    let mut buf = [0u8; 512];
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the claimant reports DONE",
        || {
            // SAFETY: Reads into a local buffer from this fixture's open, nonblocking master.
            let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                text.extend_from_slice(&buf[..usize::try_from(n).unwrap_or(0)]);
            } else if n < 0 {
                let error = std::io::Error::last_os_error();
                assert!(
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ),
                    "read claimant: {error}"
                );
            } else {
                panic!("the claimant closed before DONE: {text:?}");
            }
            String::from_utf8_lossy(&text)
                .contains("DONE")
                .then_some(())
        },
    );
    String::from_utf8_lossy(&text).into_owned()
}

/// THE PROOF OVER A REAL CONNECTION: a real process that leads a real pty's
/// foreground group connects to a real socket served by the real lanes and
/// claims under its own pid — proven (`supervisor_proven=1`), by the kernel's
/// peer pid of that connection, the live `tcgetpgrp` of the master and its
/// image (here a stand-in that takes `perl` for an ALab build).
#[cfg(target_os = "macos")]
#[test]
fn a_claim_is_proven_by_its_connections_peer_leading_the_foreground() {
    let dir = crate::test_scratch::short_dir("the claim's socket");
    let sock = dir.path().join("c.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let stand_in: fn(u32) -> bool = |_| true;
    let real: fn(u32) -> bool = crate::self_claim::runs_alab_build;
    for (mode, alab_image, want_proven) in [
        ("self", stand_in, true),
        ("child", stand_in, false),
        ("self", real, false),
        ("ttl", stand_in, false),
    ] {
        let mut claimant = claimant(&sock, mode);
        let pid = claimant.child.id();
        use std::os::fd::AsRawFd as _;
        let master = claimant.master.as_ref().expect("open master").as_raw_fd();
        let handle = crate::session_store::test_handle_on(7503, master);
        let service = Arc::new(MetaService {
            ctx: Arc::clone(&handle.ctx),
            master,
            alab_image,
            replies: Mutex::new(Vec::new()),
        });
        let lanes = Lanes::new(LaneLimits {
            rpc: 1,
            wait: 0,
            inline: 0,
            open: 4,
            subscriptions: 0,
        });
        assert_eq!(lanes.start().rpc, 1, "the request lane starts");
        lanes.publish(service.clone());
        let (stream, _) = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the claimant connects",
            || match listener.accept() {
                Ok(connection) => Some(connection),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
                Err(error) => panic!("accept claimant: {error}"),
            },
        );
        stream.set_nonblocking(false).expect("blocking lane stream");
        assert!(lanes.admit(stream).is_ok(), "admitted");
        let said = read_until_done(master);
        let reply = service
            .replies
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .first()
            .cloned();
        assert_eq!(reply.as_deref(), Some("OK\n"), "{mode}: {said}");
        let holder = format!("codex-alab@{pid}");
        assert_eq!(
            shown(&handle.ctx),
            (Some(holder.clone()), want_proven),
            "{mode}: {said}"
        );
        let meta = super::control_session::cmd_meta(
            &handle.term,
            &crate::session_store::new_store(),
            7503,
            &handle.ctx,
            "",
        )
        .0;
        assert!(
            meta.contains(&format!(
                " supervisor={} supervisor_proven={} ",
                crate::control::pct_encode(&holder),
                u8::from(want_proven)
            )),
            "{meta}"
        );
        claimant.kill_and_wait();
        if want_proven {
            let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
            while shown(&handle.ctx) != (None, false) {
                let _ = release_proven_if_gone(&handle.ctx, -1, crate::metrics::now_us());
                assert!(
                    Instant::now() < deadline,
                    "hang: the claim was never cleared"
                );
                std::thread::yield_now();
            }
        }
    }
}

/// A frame header resolves before its full payload and before ordinary
/// dispatch.
#[test]
fn a_startup_binary_header_is_counted_from_the_current_direct_peer() {
    let store = crate::session_store::new_store();
    let handle = crate::session_store::test_handle(907);
    let witness = Arc::clone(&handle.ctx.witness);
    // SAFETY: Getsid(0) reads this test process's own session identity.
    let leader = unsafe { libc::getsid(0) };
    assert!(leader > 0);
    witness.shell.set((leader, None)).expect("unobserved shell");
    store.write().unwrap().register(handle);
    let active: super::ActiveHandle = Arc::new(Mutex::new(None));
    let (_client, server) = CtlStream::pair().expect("socket pair");
    let serving = ServingFd::enter(&server);
    let selector = aterm_types::control_request::Selector::parse("907");
    let header = |out: &mut Vec<u8>| {
        super::feed_bin_header(
            Some(&selector),
            super::FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
            },
            out,
        )
    };
    assert!(header(&mut Vec::new()));
    assert_eq!(
        witness
            .early_control
            .load(std::sync::atomic::Ordering::Relaxed),
        0,
        "an inherited/undirected fd establishes no local lineage"
    );
    {
        let _direct = super::DirectPeer::enter();
        assert!(header(&mut Vec::new()));
        assert_eq!(
            witness
                .early_control
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the header's direct peer is counted before ordinary dispatch"
        );
    }
    drop(serving);
    assert!(header(&mut Vec::new()));
    assert_eq!(
        witness
            .early_control
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the released worker leaves no old peer behind"
    );
}

/// A subscription resolves on its new worker.
#[test]
fn a_startup_subscription_resolution_uses_its_scoped_peer() {
    let store = crate::session_store::new_store();
    let handle = crate::session_store::test_handle(908);
    let witness = Arc::clone(&handle.ctx.witness);
    // SAFETY: Getsid(0) reads only this test process's own session identity.
    let leader = unsafe { libc::getsid(0) };
    assert!(leader > 0);
    witness.shell.set((leader, None)).expect("unobserved shell");
    store.write().unwrap().register(handle);
    let active: super::ActiveHandle = Arc::new(Mutex::new(None));
    let subscribe = || {
        let mut out = Vec::new();
        super::run_subscribe_with_peer_probe(
            aterm_types::control_request::head("subscribe @908 screen"),
            &active,
            &store,
            Scope::Owner,
            &mut out,
            || true,
        );
        assert!(String::from_utf8(out).unwrap().starts_with("OK subscribe "));
    };
    subscribe();
    assert_eq!(
        witness
            .early_control
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    {
        let (_client, server) = CtlStream::pair().expect("socket pair");
        let _serving = ServingFd::enter(&server);
        let _direct = super::DirectPeer::enter();
        subscribe();
        assert_eq!(
            witness
                .early_control
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
    subscribe();
    assert_eq!(
        witness
            .early_control
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a subsequent undirected request cannot inherit the old peer"
    );
}
