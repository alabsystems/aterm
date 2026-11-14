// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The typed input boundary, including queued versus already-writing batches.

use super::*;
use crate::control::Principal;
use aterm_agent::harness::contract::{self, Scope, Scopes};
use std::sync::{Arc, Barrier, mpsc};
use std::time::{Duration, Instant};

const PATIENCE: Duration = Duration::from_secs(60);
const CONN: u64 = 71;

pub(crate) fn scoped(duties: &[Scope]) -> crate::harness_claim::Scoped {
    let contract::Words::Scoped(mut claim) =
        contract::read("grok-alab@4242", &["contract=3", "scope=upgrade"])
    else {
        panic!("rostered fixture");
    };
    // Deliberately wider than today's roster: this exercises the boundary without making
    // any new production claim admissible.
    claim.scopes = Scopes::for_test(duties);
    crate::harness_claim::Scoped {
        proven: crate::self_claim::Proven {
            pid: 4242,
            start: "fixture-birth".to_string(),
        },
        claim,
    }
}

pub(crate) fn install(ctx: &crate::SessionCtx, conn: u64, duties: &[Scope]) -> Claimed {
    claim_supervisor_scoped(
        ctx,
        "grok-alab@4242",
        Some(conn),
        scoped(duties),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("fixture claim")
}

fn revision(ctx: &crate::SessionCtx) -> u64 {
    ctx.meta
        .lock()
        .unwrap()
        .supervisor_authority(crate::metrics::now_us())
        .revision()
}

fn caller(builtin: bool, conn: u64) -> SupervisorInputCaller {
    SupervisorInputCaller {
        principal: if builtin {
            Principal::Harness
        } else {
            Principal::Instance
        },
        connection: Some(conn),
        peer: (!builtin).then_some(4242),
        proof: (!builtin).then(|| scoped(&[Scope::Upgrade]).proven),
    }
}

fn request(ctx: &crate::SessionCtx, builtin: bool, duty: Scope) -> SupervisorInputRequest {
    SupervisorInputRequest {
        holder: if builtin {
            crate::harness_host::holder()
        } else {
            "grok-alab@4242".to_string()
        },
        revision: revision(ctx),
        duty,
    }
}

pub(crate) fn ticket(ctx: &crate::SessionCtx, builtin: bool, duty: Scope) -> SupervisorInputFence {
    SupervisorInputFence::authorize(
        ctx,
        request(ctx, builtin, duty),
        &caller(builtin, CONN),
        crate::metrics::now_us(),
    )
    .expect("typed fixture admission")
}

fn await_revision(ctx: &crate::SessionCtx, old: u64) {
    let since = Instant::now();
    while revision(ctx) == old {
        assert!(
            since.elapsed() < PATIENCE,
            "hang: metadata transition never became visible"
        );
        aterm_uds::deadline::poll_yield();
    }
}

#[test]
fn scoped_input_binds_revision_identity_connection_and_explicit_duty() {
    let h = crate::session_store::test_handle_on(7611, -1);
    let ctx = &h.ctx;
    install(ctx, CONN, &[Scope::Upgrade]);
    for duty in [
        Scope::TurnEnd,
        Scope::Approvals,
        Scope::Questions,
        Scope::Walls,
    ] {
        assert!(
            SupervisorInputFence::authorize(
                ctx,
                request(ctx, false, duty),
                &caller(false, CONN),
                crate::metrics::now_us()
            )
            .is_err()
        );
        assert!(
            ticket(ctx, true, duty)
                .check(ctx, crate::metrics::now_us())
                .is_ok()
        );
    }
    assert!(
        SupervisorInputFence::authorize(
            ctx,
            request(ctx, true, Scope::Upgrade),
            &caller(true, CONN),
            crate::metrics::now_us()
        )
        .is_err()
    );
    // Even an owned non-typing scope cannot be converted into arbitrary PTY input.
    assert!(
        SupervisorInputFence::authorize(
            ctx,
            request(ctx, false, Scope::Upgrade),
            &caller(false, CONN),
            crate::metrics::now_us()
        )
        .is_err()
    );
    install(ctx, CONN, &[Scope::Approvals]);
    for duty in [Scope::Upgrade, Scope::Relaunch] {
        assert!(
            SupervisorInputFence::authorize(
                ctx,
                request(ctx, true, duty),
                &caller(true, CONN),
                crate::metrics::now_us()
            )
            .is_err(),
            "retained non-typing builtin scope grants no PTY authority: {duty:?}"
        );
    }
    let good = request(ctx, false, Scope::Approvals);
    assert!(
        SupervisorInputFence::authorize(
            ctx,
            good.clone(),
            &caller(false, CONN),
            crate::metrics::now_us()
        )
        .is_ok()
    );
    assert!(
        SupervisorInputFence::authorize(
            ctx,
            request(ctx, true, Scope::Approvals),
            &caller(true, CONN),
            crate::metrics::now_us()
        )
        .is_err()
    );
    let mut bad_revision = good.clone();
    bad_revision.revision -= 1;
    let mut bad_holder = good.clone();
    bad_holder.holder = crate::harness_host::holder();
    for bad in [bad_revision, bad_holder] {
        assert!(
            SupervisorInputFence::authorize(
                ctx,
                bad,
                &caller(false, CONN),
                crate::metrics::now_us()
            )
            .is_err()
        );
    }
    let mut wrong_birth = caller(false, CONN);
    wrong_birth.proof.as_mut().unwrap().start.push('x');
    let mut no_connection = caller(false, CONN);
    no_connection.connection = None;
    let mut no_proof = caller(false, CONN);
    no_proof.proof = None;
    let mut wrong_peer = caller(false, CONN);
    wrong_peer.peer = Some(4243);
    let mut no_peer = caller(false, CONN);
    no_peer.peer = None;
    let mut impersonated_builtin = caller(false, CONN);
    impersonated_builtin.principal = Principal::Harness;
    for bad in [
        wrong_birth,
        no_connection,
        no_proof,
        wrong_peer,
        no_peer,
        impersonated_builtin,
        caller(false, CONN + 1),
    ] {
        assert!(
            SupervisorInputFence::authorize(ctx, good.clone(), &bad, crate::metrics::now_us())
                .is_err(),
            "{bad:?}"
        );
    }
    let other = crate::session_store::test_handle_on(7612, -1);
    install(&other.ctx, CONN, &[Scope::Approvals]);
    assert!(
        ticket(ctx, false, Scope::Approvals)
            .check(&other.ctx, crate::metrics::now_us())
            .is_err()
    );
}

#[test]
fn recognized_claimants_cannot_omit_the_typed_group_but_independent_operators_keep_authority() {
    let h = crate::session_store::test_handle_on(7613, -1);
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    install(&h.ctx, CONN + 1, &[Scope::Upgrade]);
    let meta = h.ctx.meta.lock().unwrap();
    let now = crate::metrics::now_us();
    let peer = scoped(&[Scope::Upgrade]).proven;
    let other_peer = crate::self_claim::Proven {
        pid: 4243,
        ..peer.clone()
    };
    assert!(meta.scoped_input_required(Principal::Harness, Some(999), None, now));
    assert!(
        meta.scoped_input_required(Principal::Instance, Some(CONN), None, now),
        "historical bound connection"
    );
    assert!(
        meta.scoped_input_required(Principal::Instance, Some(CONN + 1), None, now),
        "current connection"
    );
    assert!(
        meta.scoped_input_required(Principal::Instance, Some(999), Some((&peer).into()), now),
        "same kernel peer after reconnect"
    );
    assert!(
        !meta.scoped_input_required(
            Principal::Instance,
            Some(999),
            Some((&other_peer).into()),
            now
        ),
        "independent operator/drive credentials"
    );
    assert!(
        !meta.scoped_input_required(Principal::Instance, None, None, now),
        "bridge/fleet keep their existing credential authority"
    );
    drop(meta);
    assert!(
        FencedWrite::enter(&h.ctx, "grok-alab@4242", now).is_err(),
        "a holder word cannot recover v2 full scope"
    );
}

#[test]
fn scoped_history_survives_release_expiry_and_ordinary_relabel_without_restricting_v2_only_peers() {
    for expires in [false, true] {
        let h = crate::session_store::test_handle_on(7621 + u64::from(expires), -1);
        install(&h.ctx, CONN, &[Scope::Upgrade]);
        let now = crate::metrics::now_us();
        if expires {
            h.ctx
                .meta
                .lock()
                .unwrap()
                .supervisor
                .as_mut()
                .unwrap()
                .expires_us = Some(now);
            assert!(h.ctx.meta.lock().unwrap().live_scoped(now).is_none());
        } else {
            assert!(release_supervisor_held_by(
                &h.ctx,
                "grok-alab@4242",
                now,
                aterm_slog::Actor::Ctl
            ));
        }
        let peer = scoped(&[Scope::Upgrade]).proven;
        let reused_pid = crate::self_claim::Proven {
            start: "a-different-birth".to_string(),
            ..peer.clone()
        };
        {
            let meta = h.ctx.meta.lock().unwrap();
            assert!(meta.scoped_input_required(Principal::Instance, Some(CONN), None, now));
            assert!(meta.scoped_input_required(Principal::Harness, Some(999), None, now));
            assert!(
                meta.scoped_input_required(
                    Principal::Instance,
                    Some(999),
                    Some(SupervisorInputPeer {
                        pid: 4242,
                        start: None
                    }),
                    now
                ),
                "release and expiry preserve unknown-birth refusal"
            );
            assert!(meta.scoped_input_required(
                Principal::Instance,
                Some(999),
                Some((&peer).into()),
                now
            ));
            assert!(!meta.scoped_input_required(
                Principal::Instance,
                Some(999),
                Some((&reused_pid).into()),
                now
            ));
            assert!(!meta.scoped_input_required(Principal::Instance, Some(999), None, now));
        }
        if expires {
            assert!(lapse_supervisor(&h.ctx, now));
        }
        assert!(
            claim_supervisor_as(
                &h.ctx,
                "ordinary-owner",
                Some(CONN),
                None,
                None,
                now,
                aterm_slog::Actor::Ctl
            )
            .is_ok()
        );
        let mut meta = h.ctx.meta.lock().unwrap();
        assert!(
            meta.claimant_on(Some(CONN)).is_none(),
            "ordinary relabel erases only the old wait attribution"
        );
        assert!(meta.scoped_input_required(Principal::Instance, Some(CONN), None, now));
        meta.close_connection(CONN, now);
        assert!(!meta.scoped_input_required(Principal::Instance, Some(CONN), None, now));
        assert!(
            meta.scoped_input_required(Principal::Instance, Some(999), Some((&peer).into()), now),
            "reconnect still needs typed authority, even after old socket EOF"
        );
    }
    let h = crate::session_store::test_handle_on(7623, -1);
    let proof = scoped(&[Scope::Upgrade]).proven;
    assert!(
        claim_supervisor_as(
            &h.ctx,
            "codex-alab@4242",
            Some(CONN),
            None,
            Some(proof.clone()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl
        )
        .is_ok()
    );
    let now = crate::metrics::now_us();
    assert!(release_supervisor_held_by(
        &h.ctx,
        "codex-alab@4242",
        now,
        aterm_slog::Actor::Ctl
    ));
    let meta = h.ctx.meta.lock().unwrap();
    assert!(meta.claimant_on(Some(CONN)).is_some());
    assert!(
        !meta.scoped_input_required(Principal::Instance, Some(CONN), Some((&proof).into()), now),
        "v2-only history retains its existing operator authority"
    );
    assert!(
        !meta.scoped_input_required(Principal::Harness, Some(999), None, now),
        "never-scoped builtin legacy client is unchanged"
    );
}

#[test]
fn queued_batches_revalidate_once_and_guarded_admission_never_parks_under_meta() {
    let h = crate::session_store::test_handle_on(7614, -1);
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
    let batch = delivery.batch().unwrap();
    let meta = h.ctx.meta.lock().unwrap();
    assert!(
        !batch.start_inline(),
        "the guarded seam cannot wait on its own meta lock"
    );
    drop(meta);
    assert!(
        !batch.start(),
        "a refused first delivery cannot later spring back to life"
    );
    let queued = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
    install(&h.ctx, CONN, &[Scope::Approvals]);
    assert!(queued.check(&h.ctx, crate::metrics::now_us()).is_err());
    assert!(
        !queued.batch().unwrap().start(),
        "stale GUI/writer queues write nothing"
    );
}

#[test]
fn unreadable_birth_cannot_turn_a_known_scoped_peer_into_an_independent_operator() {
    let h = crate::session_store::test_handle_on(7636, -1);
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    for stage in 0..3 {
        let now = crate::metrics::now_us();
        if stage == 1 {
            assert!(release_supervisor_held_by(
                &h.ctx,
                "grok-alab@4242",
                now,
                aterm_slog::Actor::Ctl
            ));
        }
        if stage == 2 {
            h.ctx.meta.lock().unwrap().close_connection(CONN, now);
        }
        let meta = h.ctx.meta.lock().unwrap();
        assert!(
            meta.scoped_input_required(
                Principal::Instance,
                Some(999),
                Some(SupervisorInputPeer {
                    pid: 4242,
                    start: None
                }),
                now
            ),
            "cached kernel peer PID with an unreadable/exited birth stays typed at stage {stage}"
        );
        assert!(
            !meta.scoped_input_required(
                Principal::Instance,
                Some(999),
                Some(SupervisorInputPeer {
                    pid: 4242,
                    start: Some("a-new-birth")
                }),
                now
            ),
            "a positively observed different birth keeps independent operator authority"
        );
        assert!(
            !meta.scoped_input_required(
                Principal::Instance,
                Some(999),
                Some(SupervisorInputPeer {
                    pid: 4243,
                    start: None
                }),
                now
            ),
            "an unrelated PID is not inferred to be the scoped actor"
        );
    }
    let mut missing = caller(false, 999);
    missing.proof = None;
    assert!(
        SupervisorInputFence::authorize(
            &h.ctx,
            request(&h.ctx, false, Scope::Approvals),
            &missing,
            crate::metrics::now_us()
        )
        .is_err(),
        "requiring typed authority never supplies the missing proof"
    );
    let v2 = crate::session_store::test_handle_on(7637, -1);
    claim_supervisor_as(
        &v2.ctx,
        "codex-alab@4242",
        Some(CONN),
        None,
        Some(scoped(&[Scope::Upgrade]).proven),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    assert!(
        !v2.ctx.meta.lock().unwrap().scoped_input_required(
            Principal::Instance,
            Some(999),
            Some(SupervisorInputPeer {
                pid: 4242,
                start: None
            }),
            crate::metrics::now_us()
        ),
        "v2-only history is not a scoped restriction"
    );
}

#[test]
fn guarded_and_ordered_batches_cannot_extend_each_others_kernel_spill_drain() {
    let h = crate::session_store::test_handle_on(7619, -1);
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    let inline = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
    assert!(inline.batch().unwrap().start_inline());
    let different_duty = ticket(&h.ctx, true, Scope::Questions);
    assert_eq!(
        FencedWrite::enter_fence(&h.ctx, &different_duty, crate::metrics::now_us())
            .err()
            .as_deref(),
        Some(SCOPED_INPUT_BUSY)
    );
    drop(inline);
    let ordered =
        FencedWrite::enter_fence(&h.ctx, &different_duty, crate::metrics::now_us()).unwrap();
    let inline = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
    assert!(
        !inline.batch().unwrap().start_inline(),
        "a separate immediate writer cannot append beside an ordered drain"
    );
    drop(ordered);
}

/// A renewal in place supersedes nothing: the claimant's write admitted under the revision it keeps
/// goes on with its grant intact, and its ACK owes no drain, since one of the revision it keeps
/// would wait on that write and on every later one that joins it. The same claim renewed on
/// another connection moves the binding: the grant ends and its ACK waits for the write.
#[test]
fn a_renewal_in_place_keeps_admitted_input_and_acknowledges_without_a_drain() {
    let h = crate::session_store::test_handle_on(7641, -1);
    let ctx = Arc::clone(&h.ctx);
    install(&ctx, CONN, &[Scope::Approvals]);
    let kept = revision(&ctx);
    let grant = SupervisorInputFence::authorize(
        &ctx,
        request(&ctx, false, Scope::Approvals),
        &caller(false, CONN),
        crate::metrics::now_us(),
    )
    .unwrap();
    let renew = |conn: u64| {
        publish_claim_supervisor_scoped(
            &ctx,
            "grok-alab@4242",
            Some(conn),
            scoped(&[Scope::Approvals]),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .unwrap()
    };
    let acknowledged = |transition: SupervisorTransition<Claimed>| {
        let (tx, rx) = mpsc::channel();
        let barrier = Arc::new(Barrier::new(2));
        let at = Arc::clone(&barrier);
        let ack = std::thread::spawn(move || {
            at.wait();
            tx.send(transition.acknowledge().revision).unwrap();
        });
        barrier.wait();
        (rx, ack)
    };
    let writing = FencedWrite::enter_fence(&ctx, &grant, crate::metrics::now_us()).unwrap();
    let (ack, acker) = acknowledged(renew(CONN));
    assert_eq!(
        ack.recv_timeout(PATIENCE)
            .expect("hang: a renewal in place waited on the write its revision keeps"),
        kept
    );
    acker.join().unwrap();
    assert!(grant.check(&ctx, crate::metrics::now_us()).is_ok());
    drop(writing);

    let writing = FencedWrite::enter_fence(&ctx, &grant, crate::metrics::now_us()).unwrap();
    let (ack, acker) = acknowledged(renew(CONN + 1));
    assert!(grant.check(&ctx, crate::metrics::now_us()).is_err());
    assert!(
        ack.try_recv().is_err(),
        "NEGATIVE CONTROL: a moved claim's ACK waits for the superseded write"
    );
    drop(writing);
    assert_ne!(
        ack.recv_timeout(PATIENCE)
            .expect("hang: the moved claim's ACK outlived the superseded write"),
        kept
    );
    acker.join().unwrap();
}

#[test]
fn scope_renewal_drains_started_batches_and_does_not_wait_for_new_revision_work() {
    for rebind in [false, true] {
        let h = crate::session_store::test_handle_on(7615 + u64::from(rebind), -1);
        let ctx = Arc::clone(&h.ctx);
        install(
            &ctx,
            CONN,
            &[if rebind {
                Scope::Approvals
            } else {
                Scope::Upgrade
            }],
        );
        let old_revision = revision(&ctx);
        let delivery = SupervisorInputDelivery::new(&ctx, ticket(&ctx, !rebind, Scope::Approvals));
        let writing = delivery.batch().unwrap();
        assert!(writing.start());
        let stale = SupervisorInputDelivery::new(&ctx, ticket(&ctx, !rebind, Scope::Approvals));
        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = mpsc::channel();
        let transition = {
            let ctx = Arc::clone(&ctx);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let conn = CONN + u64::from(rebind);
                let claimed = install(&ctx, conn, &[Scope::Approvals]);
                tx.send(claimed.revision).unwrap();
            })
        };
        barrier.wait();
        await_revision(&ctx, old_revision);
        assert!(
            rx.try_recv().is_err(),
            "transition ACK must wait for actual writer custody"
        );
        assert!(
            !stale.batch().unwrap().start(),
            "queued old revision is not in flight"
        );
        assert!(delivery.check(&ctx, crate::metrics::now_us()).is_ok());
        assert!(
            writing.start(),
            "an admitted multi-write command finishes after scope loss"
        );
        let mut external = caller(false, CONN + u64::from(rebind));
        let current = SupervisorInputFence::authorize(
            &ctx,
            request(&ctx, false, Scope::Approvals),
            &external,
            crate::metrics::now_us(),
        )
        .unwrap();
        assert_eq!(
            FencedWrite::enter_fence(&ctx, &current, crate::metrics::now_us())
                .err()
                .as_deref(),
            Some(SCOPED_INPUT_BUSY),
            "new conflicting actor waits even though new metadata is visible"
        );
        let fresh = ticket(&ctx, true, Scope::Questions);
        let fresh_lease = FencedWrite::enter_fence(&ctx, &fresh, crate::metrics::now_us()).unwrap();
        drop(writing);
        drop(delivery);
        assert_eq!(
            rx.recv_timeout(PATIENCE)
                .expect("hang: new work starved old revision drain"),
            revision(&ctx)
        );
        transition.join().unwrap();
        assert!(FencedWrite::enter_fence(&ctx, &current, crate::metrics::now_us()).is_ok());
        drop(fresh_lease);
        // A historical connection cannot act on the new binding even with a current revision.
        if rebind {
            external.connection = Some(CONN);
            assert!(
                SupervisorInputFence::authorize(
                    &ctx,
                    request(&ctx, false, Scope::Approvals),
                    &external,
                    crate::metrics::now_us()
                )
                .is_err()
            );
        }
    }
}

#[test]
fn scoped_input_drain_conforms_for_queued_and_started_batches() {
    use aterm_models_harness::scoped_input_drain_model;
    let model = scoped_input_drain_model();
    for started in [false, true] {
        let h = crate::session_store::test_handle_on(7617 + u64::from(started), -1);
        let ctx = Arc::clone(&h.ctx);
        install(&ctx, CONN, &[Scope::Upgrade]);
        let initial_revision = revision(&ctx);
        let mut state = model.init_state();
        let mut old_batch = None;
        let mut fresh = None;
        let mut transition = None;
        let mut reply = None;
        let mut observed_ack = false;
        let mut stale_byte = false;
        let mut conflict = false;
        let mut tail = false;
        let queued = SupervisorInputDelivery::new(&ctx, ticket(&ctx, true, Scope::Approvals));
        let mut schedule = vec!["Queue"];
        if started {
            schedule.extend(["Start", "TransitionWriting"]);
        } else {
            schedule.push("TransitionQueued");
        }
        schedule.extend([
            "Stale",
            "ConflictingStart",
            "NewWork",
            "Continue",
            "EarlyAck",
            "Finish",
            "Ack",
        ]);
        for action in schedule {
            assert!(model.fire(action, &mut state), "enabled {action}");
            match action {
                "Queue" => {}
                "Start" => {
                    let delivery =
                        SupervisorInputDelivery::new(&ctx, ticket(&ctx, true, Scope::Approvals));
                    let batch = delivery.batch().unwrap();
                    assert!(batch.start());
                    old_batch = Some(batch);
                }
                "TransitionQueued" | "TransitionWriting" => {
                    let (tx, rx) = mpsc::channel();
                    let barrier = Arc::new(Barrier::new(2));
                    let worker_ctx = Arc::clone(&ctx);
                    let worker_barrier = Arc::clone(&barrier);
                    transition = Some(std::thread::spawn(move || {
                        worker_barrier.wait();
                        tx.send(install(&worker_ctx, CONN, &[Scope::Approvals]).revision)
                            .unwrap();
                    }));
                    reply = Some(rx);
                    barrier.wait();
                    await_revision(&ctx, initial_revision);
                }
                "Stale" => stale_byte = queued.batch().unwrap().start(),
                "ConflictingStart" => {
                    let current = ticket(&ctx, false, Scope::Approvals);
                    conflict =
                        FencedWrite::enter_fence(&ctx, &current, crate::metrics::now_us()).is_ok();
                }
                "NewWork" => {
                    fresh = Some(
                        FencedWrite::enter_fence(
                            &ctx,
                            &ticket(&ctx, true, Scope::Questions),
                            crate::metrics::now_us(),
                        )
                        .unwrap(),
                    )
                }
                "Continue" => tail = old_batch.as_ref().is_some_and(|batch| batch.start()),
                "EarlyAck" => {
                    let rx = reply.as_ref().unwrap();
                    observed_ack = if started {
                        rx.try_recv().is_ok()
                    } else {
                        rx.recv_timeout(PATIENCE).expect("hang: empty transition");
                        true
                    };
                }
                "Finish" => drop(old_batch.take()),
                "Ack" => {
                    if !observed_ack {
                        reply
                            .as_ref()
                            .unwrap()
                            .recv_timeout(PATIENCE)
                            .expect("hang: newer revision held the transition");
                        observed_ack = true;
                    }
                }
                _ => unreachable!(),
            }
            let current_revision = revision(&ctx);
            let inflight = ctx.fenced_writes.state.inflight.lock().unwrap();
            let count = |rev| inflight.get(&rev).map_or(0, Vec::len) as i64;
            for (name, actual) in [
                ("revision", (current_revision - initial_revision) as i64),
                ("old", count(initial_revision)),
                ("fresh", count(initial_revision + 1)),
                ("started", i64::from(started && action != "Queue")),
                ("stale_byte", i64::from(stale_byte)),
                ("conflict", i64::from(conflict)),
                ("tail", i64::from(tail)),
                ("ack", i64::from(observed_ack)),
            ] {
                assert_eq!(
                    actual, state[name],
                    "{name} after {action}, started={started}"
                );
            }
            drop(inflight);
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &state),
                    "{} after {action}",
                    invariant.name
                );
            }
        }
        transition.unwrap().join().unwrap();
        drop(fresh);
    }
}

#[test]
fn scoped_input_drain_negative_controls_break_each_law() {
    use aterm_models_harness::scoped_input_drain_model;
    use aterm_spec::interp;
    let base = scoped_input_drain_model();
    for (bug, law) in [
        (1, "StaleQueueWritesNothing"),
        (2, "OldWriterExcludesConflictingActor"),
        (3, "StartedCommandFinishes"),
        (4, "AckWaitsForOldWriter"),
        (5, "NewWorkDoesNotStarveDrain"),
    ] {
        let mutant = interp::with_buggy(&base, bug);
        let mut state = mutant.init_state();
        let mut caught = false;
        for action in [
            "Queue",
            "Start",
            "TransitionWriting",
            "Stale",
            "ConflictingStart",
            "NewWork",
            "Continue",
            "EarlyAck",
            "Finish",
            "Ack",
        ] {
            assert!(mutant.fire(action, &mut state));
            caught |= !base.check_invariant(law, &state);
        }
        assert!(caught, "negative control {bug} must falsify {law}");
    }
}

#[test]
fn scoped_input_seal_conforms_while_response_handles_remain_live() {
    use crate::control_input_ack::InputMark;
    use aterm_models_harness::scoped_input_seal_model;
    let model = scoped_input_seal_model();
    for seal_first in [false, true] {
        let h = crate::session_store::test_handle_on(7626 + u64::from(seal_first), -1);
        install(&h.ctx, CONN, &[Scope::Upgrade]);
        let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
        let batch = delivery.batch().unwrap();
        let mark = InputMark::at_epoch(
            &h.ctx,
            h.local_id,
            aterm_slog::Actor::Harness,
            crate::session_log::InputVerb::Turn,
            None,
            true,
        )
        .supervised(Some(Arc::clone(&batch)));
        let producer = mark.supervisor_producer().unwrap();
        let mut completion = Some(mark.completion());
        let mut state = model.init_state();
        let mut ack = false;
        let mut schedule = vec!["Start"];
        schedule.extend(if seal_first {
            ["SealPending", "FinishPending"]
        } else {
            ["FinishUnsealed", "SealDone"]
        });
        schedule.extend(["ReadWait", "Ack"]);
        for action in schedule {
            assert!(model.fire(action, &mut state));
            match action {
                "Start" => assert!(completion.as_ref().unwrap().start_supervised()),
                "SealPending" | "SealDone" => mark.seal_supervisor_input(),
                "FinishPending" | "FinishUnsealed" => drop(completion.take()),
                "ReadWait" => {
                    assert!(
                        !mark.completion().start_supervised(),
                        "a sealed producer cannot enqueue more work"
                    );
                }
                "Ack" => {
                    assert!(batch.state.lock().unwrap().sealed);
                    ack = !h
                        .ctx
                        .fenced_writes
                        .state
                        .inflight
                        .lock()
                        .unwrap()
                        .values()
                        .any(|v| !v.is_empty());
                }
                _ => unreachable!(),
            }
            let actual = batch.state.lock().unwrap();
            for (key, value) in [
                ("sealed", i64::from(actual.sealed)),
                ("pending", actual.pending as i64),
                (
                    "lease",
                    i64::from(matches!(actual.phase, InputBatchPhase::Writing(_))),
                ),
                ("ack", i64::from(ack)),
            ] {
                assert_eq!(
                    value, state[key],
                    "{key} after {action}, seal_first={seal_first}"
                );
            }
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &state),
                    "{} after {action}",
                    invariant.name
                );
            }
        }
        // These are all still alive during Ack: their Arc lifetime is no longer write custody.
        drop((producer, mark, delivery, batch));
    }
    for (bug, law, middle) in [
        (1, "PendingWorkOwnsLease", ["SealPending", "FinishPending"]),
        (
            2,
            "ResponseReadersOwnNoLease",
            ["SealPending", "FinishPending"],
        ),
        (
            3,
            "ProducerCanFinishItsSuffix",
            ["FinishUnsealed", "SealDone"],
        ),
        (
            2,
            "AckDoesNotWaitForResponse",
            ["FinishUnsealed", "SealDone"],
        ),
    ] {
        let mutant = aterm_spec::interp::with_buggy(&model, bug);
        let mut state = mutant.init_state();
        let mut caught = false;
        for action in ["Start", middle[0], middle[1], "ReadWait", "Ack"] {
            assert!(mutant.fire(action, &mut state));
            caught |= !model.check_invariant(law, &state);
        }
        assert!(caught, "negative control {bug}: {law}");
    }
}

#[test]
fn sealed_gui_work_can_finish_but_new_work_and_cancelled_queues_cannot_revive() {
    use crate::control_input_ack::InputMark;
    for cancel in [false, true] {
        let h = crate::session_store::test_handle_on(7628 + u64::from(cancel), -1);
        install(&h.ctx, CONN, &[Scope::Upgrade]);
        let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
        let mark = InputMark::at_epoch(
            &h.ctx,
            h.local_id,
            aterm_slog::Actor::Harness,
            crate::session_log::InputVerb::Turn,
            None,
            true,
        )
        .supervised(delivery.batch());
        let prefix = mark.completion();
        assert!(prefix.start_supervised());
        drop(prefix);
        let queued_gui_mark = mark.completion().into_mark();
        mark.seal_supervisor_input();
        let after_seal = queued_gui_mark.completion();
        drop(queued_gui_mark);
        assert!(
            !h.ctx
                .fenced_writes
                .state
                .inflight
                .lock()
                .unwrap()
                .is_empty(),
            "GUI-to-writer transfer never loses pending custody"
        );
        assert!(
            !mark.completion().start_supervised(),
            "base observation mark cannot manufacture new sealed work"
        );
        if cancel {
            drop(after_seal);
        } else {
            assert!(
                after_seal.start_supervised(),
                "registered pre-seal work keeps the admitted suffix"
            );
            after_seal.finish(crate::input::EgressReceipt::unwritten(
                crate::input::Egress::Reported(crate::input::Delivery::Failed),
            ));
        }
        assert!(
            h.ctx
                .fenced_writes
                .state
                .inflight
                .lock()
                .unwrap()
                .is_empty(),
            "cancel/drop and failed delivery both release exactly their pending reservation"
        );
        assert!(delivery.batch().unwrap().state.lock().unwrap().sealed);
    }
}

#[test]
#[cfg(unix)]
fn actual_turn_seals_input_before_its_read_only_response_wait() {
    use crate::control::{TurnIo, cmd_turn};
    use crate::control_input_ack::{InputMark, testing};
    let (h, mut reader) = testing::owned_handle(7630);
    let store = crate::session_store::new_store();
    store.write().unwrap().register(h.clone());
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::TurnEnd));
    let batch = delivery.batch().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = mpsc::channel();
    let turn = {
        let h = h.clone();
        let store = Arc::clone(&store);
        let batch = Arc::clone(&batch);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let mark = InputMark::at_epoch(
                &h.ctx,
                h.local_id,
                aterm_slog::Actor::Harness,
                crate::session_log::InputVerb::Turn,
                None,
                true,
            )
            .supervised(Some(batch));
            let paste = |text: &str| {
                let completion = mark.completion();
                assert!(completion.start_supervised());
                let receipt = crate::app_input::tracked_egress(
                    &h.term,
                    &h.ctx.modes,
                    &h.ctx.sink,
                    &h.ctx.output_echo,
                    &crate::input::InputEvent::KeySequence(text.as_bytes().to_vec()),
                    crate::input::EgressMode::Backpressured,
                );
                completion.finish(receipt);
                barrier.wait();
                true
            };
            let reply = cmd_turn(
                &h.term,
                &store,
                h.local_id,
                "submit=none settle=match:response-done idle=60000 timeout=60000 X",
                &h.ctx,
                &TurnIo {
                    paste: &paste,
                    mark: Some(&mark),
                    ..TurnIo::paste_only()
                },
            );
            tx.send(reply).unwrap();
        })
    };
    barrier.wait();
    assert_eq!(testing::byte(&mut reader), b'X');
    let since = Instant::now();
    while !batch.state.lock().unwrap().sealed {
        assert!(
            since.elapsed() < PATIENCE,
            "hang: turn never sealed its input"
        );
        aterm_uds::deadline::poll_yield();
    }
    let response_still_waits = rx.try_recv().is_err();
    let drained = h
        .ctx
        .fenced_writes
        .state
        .inflight
        .lock()
        .unwrap()
        .is_empty();
    let transition_acked_while_response_waited = if drained {
        install(&h.ctx, CONN, &[Scope::Approvals]);
        rx.try_recv().is_err()
    } else {
        false
    };
    // Always release the test's response wait before assertions, including a negative control.
    h.term.lock().unwrap().process(b"response-done\r\n");
    h.ctx.watchers.ring_watchers();
    let reply = rx
        .recv_timeout(PATIENCE)
        .expect("hang: settled response never answered");
    turn.join().unwrap();
    assert!(response_still_waits, "the response phase was exercised");
    assert!(
        drained,
        "all input was finished while the response and its Arcs remained live"
    );
    assert!(transition_acked_while_response_waited);
    assert!(reply.contains("status=settled"), "{reply}");
}

#[test]
fn turn_early_return_seals_the_producer_without_dropping_pending_work() {
    use crate::control::{TurnIo, cmd_turn};
    use crate::control_input_ack::InputMark;
    let h = crate::session_store::test_handle_on(7631, -1);
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::TurnEnd));
    let mark = InputMark::at_epoch(
        &h.ctx,
        h.local_id,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Turn,
        None,
        true,
    )
    .supervised(delivery.batch());
    let prefix = mark.completion();
    assert!(prefix.start_supervised());
    drop(prefix);
    let pending = mark.completion();
    let reply = cmd_turn(
        &h.term,
        &crate::session_store::new_store(),
        h.local_id,
        "timeout=invalid X",
        &h.ctx,
        &TurnIo {
            mark: Some(&mark),
            ..TurnIo::paste_only()
        },
    );
    assert!(reply.starts_with("ERR usage"));
    assert!(delivery.batch().unwrap().state.lock().unwrap().sealed);
    assert!(
        !h.ctx
            .fenced_writes
            .state
            .inflight
            .lock()
            .unwrap()
            .is_empty()
    );
    drop(pending);
    assert!(
        h.ctx
            .fenced_writes
            .state
            .inflight
            .lock()
            .unwrap()
            .is_empty()
    );
}

#[test]
#[cfg(unix)]
fn gui_revocation_expiry_and_eof_publish_before_queued_suffix_and_control_ack_waits_for_it() {
    use crate::control_input_ack::{InputMark, testing};
    for kind in 0..3 {
        let (h, mut reader) = testing::owned_handle(7632 + kind);
        install(&h.ctx, CONN, &[Scope::Upgrade]);
        let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
        let mark = InputMark::at_epoch(
            &h.ctx,
            h.local_id,
            aterm_slog::Actor::Harness,
            crate::session_log::InputVerb::Turn,
            None,
            true,
        )
        .supervised(delivery.batch());
        let _producer = mark.supervisor_producer();
        let prefix = mark.completion();
        assert!(prefix.start_supervised());
        h.ctx.sink.write_frame(b"A").unwrap();
        drop(prefix);
        assert_eq!(testing::byte(&mut reader), b'A');
        let suffix = mark.completion().into_mark();
        let (published_tx, published_rx) = mpsc::channel();
        let (work_tx, work_rx) = mpsc::channel::<InputMark>();
        let (delivered_tx, delivered_rx) = mpsc::channel();
        let gui = {
            let h = h.clone();
            std::thread::spawn(move || {
                let now = crate::metrics::now_us();
                let transition = match kind {
                    0 => publish_release_proven_if_gone(&h.ctx, -1, now),
                    1 => {
                        h.ctx
                            .meta
                            .lock()
                            .unwrap()
                            .supervisor
                            .as_mut()
                            .unwrap()
                            .expires_us = Some(now);
                        publish_lapse_supervisor(&h.ctx, now)
                    }
                    _ => {
                        let transition = publish_release_supervisor(
                            &h.ctx,
                            Some(CONN),
                            now,
                            aterm_slog::Actor::Aterm,
                        );
                        close_claim_connection(&h.ctx, CONN, now);
                        transition
                    }
                };
                assert!(*transition.published_ref());
                published_tx.send(transition).unwrap();
                // This is the GUI event queued behind revocation. Waiting for drain above
                // would keep the suffix itself from ever making progress.
                let queued = work_rx
                    .recv_timeout(PATIENCE)
                    .expect("hang: suffix event never dispatched");
                let completion = queued.completion();
                assert!(completion.start_supervised());
                h.ctx.sink.write_frame(b"B").unwrap();
                drop(completion);
                drop(queued);
                delivered_tx.send(()).unwrap();
            })
        };
        let transition = published_rx
            .recv_timeout(PATIENCE)
            .expect("hang: GUI publication waited for its own queued suffix");
        assert!(
            h.ctx
                .meta
                .lock()
                .unwrap()
                .live_claim(crate::metrics::now_us())
                .is_none()
        );
        let (ack_tx, ack_rx) = mpsc::channel();
        let barrier = Arc::new(Barrier::new(2));
        let ack = {
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ack_tx.send(transition.acknowledge()).unwrap();
            })
        };
        barrier.wait();
        assert!(
            ack_rx.try_recv().is_err(),
            "negative control: the ACK lane must still drain the admitted writer"
        );
        work_tx.send(suffix).unwrap();
        delivered_rx
            .recv_timeout(PATIENCE)
            .expect("hang: queued suffix could not run after GUI publication");
        assert_eq!(testing::byte(&mut reader), b'B');
        assert!(
            ack_rx.try_recv().is_err(),
            "producer may still append to its command"
        );
        mark.seal_supervisor_input();
        assert!(
            ack_rx
                .recv_timeout(PATIENCE)
                .expect("hang: completed suffix never released control ACK")
        );
        ack.join().unwrap();
        gui.join().unwrap();
    }
}

#[test]
#[cfg(unix)]
fn guarded_scope_ack_drains_its_real_pipe_prefix_without_waiting_for_later_terminal_replies() {
    use crate::control_input_ack::{InputMark, testing};
    use std::io::Read;
    let (h, mut reader) = testing::owned_handle(7635);
    aterm_pty::set_nonblocking(h.ctx.sink.master(), true).unwrap();
    h.ctx.sink.note_master_nonblocking(true);
    h.ctx.sink.install_spill_arranger(|| {});
    let mut filled = 0;
    loop {
        match aterm_pty::write_some(h.ctx.sink.master(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("fill private authority pipe: {other:?}"),
        }
    }
    assert!(filled > 0);
    let room = filled.min(8192);
    let mut removed = vec![0; room];
    reader.read_exact(&mut removed).unwrap();
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
    let mark = InputMark::at_epoch(
        &h.ctx,
        h.local_id,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Key,
        None,
        false,
    )
    .supervised(delivery.batch());
    let old = vec![b'P'; aterm_session::sink::SinkWriter::IMMEDIATE_FRAME_MAX];
    let (write_tx, write_rx) = mpsc::channel();
    let writer = {
        let h = h.clone();
        let old = old.clone();
        std::thread::spawn(move || {
            let _producer = mark.supervisor_producer();
            let decision = crate::control::input_if_fenced_observed(
                &h.term,
                &h.ctx,
                crate::input::InputEvent::KeySequence(old),
                &crate::control::InputFence::default(),
                crate::ReadOn::Control,
                Some(mark.completion()),
            );
            write_tx.send(decision).unwrap();
        })
    };
    let since = Instant::now();
    while h.ctx.sink.spilled_bytes() == 0 {
        assert!(
            since.elapsed() < PATIENCE,
            "hang: guarded write never admitted its partial pipe suffix"
        );
        aterm_uds::deadline::poll_yield();
    }
    assert_eq!(
        h.ctx.sink.write_reply(&vec![b'R'; 512 * 1024]).unwrap(),
        512 * 1024
    );
    let transition = publish_claim_supervisor_scoped(
        &h.ctx,
        "grok-alab@4242",
        Some(CONN),
        scoped(&[Scope::Approvals]),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    let (ack_tx, ack_rx) = mpsc::channel();
    let ack = std::thread::spawn(move || {
        ack_tx.send(transition.acknowledge().revision).unwrap();
    });
    assert!(
        ack_rx.try_recv().is_err(),
        "the actual command prefix still owns authority"
    );
    let want = filled - room + old.len();
    let mut received = vec![0; want];
    let mut off = 0;
    let since = Instant::now();
    while off < want {
        match reader.read(&mut received[off..]) {
            Ok(n) if n > 0 => off += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    since.elapsed() < PATIENCE,
                    "hang: admitted command prefix never reached the pipe"
                );
                aterm_uds::deadline::poll_yield();
            }
            other => panic!("read authority prefix: {other:?}"),
        }
    }
    assert!(received[..filled - room].iter().all(|byte| *byte == b'.'));
    assert_eq!(
        &received[filled - room..],
        old.as_slice(),
        "no admitted prefix or suffix byte was truncated"
    );
    assert!(matches!(
        write_rx
            .recv_timeout(PATIENCE)
            .expect("hang: later query reply held guarded command completion"),
        crate::control::GuardedInput::Pressed(crate::input::Delivery::Full)
    ));
    assert!(
        ack_rx
            .recv_timeout(PATIENCE)
            .expect("hang: later reply held old authority ACK")
            > 0
    );
    assert!(
        h.ctx.sink.spilled_bytes() > 0,
        "the later reply is still genuinely queued"
    );
    assert_ne!(
        h.ctx.sink.try_egress_drained_to_kernel(),
        Some(true),
        "negative control: the former global drain still refuses here, including when busy"
    );
    writer.join().unwrap();
    ack.join().unwrap();
    h.ctx.sink.sever_input();
}

#[test]
#[cfg(unix)]
fn guarded_scope_cancel_keeps_transition_ack_behind_its_started_native_write() {
    use crate::control_input_ack::{InputMark, testing};
    use aterm_session::sink::{Seam, SinkWriter};
    use std::io::Read;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (h, mut reader) = testing::owned_handle(7636);
    aterm_pty::set_nonblocking(h.ctx.sink.master(), true).unwrap();
    h.ctx.sink.note_master_nonblocking(true);
    let sink = Arc::downgrade(&h.ctx.sink);
    h.ctx.sink.install_spill_arranger(move || {
        let sink = sink.upgrade().expect("fixture sink remains owned");
        // The guarded direct prefix has finished; the queued suffix now exercises
        // the real blocking-description drainer, which must release its spill lock.
        aterm_pty::set_nonblocking(sink.master(), false).unwrap();
        sink.note_master_nonblocking(false);
    });
    let mut filled = 0;
    loop {
        match aterm_pty::write_some(h.ctx.sink.master(), &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("fill cancellation pipe: {other:?}"),
        }
    }
    assert!(filled > 0);
    let room = filled.min(8192);
    let mut removed = vec![0; room];
    reader.read_exact(&mut removed).unwrap();
    let (at_tx, at_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let resume_rx = Mutex::new(resume_rx);
    let calls = Arc::new(AtomicUsize::new(0));
    h.ctx.sink.seam_for_testing({
        let calls = Arc::clone(&calls);
        move |seam| {
            if seam == Seam::DrainWrite {
                assert_eq!(calls.fetch_add(1, Ordering::SeqCst), 0);
                at_tx.send(()).unwrap();
                resume_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(PATIENCE)
                    .expect("hang: guarded native reservation was never released");
            }
        }
    });
    install(&h.ctx, CONN, &[Scope::Upgrade]);
    let old_revision = revision(&h.ctx);
    let delivery = SupervisorInputDelivery::new(&h.ctx, ticket(&h.ctx, true, Scope::Approvals));
    let mark = InputMark::at_epoch(
        &h.ctx,
        h.local_id,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Key,
        None,
        false,
    )
    .supervised(delivery.batch());
    let (write_tx, write_rx) = mpsc::channel();
    let writer = {
        let h = h.clone();
        std::thread::spawn(move || {
            let _producer = mark.supervisor_producer();
            let decision = crate::control::input_if_fenced_observed(
                &h.term,
                &h.ctx,
                crate::input::InputEvent::KeySequence(vec![b'P'; SinkWriter::IMMEDIATE_FRAME_MAX]),
                &crate::control::InputFence::default(),
                crate::ReadOn::Control,
                Some(mark.completion()),
            );
            write_tx.send(decision).unwrap();
        })
    };
    at_rx
        .recv_timeout(PATIENCE)
        .expect("hang: guarded suffix never started");
    assert_eq!(h.ctx.sink.try_egress_drained_to_kernel(), Some(false));
    let direct = SinkWriter::IMMEDIATE_FRAME_MAX - h.ctx.sink.spilled_bytes();
    assert!(direct > 0 && direct < SinkWriter::IMMEDIATE_FRAME_MAX);
    let transition = publish_claim_supervisor_scoped(
        &h.ctx,
        "grok-alab@4242",
        Some(CONN),
        scoped(&[Scope::Approvals]),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    let (ack_tx, ack_rx) = mpsc::channel();
    let ack = std::thread::spawn(move || {
        ack_tx.send(transition.acknowledge().revision).unwrap();
    });
    h.ctx.sink.sever_input();
    assert_eq!(
        h.ctx.sink.spilled_bytes(),
        0,
        "all queued bytes were canceled"
    );
    assert_eq!(
        h.ctx
            .fenced_writes
            .state
            .inflight
            .lock()
            .unwrap()
            .get(&old_revision)
            .map(Vec::len),
        Some(1),
        "the actual guarded worker retains authority across canceled native I/O"
    );
    assert!(write_rx.try_recv().is_err());
    assert!(ack_rx.try_recv().is_err());
    // Empty the actual pipe so its already registered native call can finish.
    let mut before = vec![0; filled - room + direct];
    let mut off = 0;
    let since = Instant::now();
    while off < before.len() {
        match reader.read(&mut before[off..]) {
            Ok(n) if n > 0 => off += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    since.elapsed() < PATIENCE,
                    "hang: direct pipe prefix never arrived"
                );
                aterm_uds::deadline::poll_yield();
            }
            other => panic!("read direct cancellation prefix: {other:?}"),
        }
    }
    assert!(before[..filled - room].iter().all(|byte| *byte == b'.'));
    assert!(before[filled - room..].iter().all(|byte| *byte == b'P'));
    resume_tx.send(()).unwrap();
    let since = Instant::now();
    let mut native_bytes = 0;
    loop {
        match write_rx.try_recv() {
            Ok(_decision) if native_bytes > 0 => break,
            Ok(_decision) => {
                let mut byte = [0];
                reader.read_exact(&mut byte).unwrap();
                assert_eq!(byte, [b'P']);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(error) => panic!("guarded worker disappeared: {error}"),
        }
        let mut bytes = [0; 8192];
        match reader.read(&mut bytes) {
            Ok(n) if n > 0 => {
                native_bytes += n;
                assert!(bytes[..n].iter().all(|byte| *byte == b'P'));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    since.elapsed() < PATIENCE,
                    "hang: canceled native write never retired"
                );
                aterm_uds::deadline::poll_yield();
            }
            other => panic!("read registered native write: {other:?}"),
        }
    }
    assert!(
        ack_rx
            .recv_timeout(PATIENCE)
            .expect("hang: canceled native operation held ACK")
            > 0
    );
    writer.join().unwrap();
    ack.join().unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no canceled chunk suffix may start another write"
    );
}
