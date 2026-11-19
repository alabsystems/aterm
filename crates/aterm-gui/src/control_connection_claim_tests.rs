// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn transferred_native_exec_claim_is_released_when_its_reply_is_lost() {
    use super::control_lanes::{LaneLimits, LaneReader, LaneService, Lanes, Outbox};
    use crate::session_timeline::SupervisorAuthority;
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use std::sync::mpsc;

    struct Service {
        store: Store,
        session: Arc<crate::SessionCtx>,
        expected: SupervisorAuthority,
        dispatched: mpsc::Sender<(u64, bool)>,
        continue_reply: Mutex<Option<mpsc::Receiver<()>>>,
        released: mpsc::Sender<u64>,
    }

    impl LaneService for Service {
        fn authenticate(
            &self,
            _stream: &CtlStream,
            reader: &mut LaneReader<'_>,
        ) -> Option<(Scope, Principal, Option<String>)> {
            (read_request_line(reader)? == "AUTH ok").then_some((
                Scope::Owner,
                Principal::Instance,
                None,
            ))
        }

        fn serve_line(
            &self,
            request: Request,
            _scope: Scope,
            _stream: &CtlStream,
            _reader: &mut LaneReader<'_>,
            out: &mut Outbox,
        ) -> Option<ServeDisposition> {
            assert_eq!(request.line(), "transfer");
            let connection = serving_connection().expect("serving lane serial");
            let accepted = transfer_served_native_exec_claim(
                &mut self.session.meta.lock().unwrap(),
                &self.expected,
                connection,
                None,
                crate::metrics::now_us(),
            )
            .is_some();
            self.dispatched.send((connection, accepted)).unwrap();
            if accepted {
                let resume = self.continue_reply.lock().unwrap().take().unwrap();
                resume.recv_timeout(MUST_HAPPEN).unwrap();
            }
            out.extend_from_slice(if accepted { b"OK\n" } else { b"ERR stale\n" });
            None
        }

        fn is_wait(&self, _request: &Request, _scope: Scope) -> bool {
            false
        }

        fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

        fn has_live_claim(&self, id: u64) -> bool {
            connection_has_live_claim(&self.store, id, crate::metrics::now_us())
        }

        fn release(&self, id: u64) {
            release_connection_claims(&self.store, None, id);
            self.released.send(id).unwrap();
        }
    }

    let store = crate::session_store::new_store();
    let handle = crate::session_store::test_handle(901);
    store.write().unwrap().register(handle.clone());
    let proof = crate::self_claim::stand_in::running();
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor_as(
        &handle.ctx,
        &format!("codex-alab@{}", proof.pid),
        Some(u64::MAX),
        None,
        Some(proof),
        now,
        aterm_slog::Actor::Ctl,
    )
    .unwrap();
    let expected = handle.ctx.meta.lock().unwrap().supervisor_authority(now);
    let (dispatched, dispatch) = mpsc::channel();
    let (continue_reply, resume_reply) = mpsc::channel();
    let (released, release) = mpsc::channel();
    let lanes = Lanes::new(LaneLimits {
        rpc: 2,
        wait: 0,
        inline: 0,
        open: 2,
        subscriptions: 0,
    });
    let started = lanes.start();
    assert_eq!(started.rpc, 2);
    lanes.publish(Arc::new(Service {
        store: store.clone(),
        session: Arc::clone(&handle.ctx),
        expected,
        dispatched,
        continue_reply: Mutex::new(Some(resume_reply)),
        released,
    }));
    started.activate();
    let request_transfer = || {
        let (client, server) = CtlStream::pair().unwrap();
        assert!(lanes.admit(server).is_ok());
        (&client).write_all(b"AUTH ok\ntransfer\n").unwrap();
        client
    };

    let successor = request_transfer();
    let (owner, true) = dispatch.recv_timeout(MUST_HAPPEN).unwrap() else {
        panic!("the original v2 authority transfers to the successor");
    };
    let stale = request_transfer();
    let (rejected, false) = dispatch.recv_timeout(MUST_HAPPEN).unwrap() else {
        panic!("the old snapshot cannot take the transferred claim");
    };
    assert_ne!(owner, rejected);
    assert!(connection_has_live_claim(
        &store,
        owner,
        crate::metrics::now_us()
    ));
    drop(stale);
    drop(successor);
    continue_reply.send(()).unwrap();
    assert_eq!(release.recv_timeout(MUST_HAPPEN).unwrap(), owner);
    until_within(MUST_HAPPEN, "both sockets finish their tenure", || {
        let census = lanes.census();
        (census.open == 0 && census.placing == 0).then_some(())
    });
    assert!(release.try_recv().is_err());
    assert!(!connection_has_live_claim(
        &store,
        owner,
        crate::metrics::now_us()
    ));
}

#[test]
fn only_live_connection_bound_claims_spare_their_connection() {
    let store = crate::session_store::new_store();
    let supervisor = crate::session_store::test_handle(1);
    let driver = crate::session_store::test_handle(2);
    store.write().unwrap().register(supervisor.clone());
    store.write().unwrap().register(driver.clone());
    let claimed = |conn, now| connection_has_live_claim(&store, conn, now);
    assert!(!claimed(7, 100));
    assert_eq!(
        crate::session_timeline::claim_supervisor(&supervisor.ctx, "sup", Some(7), None, 100),
        Ok(true)
    );
    assert!(claimed(7, 100));
    assert!(!claimed(8, 100), "another connection owns no claim");
    assert!(crate::session_timeline::release_supervisor(
        &supervisor.ctx,
        Some(7),
        100,
        aterm_slog::Actor::Ctl
    ));
    assert!(!claimed(7, 100), "release makes the connection evictable");
    assert_eq!(
        crate::session_timeline::claim_supervisor(&supervisor.ctx, "sup", None, Some(500), 100),
        Ok(true)
    );
    assert!(
        supervisor
            .ctx
            .meta
            .lock()
            .unwrap()
            .live_claim(499)
            .is_some()
    );
    assert!(
        !claimed(7, 499),
        "a detached live TTL outlives the connection"
    );
    assert!(
        supervisor
            .ctx
            .meta
            .lock()
            .unwrap()
            .live_claim(500)
            .is_none()
    );
    assert!(
        !claimed(7, 500),
        "expired supervisor authority spares no connection"
    );

    struct Restore(Option<u64>, bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERVING_CONNECTION.with(|s| s.set(self.0));
            CONNECTION_CLAIMED.with(|c| c.set(self.1));
        }
    }
    let _restore = Restore(
        SERVING_CONNECTION.with(|s| s.replace(Some(7))),
        CONNECTION_CLAIMED.with(|c| c.replace(false)),
    );
    let lease = |rest| control_session::cmd_lease(&driver.ctx, rest);
    assert!(lease("acquire holder=drive ttl=60000 hard here").starts_with("OK lease acquired"));
    let expiry = match driver.ctx.turn_lease.lock().unwrap().as_ref().unwrap() {
        crate::Lease::Drive { expires_us, .. } => *expires_us,
        other => panic!("expected the acquired drive lease, got {other:?}"),
    };
    assert!(claimed(7, expiry - 1));
    assert!(
        !claimed(8, expiry - 1),
        "another owner does not protect this connection"
    );
    assert!(
        !claimed(7, expiry),
        "the drive lease's expiry is authoritative"
    );
    assert_eq!(lease("release holder=drive here"), "OK lease released\n");
    assert!(!claimed(7, crate::metrics::now_us()));
    assert!(
        CONNECTION_CLAIMED.with(std::cell::Cell::get),
        "Drop still owes cleanup"
    );
    assert!(lease("acquire holder=drive ttl=60000 hard here").starts_with("OK lease acquired"));
    SERVING_CONNECTION.with(|s| s.set(Some(8)));
    assert!(lease("acquire holder=drive ttl=60000 hard").starts_with("OK lease acquired"));
    assert!(!claimed(7, crate::metrics::now_us()));
    assert!(
        !claimed(8, crate::metrics::now_us()),
        "a detached renewal outlives either connection"
    );
}

/// EVERY PROVEN CONTRACT 4 CLAIM KEEPS THE RE-EXEC GRACE, a stream or none (D28): its connection
/// closes and the claim stands for 45 s more with the grace's end as its expiry, placed by no
/// claim (the placement count and the revision stand). A second close of the same serial does
/// not extend it; the same holder claiming on a new connection renews it with no expiry, and
/// the old connection's close then releases nothing; `meta unset supervisor` releases it at
/// once, inside the grace; left alone it lapses at the grace's end and not before.
/// NEGATIVE HALF: a contract 3 claim, a proven codex-alab self-claim and an unproven claim
/// each end with their connection, as they always did.
#[cfg(unix)]
#[test]
fn a_contract_4_claim_without_a_stream_keeps_the_grace_too() {
    use aterm_agent::harness::contract::{SCOPED, Scope as Duty, UNIVERSAL};
    let grace_us = alab_wire::record::REEXEC_GRACE_MS * 1000;
    assert_eq!(
        reexec_grace_us(),
        Some(45_000_000),
        "the wire contract's 45 s"
    );
    let store = crate::session_store::new_store();
    let h = crate::session_store::test_handle(1);
    store.write().unwrap().register(h.clone());
    let holder = "grok-alab@4242";
    let claim = |conn: u64, contract: u32| {
        let mut scoped = crate::session_timeline::scoped_input_tests::scoped(&[Duty::Relaunch]);
        scoped.claim.contract = contract;
        crate::session_timeline::claim_supervisor_scoped(
            &h.ctx,
            holder,
            Some(conn),
            scoped,
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("the claim");
    };
    // (live at `now`, the connection, the expiry, the placements, the revision)
    let read = |now: u64| {
        let meta = h.ctx.meta.lock().unwrap();
        (
            meta.live_claim(now).is_some(),
            meta.supervisor.as_ref().and_then(|claim| claim.conn),
            meta.supervisor_expiry(),
            meta.claim_placements,
            meta.supervisor_revision,
        )
    };

    claim(10, UNIVERSAL);
    let (_, _, _, placements, revision) = read(crate::metrics::now_us());
    let closing = crate::metrics::now_us();
    release_connection_claims(&store, None, 10);
    let closed = crate::metrics::now_us();
    let (live, conn, expiry, placed, at) = read(closed);
    let expiry = expiry.expect("the grace's end is the claim's expiry");
    assert!(live, "the claim outlives its connection");
    assert!(
        (closing + grace_us..=closed + grace_us).contains(&expiry),
        "{expiry} is the close plus 45 s"
    );
    assert_eq!(
        (conn, placed, at),
        (Some(10), placements, revision),
        "a close is no claim: nothing was placed and the authority stands"
    );
    assert!(read(expiry - 1).0 && !read(expiry).0, "live to its end");
    assert!(
        !connection_has_live_claim(&store, 11, closed),
        "the kept claim spares no other connection"
    );

    // A second close of the same serial does not extend the grace.
    release_connection_claims(&store, None, 10);
    assert_eq!(read(closed).2, Some(expiry));

    // The same holder claims on a new connection inside the grace: renewed, no expiry; the old
    // connection's close then finds nothing of its own.
    claim(11, UNIVERSAL);
    assert_eq!(read(crate::metrics::now_us()).1, Some(11));
    assert_eq!(read(crate::metrics::now_us()).2, None, "bound again");
    release_connection_claims(&store, None, 10);
    let (live, conn, expiry, ..) = read(crate::metrics::now_us());
    assert_eq!((live, conn, expiry), (true, Some(11), None));

    // `meta unset supervisor` releases at once, inside the grace.
    release_connection_claims(&store, None, 11);
    assert!(read(crate::metrics::now_us()).2.is_some(), "in its grace");
    assert_eq!(
        meta_line_for_test(&store, &h.term, h.local_id, &h.ctx, "unset supervisor"),
        "OK\n"
    );
    assert!(
        h.ctx.meta.lock().unwrap().supervisor.is_none(),
        "released with no grace"
    );

    // Left alone, the grace's end is the claim's end, and not a microsecond before.
    claim(12, UNIVERSAL);
    release_connection_claims(&store, None, 12);
    let expiry = read(crate::metrics::now_us()).2.expect("in its grace");
    assert!(!crate::session_timeline::lapse_supervisor(
        &h.ctx,
        expiry - 1
    ));
    assert!(h.ctx.meta.lock().unwrap().supervisor.is_some());
    assert!(crate::session_timeline::lapse_supervisor(&h.ctx, expiry));
    assert!(h.ctx.meta.lock().unwrap().supervisor.is_none());

    // NEGATIVE HALF: no other claim has a grace.
    claim(13, SCOPED);
    release_connection_claims(&store, None, 13);
    assert!(
        h.ctx.meta.lock().unwrap().supervisor.is_none(),
        "a contract 3 claim ends with its connection"
    );
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor_as(
        &h.ctx,
        "codex-alab@4242",
        Some(14),
        None,
        Some(crate::self_claim::stand_in::running()),
        now,
        aterm_slog::Actor::Ctl,
    )
    .expect("a proven self-claim");
    release_connection_claims(&store, None, 14);
    assert!(
        h.ctx.meta.lock().unwrap().supervisor.is_none(),
        "a contract 2 self-claim ends with its connection"
    );
    assert_eq!(
        crate::session_timeline::claim_supervisor(&h.ctx, "watcher", Some(15), None, now),
        Ok(true)
    );
    release_connection_claims(&store, None, 15);
    assert!(
        h.ctx.meta.lock().unwrap().supervisor.is_none(),
        "an unproven claim ends with its connection"
    );
}
