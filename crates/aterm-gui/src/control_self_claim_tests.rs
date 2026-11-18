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

#[test]
fn continuation_observations_require_the_claim_and_reject_older_sequences() {
    use super::control_session::cmd_meta_harness_activity;
    let h = crate::session_store::test_handle_on(7546, -1);
    let ctx = h.ctx.as_ref();
    let no_proof = |_: &str| None;
    let line = "set harness_activity reason=person wait_ms=120000 seq=2";
    assert_eq!(
        cmd_meta_harness_activity(ctx, Some(1), &no_proof, line)
            .unwrap()
            .0,
        "ERR denied\n"
    );
    let me = format!("codex-alab@{}", stand_in());
    claim_supervisor_as(
        ctx,
        &me,
        Some(1),
        None,
        Some(running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    assert_eq!(
        cmd_meta_harness_activity(ctx, Some(1), &no_proof, line).unwrap(),
        ("OK\n".into(), true)
    );
    let earlier = "set harness_activity reason=working wait_ms=- seq=1";
    assert_eq!(
        cmd_meta_harness_activity(ctx, Some(1), &no_proof, earlier).unwrap(),
        ("OK\n".into(), false)
    );
    let meta = ctx.meta.lock().unwrap();
    assert_eq!(
        meta.continuation_activity.as_ref().unwrap().1.reason,
        "person"
    );
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
        aterm_uds::deadline::poll_yield();
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
            },
            crate::Wake::HarnessFact {
                fact: crate::HarnessFact::SelfAct,
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
            },
            crate::Wake::HarnessFact {
                fact: crate::HarnessFact::SelfAct,
                ..
            }
        ]
    ));
}

/// A PROVEN HOLDER'S ACT IS ONE `act` EVENT on `subscribe … events`, through the real meta
/// dispatch: `meta set harness_act continue@v1` pushes `EVENT <local> act continue@v1 rev=<id>`,
/// `<id>` its `harness-act` row's, and no `agent` or `program` event beside it.
/// NEGATIVE CONTROL: the same act refused on a connection the claim is not bound to (and the
/// kernel does not prove) pushes nothing.
#[test]
fn a_proven_holders_act_pushes_one_act_event() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7512, -1);
    store.write().unwrap().register(h.clone());
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
    let refuse = |_: &str| -> Option<Proven> { None };
    let mut watermark = h.ctx.timeline.lock().unwrap().view_head();
    let mut pushed = |rest: &str, conn| {
        let (reply, _) =
            super::meta_wakes_proving_for_test(&store, &h.term, 7512, &h.ctx, rest, conn, &refuse);
        let mut out = String::new();
        watermark = crate::subscribe::drain_timeline_events(
            &h.ctx.timeline.lock().unwrap(),
            "7512",
            watermark,
            &mut out,
        );
        (reply, out)
    };
    let (reply, out) = pushed("set harness_act continue@v1", Some(3));
    assert_eq!(reply, "ERR denied\n");
    assert_eq!(out, "", "control: a refused act pushes nothing");
    let (reply, out) = pushed("set harness_act continue@v1", Some(2));
    assert_eq!(reply, "OK\n");
    let id = h
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|row| row.kind == crate::session_timeline::HARNESS_ACT)
        .map(|row| row.id)
        .next_back()
        .expect("the act's row");
    let events: Vec<&str> = out
        .lines()
        .filter(|l| {
            [" act ", " agent ", " program "]
                .iter()
                .any(|k| l.contains(k))
        })
        .collect();
    assert_eq!(
        events,
        [format!("EVENT 7512 act continue@v1 rev={id}")],
        "{out}"
    );
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
    let mark = crate::session_timeline::LandingMark { program_turns: 7 };
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
fn leased_standing_faces_wait_for_rest_and_keep_unbounded_compatibility() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::tempdir().expect("scratch state dir");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        use super::control_session::{HarnessActSnapshot, cmd_meta_harness_act_with_clock};
        use crate::session_timeline::LandingMark;
        use aterm_agent::harness::acts::TaskFace;
        for (act, face) in [
            ("landed@v1", TaskFace::Landed),
            ("left-to-person@v1", TaskFace::Theirs),
        ] {
            let h = crate::session_store::test_handle_on(7514, -1);
            let ctx = h.ctx.as_ref();
            let base = crate::metrics::now_us();
            claim_supervisor_as(
                ctx,
                &format!("codex-alab@{}", std::process::id()),
                Some(2),
                None,
                Some(running()),
                base,
                aterm_slog::Actor::Ctl,
            )
            .expect("proven holder");
            let snapshot = |program_turns, left_at_rest| HarnessActSnapshot {
                mark: LandingMark { program_turns },
                left_at_rest,
            };
            let serve = |snapshot, rest: &str, now| {
                cmd_meta_harness_act_with_clock(
                    ctx,
                    snapshot,
                    Some(2),
                    aterm_slog::Actor::Ctl,
                    &|_| Some(running()),
                    rest,
                    &|| now,
                )
                .expect("a harness-act command")
            };
            let bounded = format!("set harness_act {act} until_us={}", base + 100);
            // A missing snapshot and a pending turn's working report admit neither face.
            // NEGATIVE CONTROL: checking only Theirs wrongly admits Landed here.
            for point in [None, Some(snapshot(7, false))] {
                assert_eq!(
                    serve(point, &bounded, base),
                    ("ERR busy harness_act\n".to_string(), false)
                );
                let mut timeline = ctx.timeline.lock().unwrap();
                assert!(timeline.last_act().is_none(), "{act}");
                assert!(timeline.landing_mark().is_none(), "{act}");
                assert!(timeline.take_self_tasks().is_empty(), "{act}");
                assert!(timeline.take_self_acts().is_empty(), "{act}");
                assert!(
                    !timeline
                        .since(None)
                        .any(|row| row.kind == crate::session_timeline::HARNESS_ACT)
                );
            }
            assert_eq!(
                serve(Some(snapshot(7, true)), &bounded, base),
                ("OK\n".to_string(), true)
            );
            {
                let mut timeline = ctx.timeline.lock().unwrap();
                assert_eq!(timeline.last_act(), Some(act));
                assert_eq!(timeline.landing_mark(), Some(snapshot(7, true).mark));
                assert_eq!(timeline.take_self_tasks(), [face]);
            }
            assert_eq!(
                serve(Some(snapshot(8, true)), &bounded, base + 100),
                ("ERR expired harness_act\n".to_string(), false)
            );
            assert_eq!(
                ctx.timeline.lock().unwrap().landing_mark(),
                Some(snapshot(7, true).mark)
            );
            // Older clients use this spelling without a lease. Its historical behavior is
            // unchanged; only a bounded client and the paired host fence this window.
            assert_eq!(
                serve(
                    Some(snapshot(8, false)),
                    &format!("set harness_act {act}"),
                    base
                ),
                ("OK\n".to_string(), true)
            );
            let mut timeline = ctx.timeline.lock().unwrap();
            assert_eq!(timeline.landing_mark(), Some(snapshot(8, false).mark));
            assert_eq!(timeline.take_self_tasks(), [face]);
        }
    });
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
fn a_proven_holders_act_is_shown_and_booked_once_and_a_refusal_or_lease_neither() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::tempdir().expect("scratch state dir");
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        proven_holder_booking_and_display(state.path());
    });
}

#[allow(clippy::too_many_lines)]
fn proven_holder_booking_and_display(state_root: &std::path::Path) {
    use super::control_session::{HarnessActSnapshot, cmd_meta_harness_act_with_clock};
    use crate::session_timeline::{LandingMark, SelfAct};
    use aterm_agent::harness::acts::Act;
    use aterm_agent::supervise::approvals::{SelfClaimRow, ledger_under};
    use std::cell::Cell;
    let h = crate::session_store::test_handle_on(7513, -1);
    let ctx = h.ctx.as_ref();
    let base = crate::metrics::now_us();
    let clock = Cell::new(base);
    let now = || clock.get();
    let holder = format!("codex-alab@{}", std::process::id());
    let ledger = ledger_under(state_root, Some(ctx.self_id.as_str()));
    let booked = || -> Vec<(String, String)> {
        std::fs::read_to_string(&ledger)
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let doc: aterm_json::Value = aterm_json::from_str(line).expect("a JSON row");
                let row = SelfClaimRow::of(&doc).expect("only self-claim rows");
                (row.holder.to_owned(), row.act.to_owned())
            })
            .collect()
    };
    let shown = || -> Vec<SelfAct> {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take_self_acts()
            .into()
    };
    let at = |program_turns| {
        Some(HarnessActSnapshot {
            mark: LandingMark { program_turns },
            left_at_rest: true,
        })
    };
    let proof = |_: &str| Some(running());
    let serve = |snapshot, rest: &str| {
        cmd_meta_harness_act_with_clock(
            ctx,
            snapshot,
            Some(2),
            aterm_slog::Actor::Ctl,
            &proof,
            rest,
            &now,
        )
        .expect("a harness-act command")
    };
    let denied = ("ERR denied\n".to_string(), false);
    let ok = ("OK\n".to_string(), true);
    let approve = "set harness_act approve-all@v1";

    // NEGATIVE CONTROLS: no claim, an unproven holder's, a lapsed claim's act and a lease.
    assert_eq!(serve(at(1), approve), denied, "no claim");
    claim_supervisor_as(
        ctx,
        &holder,
        Some(2),
        None,
        None,
        base,
        aterm_slog::Actor::Ctl,
    )
    .expect("an unproven holder");
    assert_eq!(serve(at(1), approve), denied, "unproven");
    assert!(release_supervisor(ctx, None, base, aterm_slog::Actor::Ctl));
    claim_supervisor_as(
        ctx,
        &holder,
        Some(2),
        Some(base + 100),
        Some(running()),
        base,
        aterm_slog::Actor::Ctl,
    )
    .expect("a proven claim that lapses");
    clock.set(base + 100);
    assert_eq!(serve(at(1), approve), denied, "lapsed");
    clock.set(base);
    assert!(
        serve(at(1), "prepare harness_act")
            .0
            .starts_with("OK until_us="),
        "a lease is granted"
    );
    assert_eq!(
        (booked(), shown()),
        (Vec::new(), Vec::new()),
        "none shown or booked"
    );

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
    .expect("a proven claim");
    assert_eq!(serve(at(1), approve), ok);
    assert_eq!(serve(at(1), "set harness_act continue@v1"), ok);
    let mine = |act: &str| (holder.clone(), act.to_owned());
    assert_eq!(booked(), [mine("approve-all@v1"), mine("continue@v1")]);
    assert_eq!(
        shown(),
        [
            SelfAct {
                act: Some(Act::Approved),
                holder: holder.clone()
            },
            SelfAct {
                act: Some(Act::Continued),
                holder: holder.clone()
            }
        ]
    );

    // A standing face told again at the same turn is answered, and neither shown nor booked again.
    let left = "set harness_act left-to-person@v1";
    assert_eq!(serve(at(5), left), ok);
    assert_eq!(serve(at(5), left), ok, "a resend is answered");
    assert_eq!(booked().len(), 3, "a resend books once: {:?}", booked());
    assert_eq!(shown().len(), 1, "a resend shows once");
    // NEGATIVE CONTROLS: a later turn, an act that reopens the work between, a fresh claim
    // after release, and an unknown count are new acts.
    assert_eq!(serve(at(6), left), ok);
    assert_eq!(serve(at(6), "set harness_act continue@v1"), ok);
    assert_eq!(serve(at(6), left), ok);
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
    .expect("reclaimed proven claim");
    assert_eq!(serve(at(6), left), ok, "a new claim's act is not a resend");
    assert_eq!(serve(None, left), ok);
    assert_eq!(serve(None, left), ok);
    assert_eq!(booked().len(), 9, "{:?}", booked());
    assert_eq!(shown().len(), 6);
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
    let landing = |program_turns| crate::session_timeline::LandingMark { program_turns };
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
    for (conn, next) in [(2, landing(3)), (3, landing(4))] {
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
            Some(snapshot(landing(8))),
            Some(2),
            aterm_slog::Actor::Ctl,
            &proof,
            "set harness_act continue"
        ),
        ok
    );
    assert_eq!(mark(), Some(landing(4)), "an ordinary act marks no landing");
    let before = acts(ctx);
    assert_eq!(
        cmd_meta_harness_act(
            ctx,
            Some(snapshot(landing(8))),
            Some(3),
            aterm_slog::Actor::Ctl,
            &no_proof,
            "set harness_act landed@v1"
        ),
        denied
    );
    assert_eq!(acts(ctx), before, "a refused landing records nothing");
    assert_eq!(mark(), Some(landing(4)), "nor replaces the admitted mark");

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
    assert_eq!(mark(), Some(landing(4)));
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
                Some(snapshot(landing(7))),
                Some(2),
                aterm_slog::Actor::Ctl,
                &no_proof,
                &request,
                &guarded_clock
            ),
            ok
        );
        assert!(guarded_read.get(), "deadline read under both commit guards");
        assert_eq!(mark(), Some(landing(7)));
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
                Some(snapshot(landing(20))),
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
        assert_eq!(mark(), Some(landing(7)), "nor replaces the captured mark");
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

/// ONLY A PROVEN CLAIM'S HOLDER TELLS ITS OWN STOP (`meta set harness_held`): the verb takes both
/// arms of ruling F's hold by their `status held=` words and `meta unset harness_held`, answers a
/// malformed line with its usage, and is admitted only from the session's standing proven
/// self-claim, on that claim's connection or one its very process proves; the standing arm told
/// again answers `OK` and moves nothing. NEGATIVE CONTROLS: no claim, an unproven claim, another
/// session's proven claim, a connection that proves nothing, another process's proof and a claim
/// that moved between the look and the write are each `ERR denied` and keep nothing; a usage error
/// leaves the standing stop as it was.
#[test]
fn only_a_proven_claims_holder_tells_its_own_stop() {
    use super::control_session::cmd_meta_harness_held;
    use aterm_agent::harness::acts::HeldBy;
    let h = crate::session_store::test_handle_on(7540, -1);
    let other = crate::session_store::test_handle_on(7541, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let me = format!("codex-alab@{}", stand_in());
    let no_proof = |_: &str| -> Option<Proven> { None };
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    let reused = |_: &str| -> Option<Proven> { Some(gone()) };
    let kept = |ctx: &crate::SessionCtx| {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .self_held(crate::metrics::now_us())
    };
    let denied = Some(("ERR denied\n".to_string(), false));
    let ok = |moved| Some(("OK\n".to_string(), moved));
    let usage = Some((
        "ERR usage: meta set harness_held same-reply|no-work | meta unset harness_held\n"
            .to_string(),
        false,
    ));
    let malformed = [
        "set harness_held",
        "set harness_held held",
        "set harness_held -",
        "set harness_held SAME-REPLY",
        "set harness_held same-reply no-work",
        "unset harness_held same-reply",
    ];
    for rest in [
        "set title x",
        "",
        "set harness_act continue",
        "unset attention",
    ] {
        assert_eq!(
            cmd_meta_harness_held(ctx, Some(1), &proof, rest),
            None,
            "not this verb's: {rest}"
        );
    }
    for rest in malformed {
        assert_eq!(
            cmd_meta_harness_held(ctx, Some(1), &proof, rest),
            usage,
            "{rest}"
        );
    }
    for rest in ["set harness_held same-reply", "unset harness_held"] {
        assert_eq!(
            cmd_meta_harness_held(ctx, Some(1), &proof, rest),
            denied,
            "no claim: {rest}"
        );
    }
    claim_supervisor_as(ctx, &me, Some(1), None, None, now, aterm_slog::Actor::Ctl)
        .expect("an unproven claim");
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(1), &proof, "set harness_held same-reply"),
        denied,
        "an unproven claim tells no stop"
    );
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    claim_supervisor_as(
        &other.ctx,
        &me,
        Some(2),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("another session's proven claim");
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(2), &proof, "set harness_held same-reply"),
        denied,
        "another session's claim"
    );
    assert_eq!(kept(ctx), None);
    claim_supervisor_as(
        ctx,
        &me,
        Some(2),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("this session's proven claim");
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(3), &no_proof, "set harness_held same-reply"),
        denied,
        "a connection that proves nothing"
    );
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(3), &reused, "set harness_held same-reply"),
        denied,
        "another process's proof"
    );
    let moved = |_: &str| {
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        claim_supervisor_as(
            ctx,
            &me,
            Some(2),
            None,
            Some(running()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("claimed again");
        Some(running())
    };
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(3), &moved, "set harness_held same-reply"),
        denied,
        "the claim moved between the look and the write"
    );
    assert_eq!(kept(ctx), None, "a refusal keeps nothing");

    assert_eq!(
        cmd_meta_harness_held(ctx, Some(2), &no_proof, "set harness_held same-reply"),
        ok(true),
        "the claim's own connection"
    );
    assert_eq!(kept(ctx), Some(HeldBy::SameReply));
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(2), &no_proof, "set harness_held same-reply"),
        ok(false),
        "the standing arm told again moves nothing"
    );
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(3), &proof, "set harness_held no-work"),
        ok(true),
        "a connection its process proves"
    );
    assert_eq!(kept(ctx), Some(HeldBy::NoWork));
    for rest in malformed {
        assert_eq!(
            cmd_meta_harness_held(ctx, Some(2), &no_proof, rest),
            usage,
            "{rest}"
        );
    }
    assert_eq!(kept(ctx), Some(HeldBy::NoWork), "a usage error keeps it");
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(2), &no_proof, "unset harness_held"),
        ok(true)
    );
    assert_eq!(kept(ctx), None, "the typing goes on");
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(2), &no_proof, "unset harness_held"),
        ok(false)
    );
    assert_eq!(kept(&other.ctx), None, "nothing reached the other session");
}

/// A READ-ONLY COPY ADMITS NO ACT: a released claim's process keeps its copy for the rollout tap
/// to read ([`crate::self_claim::tap_target`] names it while that process leads the front), and
/// `meta set harness_act` from that process with no claim standing is still `ERR denied`: nothing
/// recorded, no face queued, the copy kept as it was and still no act path's
/// ([`crate::self_claim::copy_of_tab`], [`crate::self_claim::rollout_target`]). POSITIVE
/// CONTROL: the same act under the process's standing proven claim records it.
#[test]
fn a_read_only_copy_admits_no_act() {
    use super::control_session::cmd_meta_harness_act_with_clock;
    use aterm_agent::harness::self_claimed::{Copy, Record, Seen};
    let h = crate::session_store::test_handle_on(7513, -1);
    let ctx = h.ctx.as_ref();
    let sid = ctx.self_id.as_str();
    let now = crate::metrics::now_us;
    let proven = running();
    let group = i32::try_from(stand_in()).expect("a pid");
    let holder = format!("codex-alab@{}", stand_in());
    let copy = Copy {
        pid: proven.pid,
        lifetime: None,
        start: proven.start.clone(),
        home: "/private/tmp/x/codex-home".into(),
        seen: Seen::Present(Record {
            pid: proven.pid,
            session_id: Some("019a0000-0000-7000-8000-00000000000a".to_string()),
            resume_argv: None,
            env: Vec::new(),
            status: Some("idle".to_string()),
            exit: None,
            cwd: None,
            draft_empty: Some(true),
            frame: None,
            goal: None,
            background_terminals: None,
            initializing: false,
        }),
        resume: Err("no-resume-line"),
    };
    let act = |rest: &str| {
        cmd_meta_harness_act_with_clock(
            ctx,
            None,
            Some(2),
            aterm_slog::Actor::Ctl,
            &|_| Some(running()),
            rest,
            &now,
        )
        .expect("a harness-act command")
    };
    crate::self_claim::forget(sid);
    claim_supervisor_as(
        ctx,
        &holder,
        Some(2),
        None,
        Some(proven.clone()),
        now(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the proven claim");
    crate::self_claim::set_copy_for_test(sid, Some(copy.clone()));
    assert!(release_supervisor(ctx, None, now(), aterm_slog::Actor::Ctl));
    let revision = ctx.meta.lock().unwrap().supervisor_revision;
    assert!(crate::self_claim::released_for_test(
        ctx,
        &proven,
        revision,
        true,
        Some(copy.clone())
    ));
    let watched = crate::self_claim::tap_target(ctx, group);
    assert!(
        matches!(watched, Some(crate::self_claim::TapTarget::Watched(_))),
        "{watched:?}"
    );
    for rest in [
        "prepare harness_act",
        "set harness_act continue@v1",
        "set harness_act landed@v1",
    ] {
        assert_eq!(act(rest), ("ERR denied\n".to_string(), false), "{rest}");
    }
    {
        let mut timeline = ctx.timeline.lock().unwrap();
        assert!(timeline.last_act().is_none());
        assert!(timeline.landing_mark().is_none());
        assert!(timeline.take_self_tasks().is_empty());
    }
    assert_eq!(
        crate::self_claim::tap_target(ctx, group),
        watched,
        "the copy is kept"
    );
    assert_eq!(crate::self_claim::copy_of_tab(sid), None);
    assert_eq!(crate::self_claim::rollout_target(ctx), None);

    claim_supervisor_as(
        ctx,
        &holder,
        Some(2),
        None,
        Some(proven),
        now(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the proven claim again");
    assert_eq!(
        act("set harness_act continue@v1"),
        ("OK\n".to_string(), true)
    );
    assert_eq!(ctx.timeline.lock().unwrap().last_act(), Some("continue@v1"));
    crate::self_claim::forget(sid);
}

/// A CLOSED TAB TAKES ITS READ-ONLY COPY WITH IT: the copy a passive release keeps for the
/// rollout tap goes when its tab closes ([`crate::App::retire_session_registration`]), with no
/// harness host running to forget it (`unhosted`), even while its process still runs. NEGATIVE
/// CONTROL: a claim's copy, kept at its release for the exit (its record shows the end coming),
/// stays through the close for the host's own pass ([`crate::self_claim::copy_of_tab`]).
#[test]
fn a_closed_tab_takes_its_read_only_copy_with_it() {
    use aterm_agent::harness::self_claimed::{Copy, Record, Seen};
    let mut app = crate::App::headless_for_test();
    let proven = running();
    let group = i32::try_from(stand_in()).expect("a pid");
    let holder = format!("codex-alab@{}", stand_in());
    let now = crate::metrics::now_us;
    let copy = |status: &str| Copy {
        pid: proven.pid,
        lifetime: None,
        start: proven.start.clone(),
        home: "/private/tmp/x/codex-home".into(),
        seen: Seen::Present(Record {
            pid: proven.pid,
            session_id: Some("019a0000-0000-7000-8000-00000000000d".to_string()),
            resume_argv: None,
            env: Vec::new(),
            status: Some(status.to_string()),
            exit: None,
            cwd: None,
            draft_empty: Some(true),
            frame: None,
            goal: None,
            background_terminals: None,
            initializing: false,
        }),
        resume: Err("no-resume-line"),
    };
    let mut released_tab = |status: &str| {
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
        let local = app.next_session_id - 1;
        let ctx = Arc::clone(&app.pool.get(local).expect("the stub").ctx);
        crate::self_claim::forget(ctx.self_id.as_str());
        claim_supervisor_as(
            &ctx,
            &holder,
            Some(2),
            None,
            Some(proven.clone()),
            now(),
            aterm_slog::Actor::Ctl,
        )
        .expect("the proven claim");
        crate::self_claim::set_copy_for_test(ctx.self_id.as_str(), Some(copy(status)));
        assert!(release_supervisor(
            &ctx,
            None,
            now(),
            aterm_slog::Actor::Ctl
        ));
        let revision = ctx.meta.lock().unwrap().supervisor_revision;
        assert!(crate::self_claim::released_for_test(
            &ctx,
            &proven,
            revision,
            true,
            Some(copy(status))
        ));
        (local, ctx)
    };
    let (read_local, read_ctx) = released_tab("idle");
    let (ended_local, ended_ctx) = released_tab("exited");
    assert!(matches!(
        crate::self_claim::tap_target(&read_ctx, group),
        Some(crate::self_claim::TapTarget::Watched(_))
    ));
    assert_eq!(
        crate::self_claim::copy_of_tab(ended_ctx.self_id.as_str()),
        Some(copy("exited")),
        "kept for the exit"
    );

    app.retire_session_registration(read_local);
    app.retire_session_registration(ended_local);
    assert_eq!(
        crate::self_claim::tap_target(&read_ctx, group),
        None,
        "the read-only copy went with its tab"
    );
    assert_eq!(
        crate::self_claim::copy_of_tab(ended_ctx.self_id.as_str()),
        Some(copy("exited")),
        "the claim's copy is the host's to forget"
    );
    crate::self_claim::forget(ended_ctx.self_id.as_str());
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

/// A SELF-CLAIMANT'S WAIT LAPSES WITH ITS CLAIM: a `kind=wait` entry written on the connection
/// that holds a proven self-claim is its claimant's own (codex-alab sitting out a usage reset by
/// itself), so it shows under Waiting while the claim stands, a renewal keeps it, and it goes when
/// the claim ends: the connection closing (a killed image's), a dead image's claim replaced by the
/// next image's proof, or a `ttl=` lapse (a release past the `ttl=` still reports it); it never
/// crosses an update, which the claim never does. Each lapse that moves the shown entry is the
/// session's attention `meta-change`. NEGATIVE CONTROL: an ask on that same connection, and a wait
/// written on another connection, outlive the claim, as every entry did before.
#[test]
fn a_self_claimants_wait_lapses_with_its_claim() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7530, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str, conn: u64| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7530, &h.ctx, rest, Some(conn), &prove)
            .0
    };
    let kinds = || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .kinds_field()
    };
    let waiting = || {
        let attention = ctx
            .meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .kinded();
        crate::needs::of(&crate::needs::NeedFacts {
            agent: Some(crate::needs::AgentFact {
                word: "unknown",
                detail: None,
                rev: 1,
                subject: None,
            }),
            attention,
            ..crate::needs::NeedFacts::default()
        })
        .waiting
        .and_then(|wait| wait.entry)
        .map(|entry| entry.words())
    };
    let wait = "set attention owner=codex-alab-wait kind=wait waiting out the usage limit";
    let usage = Some("waiting out the usage limit (codex-alab-wait)".to_string());

    assert_eq!(serve(&format!("set supervisor {me}"), 4), "OK\n");
    assert_eq!(shown(ctx), (Some(me.clone()), true));
    assert_eq!(
        serve("set attention owner=ci kind=wait waiting for CI", 9),
        "OK\n"
    );
    assert_eq!(
        serve("set attention owner=codex-alab box: Allow this command?", 4),
        "OK\n"
    );
    assert_eq!(serve(wait, 4), "OK\n");
    assert_eq!(kinds(), "wait:ci,-:codex-alab,wait:codex-alab-wait");
    assert_eq!(waiting(), usage, "the claimant's wait shows while it holds");
    let carried = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .handoff_attention_owners();
    assert_eq!(
        carried
            .iter()
            .map(|row| row.split_once(' ').map_or(row.as_str(), |(owner, _)| owner))
            .collect::<Vec<_>>(),
        ["ci", "codex-alab"],
        "the wait does not cross an update: {carried:?}"
    );
    assert_eq!(serve(&format!("set supervisor {me}"), 4), "OK\n");
    assert_eq!(waiting(), usage, "a renewal keeps it");

    let now = crate::metrics::now_us();
    assert!(release_supervisor(
        ctx,
        Some(4),
        now,
        aterm_slog::Actor::Aterm
    ));
    assert_eq!(shown(ctx), (None, false));
    assert_eq!(
        kinds(),
        "wait:ci,-:codex-alab",
        "the ask and the other wait stand"
    );
    assert_eq!(
        waiting(),
        Some("waiting for CI (ci)".to_string()),
        "the claimant's wait went with its claim"
    );
    let changes = meta_changes(ctx);
    assert_eq!(
        changes.get(changes.len().saturating_sub(2)..),
        Some(
            [
                "field=attention value=box:%20Allow%20this%20command?".to_string(),
                "field=supervisor value=-".to_string(),
            ]
            .as_slice()
        ),
        "{changes:?}"
    );

    let dead = format!("codex-alab@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &dead,
        Some(5),
        None,
        Some(gone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the dead image's claim");
    assert_eq!(serve(wait, 5), "OK\n");
    assert_eq!(waiting(), usage);
    let next = claim_supervisor_as(
        ctx,
        &me,
        Some(6),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the next image's proof replaces a dead one");
    assert_eq!(next.replaced.as_deref(), Some(dead.as_str()));
    assert_eq!(
        kinds(),
        "wait:ci,-:codex-alab",
        "the dead image's wait went"
    );
    let ask_then = |holder: &str| {
        let changes = meta_changes(ctx);
        assert_eq!(
            changes.get(changes.len().saturating_sub(2)..),
            Some(
                [
                    "field=attention value=box:%20Allow%20this%20command?".to_string(),
                    format!("field=supervisor value={holder}"),
                ]
                .as_slice()
            ),
            "{changes:?}"
        );
    };
    ask_then(&me);

    // A `ttl=` claim's lapse is a claim end too, and a release of a claim already past its
    // `ttl=` still reports the wait it took.
    let ttl = |conn| {
        claim_supervisor_as(
            ctx,
            &me,
            Some(conn),
            Some(now + 1_000),
            Some(running()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("the ttl claim")
    };
    let _ = ttl(6);
    assert_eq!(serve(wait, 6), "OK\n");
    assert_eq!(waiting(), usage);
    assert!(crate::session_timeline::lapse_supervisor(ctx, now + 2_000));
    assert_eq!(kinds(), "wait:ci,-:codex-alab", "the ttl lapse took it");
    ask_then("-");
    let _ = ttl(7);
    assert_eq!(serve(wait, 7), "OK\n");
    assert!(
        release_supervisor(ctx, Some(7), now + 2_000, aterm_slog::Actor::Aterm),
        "the lapse is news though the claim was past its ttl"
    );
    assert_eq!(kinds(), "wait:ci,-:codex-alab");
}

/// A WAIT ON A CLAIM CONNECTION LAPSES WITH THAT CONNECTION, THOUGH THE CLAIM ENDED FIRST: aterm
/// can end a claim while its claimant lives on and still takes itself to hold it (a person's
/// `meta unset supervisor`, the image leaving the foreground), so a wait that claimant writes on
/// its connection next is still its own, and it goes when that connection closes (the image
/// killed). NEGATIVE CONTROL: another connection's close, one that bound no claim, takes neither
/// that wait nor its own.
#[test]
fn a_wait_on_a_claim_connection_lapses_with_it_though_the_claim_ended_first() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7531, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str, conn: u64| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7531, &h.ctx, rest, Some(conn), &prove)
            .0
    };
    let kinds = || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .kinds_field()
    };

    assert_eq!(serve(&format!("set supervisor {me}"), 4), "OK\n");
    assert_eq!(
        serve("set attention owner=ci kind=wait waiting for CI", 9),
        "OK\n"
    );
    assert!(release_supervisor(
        ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(shown(ctx), (None, false));
    assert_eq!(
        serve(
            "set attention owner=codex-alab-wait kind=wait waiting out the usage limit",
            4
        ),
        "OK\n"
    );
    super::release_connection_claims(&store, None, 9);
    assert_eq!(
        kinds(),
        "wait:ci,wait:codex-alab-wait",
        "no claim's connection"
    );

    super::release_connection_claims(&store, None, 4);
    assert_eq!(
        kinds(),
        "wait:ci",
        "the claim connection closed: its wait went"
    );
    let changes = meta_changes(ctx);
    assert_eq!(
        changes.last().map(String::as_str),
        Some("field=attention value=waiting%20for%20CI"),
        "{changes:?}"
    );

    // When a released claimant on conn 4 writes a late wait and another proven claimant claims on
    // conn 6 before conn 4 closes, the new claim lapses conn 4's wait at once, and conn 4 remains
    // tracked until it closes; a detached ttl renewal keeps its wait when the original connection
    // closes.
    let now = crate::metrics::now_us();
    claim_supervisor_as(
        ctx,
        "codex-alab@1",
        Some(4),
        None,
        Some(gone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("first claimant on conn 4");
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    assert_eq!(
        serve(
            "set attention owner=stale-wait kind=wait stale wait after release",
            4
        ),
        "OK\n"
    );
    assert_eq!(kinds(), "wait:ci,wait:stale-wait");
    assert_eq!(serve(&format!("set supervisor {me}"), 6), "OK\n");
    assert_eq!(
        kinds(),
        "wait:ci",
        "a new proven claim lapses a released claimant's late wait"
    );
    assert_eq!(
        serve(
            "set attention owner=stale-wait kind=wait second late wait on conn 4",
            4
        ),
        "OK\n"
    );
    assert_eq!(
        serve(
            "set attention owner=codex-alab-wait kind=wait live claimant wait on conn 6",
            6
        ),
        "OK\n"
    );
    assert_eq!(
        kinds(),
        "wait:ci,wait:stale-wait,wait:codex-alab-wait",
        "conn 4 stays tracked across conn 6's claim"
    );
    super::release_connection_claims(&store, None, 4);
    assert_eq!(
        kinds(),
        "wait:ci,wait:codex-alab-wait",
        "closing conn 4 lapses only conn 4's wait"
    );
    claim_supervisor_as(
        ctx,
        &me,
        None,
        Some(now + 60_000_000),
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("detached ttl renewal");
    super::release_connection_claims(&store, None, 6);
    assert_eq!(
        kinds(),
        "wait:ci,wait:codex-alab-wait",
        "closing conn 6 while the same proven claim is live under ttl keeps its wait"
    );

    // An expired ttl claim does not shield a closing connection's wait, and a reconnected socket
    // (conn 8) that bound the same proof before conn 7's delayed EOF keeps the claimant's wait
    // until conn 8 closes too.
    claim_supervisor_as(
        ctx,
        &me,
        Some(7),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("rebound to conn 7");
    claim_supervisor_as(
        ctx,
        &me,
        None,
        Some(1),
        Some(running()),
        0,
        aterm_slog::Actor::Ctl,
    )
    .expect("already expired ttl");
    super::release_connection_claims(&store, None, 7);
    assert_eq!(
        kinds(),
        "wait:ci",
        "an expired ttl claim does not keep conn 7's wait"
    );

    assert_eq!(serve(&format!("set supervisor {me}"), 7), "OK\n");
    assert_eq!(serve(&format!("set supervisor {me}"), 8), "OK\n");
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    assert_eq!(
        serve(
            "set attention owner=codex-alab-wait kind=wait late wait on reconnected conn 8",
            8
        ),
        "OK\n"
    );
    super::release_connection_claims(&store, None, 7);
    assert_eq!(
        kinds(),
        "wait:ci,wait:codex-alab-wait",
        "conn 7's delayed EOF does not kill conn 8's wait while conn 8 is open"
    );
    super::release_connection_claims(&store, None, 8);
    assert_eq!(
        kinds(),
        "wait:ci",
        "closing the last connection for that proof lapses its wait"
    );
}

/// A CLAIM'S LAPSE TAKES WAITS ALONE, WHATEVER KIND ITS WRITER NAMED: the bare owner is always an
/// ask, so `meta set attention owner=- kind=wait …` on the claim connection stores the ask it is,
/// which outlives the claim and crosses an update. NEGATIVE CONTROL: a keyed `kind=wait` written
/// beside it on that connection lapses with the claim and stays out of the update's rows.
#[test]
fn an_ask_written_as_a_wait_on_the_claim_connection_outlives_the_claim() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7532, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7532, &h.ctx, rest, Some(4), &prove).0
    };
    let kinds = || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .kinds_field()
    };

    assert_eq!(serve(&format!("set supervisor {me}")), "OK\n");
    assert_eq!(
        serve("set attention owner=- kind=wait held for review"),
        "OK\n"
    );
    assert_eq!(
        serve("set attention owner=codex-alab-wait kind=wait waiting out the usage limit"),
        "OK\n"
    );
    assert_eq!(kinds(), "ask:-,wait:codex-alab-wait");
    let carried = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .handoff_attention_owners();
    assert_eq!(carried, ["- held%20for%20review"], "{carried:?}");

    assert!(release_supervisor(
        ctx,
        Some(4),
        crate::metrics::now_us(),
        aterm_slog::Actor::Aterm
    ));
    assert_eq!(kinds(), "ask:-", "the ask stands; the wait went");
}

/// EVERY PROVEN CLAIM'S WAIT LAPSES WITH IT, A CONTRACT 3 CLAIM'S AS A SELF-CLAIM'S: a rostered
/// harness proves its process by the same kernel facts (`supervisor_proven=1`), so the wait it
/// writes on its claim connection is its own and goes when that claim ends. NEGATIVE CONTROL: an
/// unproven holder's wait on its own claim connection outlives that claim, since aterm knows no
/// process behind it.
#[test]
fn a_contract_3_claimants_wait_lapses_with_its_claim_and_an_unproven_holders_does_not() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7533, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let serve = |rest: &str, conn: u64| {
        let prove = |holder: &str| holder.contains('@').then(running);
        super::meta_wakes_proving_for_test(&store, &h.term, 7533, &h.ctx, rest, Some(conn), &prove)
            .0
    };
    let kinds = || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .kinds_field()
    };
    let release = |conn| {
        release_supervisor(
            ctx,
            Some(conn),
            crate::metrics::now_us(),
            aterm_slog::Actor::Aterm,
        )
    };

    assert_eq!(serve("set supervisor watcher", 5), "OK\n");
    assert_eq!(
        serve(
            "set attention owner=watch kind=wait waiting for the deploy",
            5
        ),
        "OK\n"
    );
    assert!(release(5));
    assert_eq!(kinds(), "wait:watch", "an unproven holder's wait stands");

    let me = format!("grok-alab@{}", stand_in());
    assert_eq!(
        serve(
            &format!("set supervisor {me} contract=3 scope=relaunch,upgrade"),
            4
        ),
        "OK\n"
    );
    assert!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .claim_proven(crate::metrics::now_us())
    );
    assert_eq!(
        serve(
            "set attention owner=grok-wait kind=wait waiting out the outage",
            4
        ),
        "OK\n"
    );
    assert_eq!(kinds(), "wait:watch,wait:grok-wait");
    assert!(release(4));
    assert_eq!(kinds(), "wait:watch", "the scoped claimant's wait went");

    assert_eq!(serve("set supervisor watcher", 4), "OK\n");
    assert_eq!(
        serve(
            "set attention owner=watch-reused kind=wait unproven holder on reused conn 4",
            4
        ),
        "OK\n"
    );
    assert!(release(4));
    assert_eq!(
        kinds(),
        "wait:watch,wait:watch-reused",
        "an unproven claim on a connection clears any earlier proof recorded for that connection"
    );
}

/// A SELF-CLAIMANT'S STOP LAPSES WITH ITS CLAIM, WHERE ITS WAIT DOES: a stop told through the real
/// `meta` dispatch (`meta set harness_held`, a `MetaChanged` wake; the standing arm again wakes
/// nothing) stands while the proven claim does and across its renewal, and goes when the claim is
/// released, when its connection closes, when a dead image's claim is replaced by the next image's
/// proof and when its process leaves the foreground; a claim past its `ttl=` says it nowhere, and
/// its connection's close takes it; a new claim by the same process starts with none. Each lapse
/// empties the store, not only the read. NEGATIVE CONTROLS: another connection's close takes
/// nothing, a dead image's connection closing after the next image's proof replaced its claim
/// takes nothing of the stop that next image told (its proof is not the one that told it), and a
/// claim the same process renewed on a new connection keeps the stop past the old connection's
/// close until its own closes.
#[test]
fn a_self_claimants_stop_lapses_with_its_claim() {
    use aterm_agent::harness::acts::HeldBy;
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7542, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str, conn: u64| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7542, &h.ctx, rest, Some(conn), &prove)
    };
    let held = || {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        (
            meta.self_held(crate::metrics::now_us()),
            meta.claim_held.is_some(),
        )
    };
    let none = (None, false);
    let told = |arm| (Some(arm), true);

    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    let (reply, wakes) = serve("set harness_held same-reply", 4);
    assert_eq!(reply, "OK\n");
    assert!(
        matches!(
            wakes.as_slice(),
            [crate::Wake::MetaChanged { session: 7542 }]
        ),
        "{wakes:?}"
    );
    let (reply, wakes) = serve("set harness_held same-reply", 4);
    assert_eq!(reply, "OK\n");
    assert!(
        wakes.is_empty(),
        "the standing arm wakes nothing: {wakes:?}"
    );
    assert_eq!(held(), told(HeldBy::SameReply));
    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    assert_eq!(held(), told(HeldBy::SameReply), "a renewal keeps it");

    let now = crate::metrics::now_us();
    assert!(release_supervisor(
        ctx,
        Some(4),
        now,
        aterm_slog::Actor::Aterm
    ));
    assert_eq!(held(), none, "the release took it");
    assert_eq!(
        serve("set harness_held no-work", 4).0,
        "ERR denied\n",
        "no claim stands to tell it"
    );
    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    assert_eq!(held(), none, "a new claim starts with none");

    assert_eq!(serve("set harness_held no-work", 4).0, "OK\n");
    super::release_connection_claims(&store, None, 9);
    assert_eq!(held(), told(HeldBy::NoWork), "another connection's close");
    super::release_connection_claims(&store, None, 4);
    assert_eq!(held(), none, "the claim connection closed: its stop went");

    assert_eq!(serve(&format!("set supervisor {me}"), 7).0, "OK\n");
    assert_eq!(serve("set harness_held same-reply", 7).0, "OK\n");
    assert_eq!(serve(&format!("set supervisor {me}"), 8).0, "OK\n");
    super::release_connection_claims(&store, None, 7);
    assert_eq!(
        held(),
        told(HeldBy::SameReply),
        "the reconnected claim keeps it past the old connection's close"
    );
    super::release_connection_claims(&store, None, 8);
    assert_eq!(held(), none, "its own connection's close takes it");

    let dead = format!("codex-alab@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &dead,
        Some(5),
        None,
        Some(gone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the dead image's claim");
    assert_eq!(serve("set harness_held same-reply", 5).0, "OK\n");
    assert_eq!(held(), told(HeldBy::SameReply));
    let next = claim_supervisor_as(
        ctx,
        &me,
        Some(6),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the next image's proof replaces a dead one");
    assert_eq!(next.replaced.as_deref(), Some(dead.as_str()));
    assert_eq!(held(), none, "the dead image's stop went");

    assert_eq!(serve("set harness_held no-work", 6).0, "OK\n");
    super::release_connection_claims(&store, None, 5);
    assert_eq!(
        held(),
        told(HeldBy::NoWork),
        "the dead image's late close takes nothing of the next image's stop"
    );
    let pid = i32::try_from(stand_in()).expect("a pid");
    assert!(!release_proven_if_gone(ctx, pid, now));
    assert_eq!(held(), told(HeldBy::NoWork), "its process fronts the tab");
    assert!(release_proven_if_gone(ctx, pid + 1, now));
    assert_eq!(held(), none, "its process left the foreground");
    super::release_connection_claims(&store, None, 6);

    // A claim past its `ttl=` stays in the slot unread: its stop is said nowhere and goes when
    // the claim's connection closes.
    assert_eq!(serve(&format!("set supervisor {me}"), 10).0, "OK\n");
    assert_eq!(serve("set harness_held no-work", 10).0, "OK\n");
    claim_supervisor_as(
        ctx,
        &me,
        None,
        Some(1),
        Some(running()),
        0,
        aterm_slog::Actor::Ctl,
    )
    .expect("already expired ttl");
    assert_eq!(held(), (None, true), "an expired claim's stop is not said");
    assert!(!crate::session_timeline::close_claim_connection(
        ctx, 11, now
    ));
    assert_eq!(held(), (None, true), "another connection's close");
    assert!(crate::session_timeline::close_claim_connection(
        ctx, 10, now
    ));
    assert_eq!(held(), none, "the expired claim's connection closed");
}

/// A STOP TOLD BEFORE A LAPSE NEVER COMES BACK WITH THE NEXT CLAIM (SPECS self-claim-held: a new
/// claim, the same process's included, starts with none): a proven claim's stop is kept across a
/// renewal made while that claim stands, but once the renewal's `ttl=` lapsed the same process's
/// next claim is a new one and starts with none, though no sweep took the lapsed claim out of the
/// slot first (`lapse_supervisor`), so a stop its claimant could not lift during the lapse (`meta
/// unset harness_held` is `ERR denied` with no claim standing) is never said again. NEGATIVE
/// CONTROL: the renewal made while the claim stood kept it.
#[test]
fn a_stop_told_before_a_lapse_never_comes_back_with_the_next_claim() {
    use super::control_session::cmd_meta_harness_held;
    use aterm_agent::harness::acts::HeldBy;
    let h = crate::session_store::test_handle_on(7543, -1);
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let no_proof = |_: &str| -> Option<Proven> { None };
    let held = |at: u64| {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        (meta.self_held(at), meta.claim_held.is_some())
    };
    let claim = |expires_us: Option<u64>, at: u64| {
        claim_supervisor_as(
            ctx,
            &me,
            Some(10),
            expires_us,
            Some(running()),
            at,
            aterm_slog::Actor::Ctl,
        )
        .expect("the same process's proven claim");
    };
    let at = crate::metrics::now_us();
    claim(None, at);
    assert_eq!(
        cmd_meta_harness_held(ctx, Some(10), &no_proof, "set harness_held no-work"),
        Some(("OK\n".to_string(), true))
    );
    claim(Some(at + 1), at);
    assert_eq!(
        held(at),
        (Some(HeldBy::NoWork), true),
        "CONTROL: a renewal while the claim stands keeps it"
    );
    assert_eq!(
        held(at + 1),
        (None, true),
        "past its ttl= it is said nowhere"
    );
    claim(None, at + 2);
    assert_eq!(
        held(at + 2),
        (None, false),
        "the next claim after the lapse starts with none"
    );
    assert_eq!(held(crate::metrics::now_us()), (None, false));
}

/// The unix ms a `meta set harness_land` line names as its land act's.
const SINCE_MS: i64 = 1_792_159_320_000;

/// The Land hold the session's live proven self-claim told (`meta set harness_land`), and whether
/// one is kept at all, whatever the claim stands at.
fn land_kept(
    ctx: &crate::SessionCtx,
    at: u64,
) -> (Option<aterm_agent::harness::acts::LandHeld>, bool) {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    (
        meta.self_land(at).map(|hold| hold.held),
        meta.claim_land.is_some(),
    )
}

/// ONLY A PROVEN CLAIM'S HOLDER TELLS ITS OWN LAND HOLD (`meta set harness_land`, the wire of
/// §14.3.3), and the line is read strictly: `since=` and `nudges=` on every line, in any order,
/// `read=1` with its `legs=` (each `HoldLeg::word`), the percent-encoded `open=` words (the first
/// four the strict reading knows kept, every other dropped) and the five counts (held at their
/// largest past it) only beside it, each token at most once; `meta unset harness_land` lifts it.
/// Every malformed line in the wire's list answers its usage, with or without a claim, and leaves
/// the standing hold as it was; an admitted line is `OK`, and the standing hold told again moves
/// nothing. NEGATIVE CONTROLS: no claim, an unproven claim, another session's proven claim, a
/// connection that proves nothing, another process's proof and a claim that moved between the
/// look and the write are each `ERR denied` and keep nothing; nothing reaches another session.
#[test]
fn only_a_proven_claims_holder_tells_its_own_land_hold() {
    use super::control_session::cmd_meta_harness_land;
    use aterm_agent::harness::acts::{HoldLeg, LandHeld, OpenWord};
    let h = crate::session_store::test_handle_on(7544, -1);
    let other = crate::session_store::test_handle_on(7545, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let me = format!("codex-alab@{}", stand_in());
    let no_proof = |_: &str| -> Option<Proven> { None };
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    let reused = |_: &str| -> Option<Proven> { Some(gone()) };
    let kept = |ctx: &crate::SessionCtx| land_kept(ctx, crate::metrics::now_us()).0;
    let denied = Some(("ERR denied\n".to_string(), false));
    let ok = |moved| Some(("OK\n".to_string(), moved));
    let usage = Some((
        "ERR usage: meta set harness_land since=<ms> nudges=<n> [read=1 legs=\u{2026} \
         open=\u{2026} changed=\u{2026} untracked=\u{2026} stashes=\u{2026} unpushed=\u{2026} \
         unlanded=\u{2026} left=\u{2026}] | meta unset harness_land\n"
            .to_string(),
        false,
    ));
    let line = |tail: &str| format!("set harness_land since={SINCE_MS} nudges=0 {tail}");
    let malformed: Vec<String> = [
        "set harness_land".to_string(),
        format!("set harness_land since={SINCE_MS}"),
        "set harness_land nudges=0".to_string(),
        // An unknown token, a bare word, a key in another case.
        line("held=1"),
        line("landed"),
        format!("set harness_land since={SINCE_MS} NUDGES=0"),
        // A leg word aterm does not know.
        line("read=1 legs=repo,unread"),
        line("read=1 legs="),
        line("read=1 legs=REPO"),
        line("read=1 legs=repo,"),
        // A count, or the land act's instant, that is no number.
        format!("set harness_land since={SINCE_MS} nudges=x"),
        format!("set harness_land since={SINCE_MS} nudges=-1"),
        format!("set harness_land since={SINCE_MS} nudges=+1"),
        format!("set harness_land since={SINCE_MS} nudges="),
        line("read=1 legs=repo changed=1.5"),
        line("read=1 legs=repo unlanded=two"),
        "set harness_land since=-5 nudges=0".to_string(),
        "set harness_land since=abc nudges=0".to_string(),
        "set harness_land since=99999999999999999999 nudges=0".to_string(),
        // read=1 without legs, legs without read=1, a read= other than 1.
        line("read=1"),
        line("legs=repo"),
        line("read=0 legs=repo"),
        line("read=true legs=repo"),
        // What only read=1's group carries, without it.
        line("open=follow-up"),
        line("unpushed=2"),
        // A token twice.
        format!("set harness_land since={SINCE_MS} since=1 nudges=0"),
        line("nudges=1"),
        line("read=1 legs=repo legs=open"),
        line("read=1 read=1 legs=repo"),
        line("read=1 legs=repo unpushed=1 unpushed=2"),
        line("left=1"),
        line("read=1 legs=left left=x"),
        // An unset that says more.
        format!("unset harness_land since={SINCE_MS}"),
    ]
    .into();
    for rest in [
        "set title x",
        "",
        "set harness_held same-reply",
        "set harness_lands since=1 nudges=0",
        "unset attention",
    ] {
        assert_eq!(
            cmd_meta_harness_land(ctx, Some(1), &proof, rest),
            None,
            "not this verb's: {rest}"
        );
    }
    for rest in &malformed {
        assert_eq!(
            cmd_meta_harness_land(ctx, Some(1), &proof, rest),
            usage,
            "no claim: {rest}"
        );
    }
    for rest in [line(""), "unset harness_land".to_string()] {
        assert_eq!(
            cmd_meta_harness_land(ctx, Some(1), &proof, &rest),
            denied,
            "no claim: {rest}"
        );
    }
    claim_supervisor_as(ctx, &me, Some(1), None, None, now, aterm_slog::Actor::Ctl)
        .expect("an unproven claim");
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(1), &proof, &line("")),
        denied,
        "an unproven claim tells no hold"
    );
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    claim_supervisor_as(
        &other.ctx,
        &me,
        Some(2),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("another session's proven claim");
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(2), &proof, &line("")),
        denied,
        "another session's claim"
    );
    assert_eq!(kept(ctx), None);
    claim_supervisor_as(
        ctx,
        &me,
        Some(2),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("this session's proven claim");
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(3), &no_proof, &line("")),
        denied,
        "a connection that proves nothing"
    );
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(3), &reused, &line("")),
        denied,
        "another process's proof"
    );
    let moved = |_: &str| {
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        claim_supervisor_as(
            ctx,
            &me,
            Some(2),
            None,
            Some(running()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("claimed again");
        Some(running())
    };
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(3), &moved, &line("")),
        denied,
        "the claim moved between the look and the write"
    );
    assert_eq!(kept(ctx), None, "a refusal keeps nothing");

    // Each field, in any order, read as the wire spells it.
    let begun = LandHeld {
        since_ms: Some(SINCE_MS),
        ..LandHeld::default()
    };
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(2), &no_proof, &line("")),
        ok(true),
        "the claim's own connection"
    );
    assert_eq!(
        kept(ctx),
        Some(begun),
        "the land act typed: landing, no legs read"
    );
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(2), &no_proof, &line("")),
        ok(false),
        "the standing hold told again moves nothing"
    );
    assert_eq!(
        cmd_meta_harness_land(
            ctx,
            Some(3),
            &proof,
            &format!("set harness_land nudges=2 since={SINCE_MS}")
        ),
        ok(true),
        "a connection its process proves, its tokens in another order"
    );
    assert_eq!(kept(ctx), Some(LandHeld { nudges: 2, ..begun }));
    let every = HoldLeg::ALL.iter().fold(0, |bits, leg| bits | leg.bit());
    assert_eq!(
        cmd_meta_harness_land(
            ctx,
            Some(2),
            &no_proof,
            &format!(
                "set harness_land unlanded=5 read=1 since={SINCE_MS} \
                 legs=cut,defense,open,left,repo-unread,repo,needs nudges=3 changed=3 untracked=1 \
                 stashes=2 unpushed=4 open=follow-up,not%20pushed left=2"
            )
        ),
        ok(true)
    );
    let read = LandHeld {
        since_ms: Some(SINCE_MS),
        nudges: 3,
        read: true,
        legs: every,
        open: [
            OpenWord::of("follow-up"),
            OpenWord::of("not pushed"),
            None,
            None,
        ],
        changed: 3,
        untracked: 1,
        stashes: 2,
        unpushed: 4,
        unlanded: 5,
        sub_unpushed: 0,
        parked: 0,
        left: 2,
    };
    assert!(
        ["follow-up", "not pushed", "skipped", "untested", "todo"]
            .into_iter()
            .all(|word| OpenWord::of(word).is_some())
            && OpenWord::of("nonsense").is_none(),
        "PRECONDITION: the words the strict reading knows, and one it does not"
    );
    assert_eq!(kept(ctx), Some(read), "every field of a judged yield");
    for leg in HoldLeg::ALL {
        assert_eq!(
            cmd_meta_harness_land(
                ctx,
                Some(2),
                &no_proof,
                &line(&format!("read=1 legs={}", leg.word()))
            ),
            ok(true),
            "{leg:?}"
        );
        assert_eq!(kept(ctx).map(|held| held.legs), Some(leg.bit()), "{leg:?}");
    }
    assert_eq!(
        cmd_meta_harness_land(
            ctx,
            Some(2),
            &no_proof,
            &line(
                "read=1 legs=open open=nonsense,follow-up,,skipped,%zz,untested,todo,not%20pushed"
            )
        ),
        ok(true)
    );
    assert_eq!(
        kept(ctx).map(|held| held.open),
        Some(["follow-up", "skipped", "untested", "todo"].map(OpenWord::of)),
        "the first four words the strict reading knows, every other dropped"
    );
    assert_eq!(
        cmd_meta_harness_land(
            ctx,
            Some(2),
            &no_proof,
            &format!(
                "set harness_land since={SINCE_MS} nudges=99999999999999999999999 read=1 \
                 legs=repo changed=70000 untracked=65535 stashes=007 unpushed=0 \
                 unlanded=18446744073709551616"
            )
        ),
        ok(true)
    );
    assert_eq!(
        kept(ctx),
        Some(LandHeld {
            since_ms: Some(SINCE_MS),
            nudges: u32::MAX,
            read: true,
            legs: HoldLeg::Repo.bit(),
            changed: u16::MAX,
            untracked: u16::MAX,
            stashes: 7,
            unlanded: u16::MAX,
            ..LandHeld::default()
        }),
        "a count past its field is held at its largest"
    );
    let standing = kept(ctx);
    for rest in &malformed {
        assert_eq!(
            cmd_meta_harness_land(ctx, Some(2), &no_proof, rest),
            usage,
            "{rest}"
        );
    }
    assert_eq!(kept(ctx), standing, "a usage error keeps the standing hold");
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(2), &no_proof, "unset harness_land"),
        ok(true)
    );
    assert_eq!(
        land_kept(ctx, crate::metrics::now_us()),
        (None, false),
        "no landing holds"
    );
    assert_eq!(
        cmd_meta_harness_land(ctx, Some(2), &no_proof, "unset harness_land"),
        ok(false)
    );
    assert_eq!(kept(&other.ctx), None, "nothing reached the other session");
}

/// A SELF-CLAIMANT'S LAND HOLD LAPSES WITH ITS CLAIM, WHERE ITS STOP DOES: a hold told through the
/// real `meta` dispatch (`meta set harness_land`, a `MetaChanged` wake; the standing hold again
/// wakes nothing) stands while the proven claim does and across its renewal, and goes when the
/// claim is released, when its connection closes, when a dead image's claim is replaced by the
/// next image's proof and when its process leaves the foreground; a claim past its `ttl=` shows it
/// nowhere, and its connection's close takes it; a new claim by the same process starts with none.
/// Each lapse empties the store, not only the read, and takes the stop told beside it at the same
/// step. NEGATIVE CONTROLS: another connection's close takes nothing, a dead image's connection
/// closing after the next image's proof replaced its claim takes nothing of the hold that next
/// image told, and a claim the same process renewed on a new connection keeps the hold past the
/// old connection's close until its own closes.
#[test]
fn a_self_claimants_land_hold_lapses_with_its_claim() {
    use aterm_agent::harness::acts::LandHeld;
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7546, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str, conn: u64| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7546, &h.ctx, rest, Some(conn), &prove)
    };
    let tell = |nudges: u32, conn: u64| {
        serve(
            &format!("set harness_land since={SINCE_MS} nudges={nudges}"),
            conn,
        )
        .0
    };
    let hold = |nudges| LandHeld {
        since_ms: Some(SINCE_MS),
        nudges,
        ..LandHeld::default()
    };
    let held = || land_kept(ctx, crate::metrics::now_us());
    let stopped = || {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        meta.claim_held.is_some()
    };
    let none = (None, false);
    let told = |nudges| (Some(hold(nudges)), true);

    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    let (reply, wakes) = serve(&format!("set harness_land since={SINCE_MS} nudges=0"), 4);
    assert_eq!(reply, "OK\n");
    assert!(
        matches!(
            wakes.as_slice(),
            [crate::Wake::MetaChanged { session: 7546 }]
        ),
        "{wakes:?}"
    );
    let (reply, wakes) = serve(&format!("set harness_land nudges=0 since={SINCE_MS}"), 4);
    assert_eq!(reply, "OK\n");
    assert!(
        wakes.is_empty(),
        "the standing hold wakes nothing: {wakes:?}"
    );
    assert_eq!(held(), told(0));
    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    assert_eq!(held(), told(0), "a renewal keeps it");

    let now = crate::metrics::now_us();
    assert_eq!(serve("set harness_held same-reply", 4).0, "OK\n");
    assert!(release_supervisor(
        ctx,
        Some(4),
        now,
        aterm_slog::Actor::Aterm
    ));
    assert_eq!(held(), none, "the release took it");
    assert!(!stopped(), "and the stop beside it");
    assert_eq!(tell(1, 4), "ERR denied\n", "no claim stands to tell it");
    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    assert_eq!(held(), none, "a new claim starts with none");

    assert_eq!(tell(1, 4), "OK\n");
    super::release_connection_claims(&store, None, 9);
    assert_eq!(held(), told(1), "another connection's close");
    super::release_connection_claims(&store, None, 4);
    assert_eq!(held(), none, "the claim connection closed: its hold went");

    assert_eq!(serve(&format!("set supervisor {me}"), 7).0, "OK\n");
    assert_eq!(tell(2, 7), "OK\n");
    assert_eq!(serve(&format!("set supervisor {me}"), 8).0, "OK\n");
    super::release_connection_claims(&store, None, 7);
    assert_eq!(
        held(),
        told(2),
        "the reconnected claim keeps it past the old connection's close"
    );
    super::release_connection_claims(&store, None, 8);
    assert_eq!(held(), none, "its own connection's close takes it");

    let dead = format!("codex-alab@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &dead,
        Some(5),
        None,
        Some(gone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the dead image's claim");
    assert_eq!(tell(3, 5), "OK\n");
    assert_eq!(held(), told(3));
    let next = claim_supervisor_as(
        ctx,
        &me,
        Some(6),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the next image's proof replaces a dead one");
    assert_eq!(next.replaced.as_deref(), Some(dead.as_str()));
    assert_eq!(held(), none, "the dead image's hold went");

    assert_eq!(tell(4, 6), "OK\n");
    super::release_connection_claims(&store, None, 5);
    assert_eq!(
        held(),
        told(4),
        "the dead image's late close takes nothing of the next image's hold"
    );
    let pid = i32::try_from(stand_in()).expect("a pid");
    assert!(!release_proven_if_gone(ctx, pid, now));
    assert_eq!(held(), told(4), "its process fronts the tab");
    assert!(release_proven_if_gone(ctx, pid + 1, now));
    assert_eq!(held(), none, "its process left the foreground");
    super::release_connection_claims(&store, None, 6);

    // A claim past its `ttl=` stays in the slot unread: its hold is shown nowhere and goes when
    // the claim's connection closes, as the stop told beside it does.
    assert_eq!(serve(&format!("set supervisor {me}"), 10).0, "OK\n");
    assert_eq!(tell(5, 10), "OK\n");
    assert_eq!(serve("set harness_held no-work", 10).0, "OK\n");
    claim_supervisor_as(
        ctx,
        &me,
        None,
        Some(1),
        Some(running()),
        0,
        aterm_slog::Actor::Ctl,
    )
    .expect("already expired ttl");
    assert_eq!(held(), (None, true), "an expired claim's hold is not shown");
    assert!(!crate::session_timeline::close_claim_connection(
        ctx, 11, now
    ));
    assert_eq!(held(), (None, true), "another connection's close");
    assert!(crate::session_timeline::close_claim_connection(
        ctx, 10, now
    ));
    assert_eq!(held(), none, "the expired claim's connection closed");
    assert!(!stopped(), "and took the stop with it");
}

/// A LAND HOLD TOLD BEFORE A LAPSE NEVER COMES BACK WITH THE NEXT CLAIM (SPECS self-claim-land, as
/// self-claim-held for the stop): a proven claim's hold is kept across a renewal made while that
/// claim stands, but once the renewal's `ttl=` lapsed the same process's next claim is a new one
/// and starts with none, though no sweep took the lapsed claim out of the slot first, so a hold
/// its claimant could not lift during the lapse (`meta unset harness_land` is `ERR denied` with no
/// claim standing) is never shown again. NEGATIVE CONTROL: the renewal made while the claim stood
/// kept it.
#[test]
fn a_land_hold_told_before_a_lapse_never_comes_back_with_the_next_claim() {
    use super::control_session::cmd_meta_harness_land;
    use aterm_agent::harness::acts::LandHeld;
    let h = crate::session_store::test_handle_on(7547, -1);
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let no_proof = |_: &str| -> Option<Proven> { None };
    let claim = |expires_us: Option<u64>, at: u64| {
        claim_supervisor_as(
            ctx,
            &me,
            Some(10),
            expires_us,
            Some(running()),
            at,
            aterm_slog::Actor::Ctl,
        )
        .expect("the same process's proven claim");
    };
    let at = crate::metrics::now_us();
    claim(None, at);
    assert_eq!(
        cmd_meta_harness_land(
            ctx,
            Some(10),
            &no_proof,
            &format!("set harness_land since={SINCE_MS} nudges=1")
        ),
        Some(("OK\n".to_string(), true))
    );
    let told = LandHeld {
        since_ms: Some(SINCE_MS),
        nudges: 1,
        ..LandHeld::default()
    };
    claim(Some(at + 1), at);
    assert_eq!(
        land_kept(ctx, at),
        (Some(told), true),
        "CONTROL: a renewal while the claim stands keeps it"
    );
    assert_eq!(
        land_kept(ctx, at + 1),
        (None, true),
        "past its ttl= it is shown nowhere"
    );
    claim(None, at + 2);
    assert_eq!(
        land_kept(ctx, at + 2),
        (None, false),
        "the next claim after the lapse starts with none"
    );
    assert_eq!(land_kept(ctx, crate::metrics::now_us()), (None, false));
}

/// The `owner=needs` entry a codex-alab sets for three needs, as it sets it.
const NEEDS_ENTRY: &str = "Codex needs you: the release permission to sign 0.48.0 (+2 more) \u{2014} \
                           carrying on";

/// [`NEEDS_ENTRY`] percent-encoded, as `meta set harness_needs entry=` carries it.
fn needs_entry() -> String {
    aterm_control::wire::pct_encode(NEEDS_ENTRY)
}

/// Put `text` up as the session's `owner=needs kind=ask` entry, or take it down (`None`).
fn put_needs_entry(ctx: &crate::SessionCtx, text: Option<&str>) {
    ctx.meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .set_attention_kind(
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            text.map(str::to_string),
            text.map(|_| aterm_types::needs::AttentionKind::Ask),
        )
        .expect("a slot");
}

/// `meta set|unset harness_needs` as the dispatch serves it, its moves read as one change
/// ([`super::control_session::NeedsMoved::any`]).
fn cmd_meta_harness_needs(
    ctx: &crate::SessionCtx,
    conn: Option<u64>,
    prove: &dyn Fn(&str) -> Option<Proven>,
    rest: &str,
) -> Option<(String, bool)> {
    super::control_session::cmd_meta_harness_needs(ctx, conn, prove, rest)
        .map(|(reply, moved)| (reply, moved.any()))
}

/// The needs the session's live proven self-claim told last and the entry's text they were told
/// with, whatever entry stands, and whether a set is kept at all, whatever the claim stands at.
fn needs_kept(ctx: &crate::SessionCtx, at: u64) -> (Option<(Vec<String>, String)>, bool) {
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    let told = meta.self_needs(at).and_then(|told| {
        told.current()
            .map(|set| (set.needs().to_vec(), set.entry().to_string()))
    });
    (told, meta.claim_needs.is_some())
}

/// What every reader reads of session `ctx`'s needs beside its standing `owner=needs` entry
/// ([`crate::needs::told_of`]).
fn needs_read(ctx: &crate::SessionCtx) -> Option<Vec<String>> {
    let standing = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .attention_owners
        .get(crate::harness_host::NEEDS_ATTENTION_OWNER)
        .map(str::to_string);
    crate::needs::told_of(ctx, crate::metrics::now_us())
        .words_for(standing.as_deref())
        .map(<[String]>::to_vec)
}

fn strings(needs: &[&str]) -> Vec<String> {
    needs.iter().map(|need| (*need).to_string()).collect()
}

/// ONLY A PROVEN CLAIM'S HOLDER TELLS ITS OWN NEEDS YOU SET (`meta set harness_needs`, the wire of
/// §14.3.4), and the line is read strictly: `entry=` exactly once, anywhere on the line, and
/// `need=` one to 32 times in the worker's order, each value percent-decoded (either case, a raw
/// letter passing through), a need non-empty once folded as every reader folds it and at most
/// `NEED_TOLD_MAX` bytes; `meta unset
/// harness_needs` takes it down. Every malformed line in the wire's list answers its usage, with
/// or without a claim, and leaves the standing set as it was; an admitted line is `OK`, and the
/// standing set told again moves nothing. NEGATIVE CONTROLS: no claim, an unproven claim, another
/// session's proven claim, a connection that proves nothing, another process's proof and a claim
/// that moved between the look and the write are each `ERR denied` and keep nothing; a 33rd need
/// and a need one byte past `NEED_TOLD_MAX` are refused where 32 and `NEED_TOLD_MAX` are taken;
/// nothing reaches another session, nor the timeline's set (aterm's own loop's).
#[test]
fn only_a_proven_claims_holder_tells_its_own_needs_set() {
    use super::control_session::HARNESS_NEEDS_MAX;
    use crate::needs::NEED_TOLD_MAX;
    let h = crate::session_store::test_handle_on(7548, -1);
    let other = crate::session_store::test_handle_on(7549, -1);
    let ctx = h.ctx.as_ref();
    let now = crate::metrics::now_us();
    let me = format!("codex-alab@{}", stand_in());
    let no_proof = |_: &str| -> Option<Proven> { None };
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    let reused = |_: &str| -> Option<Proven> { Some(gone()) };
    let kept = |ctx: &crate::SessionCtx| needs_kept(ctx, crate::metrics::now_us()).0;
    let denied = Some(("ERR denied\n".to_string(), false));
    let ok = |moved| Some(("OK\n".to_string(), moved));
    let usage = Some((
        "ERR usage: meta set harness_needs entry=<pct text> need=<pct need> [need=<pct need> \
         \u{2026}] | meta unset harness_needs\n"
            .to_string(),
        false,
    ));
    let entry = needs_entry();
    let line = |tail: &str| format!("set harness_needs entry={entry} {tail}");
    let many = |n: usize| {
        (1..=n)
            .map(|i| format!("need=need%20{i}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let malformed: Vec<String> = [
        "set harness_needs".to_string(),
        // No entry=, entry= twice, an entry empty or empty once sanitized.
        "set harness_needs need=a".to_string(),
        line(&format!("entry={entry} need=a")),
        "set harness_needs entry= need=a".to_string(),
        "set harness_needs entry=%20%20 need=a".to_string(),
        // No need=, or one too many.
        line(""),
        line(&many(HARNESS_NEEDS_MAX + 1)),
        // An unknown token, a bare word, a key in another case.
        line("need=a held=1"),
        line("need=a landed"),
        format!("set harness_needs ENTRY={entry} need=a"),
        line("NEED=a"),
        // A need empty, blank once folded (whitespace, a control, a format character alone),
        // past NEED_TOLD_MAX, or not UTF-8.
        line("need="),
        line("need=a need="),
        line("need=%20"),
        line("need=a need=%0A"),
        line("need=%E2%80%AE need=b"),
        line("need=%E2%80%8B%09"),
        line(&format!("need={}", "x".repeat(NEED_TOLD_MAX + 1))),
        line("need=%FF"),
        line("need=%C3"),
        line("need=%C3%28"),
        // A bad percent escape, in a need or in the entry.
        line("need=%zz"),
        line("need=a%2"),
        line("need=%"),
        line("need=%+1"),
        "set harness_needs entry=Codex%G0needs need=a".to_string(),
        // An unset that says more.
        format!("unset harness_needs entry={entry}"),
        "unset harness_needs need=a".to_string(),
    ]
    .into();
    for rest in [
        "set title x",
        "",
        "set harness_land since=1 nudges=0",
        "set harness_need entry=a need=a",
        "set harness_needs_all entry=a need=a",
        "unset attention",
    ] {
        assert_eq!(
            cmd_meta_harness_needs(ctx, Some(1), &proof, rest),
            None,
            "not this verb's: {rest}"
        );
    }
    for rest in &malformed {
        assert_eq!(
            cmd_meta_harness_needs(ctx, Some(1), &proof, rest),
            usage,
            "no claim: {rest}"
        );
    }
    for rest in [line("need=a"), "unset harness_needs".to_string()] {
        assert_eq!(
            cmd_meta_harness_needs(ctx, Some(1), &proof, &rest),
            denied,
            "no claim: {rest}"
        );
    }
    claim_supervisor_as(ctx, &me, Some(1), None, None, now, aterm_slog::Actor::Ctl)
        .expect("an unproven claim");
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(1), &proof, &line("need=a")),
        denied,
        "an unproven claim tells no set"
    );
    assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
    claim_supervisor_as(
        &other.ctx,
        &me,
        Some(2),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("another session's proven claim");
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(2), &proof, &line("need=a")),
        denied,
        "another session's claim"
    );
    assert_eq!(kept(ctx), None);
    claim_supervisor_as(
        ctx,
        &me,
        Some(2),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("this session's proven claim");
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(3), &no_proof, &line("need=a")),
        denied,
        "a connection that proves nothing"
    );
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(3), &reused, &line("need=a")),
        denied,
        "another process's proof"
    );
    let moved = |_: &str| {
        assert!(release_supervisor(ctx, None, now, aterm_slog::Actor::Ctl));
        claim_supervisor_as(
            ctx,
            &me,
            Some(2),
            None,
            Some(running()),
            now,
            aterm_slog::Actor::Ctl,
        )
        .expect("claimed again");
        Some(running())
    };
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(3), &moved, &line("need=a")),
        denied,
        "the claim moved between the look and the write"
    );
    assert_eq!(
        needs_kept(ctx, now),
        (None, false),
        "a refusal keeps nothing"
    );

    // Each set read as the wire spells it, its entry standing so each move is one.
    put_needs_entry(ctx, Some(NEEDS_ENTRY));
    let told = |needs: &[&str]| Some((strings(needs), NEEDS_ENTRY.to_string()));
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(2), &no_proof, &line("need=a")),
        ok(true),
        "the claim's own connection"
    );
    assert_eq!(kept(ctx), told(&["a"]));
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(2), &no_proof, &line("need=a")),
        ok(false),
        "the standing set told again moves nothing"
    );
    assert_eq!(
        cmd_meta_harness_needs(
            ctx,
            Some(3),
            &proof,
            &format!(
                "set harness_needs need=grant%20Screen%20Recording%20%e2%96%b8%20Privacy \
                 entry={entry} need=sign%200.48.0%3A%20100%25 need=approve▸%2Bnotes"
            )
        ),
        ok(true),
        "a connection its process proves, the entry between the needs"
    );
    assert_eq!(
        kept(ctx),
        told(&[
            "grant Screen Recording ▸ Privacy",
            "sign 0.48.0: 100%",
            "approve▸+notes"
        ]),
        "each value decoded, either case, a raw letter passing through, in the worker's order"
    );
    assert!(
        ctx.timeline
            .lock()
            .unwrap()
            .harness_needs()
            .current()
            .is_none(),
        "never in aterm's own loop's set"
    );
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(2), &no_proof, &line(&many(HARNESS_NEEDS_MAX))),
        ok(true)
    );
    let thirty_two: Vec<String> = (1..=HARNESS_NEEDS_MAX)
        .map(|i| format!("need {i}"))
        .collect();
    assert_eq!(
        kept(ctx),
        Some((thirty_two, NEEDS_ENTRY.to_string())),
        "32 needs, in their order"
    );
    let longest = "\u{e9}".repeat(NEED_TOLD_MAX / 2);
    assert_eq!(
        longest.len(),
        NEED_TOLD_MAX,
        "PRECONDITION: two bytes a letter"
    );
    assert_eq!(
        cmd_meta_harness_needs(
            ctx,
            Some(2),
            &no_proof,
            &line(&format!(
                "need={}",
                aterm_control::wire::pct_encode(&longest)
            ))
        ),
        ok(true)
    );
    assert_eq!(kept(ctx), told(&[longest.as_str()]), "NEED_TOLD_MAX bytes");
    let standing = kept(ctx);
    for rest in &malformed {
        assert_eq!(
            cmd_meta_harness_needs(ctx, Some(2), &no_proof, rest),
            usage,
            "{rest}"
        );
    }
    assert_eq!(kept(ctx), standing, "a usage error keeps the standing set");
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(2), &no_proof, "unset harness_needs"),
        ok(false),
        "the take-down beside the entry it described moves no reading: the entry's own unset does"
    );
    assert_eq!(needs_kept(ctx, now).0, None, "no set told last");
    assert_eq!(
        needs_read(ctx),
        Some(strings(&[longest.as_str()])),
        "the outgoing set keeps describing its entry until it goes, as aterm's own does"
    );
    put_needs_entry(ctx, None);
    assert_eq!(
        cmd_meta_harness_needs(ctx, Some(2), &no_proof, "unset harness_needs"),
        ok(false)
    );
    assert_eq!(needs_read(ctx), None);
    assert_eq!(kept(&other.ctx), None, "nothing reached the other session");
}

/// A SELF-CLAIMANT'S NEEDS YOU SET LAPSES WITH ITS CLAIM, WHERE ITS STOP AND ITS LAND HOLD DO: a
/// set told through the real `meta` dispatch (a `HarnessFact::Needs` wake and nothing else, as
/// aterm's own loop's tell is; the standing set again, and a set whose entry does not stand yet,
/// wake nothing) is read while the proven claim stands and across its renewal, and goes when the
/// claim is released, when its connection closes, when a dead image's claim is replaced by the
/// next image's proof and when its process leaves the foreground; a claim past its `ttl=` reads it
/// nowhere (a reader reads aterm's own loop's set instead), and its connection's close takes it; a
/// new claim by the same process starts with none. Each lapse empties the store, not only the
/// read. NEGATIVE CONTROLS: another connection's close takes nothing, a dead image's connection
/// closing after the next image's proof replaced its claim takes nothing of the set that next
/// image told, and a claim the same process renewed on a new connection keeps the set past the
/// old connection's close until its own closes.
#[test]
fn a_self_claimants_needs_set_lapses_with_its_claim() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7550, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let sid = ctx.self_id.as_str().to_string();
    let me = format!("codex-alab@{}", stand_in());
    let entry = needs_entry();
    let serve = |rest: &str, conn: u64| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7550, &h.ctx, rest, Some(conn), &prove)
    };
    let tell = |need: &str, conn: u64| {
        serve(
            &format!("set harness_needs entry={entry} need={need} need=b need=c"),
            conn,
        )
        .0
    };
    let set = |need: &str| strings(&[need, "b", "c"]);
    let held = || {
        let (told, kept) = needs_kept(ctx, crate::metrics::now_us());
        (told.map(|(needs, _)| needs), kept)
    };
    let none = (None, false);
    let told = |need: &str| (Some(set(need)), true);
    let needs_fact = |wakes: &[crate::Wake]| match wakes {
        [crate::Wake::HarnessFact { sid: woken, fact }] => {
            *woken == sid && *fact == crate::HarnessFact::Needs
        }
        _ => false,
    };

    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    let (reply, wakes) = serve(
        &format!("set harness_needs entry={entry} need=a need=b need=c"),
        4,
    );
    assert_eq!(reply, "OK\n");
    assert!(
        wakes.is_empty(),
        "a set whose entry does not stand yet moves no reading: {wakes:?}"
    );
    put_needs_entry(ctx, Some(NEEDS_ENTRY));
    let (reply, wakes) = serve(
        &format!("set harness_needs need=a entry={entry} need=b need=x"),
        4,
    );
    assert_eq!(reply, "OK\n");
    assert!(needs_fact(&wakes), "a needs fact alone: {wakes:?}");
    let (reply, wakes) = serve(
        &format!("set harness_needs entry={entry} need=a need=b need=x"),
        4,
    );
    assert_eq!(reply, "OK\n");
    assert!(
        wakes.is_empty(),
        "the standing set wakes nothing: {wakes:?}"
    );
    assert_eq!(tell("a", 4), "OK\n");
    assert_eq!(held(), told("a"));
    assert_eq!(needs_read(ctx), Some(set("a")));
    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    assert_eq!(held(), told("a"), "a renewal keeps it");

    let now = crate::metrics::now_us();
    assert!(release_supervisor(
        ctx,
        Some(4),
        now,
        aterm_slog::Actor::Aterm
    ));
    assert_eq!(held(), none, "the release took it");
    assert_eq!(needs_read(ctx), None, "and no reader reads it");
    assert_eq!(tell("d", 4), "ERR denied\n", "no claim stands to tell it");
    assert_eq!(serve(&format!("set supervisor {me}"), 4).0, "OK\n");
    assert_eq!(held(), none, "a new claim starts with none");
    assert_eq!(needs_read(ctx), None, "nor reads the one the release took");

    assert_eq!(tell("d", 4), "OK\n");
    super::release_connection_claims(&store, None, 9);
    assert_eq!(held(), told("d"), "another connection's close");
    super::release_connection_claims(&store, None, 4);
    assert_eq!(held(), none, "the claim connection closed: its set went");

    assert_eq!(serve(&format!("set supervisor {me}"), 7).0, "OK\n");
    assert_eq!(tell("e", 7), "OK\n");
    assert_eq!(serve(&format!("set supervisor {me}"), 8).0, "OK\n");
    super::release_connection_claims(&store, None, 7);
    assert_eq!(
        held(),
        told("e"),
        "the reconnected claim keeps it past the old connection's close"
    );
    super::release_connection_claims(&store, None, 8);
    assert_eq!(held(), none, "its own connection's close takes it");

    let dead = format!("codex-alab@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &dead,
        Some(5),
        None,
        Some(gone()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the dead image's claim");
    assert_eq!(tell("f", 5), "OK\n");
    assert_eq!(held(), told("f"));
    let next = claim_supervisor_as(
        ctx,
        &me,
        Some(6),
        None,
        Some(running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("the next image's proof replaces a dead one");
    assert_eq!(next.replaced.as_deref(), Some(dead.as_str()));
    assert_eq!(held(), none, "the dead image's set went");

    assert_eq!(tell("g", 6), "OK\n");
    super::release_connection_claims(&store, None, 5);
    assert_eq!(
        held(),
        told("g"),
        "the dead image's late close takes nothing of the next image's set"
    );
    let pid = i32::try_from(stand_in()).expect("a pid");
    assert!(!release_proven_if_gone(ctx, pid, now));
    assert_eq!(held(), told("g"), "its process fronts the tab");
    assert!(release_proven_if_gone(ctx, pid + 1, now));
    assert_eq!(held(), none, "its process left the foreground");
    super::release_connection_claims(&store, None, 6);

    // A claim past its `ttl=` stays in the slot unread: its set is read nowhere, a reader reads
    // aterm's own loop's in its place, and it goes when the claim's connection closes.
    assert_eq!(serve(&format!("set supervisor {me}"), 10).0, "OK\n");
    assert_eq!(tell("h", 10), "OK\n");
    claim_supervisor_as(
        ctx,
        &me,
        None,
        Some(1),
        Some(running()),
        0,
        aterm_slog::Actor::Ctl,
    )
    .expect("already expired ttl");
    assert_eq!(held(), (None, true), "an expired claim's set is not read");
    assert!(crate::harness_host::store_needs(
        &store,
        &sid,
        &strings(&["aterm's own"]),
        NEEDS_ENTRY
    ));
    assert_eq!(
        needs_read(ctx),
        Some(strings(&["aterm's own"])),
        "CONTROL: with no proven claim standing, a reader reads aterm's own loop's set"
    );
    assert!(!crate::session_timeline::close_claim_connection(
        ctx, 11, now
    ));
    assert_eq!(held(), (None, true), "another connection's close");
    assert!(crate::session_timeline::close_claim_connection(
        ctx, 10, now
    ));
    assert_eq!(held(), none, "the expired claim's connection closed");
}

/// A NEEDS YOU SET TOLD BEFORE A LAPSE NEVER COMES BACK WITH THE NEXT CLAIM (SPECS
/// self-claim-needs, as self-claim-land for the hold): a proven claim's set is kept across a
/// renewal made while that claim stands, but once the renewal's `ttl=` lapsed the same process's
/// next claim is a new one and starts with none, though no sweep took the lapsed claim out of the
/// slot first, so a set its claimant could not take down during the lapse (`meta unset
/// harness_needs` is `ERR denied` with no claim standing) is never read again. NEGATIVE CONTROL:
/// the renewal made while the claim stood kept it.
#[test]
fn a_needs_set_told_before_a_lapse_never_comes_back_with_the_next_claim() {
    let h = crate::session_store::test_handle_on(7551, -1);
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let no_proof = |_: &str| -> Option<Proven> { None };
    let claim = |expires_us: Option<u64>, at: u64| {
        claim_supervisor_as(
            ctx,
            &me,
            Some(10),
            expires_us,
            Some(running()),
            at,
            aterm_slog::Actor::Ctl,
        )
        .expect("the same process's proven claim");
    };
    put_needs_entry(ctx, Some(NEEDS_ENTRY));
    let at = crate::metrics::now_us();
    claim(None, at);
    assert_eq!(
        cmd_meta_harness_needs(
            ctx,
            Some(10),
            &no_proof,
            &format!("set harness_needs entry={} need=a need=b", needs_entry())
        ),
        Some(("OK\n".to_string(), true))
    );
    let told = (strings(&["a", "b"]), NEEDS_ENTRY.to_string());
    claim(Some(at + 1), at);
    assert_eq!(
        needs_kept(ctx, at),
        (Some(told), true),
        "CONTROL: a renewal while the claim stands keeps it"
    );
    assert_eq!(
        needs_kept(ctx, at + 1),
        (None, true),
        "past its ttl= it is read nowhere"
    );
    claim(None, at + 2);
    assert_eq!(
        needs_kept(ctx, at + 2),
        (None, false),
        "the next claim after the lapse starts with none"
    );
    assert_eq!(needs_kept(ctx, crate::metrics::now_us()), (None, false));
    assert_eq!(
        needs_read(ctx),
        None,
        "the entry stands, read by its own words"
    );
}

/// A SELF-CLAIM'S SET DESCRIBES ONLY THE ENTRY IT WAS TOLD WITH, as aterm's own loop's does
/// (`ToldNeeds::tell` beside the entry as stored): a set told before its entry stands is read once
/// that entry stands; one told for another entry's text is never read, and the outgoing set keeps
/// describing the standing entry until the new one lands; under the claim a set aterm's own loop
/// left on the timeline is never read in its place. NEGATIVE CONTROLS: the same set told for the
/// entry that then stands is read; once the claim ends, aterm's own loop's set is read again.
#[test]
fn a_self_claims_set_waits_for_its_entry_and_describes_no_other() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7552, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let sid = ctx.self_id.as_str().to_string();
    let me = format!("codex-alab@{}", stand_in());
    let no_proof = |_: &str| -> Option<Proven> { None };
    let other_entry = "Codex needs you: approve the release notes \u{2014} carrying on";
    let tell = |text: &str, needs: &str| {
        cmd_meta_harness_needs(
            ctx,
            Some(12),
            &no_proof,
            &format!(
                "set harness_needs entry={} {needs}",
                aterm_control::wire::pct_encode(text)
            ),
        )
    };
    let ok = |moved| Some(("OK\n".to_string(), moved));
    assert!(!crate::harness_host::store_needs(
        &store,
        &sid,
        &strings(&["aterm's own"]),
        NEEDS_ENTRY
    ));
    put_needs_entry(ctx, Some(NEEDS_ENTRY));
    assert_eq!(
        needs_read(ctx),
        Some(strings(&["aterm's own"])),
        "CONTROL: with no claim, aterm's own loop's set is read"
    );
    claim_supervisor_as(
        ctx,
        &me,
        Some(12),
        None,
        Some(running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the proven claim");
    assert_eq!(
        needs_read(ctx),
        None,
        "aterm's own loop's set is never read under the claim"
    );
    put_needs_entry(ctx, None);

    assert_eq!(tell(NEEDS_ENTRY, "need=a need=b need=c"), ok(false));
    assert_eq!(needs_read(ctx), None, "no entry stands");
    put_needs_entry(ctx, Some(NEEDS_ENTRY));
    assert_eq!(
        needs_read(ctx),
        Some(strings(&["a", "b", "c"])),
        "the set told before its entry, once it stands"
    );
    assert_eq!(
        tell(other_entry, "need=d"),
        ok(false),
        "told for another entry: the outgoing set is read meanwhile"
    );
    assert_eq!(needs_read(ctx), Some(strings(&["a", "b", "c"])));
    put_needs_entry(ctx, Some(other_entry));
    assert_eq!(needs_read(ctx), Some(strings(&["d"])), "its entry landed");
    assert_eq!(
        tell(NEEDS_ENTRY, "need=e"),
        ok(false),
        "told for an entry that does not stand"
    );
    put_needs_entry(ctx, Some("Codex needs you: something nobody told aterm"));
    assert_eq!(
        needs_read(ctx),
        None,
        "a set told for another entry's text is never read"
    );
    put_needs_entry(ctx, Some(NEEDS_ENTRY));
    assert_eq!(
        needs_read(ctx),
        Some(strings(&["e"])),
        "CONTROL: the entry it was told for"
    );
    assert!(release_supervisor(
        ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        needs_read(ctx),
        Some(strings(&["aterm's own"])),
        "CONTROL: the claim gone, aterm's own loop's set is read again"
    );
}

/// AN OUTGOING SET GOES WITH THE ENTRY IT DESCRIBED (§12.2.11: "the moment its owner moves or
/// clears it, they go"), through the real `meta` dispatch: a self-claim's set taken down with
/// `meta unset harness_needs` while its entry stands keeps describing that entry, and once the
/// entry is taken down nothing is kept, so the same entry's text set again before the claimant's
/// next tell is read by its own words, never by the set the claimant took down. aterm's own loop's
/// set goes the same way when its entry is cleared. NEGATIVE CONTROLS: while the entry stands, and
/// when it is set again to the same text, the outgoing set is read; the claimant's next tell is
/// read beside the entry that stands.
#[test]
fn an_outgoing_needs_set_goes_with_the_entry_it_described() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7553, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let sid = ctx.self_id.as_str().to_string();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7553, &h.ctx, rest, Some(13), &prove).0
    };
    let entry_line = format!("set attention owner=needs kind=ask {NEEDS_ENTRY}");
    let abc = Some(strings(&["a", "b", "c"]));
    assert_eq!(serve(&format!("set supervisor {me}")), "OK\n");
    assert_eq!(serve(&entry_line), "OK\n");
    assert_eq!(
        serve(&format!(
            "set harness_needs entry={} need=a need=b need=c",
            needs_entry()
        )),
        "OK\n"
    );
    assert_eq!(needs_read(ctx), abc);
    assert_eq!(serve("unset harness_needs"), "OK\n");
    assert_eq!(
        needs_read(ctx),
        abc,
        "CONTROL: the outgoing set describes its entry while it stands"
    );
    assert_eq!(serve(&entry_line), "OK\n");
    assert_eq!(
        needs_read(ctx),
        abc,
        "CONTROL: the same text set again is still the entry it described"
    );
    assert_eq!(serve("unset attention owner=needs"), "OK\n");
    assert_eq!(
        needs_kept(ctx, crate::metrics::now_us()),
        (None, false),
        "its entry went: nothing kept"
    );
    assert_eq!(serve(&entry_line), "OK\n");
    assert_eq!(
        needs_read(ctx),
        None,
        "the same text set again is read by its own words, never by the set taken down"
    );
    assert_eq!(
        serve(&format!("set harness_needs entry={} need=d", needs_entry())),
        "OK\n"
    );
    assert_eq!(
        needs_read(ctx),
        Some(strings(&["d"])),
        "CONTROL: the next tell"
    );

    assert!(release_supervisor(
        ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    let own = Some(strings(&["aterm's own", "and more"]));
    assert!(crate::harness_host::store_needs(
        &store,
        &sid,
        &strings(&["aterm's own", "and more"]),
        NEEDS_ENTRY
    ));
    assert!(!crate::harness_host::store_needs(&store, &sid, &[], ""));
    assert_eq!(
        needs_read(ctx),
        own,
        "CONTROL: aterm's own outgoing set describes its entry while it stands"
    );
    assert_eq!(serve("unset attention owner=needs"), "OK\n");
    assert_eq!(serve(&entry_line), "OK\n");
    assert_eq!(
        needs_read(ctx),
        None,
        "aterm's own outgoing set went with its entry"
    );
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
        let prove = |holder: &str, _: Option<u32>| {
            super::prove_self_claim_by(holder, master, &alab_image).map(Into::into)
        };
        let reply = super::control_session::cmd_meta_supervisor(
            &self.ctx,
            scope,
            super::serving_connection(),
            &prove,
            &|_, _| None,
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
    use std::os::fd::AsRawFd as _;
    let (master, slave, child) = aterm_pty::spawn_session_leader_on_pty(|slave| {
        let mut program = std::process::Command::new("/usr/bin/perl");
        program
            .env_clear()
            .env("HOME", sock.parent().expect("scratch socket directory"))
            .args(["-e", CLAIMANT])
            .arg(sock)
            .arg(mode)
            .stdout(std::process::Stdio::from(slave.try_clone()?))
            .stderr(std::process::Stdio::from(slave.try_clone()?));
        Ok(program)
    })
    .expect("spawn the claimant");
    drop(slave);
    aterm_pty::set_nonblocking(master.as_raw_fd(), true).expect("nonblocking master");
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
            // SAFETY: reads into a local buffer from this fixture's open,
            // nonblocking master, so the outer hang detector can always run.
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
        let started = lanes.start();
        assert_eq!(started.rpc, 1, "the request lane starts");
        lanes.publish(service.clone());
        started.activate();
        let (stream, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN),
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("the claimant connects");
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
                aterm_uds::deadline::poll_yield();
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

/// CONTRACT 3 through the real `meta` dispatch (`docs/DESIGN-harness-repos-2026-10-24.md` §8.1
/// items 3 and 4): a rostered harness's scoped claim is proven or refused whole; a proven one
/// replaces the window host's claim and prints `supervisor_proven=1`, but is never codex-alab's
/// v2 self-claim; a rostered holder never claims without `contract=3`, a scope word unknown or
/// outside its row refuses the claim, and an unrostered holder's v3 claim is an ordinary claim,
/// never proven.
#[test]
fn a_rostered_harness_claims_its_scope_through_meta_proven_or_refused_whole() {
    use crate::session_timeline::SupervisorClaim;
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7520, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let host = format!("aterm-harness@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &host,
        None,
        None,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the host's claim");
    let me = format!("grok-alab@{}", stand_in());
    let serve = |rest: &str, proves: bool| {
        let prove = move |_: &str| proves.then(running);
        super::meta_wakes_proving_for_test(&store, &h.term, 7520, &h.ctx, rest, Some(4), &prove).0
    };
    let scoped_now = || {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        let now = crate::metrics::now_us();
        (
            meta.live_claim(now)
                .and_then(|c: &SupervisorClaim| c.scoped.as_ref())
                .map(|s| (s.claim.scopes.words(), s.claim.versions())),
            meta.claim_proven(now),
        )
    };
    let claim = format!(
        "set supervisor {me} contract=3 scope=relaunch,upgrade version=0.2.0 vendor=1.0.70"
    );

    let reply = serve(&claim, false);
    assert!(reply.starts_with("ERR unproven:"), "{reply}");
    assert_eq!(
        shown(ctx),
        (Some(host.clone()), false),
        "refused whole: the host still holds"
    );

    for bad in [
        format!("set supervisor {me}"),
        format!("set supervisor {me} ttl=500"),
        format!("set supervisor {me} contract=3"),
        format!("set supervisor {me} contract=3 scope=walls"),
        format!("set supervisor {me} contract=3 scope=relaunch,bogus"),
        format!("set supervisor {me} contract=3 scope=relaunch,relaunch"),
        format!("set supervisor {me} contract=3 scope=relaunch ttl=500"),
        format!("set supervisor {me} contract=2 scope=relaunch"),
        format!("set supervisor {me} contract=3 scope=relaunch version=0.1.1"),
    ] {
        let reply = serve(&bad, true);
        assert!(reply.starts_with("ERR usage"), "{bad}: {reply}");
        assert_eq!(shown(ctx), (Some(host.clone()), false), "{bad}");
    }

    assert_eq!(serve(&claim, true), "OK\n");
    assert_eq!(
        shown(ctx),
        (Some(me.clone()), false),
        "the proof replaced the host's claim, and it is not codex-alab's self-claim"
    );
    assert_eq!(
        scoped_now(),
        (
            Some((
                "relaunch,upgrade".to_string(),
                "grok-alab 0.2.0 (grok 1.0.70)".to_string()
            )),
            true
        )
    );
    let status = serve("", true);
    assert!(status.contains(" supervisor_proven=1 "), "{status}");
    assert!(
        status.contains(&format!(" supervisor={me} "))
            || status.contains(" supervisor=grok-alab%40"),
        "{status}"
    );
    assert!(
        serve(&format!("set supervisor {host}"), false).starts_with("ERR busy"),
        "the host cannot take a proven claim back"
    );

    assert!(release_supervisor(
        ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    assert_eq!(
        serve(
            "set supervisor other-alab@4242 contract=3 scope=relaunch",
            true
        ),
        "OK\n"
    );
    assert_eq!(shown(ctx), (Some("other-alab@4242".to_string()), false));
    assert_eq!(
        scoped_now(),
        (None, false),
        "an unrostered holder's claim is never proven"
    );
}

/// claude-code-alab THROUGH THE REAL `meta` DISPATCH (`docs/DESIGN-harness-repos-2026-10-24.md`
/// §11 D53): now it is rostered, its claim without `contract=` (the v2 grammar, a lease included)
/// is `ERR usage` and stores nothing, as is a word that names no duty; its `scope=upgrade` claim,
/// the one its launch makes, is proven or refused whole, replaces the window host's claim, keeps
/// its scope and versions and reads `supervisor_proven=1` on `meta`; the same connection widens
/// it to `relaunch,upgrade`.
#[test]
fn claude_code_alab_claims_through_meta_under_contract_three_alone() {
    use crate::session_timeline::SupervisorClaim;
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7521, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let host = format!("aterm-harness@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &host,
        None,
        None,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the host's claim");
    let me = format!("claude-code-alab@{}", stand_in());
    let serve = |rest: &str, proves: bool| {
        let prove = move |_: &str| proves.then(running);
        super::meta_wakes_proving_for_test(&store, &h.term, 7521, &h.ctx, rest, Some(4), &prove).0
    };
    let scoped_now = || {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        let now = crate::metrics::now_us();
        (
            meta.live_claim(now)
                .and_then(|c: &SupervisorClaim| c.scoped.as_ref())
                .map(|s| (s.claim.scopes.words(), s.claim.versions())),
            meta.claim_proven(now),
        )
    };
    for bad in [
        format!("set supervisor {me}"),
        format!("set supervisor {me} ttl=500"),
        format!("set supervisor {me} scope=upgrade"),
        format!("set supervisor {me} contract=2 scope=upgrade"),
        format!("set supervisor {me} contract=3 scope=upgrade,bogus"),
        format!("set supervisor {me} contract=3 scope=Turn-End"),
    ] {
        let reply = serve(&bad, true);
        assert!(reply.starts_with("ERR usage"), "{bad}: {reply}");
        assert_eq!(shown(ctx), (Some(host.clone()), false), "{bad}");
        assert_eq!(scoped_now(), (None, false), "{bad}");
    }
    let launch =
        format!("set supervisor {me} contract=3 scope=upgrade version=0.2.0 vendor=2.1.310");
    let reply = serve(&launch, false);
    assert!(reply.starts_with("ERR unproven:"), "{reply}");
    assert_eq!(
        shown(ctx),
        (Some(host.clone()), false),
        "refused whole: the host still holds"
    );
    assert_eq!(serve(&launch, true), "OK\n");
    assert_eq!(shown(ctx), (Some(me.clone()), false));
    assert_eq!(
        scoped_now(),
        (
            Some((
                "upgrade".to_string(),
                "claude-code-alab 0.2.0 (claude 2.1.310)".to_string()
            )),
            true
        )
    );
    let read = serve("", true);
    assert!(
        read.contains(&format!(" supervisor={me} supervisor_proven=1 ")),
        "{read}"
    );
    assert!(
        serve(&format!("set supervisor {host}"), false).starts_with("ERR busy"),
        "the host cannot take a proven claim back"
    );
    assert_eq!(
        serve(
            &format!(
                "set supervisor {me} contract=3 scope=relaunch,upgrade version=0.2.0 \
                 vendor=2.1.310"
            ),
            true
        ),
        "OK\n"
    );
    assert_eq!(
        scoped_now().0.map(|(scope, _)| scope).as_deref(),
        Some("relaunch,upgrade")
    );
}

/// claude-code-alab TAKES THE TYPING DUTIES THROUGH THE REAL `meta` DISPATCH
/// (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2, HX-07): its roster row allows every duty,
/// so its claim naming `turn-end` is proven, replaces the window host's claim, reads
/// `supervisor_proven=1` on `meta` and holds the proof a turn end's acts, stop, Land hold and
/// NEEDS YOU are admitted under (`SupervisorClaim::act_proof`); the same connection widens it to
/// every duty, which speaks for the boxes and walls. NEGATIVE CONTROLS: the same claim unproven is
/// `ERR unproven:` and the host still holds; a claim naming a word that is no duty (`bogus`) is
/// `ERR usage`, and the claim before it stands.
#[test]
fn claude_code_alab_claims_the_typing_duties_under_contract_three() {
    use crate::session_timeline::SupervisorClaim;
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7524, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let host = format!("aterm-harness@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &host,
        None,
        None,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the host's claim");
    let me = format!("claude-code-alab@{}", stand_in());
    let serve = |rest: &str, proves: bool| {
        let prove = move |_: &str| proves.then(running);
        super::meta_wakes_proving_for_test(&store, &h.term, 7524, &h.ctx, rest, Some(4), &prove).0
    };
    // The live scoped claim's scope, whether it is proven, whether it holds the act proof, and
    // whether it speaks for the boxes and walls (the host's claim, no scoped one, takes them all).
    let scoped_now = || {
        let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
        let now = crate::metrics::now_us();
        let scoped = meta
            .live_claim(now)
            .filter(|c: &&SupervisorClaim| c.scoped.is_some());
        (
            scoped
                .and_then(|c| c.scoped.as_ref())
                .map(|s| s.claim.scopes.words()),
            meta.claim_proven(now),
            scoped.is_some_and(|c| c.act_proof().is_some()),
            scoped.is_some_and(SupervisorClaim::speaks_for_boxes_and_walls),
        )
    };
    let turn_end = format!(
        "set supervisor {me} contract=3 scope=upgrade,relaunch,turn-end version=0.3.0 \
         vendor=2.1.310"
    );
    let reply = serve(&turn_end, false);
    assert!(reply.starts_with("ERR unproven:"), "{reply}");
    assert_eq!(
        shown(ctx),
        (Some(host.clone()), false),
        "refused whole: the host still holds"
    );
    assert_eq!(scoped_now(), (None, false, false, false));
    assert_eq!(serve(&turn_end, true), "OK\n");
    assert_eq!(shown(ctx), (Some(me.clone()), false));
    let typing = Some("relaunch,upgrade,turn-end".to_string());
    assert_eq!(scoped_now(), (typing.clone(), true, true, false));
    let read = serve("", true);
    assert!(
        read.contains(&format!(" supervisor={me} supervisor_proven=1 ")),
        "{read}"
    );
    let reply = serve(
        &format!("set supervisor {me} contract=3 scope=turn-end,bogus"),
        true,
    );
    assert!(reply.starts_with("ERR usage"), "{reply}");
    assert_eq!(
        scoped_now(),
        (typing, true, true, false),
        "the claim before it stands"
    );
    assert_eq!(
        serve(
            &format!(
                "set supervisor {me} contract=3 \
                 scope=relaunch,upgrade,turn-end,approvals,questions,walls version=0.3.0 \
                 vendor=2.1.310"
            ),
            true
        ),
        "OK\n"
    );
    assert_eq!(
        scoped_now(),
        (
            Some("relaunch,upgrade,turn-end,approvals,questions,walls".to_string()),
            true,
            true,
            true
        )
    );
}

/// HX-06 EXIT TEST: A CONTRACT 4 CLAIM IS GRANTED ITS KNOWN WORDS AND NAMES ITS REVISION
/// (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2), through the real `meta` dispatch. A
/// seed row's harness (grok-alab) claiming `relaunch,turn-end,state,accounts` and a word aterm
/// does not know is answered `OK scope=relaunch,state,accounts revision=<n>`: the unknown word
/// dropped, `turn-end` not granted to a seed row (no verified record stands behind it), `<n>` the
/// claim's revision as `status` prints it, with `supervisor_contract=4` (the session's
/// `supervisor_authority().wire_fields()`, `status`'s words). The same claim again keeps
/// its revision; a claim of words of which none is granted stands with `scope=-`.
/// NEGATIVE CONTROLS: a claim without `version=` or `scope=`, and one naming a malformed
/// `child=`, are `ERR usage`; an unregistered holder is `ERR unproven`, and a claim that proves
/// nothing is `ERR unproven`, each leaving the host's claim standing.
#[test]
fn a_contract_4_claim_is_granted_its_known_words_and_names_its_revision() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7522, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let host = format!("aterm-harness@{}", std::process::id());
    claim_supervisor_as(
        ctx,
        &host,
        None,
        None,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the host's claim");
    let me = format!("grok-alab@{}", stand_in());
    let serve = |rest: &str, proves: bool| {
        let prove = move |_: &str| proves.then(running);
        super::meta_wakes_proving_for_test(&store, &h.term, 7522, &h.ctx, rest, Some(4), &prove).0
    };
    let revision = || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor_revision
    };
    for bad in [
        format!("set supervisor {me} contract=4 scope=relaunch"),
        format!("set supervisor {me} contract=4 version=0.2.0"),
        format!("set supervisor {me} contract=4 scope=relaunch version=0.2.0 child=1:5"),
        format!("set supervisor {me} contract=4 scope=relaunch version=0.2.0 child=x"),
        format!("set supervisor {me} contract=4 scope=Relaunch version=0.2.0"),
        format!("set supervisor {me} contract=4 scope=relaunch,relaunch version=0.2.0"),
    ] {
        let reply = serve(&bad, true);
        assert!(reply.starts_with("ERR usage"), "{bad}: {reply}");
        assert_eq!(shown(ctx), (Some(host.clone()), false), "{bad}");
    }
    let unregistered = serve(
        "set supervisor other-alab@4242 contract=4 scope=relaunch version=1.0.0",
        true,
    );
    assert!(unregistered.starts_with("ERR unproven"), "{unregistered}");
    let claim = format!(
        "set supervisor {me} contract=4 scope=relaunch,turn-end,state,accounts,telepathy \
         version=0.3.0 vendor=1.0.70"
    );
    let unproven = serve(&claim, false);
    assert!(unproven.starts_with("ERR unproven"), "{unproven}");
    assert_eq!(shown(ctx), (Some(host.clone()), false));

    let reply = serve(&claim, true);
    let n = revision();
    assert_eq!(
        reply,
        format!("OK scope=relaunch,state,accounts revision={n}\n")
    );
    assert_eq!(shown(ctx), (Some(me.clone()), false));
    assert_eq!(
        status_words(ctx),
        format!(
            "supervisor_contract=4 supervisor_scope=relaunch,state,accounts supervisor_revision={n}"
        )
    );
    assert_eq!(
        serve(&claim, true),
        format!("OK scope=relaunch,state,accounts revision={n}\n"),
        "the same claim again keeps its revision"
    );
    let nothing = serve(
        &format!("set supervisor {me} contract=4 scope=turn-end,telepathy version=0.3.0"),
        true,
    );
    let moved = revision();
    assert_ne!(moved, n);
    assert_eq!(nothing, format!("OK scope=- revision={moved}\n"));
    assert_eq!(
        status_words(ctx),
        format!("supervisor_contract=4 supervisor_scope=- supervisor_revision={moved}")
    );
}

/// The claim words `status` prints for `ctx`'s session: its supervisor authority's
/// `supervisor_contract=`, `supervisor_scope=` and `supervisor_revision=`
/// (`crate::session_status`, the same call).
fn status_words(ctx: &crate::SessionCtx) -> String {
    ctx.meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .supervisor_authority(crate::metrics::now_us())
        .wire_fields()
}

/// HX-06 EXIT TEST: CONTRACTS 2 AND 3 ARE STILL SERVED, through the real `meta` dispatch:
/// `contract 2,3,4` is what `meta` serves; codex-alab's contract 2 self-claim (no `contract=`)
/// is proven and answered `OK`; a rostered harness's contract 3 claim is answered `OK` with no
/// grant words, and still refuses whole what contract 4 would drop (an unknown word, a duty
/// outside its row, `state`, `child=`).
#[test]
fn contracts_2_and_3_are_still_served() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7523, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let serve = |rest: &str| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7523, &h.ctx, rest, Some(4), &prove).0
    };
    assert_eq!(
        aterm_agent::harness::contract::SERVED,
        [2, 3, 4],
        "contract 2,3,4 served"
    );
    let codex = format!("codex-alab@{}", stand_in());
    assert_eq!(serve(&format!("set supervisor {codex}")), "OK\n");
    assert_eq!(shown(ctx), (Some(codex.clone()), true));
    assert!(release_supervisor(
        ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));
    let me = format!("grok-alab@{}", stand_in());
    for bad in [
        format!("set supervisor {me} contract=3 scope=relaunch,telepathy"),
        format!("set supervisor {me} contract=3 scope=walls"),
        format!("set supervisor {me} contract=3 scope=state"),
        format!("set supervisor {me} contract=3 scope=relaunch child=4243:1763000000123456"),
    ] {
        let reply = serve(&bad);
        assert!(reply.starts_with("ERR usage"), "{bad}: {reply}");
        assert_eq!(shown(ctx).0, None, "{bad}");
    }
    assert_eq!(
        serve(&format!(
            "set supervisor {me} contract=3 scope=relaunch,upgrade version=0.2.0"
        )),
        "OK\n"
    );
    assert!(
        status_words(ctx).starts_with("supervisor_contract=3 supervisor_scope=relaunch,upgrade "),
        "{}",
        status_words(ctx)
    );
}

/// Put `holder`'s proven contract 3 claim of `duties` on `ctx`'s session, bound to connection
/// `conn`, proven as the running stand-in ([`running`]). The scope is a fixture
/// ([`Scopes::for_test`](aterm_agent::harness::contract::Scopes::for_test)), so it may name a
/// duty `holder`'s roster row does not allow.
fn claim_scoped(
    ctx: &crate::SessionCtx,
    holder: &str,
    duties: &[aterm_agent::harness::contract::Scope],
    conn: u64,
) {
    claim_scoped_under(ctx, holder, 3, duties, conn);
}

/// [`claim_scoped`] under `contract` (3 or 4).
fn claim_scoped_under(
    ctx: &crate::SessionCtx,
    holder: &str,
    contract: u32,
    duties: &[aterm_agent::harness::contract::Scope],
    conn: u64,
) {
    let aterm_agent::harness::contract::Words::Scoped(mut claim) =
        aterm_agent::harness::contract::read(holder, &["contract=3", "scope=upgrade"])
    else {
        panic!("the roster admits {holder}");
    };
    claim.contract = contract;
    claim.scopes = aterm_agent::harness::contract::Scopes::for_test(duties);
    crate::session_timeline::claim_supervisor_scoped(
        ctx,
        holder,
        Some(conn),
        crate::harness_claim::Scoped {
            proven: running(),
            claim,
            child: None,
        },
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("the proven scoped claim");
}

/// What a session keeps of its claimant's own reports: its stop, its Land hold, its NEEDS YOU set
/// (each as read while the claim stands), and the acts on its timeline.
fn reports(ctx: &crate::SessionCtx) -> (bool, bool, bool, usize) {
    let now = crate::metrics::now_us();
    let acts = ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .since(None)
        .filter(|e| e.kind == crate::session_timeline::HARNESS_ACT)
        .count();
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    (
        meta.self_held(now).is_some(),
        meta.self_land(now).is_some(),
        meta.self_needs(now)
            .is_some_and(|told| told.current().is_some()),
        acts,
    )
}

/// HX-06 EXIT TEST: AN ACT IS ADMITTED FOR ANY PROVEN CLAIM THAT HOLDS ITS SCOPE
/// (`docs/DESIGN-harness-interface-2026-11-12.md` §2.2). Each act names the duty it reports on
/// ([`aterm_agent::harness::acts::act_scope`]): `continue@v1` turn-end, `approve-all@v1`
/// approvals, `api-retry@v1` walls, `upgrade@v1` upgrade; the lease, the stop, Land's hold and
/// NEEDS YOU report on turn-end, the continuation report on state. codex-alab's contract 2
/// self-claim takes every duty, an act naming none included. A proven contract 4 claim (and a
/// contract 3 one, the same rule) is admitted for the acts whose duty it holds, on its own
/// connection or one its very process proves, each read as that claim's while it stands.
/// NEGATIVE CONTROLS: another connection that proves nothing is `ERR denied`; an act naming no
/// duty is contract 2's alone; the claim renewed without `turn-end` (a new revision) still
/// approves and reports its state but continues nothing, its stop, hold and set go, and they do
/// not come back when it takes `turn-end` again.
#[test]
fn an_act_is_admitted_for_any_proven_claim_that_holds_its_scope() {
    use super::control_session::{
        cmd_meta_harness_act, cmd_meta_harness_activity, cmd_meta_harness_held,
        cmd_meta_harness_land,
    };
    use aterm_agent::harness::contract::{Scope, UNIVERSAL};
    let h = crate::session_store::test_handle_on(7560, -1);
    let ctx = h.ctx.as_ref();
    let no_proof = |_: &str| -> Option<Proven> { None };
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    let ok = Some(("OK\n".to_string(), true));
    let denied = Some(("ERR denied\n".to_string(), false));
    let act = |conn: u64, prove: &dyn Fn(&str) -> Option<Proven>, act: &str| {
        cmd_meta_harness_act(
            ctx,
            None,
            Some(conn),
            aterm_slog::Actor::Ctl,
            prove,
            &format!("set harness_act {act}"),
        )
    };
    let entry = needs_entry();
    let tell_all = |conn: u64, prove: &dyn Fn(&str) -> Option<Proven>| {
        [
            cmd_meta_harness_held(ctx, Some(conn), prove, "set harness_held no-work"),
            cmd_meta_harness_land(
                ctx,
                Some(conn),
                prove,
                &format!("set harness_land since={SINCE_MS} nudges=1"),
            ),
            cmd_meta_harness_needs(
                ctx,
                Some(conn),
                prove,
                &format!("set harness_needs entry={entry} need=sign%200.48.0"),
            ),
            cmd_meta_harness_activity(
                ctx,
                Some(conn),
                prove,
                "set harness_activity reason=waiting wait_ms=- seq=1",
            ),
        ]
    };
    put_needs_entry(ctx, Some(NEEDS_ENTRY));

    // codex-alab's contract 2 self-claim: every act, one naming no duty included.
    let codex = format!("codex-alab@{}", stand_in());
    claim_supervisor_as(
        ctx,
        &codex,
        Some(1),
        None,
        Some(running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("codex-alab's proven self-claim");
    assert_eq!(act(1, &no_proof, "codex-act@v1"), ok);
    assert!(release_supervisor(
        ctx,
        None,
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl
    ));

    let me = format!("claude-code-alab@{}", stand_in());
    claim_scoped_under(ctx, &me, UNIVERSAL, &Scope::ALL, 2);
    assert_eq!(act(2, &no_proof, "continue@v1"), ok);
    assert_eq!(
        act(3, &proof, "approve-all@v1"),
        ok,
        "a connection its very process proves"
    );
    assert_eq!(
        act(3, &no_proof, "approve-all@v1"),
        denied,
        "a connection that proves nothing"
    );
    assert_eq!(
        act(2, &no_proof, "codex-act@v1"),
        denied,
        "an act naming no duty is contract 2's alone"
    );
    let lease = cmd_meta_harness_act(
        ctx,
        None,
        Some(2),
        aterm_slog::Actor::Ctl,
        &no_proof,
        "prepare harness_act",
    )
    .expect("the act verb");
    assert!(lease.0.starts_with("OK until_us="), "{lease:?}");
    for (told, line) in tell_all(3, &no_proof)
        .into_iter()
        .zip(["held", "land", "needs", "activity"])
    {
        assert_eq!(told, denied, "{line}: a connection that proves nothing");
    }
    for (told, line) in tell_all(2, &no_proof)
        .into_iter()
        .zip(["held", "land", "needs", "activity"])
    {
        assert_eq!(told, ok, "{line}");
    }
    assert_eq!(
        reports(ctx),
        (true, true, true, 3),
        "each read as the claim's own"
    );
    let booked = ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .last_act()
        .map(str::to_owned);
    assert_eq!(booked.as_deref(), Some("approve-all@v1"));

    // The same process renews its claim without turn-end: a new revision that still approves
    // and reports its state, and continues nothing.
    claim_scoped_under(
        ctx,
        &me,
        UNIVERSAL,
        &[
            Scope::Relaunch,
            Scope::Upgrade,
            Scope::Approvals,
            Scope::Questions,
            Scope::Walls,
            Scope::State,
        ],
        2,
    );
    assert_eq!(act(2, &no_proof, "continue@v1"), denied, "no turn-end act");
    assert_eq!(act(2, &no_proof, "api-retry@v1"), ok, "walls is held");
    for (told, line) in tell_all(2, &no_proof)
        .into_iter()
        .zip(["held", "land", "needs"])
    {
        assert_eq!(told, denied, "{line}: no turn-end report");
    }
    assert_eq!(
        tell_all(2, &no_proof)[3],
        Some(("OK\n".to_string(), false)),
        "its state is still its own to report (the same report moves nothing)"
    );
    assert_eq!(reports(ctx), (false, false, false, 4));

    // A contract 3 claim follows the same rule.
    claim_scoped(
        ctx,
        &me,
        &[Scope::Relaunch, Scope::Upgrade, Scope::TurnEnd],
        2,
    );
    assert_eq!(
        reports(ctx),
        (false, false, false, 4),
        "a stop, hold or set told before the narrowing never comes back"
    );
    assert_eq!(act(2, &no_proof, "continue@v1"), ok);
    assert_eq!(act(2, &no_proof, "upgrade@v1"), ok, "upgrade is held");
    assert_eq!(
        act(2, &no_proof, "approve-all@v1"),
        denied,
        "approvals is not"
    );
}

/// A CLAIM HOLDING ONLY `relaunch,upgrade` WRITES NO TURN-END ACT: claude-code-alab's and
/// grok-alab's proven contract 3 claims of `relaunch,upgrade`, on their own connection and on a
/// connection their process proves, are `ERR denied` for a turn-end act, its lease, the stop,
/// Land's hold, NEEDS YOU and the continuation report, and nothing is kept: no act on the
/// timeline, no stop, hold or set read or stored. (Their `upgrade@v1` act is admitted: the claim
/// holds `upgrade`.) CONTROL: the same claimant widened to `turn-end` (a scope fixture) on the
/// same connection records its act, so the refusal is the scope's.
#[test]
fn a_claim_holding_only_relaunch_and_upgrade_writes_no_act() {
    use super::control_session::{
        cmd_meta_harness_act, cmd_meta_harness_activity, cmd_meta_harness_held,
        cmd_meta_harness_land,
    };
    use aterm_agent::harness::contract::Scope;
    let denied = Some(("ERR denied\n".to_string(), false));
    let no_proof = |_: &str| -> Option<Proven> { None };
    let proof = |_: &str| -> Option<Proven> { Some(running()) };
    for (port, id) in [(7562, "claude-code-alab"), (7563, "grok-alab")] {
        let h = crate::session_store::test_handle_on(port, -1);
        let ctx = h.ctx.as_ref();
        put_needs_entry(ctx, Some(NEEDS_ENTRY));
        let me = format!("{id}@{}", stand_in());
        claim_scoped(ctx, &me, &[Scope::Relaunch, Scope::Upgrade], 2);
        let entry = needs_entry();
        for (conn, prove) in [
            (2, &no_proof as &dyn Fn(&str) -> Option<Proven>),
            (3, &proof as &dyn Fn(&str) -> Option<Proven>),
        ] {
            for line in ["set harness_act continue@v1", "prepare harness_act"] {
                assert_eq!(
                    cmd_meta_harness_act(
                        ctx,
                        None,
                        Some(conn),
                        aterm_slog::Actor::Ctl,
                        prove,
                        line
                    ),
                    denied,
                    "{id} on {conn}: {line}"
                );
            }
            assert_eq!(
                cmd_meta_harness_held(ctx, Some(conn), prove, "set harness_held same-reply"),
                denied,
                "{id} on {conn}"
            );
            assert_eq!(
                cmd_meta_harness_land(
                    ctx,
                    Some(conn),
                    prove,
                    &format!("set harness_land since={SINCE_MS} nudges=0")
                ),
                denied,
                "{id} on {conn}"
            );
            assert_eq!(
                cmd_meta_harness_needs(
                    ctx,
                    Some(conn),
                    prove,
                    &format!("set harness_needs entry={entry} need=a")
                ),
                denied,
                "{id} on {conn}"
            );
            assert_eq!(
                cmd_meta_harness_activity(
                    ctx,
                    Some(conn),
                    prove,
                    "set harness_activity reason=waiting wait_ms=- seq=1"
                ),
                denied,
                "{id} on {conn}"
            );
        }
        assert_eq!(reports(ctx), (false, false, false, 0), "{id}: nothing read");
        {
            let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
            assert!(
                meta.claim_held.is_none()
                    && meta.claim_land.is_none()
                    && meta.claim_needs.is_none(),
                "{id}: nothing stored"
            );
            assert!(meta.continuation_activity.is_none(), "{id}: no report kept");
        }
        if id == "claude-code-alab" {
            claim_scoped(
                ctx,
                &me,
                &[Scope::Relaunch, Scope::Upgrade, Scope::TurnEnd],
                2,
            );
            assert_eq!(
                cmd_meta_harness_act(
                    ctx,
                    None,
                    Some(2),
                    aterm_slog::Actor::Ctl,
                    &no_proof,
                    "set harness_act continue@v1"
                ),
                Some(("OK\n".to_string(), true)),
                "CONTROL: the claim widened to turn-end records its act"
            );
        }
    }
}

/// A scoped renewal publishes semantic scope and version changes through MetaChanged and moves the
/// claim's revision, while an identical renewal wakes nothing and keeps it. The holder row is not
/// duplicated for those changes.
#[test]
fn a_scoped_renewal_wakes_for_scope_version_and_vendor_changes() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7550, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("grok-alab@{}", stand_in());
    let serve = |words: &str| {
        super::meta_wakes_proving_for_test(
            &store,
            &h.term,
            7550,
            &h.ctx,
            &format!("set supervisor {me} contract=3 {words}"),
            Some(4),
            &|_| Some(running()),
        )
    };
    let claims = [
        "scope=upgrade version=0.3.0 vendor=1.0.70",
        "scope=relaunch version=0.3.0 vendor=1.0.70",
        "scope=relaunch version=0.4.0 vendor=1.0.70",
        "scope=relaunch version=0.4.0 vendor=1.0.71",
        "scope=relaunch",
    ];
    for words in claims {
        let earlier = ctx.meta.lock().unwrap().supervisor_revision;
        let (reply, wakes) = serve(words);
        assert_eq!(reply, "OK\n");
        assert!(
            matches!(
                wakes.as_slice(),
                [crate::Wake::MetaChanged { session: 7550 }]
            ),
            "a semantic claim change wakes the window: {words}: {wakes:?}"
        );
        let before = ctx.meta.lock().unwrap().supervisor_revision;
        assert_ne!(
            before, earlier,
            "a semantic claim change is a new revision: {words}"
        );
        let (reply, wakes) = serve(words);
        assert_eq!(reply, "OK\n");
        assert!(
            wakes.is_empty(),
            "an identical renewal wakes nothing: {wakes:?}"
        );
        let meta = ctx.meta.lock().unwrap();
        assert_eq!(
            meta.supervisor_revision, before,
            "an identical renewal keeps the revision: {words}"
        );
        assert_eq!(
            meta.live_scoped(crate::metrics::now_us())
                .expect("scoped")
                .claim
                .scopes
                .words(),
            words
                .split_whitespace()
                .next()
                .expect("scope")
                .strip_prefix("scope=")
                .expect("scope value")
        );
    }
    let (_, wakes) = serve("scope=upgrade,relaunch");
    assert_eq!(wakes.len(), 1, "adding a scope wakes the window");
    let before = ctx.meta.lock().unwrap().supervisor_revision;
    let (_, wakes) = serve("scope=relaunch,upgrade");
    assert!(wakes.is_empty(), "scope word order changes no authority");
    assert_eq!(
        ctx.meta.lock().unwrap().supervisor_revision,
        before,
        "scope word order keeps the authority revision"
    );
    assert_eq!(
        meta_changes(ctx)
            .into_iter()
            .filter(|row| row.starts_with("field=supervisor "))
            .count(),
        1,
        "renewal changes no holder spelling"
    );
}

/// A contract 4 renewal can change the unknown or withheld words it asks without changing
/// its granted authority. It still places the claim and retains the latest canonical request
/// for a later scope-less record to ask again.
#[test]
fn a_scoped_renewal_keeps_authority_while_replacing_its_ungranted_request() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7550, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("grok-alab@{}", stand_in());
    let serve = |scopes: &str| {
        super::meta_wakes_proving_for_test(
            &store,
            &h.term,
            7550,
            &h.ctx,
            &format!("set supervisor {me} contract=4 scope={scopes} version=0.3.0 vendor=1.0.70"),
            Some(4),
            &|_| Some(running()),
        )
    };
    let (reply, wakes) = serve("relaunch,turn-end,telepathy");
    assert!(reply.starts_with("OK scope=relaunch revision="), "{reply}");
    assert!(matches!(
        wakes.as_slice(),
        [crate::Wake::MetaChanged { session: 7550 }]
    ));
    let (authority, placements) = {
        let meta = ctx.meta.lock().unwrap();
        let scoped = meta.live_scoped(crate::metrics::now_us()).expect("scoped");
        assert_eq!(scoped.claim.scopes.words(), "relaunch");
        assert_eq!(scoped.claim.claimed, "relaunch,telepathy,turn-end");
        (
            meta.supervisor_authority(crate::metrics::now_us()),
            meta.claim_placements,
        )
    };
    for (n, scopes) in ["walls,telekinesis,relaunch", "telekinesis,relaunch,walls"]
        .into_iter()
        .enumerate()
    {
        let (reply, wakes) = serve(scopes);
        assert_eq!(
            reply,
            format!("OK scope=relaunch revision={}\n", authority.revision())
        );
        assert!(wakes.is_empty(), "the grant is unchanged: {wakes:?}");
        let meta = ctx.meta.lock().unwrap();
        assert_eq!(
            meta.supervisor_authority(crate::metrics::now_us()),
            authority,
            "requested words alone change no authority or revision"
        );
        assert_eq!(meta.claim_placements, placements + n as u64 + 1);
        let scoped = meta.live_scoped(crate::metrics::now_us()).expect("scoped");
        assert_eq!(scoped.claim.scopes.words(), "relaunch");
        assert_eq!(scoped.claim.claimed, "relaunch,telekinesis,walls");
    }

    let (reply, wakes) = serve("walls,telekinesis,relaunch,upgrade");
    assert!(
        reply.starts_with("OK scope=relaunch,upgrade revision="),
        "{reply}"
    );
    assert!(matches!(
        wakes.as_slice(),
        [crate::Wake::MetaChanged { session: 7550 }]
    ));
    let meta = ctx.meta.lock().unwrap();
    assert_ne!(meta.supervisor_revision, authority.revision());
    assert_eq!(meta.claim_placements, placements + 3);
    assert_eq!(
        meta.live_scoped(crate::metrics::now_us())
            .expect("scoped")
            .claim
            .scopes
            .words(),
        "relaunch,upgrade",
        "a real additional duty remains an authority change"
    );
}

/// A RENEWAL WAKES `MetaChanged` WHEN ITS PROVEN IDENTITY CHANGED OR IT LAPSED A WAIT: an ordinary
/// renewal under the same proven identity with no wait to lapse wakes nothing (`changed == false`),
/// whereas a renewal of the same holder spelling under a new birth (`renewed_identity_changed`) or
/// one that lapses a late wait written on a released claimant's connection wakes `MetaChanged`
/// (`changed == true`) without recording a duplicate `field=supervisor` row.
#[test]
fn a_renewal_wakes_meta_changed_when_it_changes_proven_identity_or_lapses_a_wait() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7549, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str, conn: u64, proof: Proven| {
        let prove = move |_: &str| Some(proof.clone());
        super::meta_wakes_proving_for_test(&store, &h.term, 7549, &h.ctx, rest, Some(conn), &prove)
    };
    let first = running();
    let second = Proven {
        pid: first.pid,
        start: format!("{} (reborn)", first.start),
    };

    let (reply, wakes) = serve(&format!("set supervisor {me}"), 4, first.clone());
    assert_eq!(reply, "OK\n");
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    let (_, wakes) = serve(&format!("set supervisor {me}"), 4, first.clone());
    assert!(
        wakes.is_empty(),
        "NEGATIVE CONTROL: an ordinary renewal under the same birth wakes nothing: {wakes:?}"
    );

    assert_eq!(
        serve("set harness_held no-work", 4, first.clone()).0,
        "OK\n"
    );
    assert_eq!(
        serve(
            &format!("set harness_land since={SINCE_MS} nudges=1"),
            4,
            first.clone()
        )
        .0,
        "OK\n"
    );
    let before_supervisor_rows = meta_changes(ctx)
        .into_iter()
        .filter(|row| row.starts_with("field=supervisor "))
        .count();

    let (reply, wakes) = serve(&format!("set supervisor {me}"), 5, second.clone());
    assert_eq!(reply, "OK\n");
    assert!(
        matches!(
            wakes.as_slice(),
            [crate::Wake::MetaChanged { session: 7549 }]
        ),
        "a renewal under a new proven birth must wake MetaChanged: {wakes:?}"
    );
    let now = crate::metrics::now_us();
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .self_held(now),
        None
    );
    assert_eq!(land_kept(ctx, now), (None, false));
    let after_supervisor_rows = meta_changes(ctx)
        .into_iter()
        .filter(|row| row.starts_with("field=supervisor "))
        .count();
    assert_eq!(
        after_supervisor_rows, before_supervisor_rows,
        "a renewal does not record a duplicate field=supervisor meta-change"
    );

    // Conn 4 still holds `first` in `claimant_conn`; a late wait written on conn 4 is lapsed when
    // `second` renews its claim on conn 5, and that renewal must also wake `MetaChanged`.
    assert_eq!(
        serve(
            "set attention owner=stale-wait kind=wait late wait on conn 4",
            4,
            second.clone()
        )
        .0,
        "OK\n"
    );
    let (reply, wakes) = serve(&format!("set supervisor {me}"), 5, second);
    assert_eq!(reply, "OK\n");
    assert!(
        matches!(
            wakes.as_slice(),
            [crate::Wake::MetaChanged { session: 7549 }]
        ),
        "a renewal that lapses a stale wait must wake MetaChanged: {wakes:?}"
    );
    assert_eq!(
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .kinds_field(),
        "-"
    );
}

/// A RECONFIRMATION IN PLACE IS NO CLAIM TRANSITION: codex-alab's same-holder `meta set
/// supervisor` on its claim connection under the same proven birth, which it sends at the top of
/// every follow tick, leaves the authority a sealed rotation's Prepare and receipt were bound to
/// (the identity and revision `Seal::admits` reads, the revision native admission compares, the
/// whole snapshot its Signal compares) exactly as it was, so the replayed prepare after a commit is
/// still admitted. The same holder on another connection, or under a new birth, is a new authority
/// and ends it.
#[test]
fn a_reconfirmation_in_place_keeps_the_sealed_authority_and_a_moved_claim_ends_it() {
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7556, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str, conn: u64, proof: &Proven| {
        let proof = proof.clone();
        let prove = move |_: &str| Some(proof.clone());
        super::meta_wakes_proving_for_test(&store, &h.term, 7556, &h.ctx, rest, Some(conn), &prove)
            .0
    };
    let reconfirm = |conn: u64, proof: &Proven| serve(&format!("set supervisor {me}"), conn, proof);
    let authority = || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .supervisor_authority(crate::metrics::now_us())
    };
    let first = running();
    assert_eq!(reconfirm(4, &first), "OK\n");
    let sealed = authority();
    assert_eq!(sealed.identity(), Some(&first));
    assert_eq!(sealed.connection(), Some(4));
    assert_eq!(
        serve(
            "set harness_activity reason=person wait_ms=120000 seq=1",
            4,
            &first
        ),
        "OK\n"
    );
    assert_eq!(reconfirm(4, &first), "OK\n");
    assert_eq!(
        authority(),
        sealed,
        "a reconfirmation in place keeps the sealed authority, its revision included"
    );
    let meta = ctx.meta.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(
        meta.continuation_activity
            .as_ref()
            .map(|(revision, _)| *revision),
        Some(meta.supervisor_revision),
        "the continuation it reported still reads as the standing claim's"
    );
    drop(meta);

    assert_eq!(reconfirm(5, &first), "OK\n");
    let moved = authority();
    assert_eq!(moved.connection(), Some(5));
    assert_ne!(
        moved.revision(),
        sealed.revision(),
        "the claim moved to another connection is a new authority"
    );
    let reborn = Proven {
        pid: first.pid,
        start: format!("{} (reborn)", first.start),
    };
    assert_eq!(reconfirm(5, &reborn), "OK\n");
    let renewed = authority();
    assert_eq!(renewed.identity(), Some(&reborn));
    assert_ne!(
        renewed.revision(),
        moved.revision(),
        "the claim under a new birth is a new authority"
    );
}

/// A CREDENTIAL IS NO PERSON'S NEED, WHOEVER RUNS THE LOOP: a self-claim's set (codex-alab's
/// `meta set harness_needs`) loses its credential lines at ingest, read by aterm's own loop's
/// reader (`credential_need`), and each want they name goes on the one path (the registry, else
/// `aterm secrets need`). The claimant's `owner=needs` entry is read against what it said, never
/// against the entry aterm stands for it: an entry leading with a credential stands for no one
/// until its set says what else it counts (an await task's rev never moves for it); a set of
/// credentials alone stands nothing; a mixed set stands the entry rebuilt for the person's needs,
/// rebuilt again when the claimant tells another set under its unchanged entry, and the
/// claimant's own entry stands again once its set holds no credential; a set told before its
/// entry stands once that entry comes. NEGATIVE CONTROLS: a set with no credential line keeps the
/// claimant's entry and set as told, moves no entry and files nothing; a malformed line naming a
/// credential still answers its usage and files nothing; a release's want (the release
/// machine's) is not filed.
#[test]
fn a_self_claims_credential_needs_reach_no_person() {
    use crate::task_record::{TaskMark, TaskState, mark_session_task};
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle_on(7556, -1);
    store.write().unwrap().register(h.clone());
    let ctx = h.ctx.as_ref();
    mark_session_task(&h.ctx, h.local_id, &TaskMark::new(TaskState::Working)).expect("unfenced");
    let me = format!("codex-alab@{}", stand_in());
    let serve = |rest: &str| {
        let prove = |_: &str| Some(running());
        super::meta_wakes_proving_for_test(&store, &h.term, 7556, &h.ctx, rest, Some(4), &prove)
    };
    let pct = aterm_control::wire::pct_encode;
    let put = |entry: &str| serve(&format!("set attention owner=needs kind=ask {entry}")).0;
    let tell = |entry: &str, needs: &[&str]| {
        let needs: Vec<String> = needs.iter().map(|n| format!("need={}", pct(n))).collect();
        serve(&format!(
            "set harness_needs entry={} {}",
            pct(entry),
            needs.join(" ")
        ))
    };
    let standing = || {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .get(crate::harness_host::NEEDS_ATTENTION_OWNER)
            .map(str::to_string)
    };
    let rev = || {
        ctx.timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .task()
            .map(|task| task.rev)
    };
    let filed = || super::control_session::CREDENTIALS_RESOLVED.take();
    let meta_moved = |wakes: &[crate::Wake]| {
        wakes
            .iter()
            .any(|w| matches!(w, crate::Wake::MetaChanged { session: 7556 }))
    };
    let codex = |words: &str| format!("Codex needs you: {words} \u{2014} carrying on");
    assert_eq!(serve(&format!("set supervisor {me}")).0, "OK\n");
    filed();

    let one = codex("the GitHub token for alabsystems");
    let before = rev();
    assert_eq!(put(&one), "OK\n");
    assert_eq!(standing(), None, "an entry leading with a credential");
    assert_eq!(tell(&one, &["the GitHub token for alabsystems"]).0, "OK\n");
    assert_eq!((standing(), needs_read(ctx)), (None, None));
    assert_eq!(needs_kept(ctx, crate::metrics::now_us()).0, None);
    assert_eq!(filed(), vec![strings(&["gh:release"])]);

    let two = codex("the notary password (+1 more)");
    assert_eq!(put(&two), "OK\n");
    assert_eq!(standing(), None, "its set has yet to say what it counts");
    let (reply, wakes) = tell(
        &two,
        &["the notary password", "an SSH deploy key for the site"],
    );
    assert_eq!(reply, "OK\n");
    assert!(!meta_moved(&wakes), "no entry ever stood: {wakes:?}");
    assert_eq!((standing(), needs_read(ctx)), (None, None));
    assert_eq!(rev(), before, "no await task woke for a credential");
    assert_eq!(filed(), vec![strings(&["notary", "site-deployer-key"])]);

    let notes = "approve the 0.48.0 release notes";
    let screen = "grant Screen Recording to com.aterm.aterm";
    let changelog = "review the changelog for 0.48.0";
    let mixed = codex("the GitHub token for alabsystems (+2 more)");
    assert_eq!(put(&mixed), "OK\n");
    let (reply, wakes) = tell(&mixed, &["the GitHub token for alabsystems", notes, screen]);
    assert_eq!(reply, "OK\n");
    assert!(meta_moved(&wakes), "the entry was rebuilt: {wakes:?}");
    assert_eq!(
        standing(),
        Some(codex("approve the 0.48.0 release notes (+1 more)"))
    );
    assert_eq!(needs_read(ctx), Some(strings(&[notes, screen])));
    assert_eq!(filed(), vec![strings(&["gh:release"])]);

    // The claimant's first need and count stay, so it writes no new entry: its next set is read
    // against what it said, and the entry aterm stood for the last one is rebuilt.
    let (reply, wakes) = tell(
        &mixed,
        &["the GitHub token for alabsystems", changelog, screen],
    );
    assert_eq!(reply, "OK\n");
    assert!(meta_moved(&wakes), "{wakes:?}");
    assert_eq!(
        standing(),
        Some(codex("review the changelog for 0.48.0 (+1 more)"))
    );
    assert_eq!(needs_read(ctx), Some(strings(&[changelog, screen])));
    filed();

    let first = codex("approve the 0.48.0 release notes (+1 more)");
    assert_eq!(put(&first), "OK\n");
    assert_eq!(
        standing().as_deref(),
        Some(first.as_str()),
        "a person's need leads"
    );
    assert_eq!(tell(&first, &[notes, "the notary password"]).0, "OK\n");
    assert_eq!(standing(), Some(codex(notes)));
    assert_eq!(needs_read(ctx), Some(strings(&[notes])));
    let (reply, wakes) = tell(&first, &[notes, screen]);
    assert_eq!(reply, "OK\n");
    assert!(
        meta_moved(&wakes),
        "the claimant's own entry is back: {wakes:?}"
    );
    assert_eq!(standing().as_deref(), Some(first.as_str()));
    assert_eq!(needs_read(ctx), Some(strings(&[notes, screen])));
    filed();

    let late = codex("an SSH deploy key for the site (+1 more)");
    assert_eq!(
        tell(&late, &["an SSH deploy key for the site", changelog]).0,
        "OK\n"
    );
    assert_eq!(
        standing().as_deref(),
        Some(first.as_str()),
        "a set told before its entry waits for it"
    );
    assert_eq!(put(&late), "OK\n");
    assert_eq!(standing(), Some(codex(changelog)));
    assert_eq!(needs_read(ctx), Some(strings(&[changelog])));
    assert_eq!(filed(), vec![strings(&["site-deployer-key"])]);

    let release =
        "Codex needs you: the release signing certificate (+1 more) \u{2014} waits on you";
    assert_eq!(put(release), "OK\n");
    assert_eq!(
        standing(),
        None,
        "an entry leading with a credential comes down"
    );
    assert_eq!(
        tell(release, &["the release signing certificate", notes]).0,
        "OK\n"
    );
    assert_eq!(
        standing().as_deref(),
        Some("Codex needs you: approve the 0.48.0 release notes \u{2014} waits on you")
    );
    assert_eq!(needs_read(ctx), Some(strings(&[notes])));
    assert_eq!(filed(), Vec::<Vec<String>>::new(), "the release machine's");

    assert_eq!(put(NEEDS_ENTRY), "OK\n");
    let (reply, wakes) = tell(NEEDS_ENTRY, &["a", "b", "c"]);
    assert_eq!(reply, "OK\n");
    assert!(
        !meta_moved(&wakes),
        "CONTROL: no credential, no entry move: {wakes:?}"
    );
    assert_eq!(standing().as_deref(), Some(NEEDS_ENTRY));
    assert_eq!(needs_read(ctx), Some(strings(&["a", "b", "c"])));
    assert_eq!(filed(), Vec::<Vec<String>>::new());

    let (reply, _) = serve(&format!(
        "set harness_needs entry={} need={} bogus=1",
        needs_entry(),
        pct("the notary password")
    ));
    assert!(
        reply.starts_with("ERR usage: meta set harness_needs"),
        "{reply}"
    );
    assert_eq!(needs_read(ctx), Some(strings(&["a", "b", "c"])), "CONTROL");
    assert_eq!(filed(), Vec::<Vec<String>>::new(), "CONTROL: nothing filed");
}

/// What HX-07's claimant writes.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hx07Verb {
    Send,
    Key,
    /// A turn with the default submit: its Enter pressed through the seam.
    Turn,
    /// A turn whose Enter is `submit=guarded:<re>`'s, pressed under the terminal lock.
    GuardedTurn,
}

/// Where HX-07 puts a person's key against one of the claimant's writes.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hx07Window {
    /// Only the person's free-running keys, in bursts, wherever they fall.
    Free,
    /// The person's hands off for the whole write: what it does when nobody types.
    Hushed,
    /// One key processed after the claimant read `human_seq` and before it wrote.
    BeforeWrite,
    /// The person's hands off but for one key processed inside a turn, after every look the turn
    /// takes before its Enter and before that Enter is written
    /// ([`super::control_session::set_before_submit_for_test`]).
    BeforeEnter,
}

/// One write of HX-07's claimant: the `human_seq` it read, its bytes, and its reply.
#[cfg(unix)]
#[derive(Debug)]
struct Hx07Write {
    verb: Hx07Verb,
    window: Hx07Window,
    read: u64,
    text: Vec<u8>,
    reply: String,
}

#[cfg(unix)]
impl Hx07Write {
    /// The first line of its reply.
    fn head(&self) -> &str {
        self.reply.lines().next().unwrap_or_default()
    }

    /// A word `<key>=<n>` of its reply's first line.
    fn count(&self, key: &str) -> Option<usize> {
        self.head().split_whitespace().find_map(|w| {
            w.strip_prefix(key)
                .and_then(|w| w.strip_prefix('='))
                .and_then(|n| n.parse().ok())
        })
    }

    /// How many Enters a turn's reply says it pressed: `pressed=<n>`, else one for a submitted
    /// turn and none for any other.
    fn presses(&self) -> usize {
        self.count("pressed")
            .unwrap_or(usize::from(self.head().contains(" submitted=1 ")))
    }

    /// The bytes its reply says reached the program, in the order they did: a `send`'s or
    /// `key`'s whole text on `OK` and nothing otherwise; a turn's text where it pasted or
    /// pressed, then each Enter it pressed.
    fn landed(&self) -> Vec<u8> {
        match self.verb {
            Hx07Verb::Send | Hx07Verb::Key if self.head() == "OK" => self.text.clone(),
            Hx07Verb::Send | Hx07Verb::Key => Vec::new(),
            Hx07Verb::Turn | Hx07Verb::GuardedTurn => {
                let presses = self.presses();
                let mut bytes = Vec::new();
                if presses > 0 || self.count("pasted").is_some_and(|n| n > 0) {
                    bytes.extend_from_slice(&self.text);
                }
                bytes.extend(std::iter::repeat_n(b'\r', presses));
                bytes
            }
        }
    }

    /// Whether it took effect: a `send` or `key` answered `OK`, a turn that pressed Enter.
    fn went(&self) -> bool {
        match self.verb {
            Hx07Verb::Send | Hx07Verb::Key => self.head() == "OK",
            Hx07Verb::Turn | Hx07Verb::GuardedTurn => self.presses() > 0,
        }
    }

    /// Whether its reply is the person's skip: `OK skipped reason=person`, or a turn's.
    fn yielded(&self) -> bool {
        let head = self.head();
        head.starts_with("OK") && head.contains("skipped reason=person")
    }
}

/// One HX-07 run: every byte the program read, the person's keys in the order the main thread
/// took them (each with `human_seq` as that key left it), and the claimant's writes.
#[cfg(unix)]
struct Hx07Run {
    out: Vec<u8>,
    person: Vec<(u8, u64)>,
    writes: Vec<Hx07Write>,
}

/// What the main thread is handed: a person's key (acknowledged once it is written, where the
/// claimant forces one), or a control write delivered as `Wake::Input` delivers one.
#[cfg(unix)]
enum Hx07Main {
    Person(u8, Option<std::sync::mpsc::Sender<()>>),
    /// Answered once everything handed before it was taken.
    Sync(std::sync::mpsc::Sender<()>),
    Control(Box<Hx07Control>),
}

/// End one worker's work on return and unwind alike.
#[cfg(unix)]
struct Hx07Done<'a>(&'a std::sync::atomic::AtomicBool);

#[cfg(unix)]
impl Drop for Hx07Done<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Callback-local custody: cancel workers, then close their queued replies, before the scope joins.
#[cfg(unix)]
struct Hx07Owner<'a> {
    _done: Hx07Done<'a>,
    _drained: Hx07Done<'a>,
    input: std::sync::mpsc::Receiver<Hx07Main>,
}

#[cfg(unix)]
impl<'a> Hx07Owner<'a> {
    fn new(
        input: std::sync::mpsc::Receiver<Hx07Main>,
        done: &'a std::sync::atomic::AtomicBool,
        drained: &'a std::sync::atomic::AtomicBool,
    ) -> Self {
        Self {
            _done: Hx07Done(done),
            _drained: Hx07Done(drained),
            input,
        }
    }
}

/// A control write for the main thread: the event, its mark and fence, and where its outcome goes.
#[cfg(unix)]
struct Hx07Control {
    ev: crate::input::InputEvent,
    mark: crate::control_input_ack::InputMark,
    fence: Option<crate::session_timeline::SupervisorInputDelivery>,
    reply: std::sync::mpsc::Sender<crate::input::InputOutcome>,
}

/// HX-07's script: per round, each verb once with a person's key forced between its read and
/// its write; each turn once more with one forced before its Enter is written; each turn once
/// with the person's hands off; and the rest left to the person's free-running bursts.
#[cfg(unix)]
fn hx07_script() -> Vec<(Hx07Verb, Hx07Window)> {
    let mut script = Vec::new();
    for _ in 0..3 {
        for verb in [Hx07Verb::Send, Hx07Verb::Key] {
            script.push((verb, Hx07Window::BeforeWrite));
            script.extend(std::iter::repeat_n((verb, Hx07Window::Free), 6));
        }
        for verb in [Hx07Verb::Turn, Hx07Verb::GuardedTurn] {
            script.push((verb, Hx07Window::BeforeWrite));
            script.push((verb, Hx07Window::BeforeEnter));
            script.push((verb, Hx07Window::Hushed));
            script.extend(std::iter::repeat_n((verb, Hx07Window::Free), 3));
        }
    }
    script
}

/// The real pipe reader, with a checkpoint a fault test can close independently of cancellation.
#[cfg(unix)]
fn hx07_read(
    mut program: std::fs::File,
    drained: &std::sync::atomic::AtomicBool,
    mut next: impl FnMut() -> bool,
    mut process: impl FnMut(&[u8]),
) -> Vec<u8> {
    use std::io::Read;
    use std::sync::atomic::Ordering;

    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while next() {
        let stopping = drained.load(Ordering::Acquire);
        match program.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut echo = Vec::new();
                for &b in &buf[..n] {
                    match b {
                        b'\r' => echo.extend_from_slice(b"\r\nresponse\r\n"),
                        b if b.is_ascii_lowercase() => {}
                        b => echo.push(b),
                    }
                }
                process(&echo);
                out.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if stopping {
                    break;
                }
                aterm_uds::deadline::poll_yield();
            }
            Err(e) => panic!("read the program's input: {e}"),
        }
    }
    out
}

/// The real person worker; a closed fault-test checkpoint retires it even with a broken guard.
#[cfg(unix)]
fn hx07_type_person(
    done: &std::sync::atomic::AtomicBool,
    hushed: &Mutex<bool>,
    main: std::sync::mpsc::Sender<Hx07Main>,
    mut next: impl FnMut() -> bool,
) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const PAUSES_MS: [u64; 7] = [2, 5, 20, 80, 160, 240, 320];
    let mut letters = (b'a'..=b'y').cycle();
    for burst in 0usize.. {
        for _ in 0..=burst % 8 {
            if !next() || done.load(Ordering::Acquire) {
                return;
            }
            {
                let hush = hushed.lock().unwrap_or_else(PoisonError::into_inner);
                if !*hush {
                    let c = letters.next().expect("cycles");
                    if main.send(Hx07Main::Person(c, None)).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(PAUSES_MS[burst % PAUSES_MS.len()]));
    }
}

/// One HX-07 run on a headless window whose session's program reads a pipe: claude-code-alab's
/// proven claim of every duty stands (a scope fixture, as wide as its roster row); a person
/// types `a`-`y` through the window's own input path (`as=person`'s route,
/// [`crate::control::control_input::person_input`]) while the claimant,
/// on its own connection, types [`hx07_script`]'s `send`, `key` and `turn` with
/// `if-supervisor=`, `if-supervisor-revision=` and `harness-duty=turn-end`, and, where `fenced`,
/// `if-human=` the `human_seq` it read just before. Each write takes the route the dispatch
/// gives it: a fenced `send`/`key` the fenced arm under the terminal lock
/// ([`super::control_input::cmd_send_fenced_observed`]), as does the Enter of a turn under
/// `if-human=` or `submit=guarded:`; an unfenced one and every other write of a turn go by the
/// seam, delivered on the main thread as `Wake::Input` delivers it (the scoped fence checked
/// again, then [`crate::App::input_to_session_observed`]). The program echoes what the harness
/// types.
#[cfg(unix)]
fn hx07_run(fenced: bool) -> Hx07Run {
    use super::control_input::{InputFence, cmd_key_fenced_observed, cmd_send_fenced_observed};
    use super::control_session::{TurnIo, cmd_turn, turn_paste_event};
    use crate::control_input_ack::InputMark;
    use crate::input::{InputEvent, InputOutcome, Source};
    use crate::session_timeline::{
        SupervisorInputCaller, SupervisorInputDelivery, SupervisorInputFence,
    };
    use aterm_agent::harness::contract::Scope;
    use std::os::fd::{AsRawFd, IntoRawFd};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;
    const PATIENCE: Duration = Duration::from_secs(60);
    const CONN: u64 = 74;

    let (rd, wr) = aterm_uds::pipe().expect("pipe");
    aterm_pty::set_nonblocking(rd.as_raw_fd(), true).expect("nonblocking program end");
    let program = std::fs::File::from(std::os::fd::OwnedFd::from(rd));
    let wr = wr.into_raw_fd();
    aterm_pty::set_nonblocking(wr, true).expect("nonblocking master");
    let mut app =
        crate::App::headless_for_test_with_sink(Arc::new(aterm_session::sink::SinkWriter::new(wr)));
    let (term, ctx) = {
        let session = app.pool.get(0).expect("session 0");
        (Arc::clone(&session.term), Arc::clone(&session.ctx))
    };
    ctx.sink.note_master_nonblocking(true);
    let store = app.store.clone();
    let holder = format!("claude-code-alab@{}", stand_in());
    claim_scoped(&ctx, &holder, &Scope::ALL, CONN);
    let revision = ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .supervisor_authority(crate::metrics::now_us())
        .revision();
    let caller = SupervisorInputCaller {
        principal: super::Principal::Instance,
        connection: Some(CONN),
        peer: Some(stand_in()),
        proof: Some(running()),
    };

    let (main_tx, main_rx) = mpsc::channel::<Hx07Main>();
    let claimant_done = AtomicBool::new(false);
    let hushed = Mutex::new(false);
    let drained = AtomicBool::new(false);
    let mut person: Vec<(u8, u64)> = Vec::new();
    let (writes, out) = std::thread::scope(|s| {
        // This receiver and its pending replies must drop before the scope joins on a panic.
        let owner = Hx07Owner::new(main_rx, &claimant_done, &drained);
        // The program: it echoes what the harness types (an Enter draws a response) and not the
        // person's letters, so the screen is quiet whenever the harness is.
        let reader = s.spawn(|| {
            hx07_read(
                program,
                &drained,
                || true,
                |echo| {
                    term.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .process(echo);
                },
            )
        });
        // The person: bursts of one to eight keys a millisecond apart, and pauses from 2 ms to
        // a third of a second between them, so a key falls anywhere in a write; held off while
        // `hushed` holds, checked and handed under its lock so no key follows a hush.
        let typist = {
            let main = main_tx.clone();
            let (done, hushed) = (&claimant_done, &hushed);
            s.spawn(move || hx07_type_person(done, hushed, main, || true))
        };
        let claimant = {
            let main = main_tx.clone();
            let (ctx, term, store, holder, caller) = (&ctx, &term, &store, &holder, &caller);
            let (done, hushed) = (&claimant_done, &hushed);
            s.spawn(move || {
                let _done = Hx07Done(done);
                // Hold the person off (`on`) or let them go on; a hush takes every key handed
                // before it first.
                let hush = |on: bool| {
                    *hushed.lock().unwrap_or_else(PoisonError::into_inner) = on;
                    if on {
                        let (ack, acked) = mpsc::channel();
                        main.send(Hx07Main::Sync(ack))
                            .expect("the main thread syncs");
                        acked
                            .recv_timeout(PATIENCE)
                            .expect("hang: the main thread never synced");
                    }
                };
                let force = || hx07_force(&main);
                let post = |ev: InputEvent, mark: InputMark| -> Result<InputOutcome, String> {
                    let fence = super::carried_fence();
                    let mark = mark
                        .supervised(fence.as_ref().and_then(SupervisorInputDelivery::batch))
                        .pending_supervisor();
                    if !mark.supervisor_queue_ready() {
                        return Err(crate::session_timeline::SCOPED_INPUT_DENIED.to_string());
                    }
                    let (reply, replied) = mpsc::channel();
                    main.send(Hx07Main::Control(Box::new(Hx07Control {
                        ev,
                        mark,
                        fence,
                        reply,
                    })))
                    .map_err(|e| e.to_string())?;
                    Ok(replied
                        .recv_timeout(PATIENCE)
                        .expect("hang: the main thread never answered a control write"))
                };
                let mut writes = Vec::new();
                for (n, (verb, window)) in hx07_script().into_iter().enumerate() {
                    if done.load(Ordering::Acquire) {
                        break;
                    }
                    hush(matches!(
                        window,
                        Hx07Window::Hushed | Hx07Window::BeforeEnter
                    ));
                    let read = ctx.human_input.seq();
                    let text: Vec<u8> = match verb {
                        Hx07Verb::Send => format!("S{n:02}X").into_bytes(),
                        Hx07Verb::Key => vec![b"0123456789"[n % 10]],
                        Hx07Verb::Turn => format!("T{n:02}X").into_bytes(),
                        Hx07Verb::GuardedTurn => format!("G{n:02}X").into_bytes(),
                    };
                    let body = String::from_utf8(text.clone()).expect("ascii");
                    let human = if fenced {
                        format!(" if-human={read}")
                    } else {
                        String::new()
                    };
                    let word = match verb {
                        Hx07Verb::Send => "send",
                        Hx07Verb::Key => "key",
                        Hx07Verb::Turn | Hx07Verb::GuardedTurn => "turn",
                    };
                    let tail = match verb {
                        Hx07Verb::Turn => " idle=20 timeout=60000",
                        Hx07Verb::GuardedTurn => " idle=20 timeout=60000 submit=guarded:G[0-9]+X",
                        Hx07Verb::Send | Hx07Verb::Key => "",
                    };
                    let line = format!(
                        "if-supervisor={holder} if-supervisor-revision={revision} \
                         harness-duty=turn-end{human}{tail} {body}"
                    );
                    if window == Hx07Window::BeforeWrite {
                        force();
                    }
                    let (opts, rest) = super::control_input::take_leading_options(
                        aterm_types::control_verbs::input_of(word),
                        &line,
                    );
                    assert!(opts.refusal.is_none(), "{word} {line}: {:?}", opts.refusal);
                    let request = opts.supervisor_request().expect("the typed group");
                    let fence = SupervisorInputFence::authorize(
                        ctx,
                        request,
                        caller,
                        crate::metrics::now_us(),
                    )
                    .expect("claude-code-alab's typing claim admits the write");
                    let delivery = SupervisorInputDelivery::new(ctx, fence);
                    delivery
                        .check(ctx, crate::metrics::now_us())
                        .expect("the claim still stands");
                    let _carried = super::CarriedFence::enter(delivery.clone());
                    let _producer = delivery.producer();
                    let input_verb = match verb {
                        Hx07Verb::Send => crate::session_log::InputVerb::Send,
                        Hx07Verb::Key => crate::session_log::InputVerb::Key,
                        Hx07Verb::Turn | Hx07Verb::GuardedTurn => {
                            crate::session_log::InputVerb::Turn
                        }
                    };
                    let mark = InputMark::at_epoch(
                        ctx,
                        0,
                        aterm_slog::Actor::Ctl,
                        input_verb,
                        None,
                        matches!(verb, Hx07Verb::Turn | Hx07Verb::GuardedTurn),
                    )
                    .supervised(delivery.batch());
                    let reply = match verb {
                        Hx07Verb::Send | Hx07Verb::Key if opts.if_human.is_some() => {
                            let fence = InputFence {
                                human: opts.if_human,
                                ..InputFence::default()
                            };
                            if verb == Hx07Verb::Key {
                                cmd_key_fenced_observed(
                                    term,
                                    ctx,
                                    &fence,
                                    rest,
                                    Some(mark.completion()),
                                )
                            } else {
                                cmd_send_fenced_observed(
                                    term,
                                    ctx,
                                    &fence,
                                    rest,
                                    Some(mark.completion()),
                                )
                            }
                        }
                        Hx07Verb::Send => super::control_input::input_reply_to_str(post(
                            InputEvent::KeySequence(super::control_input::send_bytes(rest)),
                            mark.completion().into_mark(),
                        )),
                        Hx07Verb::Key => super::control_input::input_reply_to_str(post(
                            super::parse_key(rest).expect("a key"),
                            mark.completion().into_mark(),
                        )),
                        Hx07Verb::Turn | Hx07Verb::GuardedTurn => {
                            if window == Hx07Window::BeforeEnter {
                                let main = main.clone();
                                let forced = std::cell::Cell::new(false);
                                super::control_session::set_before_submit_for_test(Some(Box::new(
                                    move || {
                                        if !forced.replace(true) {
                                            hx07_force(&main);
                                        }
                                    },
                                )));
                            }
                            let refused = std::cell::RefCell::new(None);
                            let applied = |ev: InputEvent| {
                                let wrote = super::turn_write(ctx, Some(&delivery), || {
                                    post(ev, mark.completion().into_mark())
                                });
                                let delivered = wrote.is_ok();
                                *refused.borrow_mut() = wrote.err().flatten();
                                delivered
                            };
                            let paste = |text: &str| applied(turn_paste_event(text));
                            let press = |name: &str| super::parse_key(name).is_some_and(applied);
                            let momentum = || Ok((0.0, std::time::Instant::now()));
                            let reply = cmd_turn(
                                term,
                                store,
                                0,
                                rest,
                                ctx,
                                &TurnIo {
                                    paste: &paste,
                                    press: &press,
                                    key: &applied,
                                    momentum: &momentum,
                                    mark: Some(&mark),
                                    refused: &|| refused.take(),
                                    ..TurnIo::paste_only()
                                },
                            );
                            super::control_session::set_before_submit_for_test(None);
                            reply
                        }
                    };
                    writes.push(Hx07Write {
                        verb,
                        window,
                        read,
                        text,
                        reply,
                    });
                }
                writes
            })
        };
        drop(main_tx);
        let wid = crate::WindowId(0);
        loop {
            match owner.input.recv_timeout(PATIENCE) {
                Ok(Hx07Main::Person(c, ack)) => {
                    let ev = super::parse_key(&char::from(c).to_string()).expect("a letter");
                    let outcome = app.input_to_session_observed(
                        wid,
                        ev,
                        Source::Human,
                        Some(0),
                        crate::app_input::PressPhase::Initial,
                        None,
                    );
                    assert_eq!(
                        outcome,
                        InputOutcome::Ok,
                        "the person's key {}",
                        char::from(c)
                    );
                    person.push((c, ctx.human_input.seq()));
                    if let Some(ack) = ack {
                        let _ = ack.send(());
                    }
                }
                Ok(Hx07Main::Sync(ack)) => {
                    let _ = ack.send(());
                }
                Ok(Hx07Main::Control(control)) => {
                    let Hx07Control {
                        ev,
                        mark,
                        fence,
                        reply,
                    } = *control;
                    if fence
                        .as_ref()
                        .is_some_and(|fence| fence.check(&ctx, crate::metrics::now_us()).is_err())
                    {
                        let _ = reply.send(InputOutcome::WriteFailed);
                        continue;
                    }
                    let outcome = app.input_to_session_observed(
                        wid,
                        ev,
                        Source::Controller,
                        Some(0),
                        crate::app_input::PressPhase::Initial,
                        Some(mark.completion()),
                    );
                    let _ = reply.send(outcome);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("hang: no input for a minute"),
            }
        }
        typist.join().expect("the person");
        let writes = claimant.join().expect("the claimant");
        let since = std::time::Instant::now();
        while ctx.sink.ordered_egress_count() > 0 || ctx.sink.spilled_bytes() > 0 {
            assert!(
                since.elapsed() < PATIENCE,
                "hang: queued input never reached the program"
            );
            aterm_uds::deadline::poll_yield();
        }
        drop(owner);
        (writes, reader.join().expect("the reader"))
    });
    ctx.sink.sever_input();
    Hx07Run {
        out,
        person,
        writes,
    }
}

#[test]
#[cfg(unix)]
fn a_panicked_hx07_claimant_retires_the_real_typist() {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    const PATIENCE: Duration = Duration::from_secs(60);
    let done = AtomicBool::new(false);
    let hushed = Mutex::new(false);
    let (main, input) = mpsc::channel();
    let (at, paused) = mpsc::channel();
    let (go, step) = mpsc::channel();
    std::thread::scope(|s| {
        // Callback-owned: a failed assertion closes the checkpoint before automatic joins.
        let go = go;
        let (done, hushed) = (&done, &hushed);
        let typist = s.spawn(move || {
            hx07_type_person(done, hushed, main, || {
                at.send(()).expect("announce the next typing look");
                step.recv().is_ok()
            });
        });
        paused
            .recv_timeout(PATIENCE)
            .expect("typist at its first look");
        let claimant = s.spawn(move || {
            let _done = Hx07Done(done);
            panic!("HX-07 injected claimant panic");
        });
        assert!(claimant.join().is_err(), "the claimant must unwind");
        go.send(()).expect("release the typing look after unwind");
        match input.recv_timeout(PATIENCE) {
            Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => panic!("hang: the typist never retired"),
            Ok(_) => panic!("HX-07: the person kept typing after the claimant panicked"),
        }
        drop(go);
        typist.join().expect("the person retired");
    });
}

#[test]
#[cfg(unix)]
fn a_panicked_hx07_owner_retires_both_workers_and_queued_replies() {
    use std::os::fd::AsRawFd;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    #[derive(Debug, PartialEq, Eq)]
    enum WorkerStep {
        Look,
        End,
    }

    const PATIENCE: Duration = Duration::from_secs(60);
    let (rd, _wr) = aterm_uds::pipe().expect("pipe");
    aterm_pty::set_nonblocking(rd.as_raw_fd(), true).expect("nonblocking program end");
    let program = std::fs::File::from(std::os::fd::OwnedFd::from(rd));
    let done = AtomicBool::new(false);
    let drained = AtomicBool::new(false);
    let hushed = Mutex::new(true);
    let (main, input) = mpsc::channel();
    let (reply, replied) = mpsc::channel();
    main.send(Hx07Main::Sync(reply))
        .expect("queue an owned reply");
    let (events, seen) = mpsc::channel();
    let (go, step) = mpsc::channel();
    let (typing_events, typing_seen) = mpsc::channel();
    let (typing_go, typing_step) = mpsc::channel();
    std::thread::scope(|s| {
        // Independent of Hx07Done: a guard mutant cannot strand either worker on a red.
        let go = go;
        let typing_go = typing_go;
        let drained_ref = &drained;
        let reader = s.spawn(move || {
            hx07_read(
                program,
                drained_ref,
                || {
                    events
                        .send(WorkerStep::Look)
                        .expect("announce the next read");
                    step.recv().is_ok()
                },
                |_| panic!("the held-open pipe is empty"),
            );
            events
                .send(WorkerStep::End)
                .expect("announce reader retirement");
        });
        let (done_ref, hushed_ref) = (&done, &hushed);
        let typist = s.spawn(move || {
            hx07_type_person(done_ref, hushed_ref, main, || {
                typing_events
                    .send(WorkerStep::Look)
                    .expect("announce the next typing look");
                typing_step.recv().is_ok()
            });
            typing_events
                .send(WorkerStep::End)
                .expect("announce typist retirement");
        });
        assert_eq!(
            seen.recv_timeout(PATIENCE)
                .expect("reader at its first look"),
            WorkerStep::Look
        );
        assert_eq!(
            typing_seen
                .recv_timeout(PATIENCE)
                .expect("hushed typist at its first look"),
            WorkerStep::Look
        );
        let panicked = catch_unwind(AssertUnwindSafe(|| {
            let _owner = Hx07Owner::new(input, &done, &drained);
            panic!("HX-07 injected owner panic");
        }));
        assert!(panicked.is_err(), "the owner must unwind");
        match replied.recv_timeout(PATIENCE) {
            Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => panic!("hang: the owner retained its queued reply"),
            Ok(()) => panic!("no main thread answered the queued reply"),
        }
        go.send(()).expect("release the read after owner unwind");
        typing_go
            .send(())
            .expect("release the typing look after owner unwind");
        assert_eq!(
            seen.recv_timeout(PATIENCE).expect("reader after unwind"),
            WorkerStep::End
        );
        assert_eq!(
            typing_seen
                .recv_timeout(PATIENCE)
                .expect("hushed typist after unwind"),
            WorkerStep::End
        );
        drop(go);
        drop(typing_go);
        reader.join().expect("the reader retired");
        typist.join().expect("the hushed person retired");
    });
}

/// A person's key, forced: handed to the main thread, which acknowledges it once it is written.
#[cfg(unix)]
fn hx07_force(main: &std::sync::mpsc::Sender<Hx07Main>) {
    let (ack, acked) = std::sync::mpsc::channel();
    main.send(Hx07Main::Person(b'z', Some(ack)))
        .expect("the main thread takes the key");
    acked
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("hang: the person's forced key was never written");
}

/// HX-07's detector over one run: every way the run broke the law that the person's keys arrive
/// whole and in order and that each of the claimant's writes either lands whole, behind every
/// key the person typed before its read and ahead of every key typed after, or answers the
/// person's skip having written nothing it should not. A turn cut after its text names the
/// person and leaves that text, its Enter unpressed.
#[cfg(unix)]
fn hx07_breaks(run: &Hx07Run) -> Vec<String> {
    let mut breaks = Vec::new();
    let typed: Vec<u8> = run
        .out
        .iter()
        .copied()
        .filter(u8::is_ascii_lowercase)
        .collect();
    let injected: Vec<u8> = run.person.iter().map(|(c, _)| *c).collect();
    if typed != injected {
        let at = typed
            .iter()
            .zip(&injected)
            .position(|(a, b)| a != b)
            .unwrap_or(typed.len().min(injected.len()));
        breaks.push(format!(
            "the person's keys: {} typed, {} reached the program, first apart at {at}",
            injected.len(),
            typed.len()
        ));
    }
    // Every byte that is not the person's, with its place in what the program read.
    let harness: Vec<(usize, u8)> = run
        .out
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, b)| !b.is_ascii_lowercase())
        .collect();
    let persons_before = |at: usize| {
        run.out[..at]
            .iter()
            .filter(|b| b.is_ascii_lowercase())
            .count()
    };
    let mut h = 0;
    for (n, w) in run.writes.iter().enumerate() {
        let landed = w.landed();
        let got: Vec<u8> = harness
            .iter()
            .skip(h)
            .take(landed.len())
            .map(|(_, b)| *b)
            .collect();
        if got != landed {
            breaks.push(format!(
                "write {n} ({:?} {:?}, read {}) answered {:?}: {:?} should have reached the \
                 program next, {:?} did",
                w.verb,
                w.window,
                w.read,
                w.reply.lines().next().unwrap_or_default(),
                String::from_utf8_lossy(&landed),
                String::from_utf8_lossy(&got)
            ));
            break;
        }
        if landed.is_empty() {
            continue;
        }
        let first = harness[h].0;
        let last = harness[h + landed.len() - 1].0;
        h += landed.len();
        if w.went() {
            let ahead = persons_before(last + 1);
            if let Some(&(c, seq)) = ahead.checked_sub(1).and_then(|k| run.person.get(k))
                && seq > w.read
            {
                breaks.push(format!(
                    "write {n} ({:?} {:?}) read human_seq={} and went whole ({:?}) behind the \
                     person's {:?}, typed at human_seq={seq}",
                    w.verb,
                    w.window,
                    w.read,
                    w.reply.lines().next().unwrap_or_default(),
                    char::from(c)
                ));
            }
        }
        let seen = run.person.iter().filter(|(_, seq)| *seq <= w.read).count();
        if persons_before(first) < seen {
            breaks.push(format!(
                "write {n} ({:?} {:?}) overtook a key the person typed before its read \
                 (human_seq={})",
                w.verb, w.window, w.read
            ));
        }
        if matches!(w.verb, Hx07Verb::Send | Hx07Verb::Key)
            && run.out[first..=last].iter().any(u8::is_ascii_lowercase)
        {
            breaks.push(format!(
                "write {n} ({:?}) was torn by a person's key",
                w.verb
            ));
        }
    }
    if h != harness.len() && breaks.is_empty() {
        breaks.push(format!(
            "{} bytes reached the program that no write answered for: {:?}",
            harness.len() - h,
            String::from_utf8_lossy(&harness[h..].iter().map(|(_, b)| *b).collect::<Vec<_>>())
        ));
    }
    breaks
}

/// The run's replies, counted per verb and window, for a failure message.
#[cfg(unix)]
fn hx07_tally(run: &Hx07Run) -> String {
    let mut tally = std::collections::BTreeMap::<String, usize>::new();
    for w in &run.writes {
        let first = w.reply.lines().next().unwrap_or_default();
        let word = if w.went() {
            "went"
        } else if w.yielded() {
            "person"
        } else {
            first
        };
        *tally
            .entry(format!("{:?}/{:?}: {word}", w.verb, w.window))
            .or_default() += 1;
    }
    format!("{} person keys; {tally:?}", run.person.len())
}

/// HX-07 (`docs/DESIGN-harness-interface-2026-11-12.md` §5): A TAB'S OWN PROCESS TYPES UNDER
/// `if-human=` WHILE A PERSON TYPES, AND NONE OF THE PERSON'S KEYS IS LOST OR REORDERED.
/// claude-code-alab's proven claim of every duty stands on a headless window's session (a scope
/// fixture, as wide as its roster row); a person types through the window's own input path, in
/// bursts, with hands off, and once forced into each of the claimant's windows (between its read
/// of `human_seq` and its write; inside a turn, after every look it takes before its Enter and
/// before that Enter is written), while the claimant types `send`, `key`, `turn` and a
/// `submit=guarded:` turn with the typed group and `if-human=` ([`hx07_run`]). The detector
/// ([`hx07_breaks`]) holds the person's keys to arriving whole and in order and each write to
/// landing whole behind the keys before its read and ahead of those after, or to answering the
/// person's skip. A turn's Enter, default or guarded, is written under the terminal lock that
/// compares `human_seq` (`fenced_submit`), so a key forced before it is the person's skip, the
/// text left unsubmitted. NEGATIVE CONTROL: the same run without `if-human=` breaks that law for
/// each verb, so the detector sees the interleaving the fence is there to prevent.
#[test]
#[cfg(unix)]
fn a_tabs_own_process_types_under_if_human_while_keys_are_injected_and_none_is_lost_or_reordered() {
    let control = hx07_run(false);
    let control_breaks = hx07_breaks(&control);
    for verb in [
        Hx07Verb::Send,
        Hx07Verb::Key,
        Hx07Verb::Turn,
        Hx07Verb::GuardedTurn,
    ] {
        assert!(
            control_breaks
                .iter()
                .any(|b| b.contains(&format!("({verb:?} "))),
            "NEGATIVE CONTROL: without if-human= a {verb:?} lands behind a newer key: {} {:#?}",
            hx07_tally(&control),
            control_breaks
        );
    }
    let fenced = hx07_run(true);
    let breaks = hx07_breaks(&fenced);
    assert!(
        breaks.is_empty(),
        "HX-07: {} {:#?}",
        hx07_tally(&fenced),
        breaks
    );
    for verb in [
        Hx07Verb::Send,
        Hx07Verb::Key,
        Hx07Verb::Turn,
        Hx07Verb::GuardedTurn,
    ] {
        let forced: Vec<&Hx07Write> = fenced
            .writes
            .iter()
            .filter(|w| {
                w.verb == verb
                    && matches!(w.window, Hx07Window::BeforeWrite | Hx07Window::BeforeEnter)
            })
            .collect();
        assert!(
            forced.iter().all(|w| w.yielded()),
            "a {verb:?} after the person's key answers the person's skip: {forced:#?}"
        );
        assert!(
            fenced.writes.iter().any(|w| w.verb == verb && w.went()),
            "some {verb:?} went whole: {}",
            hx07_tally(&fenced)
        );
    }
}

// A claimant reads the person's new sequence while the gesture is still being routed. The
// program must receive that gesture first even when the ordered writer is currently empty.
#[test]
#[cfg(unix)]
fn a_fenced_key_waits_for_a_counted_person_gesture_to_reach_the_writer() {
    use super::control_input::{GuardedInput, InputFence, input_if_fenced_observed};
    use crate::ReadOn;
    use crate::input::InputEvent;
    use std::io::Read;
    use std::os::fd::{AsRawFd, IntoRawFd};

    let (rd, wr) = aterm_uds::pipe().expect("pipe");
    aterm_pty::set_nonblocking(rd.as_raw_fd(), true).expect("nonblocking reader");
    let mut program = std::fs::File::from(std::os::fd::OwnedFd::from(rd));
    aterm_pty::set_nonblocking(wr.as_raw_fd(), true).expect("nonblocking writer");
    let mut app = crate::App::headless_for_test_with_sink(Arc::new(
        aterm_session::sink::SinkWriter::new(wr.into_raw_fd()),
    ));
    let session = app.pool.get(0).expect("session");
    let term = session.term.clone();
    let ctx = session.ctx.clone();
    ctx.sink.note_master_nonblocking(true);
    let routing = ctx.human_input.begin_delivery();
    let person = InputEvent::KeySequence(b"p".to_vec());
    app.note_person_input(0, &person, crate::input::Source::Human);
    let fence = InputFence {
        human: Some(ctx.human_input.seq()),
        ..InputFence::default()
    };
    let press = || {
        input_if_fenced_observed(
            &term,
            &ctx,
            InputEvent::KeySequence(b"C".to_vec()),
            &fence,
            ReadOn::Control,
            None,
        )
    };
    assert_eq!(ctx.sink.ordered_egress_count(), 0);
    assert!(matches!(press(), GuardedInput::Queued));
    // The same route queues the person's bytes before its dispatch guard leaves scope.
    assert_eq!(
        app.input_to_session(
            crate::WindowId(0),
            person,
            crate::input::Source::Controller,
            Some(0),
            crate::app_input::PressPhase::Initial,
        ),
        crate::input::InputOutcome::Ok
    );
    drop(routing);
    assert!(matches!(
        press(),
        GuardedInput::Pressed(crate::input::Delivery::Full)
    ));
    let mut bytes = [0; 2];
    let mut read = 0;
    aterm_spec::wait::until_while_working(std::process::id(), "both ordered writes", || {
        match program.read(&mut bytes[read..]) {
            Ok(n) => read += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("read ordered input: {error}"),
        }
        (read == bytes.len()).then_some(())
    });
    assert_eq!(&bytes, b"pC");
    ctx.sink.sever_input();
}
