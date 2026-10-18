// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// How long a test waits for something that must happen (a reader thread's reply,
/// a worker releasing its lane, a proxy handshake). A hang detector, not a latency
/// budget: a passing test waits only as long as the event takes, and short bounds
/// fail under heavy machine load. The quarantine reaper has its own
/// (`REAPER_PATIENCE`); grace intervals stay as the tests set them.
const EVENTUALLY: std::time::Duration = std::time::Duration::from_secs(60);

/// A pid alive on every host and never this process: 1 (init/launchd) on Unix, 4
/// (System) on Windows, where pid 1 is not a process
/// (`identity_claim::tests::LIVE_FOREIGN_PID`).
const LIVE_FOREIGN_PID: u32 = if cfg!(windows) { 4 } else { 1 };

/// A discovery entry a live foreign instance would have written.
fn live_foreign_entry() -> String {
    format!("sock /nonexistent/aterm.sock\nnonce ab\npid {LIVE_FOREIGN_PID}\n")
}

/// The ambiguity refusal belongs to the resolver, so seams that skip `handle`
/// cannot skip it: `post` with a binary body, `feed-bin`/`paste-bin` at the header
/// fence and the effect boundary, and `subscribe`'s target list all resolve without
/// `handle`, and they write input and stream output. The gate lives in
/// [`resolve_explicit`], whose `Result` no caller can ignore, with the bare lookup
/// nested inside it.
///
/// This pins the resolver's three answers; the type checker ensures every seam
/// is bound by them.
#[test]
fn an_ambiguous_id_is_refused_by_the_resolver_every_seam_shares() {
    use super::{Selector, resolve_explicit};
    use aterm_session::{LaunchNonce, SessionId};

    let _guard = crate::identity_claim::rendezvous_test_guard();
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    crate::identity_claim::set_rendezvous_override(Some(dir.path().to_path_buf()));

    // One session this instance really hosts, under an id it ADOPTED —
    // the premint population, the only one that can be doubly held.
    let store = crate::session_store::new_store();
    let mut handle = crate::session_store::test_handle(1);
    let sid = SessionId::generate();
    handle.sid = sid.clone();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    crate::identity_claim::note_adopted(&sid);
    let sel = Selector::Sid(sid.clone());

    // Our own entry, nobody else's: the ordinary cross-session address.
    crate::proxy::write_graph_entry(
        dir.path(),
        &sid,
        "/nonexistent/aterm.sock",
        &LaunchNonce::generate(),
    );
    assert!(
        resolve_explicit(&store, &sel)
            .expect("an id only this instance serves resolves")
            .is_some(),
        "the gate must not refuse the ids this instance published"
    );

    // A live foreign instance publishing the SAME id (`LIVE_FOREIGN_PID`:
    // alive on every host, never us).
    std::fs::write(
        dir.path().join("graph").join(sid.as_str()),
        live_foreign_entry(),
    )
    .expect("plant a foreign entry");
    let Err(refusal) = resolve_explicit(&store, &sel) else {
        panic!("an id two live instances serve names two places, and is refused");
    };
    assert!(
        refusal.starts_with("ERR ambiguous session id ")
            && refusal.contains(&format!("pid {LIVE_FOREIGN_PID}")),
        "the refusal must name the pid to address instead: {refusal:?}"
    );

    // A miss is still a miss: an id this instance does not host earns
    // `ERR no such session` even over a live foreign entry, or a planted entry
    // would turn every unknown id into an ambiguity.
    let stranger = SessionId::generate();
    crate::identity_claim::note_adopted(&stranger);
    std::fs::write(
        dir.path().join("graph").join(stranger.as_str()),
        live_foreign_entry(),
    )
    .expect("plant a foreign entry");
    assert!(
        resolve_explicit(&store, &Selector::Sid(stranger))
            .expect("an id we do not host is a miss, not an ambiguity")
            .is_none(),
    );
}

/// An id handed to this process's own update candidate is served here until the
/// Commit: the successor's per-process socket publishes every carried id before
/// the predecessor commits, and in between the predecessor must still answer for
/// them rather than refuse them as ambiguous.
///
/// Tier-1 binding for `HandoffAddressOwner`: the real dispatch probe and resolver,
/// the state projected onto the model
/// (`identity_claim::project_handoff_address_owner`) and the model's `Request` fired
/// on it; the real refusal must equal the model's `refused` in every state: the live
/// pre-Commit shape (served), the same shape before the lane registered its
/// candidate (refused, as the `Buggy` model does too), a stranger mid-update
/// (refused), the registration dropped with the candidate up (refused), and the
/// candidate dead (nothing holds the id). A minted id is never contestable.
#[cfg(unix)]
#[test]
fn an_id_handed_to_this_processs_update_candidate_is_served_here_until_commit() {
    use super::{Selector, ambiguous_sid_refusal, resolve_explicit};
    use crate::identity_claim::project_handoff_address_owner as project;
    use aterm_session::SessionId;

    let _guard = crate::identity_claim::rendezvous_test_guard();
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    crate::identity_claim::set_rendezvous_override(Some(dir.path().to_path_buf()));
    let model = aterm_models_session::handoff_address_owner_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);

    // A session this process hosts under an adopted id (it survived one handoff).
    let store = crate::session_store::new_store();
    let mut handle = crate::session_store::test_handle(1);
    let sid = SessionId::generate();
    handle.sid = sid.clone();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(handle);
    crate::identity_claim::note_adopted(&sid);
    let sel = Selector::Sid(sid.clone());

    // The candidate: a live process that is not us.
    let mut candidate = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("30")
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("spawn the candidate stand-in");
    let cand = candidate.id();
    std::fs::create_dir_all(dir.path().join("graph")).expect("graph dir");
    let entry = |pid: u32| {
        std::fs::write(
            dir.path().join("graph").join(sid.as_str()),
            format!("sock /nonexistent/aterm-{pid}.sock\nnonce ab\npid {pid}\n"),
        )
        .expect("plant the entry");
    };
    // The real verdict beside the model's, at the state the real code is in.
    let conforms = |m: &aterm_spec::derive::Model, why: &str| -> Option<String> {
        let mut st = project(dir.path(), &sid, cand);
        assert!(m.fire("Request", &mut st), "{why}: Request at {st:?}");
        let refusal = ambiguous_sid_refusal(&sel);
        let resolved = resolve_explicit(&store, &sel);
        assert_eq!(
            refusal.is_some(),
            resolved.is_err(),
            "{why}: the resolver and the dispatch seam agree"
        );
        assert_eq!(
            i64::from(refusal.is_some()),
            st["refused"],
            "{why}: the real refusal {refusal:?} against the model at {st:?}"
        );
        refusal
    };

    // The pre-Commit shape without the lane's registration: the candidate's entry
    // alone is refused, which is also the `Buggy` model's verdict.
    entry(cand);
    let refusal = conforms(&model, "unregistered").expect("the pre-fix refusal");
    assert_eq!(
        refusal,
        format!(
            "ERR ambiguous session id {} also served by pid {cand}; use aterm ctl --pid \
             {cand}\n",
            sid.as_str()
        ),
        "the live line"
    );
    let mut path = buggy.init_state();
    for action in ["Launch", "SuccessorPublishes", "Request"] {
        assert!(buggy.fire(action, &mut path), "{action} at {path:?}");
    }
    assert_eq!(path["refused"], 1, "the Buggy model refuses the same path");

    // THE FIX: the lane registered its candidate — served, by the dispatch
    // probe and by the resolver every seam shares.
    let reg = crate::identity_claim::register_handoff_candidate(cand);
    assert!(conforms(&model, "registered").is_none());
    assert!(
        resolve_explicit(&store, &sel)
            .expect("served here until the Commit")
            .is_some()
    );

    // Negative control: a stranger publishing the same id mid-update.
    entry(LIVE_FOREIGN_PID);
    let refusal = conforms(&model, "stranger").expect("a stranger is refused");
    assert!(
        refusal.contains(&format!("pid {LIVE_FOREIGN_PID}")),
        "{refusal:?}"
    );

    // Negative control: the registration dropped, the candidate still up.
    drop(reg);
    entry(cand);
    assert!(conforms(&model, "dropped").is_some(), "refused again");

    // A MINTED id is never contestable, whatever its entry says.
    let minted = SessionId::generate();
    std::fs::write(
        dir.path().join("graph").join(minted.as_str()),
        format!("sock /nonexistent/aterm.sock\nnonce ab\npid {cand}\n"),
    )
    .expect("plant the entry");
    assert!(ambiguous_sid_refusal(&Selector::Sid(minted)).is_none());

    // The candidate dead (a rejected attempt's reap): its entry holds nothing.
    let _ = candidate.kill();
    let _ = candidate.wait();
    assert!(conforms(&model, "dead").is_none());
    crate::identity_claim::set_rendezvous_override(None);
}

/// The routing law for an explicit selector: `@self` expands client-side to
/// `@<sid>`, and when it names the tab on screen it must route through the App
/// input seam (front), not straight to the sink, or it skips that seam's side
/// effects (the typing-momentum hints the cursor effects read, and the
/// `video ... keys` ledger).
#[test]
fn an_explicit_selector_for_the_front_tab_is_routed_through_the_input_seam() {
    use super::{TurnInputRoute, front_routed, license_clear_reply, turn_input_route};
    // Named the tab on screen -> the seam, whoever asks.
    for owner_class in [true, false] {
        assert!(front_routed(true, owner_class, Some(7), 7));
        // Named a BACKGROUND tab -> the direct egress it exists for.
        assert!(!front_routed(true, owner_class, Some(7), 9));
        // No front tab at all (headless with no window, every tab closed) ->
        // nothing to be the front of; fail to the background path.
        assert!(!front_routed(true, owner_class, None, 7));
    }
    // An OWNER's FLAGLESS request keeps the follow-the-front post, whatever
    // the front tab happens to be.
    assert!(!front_routed(false, true, Some(7), 7));
    assert!(!front_routed(false, true, None, 7));
    // A SCOPED caller's flagless input is PINNED to the session its edge was
    // checked against: a tab switch, window focus change or overlay opened
    // between the control-thread check and the main-thread delivery must not
    // re-resolve it into a session (or the Owner-only palette) the edge does
    // not cover.
    assert!(front_routed(false, false, Some(7), 7));
    assert!(front_routed(false, false, Some(9), 7));

    // `turn` is composite paste + Return, but both phases must make the
    // identical routing decision. In particular, explicit `@self` (which
    // arrives as a cross selector naming the visible sid) is FRONT, as is a
    // scoped caller's flagless turn, while a genuinely hidden target remains
    // on the background sink and only the Owner's flagless turn follows the
    // front.
    assert_eq!(turn_input_route(true, true), TurnInputRoute::Front);
    assert_eq!(turn_input_route(true, false), TurnInputRoute::Background);
    assert_eq!(turn_input_route(false, false), TurnInputRoute::Local);
    assert_eq!(turn_input_route(false, true), TurnInputRoute::Front);

    // The reply tells transport failure from a harmless race: a `false` main-thread
    // result means the tab switched before the fence, so the target is now
    // background and its valid signal must not be dropped.
    assert_eq!(license_clear_reply(Ok(true)), "OK\n");
    assert_eq!(license_clear_reply(Ok(false)), "OK\n");
    assert!(
        license_clear_reply(Err(super::control_media::MainHopError::Reason(
            "event loop gone"
        )))
        .starts_with("ERR ")
    );

    // A scoped caller's front-surface hop: a stalled main thread's refusal reaches
    // the wire as the retryable stall line, like every main-thread verb, never as a
    // denial. Any other failed hop is denied (fail closed).
    let stall = crate::watchdog::MainStall {
        stalled: std::time::Duration::from_secs(6),
        root: crate::watchdog::Breadcrumb::AboutToWait,
        returned: true,
    };
    assert_eq!(
        front_surface_authority(
            "key",
            Err(super::control_media::MainHopError::Stalled(stall))
        ),
        Err("ERR main thread stalled 6s; retry\n".to_string())
    );
    assert_eq!(
        front_surface_authority(
            "key",
            Err(super::control_media::MainHopError::Reason(
                "event loop gone"
            ))
        ),
        Err("ERR denied\n".to_string())
    );
    assert_eq!(
        front_surface_authority("send", Ok(FrontControlSurface::None)),
        Ok(FrontControlSurface::None)
    );
}

struct WireProbe {
    alive: Arc<AtomicBool>,
    prepared: Arc<AtomicBool>,
    fail_prepare: bool,
}

impl WireRetention for WireProbe {
    fn prepare_write(&mut self) -> Result<(), String> {
        self.prepared.store(true, Ordering::Release);
        if self.fail_prepare {
            Err("injected identity replacement".to_string())
        } else {
            Ok(())
        }
    }
}

impl Drop for WireProbe {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Release);
    }
}

#[test]
fn artifact_handoff_permits_bound_and_release_capacity() {
    const CHARGE: usize = 2;
    const DESCRIPTOR_LIMIT: usize = 6;
    static USAGE: Mutex<ArtifactHandoffUsage> = Mutex::new(ArtifactHandoffUsage {
        live: 0,
        descriptor_units: 0,
    });
    let model = aterm_models_misc::artifact_handoff_capacity_model();
    type CapacityState = (usize, usize, [usize; 3], usize);
    let project = |state: CapacityState| {
        crate::artifact_transaction_conformance::project_artifact_handoff_capacity(
            &model,
            crate::artifact_transaction_conformance::ArtifactHandoffCapacityObservation {
                live: i64::try_from(state.0).unwrap(),
                descriptor_units: i64::try_from(state.1).unwrap(),
                charge_one: i64::try_from(state.2[0]).unwrap(),
                charge_two: i64::try_from(state.2[1]).unwrap(),
                charge_three: i64::try_from(state.2[2]).unwrap(),
                selected: i64::try_from(state.3).unwrap(),
            },
        )
    };
    // Each call follows the shipping operation it names, so the after state's
    // `(live, descriptor_units)` is read off the real pool: the model judges what
    // `try_acquire_from`, a reconcile or a drop left. The charge buckets and
    // `selected` stay drive coordinates (the pool has no per-permit view).
    let validate = |action: &str, before: CapacityState, after: CapacityState| {
        {
            let usage = USAGE.lock().unwrap();
            assert_eq!(
                (usage.live, usage.descriptor_units),
                (after.0, after.1),
                "{action}: the pool the shipping operation left"
            );
        }
        let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &project(before),
            &project(after),
            Some(action),
            "artifact handoff capacity",
        );
        assert!(conforms, "{action} {before:?}->{after:?}: {evidence}");
    };
    let rejects = |action: &str, before: CapacityState, after: CapacityState, label: &str| {
        let (conforms, evidence) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            &project(before),
            &project(after),
            Some(action),
            label,
        );
        assert!(
            !conforms,
            "{label}: {before:?}->{after:?} conformed: {evidence}"
        );
    };

    let mut permits = Vec::new();
    let descriptor_slots = DESCRIPTOR_LIMIT / CHARGE;
    for before in 0..descriptor_slots {
        permits.push(
            ArtifactHandoffPermit::try_acquire_from(
                &USAGE,
                ARTIFACT_HANDOFF_LIMIT,
                DESCRIPTOR_LIMIT,
                CHARGE,
            )
            .unwrap(),
        );
        validate(
            "Acquire",
            (before, before * CHARGE, [0, before, 0], CHARGE),
            (
                before + 1,
                (before + 1) * CHARGE,
                [0, before + 1, 0],
                CHARGE,
            ),
        );
    }
    assert_eq!(
        *USAGE.lock().unwrap(),
        ArtifactHandoffUsage {
            live: descriptor_slots,
            descriptor_units: DESCRIPTOR_LIMIT,
        }
    );
    assert!(
        ArtifactHandoffPermit::try_acquire_from(
            &USAGE,
            ARTIFACT_HANDOFF_LIMIT,
            DESCRIPTOR_LIMIT,
            CHARGE,
        )
        .is_none()
    );
    validate(
        "RefuseAtCap",
        (
            descriptor_slots,
            DESCRIPTOR_LIMIT,
            [0, descriptor_slots, 0],
            CHARGE,
        ),
        (
            descriptor_slots,
            DESCRIPTOR_LIMIT,
            [0, descriptor_slots, 0],
            CHARGE,
        ),
    );

    rejects(
        "Acquire",
        (
            descriptor_slots,
            DESCRIPTOR_LIMIT,
            [0, descriptor_slots, 0],
            CHARGE,
        ),
        (
            descriptor_slots + 1,
            DESCRIPTOR_LIMIT + CHARGE,
            [0, descriptor_slots + 1, 0],
            CHARGE,
        ),
        "artifact handoff overbook negative control",
    );
    // A refusal that took the count slot before its unit check and never returned
    // it. The shipping refusal leaves the pool untouched; a leaked pool is not a
    // RefuseAtCap.
    rejects(
        "RefuseAtCap",
        (
            descriptor_slots,
            DESCRIPTOR_LIMIT,
            [0, descriptor_slots, 0],
            CHARGE,
        ),
        (
            descriptor_slots + 1,
            DESCRIPTOR_LIMIT,
            [0, descriptor_slots, 0],
            CHARGE,
        ),
        "artifact handoff leaked-refusal negative control",
    );

    while let Some(permit) = permits.pop() {
        let before = {
            let usage = USAGE.lock().unwrap();
            (usage.live, usage.descriptor_units)
        };
        drop(permit);
        validate(
            "Release",
            (before.0, before.1, [0, before.0, 0], CHARGE),
            (
                before.0 - 1,
                before.1 - CHARGE,
                [0, before.0 - 1, 0],
                CHARGE,
            ),
        );
    }
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());

    let mut reconciled = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("provisional charge fits");
    validate(
        "Acquire",
        (0, 0, [0, 0, 0], CHARGE),
        (1, CHARGE, [0, 1, 0], CHARGE),
    );
    assert!(reconciled.try_reconcile_descriptor_units(CHARGE + 1));
    validate(
        "ReconcileGrow",
        (1, CHARGE, [0, 1, 0], CHARGE),
        (1, CHARGE + 1, [0, 0, 1], CHARGE + 1),
    );
    drop(reconciled);
    validate(
        "Release",
        (1, CHARGE + 1, [0, 0, 1], CHARGE + 1),
        (0, 0, [0, 0, 0], CHARGE + 1),
    );
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());
    // The same drop returning the provisional charge — a reconcile that
    // never recorded the grown charge on the permit — strands one unit.
    rejects(
        "Release",
        (1, CHARGE + 1, [0, 0, 1], CHARGE + 1),
        (0, 1, [0, 0, 0], CHARGE + 1),
        "artifact handoff provisional-release negative control",
    );

    let mut reconciled = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("second provisional charge fits");
    assert!(reconciled.try_reconcile_descriptor_units(CHARGE - 1));
    validate(
        "ReconcileShrink",
        (1, CHARGE, [0, 1, 0], CHARGE),
        (1, CHARGE - 1, [1, 0, 0], CHARGE - 1),
    );
    drop(reconciled);
    validate(
        "Release",
        (1, CHARGE - 1, [1, 0, 0], CHARGE - 1),
        (0, 0, [0, 0, 0], CHARGE - 1),
    );
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());

    let in_memory = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("byte reply provisional charge fits");
    struct AccountedCapabilityProbe {
        usage: &'static Mutex<ArtifactHandoffUsage>,
        expected_units: usize,
    }

    impl Drop for AccountedCapabilityProbe {
        fn drop(&mut self) {
            assert_eq!(
                self.usage.lock().unwrap().descriptor_units,
                self.expected_units,
                "the accounted capability must close before its charge shrinks"
            );
        }
    }

    let retention = ReplyRetentionPermit(in_memory).release_path_and_retain_in_memory(
        AccountedCapabilityProbe {
            usage: &USAGE,
            expected_units: CHARGE,
        },
    );
    assert_eq!(
        *USAGE.lock().unwrap(),
        ArtifactHandoffUsage {
            live: 1,
            descriptor_units: 1,
        },
        "dropping the path capability releases its descriptor charge"
    );
    validate(
        "ReconcileShrink",
        (1, CHARGE, [0, 1, 0], CHARGE),
        (1, 1, [1, 0, 0], 1),
    );
    drop(retention);
    validate("Release", (1, 1, [1, 0, 0], 1), (0, 0, [0, 0, 0], 1));
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());

    let mut grown = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("mixed-charge first permit fits");
    assert!(grown.try_reconcile_descriptor_units(CHARGE + 1));
    let ordinary = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("mixed-charge second permit fits");
    validate("Acquire", (1, 3, [0, 0, 1], 3), (2, 5, [0, 1, 1], 2));
    drop(ordinary);
    validate("Release", (2, 5, [0, 1, 1], 2), (1, 3, [0, 0, 1], 2));
    validate("SelectThree", (1, 3, [0, 0, 1], 2), (1, 3, [0, 0, 1], 3));
    drop(grown);
    validate("Release", (1, 3, [0, 0, 1], 3), (0, 0, [0, 0, 0], 3));
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());

    let mut small = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("far-refusal selected permit fits");
    assert!(small.try_reconcile_descriptor_units(1));
    let ordinary_a = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("far-refusal first competing permit fits");
    let ordinary_b = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        CHARGE,
    )
    .expect("far-refusal second competing permit fits");
    assert_eq!(
        *USAGE.lock().unwrap(),
        ArtifactHandoffUsage {
            live: 3,
            descriptor_units: 5,
        }
    );
    assert!(
        !small.try_reconcile_descriptor_units(3),
        "a direct one-to-three jump cannot consume two units of one-unit headroom"
    );
    assert_eq!(
        *USAGE.lock().unwrap(),
        ArtifactHandoffUsage {
            live: 3,
            descriptor_units: 5,
        },
        "failed far reconciliation leaves the original charge intact"
    );
    validate(
        "RefuseReconcile",
        (3, 5, [1, 2, 0], 1),
        (3, 5, [1, 2, 0], 1),
    );
    drop(small);
    validate("Release", (3, 5, [1, 2, 0], 1), (2, 4, [0, 2, 0], 1));
    validate("SelectTwo", (2, 4, [0, 2, 0], 1), (2, 4, [0, 2, 0], 2));
    drop(ordinary_a);
    validate("Release", (2, 4, [0, 2, 0], 2), (1, 2, [0, 1, 0], 2));
    drop(ordinary_b);
    validate("Release", (1, 2, [0, 1, 0], 2), (0, 0, [0, 0, 0], 2));
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());

    let mut count_permits = Vec::new();
    for _ in 0..ARTIFACT_HANDOFF_LIMIT {
        count_permits.push(
            ArtifactHandoffPermit::try_acquire_from(&USAGE, ARTIFACT_HANDOFF_LIMIT, usize::MAX, 1)
                .expect("count slot remains available"),
        );
    }
    assert!(
        ArtifactHandoffPermit::try_acquire_from(&USAGE, ARTIFACT_HANDOFF_LIMIT, usize::MAX, 1,)
            .is_none(),
        "object count refuses work independently of descriptor capacity"
    );
    drop(count_permits);
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());

    // Partial headroom: a charge past what is left of the descriptor budget is
    // refused before the object-count cap, and a single charge past the whole
    // budget is refused outright (the headroom subtraction cannot underflow).
    let deep = ArtifactHandoffPermit::try_acquire_from(
        &USAGE,
        ARTIFACT_HANDOFF_LIMIT,
        DESCRIPTOR_LIMIT,
        DESCRIPTOR_LIMIT - 2,
    )
    .expect("one deep request fits the descriptor budget");
    assert!(
        ArtifactHandoffPermit::try_acquire_from(
            &USAGE,
            ARTIFACT_HANDOFF_LIMIT,
            DESCRIPTOR_LIMIT,
            3,
        )
        .is_none(),
        "descriptor saturation refuses work before the object-count cap"
    );
    drop(deep);
    assert!(
        ArtifactHandoffPermit::try_acquire_from(
            &USAGE,
            ARTIFACT_HANDOFF_LIMIT,
            DESCRIPTOR_LIMIT,
            DESCRIPTOR_LIMIT + 1,
        )
        .is_none(),
        "a single over-budget descriptor charge must be refused"
    );
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());
}

#[test]
fn video_retention_capacity_is_reserved_and_returned_exactly() {
    static USAGE: Mutex<VideoRetentionUsage> = Mutex::new(VideoRetentionUsage {
        live: 0,
        descriptor_units: 0,
    });

    let first = ReplyRetention::try_reserve_video_retention_from(&USAGE, 1, 8, 4)
        .expect("first distinct video reserves its future retention sweep");
    assert_eq!(
        *USAGE.lock().unwrap(),
        VideoRetentionUsage {
            live: 1,
            descriptor_units: 4,
        }
    );
    assert!(
        ReplyRetention::try_reserve_video_retention_from(&USAGE, 1, 8, 1).is_none(),
        "the distinct-video count bound is independent of GC queue pressure"
    );
    assert!(
        ReplyRetention::try_reserve_video_retention_from(&USAGE, 2, 8, 5).is_none(),
        "the descriptor bound is enforced independently of count"
    );

    drop(first);
    assert_eq!(*USAGE.lock().unwrap(), VideoRetentionUsage::default());
    let reopened = ReplyRetention::try_reserve_video_retention_from(&USAGE, 1, 8, 8)
        .expect("dropping the sweep returns both accounting dimensions");
    drop(reopened);
    assert_eq!(*USAGE.lock().unwrap(), VideoRetentionUsage::default());
}

/// How long a test waits for the quarantine reaper to drop a guard whose
/// quarantine has already run out. A wait ends the moment the guard goes, so
/// this bound only decides how long a reaper that never runs takes to fail
/// the test: the house hang detector. The reaper runs at `Background`
/// ([`super::ARTIFACT_QUARANTINE_REAPER_ROLE`]), and each wait also drives
/// its sweep from the test thread (`sweep_artifact_quarantine_for_test`): at
/// load 100 (2026-09-30) a `Housekeeping` reaper ran three tests out of 60 s.
const REAPER_PATIENCE: std::time::Duration = EVENTUALLY;

/// A quarantine a test holds for as long as it looks at it: a day, which no
/// test run lasts, so the guard IS in quarantine whenever the test reads it,
/// however late the machine schedules the test thread, and [`end_quarantine`]
/// ends it when the test says so. A grace of 40 ms, 200 ms or 1 s raced the
/// reaper instead: a test thread descheduled past the grace between the
/// failed ACK and its next look found the guard already reaped, and went red
/// ("transfers the exact guard to quarantine") with the product right — or,
/// where a test had grown an either/or branch that accepted the guard gone,
/// passed without looking at the quarantine at all.
const HELD_QUARANTINE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// The ticket of the guard this thread put in quarantine last.
fn last_quarantine_ticket() -> u64 {
    super::LAST_QUARANTINE_TICKET
        .with(std::cell::Cell::take)
        .expect("this thread quarantined a guard")
}

/// End quarantine `ticket`'s grace now and ring the reaper: the guard goes at
/// the next sweep, the reaper's or a test's own
/// (`sweep_artifact_quarantine_for_test`) — the shipping expiry path, only the
/// deadline moved. Every other test's entry keeps its own.
fn end_quarantine(ticket: u64) {
    let quarantine = artifact_reply_quarantine();
    let mut entries = quarantine
        .entries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    entries
        .iter_mut()
        .find(|entry| entry.ticket == ticket)
        .expect("the quarantine is still held")
        .until = std::time::Instant::now();
    quarantine.changed.notify_one();
}

/// Drive the shipping reservation through a real failed ACK. The permit
/// must remain charged while its guard is in the process-global quarantine,
/// and return only when the quarantine's expiry sweep drops that guard.
///
/// The sweep runs on THIS thread, at an instant past the quarantine, so the
/// test waits on no clock and no scheduler: this isolated child initialises
/// the global quarantine itself, before any failed ACK can, and so no reaper
/// thread ever exists in it. The reaper's own wake-up is the subject of
/// `capture_reply_quarantines_conform_to_artifact_reply_publication`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn quarantined_reply_holds_production_handoff_capacity() {
    if !crate::control_auth::enter_low_nofile_test_child(96) {
        return;
    }
    let quarantine = *ARTIFACT_REPLY_QUARANTINE.get_or_init(ArtifactReplyQuarantine::leak);
    // Long enough that no run of this test outlives it: expiry is the
    // sweep's instant below, never elapsed time.
    let quarantine_for = std::time::Duration::from_secs(3600);
    let probe = || WireProbe {
        alive: Arc::new(AtomicBool::new(true)),
        prepared: Arc::new(AtomicBool::new(false)),
        fail_prepare: false,
    };

    let alive = Arc::new(AtomicBool::new(true));
    let retention = ReplyRetention::try_new(WireProbe {
        alive: Arc::clone(&alive),
        prepared: Arc::new(AtomicBool::new(false)),
        fail_prepare: false,
    })
    .unwrap_or_else(|_| panic!("first production permit is available"));
    let reply = ControlReply::guarded("OK guarded\n".to_string(), Some(retention));
    let mut wire = Vec::new();
    let pending = write_control_reply(&mut wire, reply)
        .expect("wire write")
        .expect("guarded reply");
    let (mut client, server) = CtlStream::pair().unwrap();
    client.write_all(b"ACK wrong\n").unwrap();
    client.flush().unwrap();
    let mut reader = BufReader::new(&server);
    assert_eq!(
        await_guarded_reply_close_with_quarantine(&server, &mut reader, pending, quarantine_for,),
        ArtifactAckOutcome::AcknowledgementQuarantined
    );
    assert!(alive.load(Ordering::Acquire));

    let mut admitted = Vec::new();
    for _ in 1..ARTIFACT_HANDOFF_LIMIT {
        admitted.push(
            ReplyRetention::try_new(probe())
                .unwrap_or_else(|_| panic!("capacity below the quarantined reply remains usable")),
        );
    }
    let refused_alive = Arc::new(AtomicBool::new(true));
    let refused = ReplyRetention::try_new(WireProbe {
        alive: Arc::clone(&refused_alive),
        prepared: Arc::new(AtomicBool::new(false)),
        fail_prepare: false,
    });
    assert!(
        refused.is_err(),
        "quarantine must consume its production permit"
    );
    drop(refused);
    assert!(!refused_alive.load(Ordering::Acquire));

    let sweep = |at: std::time::Instant| {
        let mut entries = quarantine
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ArtifactReplyQuarantine::take_expired(&mut entries, at)
    };
    assert!(
        sweep(std::time::Instant::now()).is_empty(),
        "a sweep inside the quarantine releases nothing"
    );
    assert!(alive.load(Ordering::Acquire), "the guard is still held");
    assert!(
        ReplyRetention::try_new(probe()).is_err(),
        "and so is its permit"
    );
    drop(admitted);

    let expired = sweep(std::time::Instant::now() + quarantine_for);
    assert_eq!(expired.len(), 1, "the expired reply leaves the quarantine");
    drop(expired);
    assert!(!alive.load(Ordering::Acquire), "its guard is released");
    let reopened: Vec<_> = (0..ARTIFACT_HANDOFF_LIMIT)
        .map(|_| {
            ReplyRetention::try_new(probe())
                .unwrap_or_else(|_| panic!("expired quarantine returns every permit"))
        })
        .collect();
    drop(reopened);
}

struct FailAfterFirstWrite {
    bytes: Vec<u8>,
}

impl Write for FailAfterFirstWrite {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.bytes.is_empty() {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "injected challenge write failure",
            ))
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn in_memory_handoff_releases_after_flush_without_ack_or_quarantine() {
    static USAGE: Mutex<ArtifactHandoffUsage> = Mutex::new(ArtifactHandoffUsage {
        live: 0,
        descriptor_units: 0,
    });

    struct UsageCheckingWriter {
        bytes: usize,
        fail_flush: bool,
    }

    impl Write for UsageCheckingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert_eq!(
                *USAGE.lock().unwrap(),
                ArtifactHandoffUsage {
                    live: 1,
                    descriptor_units: 1,
                },
                "the in-memory permit spans every body write"
            );
            assert!(
                !bytes.starts_with(
                    aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX.as_bytes()
                ),
                "a write-only reply emits no acknowledgement challenge"
            );
            self.bytes += bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            assert_eq!(
                *USAGE.lock().unwrap(),
                ArtifactHandoffUsage {
                    live: 1,
                    descriptor_units: 1,
                },
                "the in-memory permit spans the socket flush"
            );
            if self.fail_flush {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "injected write-only flush failure",
                ))
            } else {
                Ok(())
            }
        }
    }

    let make_reply = || {
        let permit = ArtifactHandoffPermit::try_acquire_from(&USAGE, 1, 1, 1)
            .expect("write-only handoff capacity");
        let retention = ReplyRetentionPermit(permit).release_path_and_retain_in_memory(());
        ControlReply::with_handoff(
            "x".repeat(1024 * 1024),
            Some(ReplyHandoff::WriteOnly(retention)),
        )
    };

    let mut writer = UsageCheckingWriter {
        bytes: 0,
        fail_flush: false,
    };
    assert!(
        write_control_reply(&mut writer, make_reply())
            .expect("write-only reply")
            .is_none(),
        "an in-memory reply has no pending ACK"
    );
    assert_eq!(writer.bytes, 1024 * 1024);
    assert_eq!(*USAGE.lock().unwrap(), ArtifactHandoffUsage::default());

    let mut failing = UsageCheckingWriter {
        bytes: 0,
        fail_flush: true,
    };
    let error = match write_control_reply(&mut failing, make_reply()) {
        Err(error) => error,
        Ok(_) => panic!("flush failure must propagate without quarantine"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    assert_eq!(
        *USAGE.lock().unwrap(),
        ArtifactHandoffUsage::default(),
        "a failed in-memory write releases immediately instead of quarantining"
    );
}

#[test]
fn in_memory_handoff_restores_timeout_before_next_persistent_reply() {
    struct TimeoutCheckingWriter {
        timeout: Arc<Mutex<Option<std::time::Duration>>>,
        expected_armed: bool,
        bytes: Vec<u8>,
    }

    impl Write for TimeoutCheckingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert_eq!(
                self.timeout.lock().unwrap().is_some(),
                self.expected_armed,
                "each reply must observe only its own timeout policy"
            );
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            assert_eq!(
                self.timeout.lock().unwrap().is_some(),
                self.expected_armed,
                "flush must observe the same timeout policy as body writes"
            );
            Ok(())
        }
    }

    let timeout = Arc::new(Mutex::new(None));
    let retention =
        ReplyRetentionPermit::unmetered_for_test().release_path_and_retain_in_memory(());
    let inline = ControlReply::with_handoff(
        "OK inline\n".to_string(),
        Some(ReplyHandoff::WriteOnly(retention)),
    );
    let mut inline_wire = TimeoutCheckingWriter {
        timeout: Arc::clone(&timeout),
        expected_armed: true,
        bytes: Vec::new(),
    };
    let setter_timeout = Arc::clone(&timeout);
    assert!(
        write_control_reply_with_timeout_arm(
            &mut inline_wire,
            inline,
            std::time::Duration::from_secs(1),
            |value| {
                *setter_timeout.lock().unwrap() = value;
                Ok(())
            },
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(inline_wire.bytes, b"OK inline\n");
    assert_eq!(
        *timeout.lock().unwrap(),
        None,
        "the write-only deadline must be cleared before keep-alive resumes"
    );

    let ordinary = ControlReply::with_handoff("OK next\n".to_string(), None);
    let mut ordinary_wire = TimeoutCheckingWriter {
        timeout: Arc::clone(&timeout),
        expected_armed: false,
        bytes: Vec::new(),
    };
    let setter_timeout = Arc::clone(&timeout);
    assert!(
        write_control_reply_with_timeout_arm(
            &mut ordinary_wire,
            ordinary,
            std::time::Duration::from_secs(1),
            |value| {
                *setter_timeout.lock().unwrap() = value;
                Ok(())
            },
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(ordinary_wire.bytes, b"OK next\n");
}

/// A reply carrying a [`ReplyWritten`] releases its [`ReplyWire`] once its bytes are
/// written and flushed (not before, and not only at a guarded reply's ACK), and on
/// a failed write too; a seamless update's Commit waits on this for a `video` reply.
/// A reply dropped unwritten releases it as well.
#[test]
fn a_reply_wire_releases_when_its_reply_is_written() {
    struct WireCheckingWriter {
        wire: ReplyWire,
        bytes: Vec<u8>,
        flushed: bool,
    }
    impl Write for WireCheckingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            assert!(!self.wire.is_released(), "released before the write");
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            assert!(!self.wire.is_released(), "released before the flush");
            self.flushed = true;
            Ok(())
        }
    }

    let (wire, written) = ReplyWire::new();
    let reply = ControlReply::from("ERR video: recording aborted: aterm is updating\n")
        .releasing_on_write(written);
    let mut writer = WireCheckingWriter {
        wire: wire.clone(),
        bytes: Vec::new(),
        flushed: false,
    };
    assert!(!wire.is_released(), "owed until written");
    assert!(write_control_reply(&mut writer, reply).unwrap().is_none());
    assert!(writer.flushed && wire.is_released(), "written, so released");
    assert!(wire.wait_released(std::time::Instant::now()));

    // A failed write releases it too: nothing more is coming.
    let (wire, written) = ReplyWire::new();
    let reply = ControlReply::from("OK a\n").releasing_on_write(written);
    let mut failing = FailAfterFirstWrite {
        bytes: b"x".to_vec(),
    };
    assert!(write_control_reply(&mut failing, reply).is_err());
    assert!(wire.is_released());

    // Through `handle`'s tail, as the `video` arm hands it on.
    let (wire, written) = ReplyWire::new();
    let mut tail = ReplyTail::default();
    let body = ControlReply::from("OK b\n")
        .releasing_on_write(written)
        .into_body_retaining(&mut tail);
    assert!(!wire.is_released(), "the tail keeps it owed");
    let mut bytes = Vec::new();
    assert!(
        write_control_reply(&mut bytes, ControlReply::with_tail(body, tail))
            .unwrap()
            .is_none()
    );
    assert_eq!(bytes, b"OK b\n");
    assert!(wire.is_released());

    // Dropped unwritten: released, and a waiter is not left hanging.
    let (wire, written) = ReplyWire::new();
    drop(ControlReply::from("OK c\n").releasing_on_write(written));
    assert!(wire.wait_released(std::time::Instant::now()));
    // CONTROL: an unreleased wire's wait ends at its deadline.
    let (wire, _written) = ReplyWire::new();
    assert!(!wire.wait_released(std::time::Instant::now() + std::time::Duration::from_millis(20)));
}

// ---- TIER-1: `ArtifactReplyPublication` over the shipping capture reply. ----
//
// One automatic image capture is driven through the real seams in order:
// `CaptureCancellation`'s CAS and the authorized final-name write, the encode
// worker's `send_capture_reply_after_validation`, this module's writer and ACK wait
// with its central quarantine. After each step the reply is read back: `artifact`
// from the file, `guard` from the name lease (held only by the capture guard once
// written), `committed`/`reply`/`challenge` from the writer's bytes, the ACK verdict
// from the wait; `phase` and `quarantine_age` are the drive's coordinates. Where
// one shipping call crosses two model steps (the writer prepares and writes; the
// reaper expires and releases) the intermediate is the model's own successor.

/// A fresh automatic capture target and the name lease the encode worker
/// takes before writing. Returns the socket dir to clean up.
fn capture_reply_target(
    label: &str,
) -> (
    std::path::PathBuf,
    crate::control_auth::ConfinedImage,
    crate::control_auth::ArtifactPathLease,
) {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sock = std::env::temp_dir().join(format!(
        "aterm-reply-bind-{label}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&sock);
    crate::control_auth::ensure_private_dir(&sock).unwrap();
    let target = crate::control_auth::confine_automatic_image_path(&sock, "image")
        .expect("automatic capture target");
    let lease = crate::control_auth::acquire_capture_name_lease(&target, || false)
        .expect("name lease acquisition")
        .expect("automatic name lease");
    (sock, target, lease)
}

/// The writer's output as the model reads it: `committed` once the OK body went out
/// (a failed preparation writes ERR instead), `challenge` once any nonce trailer
/// went out, `reply` only for the complete frame (body, then exactly its trailer).
fn reply_wire(wire: &[u8], body: &str) -> (bool, bool, bool) {
    let text = String::from_utf8_lossy(wire);
    let prefix = aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX;
    let challenge = text.lines().any(|line| line.starts_with(prefix));
    let complete = text.strip_prefix(body).is_some_and(|rest| {
        rest.strip_prefix(prefix)
            .and_then(|nonce| nonce.strip_suffix('\n'))
            .is_some_and(aterm_types::control_verbs::valid_artifact_ack_nonce)
    });
    (!body.is_empty() && text.contains(body), complete, challenge)
}

fn observe_capture_reply(
    model: &aterm_spec::derive::Model,
    artifact: &std::path::Path,
    lease_key: &std::path::Path,
    wire: &[u8],
    body: &str,
    drive: crate::artifact_transaction_conformance::ArtifactReplyObservation,
) -> aterm_spec::interp::State {
    let (committed, reply, challenge) = reply_wire(wire, body);
    crate::artifact_transaction_conformance::project_artifact_reply(
        model,
        crate::artifact_transaction_conformance::ArtifactReplyObservation {
            artifact: artifact.exists(),
            guard: crate::control_auth::artifact_lease_registry_for_test(lease_key).is_some(),
            committed,
            reply,
            challenge,
            ..drive
        },
    )
}

fn capture_reply_observation(
    phase: i64,
) -> crate::artifact_transaction_conformance::ArtifactReplyObservation {
    crate::artifact_transaction_conformance::ArtifactReplyObservation {
        phase,
        ..Default::default()
    }
}

fn reply_successor(
    model: &aterm_spec::derive::Model,
    action: &str,
    before: &aterm_spec::interp::State,
) -> aterm_spec::interp::State {
    let successors = model.successors(action, before);
    assert_eq!(
        successors.len(),
        1,
        "{action} is deterministic at {before:?}"
    );
    successors[0].clone()
}

#[test]
fn capture_reply_ack_path_conforms_to_artifact_reply_publication() {
    use crate::artifact_transaction_conformance::{assert_transition, reject_transition};
    let model = aterm_models_misc::artifact_reply_publication_model();
    let (sock, target, lease) = capture_reply_target("ack");
    let path = target.display_path();
    let body = format!("OK 1 1 {}\n", path.display());
    let observe = |wire: &[u8], phase: i64| {
        observe_capture_reply(
            &model,
            &path,
            &path,
            wire,
            &body,
            capture_reply_observation(phase),
        )
    };
    let idle = model.init_state();

    let cancel = CaptureCancellation::new();
    let file = target
        .write_private_authorized(b"png", || cancel.authorize_commit())
        .expect("authorized final-name write");
    let authorized = observe(b"", 2);
    assert_transition(
        &model,
        "AuthorizeCommit",
        &idle,
        &authorized,
        "capture wins the CAS",
    );

    let retained = crate::app_introspect::queue_capture_reply_for_test(target, file, Some(lease))
        .expect("validated capture queues its guard");
    let queued = observe(b"", 3);
    assert_transition(
        &model,
        "QueueGuard",
        &authorized,
        &queued,
        "the worker hands the guard over",
    );
    let dropped_before_write = project_artifact_reply_with(&model, &queued, |o| o.guard = false);
    reject_transition(
        &model,
        "QueueGuard",
        &authorized,
        &dropped_before_write,
        "queue handoff cannot drop the exact artifact guard",
    );

    // A sibling capture's retention sweep over a namespace past its keep
    // limit skips the queued artifact: its lease protects it.
    let sibling = crate::control_auth::confine_automatic_image_path(&sock, "image")
        .expect("sibling capture target");
    let namespace = path.parent().unwrap().to_path_buf();
    for _ in 0..(crate::control_auth::AUTO_IMAGE_KEEP + 4) {
        let newer = crate::control_auth::automatic_capture_name("image");
        std::fs::write(namespace.join(newer), b"newer").unwrap();
    }
    let before_sweep = std::fs::read_dir(&namespace).unwrap().count();
    crate::control_auth::prune_automatic_images_for_test(&sibling);
    assert!(
        std::fs::read_dir(&namespace).unwrap().count() < before_sweep,
        "the sibling sweep ran"
    );
    let swept = observe(b"", 3);
    assert_transition(
        &model,
        "RetentionSweep",
        &queued,
        &swept,
        "retention skips the leased artifact",
    );
    let pruned = project_artifact_reply_with(&model, &queued, |o| o.artifact = false);
    reject_transition(
        &model,
        "RetentionSweep",
        &queued,
        &pruned,
        "retention cannot prune a queued artifact lease",
    );

    let (_, handoff) = retained.into_parts();
    let mut wire = Vec::new();
    let pending = write_control_reply(&mut wire, ControlReply::with_handoff(body.clone(), handoff))
        .expect("the complete frame is written")
        .expect("a guarded reply awaits its ACK");
    let prepared = reply_successor(&model, "PrepareWire", &swept);
    let written = observe(&wire, 5);
    assert_transition(
        &model,
        "WriteWire",
        &prepared,
        &written,
        "body, then the nonce trailer, flushed",
    );

    // The trailer ahead of the body: an echo of it would not prove the client
    // read the complete frame.
    let text = String::from_utf8(wire.clone()).unwrap();
    let (sent_body, trailer) = text.split_at(body.len());
    let early = format!("{trailer}{sent_body}");
    let early_challenge = observe(early.as_bytes(), 4);
    reject_transition(
        &model,
        "WriteWire",
        &prepared,
        &early_challenge,
        "the nonce challenge follows the complete reply frame",
    );
    assert!(!model.check_invariant("ChallengeRequiresCompleteWire", &early_challenge));
    let ack_before_challenge = project_artifact_reply_with(&model, &prepared, |o| {
        o.phase = 6;
        o.ack = true;
    });
    reject_transition(
        &model,
        "AcknowledgePeer",
        &prepared,
        &ack_before_challenge,
        "a pre-pipelined acknowledgement cannot precede the causal nonce challenge",
    );

    let ack = format!(
        "{}{nonce}\n",
        aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX,
        nonce = pending.nonce.as_str()
    );
    let (mut client, server) = CtlStream::pair().unwrap();
    client.write_all(ack.as_bytes()).unwrap();
    client.flush().unwrap();
    let mut reader = BufReader::new(&server);
    assert_eq!(
        await_guarded_reply_close_with_quarantine(
            &server,
            &mut reader,
            pending,
            std::time::Duration::from_millis(20),
        ),
        ArtifactAckOutcome::PeerAcknowledged
    );
    let acknowledged = reply_successor(&model, "AcknowledgePeer", &written);
    let released = observe_capture_reply(
        &model,
        &path,
        &path,
        &wire,
        &body,
        crate::artifact_transaction_conformance::ArtifactReplyObservation {
            phase: 10,
            ack: true,
            ..Default::default()
        },
    );
    assert_transition(
        &model,
        "ReleaseGuard",
        &acknowledged,
        &released,
        "the matching nonce ACK releases the guard and keeps the published file",
    );
    let silent_release = project_artifact_reply_with(&model, &released, |o| o.ack = false);
    reject_transition(
        &model,
        "ReleaseGuard",
        &written,
        &silent_release,
        "a complete reply cannot release without the matching nonce ACK",
    );

    let _ = std::fs::remove_dir_all(sock);
}

#[test]
fn capture_reply_quarantines_conform_to_artifact_reply_publication() {
    use crate::artifact_transaction_conformance::{assert_transition, reject_transition};
    let model = aterm_models_misc::artifact_reply_publication_model();

    // A failed ACK: the guard waits out the central quarantine, then goes.
    let (sock, target, lease) = capture_reply_target("ack-failed");
    let path = target.display_path();
    let body = format!("OK 1 1 {}\n", path.display());
    let cancel = CaptureCancellation::new();
    let file = target
        .write_private_authorized(b"png", || cancel.authorize_commit())
        .unwrap();
    let (_, handoff) =
        crate::app_introspect::queue_capture_reply_for_test(target, file, Some(lease))
            .unwrap()
            .into_parts();
    let queued = observe_capture_reply(
        &model,
        &path,
        &path,
        b"",
        &body,
        capture_reply_observation(3),
    );
    let mut wire = Vec::new();
    let pending = write_control_reply(&mut wire, ControlReply::with_handoff(body.clone(), handoff))
        .unwrap()
        .expect("guarded reply");
    let prepared = reply_successor(&model, "PrepareWire", &queued);
    let written = observe_capture_reply(
        &model,
        &path,
        &path,
        &wire,
        &body,
        capture_reply_observation(5),
    );
    assert_transition(
        &model,
        "WriteWire",
        &prepared,
        &written,
        "the complete frame is out",
    );
    let (mut client, server) = CtlStream::pair().unwrap();
    client.write_all(b"ACK wrong\n").unwrap();
    client.flush().unwrap();
    let mut reader = BufReader::new(&server);
    assert_eq!(
        await_guarded_reply_close_with_quarantine(&server, &mut reader, pending, HELD_QUARANTINE),
        ArtifactAckOutcome::AcknowledgementQuarantined
    );
    let ticket = last_quarantine_ticket();
    // The guard is in quarantine whenever this thread gets to look: its grace
    // ends only when this test ends it ([`HELD_QUARANTINE`]).
    let quarantined_drive = crate::artifact_transaction_conformance::ArtifactReplyObservation {
        phase: 7,
        ack_failed: true,
        ..Default::default()
    };
    let ack_failed = reply_successor(&model, "AcknowledgeFailed", &written);
    let quarantined = observe_capture_reply(&model, &path, &path, &wire, &body, quarantined_drive);
    assert_eq!(
        quarantined, ack_failed,
        "a failed ACK transfers the guard to quarantine"
    );
    let early_release = project_artifact_reply_with(&model, &ack_failed, |o| {
        o.phase = 11;
        o.guard = false;
    });
    reject_transition(
        &model,
        "ReleaseGuard",
        &ack_failed,
        &early_release,
        "failed or half-closed clients retain the guard for the full quarantine",
    );
    end_quarantine(ticket);
    let deadline = std::time::Instant::now() + REAPER_PATIENCE;
    while crate::control_auth::artifact_lease_registry_for_test(&path).is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the quarantine must expire"
        );
        sweep_artifact_quarantine_for_test();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let aged = reply_successor(&model, "AdvanceQuarantine", &ack_failed);
    let expired = reply_successor(&model, "ExpireQuarantine", &aged);
    let released = observe_capture_reply(
        &model,
        &path,
        &path,
        &wire,
        &body,
        crate::artifact_transaction_conformance::ArtifactReplyObservation {
            phase: 11,
            ack_failed: true,
            quarantine_age: 1,
            ..Default::default()
        },
    );
    assert_transition(
        &model,
        "ReleaseGuard",
        &expired,
        &released,
        "the reaper releases the guard only after the quarantine expires",
    );
    let _ = std::fs::remove_dir_all(sock);

    // A partial wire: body out, trailer failed. Path bytes may be visible, so the
    // guard enters the same production 30 s quarantine rather than releasing.
    let (sock, target, lease) = capture_reply_target("write-failed");
    let path = target.display_path();
    let body = format!("OK 1 1 {}\n", path.display());
    let cancel = CaptureCancellation::new();
    let file = target
        .write_private_authorized(b"png", || cancel.authorize_commit())
        .unwrap();
    let (_, handoff) =
        crate::app_introspect::queue_capture_reply_for_test(target, file, Some(lease))
            .unwrap()
            .into_parts();
    let queued = observe_capture_reply(
        &model,
        &path,
        &path,
        b"",
        &body,
        capture_reply_observation(3),
    );
    let mut writer = FailAfterFirstWrite { bytes: Vec::new() };
    let error = match write_control_reply(
        &mut writer,
        ControlReply::with_handoff(body.clone(), handoff),
    ) {
        Err(error) => error,
        Ok(_) => panic!("the trailer write fails after the complete body"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    let prepared = reply_successor(&model, "PrepareWire", &queued);
    let write_failed = observe_capture_reply(
        &model,
        &path,
        &path,
        &writer.bytes,
        &body,
        crate::artifact_transaction_conformance::ArtifactReplyObservation {
            phase: 7,
            write_error: true,
            ..Default::default()
        },
    );
    assert_transition(
        &model,
        "WriteFailed",
        &prepared,
        &write_failed,
        "a partial socket write enters quarantine because path bytes may be visible",
    );
    // The same bytes with the trailer's error dropped: the partial frame
    // would await an ACK to a challenge that never went out.
    let awaited_partial = observe_capture_reply(
        &model,
        &path,
        &path,
        &writer.bytes,
        &body,
        capture_reply_observation(5),
    );
    reject_transition(
        &model,
        "WriteWire",
        &prepared,
        &awaited_partial,
        "only the complete frame awaits its acknowledgement",
    );
    assert!(!model.check_invariant("CompleteReplyPrecedesAck", &awaited_partial));
    // From here the guard waits on the same central reaper, at the production 30 s
    // delay; it ages, expires and releases like the failed ACK above, never early.
    let early_release = project_artifact_reply_with(&model, &write_failed, |o| {
        o.phase = 11;
        o.guard = false;
    });
    reject_transition(
        &model,
        "ReleaseGuard",
        &write_failed,
        &early_release,
        "a partial write retains the guard for the full quarantine",
    );
    let aged = reply_successor(&model, "AdvanceQuarantine", &write_failed);
    let expired = reply_successor(&model, "ExpireQuarantine", &aged);
    let released = reply_successor(&model, "ReleaseGuard", &expired);
    assert_eq!(
        (
            released["phase"],
            released["write_error"],
            released["guard"]
        ),
        (11, 1, 0)
    );
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, &released),
            "the write-failure release violates {}",
            invariant.name
        );
    }
    let _ = std::fs::remove_dir_all(sock);
}

#[test]
fn capture_reply_aborts_conform_to_artifact_reply_publication() {
    use crate::artifact_transaction_conformance::{assert_transition, reject_transition};
    let model = aterm_models_misc::artifact_reply_publication_model();
    let idle = model.init_state();

    // Timeout wins the CAS: nothing is ever published.
    let (sock, target, lease) = capture_reply_target("cancelled");
    let path = target.display_path();
    let cancel = CaptureCancellation::new();
    assert!(cancel.cancel());
    let error = target
        .write_private_authorized(b"never", || cancel.authorize_commit())
        .expect_err("a cancelled capture cannot take the final name");
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    drop(lease);
    let cancelled =
        observe_capture_reply(&model, &path, &path, b"", "", capture_reply_observation(1));
    assert_transition(
        &model,
        "Cancel",
        &idle,
        &cancelled,
        "timeout wins before publication",
    );
    let published_after_cancel = project_artifact_reply_with(&model, &cancelled, |o| {
        o.artifact = true;
        o.committed = true;
    });
    reject_transition(
        &model,
        "AuthorizeCommit",
        &cancelled,
        &published_after_cancel,
        "cancelled publication cannot be revived",
    );
    let _ = std::fs::remove_dir_all(sock);

    // Queued, then abandoned before the wire: the uncommitted guard removes
    // its exact file as it drops.
    let (sock, target, lease) = capture_reply_target("abandoned");
    let path = target.display_path();
    let cancel = CaptureCancellation::new();
    let file = target
        .write_private_authorized(b"png", || cancel.authorize_commit())
        .unwrap();
    let retained = crate::app_introspect::queue_capture_reply_for_test(target, file, Some(lease))
        .expect("validated capture queues its guard");
    let queued = observe_capture_reply(&model, &path, &path, b"", "", capture_reply_observation(3));
    drop(retained);
    let abort_pending = reply_successor(&model, "AbortQueued", &queued);
    let released_abort =
        observe_capture_reply(&model, &path, &path, b"", "", capture_reply_observation(12));
    assert_transition(
        &model,
        "ReleaseGuard",
        &abort_pending,
        &released_abort,
        "a pre-wire abort removes the unpublished artifact",
    );
    // The same release with the drop's uncommitted `remove_exact` arm lost.
    let orphaned = project_artifact_reply_with(&model, &released_abort, |o| o.artifact = true);
    reject_transition(
        &model,
        "ReleaseGuard",
        &abort_pending,
        &orphaned,
        "a pre-wire abort leaves no unpublished file behind",
    );
    assert!(!model.check_invariant("AbortReleaseRemovesUncommittedArtifact", &orphaned));
    let _ = std::fs::remove_dir_all(sock);
}

/// The identity barrier fails before queueing (`AbortAuthorized`) and at the wire
/// (`PrepareFailed`) when an ancestor is swapped under the retained handles; either
/// way the exact file is removed through its handle.
#[cfg(unix)]
#[test]
fn capture_reply_identity_failures_conform_to_artifact_reply_publication() {
    use crate::artifact_transaction_conformance::{assert_transition, reject_transition};
    use std::os::unix::fs::symlink;
    let model = aterm_models_misc::artifact_reply_publication_model();
    let body = "OK 1 1 swapped\n";
    for at_wire in [false, true] {
        let (sock, target, lease) = capture_reply_target("identity-failed");
        let path = target.display_path();
        let namespace = path.parent().unwrap().to_path_buf();
        let moved = sock.join("namespace-moved");
        let moved_file = moved.join(path.file_name().unwrap());
        let outside = sock.join("outside");
        crate::control_auth::ensure_private_dir(&outside).unwrap();
        let cancel = CaptureCancellation::new();
        let file = target
            .write_private_authorized(b"png", || cancel.authorize_commit())
            .unwrap();
        let authorized =
            observe_capture_reply(&model, &path, &path, b"", "", capture_reply_observation(2));
        let swap = || {
            std::fs::rename(&namespace, &moved).unwrap();
            symlink(&outside, &namespace).unwrap();
        };
        let mut wire = Vec::new();
        let (before, action) = if at_wire {
            let (_, handoff) =
                crate::app_introspect::queue_capture_reply_for_test(target, file, Some(lease))
                    .expect("the identity still validates when queued")
                    .into_parts();
            let queued =
                observe_capture_reply(&model, &path, &path, b"", "", capture_reply_observation(3));
            swap();
            assert!(
                write_control_reply(
                    &mut wire,
                    ControlReply::with_handoff(body.to_string(), handoff),
                )
                .expect("the failure reply is written")
                .is_none(),
                "a failed preparation leaves nothing to acknowledge"
            );
            assert!(
                String::from_utf8_lossy(&wire).starts_with("ERR artifact reply validation failed"),
                "no OK byte certifies the swapped identity"
            );
            (queued, "PrepareFailed")
        } else {
            swap();
            let refused =
                crate::app_introspect::queue_capture_reply_for_test(target, file, Some(lease))
                    .expect_err("the barrier refuses a swapped ancestor");
            assert!(refused.contains("path identity changed at reply barrier"));
            (authorized, "AbortAuthorized")
        };
        let abort_pending = reply_successor(&model, action, &before);
        let released_abort = observe_capture_reply(
            &model,
            &moved_file,
            &path,
            &wire,
            body,
            capture_reply_observation(12),
        );
        assert_transition(
            &model,
            "ReleaseGuard",
            &abort_pending,
            &released_abort,
            "the failed barrier removes the exact file through its retained handle",
        );
        if at_wire {
            // The OK body written before the revalidation that then
            // failed: the client read OK for the file the abort removed.
            let ok_then_err = [body.as_bytes(), wire.as_slice()].concat();
            let certified_abort = observe_capture_reply(
                &model,
                &moved_file,
                &path,
                &ok_then_err,
                body,
                capture_reply_observation(12),
            );
            reject_transition(
                &model,
                "ReleaseGuard",
                &abort_pending,
                &certified_abort,
                "no OK byte precedes the reply-time revalidation",
            );
            assert!(!model.check_invariant("CommitRequiresWirePreparation", &certified_abort));
        }
        assert!(
            std::fs::read_dir(&outside).unwrap().next().is_none(),
            "nothing was written outside"
        );
        let _ = std::fs::remove_file(&namespace);
        let _ = std::fs::remove_dir_all(sock);
    }
}

/// Copy a projected reply state and change some of its observations.
fn project_artifact_reply_with(
    model: &aterm_spec::derive::Model,
    state: &aterm_spec::interp::State,
    change: impl FnOnce(&mut crate::artifact_transaction_conformance::ArtifactReplyObservation),
) -> aterm_spec::interp::State {
    let mut observed = crate::artifact_transaction_conformance::ArtifactReplyObservation {
        phase: state["phase"],
        artifact: state["artifact"] == 1,
        guard: state["guard"] == 1,
        committed: state["committed"] == 1,
        reply: state["reply"] == 1,
        challenge: state["challenge"] == 1,
        ack: state["ack"] == 1,
        ack_failed: state["ack_failed"] == 1,
        write_error: state["write_error"] == 1,
        quarantine_age: state["quarantine_age"],
    };
    change(&mut observed);
    crate::artifact_transaction_conformance::project_artifact_reply(model, observed)
}

#[test]
fn nonreading_peer_cannot_hold_a_guarded_write_past_its_deadline() {
    let alive = Arc::new(AtomicBool::new(true));
    let prepared = Arc::new(AtomicBool::new(false));
    let reply = ControlReply::guarded(
        "x".repeat(8 * 1024 * 1024),
        Some(ReplyRetention::new(WireProbe {
            alive: Arc::clone(&alive),
            prepared,
            fail_prepare: false,
        })),
    );
    let (_nonreading_client, server) = CtlStream::pair().unwrap();
    let mut writer = &server;
    let started = std::time::Instant::now();
    let error = match write_control_reply_with_timeout_arm(
        &mut writer,
        reply,
        std::time::Duration::from_millis(100),
        |timeout| server.set_write_timeout(timeout),
    ) {
        Err(error) => error,
        Ok(_) => panic!("a non-reading peer must exhaust the whole-write deadline"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    // A HANG DETECTOR, not a budget: a write with no deadline armed blocks
    // on this peer forever.
    assert!(
        started.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "a non-reading peer held the guarded write past its deadline: {:?}",
        started.elapsed()
    );
    assert!(
        alive.load(Ordering::Acquire),
        "a timed-out partial reply transfers its guard to quarantine"
    );
}

#[test]
fn dribbling_ack_cannot_refresh_the_handoff_deadline() {
    let alive = Arc::new(AtomicBool::new(true));
    let prepared = Arc::new(AtomicBool::new(false));
    let reply = ControlReply::guarded(
        "OK guarded\n".to_string(),
        Some(ReplyRetention::new(WireProbe {
            alive,
            prepared,
            fail_prepare: false,
        })),
    );
    let mut wire = Vec::new();
    let pending = write_control_reply_with_timeout_arm(
        &mut wire,
        reply,
        std::time::Duration::from_millis(250),
        |_| Ok(()),
    )
    .unwrap()
    .expect("guarded response");
    let ack = format!(
        "{}{nonce}\n",
        aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX,
        nonce = pending.nonce.as_str()
    );
    let (mut client, server) = CtlStream::pair().unwrap();
    // One byte per `DRIBBLE_GAP`, inside the 250 ms handoff budget, so a budget
    // refreshed per byte would read the whole ack. The dribbler stops when told
    // (Darwin keeps accepting writes after the server's shutdown), so teardown
    // costs at most one gap.
    const DRIBBLE_GAP: std::time::Duration = std::time::Duration::from_millis(50);
    let stop = Arc::new(AtomicBool::new(false));
    let dribbler_stop = Arc::clone(&stop);
    let (dribbled_tx, dribbled) = std::sync::mpsc::channel::<()>();
    let dribbler = std::thread::spawn(move || {
        for (index, byte) in ack.bytes().enumerate() {
            if dribbler_stop.load(Ordering::Acquire)
                || client.write_all(&[byte]).is_err()
                || client.flush().is_err()
            {
                break;
            }
            if index == 1 {
                let _ = dribbled_tx.send(());
            }
            std::thread::sleep(DRIBBLE_GAP);
        }
    });
    let mut reader = BufReader::new(&server);
    assert_eq!(
        await_guarded_reply_close_with_quarantine(
            &server,
            &mut reader,
            pending,
            std::time::Duration::from_millis(20),
        ),
        ArtifactAckOutcome::AcknowledgementQuarantined
    );
    // The outcome is the proof: the dribbled ack is the CORRECT one, so a budget
    // refreshed per byte would read the whole line and acknowledge it.
    // "The peer really dribbled" is a handshake: its second byte went into the
    // still-open socket, so the quarantine was the deadline's verdict on a live
    // pair, not a dead pair's EOF.
    dribbled
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the peer really dribbled: a second byte reached the open socket");
    stop.store(true, Ordering::Release);
    let _ = server.shutdown(std::net::Shutdown::Both);
    dribbler.join().unwrap();
}

/// Read the guarded frame, acknowledge it and close, like the one-shot `aterm ctl`
/// client that exits as soon as its ACK is flushed. Returns the server's outcome.
///
/// The close happens before the server reads, deliberately and
/// deterministically: that ordering is the whole hazard. Darwin refuses
/// `setsockopt(SO_RCVTIMEO)` on an AF_UNIX socket whose peer has detached,
/// so a server that re-arms its read bound per refill finds the bound
/// unarmable and — before this was fixed — abandoned an acknowledgement
/// that was already sitting in its own receive queue. The caller runs
/// alone, and the detach is asserted before the server reads
/// ([`assert_peer_detached`]).
fn acknowledge_then_exit(
    client: CtlStream,
    server: &CtlStream,
    pending: PendingArtifactAck,
) -> ArtifactAckOutcome {
    {
        let mut writing = &client;
        let mut reader = BufReader::new(&client);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert!(response.starts_with("OK "), "guarded OK: {response:?}");
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        let nonce = challenge
            .trim_end()
            .strip_prefix(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX)
            .expect("challenge trailer");
        writing
            .write_all(
                format!(
                    "{}{nonce}\n",
                    aterm_types::control_verbs::ARTIFACT_REPLY_ACK_PREFIX
                )
                .as_bytes(),
            )
            .unwrap();
        writing.flush().unwrap();
    }
    // The one-shot client process exits HERE, with its ACK already in the
    // server's receive queue and unread.
    drop(client);
    assert_peer_detached(server);
    let mut server_reader = BufReader::new(server);
    await_guarded_reply_close_with_quarantine(
        server,
        &mut server_reader,
        pending,
        std::time::Duration::from_millis(40),
    )
}

/// THE DETACHED-PEER HAZARD, SET UP — asserted, never assumed. `drop`
/// detaches the peer only when it closes the client end's LAST descriptor,
/// and in the shared lib-test process a child any sibling forks holds a
/// copy of every descriptor until it execs (macOS 26.6 honors no
/// `FD_CLOFORK`). While that copy lives the server sees an ATTACHED peer:
/// its read bound re-arms, the detached-peer drain is never entered, and a
/// test of the drain passes without exercising it. So the tests that need
/// the detach run alone ([`crate::test_alone`]), where no other thread
/// forks, and say here that it was real: the peer is gone
/// ([`aterm_uds::hangup::peer_closed`], which can say so only on unix —
/// on Windows it answers `false` always), and on Darwin the server meets
/// the very refusal the drain exists for — `SO_RCVTIMEO` answers `EINVAL`.
#[track_caller]
fn assert_peer_detached(server: &CtlStream) {
    #[cfg(not(unix))]
    let _ = server;
    #[cfg(unix)]
    assert!(
        aterm_uds::hangup::peer_closed(server),
        "the client end's last descriptor closed: the peer detached"
    );
    #[cfg(target_os = "macos")]
    assert_eq!(
        server
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .map_err(|refused| refused.raw_os_error()),
        Err(Some(libc::EINVAL)),
        "Darwin refuses the read bound of a detached peer: the hazard the drain recovers from"
    );
}

fn guarded_probe_reply(retention: ReplyRetention) -> ControlReply {
    ControlReply::guarded(
        "OK 1 1 /private/artifact.png\n".to_string(),
        Some(retention),
    )
}

#[cfg(target_os = "macos")]
#[test]
fn detached_peer_drain_preserves_unrelated_timeout_errors() {
    let (_client, server) = CtlStream::pair().unwrap();
    let error = enter_detached_peer_drain(&server, std::io::Error::from_raw_os_error(libc::EBADF))
        .expect_err("only Darwin's detached-peer EINVAL authorizes a drain");
    assert_eq!(error.raw_os_error(), Some(libc::EBADF));
}

/// THE REGRESSION THAT SHIPPED. A client that acknowledges and exits in the
/// same breath — every `aterm ctl image <name>` — was recorded as a FAILED
/// acknowledgement, so its guard and its admission permit went to the 30 s
/// quarantine instead of being released: an ACK already in the receive
/// queue is proof of consumption, whether or not its writer is still
/// attached. Four named captures filled the pool; the fifth got
/// [`ARTIFACT_HANDOFF_BUSY`] and kept getting it for thirty seconds, so the
/// release self-check's typed take — one named capture per keystroke,
/// sixty-seven of them — died at frame four.
///
/// SEQUENTIAL guarded replies, many more than the pool holds, each
/// acknowledged by a client that exits at once. Every one must be admitted,
/// its guard released at the ACK, and the pool exactly empty at the end —
/// nothing here ever tested more than a single reply, which is why
/// four-then-stuck shipped. Under the bug the acknowledgement assertion
/// trips first, on capture zero; the admission `panic!` below is the one
/// that speaks for the SHIPPED symptom.
///
/// ALONE IN ITS OWN PROCESS: each capture's client must really detach
/// ([`assert_peer_detached`]), which a sibling's fork would prevent.
#[test]
fn sequential_acknowledged_replies_never_exhaust_the_handoff_pool() {
    if !crate::test_alone::alone() {
        return;
    }
    // Charge each reply so the descriptor budget saturates at exactly the
    // live cap, the way a real capture path's pinned chain does.
    const CHARGE: usize = ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT / ARTIFACT_HANDOFF_LIMIT;
    const CAPTURES: usize = 120;
    static USAGE: Mutex<ArtifactHandoffUsage> = Mutex::new(ArtifactHandoffUsage {
        live: 0,
        descriptor_units: 0,
    });

    for capture in 0..CAPTURES {
        let permit = ArtifactHandoffPermit::try_acquire_from(
            &USAGE,
            ARTIFACT_HANDOFF_LIMIT,
            ARTIFACT_HANDOFF_DESCRIPTOR_LIMIT,
            CHARGE,
        )
        .unwrap_or_else(|| {
            panic!(
                "capture {capture} was refused admission: a released reply \
                 is still holding its handoff permit"
            )
        });
        let alive = Arc::new(AtomicBool::new(true));
        let (client, server) = CtlStream::pair().unwrap();
        let reply = guarded_probe_reply(ReplyRetentionPermit(permit).retain(WireProbe {
            alive: Arc::clone(&alive),
            prepared: Arc::new(AtomicBool::new(false)),
            fail_prepare: false,
        }));
        let mut writer = &server;
        let pending = write_control_reply(&mut writer, reply)
            .unwrap()
            .expect("guarded response");
        assert_eq!(
            acknowledge_then_exit(client, &server, pending),
            ArtifactAckOutcome::PeerAcknowledged,
            "capture {capture} lost its acknowledgement"
        );
        assert!(
            !alive.load(Ordering::Acquire),
            "capture {capture} kept its guard past the acknowledgement"
        );
    }

    assert_eq!(
        *USAGE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        ArtifactHandoffUsage {
            live: 0,
            descriptor_units: 0,
        },
        "every one of {CAPTURES} sequential captures gave its permit back"
    );
}

/// Draining a detached peer recovers ITS BYTES, not its verdict. A one-shot
/// client that exits without acknowledging — silently, or with a wrong line
/// — must still be a failed handoff, or the fix above would have turned
/// "the peer went away" into proof the response was consumed.
///
/// ALONE IN ITS OWN PROCESS, for the reason [`assert_peer_detached`]
/// gives: under a sibling's fork the peer stays attached, the quarantine
/// is the deadline's verdict on a live pair, and the drain goes untested.
#[test]
fn detached_peer_without_a_valid_ack_is_still_quarantined() {
    if !crate::test_alone::alone() {
        return;
    }
    for (label, farewell) in [
        ("silent exit", None),
        ("wrong line", Some("ACK not-the-nonce\n")),
        ("unterminated", Some("ACK ")),
    ] {
        let alive = Arc::new(AtomicBool::new(true));
        let (client, server) = CtlStream::pair().unwrap();
        let reply = guarded_probe_reply(ReplyRetention::new(WireProbe {
            alive: Arc::clone(&alive),
            prepared: Arc::new(AtomicBool::new(false)),
            fail_prepare: false,
        }));
        let mut writer = &server;
        let pending = write_control_reply(&mut writer, reply)
            .unwrap()
            .expect("guarded response");
        if let Some(farewell) = farewell {
            let mut writing = &client;
            writing.write_all(farewell.as_bytes()).unwrap();
            writing.flush().unwrap();
        }
        drop(client);
        assert_peer_detached(&server);
        let mut server_reader = BufReader::new(&server);
        assert_eq!(
            await_guarded_reply_close_with_quarantine(
                &server,
                &mut server_reader,
                pending,
                // A hang detector's length, so the reaper cannot beat the
                // assertion below to the guard however long this thread
                // stalls between the two: this test is about the verdict,
                // not about how quickly the grace expires. (2 s raced a
                // stall of 2 s.) The process is this test's alone, and
                // ends before the grace does.
                aterm_spec::wait::MUST_HAPPEN,
            ),
            ArtifactAckOutcome::AcknowledgementQuarantined,
            "{label} must not read as a consume acknowledgement"
        );
        assert!(
            alive.load(Ordering::Acquire),
            "{label} keeps the guard until its quarantine grace elapses"
        );
    }
}

/// Neither an ACK guessed before the challenge existed nor a request half-close
/// proves the response was consumed: each is a failed handoff, and the exact guard
/// is held in quarantine.
#[test]
fn a_pre_challenge_guess_or_a_half_close_is_quarantined_not_acknowledged() {
    for (label, half_close) in [("pre-pipelined guess", false), ("request half-close", true)] {
        let alive = Arc::new(AtomicBool::new(true));
        let (mut client, server) = CtlStream::pair().unwrap();
        if half_close {
            client.shutdown(std::net::Shutdown::Write).unwrap();
        } else {
            // Written before the server mints its nonce, so it cannot causally
            // acknowledge a response it has not read.
            client.write_all(b"ACK artifact\n").unwrap();
            client.flush().unwrap();
        }
        let server_alive = Arc::clone(&alive);
        let worker = std::thread::spawn(move || {
            let reply = guarded_probe_reply(ReplyRetention::new(WireProbe {
                alive: server_alive,
                prepared: Arc::new(AtomicBool::new(false)),
                fail_prepare: false,
            }));
            let mut writer = &server;
            let pending = write_control_reply(&mut writer, reply)
                .unwrap()
                .expect("guarded response");
            let mut reader = BufReader::new(&server);
            let outcome = await_guarded_reply_close_with_quarantine(
                &server,
                &mut reader,
                pending,
                HELD_QUARANTINE,
            );
            (outcome, last_quarantine_ticket())
        });
        let mut reader = BufReader::new(&client);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert_eq!(response, "OK 1 1 /private/artifact.png\n", "{label}");
        let mut challenge = String::new();
        reader.read_line(&mut challenge).unwrap();
        assert!(
            challenge.starts_with(aterm_types::control_verbs::ARTIFACT_REPLY_CHALLENGE_PREFIX),
            "{label}"
        );
        let (outcome, ticket) = worker.join().unwrap();
        assert_eq!(
            outcome,
            ArtifactAckOutcome::AcknowledgementQuarantined,
            "{label}"
        );
        assert!(
            alive.load(Ordering::Acquire),
            "{label}: the exact guard is held in quarantine"
        );
        end_quarantine(ticket);
    }
}

/// The `cwd` / `cell` / `colors` / `modes` read verbs, one row per terminal state: a
/// fresh 24×80 terminal, seed bytes, one verb call, then exact / prefix / needle
/// checks. Two rows guard against a program forging control-protocol reply text.
#[test]
fn read_verbs_report_the_terminal_state() {
    enum Call {
        Cwd,
        Cell(&'static str),
        Colors,
        Modes,
    }
    struct Row {
        label: &'static str,
        seed: &'static [u8],
        call: Call,
        exact: Option<&'static str>,
        prefix: Option<&'static str>,
        /// The reply holds exactly this many newlines.
        newlines: Option<usize>,
        must: &'static [&'static str],
        must_not: &'static [&'static str],
        /// The session's palette gate, set before the seed (`None`: the
        /// engine default, closed).
        palette_gate: Option<bool>,
    }
    let row = |label: &'static str, seed: &'static [u8], call: Call| Row {
        label,
        seed,
        call,
        palette_gate: None,
        exact: None,
        prefix: None,
        newlines: None,
        must: &[],
        must_not: &[],
    };
    const LINKED: &[u8] = b"\x1b]8;;https://example.com\x1b\\X\x1b]8;;\x1b\\";
    let rows = [
        // `cwd` surfaces the OSC 7-reported working directory (empty until set).
        Row {
            exact: Some("OK \n"),
            ..row("cwd before OSC 7", b"", Call::Cwd)
        },
        // A program reports its cwd as a file:// URI.
        Row {
            must: &["/Users//example/x"],
            ..row(
                "cwd after OSC 7",
                b"\x1b]7;file://localhost/Users//example/x\x07",
                Call::Cwd,
            )
        },
        // An OSC 7 path cannot forge reply lines: OSC 7 percent-decodes (`%0A` is a
        // raw newline), and pct-encoding the cwd keeps the reply to one newline.
        Row {
            newlines: Some(1),
            must: &["%0A"],
            must_not: &["\nOK"],
            ..row(
                "cwd with an embedded newline",
                b"\x1b]7;file://localhost/tmp/%0AOK%20forged\x07",
                Call::Cwd,
            )
        },
        // `cell` appends `link=<url>` for an OSC 8 hyperlinked cell, and nothing
        // for a plain cell (positional fields unchanged for non-link cells).
        Row {
            must: &["link=https://example.com"],
            ..row("a linked cell", LINKED, Call::Cell("0 0"))
        },
        Row {
            must_not: &["link="],
            ..row("a plain cell beside it", LINKED, Call::Cell("0 5"))
        },
        // `cell` must pct-encode the OSC 8 hyperlink so a space in the URL
        // cannot break the space-delimited cell line into spurious fields.
        Row {
            must: &["link=https://example.com/a%20b"],
            must_not: &["/a b"],
            ..row(
                "a link with a space",
                b"\x1b]8;;https://example.com/a b\x1b\\X\x1b]8;;\x1b\\",
                Call::Cell("0 0"),
            )
        },
        // `colors` reports the theme and reflects OSC 10/11/12 dynamic changes.
        Row {
            prefix: Some("OK fg="),
            must: &[" bg=", " cursor="],
            ..row("colors, the theme", b"", Call::Colors)
        },
        Row {
            must: &["bg=102030"],
            ..row("colors after OSC 11", b"\x1b]11;#102030\x07", Call::Colors)
        },
        // The tail says what programs set over the theme, still on one line.
        Row {
            palette_gate: Some(true),
            prefix: Some("OK fg="),
            newlines: Some(1),
            must: &[" palette_sets=allowed refused=0 program_palette=- program_slots=-\n"],
            ..row("colors, nothing set, gate open", b"", Call::Colors)
        },
        Row {
            palette_gate: Some(true),
            newlines: Some(1),
            must: &[" palette_sets=allowed refused=0 program_palette=1:00ff00 program_slots=bg\n"],
            ..row(
                "colors after a program's OSC 4 and OSC 11",
                b"\x1b]4;1;rgb:00/ff/00\x07\x1b]11;rgb:10/20/30\x07",
                Call::Colors,
            )
        },
        // Negative control: a closed gate refuses the same OSC 4 and says so.
        Row {
            palette_gate: Some(false),
            newlines: Some(1),
            must: &[" palette_sets=refused refused=1 program_palette=- program_slots=-\n"],
            ..row(
                "colors after a refused OSC 4",
                b"\x1b]4;1;rgb:00/ff/00\x07",
                Call::Colors,
            )
        },
        // `modes` exposes IRM / DECAWM / DECOM, which a driving client needs to
        // predict how typed input and printed output land.
        Row {
            must: &["insert_mode=false", "auto_wrap=true", "origin_mode=false"],
            ..row("modes, defaults", b"", Call::Modes)
        },
        // IRM on (ESC[4h), auto-wrap off (ESC[?7l), origin on (ESC[?6h).
        Row {
            must: &["insert_mode=true", "auto_wrap=false", "origin_mode=true"],
            ..row("modes, set", b"\x1b[4h\x1b[?7l\x1b[?6h", Call::Modes)
        },
    ];
    for r in rows {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        if let Some(open) = r.palette_gate {
            term.lock().unwrap().set_allow_palette_reconfigure(open);
        }
        if !r.seed.is_empty() {
            term.lock().unwrap().process(r.seed);
        }
        let out = match r.call {
            Call::Cwd => cmd_cwd(&term),
            Call::Cell(args) => cmd_cell(&term, args),
            Call::Colors => cmd_colors(&term),
            Call::Modes => cmd_modes(&term),
        };
        let label = r.label;
        if let Some(want) = r.exact {
            assert_eq!(out, want, "{label}");
        }
        if let Some(want) = r.prefix {
            assert!(out.starts_with(want), "{label}: {out:?}");
        }
        if let Some(n) = r.newlines {
            assert_eq!(out.matches('\n').count(), n, "{label}: {out:?}");
        }
        for needle in r.must {
            assert!(out.contains(needle), "{label}: missing {needle:?}: {out:?}");
        }
        for needle in r.must_not {
            assert!(!out.contains(needle), "{label}: leaked {needle:?}: {out:?}");
        }
    }
}

#[cfg(unix)]
use super::control_input::{feed_bytes, paste_text, send_bytes};
use super::control_input::{parse_resize, parse_tab, take_mods};
use super::control_media::{
    MAX_IMAGE_PAYLOAD_BYTES, cmd_image_read, image_payload, image_read_line,
};
use super::control_query::{
    AbsRow, abs_row_text, cmd_cell, cmd_colors, cmd_cursor_json, cmd_cwd, cmd_line, cmd_mainscreen,
    cmd_modes, cmd_screen_styled_json, cmd_search, cmd_text, cmd_text_json, serialize_dims,
    serialize_dims_json, styled_image_json,
};
use super::control_selection::{cmd_blocks, cmd_blocks_json, cmd_wait};
use super::control_session::{
    TurnIo, cmd_await, cmd_cast, cmd_connect_in, cmd_disconnect_in, cmd_edges, cmd_edges_json,
    cmd_family, cmd_flows, cmd_grant, cmd_lease, cmd_meta, cmd_ready, cmd_revoke, cmd_sessions,
    cmd_timeline, cmd_turn, cmd_who, cmd_whoami, raise_target,
};
use super::*;
use crate::TabAction;
use crate::input::InputEvent;
use crate::session_waits::RewrapGauge;
use aterm_control::wire::pct_decode;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_session::sink::SinkWriter;
// The pipe-backed byte-drain assertions are Unix-only (the tests that read
// do so through fn-local imports).
#[cfg(unix)]
use std::os::unix::io::FromRawFd;

/// An owner-only, session-independent verb refuses an edge and an Owner's explicit
/// `@<sid>`; `@self` from the Owner passes.
#[test]
fn owner_only_verbs_refuse_an_edge_and_a_named_session() {
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    let denied = |refusal: Option<String>| refusal.is_some_and(|r| r.starts_with("ERR denied"));
    assert!(denied(owner_only_refusal(
        "sessions",
        Some(&Selector::Local(7)),
        Scope::Owner
    )));
    assert!(denied(owner_only_refusal("sessions", None, edge)));
    assert_eq!(
        owner_only_refusal("sessions", Some(&Selector::SelfTok), Scope::Owner),
        None
    );
}

/// Which requests move to a wait lane (`control_lanes.rs`): those that park on an
/// event or hand the connection to a relay. Every read, keystroke and `subscribe`
/// that flips to push here is request-lane work; a `subscribe` to one session
/// another instance hosts is relayed for the whole subscription, a wait, in every
/// spelling the relay follows. Only an Owner-class connection is relayed.
#[test]
fn wait_requests_are_the_ones_that_park_or_relay() {
    let store = session_store::new_store();
    let local = registered_session(0, -1, b"");
    let local_sid = local.ctx.self_id.as_str().to_string();
    store.write().unwrap().register(local);
    let remote_sid = SessionId::generate();
    let remote_sid = remote_sid.as_str();
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    for line in [
        "await seq 5 timeout 20000",
        "@. await agent busy,idle timeout=300",
        "ready 1000",
        "wait 500",
        "turn hello",
        "post to=@s-x kind=ask hi",
        "inbox get @42",
        "@s-not-hosted-here text",
        // The selector-second grammar: relayed to the instance that hosts it,
        // under any spelling `graph/<sid>` resolves (a path; another case on a
        // case-insensitive volume) — classed a wait wherever it is relayed.
        &format!("subscribe @{remote_sid} events"),
        &format!("subscribe  @{remote_sid} events"),
        &format!("subscribe @./{remote_sid} events"),
        &format!("subscribe @S-{} events", &remote_sid[2..]),
        // Settles 60 ms and folds the whole recording (`cast drift`).
        "cast drift",
        &format!("@{local_sid} cast drift max_runs=4"),
        // A paste into another session waits, bounded, for its program to read
        // it (`background_input`): never a request lane's wait. So does a
        // `paste-bin` whatever it names: flagless, it takes the same wait when
        // the caller's own tab is not on screen.
        &format!("@{local_sid} paste hello"),
        "@3 paste hello",
        &format!("@{local_sid} paste-bin 5"),
        "paste-bin 5",
        "@. paste-bin 5",
    ] {
        assert!(
            is_wait_request(line, Scope::Owner, &store),
            "{line:?} parks or relays"
        );
    }
    // An Edge is never relayed, so a line naming another instance's session is
    // refused at once on a request lane; what parks still parks.
    for line in [
        "@s-not-hosted-here text",
        &format!("subscribe @{remote_sid} events"),
    ] {
        assert!(
            !is_wait_request(line, edge, &store),
            "{line:?} from an Edge is refused, never relayed"
        );
    }
    assert!(is_wait_request("await seq 5 timeout 20000", edge, &store));
    assert!(is_wait_request(
        "@s-not-hosted-here await seq 5",
        edge,
        &store
    ));
    for line in [
        "version",
        "text --json tail=40",
        "status",
        "key enter",
        "send hello",
        // A flagless paste goes to the App, which never waits on the program;
        // and a key into another session, which never waits on it.
        "paste hello",
        "@. paste hello",
        &format!("@{local_sid} key enter"),
        &format!("@{local_sid} send hello"),
        "subscribe @. screen",
        // The bridge's and fleet's watch-everything subscription, a session
        // this instance hosts, and a list: each flips to push here.
        "subscribe @* events,sessions",
        &format!("subscribe @{local_sid} events"),
        &format!("subscribe @{local_sid},@{remote_sid} events"),
        "subscribe @42 events",
        "subscribe @ events",
        "subscribe events",
        // A leading selector this instance hosts runs the verb here, whatever
        // the subscribe names.
        &format!("@{local_sid} subscribe @{remote_sid} events"),
        "inbox",
        "inbox get 3",
        "inbox seen 3 handled",
        "meta set supervisor aterm-harness@1 ttl=60000",
        &format!("@{local_sid} text"),
        "cast",
        "cast frames count=4",
    ] {
        assert!(
            !is_wait_request(line, Scope::Owner, &store),
            "{line:?} is request-lane work"
        );
    }
}

/// The pre-auth read deadline relies on a timed-out read surfacing as an error:
/// `read_request_line` on a stream with a read timeout yields `None` when the peer
/// stays silent, never parking past the deadline.
#[test]
fn read_request_line_returns_none_on_read_timeout() {
    let (client, server) = CtlStream::pair().unwrap();
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(50)))
        .unwrap();
    let mut reader = BufReader::new(server);
    // The client sends nothing (a silent pre-auth peer).
    let start = std::time::Instant::now();
    assert_eq!(
        read_request_line(&mut reader),
        None,
        "a timed-out read must drop the connection"
    );
    // A hang detector: a read the timeout did not bound parks until the
    // client below is dropped, which is never before this line.
    assert!(
        start.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the deadline must actually bound the blocking read"
    );
    drop(client);
}

/// Authenticated sockets keep the persistent wire contract: a driver may send later
/// requests on the same connection. Capacity protection is admission-time
/// busy/retry, never an idle EOF; the receive timeout stays armed as a backstop.
#[test]
fn authenticated_readiness_preserves_slow_cadence() {
    use std::io::Write;

    let (mut client, server) = CtlStream::pair().expect("real control socket pair");
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(50)))
        .expect("arm authenticated recv backstop");
    let sender = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        client.write_all(b"version\n").expect("late request");
    });
    let mut reader = control_lanes::lane_reader(&server, Vec::new());
    assert_eq!(
        read_authenticated_request_line(&mut reader).as_deref(),
        Some("version"),
        "authenticated readiness must not become an undocumented EOF"
    );
    sender.join().expect("late sender exits");
}

/// A partial request line survives the socket wait after its first chunk.
/// The wait must resume the same frame, even across the receive backstop.
///
/// The line's second half is sent once the reader has TAKEN the first —
/// the server's receive queue reads empty — so the two halves are two
/// reads with a socket wait between them on every run. An 80 ms sleep let
/// a reader that started late take both in one read, and the wait went
/// unexercised. The sender polls through its own descriptor for the
/// server's socket: a failed assert unwinds without joining it and closes
/// `server`, whose number another test's thread may then reuse.
#[cfg(unix)]
#[test]
fn authenticated_readiness_preserves_partial_line() {
    use std::io::Write;
    use std::os::fd::AsFd;

    let (mut client, server) = CtlStream::pair().expect("real control socket pair");
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .expect("arm authenticated recv backstop");
    let probe = server.try_clone().expect("the sender's own descriptor");
    let sender = std::thread::spawn(move || {
        client.write_all(b"ver").expect("first line chunk");
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the reader takes the first chunk",
            || (unread_bytes(probe.as_fd()) == 0).then_some(()),
        );
        client.write_all(b"sion\n").expect("final line chunk");
    });
    let mut reader = control_lanes::lane_reader(&server, Vec::new());
    assert_eq!(
        read_authenticated_request_line(&mut reader).as_deref(),
        Some("version")
    );
    sender.join().expect("sender exits");
}

/// A BufReader may prefetch several request lines in one read, and the second is
/// then invisible to kernel readiness; an unconditional socket wait would park
/// until the peer sent a third.
#[test]
fn authenticated_readiness_drains_prefetched_lines_without_waiting() {
    use std::io::Write;

    let (mut client, server) = CtlStream::pair().expect("real control socket pair");
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .expect("arm authenticated recv backstop");
    client
        .write_all(b"first\nsecond\n")
        .expect("coalesced request lines");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut reader = control_lanes::lane_reader(&server, Vec::new());
        let first = read_authenticated_request_line(&mut reader);
        let prefetched = !reader.buffer().is_empty();
        let second = read_authenticated_request_line(&mut reader);
        let _ = tx.send((first, prefetched, second));
    });
    let result = rx.recv_timeout(EVENTUALLY);
    // Release even a broken implementation's blocked wait before failing.
    let _ = client.shutdown(std::net::Shutdown::Both);
    let Ok((first, prefetched, second)) = result else {
        panic!("a prefetched line waited for another socket event");
    };
    reader_thread.join().expect("reader exits");
    assert_eq!(first.as_deref(), Some("first"));
    assert!(prefetched, "the next line really was in BufReader");
    assert_eq!(second.as_deref(), Some("second"));
}

/// A wait moved to a wait lane carries the requests pipelined behind it
/// (`control_lanes::lane_reader`). They were read from the socket already,
/// so a readiness wait on it would park until the still-open peer sent
/// more: the carried lines must be read first, without waiting, and the
/// socket after them.
#[test]
fn authenticated_readiness_reads_carried_lines_without_waiting() {
    use std::io::Write;

    let (mut client, server) = CtlStream::pair().expect("real control socket pair");
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .expect("arm authenticated recv backstop");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut reader = control_lanes::lane_reader(&server, b"version\nstatus\n".to_vec());
        let first = read_authenticated_request_line(&mut reader);
        let second = read_authenticated_request_line(&mut reader);
        let _ = tx.send((first, second));
        read_authenticated_request_line(&mut reader)
    });
    // The carried lines MUST be answered with no socket event at all: a
    // hang detector, never a 2 s budget. A reader that waits on the socket
    // parks until `after` below, so it is still named, at the minute.
    let result = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN);
    client.write_all(b"after\n").expect("the next request");
    let after = reader_thread.join().expect("reader exits");
    let _ = client.shutdown(std::net::Shutdown::Both);
    let Ok((first, second)) = result else {
        panic!("a carried line waited for another socket event");
    };
    assert_eq!(first.as_deref(), Some("version"));
    assert_eq!(second.as_deref(), Some("status"));
    assert_eq!(after.as_deref(), Some("after"), "then the socket, in order");
}

/// A write-half close wakes the readiness wait even when the final line
/// has no newline. The queued tail is returned once, then EOF lets the
/// serving caller release its ordinary RPC worker and any bound claim.
#[test]
fn authenticated_readiness_drains_tail_then_observes_peer_eof() {
    use std::io::Write;

    let (mut client, server) = CtlStream::pair().expect("real control socket pair");
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .expect("arm authenticated recv backstop");
    let (tx, rx) = std::sync::mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut reader = control_lanes::lane_reader(&server, Vec::new());
        let tail = read_authenticated_request_line(&mut reader);
        let eof = read_authenticated_request_line(&mut reader);
        let _ = tx.send((tail, eof));
    });
    client.write_all(b"final tail").expect("unterminated line");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("peer write-half close");
    let result = rx.recv_timeout(EVENTUALLY);
    let _ = client.shutdown(std::net::Shutdown::Both);
    let Ok((tail, eof)) = result else {
        panic!("peer EOF did not release the authenticated reader");
    };
    reader_thread.join().expect("reader exits");
    assert_eq!(tail.as_deref(), Some("final tail"));
    assert_eq!(eof, None);
}

#[test]
#[cfg(target_os = "macos")]
fn authenticated_poll_event_drains_hangup_and_rejects_invalid_fd() {
    assert!(authenticated_poll_event(libc::POLLIN).is_ok());
    assert!(authenticated_poll_event(libc::POLLHUP).is_ok());
    assert!(authenticated_poll_event(libc::POLLIN | libc::POLLHUP).is_ok());
    assert!(authenticated_poll_event(libc::POLLERR).is_ok());
    assert_eq!(
        authenticated_poll_event(libc::POLLNVAL | libc::POLLIN)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EBADF),
        "an invalid fd must fail even if another bit claims readability"
    );
    assert!(authenticated_poll_event(0).is_err());
}

/// The authenticated liveness tick also preserves `feed-bin` framing: a
/// valid bounded payload may arrive in chunks separated by several kernel
/// read timeouts without losing its already-filled prefix.
///
/// Each next chunk is sent once the reader has met a REAL read timeout
/// after the bytes before it ([`TickingReads`]), so every chunk boundary
/// is crossed by a tick on every run. 80 ms sleeps let a reader that
/// started late find the whole payload queued and meet no tick at all.
#[test]
fn authenticated_binary_payload_preserves_delayed_chunks() {
    use std::io::Write;

    let (mut client, server) = CtlStream::pair().expect("real control socket pair");
    server
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .expect("arm representative authenticated poll tick");
    let reads = TickingReads::new(&server);
    let seen = Arc::clone(&reads.seen);
    let sender = std::thread::spawn(move || {
        let ticked_after = |delivered: usize| {
            aterm_spec::wait::until_within(
                aterm_spec::wait::MUST_HAPPEN,
                "the reader meets a read timeout after the bytes so far",
                || (*seen.lock().unwrap() == (delivered, true)).then_some(()),
            );
        };
        client.write_all(b"\0\x01").expect("first binary chunk");
        ticked_after(2);
        client.write_all(b"\x02").expect("second binary chunk");
        ticked_after(3);
        client.write_all(b"\x03\x04").expect("final binary chunk");
    });
    let mut payload = [0u8; 5];
    let mut reader = BufReader::new(reads);
    read_exact_authenticated(&mut reader, &mut payload)
        .expect("timeout ticks retain the partially filled binary frame");
    assert_eq!(payload, [0, 1, 2, 3, 4]);
    sender.join().expect("delayed binary sender exits");
}

/// The server end of a control socket, read through, saying what its
/// reads have met: `(bytes delivered, a read timed out since the last of
/// them)`. A test paces its sender on it, so a chunk follows a REAL
/// kernel read timeout rather than a sleep that may or may not cover one.
struct TickingReads<'a> {
    stream: &'a CtlStream,
    seen: Arc<Mutex<(usize, bool)>>,
}

impl<'a> TickingReads<'a> {
    fn new(stream: &'a CtlStream) -> Self {
        Self {
            stream,
            seen: Arc::default(),
        }
    }
}

impl std::io::Read for TickingReads<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut stream = self.stream;
        let read = stream.read(buf);
        let mut seen = self.seen.lock().unwrap();
        match &read {
            Ok(n) => *seen = (seen.0 + n, false),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                seen.1 = true;
            }
            Err(_) => {}
        }
        read
    }
}

/// Bytes queued in the receive buffer of `socket` that no read has taken
/// yet (`FIONREAD`).
#[cfg(unix)]
fn unread_bytes(socket: std::os::fd::BorrowedFd<'_>) -> usize {
    use std::os::fd::AsRawFd;

    let mut queued: libc::c_int = 0;
    // SAFETY: FIONREAD writes one `c_int` through the pointer, which
    // points at `queued`; `socket` is borrowed, so open for the call.
    let rc = unsafe { libc::ioctl(socket.as_raw_fd(), libc::FIONREAD, &mut queued) };
    assert_eq!(rc, 0, "FIONREAD on a live socket");
    usize::try_from(queued).expect("a byte count")
}

/// Push capacity is isolated and lane-exact AT ITS CEILING: a dispatch that
/// may hold four lanes and grows none, four long-lived subscribers own them,
/// the FIFTH gets a prompt explicit error rather than waiting behind them
/// without an ACK, and completion restores one admission.
#[test]
fn fifth_subscription_is_promptly_rejected_and_capacity_recovers() {
    use std::io::BufRead;

    let subscriptions = SubscriptionDispatch::new(CONTROL_SUBSCRIPTION_WORKERS);
    subscriptions
        .jobs
        .set_capacity(CONTROL_SUBSCRIPTION_WORKERS);
    let mut clients = Vec::new();
    for index in 0..CONTROL_SUBSCRIPTION_WORKERS {
        let (client, server) = CtlStream::pair().expect("subscription socket pair");
        clients.push(client);
        assert!(
            subscriptions
                .try_submit(
                    format!("subscribe @. screen since={index}"),
                    Scope::Owner,
                    server
                )
                .is_ok(),
            "subscriber {index} owns one reserved push lane"
        );
    }
    let (overflow_client, overflow_server) = CtlStream::pair().expect("fifth subscription pair");
    // The refusal MUST come: a hang detector, not a latency budget.
    overflow_client
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("bound rejection read");
    handoff_subscription(
        &subscriptions,
        "subscribe @. screen".to_string(),
        Scope::Owner,
        overflow_server,
    );
    let mut rejection = String::new();
    BufReader::new(overflow_client)
        .read_line(&mut rejection)
        .expect("fifth subscriber gets a prompt response");
    assert_eq!(rejection, "ERR subscription capacity busy; retry\n");

    // Independent ordinary admission remains live while push is saturated.
    let ordinary: BoundedDispatch<CtlStream> = BoundedDispatch::new(1);
    ordinary.set_capacity(1);
    let (_ordinary_client, ordinary_server) = CtlStream::pair().expect("ordinary control pair");
    assert!(ordinary.try_submit(ordinary_server).is_ok());

    // Popping starts long-lived work; it must NOT release capacity early.
    drop(subscriptions.jobs.pop());
    let (_recovery_client, recovery_server) =
        CtlStream::pair().expect("recovery subscription pair");
    assert!(
        subscriptions
            .try_submit(
                "subscribe @. screen".to_string(),
                Scope::Owner,
                recovery_server,
            )
            .is_err(),
        "starting a long-lived subscription does not create a phantom slot"
    );
    subscriptions.jobs.complete();
    let (_restored_client, restored_server) =
        CtlStream::pair().expect("restored subscription pair");
    assert!(
        subscriptions
            .try_submit(
                "subscribe @. screen".to_string(),
                Scope::Owner,
                restored_server,
            )
            .is_ok(),
        "one completed subscriber restores exactly one push lane"
    );
    drop(clients);
}

/// The `key` / `ctrl` grammar, one labelled row per input line.
///
/// * `key ctrl+<c>` and `ctrl <c>` build the identical event, so both write the
///   same control byte through the same seam (`ctrl+u` -> 0x15).
/// * `type=press|repeat|release` reaches the event (the Kitty CSI-u event type);
///   `base=<char>` carries the US-QWERTY base-layout key (Kitty
///   REPORT_ALTERNATE_KEYS 3rd field). A bad value rejects the line.
#[test]
fn key_and_ctrl_grammar() {
    use aterm_types::keyboard::{Key, KeyEventType as T, Modifiers as M, NamedKey as Nk};
    enum Want {
        /// `parse_key` builds exactly this event (`None`: refuses the line).
        Key(Option<InputEvent>),
        /// `parse_key(input) == parse_key(other)`.
        SameAs(&'static str),
        /// `parse_key(input) == parse_ctrl(other)`.
        SameAsCtrl(String),
        /// `parse_ctrl` builds exactly this event (`None`: refuses).
        Ctrl(Option<InputEvent>),
    }
    let ev = |key, mods, base_layout, event_type| {
        Want::Key(Some(InputEvent::Key {
            key,
            mods,
            base_layout,
            layout_text: None,
            event_type,
        }))
    };
    let press = |key, mods| ev(key, mods, None, T::Press);
    let named = |nk| press(Key::Named(nk), M::empty());
    let ch = Key::Character;
    let mut rows: Vec<(String, Want)> = vec![
        // `parse_key` builds the named-key event the seam encodes; unknown -> None.
        ("up".into(), named(Nk::ArrowUp)),
        ("f5 mods=ctrl".into(), press(Key::Named(Nk::F5), M::CTRL)),
        ("nope".into(), Want::Key(None)),
        // Case-insensitive on the character, matching `parse_ctrl`.
        ("ctrl+U".into(), press(ch('u'), M::CTRL)),
        // alt+/shift+/super+ and stacked prefixes.
        ("alt+x".into(), press(ch('x'), M::ALT)),
        ("ctrl+shift+a".into(), press(ch('a'), M::CTRL | M::SHIFT)),
        // Inline prefixes are additive with a trailing `mods=` token.
        ("ctrl+u".into(), Want::SameAs("u mods=ctrl")),
        // Inline prefixes also apply to NAMED keys.
        ("ctrl+up".into(), press(Key::Named(Nk::ArrowUp), M::CTRL)),
        // The literal `+` key (no recognized modifier before it) survives.
        ("+".into(), press(ch('+'), M::empty())),
        // A multi-char residual that is not a named key is still rejected.
        ("ctrl+nope".into(), Want::Key(None)),
        // `type=`: additive with mods=, position-independent; unknown rejects.
        ("up type=press".into(), named(Nk::ArrowUp)),
        (
            "up type=repeat".into(),
            ev(Key::Named(Nk::ArrowUp), M::empty(), None, T::Repeat),
        ),
        (
            "up type=release".into(),
            ev(Key::Named(Nk::ArrowUp), M::empty(), None, T::Release),
        ),
        (
            "up type=up".into(),
            ev(Key::Named(Nk::ArrowUp), M::empty(), None, T::Release),
        ),
        (
            "type=release mods=ctrl up".into(),
            ev(Key::Named(Nk::ArrowUp), M::CTRL, None, T::Release),
        ),
        ("up type=bogus".into(), Want::Key(None)),
        // `base=`: a single char, else the line is rejected.
        (
            "q base=a".into(),
            ev(ch('q'), M::empty(), Some('a'), T::Press),
        ),
        ("q base=ab".into(), Want::Key(None)),
        // `parse_ctrl` lower-cases and CTRL-modifies exactly one letter; else None.
        (
            "C".into(),
            Want::Ctrl(Some(InputEvent::Key {
                key: ch('c'),
                mods: M::CTRL,
                base_layout: None,
                layout_text: None,
                event_type: T::Press,
            })),
        ),
        ("".into(), Want::Ctrl(None)),
        ("ab".into(), Want::Ctrl(None)),
        // `meta` is its own bit: the inline-prefix form agrees with `mods=`.
        ("meta+x".into(), Want::SameAs("x mods=meta")),
    ];
    // Inline modifier+character combos build the SAME (Key::Character, mods)
    // event `parse_ctrl` does, so the encoder derives the control byte.
    for c in ['u', 'c', 'a', 'd', 'l', 'w'] {
        rows.push((format!("ctrl+{c}"), press(ch(c), M::CTRL)));
        rows.push((format!("ctrl+{c}"), Want::SameAsCtrl(c.to_string())));
    }
    for (input, want) in rows {
        let input = input.as_str();
        match want {
            Want::Key(event) => assert_eq!(parse_key(input), event, "key {input:?}"),
            Want::SameAs(other) => {
                assert_eq!(parse_key(input), parse_key(other), "{input:?} vs {other:?}")
            }
            Want::SameAsCtrl(other) => assert_eq!(
                parse_key(input),
                parse_ctrl(&other),
                "key {input} must equal ctrl {other}"
            ),
            Want::Ctrl(event) => assert_eq!(parse_ctrl(input), event, "ctrl {input:?}"),
        }
    }
}

/// `take_mods`, the `mods=` token every input verb shares, one labelled row per
/// input line.
///
/// * A line without `mods=` parses to empty mods and the untouched body.
/// * An unknown `mods=` name is refused, never dropped, matching the inline
///   `key CTRL+a` spelling and the sibling `type=`/`base=` tokens: a controller
///   cannot tell a sent chord from a silently stripped one. The `Accepted` rows
///   are the control.
/// * `meta` and `hyper` are their own modifier bits (Kitty), distinct from ALT.
#[test]
fn take_mods_grammar() {
    use aterm_types::keyboard::Modifiers as M;
    enum Want {
        /// `take_mods` yields exactly these mods and this body.
        Mods(M, &'static str),
        /// `take_mods` AND `parse_key` both refuse: never an unmodified key.
        Refused,
        /// `take_mods` still accepts (the refusal rows' positive control).
        Accepted,
    }
    let mut rows: Vec<(&str, Want)> = vec![
        // `take_mods` is additive: aliases, the comma separator and token
        // position-independence.
        ("up", Want::Mods(M::empty(), "up")),
        ("up mods=ctrl+shift", Want::Mods(M::CTRL | M::SHIFT, "up")),
        ("mods=cmd,alt end", Want::Mods(M::SUPER | M::ALT, "end")),
        // `meta` and `hyper` are their own bits; alt is still ALT, and meta no
        // longer aliases it (the inline `meta+x` form is a `key` grammar row).
        ("a mods=meta", Want::Mods(M::META, "a")),
        ("a mods=hyper", Want::Mods(M::HYPER, "a")),
        ("a mods=alt", Want::Mods(M::ALT, "a")),
        (
            "a mods=ctrl+meta+hyper",
            Want::Mods(M::CTRL | M::META | M::HYPER, "a"),
        ),
    ];
    // An unknown `mods=` name refuses...
    for bad in [
        "a mods=CTRL",
        "a mods=Shift",
        "a mods=shfit",
        "a mods=fn",
        "a mods=win",
        "a mods=ctrl+bogus",
        "up mods=ctrl,nope",
    ] {
        rows.push((bad, Want::Refused));
    }
    // ...and the empty list and no list at all still parse.
    for good in ["a mods=", "a"] {
        rows.push((good, Want::Accepted));
    }
    for (input, want) in rows {
        match want {
            Want::Mods(mods, body) => {
                let got = take_mods(input).expect("known names parse");
                assert_eq!((got.0, got.1.as_str()), (mods, body), "{input:?}");
            }
            Want::Refused => {
                assert!(
                    take_mods(input).is_none(),
                    "{input:?} must refuse, not deliver an unmodified key"
                );
                assert!(
                    parse_key(input).is_none(),
                    "{input:?} must not reach an InputEvent"
                );
            }
            Want::Accepted => {
                assert!(take_mods(input).is_some(), "{input:?} must still parse")
            }
        }
    }
}

/// The styled `screen` frame is lossless, one row per field family; every row also
/// checks the derived `dims` and `"seq":<content_seq>`.
///
/// * Every resolved decoration, including underline substyle, overline, underline
///   colour and the renderer's resolved rendition via the bold path.
/// * The glyph is the combining-aware grapheme (é = e+U+0301), and a cell's OSC-8
///   hyperlink target is surfaced.
/// * Inline images and DEC double-width/height line sizes; no selection / no image
///   stays null / empty. The selection object and one-glyph-per-cell count are
///   pinned in `control_query` (`styled_frame_selection_is_typed_and_side_adjusted`,
///   `styled_frame_glyphs_are_cell_grapheme_across_a_mixed_row`).
#[test]
fn screen_styled_frame_is_lossless() {
    use aterm_core::terminal::Terminal;
    /// (label, rows, cols, seed, needles)
    type Row<'a> = (&'a str, u16, u16, &'a [u8], &'a [&'a str]);
    let rows: &[Row] = &[
        (
            // bold + curly underline (SGR 4:3) + overline (SGR 53) + RGB
            // underline colour (SGR 58:2::255:0:0) applied to 'Z'.
            "decorations",
            3,
            10,
            b"\x1b[1m\x1b[4:3m\x1b[53m\x1b[58:2::255:0:0mZ",
            &[
                "\"underline_style\":\"curly\"",
                "\"overline\":true",
                "\"underline_color\":\"ff0000\"",
                "\"bold\"",
            ],
        ),
        ("shape", 3, 10, b"hi", &[]),
        (
            "grapheme and hyperlink",
            2,
            20,
            "e\u{0301}\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\".as_bytes(),
            &[
                "\"glyph\":\"e\u{0301}\"",
                "\"hyperlink\":\"https://example.com\"",
            ],
        ),
        (
            // F1: an inline image (OSC 1337 File=, 2x1) — PNG magic + 4 NULs;
            // F2: row 1 double-width (DECDWL, ESC # 6).
            "image and line size",
            3,
            10,
            b"\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\\x1b[2;1H\x1b#6",
            &[
                "\"images\":[{\"row\":0,\"col\":0,\"cols\":2,\"rows\":1,\"format\":\"png\",\"nbytes\":12,\"b64\":\"iVBORw0KGgoAAAAA\"}]",
                "\"double_width\"",
            ],
        ),
        ("plain", 2, 4, b"", &["\"selection\":null", "\"images\":[]"]),
    ];
    for &(label, r, c, seed, needles) in rows {
        let mut t = Terminal::new(r, c);
        t.process(seed);
        let frame = styled_frame_payload(&t);
        let dims = format!("\"dims\":{{\"rows\":{r},\"cols\":{c}}}");
        let seq = format!("\"seq\":{}", t.content_seq());
        for needle in needles.iter().copied().chain([dims.as_str(), seq.as_str()]) {
            assert!(frame.contains(needle), "{label}: {needle} lost: {frame}");
        }
    }
}

/// `screen` wraps the frame in the single-line `OK 1\n…\n` read framing; the JSON
/// body is one physical line, so the line-count client streams it at any grid size.
#[test]
fn screen_verb_framing_is_single_line_json() {
    use aterm_core::terminal::Terminal;
    let term = Arc::new(Mutex::new(Terminal::new(2, 4)));
    let out = cmd_screen_styled_json(&term, "");
    assert!(out.starts_with("OK 1\n"), "{out}");
    let body = out
        .strip_prefix("OK 1\n")
        .unwrap()
        .strip_suffix('\n')
        .unwrap();
    assert!(
        !body.contains('\n'),
        "styled frame must be single-line JSON: {body}"
    );
    assert!(
        body.starts_with("{\"seq\":") && body.ends_with('}'),
        "{body}"
    );
}

/// `screen` never sends a line its own client must reject
/// (`MAX_CONTROL_REPLY_LINE_BYTES`): a frame too big for it leaves out its image
/// payloads (marked `truncated`, with the real `nbytes`; `image read` still serves
/// each), and one too big even so is answered by an `ERR` line.
#[test]
fn screen_never_sends_a_line_past_the_reply_cap() {
    use aterm_core::terminal::Terminal;
    let cap = aterm_types::control_verbs::MAX_CONTROL_REPLY_LINE_BYTES;
    let fits = |out: &str| out.lines().all(|line| line.len() <= cap);
    // A grid whose cells alone pass the cap.
    let term = Arc::new(Mutex::new(Terminal::new(120, 300)));
    let out = cmd_screen_styled_json(&term, "");
    assert!(
        fits(&out) && out.starts_with("ERR screen:"),
        "{}",
        &out[..out.len().min(120)]
    );
    // Two ~3 MiB pictures on an ordinary screen.
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    for (row, fill) in [(1, 1u8), (12, 2)] {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.resize(3 << 20, fill);
        let b64 = aterm_codec::base64::encode(&png).expect("encodes");
        term.lock().unwrap().process(
            format!("\x1b[{row};1H\x1b]1337;File=inline=1;width=10;height=4:{b64}\x07").as_bytes(),
        );
    }
    let out = cmd_screen_styled_json(&term, "");
    assert!(
        out.starts_with("OK 1\n") && fits(&out),
        "{}",
        &out[..out.len().min(120)]
    );
    assert_eq!(
        out.matches("\"format\":\"truncated\"").count(),
        2,
        "both pictures named"
    );
    assert!(out.contains(&format!("\"nbytes\":{}", 3 << 20)));
}

/// A small inline image encodes normally; an oversized one (user-supplied OSC 1337)
/// is `truncated` with an empty payload but its real `nbytes`, so consumers learn
/// it exists without a multi-MB payload on every `image read` / styled frame.
#[test]
fn oversized_image_payload_is_truncated_not_encoded() {
    use aterm_core::grid::extra::{ImageData, ImageFormat, ImageScaling};
    let small = ImageData {
        bytes: vec![1, 2, 3, 4],
        format: ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    };
    let (fmt, b64) = image_payload(&small);
    assert_eq!(fmt, "png");
    assert!(!b64.is_empty(), "small image must carry its payload");

    let big = ImageData {
        bytes: vec![0u8; MAX_IMAGE_PAYLOAD_BYTES + 1],
        format: ImageFormat::Png,
        cols: 80,
        rows: 24,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    };
    let (fmt, b64) = image_payload(&big);
    assert_eq!(fmt, "truncated", "oversized image must be marked truncated");
    assert!(b64.is_empty(), "oversized image must NOT be base64-encoded");
    // The line form still reports the real size (so the consumer can decide).
    let line = image_read_line(0, 0, 0, 0, &big);
    assert!(
        line.contains(&format!("truncated {}", MAX_IMAGE_PAYLOAD_BYTES + 1)),
        "{line}"
    );
    // And the JSON form is well-formed with an empty payload + real nbytes.
    let js = styled_image_json(0, 0, &big);
    assert!(
        js.contains("\"format\":\"truncated\"") && js.contains("\"b64\":\"\""),
        "{js}"
    );
    assert!(
        js.contains(&format!("\"nbytes\":{}", MAX_IMAGE_PAYLOAD_BYTES + 1)),
        "{js}"
    );
}

/// `image read`, the headless framebuffer-free path, as exact wire replies.
///
/// An inline iTerm2 image (OSC 1337 `File=`) reads back as structured base64,
/// deduplicated across its cells; the input is a hand-computed literal (PNG magic +
/// 4 NUL bytes), so matching output checks `b64_encode` independently.
/// `image read <r> <c>` returns the covering tile with the queried cell's tile
/// coords; a cell with no image is `ERR none`; a screen with none is `OK 0`.
#[test]
fn image_read_replies_are_exact() {
    use aterm_core::terminal::Terminal;
    // 12 raw bytes = PNG magic (8) + 4×0x00; standard base64 = "iVBORw0KGgoAAAAA".
    const IMAGE: &[u8] = b"\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\";
    // (seed an image?, args, exact reply). A reply line is
    // <row> <col> <img_cols> <img_rows> <cell_row> <cell_col> <format> <nbytes> <b64>.
    let rows: &[(bool, &str, &str)] = &[
        // One deduped image line for the two cells it covers.
        (true, "", "OK 1\n0 0 2 1 0 0 png 12 iVBORw0KGgoAAAAA\n"),
        (false, "", "OK 0\n"),
        // Cell (0,1) is the right tile of the 2-wide image: cell_col == 1.
        (true, "0 1", "OK 1\n0 0 2 1 0 1 png 12 iVBORw0KGgoAAAAA\n"),
        (true, "0 1 extra", "ERR bad args\n"),
        (true, "0 5", "ERR none\n"),
        (true, "9 9", "ERR out of range: rows 0-2, cols 0-9\n"),
        (true, "7", "ERR out of range: rows 0-2, cols 0-9\n"),
    ];
    for &(seed, args, want) in rows {
        let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
        if seed {
            term_lock(&term).process(IMAGE);
        }
        assert_eq!(
            cmd_image_read(&term, args),
            want,
            "image read {args:?} (image seeded: {seed})"
        );
    }

    // Screen mode lists distinct images ROW-MAJOR, whatever order they were
    // placed in: `a` (2 wide) goes on row 1 FIRST, then `b` on row 0. The
    // screen walk reads every row's placements in one pass over the extras
    // map, whose iteration order is arbitrary, so this pins that the rows
    // still come back in order (and `a`'s second cell is deduplicated).
    const A: &[u8] = b"\x1b[2;1H\x1b]1337;File=inline=1;width=2;height=1:iVBORw0KGgoAAAAA\x1b\\";
    const B: &[u8] = b"\x1b[1;1H\x1b]1337;File=inline=1;width=1;height=1:iVBORw0KGgoAAAAB\x1b\\";
    let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
    term_lock(&term).process(A);
    term_lock(&term).process(B);
    assert_eq!(
        cmd_image_read(&term, ""),
        "OK 2\n0 0 1 1 0 0 png 12 iVBORw0KGgoAAAAB\n1 0 2 1 0 0 png 12 iVBORw0KGgoAAAAA\n",
        "screen mode is row-major, first-seen per image"
    );
    assert_eq!(
        cmd_image_read(&term, "1"),
        "OK 1\n1 0 2 1 0 0 png 12 iVBORw0KGgoAAAAA\n",
        "row mode reads that row alone"
    );
}

/// `image read <r> <c>` where Kitty images stack reports the top one: two one-cell
/// images of 800 and 200 bytes at (0, 0) read back as the second.
#[test]
fn image_read_of_a_stacked_cell_reports_the_top_image() {
    use aterm_core::terminal::Terminal;
    let term = Arc::new(Mutex::new(Terminal::new(3, 10)));
    {
        let mut t = term_lock(&term);
        t.set_cell_pixel_size(10, 20);
        for (id, w, h) in [(1, 10, 20), (2, 5, 10)] {
            let raw = vec![0x7Fu8; w * h * 4];
            let b64 = aterm_codec::base64::encode(&raw).expect("encode");
            t.process(format!("\x1b_Ga=T,f=32,s={w},v={h},i={id},C=1,q=2;{b64}\x1b\\").as_bytes());
        }
    }
    let reply = cmd_image_read(&term, "0 0");
    assert!(
        reply.starts_with("OK 1\n0 0 1 1 0 0 rgba 200 "),
        "the top image: {reply:?}"
    );
}

/// The cross-process forward decision: Owner-only, presents the per-op edge token,
/// rewrites the child selector to `@.`, and fails closed on an Edge scope, an
/// unknown child, or a relaunched (nonce-mismatched) child.
#[test]
fn proxy_forward_plan_owner_only_op_scoped_and_nonce_guarded() {
    use aterm_session::{EdgeToken, LaunchNonce, SessionId};
    let dir = std::env::temp_dir().join(format!("aterm-fwd-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let store = crate::session_store::new_store();
    let child = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let entry = crate::proxy::ProxyEntry {
        nonce,
        read: EdgeToken::generate(),
        write: EdgeToken::generate(),
        signal: EdgeToken::generate(),
    };
    let read_hex = entry.read.to_hex();
    let write_hex = entry.write.to_hex();
    crate::proxy::register_child(child.clone(), entry);
    // The published socket must live directly in `dir` (the runtime sock dir);
    // `confine_proxy_sock` rejects anything else, and the forward dials the
    // confined path.
    let child_sock = dir.join("aterm-child.sock");
    let child_sock_str = child_sock.to_string_lossy().into_owned();
    let confined = std::fs::canonicalize(&dir)
        .unwrap()
        .join("aterm-child.sock");
    let confined_str = confined.to_string_lossy().into_owned();
    crate::proxy::write_graph_entry(&dir, &child, &child_sock_str, &nonce);

    let line = format!("@{} screen", child.as_str());
    // Owner forwards: read verb → READ token, selector rewritten to @.
    let plan = proxy_forward_plan(&line, Scope::Owner, &store, &dir).expect("owner forwards");
    assert_eq!(plan.0, confined_str);
    assert_eq!(presented(&plan), format!("TOKEN {read_hex} @. screen\n"));
    // A write verb selects the WRITE token (op→token mapping).
    let kline = format!("@{} key up", child.as_str());
    let kplan = proxy_forward_plan(&kline, Scope::Owner, &store, &dir).expect("write forwards");
    assert_eq!(presented(&kplan), format!("TOKEN {write_hex} @. key up\n"));

    // The selector-second `subscribe` grammar is forwarded too, presenting the read
    // token, rewriting the child selector to `@.`, keeping streams and flags.
    let sline = format!("subscribe @{} cells,bytes every-frame", child.as_str());
    let splan = proxy_forward_plan(&sline, Scope::Owner, &store, &dir).expect("subscribe forwards");
    assert_eq!(splan.0, confined_str);
    assert_eq!(
        presented(&splan),
        format!("TOKEN {read_hex} subscribe @. cells,bytes every-frame\n")
    );
    // The rewrite replaces the selector, wherever it starts: the streams that
    // follow it reach the child whole.
    let spaced = format!("subscribe  @{} cells", child.as_str());
    let spaced_plan = proxy_forward_plan(&spaced, Scope::Owner, &store, &dir).expect("forwards");
    assert_eq!(
        presented(&spaced_plan),
        format!("TOKEN {read_hex} subscribe @. cells\n")
    );
    // A subscribe with a comma-list of targets stays local (not a single child).
    let mixed = format!("subscribe @.,@{} cells", child.as_str());
    assert!(proxy_forward_plan(&mixed, Scope::Owner, &store, &dir).is_none());
    // An unregistered child id → no plan.
    let other = format!("@{} screen", SessionId::generate().as_str());
    assert!(proxy_forward_plan(&other, Scope::Owner, &store, &dir).is_none());

    // A relaunch (graph entry under a NEW nonce) fails closed.
    crate::proxy::write_graph_entry(&dir, &child, &child_sock_str, &LaunchNonce::generate());
    assert!(
        proxy_forward_plan(&line, Scope::Owner, &store, &dir).is_none(),
        "nonce mismatch must fail closed",
    );

    // A graph entry redirected outside our runtime dir (a same-uid overwrite that
    // copies the readable nonce) fails closed: the parent neither dials it nor
    // presents the token. The correct nonce is restored so only the path trips.
    crate::proxy::write_graph_entry(&dir, &child, "/tmp/evil-attacker.sock", &nonce);
    assert!(
        proxy_forward_plan(&line, Scope::Owner, &store, &dir).is_none(),
        "out-of-dir socket path must fail closed (token must not leak)",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Sibling hop: an `@<sid>` neither hosted locally nor a spawned child but published
/// in the graph by another live same-uid instance forwards to that instance's
/// socket, presenting the sibling's own per-launch token and keeping the selector
/// so the sibling resolves it among its tabs. Fail-closed guards: Edge scope,
/// locally hosted sid, out-of-dir socket, dead listener, unreadable token, non-op
/// verbs. (The self-dial guard: `sibling_forward_plan_conforms_to_proxy_forward_one_hop`.)
#[test]
#[cfg(unix)]
fn sibling_forward_plan_presents_instance_token_and_keeps_selector() {
    use aterm_session::{LaunchNonce, SessionId};
    // Serialize with every other test touching the process-global self sock.
    let _guard = crate::proxy::self_sock_test_guard();
    let dir = std::env::temp_dir().join(format!("aterm-sib-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let store = crate::session_store::new_store();
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();

    // The "sibling": a LIVE listener at <dir>/aterm-77001.sock + its 0600 token.
    let sib_sock = dir.join("aterm-77001.sock");
    let sib_sock_str = sib_sock.to_string_lossy().into_owned();
    let _ = std::fs::remove_file(&sib_sock);
    let _listener = std::os::unix::net::UnixListener::bind(&sib_sock).expect("bind sibling");
    std::fs::write(dir.join("aterm-77001.token"), "cafe0123\n").expect("token");
    crate::proxy::write_graph_entry(&dir, &sid, &sib_sock_str, &nonce);

    let confined = std::fs::canonicalize(&dir)
        .unwrap()
        .join("aterm-77001.sock")
        .to_string_lossy()
        .into_owned();

    // Owner + live sibling + op verb → forward, selector KEPT, sibling token.
    let line = format!("@{} text", sid.as_str());
    let plan = proxy_forward_plan(&line, Scope::Owner, &store, &dir).expect("sibling forwards");
    assert_eq!(plan.0, confined);
    assert_eq!(
        presented(&plan),
        format!("TOKEN cafe0123 @{} text\n", sid.as_str())
    );

    // Selector-SECOND subscribe grammar: forwarded verbatim (selector kept).
    let sline = format!("subscribe @{} cells,bytes", sid.as_str());
    let splan = proxy_forward_plan(&sline, Scope::Owner, &store, &dir).expect("subscribe forwards");
    assert_eq!(splan.0, confined);
    assert_eq!(
        presented(&splan),
        format!("TOKEN cafe0123 subscribe @{} cells,bytes\n", sid.as_str())
    );
    // `graph/<sid>` is joined verbatim, so a path spelling reaches the same
    // entry and is relayed too.
    let pline = format!("subscribe @./{} cells", sid.as_str());
    assert!(
        proxy_forward_plan(&pline, Scope::Owner, &store, &dir).is_some(),
        "a path spelling of the sid reaches its entry"
    );
    // `@*` is this instance's live target set (the bridge's and fleet's
    // subscription), never one session: an entry named `*` relays it nowhere.
    crate::proxy::write_graph_entry(&dir, &SessionId::new("*"), &sib_sock_str, &nonce);
    let all = "subscribe @* events,sessions";
    assert!(
        proxy_forward_plan(all, Scope::Owner, &store, &dir).is_none(),
        "`subscribe @*` stays local"
    );

    // Edge scope must NOT reach a sibling.
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    for relayed in [&line, &sline] {
        assert!(
            proxy_forward_plan(relayed, edge, &store, &dir).is_none(),
            "edge scope must not forward to a sibling",
        );
    }

    // A non-op verb (identity/privilege) never crosses processes.
    let gline = format!("@{} whoami", sid.as_str());
    assert!(proxy_forward_plan(&gline, Scope::Owner, &store, &dir).is_none());

    // A locally hosted sid is not a hop (in-process resolution owns it).
    let local = registered_session(91, -1, b"");
    let local_line = format!("@{} text", local.sid.as_str());
    crate::proxy::write_graph_entry(&dir, &local.sid, &sib_sock_str, &local.nonce);
    store.write().unwrap().register(local.clone());
    assert!(
        proxy_forward_plan(&local_line, Scope::Owner, &store, &dir).is_none(),
        "locally-hosted sid must resolve in-process, never hop",
    );

    // CONFINEMENT on the SIBLING hop: an entry redirected OUTSIDE our runtime
    // dir must never be dialed (same posture as the child hop).
    let outside = SessionId::generate();
    crate::proxy::write_graph_entry(&dir, &outside, "/tmp/evil-sibling.sock", &nonce);
    let out_line = format!("@{} text", outside.as_str());
    assert!(
        proxy_forward_plan(&out_line, Scope::Owner, &store, &dir).is_none(),
        "an out-of-dir sibling socket must fail closed",
    );

    // DEAD listener: entry present, socket file present, nobody listening.
    let dead = SessionId::generate();
    let dead_sock = dir.join("aterm-77002.sock");
    let _ = std::fs::remove_file(&dead_sock);
    // Bound but never listening: a listening descriptor in a shared test process
    // can be inherited by another test's child between listen and close, and the
    // kernel keeps accepting through that duplicate until its execve. Raw
    // socket+bind+close never listens and answers ECONNREFUSED, like a crashed
    // sibling's leftover socket file.
    // SAFETY: three straight-line libc calls on a path the std bind here already
    // proved fits sun_path; every return is checked.
    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
        assert!(fd >= 0, "socket(AF_UNIX)");
        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = dead_sock.as_os_str().as_encoded_bytes();
        assert!(bytes.len() < addr.sun_path.len(), "sun_path fits");
        for (slot, byte) in addr.sun_path.iter_mut().zip(bytes) {
            *slot = *byte as libc::c_char;
        }
        let bound = libc::bind(
            fd,
            std::ptr::from_ref(&addr).cast::<libc::sockaddr>(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        );
        assert_eq!(bound, 0, "bind never-listening dead socket");
        assert_eq!(libc::close(fd), 0, "close bound socket");
    }
    std::fs::write(dir.join("aterm-77002.token"), "beef\n").expect("token");
    crate::proxy::write_graph_entry(&dir, &dead, &dead_sock.to_string_lossy(), &nonce);
    let dead_line = format!("@{} text", dead.as_str());
    // PRECONDITION, stated rather than assumed: the kernel must answer this
    // dial with ECONNREFUSED. `socket_is_live` deliberately treats ANY OTHER
    // connect error (EMFILE under a saturated test process, EINTR, EAGAIN) as
    // "maybe live — never hijack", so under load the plan below can legitimately
    // return `Some` for reasons that are the environment's, not the code's.
    // Wait out that transient instead of reporting it as a proxy defect.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "PRECONDITION: the dead sibling socket answers ECONNREFUSED (transient connect \
         errors persisting are the environment, not the proxy)",
        || match aterm_uds::CtlStream::connect(&dead_sock) {
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Some(()),
            Err(_) => None,
            Ok(_) => panic!("nobody should be listening on the dead sibling socket"),
        },
    );
    assert!(
        proxy_forward_plan(&dead_line, Scope::Owner, &store, &dir).is_none(),
        "a dead sibling must fail closed, not hang a dial",
    );

    // UNREADABLE token: live listener but no token file → no forward (the
    // relay must never dial unauthenticated).
    let toothless = SessionId::generate();
    let tl_sock = dir.join("aterm-77003.sock");
    let _tl = std::os::unix::net::UnixListener::bind(&tl_sock).expect("bind");
    crate::proxy::write_graph_entry(&dir, &toothless, &tl_sock.to_string_lossy(), &nonce);
    let tl_line = format!("@{} text", toothless.as_str());
    assert!(
        proxy_forward_plan(&tl_line, Scope::Owner, &store, &dir).is_none(),
        "a sibling with no readable token must fail closed",
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Tier-1 conformance of the router to `dispatch_complete_model`'s
/// `ForwardableRemoteAlwaysForwarded`: for an Owner reaching a registered remote
/// child, `proxy_forward_plan` forwards a verb iff it is forwardable (the op-bearing
/// verbs: `required_op` ⇒ read/write/signal). Drives the real decision over every
/// verb class; letting a forwardable verb fall to the local path fails it. (The
/// selector-second `subscribe` forward:
/// `proxy_forward_plan_owner_only_op_scoped_and_nonce_guarded`.)
#[test]
fn proxy_forward_plan_conforms_to_dispatch_model() {
    use aterm_session::{EdgeToken, LaunchNonce, SessionId};
    let dir = std::env::temp_dir().join(format!("aterm-conform-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let store = crate::session_store::new_store();
    let child = SessionId::generate();
    let nonce = LaunchNonce::generate();
    crate::proxy::register_child(
        child.clone(),
        crate::proxy::ProxyEntry {
            nonce,
            read: EdgeToken::generate(),
            write: EdgeToken::generate(),
            signal: EdgeToken::generate(),
        },
    );
    // The published socket must live directly in `dir` — `confine_proxy_sock`
    // (the proxy forward's anti-token-capture guard) rejects any out-of-dir path.
    let child_sock = dir.join("aterm-child.sock").to_string_lossy().into_owned();
    crate::proxy::write_graph_entry(&dir, &child, &child_sock, &nonce);
    let sid = child.as_str();

    // Representative verb lines across EVERY class: forwardable read-side,
    // forwardable write-side, signal, and NON-forwardable (owner-only / global).
    let cases: &[&str] = &[
        "screen",
        "text",
        "cell 0 0",
        "search x",
        "modes",
        "image read",
        "cast",
        "temporal",
        "scroll up",
        "key up",
        "ctrl c",
        "feed 03",
        "feed-bin 4",
        "paste hi",
        "resize 10 20",
        "focus in",
        "send hi",
        "signal int", // forwardable (read/write/signal)
        "grant x",
        "revoke x",
        "whoami",
        "sessions",
        "version",
        "bogus", // NOT forwardable
    ];
    for verb_line in cases {
        let verb = verb_line.split_whitespace().next().unwrap();
        let line = format!("@{sid} {verb_line}");
        let forwarded = proxy_forward_plan(&line, Scope::Owner, &store, &dir).is_some();
        let forwardable = required_op(verb).is_some();
        assert_eq!(
            forwarded, forwardable,
            "router ≠ model for `{verb}`: forwarded={forwarded} forwardable={forwardable}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The line a planned forward presents, `TOKEN <token> <verb>`, with the token the
/// forward takes once connected ([`crate::proxy::ForwardLine::token_at`]): a child's
/// edge token as planned, or a sibling's read beside its socket.
fn presented((sock, line): &(String, crate::proxy::ForwardLine)) -> String {
    let token = line
        .token_at(sock)
        .unwrap_or_else(|e| panic!("{sock}: no token to present: {e}"));
    crate::proxy::forward_first_line(&token, &line.verb)
}

/// A spawned child this process can forward to: registered in the proxy table with
/// fresh per-op edge tokens and published by a graph entry naming
/// `<dir>/aterm-child.sock` (directly in `dir`, as `confine_proxy_sock` requires).
/// Returns the child's id and the capability held over it.
fn forwardable_child(
    dir: &std::path::Path,
) -> (aterm_session::SessionId, crate::proxy::ProxyEntry) {
    use aterm_session::{LaunchNonce, SessionId};
    let child = SessionId::generate();
    let entry = forwardable_child_as(dir, &child, LaunchNonce::generate());
    (child, entry)
}

/// [`forwardable_child`] for a child whose identity is given — the
/// `(sid, nonce)` of a session a test also stands up as the child.
fn forwardable_child_as(
    dir: &std::path::Path,
    child: &aterm_session::SessionId,
    nonce: aterm_session::LaunchNonce,
) -> crate::proxy::ProxyEntry {
    use aterm_session::EdgeToken;
    let _ = std::fs::create_dir_all(dir);
    let entry = crate::proxy::ProxyEntry {
        nonce,
        read: EdgeToken::generate(),
        write: EdgeToken::generate(),
        signal: EdgeToken::generate(),
    };
    crate::proxy::register_child(child.clone(), entry.clone());
    let sock = dir.join("aterm-child.sock").to_string_lossy().into_owned();
    crate::proxy::write_graph_entry(dir, child, &sock, &entry.nonce);
    entry
}

/// Tier-1 conformance of the forward decision to
/// `aterm_models_session::no_transitive_authority_model` (`NoTransitiveAuthority`): a
/// forward is permitted only for an Owner-class connection, so a connection that
/// arrived over a forward can never start another hop.
///
/// For read, write, signal and subscribe lines aimed at a forwardable child,
/// `proxy_forward_plan` is driven for three connections: the instance token (Owner),
/// the inherited bridge fd, and one that arrived over the forward, whose scope is
/// what the child's real connect-time auth (`first_line_scope`) makes of the
/// Owner plan's first line, against a child table holding the parent edges
/// `spawn::install_parent_edges` put there. The model's `owner` guard comes from
/// what each scope is, never from `is_owner_class` (the code under test);
/// `forwarded` is the real plan. Every `Arrive`/`Forward` step must be admitted.
///
/// Negative control: `Buggy = 1` (owner guard waived) must reject the steps, since
/// the real router denies forwards the mutant permits. Removing the
/// `!scope.is_owner_class()` early return from `proxy_forward_plan` fails this.
#[test]
fn proxy_forward_plan_conforms_to_no_transitive_authority() {
    use aterm_spec::verify::validate_transition_tiered;
    use std::collections::BTreeMap;
    let dir = std::env::temp_dir().join(format!("aterm-nta-{}", std::process::id()));
    let store = crate::session_store::new_store();
    // The child, as a session of its own: its identity is the one the
    // parent registered, and its table holds the edges the parent granted.
    let at_child = registered_session(94, -1, b"");
    let entry = forwardable_child_as(&dir, &at_child.sid, at_child.nonce);
    let installed = crate::spawn::install_parent_edges(
        &mut at_child.ctx.edges.lock().unwrap(),
        &at_child.sid,
        &at_child.nonce,
        Some(aterm_session::SessionId::generate().as_str()),
        Some(&entry.read.to_hex()),
        Some(&entry.write.to_hex()),
        Some(&entry.signal.to_hex()),
    );
    assert_eq!(installed, 3, "the child holds all three parent edges");
    let child_instance_token = "0f".repeat(32);
    let sid = at_child.sid.as_str();
    let lines = [
        format!("@{sid} screen"),
        format!("@{sid} key up"),
        format!("@{sid} signal int"),
        format!("subscribe @{sid} cells,bytes"),
    ];
    // What each scope IS (see the doc): stated here, not read from the router.
    let owner_authority = |scope: Scope| match scope {
        Scope::Owner | Scope::Bridge => true,
        Scope::Edge(_) => false,
    };
    let model = aterm_models_session::no_transitive_authority_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let init = model.init_state();
    let mut buggy_rejections = 0;
    for line in &lines {
        let first_line = presented(
            &proxy_forward_plan(line, Scope::Owner, &store, &dir)
                .unwrap_or_else(|| panic!("`{line}`: the Owner must reach a registered child")),
        );
        let (arrived_scope, _, _) = first_line_scope(
            first_line.trim_end_matches('\n'),
            &child_instance_token,
            || Some(at_child.ctx.clone()),
        )
        .unwrap_or_else(|why| panic!("`{first_line}` does not authenticate at the child: {why}"));
        assert!(
            matches!(arrived_scope, Scope::Edge(_)),
            "`{first_line}`: a forwarded connection must arrive with an edge scope"
        );
        for (who, scope) in [
            ("owner", Scope::Owner),
            ("bridge", Scope::Bridge),
            ("arrived", arrived_scope),
        ] {
            let owner = owner_authority(scope);
            let forwarded = proxy_forward_plan(line, scope, &store, &dir).is_some();
            let arrived = BTreeMap::from([("owner", i64::from(owner)), ("forwarded", 0)]);
            let decided = BTreeMap::from([
                ("owner", i64::from(owner)),
                ("forwarded", if forwarded { 2 } else { 1 }),
            ]);
            let label = "NoTransitiveAuthority(proxy_forward_plan)";
            let (ok, why) =
                validate_transition_tiered(&model, &[], &init, &arrived, Some("Arrive"), label);
            assert!(ok, "{who} `{line}`: Arrive rejected — {why}");
            let (ok, why) =
                validate_transition_tiered(&model, &[], &arrived, &decided, Some("Forward"), label);
            assert!(
                ok,
                "{who} `{line}`: the router forwarded={forwarded}, which the model does \
                 not admit — {why}"
            );
            assert!(model.check_invariant("ForwardImpliesOwner", &decided));
            assert_eq!(
                forwarded, owner,
                "{who} `{line}`: an Owner-class connection must reach a registered child"
            );
            let (admitted, _) = validate_transition_tiered(
                &buggy,
                &[],
                &arrived,
                &decided,
                Some("Forward"),
                "NoTransitiveAuthority(Buggy=1)",
            );
            buggy_rejections += usize::from(!admitted);
        }
    }
    assert_eq!(
        buggy_rejections,
        lines.len(),
        "the owner-waiving mutant must part from the real router on exactly the \
         arrived connection's lines"
    );
    crate::proxy::deregister_child(&at_child.sid);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tier-1 conformance of the child hop to `aterm_models_gui::proxy_forward_model`
/// (`ProxyForward`): a chain is at most one cross-process hop, because the parent
/// rewrites the child's selector to `@.` so the child runs the verb itself. (The
/// sibling hop keeps its selector and ends differently:
/// `sibling_forward_plan_conforms_to_proxy_forward_one_hop`.)
///
/// For each forwardable line shape, the verb the child runs is the planned first
/// line past its `TOKEN <edge-hex>` handshake. Whether the chain stays `active` is
/// asked of the real router in the worst context (this process, where the child's
/// sid is forwardable) with Owner scope, so only the rewrite can end the chain. The
/// hop is validated as `Forward` and must leave `OneHopNoCycle` unthreatened.
///
/// Negative control: `Buggy = 1` (original selector relayed) must reject every real
/// hop; relaying the original line instead of `@.` makes the re-plan forward again.
#[test]
fn proxy_forward_plan_conforms_to_proxy_forward_one_hop() {
    use aterm_spec::verify::validate_transition_tiered;
    use std::collections::BTreeMap;
    let dir = std::env::temp_dir().join(format!("aterm-pfw-{}", std::process::id()));
    let store = crate::session_store::new_store();
    let (child, _) = forwardable_child(&dir);
    let sid = child.as_str();
    let model = aterm_models_gui::proxy_forward_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let before = model.init_state();
    assert_eq!(before, BTreeMap::from([("depth", 0), ("active", 1)]));
    for line in [
        format!("@{sid} screen"),
        format!("@{sid} key up"),
        format!("@{sid} signal int"),
        format!("subscribe @{sid} cells,bytes every-frame"),
    ] {
        let first_line = presented(
            &proxy_forward_plan(&line, Scope::Owner, &store, &dir)
                .unwrap_or_else(|| panic!("`{line}`: a registered child must be forwarded to")),
        );
        let at_child = first_line
            .trim_end_matches('\n')
            .splitn(3, ' ')
            .nth(2)
            .unwrap_or_else(|| panic!("`{first_line}` is not `TOKEN <hex> <verb>`"));
        let onward = proxy_forward_plan(at_child, Scope::Owner, &store, &dir).is_some();
        let after = BTreeMap::from([("depth", 1), ("active", i64::from(onward))]);
        let (ok, why) = validate_transition_tiered(
            &model,
            &[],
            &before,
            &after,
            Some("Forward"),
            "ProxyForward(proxy_forward_plan)",
        );
        assert!(
            ok,
            "`{line}` reaches the child as `{at_child}`, which would forward on \
             (onward={onward}); the model admits no such hop — {why}"
        );
        assert!(
            !model.action_enabled("Forward", &after),
            "`{line}`: a second hop is possible after `{at_child}`"
        );
        assert!(model.check_invariant("OneHopNoCycle", &after));
        let (admitted, _) = validate_transition_tiered(
            &buggy,
            &[],
            &before,
            &after,
            Some("Forward"),
            "ProxyForward(Buggy=1)",
        );
        assert!(
            !admitted,
            "`{line}`: the re-forwarding mutant admitted the real hop, so this \
             conformance cannot tell the two apart"
        );
    }
    crate::proxy::deregister_child(&child);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tier-1 conformance of the sibling hop to `aterm_models_gui::proxy_forward_model`
/// (`ProxyForward`). This hop keeps the `@<sid>` selector, so what ends the chain is
/// the sibling: the entry it followed was published by the instance at that socket,
/// so there the sid is hosted locally, or (a stale entry) names the sibling's own
/// socket, which `resolve_sibling`'s self-dial guard refuses.
///
/// The onward question is therefore asked in the sibling's context: the relayed line
/// past its `TOKEN <sibling-token>` handshake (the token read beside the socket once
/// connected, `ForwardLine::token_at`, so Owner there), planned against the
/// sibling's store (hosting the sid, and not) with the recorded self socket set to
/// the sibling's. Each hop is validated as `Forward` with `active` = whether the
/// sibling would forward on.
///
/// Negative control: `Buggy = 1` (chain continues) must reject every real hop;
/// deleting the self-dial guard makes the stale-entry case forward back to itself.
#[test]
#[cfg(unix)]
fn sibling_forward_plan_conforms_to_proxy_forward_one_hop() {
    use aterm_session::{LaunchNonce, SessionId};
    use aterm_spec::verify::validate_transition_tiered;
    use std::collections::BTreeMap;
    // Serialize with every other test touching the process-global self sock.
    let _guard = crate::proxy::self_sock_test_guard();
    crate::proxy::clear_self_sock();
    let dir = std::env::temp_dir().join(format!("aterm-sib1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    // The sibling: a live listener, its 0600 token, and its entries (one for a
    // session it hosts, one left for a session it no longer hosts).
    let sib_sock = dir.join("aterm-77011.sock");
    let sib_sock_str = sib_sock.to_string_lossy().into_owned();
    let _listener = std::os::unix::net::UnixListener::bind(&sib_sock).expect("bind sibling");
    std::fs::write(dir.join("aterm-77011.token"), "cafe0123\n").expect("token");
    let hosted = registered_session(93, -1, b"");
    crate::proxy::write_graph_entry(&dir, &hosted.sid, &sib_sock_str, &hosted.nonce);
    let stale = SessionId::generate();
    crate::proxy::write_graph_entry(&dir, &stale, &sib_sock_str, &LaunchNonce::generate());
    let here = crate::session_store::new_store();
    let hosting = crate::session_store::new_store();
    hosting.write().unwrap().register(hosted.clone());
    let emptied = crate::session_store::new_store();

    let model = aterm_models_gui::proxy_forward_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let before = model.init_state();
    for (what, sid, sibling_store) in [
        ("hosted there", hosted.sid.clone(), &hosting),
        ("a stale entry", stale, &emptied),
    ] {
        let sid = sid.as_str().to_string();
        for line in [
            format!("@{sid} screen"),
            format!("@{sid} key up"),
            format!("subscribe @{sid} cells,bytes"),
        ] {
            let first_line = presented(
                &proxy_forward_plan(&line, Scope::Owner, &here, &dir).unwrap_or_else(|| {
                    panic!("{what} `{line}`: a live sibling must be forwarded to")
                }),
            );
            let at_sibling = first_line
                .trim_end_matches('\n')
                .strip_prefix("TOKEN cafe0123 ")
                .unwrap_or_else(|| panic!("`{first_line}` does not present the sibling's token"));
            // Become the sibling for the onward question.
            crate::proxy::set_self_sock(&dir, &sib_sock_str);
            let onward = proxy_forward_plan(at_sibling, Scope::Owner, sibling_store, &dir);
            crate::proxy::clear_self_sock();
            let after = BTreeMap::from([("depth", 1), ("active", i64::from(onward.is_some()))]);
            let (ok, why) = validate_transition_tiered(
                &model,
                &[],
                &before,
                &after,
                Some("Forward"),
                "ProxyForward(proxy_forward_plan sibling hop)",
            );
            assert!(
                ok,
                "{what} `{line}` reaches the sibling as `{at_sibling}`, which it would \
                 forward on to {onward:?}; the model admits no such hop — {why}"
            );
            assert!(model.check_invariant("OneHopNoCycle", &after));
            let (admitted, _) = validate_transition_tiered(
                &buggy,
                &[],
                &before,
                &after,
                Some("Forward"),
                "ProxyForward(Buggy=1)",
            );
            assert!(
                !admitted,
                "{what} `{line}`: the re-forwarding mutant admitted the real hop"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A forward planned against a sibling's listener and token, where an update's
/// successor takes the socket first (token file rewritten, then the same path
/// bound), must present the token beside the socket once connected, the
/// successor's; the plan-time token is refused `ERR auth` there, and the caller is
/// answered `ERR forward`.
#[test]
#[cfg(unix)]
fn a_sibling_replaced_after_the_plan_is_sent_its_own_token() {
    use aterm_session::{LaunchNonce, SessionId};
    use std::io::{BufRead, BufReader, Write};
    let dir = std::env::temp_dir().join(format!("aterm-sibrep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let store = crate::session_store::new_store();
    let sid = SessionId::generate();
    let sock = dir.join("aterm-77021.sock");
    let token_file = dir.join("aterm-77021.token");
    std::fs::write(&token_file, "deadbeef\n").expect("the sibling's token");
    let predecessor = aterm_uds::CtlListener::bind(&sock).expect("bind the sibling");
    let sock_text = sock.to_string_lossy().into_owned();
    crate::proxy::write_graph_entry(&dir, &sid, &sock_text, &LaunchNonce::generate());
    let line = format!("@{} text", sid.as_str());
    let plan = proxy_forward_plan(&line, Scope::Owner, &store, &dir)
        .expect("a live sibling is forwarded to");

    std::fs::remove_file(&token_file).expect("unlink the old token");
    std::fs::write(&token_file, "cafef00d\n").expect("the successor's token");
    drop(predecessor);
    std::fs::remove_file(&sock).expect("unlink the old socket");
    let successor = aterm_uds::CtlListener::bind(&sock).expect("the successor binds");
    let served = std::thread::spawn(move || {
        let (conn, _) = successor.accept().expect("the forward's connection");
        let mut reader = BufReader::new(conn.try_clone().expect("clone"));
        let mut probe = String::new();
        reader.read_line(&mut probe).expect("the connect probe");
        if probe != "TOKEN cafef00d version\n" {
            (&conn).write_all(b"ERR auth\n").expect("refusal");
            return probe;
        }
        (&conn)
            .write_all(b"OK version=successor\n")
            .expect("probe reply");
        let mut verb = String::new();
        reader.read_line(&mut verb).expect("the verb");
        (&conn).write_all(b"OK served\n").expect("reply");
        let _ = conn.shutdown(std::net::Shutdown::Both);
        probe
    });
    let (client_app, client_relay) = CtlStream::pair().expect("pair");
    let forward = std::thread::spawn(move || {
        crate::proxy::connect_and_relay(&plan.0, &plan.1, &client_relay, &[])
    });
    // The reply ends either way (the relayed answer, or EOF once a failed
    // forward drops its end), and the forward with it once the client
    // hangs up — for the whole socket ([`hang_up`]): the relay has already
    // half-closed toward it, and a sibling's fork may hold a copy of its
    // end. The successor is joined last, since a forward that never
    // connected leaves it parked in `accept`.
    client_app
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("a hang detector on the reply");
    let mut reply = String::new();
    let _ = BufReader::new(client_app.try_clone().expect("clone")).read_line(&mut reply);
    hang_up(client_app);
    let forwarded = join_after_hang_up(forward, "the forward to end on its client's hang-up");
    assert!(
        forwarded.is_ok(),
        "the successor refused the forward: {forwarded:?}"
    );
    assert_eq!(reply, "OK served\n");
    let probe = join_after_hang_up(served, "the successor to finish serving");
    assert_eq!(
        probe, "TOKEN cafef00d version\n",
        "the successor was sent the token beside its socket at plan time"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Tier-1 conformance of the forward path to
/// `aterm_models_session::forward_handshake_model` (`ForwardHandshake`), its liveness
/// twin: the server holds a request the child must receive while the client waits
/// for the reply, so relaying must not wait on a fresh client read (e.g.
/// `fill_buf()` on the common empty buffer would wedge every forward).
///
/// End to end: a client sends one `@<child> screen` line and waits; the server reads
/// it as the serve loop does and calls `try_proxy_forward`; a stand-in child reports
/// the handshake it receives (`Relay`: buffered -> relayed) and answers; the client
/// reading it is `ClientRecv`. Each step is validated against the model, which must
/// end in `Done`.
///
/// Negative control: `Buggy = 1` (a fresh read before the first relay) disables
/// `Relay`; with `fill_buf()` in `drain_buffered` the child never gets the request
/// and the test reports the parked state.
#[test]
#[cfg(unix)]
fn try_proxy_forward_conforms_to_forward_handshake() {
    use aterm_spec::verify::validate_transition_tiered;
    use std::io::{BufRead, BufReader, Write};
    let dir = std::env::temp_dir().join(format!("aterm-fhs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (child, entry) = forwardable_child(&dir);
    let listener =
        aterm_uds::CtlListener::bind(dir.join("aterm-child.sock")).expect("bind the child");
    let (handshake_tx, handshake_rx) = std::sync::mpsc::channel::<String>();
    let child_side = std::thread::spawn(move || {
        let (conn, _) = listener.accept().expect("accept the forward");
        let mut reader = BufReader::new(conn.try_clone().expect("clone"));
        // The forward's connect probe comes first and is answered; the
        // handshake the model speaks of is the probe plus the verb.
        let mut probe = String::new();
        reader.read_line(&mut probe).expect("read the handshake");
        (&conn)
            .write_all(b"OK version=child\n")
            .expect("answer the probe");
        let mut verb = String::new();
        reader.read_line(&mut verb).expect("read the verb");
        let _ = handshake_tx.send(format!("{probe}{verb}"));
        (&conn).write_all(b"OK served\n").expect("answer");
        let _ = conn.shutdown(std::net::Shutdown::Both);
    });

    let (client, server_end) = CtlStream::pair().expect("pair");
    // The client MUST be served: a hang detector, never a latency budget.
    client
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("client deadline");
    (&client)
        .write_all(format!("@{} screen\n", child.as_str()).as_bytes())
        .expect("send the request");
    let store = crate::session_store::new_store();
    let server_dir = dir.clone();
    let server = std::thread::spawn(move || {
        let mut reader = BufReader::new(&server_end);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read the request");
        let line = line.trim_end_matches('\n');
        try_proxy_forward(
            line,
            Scope::Owner,
            &store,
            &server_dir,
            &server_end,
            &mut reader,
        )
    });

    let model = aterm_models_session::forward_handshake_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let label = "ForwardHandshake(try_proxy_forward)";
    let parked = model.init_state();
    let step = |before: &std::collections::BTreeMap<&'static str, i64>,
                action: &str,
                set: &[(&'static str, i64)]| {
        let mut after = before.clone();
        for (var, value) in set {
            after.insert(*var, *value);
        }
        let (ok, why) =
            validate_transition_tiered(&model, &[], before, &after, Some(action), label);
        assert!(
            ok,
            "the real forward took `{action}` to {after:?}, not admitted — {why}"
        );
        after
    };

    let handshake = handshake_rx.recv_timeout(EVENTUALLY);
    let Ok(handshake) = handshake else {
        // Hang up so the parked server can finish on its own, then report
        // the wedge in the model's terms (without joining a thread that may
        // still be parked).
        hang_up(client);
        let _ = std::fs::remove_dir_all(&dir);
        panic!(
            "the real forward is parked at {parked:?} with the client waiting: the \
             committed model enables `Relay` there (enabled={}), only Buggy=1 \
             wedges (enabled={}) — a fresh read is being demanded before the relay",
            model.action_enabled("Relay", &parked),
            buggy.action_enabled("Relay", &parked),
        );
    };
    assert!(
        !buggy.action_enabled("Relay", &parked),
        "the mutant must be unable to take the step the real forward took"
    );
    assert_eq!(
        handshake,
        format!("TOKEN {} version\n@. screen\n", entry.read.to_hex()),
        "the child receives the edge handshake (with the connect probe) and \
         the rewritten verb"
    );
    let relayed = step(&parked, "Relay", &[("buffered", 0), ("relayed", 1)]);

    let mut reply = String::new();
    BufReader::new(&client)
        .read_line(&mut reply)
        .expect("the client is served");
    assert_eq!(reply, "OK served\n");
    let served = step(&relayed, "ClientRecv", &[("client_waiting", 0)]);
    assert!(
        model.action_enabled("Done", &served),
        "the served state is the work-complete terminal"
    );

    // Hang up (for the whole socket, [`hang_up`]: a `drop` leaves the
    // socket open while a sibling's fork holds a copy of the client end, and
    // the both-ways `shutdown` Darwin refuses once the relay half-closed
    // toward the client shut nothing), so the relay's client-to-child pump
    // hits EOF and the forward returns.
    hang_up(client);
    assert!(
        join_after_hang_up(server, "the forward to end on its client's hang-up"),
        "the forward owned the line"
    );
    join_after_hang_up(child_side, "the stand-in child to finish serving");
    crate::proxy::deregister_child(&child);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `parse_mouse`: the additive `mods=`/`count=`/`side=`/`block=` grammar, one
/// labelled row per input line.
///
/// `mods=` refuses what the mouse report cannot carry, as `key` refuses unknown
/// names: legacy/SGR reports have bits only for shift, alt/meta and ctrl, so
/// `super` / `cmd` / `command` / `hyper` (valid for `key`) are refused rather than
/// sending an unmodified press.
#[test]
fn mouse_grammar_and_mods_refusal() {
    use aterm_core::selection::SelectionSide;
    use aterm_types::mouse::{ALT_MASK, MouseButton, SHIFT_MASK};
    enum Want {
        /// Parses to exactly this event.
        Event(InputEvent),
        /// Parses to a press whose `click_count` is this.
        ClickCount(u8),
        /// Parses to a move whose `buttons` code is this.
        DragButtons(u8),
        /// Parses (the refusal rows' positive control).
        Accepted,
        /// Refused.
        Refused,
    }
    let origin = crate::input::PixelOffset::CELL_ORIGIN;
    let mut rows: Vec<(String, Want)> = vec![
        // Bare press: empty mods, count 1, left side, simple (block=false).
        (
            "press left 5 9".into(),
            Want::Event(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                row: 5,
                col: 9,
                mods: 0,
                click_count: 1,
                side: SelectionSide::Left,
                block: false,
                suppress_copy_on_select: false,
                px_off: origin,
            }),
        ),
        // Full grammar, tokens in any position.
        (
            "count=2 press left side=right 5 9 mods=shift+alt block=1".into(),
            Want::Event(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                row: 5,
                col: 9,
                mods: SHIFT_MASK | ALT_MASK,
                click_count: 2,
                side: SelectionSide::Right,
                block: true,
                suppress_copy_on_select: false,
                px_off: origin,
            }),
        ),
        // count clamps to 1..=3.
        ("press left 0 0 count=9".into(), Want::ClickCount(3)),
        // move: bare = hover code 3; with a button = its X10 drag code.
        (
            "move 7 3".into(),
            Want::Event(InputEvent::MouseMove {
                buttons: 3,
                row: 7,
                col: 3,
                mods: 0,
                side: SelectionSide::Left,
                px_off: origin,
            }),
        ),
        (
            "move left 7 3".into(),
            Want::DragButtons(MouseButton::Left.code()),
        ),
        // wheel actions default to lines=1.
        (
            "wheelup left 2 4".into(),
            Want::Event(InputEvent::Wheel {
                dir: aterm_types::mouse::WheelDir::Up,
                lines: 1,
                detent: true,
                row: 2,
                col: 4,
                mods: 0,
                px_off: origin,
            }),
        ),
        // errors: missing row/col, bad button, bad action.
        ("press left".into(), Want::Refused),
        ("press banana 1 1".into(), Want::Refused),
        ("jump left 1 1".into(), Want::Refused),
    ];
    // An unknown (or unreportable) `mods=` name refuses, never an unmodified press...
    for bad in [
        "mods=CTRL",
        "mods=cmd",
        "mods=super",
        "mods=hyper",
        "mods=shfit",
    ] {
        rows.push((format!("press left 4 4 {bad}"), Want::Refused));
    }
    // ...CONTROL: the three the report CAN carry, and none.
    for good in [
        "press left 4 4 mods=shift+ctrl+alt",
        "press left 4 4 mods=",
        "press left 4 4",
    ] {
        rows.push((good.into(), Want::Accepted));
    }
    for (input, want) in rows {
        let got = parse_mouse(&input);
        match want {
            Want::Event(event) => assert_eq!(got, Ok(event), "mouse {input:?}"),
            Want::ClickCount(n) => {
                let Ok(InputEvent::MouseButton { click_count, .. }) = got else {
                    panic!("{input:?} parses to a press: {got:?}")
                };
                assert_eq!(click_count, n, "{input:?}");
            }
            Want::DragButtons(code) => {
                let Ok(InputEvent::MouseMove { buttons, .. }) = got else {
                    panic!("{input:?} parses to a move: {got:?}")
                };
                assert_eq!(buttons, code, "{input:?}");
            }
            Want::Accepted => assert!(got.is_ok(), "{input:?} must still parse: {got:?}"),
            Want::Refused => assert!(
                got.is_err(),
                "`mouse {input}` must refuse, not report an unmodified or partial event"
            ),
        }
    }
}

#[test]
fn sessionless_front_paste_is_owner_bare_only_and_terminal_routing_is_unchanged() {
    let route = sessionless_front_paste_event(
        "paste",
        || "Paragraph".to_string(),
        None,
        false,
        NativeControlPrincipal::Owner,
    )
    .expect("native-front paste is classified before session resolution")
    .expect("owner may drive its front app");
    assert_eq!(
        route,
        InputEvent::Paste("Paragraph".to_string(), PasteFraming::AtDrain)
    );

    assert!(
        sessionless_front_paste_event(
            "paste",
            || "terminal".to_string(),
            None,
            true,
            NativeControlPrincipal::Owner,
        )
        .is_none(),
        "a terminal front retains the established session/PTY paste path",
    );
    assert!(
        sessionless_front_paste_event(
            "paste",
            || "explicit".to_string(),
            Some(&Selector::Local(7)),
            false,
            NativeControlPrincipal::Owner,
        )
        .is_none(),
        "an explicit selector remains an exact terminal-session target",
    );
    assert!(
        sessionless_front_paste_event(
            "send",
            || "raw".to_string(),
            None,
            false,
            NativeControlPrincipal::Owner,
        )
        .is_none(),
        "raw send never mutates a native document",
    );
    assert_eq!(
        sessionless_front_paste_event(
            "paste",
            || "denied".to_string(),
            None,
            false,
            NativeControlPrincipal::Edge,
        ),
        Some(Err("ERR denied\n")),
    );
}

/// A bare `key` with a native view in front (Settings) reaches that view's own
/// keyboard handling, so its arrows and Tab can be driven headless; a terminal
/// front, an explicit selector and an Edge keep their answers.
#[test]
fn sessionless_front_key_is_owner_bare_only_and_terminal_routing_is_unchanged() {
    let route = sessionless_front_key_event("down", None, false, NativeControlPrincipal::Owner)
        .expect("a native-front key is classified before session resolution")
        .expect("owner may drive its front app");
    assert_eq!(Some(route), control_input::parse_key("down"));
    assert!(
        sessionless_front_key_event("down", None, true, NativeControlPrincipal::Owner).is_none(),
        "a terminal front retains the established session/PTY key path",
    );
    assert!(
        sessionless_front_key_event(
            "down",
            Some(&Selector::Local(7)),
            false,
            NativeControlPrincipal::Owner,
        )
        .is_none(),
        "an explicit selector remains an exact terminal-session target",
    );
    assert_eq!(
        sessionless_front_key_event("down", None, false, NativeControlPrincipal::Edge),
        Some(Err("ERR denied\n")),
    );
    assert_eq!(
        sessionless_front_key_event("no-such-key", None, false, NativeControlPrincipal::Owner),
        Some(Err(control_input::KEY_USAGE)),
    );
}

/// The px form of `resize` names the window, so a native front tab (which has no
/// `ActiveSession`) must still be resizable from the socket, reaching the compact
/// and phone Settings layouts.
#[test]
fn the_window_form_of_resize_survives_a_native_front_and_the_cell_form_does_not_move() {
    let route = sessionless_front_window_resize(
        "resize",
        "px 420 820",
        None,
        false,
        NativeControlPrincipal::Owner,
    )
    .expect("a native front classifies the window form before session resolution")
    .expect("owner may resize its own window");
    assert_eq!(
        route,
        InputEvent::ResizeWindowPx {
            width: 420,
            height: 820
        }
    );

    assert!(
        sessionless_front_window_resize(
            "resize",
            "px 420 820",
            None,
            true,
            NativeControlPrincipal::Owner,
        )
        .is_none(),
        "a terminal front retains the established session resize path",
    );
    assert!(
        sessionless_front_window_resize(
            "resize",
            "24 80",
            None,
            false,
            NativeControlPrincipal::Owner
        )
        .is_none(),
        "the CELL form resizes the engine and the PTY, so it stays session-class",
    );
    assert!(
        sessionless_front_window_resize(
            "resize",
            "px 420 820",
            Some(&Selector::Local(7)),
            false,
            NativeControlPrincipal::Owner,
        )
        .is_none(),
        "an explicit selector remains an exact terminal-session target",
    );
    assert_eq!(
        sessionless_front_window_resize(
            "resize",
            "px 420 820",
            None,
            false,
            NativeControlPrincipal::Edge,
        ),
        Some(Err("ERR denied\n".to_string())),
    );
    // A malformed window request is answered by the px parser, exactly as on a
    // terminal front, never handed to a session resolver.
    for rest in ["px 420", "px wide tall"] {
        let answer = sessionless_front_window_resize(
            "resize",
            rest,
            None,
            false,
            NativeControlPrincipal::Owner,
        );
        assert!(matches!(answer, Some(Err(_))), "{rest:?}: {answer:?}");
        assert_eq!(answer, control_input::parse_resize_px(rest), "{rest:?}");
    }
}

/// An aimed `spawn`/`tab` (`@<sid> spawn …`, `@<sid> tab …`) keeps the App verdict
/// every App-target selector gets; otherwise an edge with write-input on one
/// session could mint a tab in, or `tab close <N>` any tab of, the window hosting
/// it. The main-thread hop needs an event loop, so the lane verdict is tested:
/// `ResolveSession` is the `return None` that carries the Owner's selector into
/// `dispatch_request` -> `handle` (`cmd_spawn(.., is_cross.then_some(session))`,
/// `"tab" if is_cross => cmd_tab_aimed(..)`).
#[test]
fn an_aimed_spawn_or_tab_is_owner_only_and_the_owner_keeps_the_selector() {
    use super::{AimedAppLane, aimed_app_lane, request_head};
    // Every aimed spelling, both selector forms (local id, stable sid), with
    // a well-formed and a malformed tail: the verdict precedes the parse.
    for line in [
        "@0 spawn",
        "@0 spawn raise=maybe",
        "@s-abc spawn cwd=/w split=v",
        "@0 tab next",
        "@0 tab prev",
        "@0 tab 1",
        "@0 tab new",
        "@0 tab close 0",
        "@s-abc tab move 0 1",
    ] {
        let (selector, verb, _) = request_head(line);
        assert_eq!(
            aimed_app_lane(verb, selector.as_ref(), NativeControlPrincipal::Edge),
            Some(AimedAppLane::Denied),
            "an Edge is refused {line:?} before the sid is looked up",
        );
        assert_eq!(
            aimed_app_lane(verb, selector.as_ref(), NativeControlPrincipal::Owner),
            Some(AimedAppLane::ResolveSession),
            "an Owner's selector survives to the resolver for {line:?}",
        );
    }
    // The flagless and `@.` forms keep the ordinary App path (Owner:
    // `WithoutSession` -> the front-window verb; Edge: `ERR denied`).
    for line in [
        "spawn",
        "spawn window=1 raise=false",
        "@. spawn",
        "tab next",
        "@. tab close",
    ] {
        let (selector, verb, _) = request_head(line);
        for principal in [NativeControlPrincipal::Owner, NativeControlPrincipal::Edge] {
            assert_eq!(
                aimed_app_lane(verb, selector.as_ref(), principal),
                None,
                "{line:?} is not aimed ({principal:?})",
            );
        }
    }
    // Nor is any OTHER selector-bearing App verb: `@0 hover on` keeps the
    // lane it always had (this lane is exactly spawn/tab).
    let (selector, verb, _) = request_head("@0 hover on");
    assert_eq!(
        aimed_app_lane(verb, selector.as_ref(), NativeControlPrincipal::Edge),
        None
    );
}

/// A `pipe(2)` with both ends flagged close-on-exec as early as the platform
/// allows; the spelling every test in this file should use.
///
/// Darwin's `pipe(2)` returns both descriptors unflagged, and tests here spawn real
/// processes, so a racing `posix_spawn` can hand a child a copy of an end. A
/// stranger holding the read end means the peer is not dead: a write lands instead
/// of earning `EPIPE`. Linux closes the window with `pipe2(O_CLOEXEC)`; Darwin has
/// no `pipe2`, and a `mkfifo` carrier does not behave like a pipe here (with the
/// buffer full and the last reader closed, `poll(wr, POLLOUT)` reports nothing,
/// where a pipe reports `POLLHUP`), which would hang the sink drainer in
/// `aterm_pty::poll_writable`.
///
/// So Darwin keeps `pipe(2)` + `F_SETFD` with a narrowed window, and no assertion
/// rests on it: dead-peer tests use [`kill_peer_in_kernel`], and the mid-write paste
/// test probes the pipe after the reply and asserts only over a peer it saw dead
/// ([`paste_while_the_peer_closes_mid_write`]). A stranger can now cost a delay,
/// never a wrong verdict (the same trade as [`crate::seamless`]'s `cloexec_pipe`).
///
/// The flag is read back on both descriptors rather than trusted.
#[cfg(unix)]
fn cloexec_pipe() -> (i32, i32) {
    let mut fds = [-1i32; 2];
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        assert_eq!(
            // SAFETY: `pipe2` fills the two-element array this frame owns,
            // and flags BOTH descriptors as it creates them.
            unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) },
            0,
            "pipe2(O_CLOEXEC)"
        );
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        assert_eq!(
            // SAFETY: `pipe` fills the two-element array this frame owns.
            unsafe { libc::pipe(fds.as_mut_ptr()) },
            0,
            "pipe(2)"
        );
        for fd in fds {
            aterm_pty::set_cloexec(fd, true).expect("pipe end cloexec");
        }
    }
    for fd in fds {
        // SAFETY: `fd` is a descriptor this function just created, and
        // `F_GETFD` takes no further argument.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags, -1, "F_GETFD");
        assert!(
            flags & libc::FD_CLOEXEC != 0,
            "a descriptor came back WITHOUT FD_CLOEXEC — the race this helper \
             exists to close is open again"
        );
    }
    (fds[0], fds[1])
}

/// A live pipe-backed session: a real `SinkWriter` over the write end of a `pipe(2)`
/// (so input bytes are readable from `rx`), its own `Terminal`, and a
/// `SessionHandle` registered under `local_id`. The read end is returned so the
/// test can drain what reached this session's PTY. The term, sink and ctx are the
/// same `Arc`s the registry hands back, so cross-session resolution is real.
#[cfg(unix)]
fn pipe_session(local_id: u64) -> (crate::session_store::SessionHandle, std::fs::File) {
    let (rd, wr) = cloexec_pipe();
    let rx = unsafe { std::fs::File::from_raw_fd(rd) };
    // The write end doubles as the "master" for this headless test.
    (registered_session(local_id, wr, b""), rx)
}

/// Read whatever a pipe has buffered without blocking when empty (sets the read end
/// non-blocking, then reads once).
#[cfg(unix)]
fn drain_pipe(rx: &std::fs::File) -> Vec<u8> {
    use std::os::fd::AsRawFd;
    let fd = rx.as_raw_fd();
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    unsafe { libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) };
    let mut buf = [0u8; 4096];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n > 0 {
        buf[..n as usize].to_vec()
    } else {
        Vec::new()
    }
}

/// Kill a session's peer in the kernel, so no descriptor anywhere can revive it,
/// and prove it dead before the caller asserts. The spelling every dead-peer test
/// here should use.
///
/// Closing this process's copy of a pipe's read end is not enough: [`cloexec_pipe`]
/// cannot close Darwin's inherit window, and a stranger's duplicate keeps writes
/// succeeding. A socket's `shutdown` belongs to the socket every duplicate shares,
/// so this `dup2`s a socket shut for writing over the sink's fd, and the resulting
/// `EPIPE` cannot be undone, whoever still holds a duplicate of the read end.
///
/// The probe write is the receipt: this never returns while the fd is still
/// writable. (`dup2` also closes the description the fd named, taking the pipe's
/// write end with it.)
#[cfg(unix)]
fn kill_peer_in_kernel(master: i32, subject: &str) {
    let mut pair = [0i32; 2];
    assert_eq!(
        // SAFETY: `socketpair` fills the two-element array this frame owns.
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, pair.as_mut_ptr()) },
        0,
        "{subject}: socketpair(2)"
    );
    // Darwin has no `SOCK_CLOEXEC`: flag both ends at once, as `cloexec_pipe`
    // flags its pipe.
    for fd in pair {
        aterm_pty::set_cloexec(fd, true).expect("socket end cloexec");
    }
    assert_eq!(
        // SAFETY: `pair[0]` is a live socket this frame owns.
        unsafe { libc::shutdown(pair[0], libc::SHUT_WR) },
        0,
        "{subject}: shutdown(SHUT_WR)"
    );
    assert_eq!(
        // SAFETY: both are live descriptors; `dup2` closes what `master`
        // named and makes it a second name for the dead socket.
        unsafe { libc::dup2(pair[0], master) },
        master,
        "{subject}: dup2(2) the dead socket over the sink's fd"
    );
    // `dup2` clears close-on-exec on the copy: re-flag it, or the dead socket
    // rides every child a sibling test spawns for the rest of this test.
    aterm_pty::set_cloexec(master, true).expect("re-flag the swapped fd");
    // SAFETY: two live descriptors this frame owns, each closed once; the
    // dead socket survives as `master`.
    unsafe {
        libc::close(pair[0]);
        libc::close(pair[1]);
    }
    // SAFETY: a one-byte write from a static buffer to a live descriptor.
    let probe = unsafe { libc::write(master, b"x".as_ptr().cast(), 1) };
    assert_eq!(
        (probe, std::io::Error::last_os_error().raw_os_error()),
        (-1, Some(libc::EPIPE)),
        "{subject}: the peer must be observably dead before any verdict"
    );
}

/// One DEC 2026 frame of a synthetic alt-screen app: transcript rows `start..` above
/// a fixed composer and footer, painted with CUP + text + EL and no scroll sequence,
/// the Claude Code shape whose scrolled-off rows only the `offscreen` archive keeps.
#[cfg(unix)]
fn paint_alt_frame(t: &mut Terminal, start: usize) {
    use std::fmt::Write as _;
    let rows = usize::from(t.rows());
    let mut b = String::from("\x1b[?2026h");
    for r in 0..rows {
        let text = match rows - r {
            1 => "footer: ready".to_string(),
            2 => "> type a message".to_string(),
            _ => format!("transcript row {:05}", start + r),
        };
        let _ = write!(b, "\x1b[{};1H{text}\x1b[K", r + 1);
    }
    b.push_str("\x1b[?2026l");
    t.process(b.as_bytes());
}

/// `help --json` is the verb table as one JSON line: a row per `VerbSpec` in table
/// order with name, op, framing, target, access and summary, the fields the text
/// catalog renders.
#[test]
fn help_json_is_the_verb_table() {
    use aterm_types::control_verbs::VERBS;
    let reply = cmd_help("--json");
    let body = reply.strip_prefix("OK 1\n").expect("one content line");
    let json = body.strip_suffix('\n').expect("newline-terminated");
    assert!(!json.contains('\n'), "one line");
    let v = aterm_json::from_str::<aterm_json::Value>(json).expect("valid JSON");
    let rows = v
        .get("verbs")
        .and_then(|r| r.as_array())
        .expect("a verbs array");
    assert_eq!(rows.len(), VERBS.len(), "every verb");
    for (row, spec) in rows.iter().zip(VERBS) {
        let field = |k: &str| {
            row.get(k)
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(field("name"), spec.name);
        assert_eq!(field("op"), spec.op.wire_name(), "{}", spec.name);
        assert_eq!(field("framing"), spec.framing.wire_name(), "{}", spec.name);
        assert_eq!(field("target"), spec.target.wire_name(), "{}", spec.name);
        assert_eq!(field("access"), spec.access.wire_name(), "{}", spec.name);
        assert_eq!(field("summary"), spec.summary, "{}", spec.name);
    }
    assert!(cmd_help("--nope").starts_with("ERR usage: help"));
}

/// An empty `turn` is a usage error unless it also submits nothing (`submit=none`, a
/// settle read), so a bare `ctl turn` never presses Enter; nothing is pasted or
/// pressed on refusal.
#[test]
#[cfg(unix)]
fn a_turn_with_no_text_is_usage_unless_it_submits_nothing() {
    let store = crate::session_store::new_store();
    let subscribers = subscribe::new_registry();
    let (target, _rx) = pipe_session(74);
    store.write().unwrap().register(target.clone());
    let paste = |_: &str| panic!("an empty turn pastes nothing");
    let press = |_: &str| panic!("an empty turn presses nothing");
    let io = control_session::TurnIo {
        paste: &paste,
        press: &press,
        ..control_session::TurnIo::paste_only()
    };
    for rest in [
        "",
        "   ",
        "idle=1 timeout=500",
        "submit=enter --",
        "presses=1 -- ",
    ] {
        let reply = control_session::cmd_turn(
            &target.term,
            &store,
            target.local_id,
            rest,
            &subscribers,
            &target.ctx,
            &io,
        );
        assert!(reply.starts_with("ERR usage: turn"), "{rest:?}: {reply}");
    }
    let reply = control_session::cmd_turn(
        &target.term,
        &store,
        target.local_id,
        "idle=1 timeout=500 submit=none",
        &subscribers,
        &target.ctx,
        &io,
    );
    assert!(
        !reply.starts_with("ERR usage"),
        "a settle read runs: {reply}"
    );
}

/// `history` prints each turn's alt-screen archive mark as `arch=<origin>:<last>`,
/// taken when the turn started (so rows scrolled off while its text lands come after
/// it and `offscreen since=<mark>` reads exactly them), before `text=`, the free-text
/// tail readers cut a row at.
#[test]
#[cfg(unix)]
fn history_prints_the_turn_start_arch_mark_before_text() {
    const ORIGIN: u64 = 0xBEEF;
    let store = crate::session_store::new_store();
    let subscribers = subscribe::new_registry();
    let (target, _rx) = pipe_session(73);
    store.write().unwrap().register(target.clone());
    {
        let mut t = target.term.lock().unwrap();
        t.set_alt_archive_enabled(true);
        t.set_alt_archive_origin(ORIGIN);
        t.process(b"\x1b[?1049h");
        for start in 0..5 {
            paint_alt_frame(&mut t, start);
        }
        assert_eq!(
            t.alt_archive().last(),
            4,
            "four rows scrolled off before the turn"
        );
    }
    // The app scrolls three more rows off while the turn's text is delivered.
    let paste = |text: &str| {
        assert_eq!(text, "look here");
        let mut t = target.term.lock().unwrap();
        for start in 5..8 {
            paint_alt_frame(&mut t, start);
        }
        true
    };
    let press = |_: &str| panic!("submit=none must not press");
    let reply = control_session::cmd_turn(
        &target.term,
        &store,
        target.local_id,
        "idle=1 timeout=2000 submit=none -- look here",
        &subscribers,
        &target.ctx,
        &control_session::TurnIo {
            paste: &paste,
            press: &press,
            ..control_session::TurnIo::paste_only()
        },
    );
    assert!(
        reply.starts_with("OK ") && reply.contains(" submitted=0 status=settled "),
        "a type-only turn settles with an honest submitted=0: {reply}"
    );
    assert_eq!(target.term.lock().unwrap().alt_archive().last(), 7);

    let history = control_session::cmd_history(&target.ctx, "");
    let row = history.lines().nth(1).expect("one ledger row");
    let (meta, text) = row.split_once(" text=").expect("a text= tail");
    assert!(
        meta.ends_with(&format!(" arch={ORIGIN}:4")),
        "the mark is the START of the turn, just before text=: {row}"
    );
    assert_eq!(text, "look%20here", "text= stays the whole tail");
    assert_eq!(
        target
            .ctx
            .turns
            .lock()
            .unwrap()
            .since(None)
            .last()
            .map(|r| r.arch),
        Some(crate::turn_ledger::ArchMark {
            origin: ORIGIN,
            last: 4
        })
    );

    // The mark is the token `offscreen since=` takes: it reads exactly the rows
    // that scrolled off during the turn.
    let off = control_query::cmd_offscreen(&target.term, &format!("since={ORIGIN}:4"));
    let rows: Vec<&str> = off.lines().skip(1).collect();
    assert_eq!(
        rows,
        [
            "transcript row 00004",
            "transcript row 00005",
            "transcript row 00006"
        ],
        "{off}"
    );
}

/// A cross-session input verb resolves its `@<selector>` target like `send`/`feed`
/// and drives the source-blind seam against that session: bytes land in the
/// target's sink, never self's, and the self path is unchanged. Covers
/// `key`/`feed`/`send`. (`select` on a host: `aterm_control::conformance`.)
#[test]
#[cfg(unix)]
fn cross_session_input_reaches_target_sink_not_self() {
    use crate::session_store::new_store;
    let store = new_store();
    let (h_self, self_rx) = pipe_session(1);
    let (h_target, target_rx) = pipe_session(2);
    // Both terms stay in the default keyboard mode (`key enter` is a bare CR). Mouse
    // tracking is covered by `cross_session_mouse_reports_or_scrolls_target`.
    store.write().unwrap().register(h_self.clone());
    store.write().unwrap().register(h_target.clone());

    // Resolve `@2` (the target's local id) EXACTLY as `handle()` does.
    let (term, _, _, ctx) = resolve_explicit(&store, &Selector::parse("2"))
        .expect("@2 is not ambiguous")
        .expect("@2 resolves");
    let ctx: &SessionCtx = &ctx;

    // `key enter` cross-session: CR reaches the TARGET sink, nothing to self.
    assert_eq!(cross_input(&term, ctx, parse_key("enter"), "ERR\n"), "OK\n");
    assert_eq!(
        drain_pipe(&target_rx),
        b"\r",
        "key bytes hit the TARGET pty"
    );
    assert!(
        drain_pipe(&self_rx).is_empty(),
        "self pty must be untouched"
    );
    let turn = h_target
        .ctx
        .output_echo
        .sample(&h_target.ctx.sink, std::time::Instant::now());
    assert!(turn.last_boundary_at.is_some());
    assert!(turn.last_accepted_at.is_none());
    assert!(
        h_self
            .ctx
            .output_echo
            .sample(&h_self.ctx.sink, std::time::Instant::now())
            .last_boundary_at
            .is_none(),
        "cross input must stamp only its resolved session"
    );

    // A malformed name cross-session is the same usage line the flagless
    // arm answers, and nothing reaches either pty.
    assert_eq!(
        cross_input(&term, ctx, parse_key("notakey"), control_input::KEY_USAGE),
        control_input::KEY_USAGE
    );
    assert!(drain_pipe(&target_rx).is_empty());
    assert!(drain_pipe(&self_rx).is_empty());

    // Raw feed/send retain their direct background route but now publish an
    // accepted-input receipt on that same resolved SessionCtx.
    let unlogged_before_feed = crate::unseamed_control_inputs();
    assert_eq!(
        cross_raw_input(&term, ctx, feed_bytes("03").unwrap()),
        Ok(InputOutcome::Ok)
    );
    assert_eq!(
        drain_pipe(&target_rx),
        b"\x03",
        "feed bytes hit the TARGET pty"
    );
    assert!(drain_pipe(&self_rx).is_empty(), "feed must not touch self");
    assert!(
        crate::unseamed_control_inputs().saturating_sub(unlogged_before_feed) >= 1,
        "the one-byte raw feed must disclose one unlogged input attempt"
    );

    // `send` (the other always-cross writer) still writes to the resolved sink.
    // `send` is RAW (no implicit CR unless a literal trailing `\n` is given).
    let unlogged_before_send = crate::unseamed_control_inputs();
    assert_eq!(
        cross_raw_input(&term, ctx, send_bytes("ls")),
        Ok(InputOutcome::Ok)
    );
    assert_eq!(
        drain_pipe(&target_rx),
        b"ls",
        "send bytes hit the TARGET pty"
    );
    assert!(drain_pipe(&self_rx).is_empty(), "send must not touch self");
    assert!(
        crate::unseamed_control_inputs().saturating_sub(unlogged_before_send) >= 2,
        "the two-byte raw send must disclose two unlogged input attempts"
    );
    assert!(
        h_target
            .ctx
            .output_echo
            .sample(&h_target.ctx.sink, std::time::Instant::now())
            .last_accepted_at
            .is_some(),
        "the visible target can discount its controller-input echo"
    );
}

/// Cross-session `resize` applies `echo_to_window:false` to the TARGET term+pty
/// ONLY (never the active window). It exercises ALL THREE per-session artifacts
/// `apply_term_resize` touches (main.rs:2453-2463): the engine grid, the PTY
/// winsize (over a REAL pty master, asserted via `TIOCGWINSZ` — a pipe fd
/// would make the `TIOCSWINSZ` ioctl silently no-op), and the target's own
/// asciicast geometry record (so a cross resize is indistinguishable from a self
/// resize in the target's `cast` timeline). Out-of-range requests reuse the
/// shared `parse_resize` errors and mutate nothing.
#[test]
#[cfg(unix)]
fn cross_session_resize_targets_term_only() {
    // A REAL pty pair so `aterm_pty::resize`'s TIOCSWINSZ actually takes effect
    // (a plain pipe is not a tty and the ioctl would no-op).
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let h = registered_session(0, master, b"");
    let (term, ctx) = (h.term.clone(), h.ctx.clone());
    let before = ctx.cast.lock().unwrap().event_count();

    assert_eq!(cross_resize(&term, master, &ctx, 0, None, "10 40"), "OK\n");
    // (1) engine grid.
    {
        let t = term_lock(&term);
        assert_eq!((t.rows(), t.cols()), (10, 40), "TARGET grid resized");
    }
    // (2) PTY winsize — the half a pipe-fd test could not prove.
    {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::ioctl(master, libc::TIOCGWINSZ, &mut ws) },
            0,
            "TIOCGWINSZ"
        );
        assert_eq!(
            (ws.ws_row, ws.ws_col),
            (10, 40),
            "TARGET pty winsize updated"
        );
    }
    // (3) asciicast record — one new `[t,"r","40x10"]` on the target timeline,
    // matching the self path's `apply_term_resize`.
    assert_eq!(
        ctx.cast.lock().unwrap().event_count(),
        before + 1,
        "cross resize recorded into the TARGET's cast"
    );

    // Out-of-range reuses parse_resize's exact string; nothing is mutated (no
    // extra cast event, grid unchanged).
    assert_eq!(
        cross_resize(&term, master, &ctx, 0, None, "65535 65535"),
        "ERR out of range: rows 1-4096, cols 1-4096\n"
    );
    {
        let t = term_lock(&term);
        assert_eq!(
            (t.rows(), t.cols()),
            (10, 40),
            "grid unchanged after reject"
        );
    }
    assert_eq!(
        ctx.cast.lock().unwrap().event_count(),
        before + 1,
        "rejected resize records nothing"
    );

    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// A cross-session resize holds the target's rewrap gauge: the width change that
/// detaches the target's tiered history books the rewrap on the target's own gauge
/// (the one `rewrap_in_flight` and the row read), fills it pass by pass and ends it
/// `Done` at re-attach, so nothing is left running once the verb answers.
#[test]
#[cfg(unix)]
fn cross_session_resize_books_the_rewrap_on_the_targets_gauge() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut t = Terminal::with_scrollback(
        24,
        80,
        8,
        aterm_scrollback::Scrollback::new(64, 512, 8_000_000),
    );
    let fill = "x".repeat(72);
    let mut buf = Vec::new();
    for i in 0..800 {
        buf.extend_from_slice(format!("L{i}-{fill}\r\n").as_bytes());
    }
    t.process(&buf);
    let history = t.grid().scrollback_lines();
    assert!(
        history > 100,
        "precondition: deep tiered history ({history})"
    );
    let term = Arc::new(Mutex::new(t));
    let ctx = test_ctx();
    assert_eq!(ctx.rewrap_gauge.reading().total, 0, "nothing booked yet");

    assert_eq!(cross_resize(&term, master, &ctx, 0, None, "24 40"), "OK\n");
    let reading = ctx.rewrap_gauge.reading();
    assert!(reading.total > 0, "the rewrap was booked: {reading:?}");
    assert_eq!(reading.jobs, 1, "one job: {reading:?}");
    assert_eq!(reading.done, reading.total, "every detached line rewrapped");
    assert!(!reading.running, "the hold ended with the verb");
    assert_eq!(reading.end, Some(aterm_messages::WaitEnd::Done));
    assert!(
        !term_lock(&term).grid().reflow_offload_in_flight(),
        "the history re-attached"
    );

    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// Cross-session `mouse` against the target in both tracking states (the
/// `cross_mouse_apply` core, proxy-free):
///   * tracking on: the seam writes a mouse report to the target sink (nothing to
///     self); returns `Ok(false)` (no viewport move, no repaint).
///   * tracking off: a wheel falls back to `scroll_display` on the target term
///     (`Ok(true)`, repaint) with no pty bytes; a plain press is a no-op
///     (`Ok(false)`, sink empty, offset unchanged).
#[test]
#[cfg(unix)]
fn cross_session_mouse_reports_or_scrolls_target() {
    let (_h_self, self_rx) = pipe_session(1);
    let (h_target, target_rx) = pipe_session(2);
    let term = &h_target.term;
    let ctx: &SessionCtx = &h_target.ctx;

    // Build scrollback so a wheel can actually move the viewport.
    for i in 0..60 {
        term_lock(term).process(format!("line {i}\r\n").as_bytes());
    }
    drain_pipe(&target_rx); // engine query replies, if any, are not sink writes — clear anyway

    // ── Tracking ON (SGR mouse, DEC 1000 + 1006) ──
    term_lock(term).process(b"\x1b[?1000h\x1b[?1006h");
    assert!(
        term_lock(term).mouse_tracking_enabled(),
        "DEC 1000 enabled tracking"
    );
    assert_eq!(
        cross_mouse_apply(term, ctx, "press left 1 1"),
        Ok(false),
        "report, no viewport move"
    );
    let report = drain_pipe(&target_rx);
    assert!(
        !report.is_empty() && report.starts_with(b"\x1b["),
        "SGR press report to TARGET: {report:?}"
    );
    assert!(
        drain_pipe(&self_rx).is_empty(),
        "self sink untouched by a cross mouse"
    );

    // ── Tracking OFF (DEC 1000 / 1006 reset) ──
    term_lock(term).process(b"\x1b[?1006l\x1b[?1000l");
    assert!(!term_lock(term).mouse_tracking_enabled(), "tracking reset");
    term_lock(term).scroll_to_bottom();
    assert_eq!(term_lock(term).grid().display_offset(), 0, "at live tail");

    // Wheel-up: scroll_display fallback moves the TARGET viewport into history,
    // emits no pty bytes, and asks for a repaint.
    assert_eq!(
        cross_mouse_apply(term, ctx, "wheelup left 2 4 count=3"),
        Ok(true),
        "wheel => repaint"
    );
    assert!(
        term_lock(term).grid().display_offset() > 0,
        "wheel moved TARGET viewport"
    );
    assert!(
        drain_pipe(&target_rx).is_empty(),
        "wheel fallback emits no pty bytes"
    );

    // A plain press under tracking-off: deliberate no-op (no selection UI for a
    // background tab) — sink empty, offset unchanged, no repaint.
    let off_before = term_lock(term).grid().display_offset();
    assert_eq!(
        cross_mouse_apply(term, ctx, "press left 1 1"),
        Ok(false),
        "press no-op"
    );
    assert!(
        drain_pipe(&target_rx).is_empty(),
        "press fallback emits no pty bytes"
    );
    assert_eq!(
        term_lock(term).grid().display_offset(),
        off_before,
        "press did not move viewport"
    );
}

/// Cross-session `scroll` moves the target's viewport directly (no seam, no pty
/// bytes) and reports `OK <offset> <max>` like the self path; with history,
/// `scroll top` jumps to the oldest line.
#[test]
#[cfg(unix)]
fn cross_session_scroll_moves_target_viewport() {
    let (h_target, rx) = pipe_session(2);
    // Generate scrollback: print more lines than the 24-row screen.
    for i in 0..60 {
        term_lock(&h_target.term).process(format!("line {i}\r\n").as_bytes());
    }
    let reply = cross_scroll(&h_target.term, "top");
    assert!(reply.starts_with("OK "), "scroll reply shape: {reply}");
    assert!(
        term_lock(&h_target.term).grid().display_offset() > 0,
        "viewport moved into history"
    );
    assert!(drain_pipe(&rx).is_empty(), "scroll emits no pty bytes");
    // `scroll bottom` returns to the live tail (offset 0).
    let _ = cross_scroll(&h_target.term, "bottom");
    assert_eq!(
        term_lock(&h_target.term).grid().display_offset(),
        0,
        "back to live bottom"
    );
}

/// A fresh `SessionCtx` for the auth/gate tests (no real PTY; the sink wraps a
/// harmless `-1` fd these tests never write).
fn test_ctx() -> Arc<crate::SessionCtx> {
    registered_session(0, -1, b"").ctx
}

/// The decision the op-scope gate at the top of `handle()` makes, kept in lockstep
/// with its inline `match (scope, required_op(verb))` so it is testable without an
/// `EventLoopProxy`: the deny path returns before any proxy use.
fn gate_allows(scope: Scope, verb: &str, active: &SessionCtx) -> bool {
    // Owner passes everything; an Edge passes iff its token is authorized against
    // the now-active session for the verb's op (`cross_session_authorized`), not
    // by op alone, so it cannot drive a session it was never granted on after the
    // active handle moves.
    scope.is_owner_class() || cross_session_authorized(scope, verb, active)
}

/// An `Edge` scope with a throwaway, ungranted token. Authority is re-derived from
/// the token per request (`decide_edge`/`authorize`), so it is denied every op
/// against any real session.
fn edge() -> Scope {
    Scope::Edge(EdgeToken::generate())
}

/// An `Edge` scope whose token is granted on `ctx`, so the gate /
/// `cross_session_authorized` permit it against that session for `op` verbs, like a
/// connection authorized at connect time.
fn edge_granted(op: Op, ctx: &SessionCtx) -> Scope {
    let tok = {
        let mut tbl = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
        tbl.grant(
            SessionId::new("s-test-controller"),
            ctx.self_id.clone(),
            op,
            ctx.nonce,
        )
    };
    Scope::Edge(tok)
}

/// `invoke <clipboard/config action>` and `open prefs|settings` escalate their op on
/// the argument, so a plain WriteInput edge cannot reach the fenced fine ops
/// through them: WriteInput is denied, Owner and a granted
/// ClipboardWrite/ConfigWrite edge are allowed. For `update`, `status` is an
/// AnyScopeMeta read while `check` (stages) and `apply` (re-execs) are owner-only,
/// like `invoke ApplyUpdate`/`open update`, so a ReadScreen edge cannot re-exec.
#[test]
fn update_check_and_apply_are_owner_only_subcommands() {
    assert!(update_is_owner_only_subcmd("check"));
    assert!(update_is_owner_only_subcmd("apply"));
    assert!(update_is_owner_only_subcmd("  apply  "));
    // Reads stay any-scope so an in-session observer can SEE update state.
    assert!(!update_is_owner_only_subcmd(""));
    assert!(!update_is_owner_only_subcmd("status"));
    assert!(!update_is_owner_only_subcmd("bogus"));
}

/// `escalated_op`, the argument-dependent fences. `invoke`/`open`/`act` escalate to
/// the fine op their target needs; owner-only surfaces (the palette gateway, update
/// re-exec, standing connection authority, `family <sid>`, `pointer click`,
/// `brief all`) to `OwnerOnly`. `find` escalates for nothing: escalation replaces the base op, and
/// every `find` form answers with a match position, so it would give a
/// keystroke-only edge a screen read (the dispatch requires write on top instead).
/// The spawn fence tokenizes as `parse_spawn_args` does, so quoting or position
/// cannot walk a `connected=`/`identity=` past it.
#[test]
fn escalated_op_fences_argument_dependent_seams() {
    use Escalation::{Op as EOp, OwnerOnly};
    const SETTINGS_ACT: &str =
        "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true";
    let clipboard = Some(EOp(Op::ClipboardWrite));
    let config = Some(EOp(Op::ConfigWrite));
    let write = Some(EOp(Op::WriteInput));
    let owner = Some(OwnerOnly);
    let rows: &[(&str, &str, Option<Escalation>)] = &[
        ("invoke", "Copy", clipboard),
        ("invoke", "Paste", clipboard),
        ("invoke", "SelectAll", clipboard),
        ("invoke", "ToggleSettings", config),
        ("invoke", "Preferences", config),
        ("invoke", "OpenPalette", owner),
        ("invoke", "SoftwareUpdate", owner),
        ("invoke", "ApplyUpdate", owner),
        ("invoke", "NewControlledWindow", owner),
        ("invoke", "NewControlledTab", owner),
        ("invoke", "NewControllerWindow", owner),
        ("invoke", "NewControllerTab", owner),
        ("invoke", "ConnectToSession", owner),
        ("invoke", "ShowConnectionMap", owner),
        ("invoke", "ConfigureConnection", owner),
        ("invoke", "DisconnectSession", owner),
        ("invoke", "NewTab", None),
        ("invoke", "FontIncrease", None),
        ("invoke", "TotallyBogusAction", None),
        ("open", "about", None),
        ("open", "app editor file:///tmp/example.md", None),
        ("open", "prefs", config),
        ("open", "settings", config),
        ("open", "preferences", config),
        ("open", "app settings /security", config),
        ("act", SETTINGS_ACT, config),
        ("open", "menu", owner),
        ("open", "palette", owner),
        ("open", "update", owner),
        // `close` inherits the open's fence: an edge cannot dismiss a privileged
        // overlay it may not open.
        ("open", "prefs close", config),
        ("open", "settings close", config),
        ("open", "menu close", owner),
        ("open", "palette close", owner),
        ("open", "update close", owner),
        ("spawn", "connected=controller of=s-abc", owner),
        (
            "spawn",
            "connected=controlled place=tab of=s-abc cwd=/tmp",
            owner,
        ),
        ("spawn", "", None),
        ("spawn", "cwd=/tmp", None),
        ("spawn", "identity=worker", owner),
        ("spawn", "cwd=/tmp identity=-", owner),
        ("spawn", "\"identity=worker\" cwd=/tmp", owner),
        (
            "spawn",
            "connected=controlled place=tab of=s-abc identity=worker",
            owner,
        ),
        ("spawn", "\"connected=controller\" of=s-abc", owner),
        (
            "spawn",
            "of=s-abc \"connected=controlled\" place=tab",
            owner,
        ),
        ("spawn", "\"connected=controller", owner),
        ("open", "connections", owner),
        ("controls", "connections", owner),
        ("window", "connections", owner),
        ("open", "connections close", owner),
        ("controls", "front", None),
        ("window", "", None),
        ("window", "front shot.png", None),
        ("meta", "set title build agent", write),
        ("meta", "unset icon", write),
        ("meta", "set role operator", write),
        ("meta", "unset attention", write),
        ("meta", "", None),
        ("timeline", "10 since=3", None),
        // `log` is a plain read whatever it asks for (`tier=content` included:
        // the OSC title `text` and `timeline` already show at ReadScreen);
        // `brief` reads its target, and `brief all` the whole roster, Owner-only,
        // wherever `all` stands among its arguments (its parser takes it there).
        ("log", "tier=content --json", None),
        ("brief", "t3 budget=100", None),
        ("brief", "all", owner),
        ("brief", "budget=100 all", owner),
        ("brief", "budget=100 r1", None),
        ("brief", "", None),
        // Fail-closed: what the parser does not read as a target's brief is
        // Owner-only (it answers its usage line to the Owner alone).
        ("brief", "budget=x all", owner),
        ("family", "", None),
        ("family", "  ", None),
        ("family", "s-other", owner),
        ("pointer", "", None),
        ("pointer", "status", None),
        ("pointer", "leave", None),
        ("pointer", "move 1 2", None),
        ("pointer", "click", owner),
        ("pointer", "  click  ", owner),
        // `pointer mods` arms the click (and the person's next keys): Owner-only too.
        ("pointer", "mods link", owner),
        ("pointer", "mods cmd", owner),
        ("pointer", " mods none", owner),
        // The leading `unread=ok` the verb strips before parsing never hides the form.
        ("pointer", "unread=ok click", owner),
        ("pointer", "  unread=ok   click", owner),
        ("pointer", "unread=ok mods link", owner),
        ("pointer", "unread=ok move 1 2", None),
        ("invoke", "unread=ok OpenPalette", owner),
        ("invoke", "unread=ok Copy", clipboard),
        ("find", "", None),
        ("find", "status", None),
        ("find", "open", None),
        ("find", "next", None),
        ("find", "type secret", None),
        ("find", "accept", None),
        ("find", "cancel", None),
    ];
    for &(verb, rest, want) in rows {
        assert_eq!(escalated_op(verb, rest), want, "{verb} {rest:?}");
    }
}

/// `theme` reads with its base class; its preview lane needs `WriteInput` and
/// the forms that rewrite `aterm.toml` need `ConfigWrite`, whatever follows the
/// sub-form word. Negative control: the read forms add nothing.
#[test]
fn control_theme_sub_forms_escalate_to_the_class_they_need() {
    use Escalation::Op as EOp;
    for rest in [
        "",
        "status",
        "list",
        "palette",
        "palette session",
        "session",
        "session status",
    ] {
        assert_eq!(escalated_op("theme", rest), None, "{rest:?} is a read");
    }
    // A session's own theme is runtime state of that session (spec
    // `session-theme-install`): the preview lane's class, never a config write.
    for rest in [
        "preview Nord",
        "preview GitHub Light ttl=5",
        "revert",
        "session set Hearth",
        "session set \"Aterm Day\"",
        "session clear",
        "session bogus",
    ] {
        assert_eq!(
            escalated_op("theme", rest),
            Some(EOp(Op::WriteInput)),
            "{rest:?}"
        );
    }
    for rest in ["set Nord", "commit", "bogus", "SET Nord"] {
        assert_eq!(
            escalated_op("theme", rest),
            Some(EOp(Op::ConfigWrite)),
            "{rest:?} fails closed to the write class"
        );
    }
}

/// The full dispatch decision (`dispatch_authorized`): the effective op is the base
/// op escalated by the argument, Owner short-circuiting. A WriteInput edge cannot
/// tunnel to ClipboardWrite/ConfigWrite or an owner-only surface; an explicitly
/// granted fine-op edge reaches its op, never an owner-only one.
#[test]
fn dispatch_authorization_applies_the_escalated_op() {
    const SETTINGS_ACT: &str =
        "app/v1 view 42 settings/control/allow_osc52_query settings/set/allow_osc52_query true";
    let ctx = test_ctx();
    let owner = Scope::Owner;
    let write = edge_granted(Op::WriteInput, &ctx);
    let read = edge_granted(Op::ReadScreen, &ctx);
    let clip = edge_granted(Op::ClipboardWrite, &ctx);
    let cfg = edge_granted(Op::ConfigWrite, &ctx);
    // The base op of each argument-escalating verb, then one row per (scope,
    // escalation kind): which argument escalates to what is
    // `escalated_op_fences_argument_dependent_seams`.
    let rows: &[(&str, Scope, &str, &str, bool)] = &[
        ("write", write, "invoke", "NewTab", true),
        ("write", write, "open", "about", true),
        ("write", write, "spawn", "cwd=/tmp", true),
        ("write", write, "pointer", "move 1 2", true),
        ("read", read, "controls", "front", true),
        ("write", write, "invoke", "Copy", false),
        ("write", write, "act", SETTINGS_ACT, false),
        ("write", write, "pointer", "click", false),
        ("write", write, "pointer", "mods link", false),
        ("write", write, "pointer", "mods cmd", false),
        ("write", write, "pointer", " mods none", false),
        ("write", write, "pointer", "unread=ok click", false),
        ("write", write, "pointer", "  unread=ok   click", false),
        ("write", write, "pointer", "unread=ok mods link", false),
        ("read", read, "family", "", true),
        ("read", read, "family", "s-other", false),
        ("read", read, "brief", "budget=100", true),
        ("read", read, "brief", "budget=100 all", false),
        ("clipboard", clip, "invoke", "Copy", true),
        ("clipboard", clip, "invoke", "OpenPalette", false),
        ("config", cfg, "act", SETTINGS_ACT, true),
        ("config", cfg, "invoke", "ApplyUpdate", false),
        ("owner", owner, "invoke", "Copy", true),
        ("owner", owner, "invoke", "ApplyUpdate", true),
    ];
    for &(who, scope, verb, rest, allowed) in rows {
        assert_eq!(
            dispatch_authorized(scope, verb, rest, &ctx),
            allowed,
            "{who} edge: {verb} {rest:?}"
        );
    }
}

/// The front-drive fence: a scoped input verb escalates to Owner while any overlay
/// consumes input, and to ConfigWrite while native Settings does. Owner is exempt;
/// an ordinary native app/terminal and non-input verbs keep their base class. The
/// full path gets this enum via the `Wake::FrontControlSurface` main-thread hop.
#[test]
fn front_drive_fence_requires_owner_for_overlays_and_config_write_for_native_settings() {
    use crate::overlay::OverlayKind;
    let scoped = edge(); // an Edge scope; authority already passed the op gate above
    let owner = Scope::Owner;

    // Every overlay is a gateway with Owner-only bindings (About copies and opens
    // URLs; the connection map raises and disconnects), so input verbs escalate
    // identically.
    for kind in [
        OverlayKind::Palette,
        OverlayKind::ConnCard,
        OverlayKind::SessionPicker,
        OverlayKind::ConnectionMap,
    ] {
        for verb in [
            "key", "ctrl", "mouse", "paste", "focus", "send", "feed", "turn",
        ] {
            assert_eq!(
                front_drive_escalation(scoped, verb, FrontControlSurface::Overlay(kind)),
                Some(Escalation::OwnerOnly),
                "{verb} must be owner-only under {kind:?}"
            );
        }
    }

    // OWNER is ALWAYS exempt — the owner's established control path is untouched.
    for kind in [OverlayKind::Palette, OverlayKind::ConnectionMap] {
        assert_eq!(
            front_drive_escalation(owner, "key", FrontControlSurface::Overlay(kind)),
            None,
            "Owner may drive its own {kind:?} overlay"
        );
    }

    // Native Settings is not an Overlay at all, but it still consumes the
    // same key/mouse vocabulary and can emit ConfigPatch effects.
    let escalation = front_drive_escalation(scoped, "key", FrontControlSurface::NativeSettings);
    assert_eq!(escalation, Some(Escalation::Op(Op::ConfigWrite)));
    let ctx = test_ctx();
    assert!(
        !scope_holds_escalation(
            edge_granted(Op::WriteInput, &ctx),
            escalation.unwrap(),
            &ctx
        ),
        "a WriteInput edge cannot drive native Settings"
    );
    assert!(scope_holds_escalation(
        edge_granted(Op::ConfigWrite, &ctx),
        escalation.unwrap(),
        &ctx,
    ));

    // No privileged front consumer => preserve the verb's ordinary authority.
    assert_eq!(
        front_drive_escalation(scoped, "key", FrontControlSurface::None),
        None
    );
    assert_eq!(
        front_drive_escalation(scoped, "key", FrontControlSurface::OtherNative),
        None
    );
    // A NON-input verb is never dynamically escalated.
    assert_eq!(
        front_drive_escalation(scoped, "text", FrontControlSurface::NativeSettings),
        None,
        "reads are not front-driving input"
    );
}

#[test]
fn real_native_settings_front_is_explicitly_fenced_while_owner_open_still_works() {
    let mut app = crate::App::headless_for_test();
    assert_eq!(app.front_control_surface(), FrontControlSurface::None);
    assert!(
        app.open_settings_tab(crate::native_settings::SettingsRoute::Security),
        "the established owner open path remains usable"
    );
    assert_eq!(
        app.front_control_surface(),
        FrontControlSurface::NativeSettings,
        "the main-thread observation must not collapse native Settings into no overlay"
    );
    let ctx = test_ctx();
    let write_edge = edge_granted(Op::WriteInput, &ctx);
    let escalation = front_drive_escalation(write_edge, "key", app.front_control_surface())
        .expect("native Settings replaces the base input authority");
    assert!(!scope_holds_escalation(write_edge, escalation, &ctx));
}

/// SETTINGS ▸ HARNESS'S SESSION PRESSES ARE OWNER-ONLY, like `meta set autopilot`:
/// a press writes a session's Autopilot word (Off to Cruise puts the harness's hands
/// back on a session its owner took them off), which the Settings fine op must not
/// reach.
///
/// END TO END NO EDGE REACHES THE PAGE TODAY, whatever it holds: `act` (like
/// `settings`) is an App-target verb, which `dispatch_before_session` answers `ERR
/// denied` for every Edge (`native_control_decision`'s App arm) before `escalated_op`
/// is asked; and an Edge's flagless `key`/`mouse`/`paste` are pinned to the session
/// its token was checked against (`front_routed`), which `input_to_session` hands to
/// that session and never to `native_input_event`. That gate is asserted first. The
/// two fences below are DEFENCE IN DEPTH, read at their own predicates (the main-
/// thread hop and the dispatcher need an event loop): on the semantic `act` path and
/// on input keyed at the front Settings surface while it shows the page, a ConfigWrite
/// edge is denied a session's press and Owner passes. NEGATIVE CONTROLS: at the same
/// predicates, that edge's Refresh and other Settings controls, and its input on any
/// other route, are the ordinary ConfigWrite fence; and Owner's App verdict is the
/// sessionless lane.
#[test]
fn harness_session_presses_are_owner_only_on_both_paths() {
    use super::{
        NativeControlDecision, NativeControlTarget, front_routed, native_control_decision,
    };
    let ctx = test_ctx();
    let config_edge = edge_granted(Op::ConfigWrite, &ctx);
    let owner = Scope::Owner;

    // The gate in front of both fences: every Edge is refused an App-target verb,
    // whatever its token, and its flagless input is pinned to its session.
    for verb in ["act", "settings"] {
        assert_eq!(
            aterm_types::control_verbs::spec(verb).map(|spec| spec.target),
            Some(aterm_types::control_verbs::Target::App),
            "{verb} is an App-target verb"
        );
    }
    assert_eq!(
        native_control_decision(
            true,
            true,
            NativeControlPrincipal::Edge,
            NativeControlTarget::App
        ),
        NativeControlDecision::Denied,
        "an Edge's `act` is ERR denied before escalated_op"
    );
    assert_eq!(
        native_control_decision(
            true,
            true,
            NativeControlPrincipal::Owner,
            NativeControlTarget::App
        ),
        NativeControlDecision::WithoutSession,
        "Owner's `act` takes the sessionless lane"
    );
    assert!(
        front_routed(false, false, Some(7), 7) && front_routed(false, false, None, 7),
        "an Edge's flagless input is pinned to its session, never the page"
    );

    // The semantic path: `act` on a session's Autopilot button — and on its
    // Timer's (design §9.3.4), which arm and cancel a timed Cruise — and on
    // its Show (§10.3.7): Show writes nothing, but it moves the person's
    // keyboard to that session's pane, so it stays behind the prefix's one
    // Owner fence.
    for action in [
        "harness/session/7/autopilot/cruise",
        "harness/session/7/land-in/120",
        "harness/session/7/land-at",
        "harness/session/7/land-cancel",
        "harness/session/7/show",
    ] {
        let press = format!("app/v1 view 42 {action} {action}");
        assert_eq!(
            escalated_op("act", &press),
            Some(Escalation::OwnerOnly),
            "{action}"
        );
        assert!(
            !dispatch_authorized(config_edge, "act", &press, &ctx),
            "a ConfigWrite edge is DENIED a session's {action}"
        );
        assert!(
            dispatch_authorized(owner, "act", &press, &ctx),
            "Owner presses {action}"
        );
    }
    for rest in [
        "app/v1 view 42 harness/refresh harness/refresh",
        "app/v1 view 42 settings/control/harness.enabled settings/set/harness.enabled false",
    ] {
        assert_eq!(
            escalated_op("act", rest),
            Some(Escalation::Op(Op::ConfigWrite)),
            "{rest}"
        );
        assert!(
            dispatch_authorized(config_edge, "act", rest, &ctx),
            "{rest}"
        );
    }

    // The front path: keys and clicks at the Settings tab while it shows the page.
    let mut app = crate::App::headless_for_test();
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Harness));
    assert_eq!(
        app.front_control_surface(),
        FrontControlSurface::HarnessSettings
    );
    for verb in ["key", "mouse", "paste"] {
        let need = front_drive_escalation(config_edge, verb, app.front_control_surface())
            .expect("the page replaces the base input authority");
        assert_eq!(need, Escalation::OwnerOnly, "{verb}");
        assert!(
            !scope_holds_escalation(config_edge, need, &ctx),
            "a ConfigWrite edge is DENIED {verb} on the Harness page"
        );
        assert!(scope_holds_escalation(owner, need, &ctx));
        assert_eq!(
            front_drive_escalation(owner, verb, app.front_control_surface()),
            None,
            "Owner drives the page"
        );
    }
    assert_eq!(
        front_drive_denial(FrontControlSurface::HarnessSettings),
        "ERR native settings harness open (owner-only while open)\n"
    );
    // Another route of the same Settings tab is the ordinary Settings fence.
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Security));
    assert_eq!(
        app.front_control_surface(),
        FrontControlSurface::NativeSettings
    );
    let need = front_drive_escalation(config_edge, "key", app.front_control_surface())
        .expect("native Settings");
    assert!(scope_holds_escalation(config_edge, need, &ctx));
}

/// Copy-on-select exfil fence (the decision): a scoped-edge `mouse` gesture has its
/// copy-on-select clipboard effect suppressed, while Owner and a real human gesture
/// do not, mirroring `front_drive_escalation`. The seam honouring it is pinned by
/// `copy_on_select_fires_only_on_...`.
#[test]
fn scoped_edge_mouse_suppresses_copy_on_select_owner_does_not() {
    use super::control_input::apply_copy_on_select_policy;

    // A scoped-edge release carries the suppression flag; the SAME event from
    // Owner leaves it false.
    let release = || parse_mouse("release left 5 9").expect("release parses");
    let InputEvent::MouseButton {
        suppress_copy_on_select: scoped_flag,
        ..
    } = apply_copy_on_select_policy(edge(), release())
    else {
        panic!("mouse event")
    };
    assert!(scoped_flag, "scoped-edge release is stamped to suppress");

    let InputEvent::MouseButton {
        suppress_copy_on_select: owner_flag,
        ..
    } = apply_copy_on_select_policy(Scope::Owner, release())
    else {
        panic!("mouse event")
    };
    assert!(
        !owner_flag,
        "Owner release is not stamped — copy-on-select fires"
    );
}

/// The op-scope gate at the top of `handle()`: the Owner passes every verb (its
/// arm short-circuits before lookup); an edge passes only the verbs whose op its
/// token was granted on the active session (read != write != signal), never a
/// privilege verb or an unknown one, and a fine op (`settings` ConfigWrite, `copy`
/// ClipboardWrite) only when granted explicitly. An ungranted token passes nothing.
#[test]
fn op_scope_gate_owner_full_power_edge_op_split() {
    let ctx = test_ctx();
    let scopes = [
        ("owner", Scope::Owner),
        ("read", edge_granted(Op::ReadScreen, &ctx)),
        ("write", edge_granted(Op::WriteInput, &ctx)),
        ("signal", edge_granted(Op::Signal, &ctx)),
        ("config", edge_granted(Op::ConfigWrite, &ctx)),
        ("clipboard", edge_granted(Op::ClipboardWrite, &ctx)),
        ("ungranted", edge()),
    ];
    const T: bool = true;
    const F: bool = false;
    // Per verb, allowed for: owner, read, write, signal, config, clipboard, ungranted.
    let rows: &[(&str, [bool; 7])] = &[
        ("text", [T, T, F, F, F, F, F]),
        ("image", [T, T, F, F, F, F, F]),
        ("select", [T, T, F, F, F, F, F]),
        ("feed", [T, F, T, F, F, F, F]),
        ("send", [T, F, T, F, F, F, F]),
        // `mark` writes an attributed claim into the session log (phase 3A): a
        // write, so a read-only edge must not reach it; it never acts.
        ("mark", [T, F, T, F, F, F, F]),
        ("signal", [T, F, F, T, F, F, F]),
        ("settings", [T, F, F, F, T, F, F]),
        ("copy", [T, F, F, F, F, T, F]),
        ("grant", [T, F, F, F, F, F, F]),
        ("revoke", [T, F, F, F, F, F, F]),
        ("whoami", [T, F, F, F, F, F, F]),
        ("bogus", [T, F, F, F, F, F, F]),
    ];
    for &(verb, allowed) in rows {
        for ((who, scope), want) in scopes.iter().zip(allowed) {
            assert_eq!(gate_allows(*scope, verb, &ctx), want, "{who}: {verb}");
        }
    }
    // A read edge passes the gate for `image` (its `image read` form serves the
    // session's own pixels), but the framebuffer form is a whole window's frame,
    // refused to an edge by the dispatch arm.
    assert!(image_frame_refusal(&ctx).starts_with("ERR denied"));
}

/// grant/revoke also enforce Owner-only in the body (beyond the gate): an Edge scope
/// is rejected even if the body is reached directly. `whoami` has no body guard (the
/// gate keeps it Owner-only); it reports the op re-derived from the presented token
/// against the current session, so an ungranted token reads `edge unauthorized`.
#[test]
fn privilege_verbs_reject_edge_scope_in_body() {
    let ctx = test_ctx();
    let edge = edge();
    assert_eq!(
        cmd_grant(&ctx, edge, "s-deadbeef read-screen"),
        "ERR denied\n"
    );
    assert_eq!(cmd_revoke(&ctx, edge, &"0".repeat(64)), "ERR denied\n");
    // whoami re-derives the op from the token: an ungranted token has no authority
    // here and reports `edge unauthorized`. The gate still keeps grant/revoke
    // Owner-only.
    let who = cmd_whoami(&ctx, edge);
    assert!(
        who.trim_end().ends_with("edge unauthorized"),
        "ungranted edge: {who}"
    );
    // A GRANTED ReadScreen token reports its real effective op.
    let granted = edge_granted(Op::ReadScreen, &ctx);
    let who_granted = cmd_whoami(&ctx, granted);
    assert!(
        who_granted.trim_end().ends_with("edge read-screen"),
        "granted edge: {who_granted}"
    );
}

/// Mint → authorize → revoke through the verbs: an Owner mints an edge with
/// `grant`, it authenticates as `Edge(op)` via `edge_scope_from_first_line`, and
/// `revoke` invalidates it.
#[test]
fn grant_then_edge_handshake_then_revoke_roundtrip() {
    let ctx = test_ctx();
    let owner = Scope::Owner;

    // Owner mints a ReadScreen edge from some source session into THIS session.
    let reply = cmd_grant(&ctx, owner, "s-source01 read-screen");
    let hex = reply
        .strip_prefix("OK ")
        .and_then(|s| s.strip_suffix('\n'))
        .expect("OK <hex>");
    assert_eq!(hex.len(), 64, "edge token is 64 hex chars");

    // The bearer presents it as the handshake hex => resolves to Edge(ReadScreen).
    let line = format!("AUTH {hex}");
    let (op, _tok, inline) = edge_scope_from_first_line(&line, &ctx).expect("edge authenticates");
    assert_eq!(op, Op::ReadScreen);
    assert_eq!(inline, None);

    // A folded TOKEN form preserves the inline verb.
    let line2 = format!("TOKEN {hex} text");
    let (op2, _tok2, inline2) =
        edge_scope_from_first_line(&line2, &ctx).expect("edge authenticates");
    assert_eq!(op2, Op::ReadScreen);
    assert_eq!(inline2.as_deref(), Some("text"));

    // Owner revokes it; the same hex no longer authenticates (fail closed).
    assert_eq!(cmd_revoke(&ctx, owner, hex), "OK\n");
    assert!(
        edge_scope_from_first_line(&line, &ctx).is_none(),
        "revoked => fail closed"
    );
    // A second revoke reports no-such-edge.
    assert_eq!(cmd_revoke(&ctx, owner, hex), "ERR no such edge\n");

    // whoami as Owner reports this session's identity + the owner scope.
    let who = cmd_whoami(&ctx, owner);
    assert!(who.starts_with("OK s-"), "whoami: {who}");
    assert!(who.trim_end().ends_with("owner"), "whoami scope: {who}");
}

/// `revoke src=<sid>`: the source sweep dissolves a whole connection (every row the
/// source holds, across ops) and replies the removed count; an unknown source is
/// `ERR no such edge`, never `OK 0`. The sweep keeps the token form's in-body Owner
/// guard.
#[test]
fn revoke_src_form_sweeps_the_whole_connection_and_fails_closed_on_unknown() {
    let ctx = test_ctx();
    let owner = Scope::Owner;
    let src = SessionId::new("s-sweepsrc1");

    // Mint a full BOTH connection (three rows) through the ONE mint path
    // (`grant_connection`, §1.4#2) — the graph the sweep must dissolve whole.
    let minted = {
        let mut edges = ctx.edges.lock().unwrap();
        edges.grant_connection(
            &src,
            &ctx.self_id,
            aterm_session::ConnectionKind::Both,
            &ctx.nonce,
        )
    };
    assert_eq!(minted.len(), 3, "Both mints all three human-fidelity ops");
    for (_op, tok) in &minted {
        let line = format!("AUTH {}", tok.to_hex());
        assert!(
            edge_scope_from_first_line(&line, &ctx).is_some(),
            "live before the sweep"
        );
    }

    // The sweep removes all three rows and reports the count; every minted
    // token then fails closed at the handshake.
    assert_eq!(cmd_revoke(&ctx, owner, "src=s-sweepsrc1"), "OK 3\n");
    for (_op, tok) in &minted {
        let line = format!("AUTH {}", tok.to_hex());
        assert!(
            edge_scope_from_first_line(&line, &ctx).is_none(),
            "swept => fail closed"
        );
    }

    // Unknown source / empty source / non-Owner scope all refuse.
    assert_eq!(
        cmd_revoke(&ctx, owner, "src=s-nobody99"),
        "ERR no such edge\n"
    );
    assert!(cmd_revoke(&ctx, owner, "src=").starts_with("ERR usage"));
    assert_eq!(cmd_revoke(&ctx, edge(), "src=s-sweepsrc1"), "ERR denied\n");
}

/// The `session_edge` audit seam: each wire act (grant, token revoke, source sweep)
/// emits one structured event on the `session_edge` target with (action, origin,
/// src, dst, op) and never a token hex. Records are filtered by this test's unique
/// src sid, so parallel tests cannot interfere.
#[test]
fn session_edge_audit_events_are_structured_and_hex_free() {
    // The crate's one capture: the first installer wins process-wide (the logger
    // OnceLock), so every log-reading test shares it; it admits the seam's Info tier.
    crate::test_log::install();

    let ctx = test_ctx();
    let owner = Scope::Owner;
    let src = "s-auditsrc7";

    // grant -> revoke <hex> -> grant -> revoke src= : all four audited acts.
    let reply1 = cmd_grant(&ctx, owner, &format!("{src} read-screen"));
    let hex1 = reply1.strip_prefix("OK ").unwrap().trim_end();
    assert_eq!(cmd_revoke(&ctx, owner, hex1), "OK\n");
    let reply2 = cmd_grant(&ctx, owner, &format!("{src} write-input"));
    let hex2 = reply2.strip_prefix("OK ").unwrap().trim_end();
    assert_eq!(cmd_revoke(&ctx, owner, &format!("src={src}")), "OK 1\n");

    let records: Vec<String> = crate::test_log::containing(src)
        .into_iter()
        .filter(|record| record.target == "session_edge")
        .map(|record| record.message)
        .collect();
    let dst = ctx.self_id.as_str();
    assert_eq!(records.len(), 4, "one event per act: {records:?}");
    assert_eq!(
        records[0],
        format!("EDGE: action=grant origin=wire src={src} dst={dst} op=read-screen")
    );
    assert_eq!(
        records[1],
        format!("EDGE: action=revoke origin=wire src={src} dst={dst} op=read-screen")
    );
    assert_eq!(
        records[2],
        format!("EDGE: action=grant origin=wire src={src} dst={dst} op=write-input")
    );
    assert_eq!(
        records[3],
        format!("EDGE: action=revoke_src origin=wire src={src} dst={dst} op=*")
    );
    // The hex-free obligation: no captured line ANYWHERE carries a token.
    for hex in [hex1, hex2] {
        let leaked = crate::test_log::containing(hex);
        assert!(
            leaked.is_empty(),
            "a token hex leaked into a log line: {leaked:?}"
        );
    }
}

/// `resize`, the cell form `resize <r> <c>` and the window form `resize px <w> <h>`,
/// one labelled row per input line.
///
/// * `resize 65535 65535` asks for ~4.3 billion cells; the parse rejects anything
///   outside 1..=MAX_GRID_ROWS/COLS. The verb forwards `Wake::Resize` to the main
///   thread, which a unit test cannot drive, so these rows pin `parse_resize`, the
///   validator it gates on.
/// * The `px` form is never parsed as cell geometry: it exists to reach the
///   live-drag path (the cell form applies the grid first and echoes pixels after,
///   so the width throttle sees no reflow), and a pixel size like `100 40` is also a
///   valid cell geometry. The `px` token decides, not the magnitudes.
/// * The px form has three routers (the plain verb, the front-routed cross-session
///   twin, the native-front window lane); one parser, `parse_resize_px`, keeps them
///   equal.
#[test]
fn resize_cell_and_px_forms_parse() {
    enum Cell {
        Is(Result<(u16, u16), String>),
        Refused,
        Unchecked,
    }
    enum Px {
        Is(Option<Result<InputEvent, String>>),
        Unchecked,
    }
    fn err<T>(e: &str) -> Result<T, String> {
        Err(e.to_string())
    }
    let window = |width, height| Px::Is(Some(Ok(InputEvent::ResizeWindowPx { width, height })));
    let out_of_range = || Cell::Is(err("ERR out of range: rows 1-4096, cols 1-4096\n"));
    let rows = vec![
        ("65535 65535", out_of_range(), Px::Unchecked),
        ("4097 80", out_of_range(), Px::Unchecked),
        ("24 4097", out_of_range(), Px::Unchecked),
        ("0 80", out_of_range(), Px::Unchecked),
        ("24 0", out_of_range(), Px::Unchecked),
        ("30 100", Cell::Is(Ok((30, 100))), Px::Unchecked),
        (
            "",
            Cell::Is(err("ERR usage: resize <r> <c>\n")),
            Px::Unchecked,
        ),
        ("x y", Cell::Is(err("ERR bad args\n")), Px::Unchecked),
        // Whatever `px …` means, it is never "1400 rows by 900 cols".
        ("px 1400 900", Cell::Refused, window(1400, 900)),
        // A pixel pair that is ALSO in cell range is still not a cell geometry.
        ("px 100 40", Cell::Refused, Px::Unchecked),
        // Whitespace-tolerant, as every other verb's argument is.
        ("  px  1400   900  ", Cell::Unchecked, window(1400, 900)),
        // The cell form is not the window form.
        ("24 80", Cell::Unchecked, Px::Is(None)),
        (
            "px 1400",
            Cell::Unchecked,
            Px::Is(Some(err("ERR usage: resize px <w> <h>\n"))),
        ),
        (
            "px wide tall",
            Cell::Unchecked,
            Px::Is(Some(err("ERR bad args\n"))),
        ),
    ];
    for (input, cell, px) in rows {
        match cell {
            Cell::Is(want) => assert_eq!(parse_resize(input), want, "resize {input:?}"),
            Cell::Refused => assert!(
                parse_resize(input).is_err(),
                "resize {input:?} must not parse as a cell geometry"
            ),
            Cell::Unchecked => {}
        }
        if let Px::Is(want) = px {
            assert_eq!(
                control_input::parse_resize_px(input),
                want,
                "resize px {input:?}"
            );
        }
    }
}

/// `cast` serializes the session's asciicast recorder in the read-verb framing
/// `OK <nbytes>\n<body>`: `<nbytes>` is the exact body length and the body is valid
/// asciicast v2.
#[test]
fn cast_verb_frames_asciicast_body() {
    let ctx = test_ctx();
    // Empty recording: header-only body, framed with its true byte length.
    let reply = cmd_cast(&ctx);
    let (hdr_line, body) = reply.split_once('\n').expect("OK <n>\\n<body>");
    let n: usize = hdr_line
        .strip_prefix("OK ")
        .expect("OK prefix")
        .parse()
        .expect("nbytes");
    assert_eq!(n, body.len(), "framed length must equal the body length");
    let header = body.lines().next().expect("a header line");
    assert!(
        header.contains("\"version\": 2"),
        "not asciicast v2: {header}"
    );

    // Fold in an output burst, then the body grows by one well-formed event.
    {
        let mut rec = ctx.cast.lock().unwrap();
        let t = rec.now();
        rec.record_output(t, b"hi there\r\n");
    }
    let reply2 = cmd_cast(&ctx);
    let (hdr2, body2) = reply2.split_once('\n').unwrap();
    let n2: usize = hdr2.strip_prefix("OK ").unwrap().parse().unwrap();
    assert_eq!(n2, body2.len());
    assert!(
        body2.lines().count() >= 2,
        "expected header + >=1 event: {body2}"
    );
    let event = body2.lines().nth(1).unwrap();
    assert!(
        event.starts_with('[') && event.contains("\"o\""),
        "bad event: {event}"
    );
}

/// `mainscreen` recovers from an alternate screen a killed app left up (no `?1049l`
/// arrived, e.g. under ConPTY): `left=1` when the primary screen came back, `left=0`
/// when it was already up; a sid the host does not serve is refused.
#[test]
fn mainscreen_verb_forces_the_primary_screen_and_says_whether_it_did() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let reg = subscribe::new_registry();
    let host = GuiHost::new(0, &term, None, &reg);
    assert_eq!(cmd_mainscreen(&host, 0), "OK left=0\n");
    // A pager enters the alt screen and dies there: its rows are on the alt
    // grid and `lines` reads 0, exactly the stuck tab.
    term.lock().unwrap().process(
        b"history one\r\nhistory two\r\n$ less file\r\n\x1b[?1049h\x1b[H\x1b[2Jpager row\r\n:",
    );
    assert!(term.lock().unwrap().modes().alternate_screen);
    assert_eq!(cmd_mainscreen(&host, 0), "OK left=1\n");
    {
        let t = term.lock().unwrap();
        assert!(!t.modes().alternate_screen);
        assert_eq!(t.row_text(2).unwrap().trim_end(), "$ less file");
        // The pager's last screen is in the archive (`offscreen`), whole.
        let archived = t.alt_archive().texts();
        assert!(archived.iter().any(|r| r == "pager row"), "{archived:?}");
        assert!(archived.iter().any(|r| r == ":"), "{archived:?}");
    }
    assert_eq!(cmd_mainscreen(&host, 0), "OK left=0\n");
    assert_eq!(cmd_mainscreen(&host, 7), "ERR no such session\n");
}

/// `blocks` surfaces the OSC 133/633 shell-integration command blocks so an
/// AI can navigate by command: each block's exit code and command text, and
/// `blocks <n>` the last n. (`blocktext`: `aterm_control::conformance`.)
#[test]
fn blocks_verb_surfaces_command_blocks() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let reg = subscribe::new_registry();
    let host = GuiHost::new(0, &term, None, &reg);
    // No shell integration yet -> no blocks.
    assert_eq!(cmd_blocks(&host, 0, ""), "OK 0\n");
    // Two command blocks via OSC 133 (+ OSC 633;E commandline): exit 0, exit 1.
    // Each OSC mark is BEL-terminated so the surrounding text isn't swallowed.
    term.lock().unwrap().process(
        b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07\
\x1b]133;A\x07$ \x1b]633;E;false\x07\x1b]133;B\x07false\n\x1b]133;C\x07\x1b]133;D;1\x07",
    );
    let out = cmd_blocks(&host, 0, "");
    assert!(out.starts_with("OK 2\n"), "expected 2 blocks: {out}");
    assert!(
        out.contains("exit=0") && out.contains("cmdline=echo%20hi"),
        "block 1 wrong: {out}"
    );
    assert!(
        out.contains("exit=1") && out.contains("cmdline=false"),
        "block 2 wrong: {out}"
    );
    // `blocks 1` returns only the most recent (the failed one).
    let last = cmd_blocks(&host, 0, "1");
    assert!(
        last.starts_with("OK 1\n") && last.contains("exit=1"),
        "last block wrong: {last}"
    );
}

/// The send→wait entry race: a fast command that completed before `wait` was called
/// is returned at once (no block in flight, a completion exists), while an
/// in-flight block suppresses that fast path so a stale completion is not reported
/// for a newer command.
#[test]
fn wait_verb_returns_a_prerace_completion_immediately() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let reg = subscribe::new_registry();
    let host = GuiHost::new(0, &term, None, &reg);
    // The command completed (exit 1) and the shell is at a fresh idle prompt,
    // including the prompt-end 133;B a real shell emits before typing, which parks
    // the new block in `EnteringCommand`. `wait` sees this when a fast command beat
    // it to the socket, and the idle prompt must not read as in-flight.
    term.lock().unwrap().process(
        b"\x1b]133;A\x07$ \x1b]133;B\x07false\n\x1b]133;C\x07\x1b]133;D;1\x07\x1b]133;A\x07$ \x1b]133;B\x07",
    );
    // The verb's ten-minute ceiling: a `wait` that missed the fast path
    // runs it out, so the minute's hang detector below tells the two apart.
    let start = std::time::Instant::now();
    let resp = cmd_wait(&host, 0, "600000");
    assert!(
        resp.starts_with("OK complete ") && resp.contains("exit=1"),
        "an already-completed command must be reported: {resp}"
    );
    assert!(
        start.elapsed() < aterm_spec::wait::MUST_HAPPEN,
        "the entry fast path must answer immediately, not at the timeout"
    );

    // A NEW command is now executing: the stale completion above must NOT be
    // returned — with nothing completing in time, `wait` times out instead.
    term.lock()
        .unwrap()
        .process(b"\x1b]133;B\x07sleep 99\n\x1b]133;C\x07");
    assert_eq!(
        cmd_wait(&host, 0, "50"),
        "OK timeout\n",
        "an in-flight block must suppress the last-completed fast path"
    );
}

/// `tab` parses each form to its `TabAction`; the App mutation happens on the main
/// thread via `Wake::TabCmd`, so this checks the forwarded action. An unknown or
/// missing arg is `None` (the verb replies with usage).
#[test]
fn tab_parses_each_form() {
    assert_eq!(parse_tab("new"), Some(TabAction::New));
    assert_eq!(parse_tab("next"), Some(TabAction::Next));
    assert_eq!(parse_tab("prev"), Some(TabAction::Prev));
    assert_eq!(parse_tab("0"), Some(TabAction::Select(0)));
    assert_eq!(parse_tab("3"), Some(TabAction::Select(3)));
    // Surrounding whitespace is tolerated (the rest-of-line may carry it).
    assert_eq!(parse_tab("  2 "), Some(TabAction::Select(2)));
    // `close` (active tab) and `close <N>` (a specific tab).
    assert_eq!(parse_tab("close"), Some(TabAction::Close(None)));
    assert_eq!(parse_tab("close 1"), Some(TabAction::Close(Some(1))));
    assert_eq!(parse_tab("  close 0 "), Some(TabAction::Close(Some(0))));
    // `move <from> <to>` reorders.
    assert_eq!(
        parse_tab("move 2 0"),
        Some(TabAction::Move { from: 2, to: 0 })
    );
    assert_eq!(
        parse_tab("move 0 3"),
        Some(TabAction::Move { from: 0, to: 3 })
    );
    // Unknown / empty / negative => None (usage error).
    assert_eq!(parse_tab(""), None);
    assert_eq!(parse_tab("bogus"), None);
    assert_eq!(parse_tab("-1"), None);
    // Malformed close/move => None.
    assert_eq!(parse_tab("close x"), None);
    assert_eq!(parse_tab("close 1 2"), None);
    assert_eq!(parse_tab("move 1"), None);
    assert_eq!(parse_tab("move 1 x"), None);
    assert_eq!(parse_tab("move 1 2 3"), None);
    // A trailing word after a keyword is rejected (not silently swallowed).
    assert_eq!(parse_tab("new x"), None);
    assert_eq!(parse_tab("next y"), None);
}

/// The grapheme content of one cell via the selection path (`select` that cell +
/// `selection_to_string`), the fidelity reference the pixels also render.
fn selection_of_cell(term: &Arc<Mutex<Terminal>>, row: i32, col: u16) -> String {
    let mut t = term_lock(term);
    let sel = t.text_selection_mut();
    sel.start_selection(row, col, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(row, col, SelectionSide::Right);
    sel.complete_selection();
    t.selection_to_string().unwrap_or_default()
}

/// `text`/`cell`/`search` return the same grapheme content (base char, combining
/// marks, complex cluster) as the selection path, which the renderer also reads via
/// combining_row/cluster_row, so they agree with the pixels (an NFD accent and a ZWJ
/// family are not dropped).
#[test]
fn read_paths_preserve_combining_and_zwj_clusters() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    // Row 0: an NFD "é" = 'e' + U+0301 in one cell. Row 1: a ZWJ family
    // "👨‍👩‍👧" in one wide cell. Trailing codepoints are stored as combining marks,
    // the path the renderer reads.
    term.lock()
        .unwrap()
        .process("e\u{0301}\r\n\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}".as_bytes());

    // Ground truth from the selection path.
    let accent_sel = selection_of_cell(&term, 0, 0);
    let family_sel = selection_of_cell(&term, 1, 0);
    assert_eq!(accent_sel, "e\u{0301}", "selection ground truth (accent)");
    assert_eq!(
        family_sel, "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "selection ground truth (ZWJ family)"
    );

    // ---- cell verb: grapheme token (pct-encoded) decodes to the selection.
    let accent_cell = cmd_cell(&term, "0 0");
    let accent_tok = accent_cell
        .strip_prefix("OK ")
        .and_then(|s| s.split(' ').next())
        .expect("cell OK token");
    assert_eq!(
        pct_decode(accent_tok),
        accent_sel,
        "cell grapheme must equal selection (accent): {accent_cell}"
    );
    let family_cell = cmd_cell(&term, "1 0");
    let family_tok = family_cell
        .strip_prefix("OK ")
        .and_then(|s| s.split(' ').next())
        .expect("cell OK token");
    assert_eq!(
        pct_decode(family_tok),
        family_sel,
        "cell grapheme must equal selection (ZWJ family): {family_cell}"
    );
    // The old `ch as u32` field would have been a bare codepoint, NOT the
    // multi-codepoint cluster — assert the cluster is really present.
    assert!(
        pct_decode(family_tok).chars().count() >= 5,
        "ZWJ family must keep all 5 codepoints, got {family_tok}"
    );

    // ---- text verb: the row line equals the full-row selection.
    let text = cmd_text(&term);
    let lines: Vec<&str> = text.lines().collect();
    // lines[0] is the "OK <n>" header; row r is lines[r+1].
    assert_eq!(
        lines[1], accent_sel,
        "text row 0 must equal selection: {text}"
    );
    assert_eq!(
        lines[2], family_sel,
        "text row 1 must equal selection: {text}"
    );

    // ---- search verb: searching the cluster finds it, and the located cell
    // reads back (via cell) the same grapheme the selection shows.
    let s = cmd_search(
        &term,
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        &RewrapGauge::default(),
    );
    assert!(
        s.starts_with("OK 1"),
        "search must find the ZWJ family once: {s}"
    );
    let hit = s.lines().nth(1).expect("a match row");
    let mut parts = hit.split(' ');
    let abs_row: u64 = parts.next().unwrap().parse().expect("abs row");
    // The absolute row resolves (via abs_row_text, the `line`/`text` space)
    // to a row whose text contains the faithful cluster.
    let row_text = match abs_row_text(&term_lock(&term), abs_row) {
        AbsRow::Text(t) => t,
        AbsRow::Evicted => panic!("search row {abs_row} unexpectedly evicted"),
        AbsRow::OutOfRange => panic!("search row {abs_row} out of range"),
        AbsRow::Unreadable => panic!("search row {abs_row} unreadable"),
    };
    assert!(
        row_text.contains(&family_sel),
        "search's absolute row must resolve to the cluster: {row_text:?}"
    );
}

/// `search` reaches scrollback, and a match's absolute row resolves through `line`.
/// The pattern is the rest of the line with the `case`/`regex` flags parsed off its
/// tail: a spaced literal is one pattern, a lone flag word is the pattern, and the
/// match is case-insensitive unless `case`.
#[test]
fn search_reaches_scrollback_and_parses_its_flags_off_the_tail() {
    // A match scrolled deep into history assumes the default (large) cap:
    // serialize against the cap-mutation test that lowers the process-global cap.
    let _serial = super::search_cap_test_guard();
    let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
    term.lock()
        .unwrap()
        .process(b"zsh: command not found: frobnicate\r\nCOMMAND NOT FOUND\r\n");
    for i in 0..20 {
        term.lock()
            .unwrap()
            .process(format!("filler line {i}\r\n").as_bytes());
    }
    let search = |q: &str| cmd_search(&term, q, &RewrapGauge::default());
    let hit = search("frobnicate");
    assert!(
        hit.starts_with("OK 1"),
        "a scrolled-off match is found: {hit}"
    );
    let abs_row: u64 = hit
        .lines()
        .nth(1)
        .and_then(|l| l.split(' ').next())
        .and_then(|r| r.parse().ok())
        .expect("a match row");
    let line_out = cmd_line(&term, &abs_row.to_string());
    assert!(
        line_out.contains("frobnicate"),
        "line {abs_row}: {line_out}"
    );
    for (query, count) in [
        ("command not found", "2"),
        ("COMMAND NOT FOUND case", "1"),
        ("case", "0"),
        ("fill[a-z]+ regex", "20"),
    ] {
        let reply = search(query);
        assert_eq!(
            reply.lines().next().and_then(|h| h.split(' ').nth(1)),
            Some(count),
            "search {query:?}: {reply}"
        );
    }
}

// ---- P1.2: cross-session @selector addressing --------------------------------

use crate::session_store::{self, SessionHandle, SessionState};
use aterm_session::{EdgeTable, LaunchNonce, decide_edge};

/// Build a registered session: a fresh `Terminal` (optionally pre-fed `seed` so its
/// `text` is distinctive), a fresh fabric identity and a sink over `master` (a pipe
/// write end in write tests, else `-1`). Returns the handle so a test can register
/// it and assert on the same engine.
fn registered_session(local_id: u64, master: i32, seed: &[u8]) -> SessionHandle {
    let sid = SessionId::generate();
    let nonce = LaunchNonce::generate();
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    if !seed.is_empty() {
        term.lock().unwrap().process(seed);
    }
    let ctx = Arc::new(crate::SessionCtx {
        sink: Arc::new(SinkWriter::new(master)),
        output_echo: Arc::new(crate::app_input::OutputEchoTracker::default()),
        modes: crate::mode_mirror_of(&term),
        ui_waiting: Arc::default(),
        edges: std::sync::Arc::new(std::sync::Mutex::new(EdgeTable::new())),
        turn_lease: std::sync::Mutex::new(None),
        self_id: sid.clone(),
        nonce,
        cast: Arc::new(std::sync::Mutex::new(crate::cast::CastRecorder::new(
            80, 24,
        ))),
        temporal: Arc::new(std::sync::Mutex::new(
            crate::temporal::TemporalRecorder::new(),
        )),
        byte_fanout: Arc::new(crate::cast::ByteFanout::new()),
        turns: Arc::new(std::sync::Mutex::new(
            crate::turn_ledger::TurnLedger::default(),
        )),
        meta: std::sync::Mutex::new(crate::session_timeline::SessionMeta::default()),
        timeline: Arc::new(std::sync::Mutex::new(
            crate::session_timeline::SessionTimeline::default(),
        )),
        fabric: std::sync::Arc::default(),
        idem: Default::default(),
        rewrap_gauge: std::sync::Arc::default(),
        human_input: Default::default(),
        launch: Default::default(),
        generation_look: Default::default(),
        reset_lane: Default::default(),
        crash_journal: Default::default(),
        update_parked: Default::default(),
        witness: Default::default(),
        fenced_writes: Default::default(),
    });
    SessionHandle {
        sid,
        nonce,
        local_id,
        parent: None,
        state: SessionState::Alive,
        title: format!("tab-{local_id}"),
        term,
        master,
        ctx,
        identity: None,
    }
}

/// `who` is the presence readout: no driver or watcher reads
/// `driving=- watchers=0`; a held turn lease reads `driving=<id>`, and a live
/// subscriber bumps `watchers`.
#[test]
fn who_verb_reports_driver_and_watchers() {
    let store = session_store::new_store();
    let root = registered_session(0, -1, b"");
    store.write().unwrap().register(root.clone());
    let subs = subscribe::new_registry();

    // Idle: no driver, no watcher.
    let out = cmd_who(&store, &subs);
    let line = out.lines().nth(1).expect("one session line");
    assert!(
        line.starts_with("0 ") && line.contains("driving=- watchers=0 turns=0"),
        "idle presence: {line}"
    );
    assert!(
        line.contains(&format!("nonce={} ", root.nonce.to_hex())),
        "the owner presence row carries the same launch nonce as sessions: {line}"
    );
    assert!(
        line.ends_with("fgpgid=-"),
        "an unreadable PTY group must be explicit, not guessed: {line}"
    );

    // A held turn lease shows as the driver; a live subscription as a watcher.
    *root.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
        id: 9,
        driver: None,
        typing: true,
    });
    let _watch = subscribe::SubscriberSet::register(&subs, &[0]);
    let out = cmd_who(&store, &subs);
    let line = out.lines().nth(1).expect("one session line");
    assert!(
        line.contains("driving=9 watchers=1"),
        "driver + watcher surfaced: {line}"
    );

    // A COOPERATIVE drive lease surfaces as `driving=lease:<holder>` (R17), so a
    // raw driver's hold is visible in the same presence readout as a turn.
    *root.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "agent-a".to_string(),
        expires_us: crate::metrics::now_us() + 60_000_000,
        conn: None,
        hard: false,
        holder_pid: None,
    });
    let line = cmd_who(&store, &subs)
        .lines()
        .nth(1)
        .expect("one session line")
        .to_string();
    assert!(
        line.contains("driving=lease:agent-a"),
        "cooperative lease surfaced in who: {line}"
    );
}

/// `sessions` lists the registry one row per session, sorted by local id, with the
/// parent column: `-` for a root, the parent's sid for a child.
#[test]
fn sessions_verb_lists_the_registry() {
    let store = session_store::new_store();
    let root = registered_session(0, -1, b"");
    let mut child = registered_session(1, -1, b"");
    child.parent = Some(root.sid.clone());
    // Registered child first: the listing sorts by local id, not arrival.
    store.write().unwrap().register(child.clone());
    store.write().unwrap().register(root.clone());
    let two = cmd_sessions(&root.ctx, &store, None);
    let rows: Vec<&str> = two.lines().collect();
    assert_eq!(rows[0], "OK 2", "{two}");
    assert!(
        rows[1].starts_with(&format!("0 {} - alive ", root.sid.as_str())),
        "the root, no parent: {two}"
    );
    assert!(
        rows[2].starts_with(&format!("1 {} {} ", child.sid.as_str(), root.sid.as_str())),
        "the child names its parent: {two}"
    );
}

/// `meta` round-trip: `meta set` stores a pct-round-trippable Unicode value, bare
/// `meta` reports it (with engine title/state), a same-value re-set is not a change
/// (no repaint/notify fan-out), and `meta unset` clears to `-`. (The `sessions`
/// `meta=` bit: `meta_questions_is_a_closed_set_stored_case_folded`;
/// `attention`: `keyed_attention_on_the_wire_is_per_owner_and_the_bare_form_still_works`.)
#[test]
fn meta_set_get_unset_roundtrip_with_unicode() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"\x1b]2;vim main.rs\x07");
    store.write().unwrap().register(h.clone());

    // Baseline read: engine title present, every user field unset.
    let (read0, changed0) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(!changed0, "a read is never a change");
    assert!(
        read0.starts_with(&format!("OK title={} ", pct_encode("vim main.rs"))),
        "engine title reported pct-encoded: {read0}"
    );
    assert!(read0.contains(" user_title=- "), "unset reads -: {read0}");
    assert!(read0.contains(" state=alive "), "{read0}");

    // Set a unicode title (spaces + CJK + emoji) — stored verbatim, reported
    // pct-encoded so the reply stays one parseable line.
    let title = "构建 agent ✨";
    let (r, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, &format!("set title {title}"));
    assert_eq!(r, "OK\n");
    assert!(changed, "first set IS a change");
    let (read1, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(
        read1.contains(&format!(" user_title={} ", pct_encode(title))),
        "unicode round-trips through pct: {read1}"
    );
    // The stored value is the exact unicode string (not the encoding).
    assert_eq!(
        h.ctx.meta.lock().unwrap().user_title.as_deref(),
        Some(title)
    );
    // Re-setting the SAME value is a no-change (no repaint/notify fan-out).
    let (_, changed_again) = cmd_meta(&h.term, &store, 0, &h.ctx, &format!("set title {title}"));
    assert!(!changed_again, "same-value re-set is not a change");

    // A STORED "-" is not the unset sentinel: it reads back escaped.
    let (_, dash) = cmd_meta(&h.term, &store, 0, &h.ctx, "set role -");
    assert!(dash);
    let (read_dash, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read_dash.contains(" role=%2D "), "{read_dash}");

    // Unset clears back to '-' (labels fall back down the chain); a second
    // unset of the same field is a no-change.
    let (r, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset title");
    assert_eq!(r, "OK\n");
    assert!(changed);
    let (read3, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read3.contains(" user_title=- "), "cleared: {read3}");
    let (_, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset title");
    assert!(!changed, "second unset is a no-change");

    // Unknown fields and malformed forms are refused and change nothing.
    for bad in ["set colour red", "unset colour", "set title", "bogus"] {
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, bad);
        assert!(r.starts_with("ERR ") && !c, "{bad:?}: {r}");
    }
}

/// The question-policy field on the wire: four words (`ask`, and the supervisor's
/// priority words `recommended`, `long-term`, `short-term`) with one usage line for
/// anything else, stored case-folded, read back by key, `ls meta=1` while it alone
/// is set, and `unset` clearing it to `-`
/// (back to the global `[harness]` policy). A session principal or a misspelled
/// priority word (`long_term`) is refused.
#[test]
fn meta_questions_is_a_closed_set_stored_case_folded() {
    const USAGE: &str = "ERR usage: meta set questions ask|recommended|long-term|short-term\n";
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let (read0, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read0.contains(" questions=- "), "unset reads -: {read0}");

    for bad in [
        "set questions",
        "set questions   ",
        "set questions whatever you think",
        "set questions recommend",
        "set questions @s-4f2a91c0",
        "set questions s-abc@n-lab",
        "set questions controller",
        "set questions long_term",
        "set questions short term",
    ] {
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, bad);
        assert_eq!(r, USAGE, "{bad:?}");
        assert!(!c, "a refusal moves nothing: {bad:?}");
    }
    assert_eq!(h.ctx.meta.lock().unwrap().questions, None);
    assert!(
        cmd_sessions(&h.ctx, &store, None)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" meta=0 "),
        "nothing stored"
    );

    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "set questions Recommended");
    assert_eq!(r, "OK\n");
    assert!(c);
    let (read1, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read1.contains(" questions=recommended "), "{read1}");
    assert!(
        cmd_sessions(&h.ctx, &store, None)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" meta=1 "),
        "a question policy alone is user metadata -> meta=1"
    );
    let (_, again) = cmd_meta(&h.term, &store, 0, &h.ctx, "set questions recommended");
    assert!(!again, "the folded spelling is the same value");
    let (_, moved) = cmd_meta(&h.term, &store, 0, &h.ctx, "set questions ASK");
    assert!(moved);
    let (read2, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read2.contains(" questions=ask "), "{read2}");
    // The supervisor's priority words, case folded like the rest.
    for (typed, stored) in [("Long-Term", "long-term"), ("short-term", "short-term")] {
        let set = format!("set questions {typed}");
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, &set);
        assert_eq!((r.as_str(), c), ("OK\n", true), "{typed}");
        let (read, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(read.contains(&format!(" questions={stored} ")), "{read}");
    }
    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset questions");
    assert_eq!(r, "OK\n");
    assert!(c);
    let (read3, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read3.contains(" questions=- "), "{read3}");
    assert!(
        cmd_sessions(&h.ctx, &store, None)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" meta=0 ")
    );
}

/// Writing `questions` is Owner-only, like the supervisor claim, since it decides
/// whether the supervisor answers for the person: an edge, even with write on the
/// session, is `ERR denied` and stores nothing. Other forms fall through (`None`),
/// and an Owner's write lands.
#[test]
fn a_questions_write_is_owner_only() {
    use super::control_session::meta_questions_denied as denied;
    let h = registered_session(0, -1, b"");
    let writer = edge_granted(Op::WriteInput, &h.ctx);
    for rest in [
        "set questions recommended",
        "unset questions",
        "  set   questions ask",
    ] {
        assert_eq!(
            denied(writer, rest),
            Some(("ERR denied\n".to_string(), false)),
            "{rest:?}"
        );
        assert_eq!(
            denied(edge(), rest),
            Some(("ERR denied\n".to_string(), false))
        );
        assert_eq!(
            denied(Scope::Owner, rest),
            None,
            "an Owner's write proceeds"
        );
    }
    for rest in ["", "set title x", "unset attention", "set questionsx ask"] {
        assert_eq!(
            denied(writer, rest),
            None,
            "{rest:?} is not a questions write"
        );
    }
}

/// The Autopilot word on the wire (the Autopilot design, §2.1): the
/// supervisor's three modes and the owner's words for two of them, stored as
/// the mode's word, read back by key as the LAST field of every bare `meta` —
/// `autopilot=-` while unset, so the supervisor's loop knows the window keeps
/// it — counted by `ls meta=1`, cleared by `unset` (back to `[harness]
/// autopilot`). NEGATIVE CONTROLS: anything else, an on/off switch word
/// other than `off` included, is the one usage line and stores nothing.
#[test]
fn meta_autopilot_is_a_closed_set_stored_as_the_modes_word_and_read_last() {
    let usage = format!("ERR {}\n", crate::session_timeline::AUTOPILOT_USAGE);
    let usage = usage.as_str();
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let (read0, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(
        read0.ends_with(" autopilot=- land_at=-\n"),
        "always named: {read0}"
    );
    for bad in [
        "set autopilot",
        "set autopilot   ",
        "set autopilot on",
        "set autopilot true",
        "set autopilot cruising",
        "set autopilot land it",
    ] {
        let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, bad);
        assert_eq!(r, usage, "{bad:?}");
        assert!(!c, "a refusal moves nothing: {bad:?}");
    }
    assert_eq!(h.ctx.meta.lock().unwrap().autopilot, None);
    for (typed, stored) in [
        ("Off", "off"),
        ("wrap-up", "land"),
        ("INFINITY", "cruise"),
        ("land", "land"),
    ] {
        let (r, _) = cmd_meta(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("set autopilot {typed}"),
        );
        assert_eq!(r, "OK\n", "{typed}");
        let (read, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
        assert!(
            read.ends_with(&format!(" autopilot={stored} land_at=-\n")),
            "{read}"
        );
    }
    let (_, again) = cmd_meta(&h.term, &store, 0, &h.ctx, "set autopilot wrap");
    assert!(!again, "the owner's word for land is the same value");
    assert!(
        cmd_sessions(&h.ctx, &store, None)
            .lines()
            .nth(1)
            .unwrap()
            .contains(" meta=1 "),
        "an Autopilot word alone is user metadata -> meta=1"
    );
    let (r, c) = cmd_meta(&h.term, &store, 0, &h.ctx, "unset autopilot");
    assert_eq!(r, "OK\n");
    assert!(c);
    let (read, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(read.ends_with(" autopilot=- land_at=-\n"), "{read}");
    // `land_at=` follows it (the Autopilot design, §9.3.5): a timed Cruise's
    // instant as stored, in unix seconds — printed only, no field of its own.
    h.ctx.meta.lock().unwrap().autopilot = Some("cruise".into());
    h.ctx.meta.lock().unwrap().land_at = Some(1_792_249_200_000);
    let (read, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "");
    assert!(
        read.ends_with(" autopilot=cruise land_at=1792249200\n"),
        "{read}"
    );
    let (r, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "set land_at 18:00");
    assert!(r.starts_with("ERR "), "no field of its own: {r}");
    h.ctx.meta.lock().unwrap().autopilot = None;
    h.ctx.meta.lock().unwrap().land_at = None;
    let (r, _) = cmd_meta(&h.term, &store, 0, &h.ctx, "set colour red");
    assert!(
        r.contains("|questions|autopilot)"),
        "the closed field list names it: {r}"
    );
}

/// Writing `autopilot` is Owner-only, as `questions` is: an edge — even one
/// granted write on the session — must not put the harness's hands back on a
/// session its owner took them off. Reading it stays open.
#[test]
fn an_autopilot_write_is_owner_only() {
    use super::control_session::meta_questions_denied as denied;
    let h = registered_session(0, -1, b"");
    let writer = edge_granted(Op::WriteInput, &h.ctx);
    for rest in [
        "set autopilot cruise",
        "unset autopilot",
        " set autopilot off",
        // A timed Cruise rides the same field (design §9.3.5).
        "set autopilot cruise until=+1h",
        "set autopilot cruise until=18:00",
    ] {
        assert_eq!(
            denied(writer, rest),
            Some(("ERR denied\n".to_string(), false)),
            "{rest:?}"
        );
        assert_eq!(
            denied(Scope::Owner, rest),
            None,
            "an Owner's write proceeds"
        );
    }
    for rest in ["", "set autopilotx off", "set title autopilot"] {
        assert_eq!(denied(writer, rest), None, "{rest:?} is no autopilot write");
    }
}

/// Metadata byte caps are hard refusals, never truncation: a value one byte over
/// its field's cap is refused and stores nothing; an at-cap value is accepted.
#[test]
fn meta_caps_reject_over_cap_values() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    for (field, cap) in [
        ("title", 120),
        ("description", 1024),
        ("icon", 64),
        ("role", 64),
        ("attention", 256),
    ] {
        let set = |n: usize| {
            cmd_meta(
                &h.term,
                &store,
                0,
                &h.ctx,
                &format!("set {field} {}", "x".repeat(n)),
            )
        };
        let (reply, changed) = set(cap + 1);
        assert!(
            reply.starts_with(&format!("ERR {field} too long")) && !changed,
            "{field}: {reply:?}"
        );
        assert!(!h.ctx.meta.lock().unwrap().any_set(), "{field}: stored");
        assert_eq!(set(cap), ("OK\n".to_string(), true), "{field} at its cap");
        let _ = cmd_meta(&h.term, &store, 0, &h.ctx, &format!("unset {field}"));
    }
}

/// User metadata reaches native OS chrome, so bytes that could add a line or
/// reverse/isolate visual order are refused; a rejected write changes neither
/// metadata nor timeline.
#[test]
fn meta_rejects_controls_line_separators_and_bidi_formatting() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let baseline_events = h.ctx.timeline.lock().unwrap().len();

    for hostile in [
        "set title safe\nsecond-line",
        "set description left\u{202e}right",
        "set description one\u{2029}two",
        "set icon \u{2066}🚀\u{2069}",
        "set title hidden\u{200b}suffix",
        "set title soft\u{00ad}hyphen",
        "set title tagged\u{e0001}text",
    ] {
        let (reply, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, hostile);
        assert!(
            reply.starts_with("ERR "),
            "hostile value refused: {reply:?}"
        );
        assert!(!changed, "a refused write is not a change: {hostile:?}");
    }
    assert!(!h.ctx.meta.lock().unwrap().any_set());
    assert_eq!(
        h.ctx.timeline.lock().unwrap().len(),
        baseline_events,
        "refusals do not create meta-change events"
    );

    // ZWJ is allowed: rejecting it would break ordinary compound emoji.
    let (reply, changed) = cmd_meta(&h.term, &store, 0, &h.ctx, "set icon 👨‍👩‍👧‍👦");
    assert_eq!(reply, "OK\n");
    assert!(changed);
}

/// `timeline`: lifecycle events (spawned, state, title, meta changes) are recorded
/// in order with monotonic ids by the store mutators and `meta set`, one per actual
/// change. (The `<n>` / `since=<id>` grammar: `control_session`'s
/// `history_and_timeline_limits_match_retained_wire_rows`.)
#[test]
fn timeline_records_lifecycle_in_order() {
    let store = session_store::new_store();
    let h = registered_session(3, -1, b"");
    store.write().unwrap().register(h.clone()); // -> spawned
    store.write().unwrap().set_title(3, "vim"); // -> title-change
    let (_, changed) = cmd_meta(&h.term, &store, 3, &h.ctx, "set title build agent");
    assert!(changed); // -> meta-change
    store
        .write()
        .unwrap()
        .set_state(3, session_store::SessionState::Exited); // -> state-change
    // A SAME-state re-mark records nothing (change-gated).
    store
        .write()
        .unwrap()
        .set_state(3, session_store::SessionState::Exited);

    let out = cmd_timeline(&h.ctx, "");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[0], "OK 4",
        "exactly one event per actual change: {out}"
    );
    assert!(
        lines[1].starts_with("event 1 t=") && lines[1].contains("kind=spawned state=alive"),
        "birth first: {}",
        lines[1]
    );
    assert!(
        lines[2].contains(&format!("kind=title-change title={}", pct_encode("vim"))),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].contains(&format!(
            "kind=meta-change field=title value={}",
            pct_encode("build agent")
        )),
        "{}",
        lines[3]
    );
    assert!(
        lines[4].contains("kind=state-change state=exited"),
        "{}",
        lines[4]
    );
    // Monotonic ids 1..=4, monotonic timestamps.
    let ids: Vec<u64> = lines[1..]
        .iter()
        .map(|l| l.split_whitespace().nth(1).unwrap().parse().unwrap())
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4]);
    // Malformed args answer a usage ERR, mirroring `history`.
    assert!(cmd_timeline(&h.ctx, "since=bogus").starts_with("ERR usage"));

    // The death half: deregistration records the final state-change.
    store.write().unwrap().deregister_local(3);
    let after = cmd_timeline(&h.ctx, "since=4");
    assert!(
        after.contains("kind=state-change state=closed"),
        "deregister leaves the closing event: {after}"
    );
}

// ── P1.3 subscribe wiring ────────────────────────────────────────────────

/// Build an [`ActiveHandle`] over a registered session's tuple, so `@.` self
/// subscribe follows the active tab the same way a self read does.
fn active_for(h: &SessionHandle) -> ActiveHandle {
    Arc::new(Mutex::new(Some(ActiveSession {
        term: h.term.clone(),
        master: h.master,
        id: h.local_id,
        ctx: h.ctx.clone(),
    })))
}

/// `authenticate_connection` records its own refusals by kind through the
/// `AuthDenials` it is handed: a blind dial (no `AUTH` line), a wrong 64-hex token
/// checked against a live edge table, and a token with no active context, so a guess
/// is not folded into discovery dials. The instance token records nothing (the
/// control).
#[test]
fn a_refused_handshake_is_recorded_by_kind_through_the_denials_it_is_given() {
    use std::io::{BufRead, Write};
    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let h = registered_session(0, -1, b"");
    let with_ctx = active_for(&h);
    let no_ctx: ActiveHandle = Arc::new(Mutex::new(None));
    let denials = control_denials::AuthDenials::new();
    let dial = |first: &str, active: &ActiveHandle| {
        let (mut client, server) = CtlStream::pair().unwrap();
        client.write_all(format!("{first}\n").as_bytes()).unwrap();
        let mut reader = control_lanes::lane_reader(&server, Vec::new());
        let deadline = std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        let got = authenticate_connection(&server, &mut reader, TOKEN, active, &denials, deadline);
        if got.is_none() {
            let mut reply = String::new();
            BufReader::new(&client).read_line(&mut reply).unwrap();
            assert_eq!(reply, "ERR auth\n", "`{first}` is answered");
        }
        got.is_some()
    };
    let wrong = format!("AUTH {}", "ab".repeat(32));
    assert!(!dial("sessions", &with_ctx));
    assert!(!dial(&wrong, &with_ctx));
    assert!(!dial(&wrong, &with_ctx));
    assert!(!dial(&wrong, &no_ctx));
    assert!(dial(&format!("AUTH {TOKEN}"), &with_ctx));
    let totals = [
        control_denials::NO_TOKEN,
        control_denials::WRONG_TOKEN,
        control_denials::NO_CONTEXT,
    ]
    .map(|kind| denials.total(kind));
    assert_eq!(totals, [1, 2, 1], "blind, wrong, no-context");
}

/// Accumulate pushed bytes from a `CtlStream` until `pred` matches the accumulated
/// text or a generous deadline passes. Frames may arrive split or coalesced, so
/// matching substrings on the accumulation is robust to that and to scheduling.
fn read_until(s: &CtlStream, mut acc: String, pred: impl Fn(&str) -> bool) -> String {
    use std::io::Read;
    let mut s = s.try_clone().expect("clone client end");
    s.set_read_timeout(Some(std::time::Duration::from_millis(200)))
        .unwrap();
    // A failure bound, not a wait: the loop returns as soon as `pred` matches. It
    // must clear the slowest push path (the `sessions` stream's 250 ms instance-diff
    // tick on a background thread) under a saturated machine, where scheduling
    // starvation is unbounded in principle.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut buf = [0u8; 8192];
    while !pred(&acc) && std::time::Instant::now() < deadline {
        match s.read(&mut buf) {
            Ok(n) if n > 0 => acc.push_str(&String::from_utf8_lossy(&buf[..n])),
            Ok(_) => break, // EOF
            Err(_) => {}    // timeout: loop until the deadline
        }
    }
    acc
}

/// Whether `text` holds a whole `screen` frame — a subscription's
/// catch-up or a later DELTA: the `DELTA … screen <nrows>` header (the
/// first in `text`) and every row it announces.
fn screen_frame_read_whole(text: &str) -> bool {
    text.split_once(" screen ")
        .and_then(|(_, rest)| rest.split_once('\n'))
        .and_then(|(rows, body)| rows.parse::<usize>().ok().map(|rows| (rows, body)))
        .is_some_and(|(rows, body)| body.matches('\n').count() >= rows)
}

// A test's client hangs up for the whole socket ([`hang_up`]), so the
// production peer probe ([`run_subscribe_socket`]) ends its push loop on
// the next tick whatever copies of the client end a sibling's fork holds.
use crate::test_socket::{hang_up, join_after_hang_up};

/// A SUBSCRIBER THAT HALF-CLOSED (`printf 'subscribe …\n' | nc -U`) is gone
/// to the protocol but still reading: when its push stream ends it reads
/// END-OF-FILE, though a copy of the SERVER end survives — the copy any
/// thread's fork holds until it execs (a `try_clone` here). Once the push
/// loop has returned every shutdown it makes is made, so one drain of
/// non-blocking reads is the verdict.
///
/// NEGATIVE CONTROL (run by restoring the both-ways `shutdown` both of its
/// production callers made after it): Darwin refused it for a socket whose
/// read half the half-close had shut (`ENOTCONN`) and shut nothing, so the
/// subscriber read `WouldBlock`, not its end-of-file, while the copy lived.
#[test]
fn a_half_closed_subscriber_reads_eof_when_its_stream_ends_though_a_copy_survives() {
    use std::io::Read as _;

    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();
    let (client, server) = CtlStream::pair().unwrap();
    // Every read below is bounded by a hang detector, not a poll: the
    // end-of-file the test reads last is owed by then (the push loop has
    // ended), and a read that waits a minute for it fails. Set here, on
    // the fresh socket: macOS refuses `SO_RCVTIMEO` (EINVAL) once both
    // halves are shut. `set_read_timeout` is on both platforms' `CtlStream`.
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(60)))
        .expect("read timeout");
    let forked_copy = server
        .try_clone()
        .expect("a surviving copy of the server end");
    let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
    let join = std::thread::spawn(move || {
        let mut w = server;
        run_subscribe_socket(
            "subscribe @. screen",
            &active_t,
            &store_t,
            &reg_t,
            Scope::Owner,
            &mut w,
        );
    });
    let acc = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
    assert!(acc.contains("OK subscribe 1\n"), "subscribe ack: {acc:?}");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("the subscriber half-closes");
    join_after_hang_up(join, "the push loop to end on its subscriber's half-close");

    let mut rest = Vec::new();
    let end = (&client).read_to_end(&mut rest).map_err(|e| e.kind());
    assert!(
        end.is_ok(),
        "the subscriber read no end-of-file from a push stream that ended while a copy \
         of its server end lived: {end:?}"
    );
    drop(forked_copy);
}

/// **`topic` IS OWNER-CLASS AND SELECTOR-TAKING, and the refusal is
/// AUDITED.**
///
/// It takes `@<sid>` so a manager can arm a worker's topics from its own window, and
/// it is Owner-class because a topic decides what lands in an agent's inbox; an
/// edge token reading a session must not route a stranger's words into it, so the
/// refusal is the audited one, not the generic gate's silent `ERR denied`.
#[test]
fn topic_is_owner_class_and_names_its_session() {
    let store = session_store::new_store();
    let subscribers = crate::subscribe::new_registry();
    let h = registered_session(0, -1, b"");
    let sid = h.sid.clone();
    let h_ctx = h.ctx.clone();
    store.write().unwrap().register(h);

    // An EDGE token is refused, whatever it is a token for.
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    assert_eq!(
        dispatch_topic_verb(
            "ls",
            Some(&Selector::Sid(sid.clone())),
            edge,
            &store,
            &subscribers,
            None
        ),
        "ERR denied\n"
    );

    // OWNER, naming the session: the empty set, which is the default.
    let owner_ls = |rest: &str| {
        dispatch_topic_verb(
            rest,
            Some(&Selector::Sid(sid.clone())),
            Scope::Owner,
            &store,
            &subscribers,
            None,
        )
    };
    assert_eq!(owner_ls("ls"), "OK 0 seen=0\n");
    assert_eq!(
        owner_ls("add build.failed"),
        "OK build.failed since=head added=1\n"
    );
    assert_eq!(
        owner_ls("add sat-comp since=@42"),
        "OK sat-comp since=@42 added=1\n"
    );
    assert_eq!(
        owner_ls("ls"),
        "OK 2 seen=0\ntopic build.failed since=head\ntopic sat-comp since=@42\n"
    );
    // A repeated add does not rewind: the answer says nothing was added and the
    // stored `since` is the first add's.
    assert_eq!(
        owner_ls("add sat-comp since=@0"),
        "OK sat-comp since=@0 added=0\n"
    );
    assert!(owner_ls("ls").contains("topic sat-comp since=@42\n"));
    assert_eq!(owner_ls("drop sat-comp"), "OK sat-comp dropped=1\n");
    assert_eq!(owner_ls("drop sat-comp"), "OK sat-comp dropped=0\n");
    assert_eq!(
        owner_ls("add sat-comp since=@4"),
        "OK sat-comp since=@4 added=1\n"
    );
    assert!(owner_ls("ls").contains("topic sat-comp since=@4\n"));
    // Every change is pushed: one `topic` event per effective add or drop, in
    // order, `since=` included (the bridge's cue).
    let pushed: Vec<String> = h_ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|e| e.kind == "topic")
        .map(|e| e.payload.clone())
        .collect();
    assert_eq!(
        pushed,
        [
            "add build.failed since=head",
            "add sat-comp since=@42",
            "drop sat-comp",
            "add sat-comp since=@4",
        ]
    );
    // An offset has no sign: `@+5` is refused, not normalised to `@5`.
    assert!(owner_ls("add signed since=@+5").starts_with("ERR usage: topic"));
    // The grammar, on the verb's own door.
    for bad in [
        "add",
        "add A",
        "add a b",
        "add a since=yesterday",
        "wat",
        "ls extra",
    ] {
        assert!(
            owner_ls(bad).starts_with("ERR usage: topic"),
            "`{bad}` must be a usage error: {}",
            owner_ls(bad)
        );
    }
    // A session nobody hosts.
    assert_eq!(
        dispatch_topic_verb(
            "ls",
            Some(&Selector::Sid(aterm_session::SessionId::new("s-nobody"))),
            Scope::Owner,
            &store,
            &subscribers,
            None,
        ),
        "ERR no such session\n"
    );
}

/// ONE SESSION OPTS INTO AT MOST `MAX_TOPICS` TOPICS (round eight of the
/// update audit, U54). Every opt-in rides the session's handoff record,
/// which the writer may not shed, so a script that subscribed one topic per
/// task grew the manifest past the cap every successor reads it under and
/// the old build refused each update on content. An `add` past the bound
/// is refused, changes nothing and pushes nothing; a repeated add and a
/// drop still answer, and once one is dropped a new one goes in. A set
/// carried across an update is taken whole, past the bound included (the
/// consumer is only ever more lenient), and adds again only below it.
/// RED before the fix: every add answered `added=1`.
#[test]
fn a_session_opts_into_at_most_the_topic_bound() {
    use crate::fabric::MAX_TOPICS;
    let store = session_store::new_store();
    let subscribers = crate::subscribe::new_registry();
    let h = registered_session(0, -1, b"");
    let sid = h.sid.clone();
    let h_ctx = h.ctx.clone();
    store.write().unwrap().register(h);
    let owner = |rest: &str| {
        dispatch_topic_verb(
            rest,
            Some(&Selector::Sid(sid.clone())),
            Scope::Owner,
            &store,
            &subscribers,
            None,
        )
    };
    for n in 0..MAX_TOPICS {
        assert_eq!(
            owner(&format!("add task-{n}")),
            format!("OK task-{n} since=head added=1\n")
        );
    }
    let pushed = |ctx: &crate::SessionCtx| {
        ctx.timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "topic")
            .count()
    };
    let before = pushed(&h_ctx);
    let full = |reply: String| reply.starts_with("ERR topics full");
    assert!(full(owner("add task-over")));
    assert_eq!(pushed(&h_ctx), before, "a refused add pushes nothing");
    assert!(owner("ls").starts_with(&format!("OK {MAX_TOPICS} seen=0\n")));
    assert!(!owner("ls").contains("task-over"));
    // A topic already held still answers as one.
    assert_eq!(owner("add task-0"), "OK task-0 since=head added=0\n");
    // Drop one, and a new one goes in.
    assert_eq!(owner("drop task-0"), "OK task-0 dropped=1\n");
    assert_eq!(owner("add task-over"), "OK task-over since=head added=1\n");
    assert!(full(owner("add task-0")));

    // A carried set past the bound is taken whole.
    let carried = (0..MAX_TOPICS + 5)
        .map(|n| format!("old-{n} head"))
        .collect::<Vec<_>>();
    h_ctx
        .fabric
        .set_topics(crate::fabric::parse_topics(&carried));
    assert!(owner("ls").starts_with(&format!("OK {} seen=0\n", MAX_TOPICS + 5)));
    assert!(full(owner("add fresh")));
    for n in 0..6 {
        assert_eq!(
            owner(&format!("drop old-{n}")),
            format!("OK old-{n} dropped=1\n")
        );
    }
    assert_eq!(owner("add fresh"), "OK fresh since=head added=1\n");
}

/// `subscribe … mail` pushes metadata and no body bytes: two rows delivered to the
/// watched inbox yield two `MAIL` lines on the connection (the events digest's
/// grammar), and their body (`the-secret-words`) appears nowhere in the stream; a
/// manager reads it with `inbox get`. Then `kinds=` narrows to one.
#[test]
fn subscribe_mail_pushes_one_metadata_line_per_row_and_never_a_body() {
    const SECRET: &str = "the-secret-words";
    // `deliveries` is `(from, kind, extra)`, where `extra` is the rest of the
    // `deliver` line (`topic=…`), so one harness serves addressed and broadcast rows.
    let watch =
        |streams: &'static str, deliveries: &'static [(&str, &str, &str)], want: usize| -> String {
            let store = session_store::new_store();
            let h = registered_session(0, -1, b"");
            store.write().unwrap().register(h.clone());
            let active = active_for(&h);
            let registry = subscribe::new_registry();
            let sid = h.sid.as_str().to_string();

            let (client, server) = CtlStream::pair().unwrap();
            let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
            let line = format!("subscribe @. {streams}");
            let join = std::thread::spawn(move || {
                let mut w = server;
                run_subscribe_socket(&line, &active_t, &store_t, &reg_t, Scope::Owner, &mut w);
            });
            let acc = read_until(&client, String::new(), |s| s.contains("OK subscribe 1\n"));
            assert!(acc.contains("OK subscribe 1\n"), "ack: {acc:?}");

            // THE ACK IS THE BARRIER. The push loop
            // (`subscribe::push_loop_with_peer_probe`) seeds every watch's
            // cursor, registers, and only then writes the ack, so a row
            // delivered once the ack is read is genuinely new — never folded
            // into the seed. (The dispatch used to write the ack before the
            // loop seeded anything, and this waited for the registration
            // instead.) The registration is a fact by now, not a wait.
            assert_eq!(
                registry.lock().unwrap().watched_sessions(),
                1,
                "the ack follows the watch's registration"
            );

            // A broadcast reaches only a session that holds its topic.
            for t in deliveries
                .iter()
                .filter_map(|(_, _, extra)| extra.trim().strip_prefix("topic="))
            {
                let _ = h.ctx.fabric.topic_add(t, "head", &h.ctx.timeline);
            }
            for (n, (from, kind, extra)) in deliveries.iter().enumerate() {
                let off = 500 + n as u64;
                let reply = crate::fabric::cmd_deliver(
                    &store,
                    &format!(
                        "{sid} off={off} from={from} kind={kind} trust=agent {extra}text={SECRET}"
                    ),
                );
                assert!(reply.starts_with("OK"), "deliver: {reply}");
            }
            // The mail stream rides the loop's liveness tick; a notify only
            // makes it prompt.
            registry.lock().unwrap().notify(0);
            // Wait for what this subscription will send, not one line per delivery: a
            // filtered watch drops rows on purpose.
            let acc = read_until(&client, acc, |s| s.matches("MAIL 0 ").count() >= want);
            // A quiet mail-only subscription writes nothing, so its end is the
            // peer probe's to find: the hang-up, read on the loop's next tick.
            // (It used to be a failed WRITE, which needed a wind-down delivery
            // every filter here accepted — and which no write ever produces
            // while a sibling's fork holds a copy of the client end.)
            hang_up(client);
            join_after_hang_up(join, "the mail push loop to end on its client's hang-up");
            acc
        };

    let both = watch(
        "mail",
        &[("h-andrew", "task", ""), ("s-peer", "note", "")],
        2,
    );
    assert!(
        both.contains("MAIL 0 id=1 off=500 from=h-andrew kind=task\n"),
        "{both:?}"
    );
    assert!(
        both.contains("MAIL 0 id=2 off=501 from=s-peer kind=note\n"),
        "{both:?}"
    );
    assert_eq!(both.matches("MAIL 0 ").count(), 2, "{both:?}");
    // The body is nowhere in the stream.
    assert!(
        !both.contains(SECRET),
        "a body reached a mail subscriber: {both:?}"
    );
    assert!(!both.contains("text="), "{both:?}");

    // `kinds=` narrows: the `note` is stepped over, never pushed.
    let filtered = watch(
        "mail:kinds=task",
        &[("h-andrew", "task", ""), ("s-peer", "note", "")],
        1,
    );
    assert!(filtered.contains("MAIL 0 id=1 "), "{filtered:?}");
    assert_eq!(filtered.matches("MAIL 0 ").count(), 1, "{filtered:?}");
    assert!(!filtered.contains("kind=note"), "{filtered:?}");
    assert!(!filtered.contains(SECRET), "{filtered:?}");

    // `mail:topic=` narrows to a broadcast topic, which the push line names: the
    // addressed row is stepped over, which makes it a filter rather than a label.
    let only_topic = watch(
        "mail:topic=build.failed",
        &[
            ("h-andrew", "task", ""),
            ("s-peer", "note", "topic=build.failed "),
            ("s-peer", "note", "topic=other.thing "),
        ],
        1,
    );
    assert_eq!(only_topic.matches("MAIL 0 ").count(), 1, "{only_topic:?}");
    assert!(
        only_topic.contains("MAIL 0 id=2 ") && only_topic.contains("topic=build.failed\n"),
        "{only_topic:?}"
    );
}

/// A push-only client can disappear while every watched terminal is quiet. The
/// server must discover that from the socket's read side, not from a future
/// write, or each crash permanently occupies one of the four reserved workers.
/// Run more sequential crash cycles than the pool size, each client crashing
/// with every frame read and nothing left to write; every loop must return
/// and deregister on its own. The request is selectorless (`subscribe screen`),
/// which names the connection's own session like every selectorless verb.
#[test]
fn quiet_subscription_crashes_reap_past_the_worker_pool_size() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();

    for cycle in 0..(CONTROL_SUBSCRIPTION_WORKERS + 2) {
        let (client, server) = CtlStream::pair().expect("subscription socket pair");
        let (store_t, active_t, registry_t) = (store.clone(), active.clone(), registry.clone());
        let join = std::thread::spawn(move || {
            let mut server = server;
            run_subscribe_socket(
                "subscribe screen",
                &active_t,
                &store_t,
                &registry_t,
                Scope::Owner,
                &mut server,
            );
        });
        let ack = read_until(&client, String::new(), |text| {
            text.contains("OK subscribe 1\n")
        });
        assert!(
            ack.contains("OK subscribe 1\n"),
            "cycle {cycle} entered push mode: {ack:?}"
        );

        // The immediate catch-up is read WHOLE — the `screen` frame's header
        // and every row it announces — so no write is pending and the server
        // can only learn of the hangup below from the socket's read side.
        // Dropping earlier could be detected by a pending catch-up write,
        // which would not exercise the HUP-only failure mode. (350 ms of
        // sleep stood in for this fact.)
        let caught_up = read_until(&client, ack, screen_frame_read_whole);
        assert!(
            screen_frame_read_whole(&caught_up),
            "cycle {cycle} received its catch-up: {caught_up:?}"
        );
        assert!(
            caught_up.contains(&format!("sub 0 {}\n", h.sid.as_str())),
            "cycle {cycle}: the one target is self: {caught_up:?}"
        );
        // THE FULLY-OPEN QUIET PEER IS KEPT, at a fact: a wake asks the
        // peer probe BEFORE it pushes, so the DELTA this change produces,
        // read whole, is a probe that found this peer there and a loop that
        // went on. (`!join.is_finished()` right after the catch-up asked
        // nothing: a loop that ended at its first probe still passed it.)
        let marker = format!("live-{cycle}");
        crate::term_lock(&h.term).process(marker.as_bytes());
        registry.lock().unwrap().notify(0);
        let delta = read_until(&client, String::new(), |text| {
            text.contains(&marker) && screen_frame_read_whole(text)
        });
        assert!(
            delta.contains(&marker) && screen_frame_read_whole(&delta),
            "cycle {cycle} keeps a fully-open quiet peer subscribed: {delta:?}"
        );

        // No output or registry notify follows. Socket EOF is the only fact
        // capable of releasing this worker.
        hang_up(client);
        join_after_hang_up(join, &format!("quiet dead subscriber {cycle} to be reaped"));
        assert_eq!(
            registry.lock().unwrap().watched_sessions(),
            0,
            "cycle {cycle} released its registry entry"
        );
    }
}

/// A PUSHED FRAME WAITS ON NO PROBE READ. The push loop asks its peer probe
/// before every pass, the real wakes included, so a probe read the kernel may
/// park is a wait in front of every frame: the 1 ms receive-timeout `read` the
/// probe used to be held every `DELTA` and `EVENT` to every quiet (that is,
/// live) subscriber about 1.1 ms, the fabric bridge's `EVENT post` on the post
/// → visible path among them. Counted, never timed: over `FRAMES` pushed
/// DELTAs the push thread's [`push_probe_tally`] must show a probe read for
/// every pass (one per pushed frame, plus the one that sees the hang-up —
/// liveness is still asked every pass) and NONE that could park.
///
/// It judges the read the tally is attached to, by that read's own flags; a
/// wait put BESIDE the read is the kernel's to count, and
/// [`the_push_probe_enters_the_kernel_once`] counts it.
///
/// NEGATIVE CONTROL, run here: the same frames through the probe this
/// replaced — a clone under a 1 ms receive timeout, then `read` — tally a
/// parking read for every pass, at least one per pushed frame.
#[cfg(unix)]
#[test]
fn a_pushed_frame_waits_on_no_probe_read() {
    use std::os::fd::AsRawFd as _;

    use super::push_probe_tally::{self, Tally};

    const FRAMES: u64 = 6;
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();

    // One subscription over a real socket pair, FRAMES DELTAs pushed through
    // it one at a time (each read whole before the next change), then a
    // hang-up: the push thread's tally.
    let push = |replaced_probe: bool| -> Tally {
        let (client, server) = CtlStream::pair().expect("subscription socket pair");
        let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
        let join = std::thread::spawn(move || {
            let mut server = server;
            let line = "subscribe @. screen";
            if replaced_probe {
                let probe = server.try_clone().expect("probe clone");
                probe
                    .set_read_timeout(Some(std::time::Duration::from_millis(1)))
                    .expect("the replaced probe's 1 ms bound");
                run_subscribe_with_peer_probe(
                    line,
                    &active_t,
                    &store_t,
                    &reg_t,
                    Scope::Owner,
                    &mut server,
                    move || {
                        push_probe_tally::record(probe.as_raw_fd(), 0);
                        let mut byte = [0_u8; 1];
                        !matches!(
                            (&probe).read(&mut byte),
                            Err(error) if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::Interrupted
                            )
                        )
                    },
                );
                aterm_uds::hangup::hang_up(&server);
            } else {
                run_subscribe_socket(line, &active_t, &store_t, &reg_t, Scope::Owner, &mut server);
            }
            push_probe_tally::take()
        });
        let ack = read_until(&client, String::new(), |text| {
            text.contains("OK subscribe 1\n")
        });
        let caught_up = read_until(&client, ack, screen_frame_read_whole);
        assert!(
            screen_frame_read_whole(&caught_up),
            "entered push mode and read the catch-up: {caught_up:?}"
        );
        for frame in 0..FRAMES {
            let marker = format!("pf{frame}x");
            crate::term_lock(&h.term).process(marker.as_bytes());
            registry.lock().unwrap().notify(0);
            let delta = read_until(&client, String::new(), |text| {
                text.contains(&marker) && screen_frame_read_whole(text)
            });
            assert!(
                delta.contains(&marker) && screen_frame_read_whole(&delta),
                "frame {frame} pushed: {delta:?}"
            );
        }
        hang_up(client);
        join_after_hang_up(join, "the push loop to end on its subscriber's hang-up")
    };

    let tally = push(false);
    assert!(
        tally.reads > FRAMES,
        "the peer is still asked before every pass: {tally:?} over {FRAMES} frames"
    );
    assert_eq!(
        tally.parking, 0,
        "no probe read in front of a pushed frame may park: {tally:?} over {FRAMES} frames"
    );

    let replaced = push(true);
    assert!(
        replaced.parking > FRAMES && replaced.parking == replaced.reads,
        "negative control: the 1 ms receive-timeout probe parks before every pass: \
         {replaced:?} over {FRAMES} frames"
    );
}

/// A live subscriber keeps its write half open, so the server can tell a quiet
/// reader from a dead or half-closed one without heartbeat traffic.
///
/// AND THE PROBE NEVER WAITS FOR THE ANSWER: the socket here has NO receive
/// timeout, so a probe that read instead of peeking without waiting would
/// park on this quiet peer forever. The quiet-peer probe runs on its own
/// thread under a hang detector, which names that instead of wedging the
/// suite. NEGATIVE CONTROL (run by restoring the blocking `read`): the
/// detector fires. The probe this replaced bounded the same park with a 1 ms
/// timeout, and paid it before every frame pushed
/// ([`a_pushed_frame_waits_on_no_probe_read`] counts that).
#[test]
fn subscription_probe_keeps_quiet_peer_and_rejects_write_half_close() {
    let (client, server) = CtlStream::pair().expect("probe socket pair");
    server
        .set_read_timeout(None)
        .expect("no receive timeout: nothing bounds a probe that would park");
    let probe = std::thread::spawn(move || {
        let gone = subscription_peer_gone(&server);
        (server, gone)
    });
    let (server, gone) = join_after_hang_up(
        probe,
        "the probe of a quiet open peer to answer without a receive timeout",
    );
    assert!(!gone, "a fully-open quiet peer remains subscribed");

    client
        .shutdown(std::net::Shutdown::Write)
        .expect("client closes its send half");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !subscription_peer_gone(&server) && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(
        subscription_peer_gone(&server),
        "client write-half close is a protocol-level disconnect"
    );
}

/// THE PUSH PROBE ENTERS THE KERNEL ONCE: its `recv`, and nothing else.
///
/// [`a_pushed_frame_waits_on_no_probe_read`] judges the probe's one `recv` by
/// the flags that `recv` was given, so a wait added BESIDE it — a bounded
/// `poll(fd, POLLIN, 1)` before the peek, a 1 ms receive timeout and a `read`
/// next to it — passes that tally, and passes the quiet-peer hang detector
/// above too, because the wait is bounded. Every way a thread waits is a
/// kernel entry (a `poll`, a timed `read`, `nanosleep`, a contended lock's
/// `ulock`/`psynch` wait, a Mach semaphore), so this counts the probe's: the
/// kernel's own per-task count of BSD syscalls plus Mach traps
/// (`proc_pidinfo(PROC_PIDTASKINFO)`) must grow by exactly one per probe of a
/// quiet open peer. With the quiet-peer test above — that one `recv` answers
/// on a socket with nothing to bound a park — no part of the probe can wait
/// (short of a spin on the clock, which no kernel entry reveals).
///
/// The count is per TASK, so the test runs alone in its own process
/// ([`crate::test_alone`]), where the only other thread is libtest's, parked
/// on its result channel for the whole count. It counts, never times: a
/// scheduler under load preempts the loop but adds no syscall to it, and the
/// count's own reads are measured and taken off, not assumed.
///
/// NEGATIVE CONTROL, run here: the same count over the probe with a 1 ms
/// readiness `poll` in front of it grows by two per probe — the wait the
/// tally cannot see.
#[cfg(target_os = "macos")]
#[test]
fn the_push_probe_enters_the_kernel_once() {
    /// `struct proc_taskinfo` of `<sys/proc_info.h>`, which `libc` does not
    /// declare. The kernel copies it out only into a buffer at least its size
    /// and answers the byte count, which is checked, so a layout that drifted
    /// fails the count's read instead of reading past this one.
    #[repr(C)]
    #[derive(Default)]
    struct ProcTaskInfo {
        virtual_size: u64,
        resident_size: u64,
        total_user: u64,
        total_system: u64,
        threads_user: u64,
        threads_system: u64,
        policy: i32,
        faults: i32,
        pageins: i32,
        cow_faults: i32,
        messages_sent: i32,
        messages_received: i32,
        syscalls_mach: i32,
        syscalls_unix: i32,
        csw: i32,
        threadnum: i32,
        numrunning: i32,
        priority: i32,
    }
    const PROC_PIDTASKINFO: libc::c_int = 4;
    const PROBES: u64 = 64;

    if !crate::test_alone::alone() {
        return;
    }
    let pid = libc::c_int::try_from(std::process::id()).expect("a pid fits a c_int");
    let size = libc::c_int::try_from(std::mem::size_of::<ProcTaskInfo>()).expect("96 bytes");
    // This task's kernel entries so far, the read's own included.
    let entries = || -> u64 {
        let mut info = ProcTaskInfo::default();
        // SAFETY: `info` is an owned, writable `ProcTaskInfo` of `size` bytes,
        // and the kernel writes at most `size` bytes into it.
        let wrote =
            unsafe { libc::proc_pidinfo(pid, PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size) };
        assert_eq!(
            wrote, size,
            "proc_pidinfo(PROC_PIDTASKINFO) answers one proc_taskinfo"
        );
        let count = |n: i32| u64::try_from(n).expect("a count is never negative");
        count(info.syscalls_unix) + count(info.syscalls_mach)
    };
    // What two reads back to back cost: the second's own entry.
    let reads = {
        let before = entries();
        entries() - before
    };

    let (_client, server) = CtlStream::pair().expect("probe socket pair");
    // The kernel entries `PROBES` probes made, the count's own reads taken off.
    let over_probes = |probe: &dyn Fn() -> bool| -> u64 {
        // The first probe pays its first touches (its thread-local tally) once.
        assert!(!probe(), "a fully-open quiet peer is there");
        let before = entries();
        let mut gone = 0_u64;
        for _ in 0..PROBES {
            gone += u64::from(probe());
        }
        let after = entries();
        assert_eq!(
            gone, 0,
            "a fully-open quiet peer stays there for every probe"
        );
        after - before - reads
    };

    assert_eq!(
        over_probes(&|| subscription_peer_gone(&server)),
        PROBES,
        "the push probe enters the kernel once, for its recv, over {PROBES} probes"
    );

    let polled = over_probes(&|| {
        use std::os::fd::AsRawFd as _;
        let mut ready = libc::pollfd {
            fd: server.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one owned, initialised `pollfd` for an fd `server` keeps open.
        let _ = unsafe { libc::poll(&raw mut ready, 1, 1) };
        subscription_peer_gone(&server)
    });
    assert_eq!(
        polled,
        2 * PROBES,
        "negative control: a readiness wait in front of the recv is a second kernel entry \
         per probe, over {PROBES} probes"
    );
}

/// A FLOOD OF WAKES IS PACED; A WAKE WITH ROOM IS NOT. The push probe's 1 ms
/// sleep was also the loop's only coalescing window, and without it a session
/// printing flat out woke one pass — a terminal lock, a rendered screen, a
/// frame — per output burst, thousands a second ([`crate::subscribe`]'s
/// `Pacer` has the measurements). The pass budget puts the window back for a
/// flood only: [`crate::subscribe::PASS_BURST`] woken passes go at once, and
/// past them a pass waits for its [`crate::subscribe::PASS_INTERVAL`].
///
/// Counted, never timed: the pacer's clock is frozen on the push thread, so
/// how fast this machine runs decides nothing. Frozen, `FRAMES` DELTAs pushed
/// back to back (each read whole before the next change) spend the burst and
/// then pause before every further pass; with the clock stepped one interval
/// per wake, the same frames never pause — a keystroke's echo or a lone
/// `EVENT` is not held for a flood that is not happening. Before the budget,
/// the frozen run paused before none.
#[cfg(unix)]
#[test]
fn a_flood_of_wakes_is_paced_and_a_wake_with_room_is_not() {
    use crate::subscribe::pace_clock::tally::{self, Tally};
    use crate::subscribe::{PASS_BURST, PASS_INTERVAL};

    const FRAMES: u64 = 14;
    let burst = u64::from(PASS_BURST);
    assert!(
        FRAMES > burst + 2,
        "the run spends the burst and paces past it"
    );
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();

    // One subscription over a real socket pair with the push thread's pacer
    // clock frozen and moved on by `step` at every wake (the probe is the hook
    // the loop calls on its thread after a wake, before its pass), FRAMES
    // DELTAs pushed one at a time, then a hang-up: the push thread's tally.
    let push = |step: std::time::Duration| -> Tally {
        let (client, server) = CtlStream::pair().expect("subscription socket pair");
        let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
        let join = std::thread::spawn(move || {
            tally::freeze();
            let mut server = server;
            let probe = server.try_clone().expect("probe clone");
            run_subscribe_with_peer_probe(
                "subscribe @. screen",
                &active_t,
                &store_t,
                &reg_t,
                Scope::Owner,
                &mut server,
                move || {
                    tally::step(step);
                    subscription_peer_gone(&probe)
                },
            );
            aterm_uds::hangup::hang_up(&server);
            tally::take()
        });
        let ack = read_until(&client, String::new(), |text| {
            text.contains("OK subscribe 1\n")
        });
        let caught_up = read_until(&client, ack, screen_frame_read_whole);
        assert!(
            screen_frame_read_whole(&caught_up),
            "entered push mode and read the catch-up: {caught_up:?}"
        );
        for frame in 0..FRAMES {
            // A line each, so no marker wraps across two rows.
            let marker = format!("pace{frame}x");
            crate::term_lock(&h.term).process(format!("{marker}\r\n").as_bytes());
            registry.lock().unwrap().notify(0);
            let delta = read_until(&client, String::new(), |text| {
                text.contains(&marker) && screen_frame_read_whole(text)
            });
            assert!(
                delta.contains(&marker) && screen_frame_read_whole(&delta),
                "frame {frame} pushed: {delta:?}"
            );
        }
        hang_up(client);
        join_after_hang_up(join, "the push loop to end on its subscriber's hang-up")
    };

    let flood = push(std::time::Duration::ZERO);
    assert!(
        flood.admitted >= FRAMES,
        "every pushed frame was a woken pass: {flood:?} over {FRAMES} frames"
    );
    assert_eq!(
        flood.paused,
        flood.admitted - burst,
        "past the burst of {burst}, every woken pass of a flood waits for its interval: \
         {flood:?} over {FRAMES} frames"
    );

    let room = push(PASS_INTERVAL);
    assert!(
        room.admitted >= FRAMES,
        "every pushed frame was a woken pass: {room:?} over {FRAMES} frames"
    );
    assert_eq!(
        room.paused, 0,
        "a wake an interval after the last pass is pushed at once: {room:?} over {FRAMES} frames"
    );
}

/// `subscribe` refusals before the push loop, one row each, captured by a `Vec<u8>`
/// writer: a refused subscribe never enters push mode and registers nothing.
#[test]
fn subscribe_refusals_register_nothing() {
    enum Want {
        Exact(&'static str),
        /// Starts with `ERR ` and names this.
        Names(&'static str),
    }
    /// (label, sessions (local id, seed) with the first active, line, owner?, want)
    type Row<'a> = (&'a str, &'a [(u64, &'a [u8])], &'a str, bool, Want);
    let rows: [Row; 4] = [
        // Fail closed: a scoped `Edge` subscribing to a sibling it has no
        // authorizing edge for (no grant on the sibling's table) gets `ERR denied`.
        (
            "a sibling without an edge",
            &[(0, b""), (2, b"sibling-screen")],
            "subscribe @2 screen",
            false,
            Want::Exact("ERR denied\n"),
        ),
        // Per-session resume anchors (`since=`/`since-turn=`/`since-block=`) name a
        // point in one session's space, so they are rejected against a multi-target
        // fan-out.
        (
            "a resume anchor on a multi-target fan-out",
            &[(0, b""), (2, b"")],
            "subscribe @0,@2 screen since=5",
            true,
            Want::Names("single target"),
        ),
        // The stream list is one token: the handler treats later tokens as trailing
        // args, so `screen cursor` is refused (before target resolution) rather than
        // subscribing to `screen` alone.
        (
            "space-separated streams, targeted",
            &[(0, b"")],
            "subscribe @0 screen cursor",
            true,
            Want::Exact("ERR unknown subscribe arg\n"),
        ),
        (
            "space-separated streams, self",
            &[(0, b"")],
            "subscribe screen cursor",
            true,
            Want::Exact("ERR unknown subscribe arg\n"),
        ),
    ];
    for (label, sessions, line, owner, want) in rows {
        let store = session_store::new_store();
        let handles: Vec<SessionHandle> = sessions
            .iter()
            .map(|&(id, seed)| registered_session(id, -1, seed))
            .collect();
        for h in &handles {
            store.write().unwrap().register(h.clone());
        }
        let active = active_for(&handles[0]);
        let registry = subscribe::new_registry();
        let scope = if owner {
            Scope::Owner
        } else {
            Scope::Edge(EdgeToken::generate())
        };
        let mut out: Vec<u8> = Vec::new();
        run_subscribe(line, &active, &store, &registry, scope, &mut out);
        let s = String::from_utf8(out).unwrap();
        match want {
            Want::Exact(reply) => assert_eq!(s, reply, "{label}"),
            Want::Names(needle) => assert!(
                s.starts_with("ERR ") && s.contains(needle),
                "{label}: {s:?}"
            ),
        }
        assert_eq!(
            registry.lock().unwrap().watched_sessions(),
            0,
            "{label}: a refused subscribe registers nothing"
        );
    }
    // The comma form of the space-separated request IS the grammar — it parses.
    assert!(subscribe::Requested::parse("screen,cursor").is_some());
}

/// The `sessions` stream is instance-wide (it diffs the whole live roster), so a
/// per-target `ReadScreen` grant cannot buy it. An Edge that may `subscribe` to its
/// own session (the positive control) is still refused `sessions`, showing the
/// instance gate denies it. The refusal is `ERR denied`, not a silent stream.
#[test]
fn sessions_stream_is_denied_to_a_scoped_edge_that_may_read_its_target() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let active = active_for(&h);
    let registry = subscribe::new_registry();

    // A ReadScreen edge granted ON this very session: the per-target gate passes.
    let tok = {
        let mut edges = h.ctx.edges.lock().unwrap();
        edges.grant(h.sid.clone(), h.sid.clone(), Op::ReadScreen, h.nonce)
    };
    let scope = Scope::Edge(tok);
    assert!(
        cross_session_authorized(scope, "subscribe", &h.ctx),
        "positive control: this edge DOES authorize subscribe on its target",
    );

    let mut out: Vec<u8> = Vec::new();
    run_subscribe(
        "subscribe @. screen,sessions",
        &active,
        &store,
        &registry,
        scope,
        &mut out,
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "ERR denied\n",
        "the instance-wide sessions stream needs Owner, not a per-target grant",
    );
    assert_eq!(
        registry.lock().unwrap().watched_sessions(),
        0,
        "no registration on denial"
    );
}

/// A self (`@.`) subscribe re-verifies the edge against the session active now: the
/// global ActiveHandle retargets `@.` to the frontmost tab on every switch
/// (`sync_active_session`), so a `ReadScreen` edge granted on session B must not
/// read session A after A comes to the front.
#[test]
fn subscribe_self_edge_denied_after_active_session_swings() {
    let store = session_store::new_store();
    let a = registered_session(0, -1, b"secret-of-A"); // frontmost; what `@.` resolves to
    let b = registered_session(2, -1, b""); // a DIFFERENT session, where the edge was granted
    store.write().unwrap().register(a.clone());
    let active = active_for(&a);
    let registry = subscribe::new_registry();

    // A ReadScreen edge GRANTED on B's table (op matches subscribe) — but no grant
    // against the now-active A.
    let tok = {
        let mut edges = b.ctx.edges.lock().unwrap();
        edges.grant(a.sid.clone(), b.sid.clone(), Op::ReadScreen, b.nonce)
    };
    let scope = Scope::Edge(tok);
    // Positive control: the edge legitimately authorizes subscribe on its OWN B.
    assert!(
        cross_session_authorized(scope, "subscribe", &b.ctx),
        "B's edge authorizes subscribe against its own session",
    );

    // A SELF subscribe while A is frontmost is DENIED — no partial push, no entry.
    let mut out: Vec<u8> = Vec::new();
    run_subscribe(
        "subscribe @. screen",
        &active,
        &store,
        &registry,
        scope,
        &mut out,
    );
    assert_eq!(
        String::from_utf8_lossy(&out),
        "ERR denied\n",
        "B's edge must NOT subscribe to the swung-to active session A",
    );
    assert_eq!(
        registry.lock().unwrap().watched_sessions(),
        0,
        "no registration on denial"
    );
}

/// MULTIPLEX through per-target edges: one scoped `Edge` holding a `ReadScreen`
/// grant on each session's table (`decide_edge` against each target's table and
/// nonce) subscribes to `@0,@2` and receives DELTAs tagged with EACH session's own
/// sid, so the client demultiplexes by the leading token.
#[test]
fn subscribe_multiplex_two_sids_tags_each() {
    let store = session_store::new_store();
    let a = registered_session(0, -1, b"");
    let b = registered_session(2, -1, b"");
    store.write().unwrap().register(a.clone());
    store.write().unwrap().register(b.clone());
    let active = active_for(&a);
    let registry = subscribe::new_registry();
    let token = EdgeToken::generate();
    for h in [&a, &b] {
        assert!(h.ctx.edges.lock().unwrap().insert(
            token,
            SessionId::new("s-reader"),
            h.sid.clone(),
            Op::ReadScreen,
            h.nonce,
        ));
    }

    let (client, server) = CtlStream::pair().unwrap();
    let (store_t, active_t, reg_t) = (store.clone(), active.clone(), registry.clone());
    let join = std::thread::spawn(move || {
        let mut w = server;
        // `@0` is self (active), `@2` a sibling; the edge is granted on both.
        run_subscribe_socket(
            "subscribe @0,@2 screen",
            &active_t,
            &store_t,
            &reg_t,
            Scope::Edge(token),
            &mut w,
        );
    });
    // The ack carries the channel map (`OK subscribe 2` plus one `sub <local> <sid>`
    // line per target) so the client can demux `<local>` frame tags to sids.
    let a_sid = a.sid.as_str().to_string();
    let b_sid = b.sid.as_str().to_string();
    let ack = read_until(&client, String::new(), |s| {
        s.contains(&a_sid) && s.contains(&b_sid)
    });
    assert!(
        ack.contains("OK subscribe 2\n"),
        "two targets acked: {ack:?}"
    );
    assert!(
        ack.contains(&format!("sub 0 {a_sid}\n")),
        "channel 0 maps to its sid: {ack:?}"
    );
    assert!(
        ack.contains(&format!("sub 2 {b_sid}\n")),
        "channel 2 maps to its sid: {ack:?}"
    );

    crate::term_lock(&a.term).process(b"AAA");
    crate::term_lock(&b.term).process(b"BBB");
    registry.lock().unwrap().notify(0);
    registry.lock().unwrap().notify(2);
    // Accumulate until BOTH sids' deltas (with their text) have shown up.
    let seen = read_until(&client, ack, |s| s.contains("AAA") && s.contains("BBB"));
    assert!(
        seen.contains("DELTA 0 ") && seen.contains("AAA"),
        "sid 0 frame: {seen:?}"
    );
    assert!(
        seen.contains("DELTA 2 ") && seen.contains("BBB"),
        "sid 2 frame: {seen:?}"
    );

    hang_up(client);
    join_after_hang_up(
        join,
        "the multiplex push loop to end on its client's hang-up",
    );
}

// ── wf3: --json read mode + edges/family/feed-bin/ready verbs ─────────────

/// A `--json` body is one well-formed JSON value, as the workspace's JSON
/// reader parses it.
fn assert_well_formed_json(s: &str) {
    if let Err(e) = aterm_json::from_str::<aterm_json::Value>(s) {
        panic!("malformed JSON ({e}) in {s:?}");
    }
}

/// `--json` handling, one labelled row per verb / tail.
///
/// * The `--json`/`json` flag is parsed off `rest` and stripped, so the verb's own
///   positional parse runs unchanged (e.g. `blocks 1 --json`), even mid-tail.
/// * An explicit `--json`/`json` on a verb with no structured form (`offscreen`,
///   `modes`, …) is an ERR, never the text reply with the flag dropped; a
///   payload-bearing verb, for which `json` is data, falls through untouched.
#[test]
fn json_flag_is_stripped_and_unsupported_verbs_refuse_it() {
    for (rest, want) in [
        ("", (false, "")),
        ("1", (false, "1")),
        ("--json", (true, "")),
        ("1 --json", (true, "1")),
        ("json 1", (true, "1")),
        // The dispatch sees the flag on an offscreen tail.
        ("since=4 --json screen=1", (true, "since=4 screen=1")),
    ] {
        assert_eq!(
            take_json_flag(rest),
            (want.0, want.1.to_string()),
            "take_json_flag({rest:?})"
        );
    }
    for v in [
        "offscreen",
        "modes",
        "selection",
        "colors",
        "cell",
        "line",
        "history",
        "wait",
    ] {
        assert!(
            json_unsupported(v).is_some(),
            "{v} must refuse an explicit json request"
        );
    }
    // Payload-bearing verbs keep the token as data (`send --json` writes it to the
    // PTY; `search json` searches for it). JSON-capable verbs answer before the
    // fallback, so the allowlist must not shadow them.
    for v in [
        "send", "paste", "feed", "turn", "search", "await", "text", "screen", "cursor", "dims",
        "metrics", "blocks", "edges", "grants",
    ] {
        assert_eq!(json_unsupported(v), None, "{v} must fall through");
    }
}

/// `cursor --json` / `dims --json` / `blocks --json` round-trip the same data as
/// their text forms in a well-formed JSON body; `text --json` carries the cursor.
#[test]
fn cursor_dims_blocks_json_schemas() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    let reg = subscribe::new_registry();
    let host = GuiHost::new(0, &term, None, &reg);
    // cursor: row/col/visible/style.
    let cj = cmd_cursor_json(&term);
    let cbody = cj.strip_prefix("OK 1\n").unwrap().trim_end();
    assert_well_formed_json(cbody);
    assert!(
        cbody.contains("\"row\":0") && cbody.contains("\"style\":\"blinking_block\""),
        "{cbody}"
    );
    let tj = cmd_text_json(&term);
    assert!(tj.contains("\"cursor\":{\"row\":0,\"col\":0,"), "{tj}");

    // dims: the compatible leading grid facts plus the live
    // cell/frame/surface placement record.
    let dims = crate::App::headless_for_test().dims_snapshot(0, 24, 80);
    let dj = serialize_dims_json(&dims);
    let dbody = dj.strip_prefix("OK 1\n").unwrap().trim_end();
    assert_well_formed_json(dbody);
    assert!(
        dbody.contains(&format!(
            "\"rows\":24,\"cols\":80,\"pixel_w\":{},\"pixel_h\":{}",
            dims.pixel_w, dims.pixel_h
        )),
        "{dbody}"
    );
    assert!(
        dbody.contains(&format!(
            "\"cell_w\":{},\"cell_h\":{}",
            dims.cell_w, dims.cell_h
        )) && dbody.contains(&format!(
            "\"frame_w\":{},\"frame_h\":{},\"surface_w\":{},\"surface_h\":{}",
            dims.frame_w, dims.frame_h, dims.surface_w, dims.surface_h
        )),
        "{dbody}"
    );
    let dt = serialize_dims(&dims);
    let leading = format!("OK 24 80 {} {} ", dims.pixel_w, dims.pixel_h);
    assert!(dt.starts_with(&leading), "compatible leading fields: {dt}");
    assert!(
        dt.contains("geometry=headless")
            && dt.contains("band_left=0")
            && dt.contains("crop_right=0")
            && dt.contains("present_retry_state=ready")
            && dt.contains("present_retry_in_ms=none"),
        "{dt}"
    );
    assert!(
        dbody.contains("\"present_retry_state\":\"ready\"")
            && dbody.contains("\"present_retry_count\":0")
            && dbody.contains("\"present_retry_in_ms\":null"),
        "{dbody}"
    );

    // blocks: two OSC-133 blocks -> a `blocks` array; absent rows are JSON null.
    term.lock().unwrap().process(
        b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
    );
    let bj = cmd_blocks_json(&host, 0, "");
    let bbody = bj.strip_prefix("OK 1\n").unwrap().trim_end();
    assert_well_formed_json(bbody);
    assert!(bbody.contains("\"blocks\":[{"), "blocks array: {bbody}");
    assert!(
        bbody.contains("\"exit\":0") && bbody.contains("\"cmdline\":\"echo hi\""),
        "{bbody}"
    );
    assert!(bbody.contains("\"state\":\"complete\""), "{bbody}");
}

/// `edges`/`grants` lists this session's inbound EdgeTable rows as
/// `<src> <dst> <op>`, sorted, never leaking the bearer token; the JSON form has the
/// same triples. An empty table is `OK 0`.
#[test]
fn edges_verb_lists_capability_edges_without_tokens() {
    let ctx = test_ctx();
    assert_eq!(cmd_edges(&ctx), "OK 0\n", "no edges yet");

    // Mint two edges into this session (the data `grant` records).
    let tok = {
        let mut tbl = ctx.edges.lock().unwrap();
        tbl.grant(
            SessionId::new("s-src-a"),
            ctx.self_id.clone(),
            Op::ReadScreen,
            ctx.nonce,
        );
        tbl.grant(
            SessionId::new("s-src-b"),
            ctx.self_id.clone(),
            Op::WriteInput,
            ctx.nonce,
        )
    };
    let out = cmd_edges(&ctx);
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some("OK 2"),
        "header counts both edges: {out}"
    );
    // Sorted by (src, op): s-src-a read-screen, then s-src-b write-input.
    let l1 = lines.next().unwrap();
    let l2 = lines.next().unwrap();
    assert!(
        l1.starts_with("s-src-a ") && l1.ends_with(" read-screen"),
        "edge 1: {l1}"
    );
    assert!(
        l2.starts_with("s-src-b ") && l2.ends_with(" write-input"),
        "edge 2: {l2}"
    );
    // The dst column is always THIS session.
    assert!(l1.contains(ctx.self_id.as_str()), "dst is self: {l1}");
    // The secret token NEVER appears in the listing.
    assert!(
        !out.contains(&tok.to_hex()),
        "edge token must not leak: {out}"
    );

    // JSON form: same triples, well-formed, no token.
    let j = cmd_edges_json(&ctx);
    let body = j.strip_prefix("OK 1\n").unwrap().trim_end();
    assert_well_formed_json(body);
    assert!(
        body.contains("\"src\":\"s-src-a\"") && body.contains("\"op\":\"read-screen\""),
        "{body}"
    );
    assert!(
        !body.contains(&tok.to_hex()),
        "json must not leak the token: {body}"
    );
}

/// `family` emits a `self` line, a `parent` line (`-` for a root) and a `child` line
/// per direct child (sorted by local id). An explicit `<sid>` is Owner-only; the
/// no-arg form is scoped to the resolved session.
#[test]
fn family_verb_emits_parent_and_children() {
    let store = session_store::new_store();
    let root = registered_session(0, -1, b"");
    let root_ctx = root.ctx.clone();
    let mut child_a = registered_session(1, -1, b"");
    child_a.parent = Some(root.sid.clone());
    let mut child_b = registered_session(2, -1, b"");
    child_b.parent = Some(root.sid.clone());
    store.write().unwrap().register(root.clone());
    store.write().unwrap().register(child_a.clone());
    store.write().unwrap().register(child_b.clone());

    // No-arg form from the ROOT's ctx (Owner): self=root, parent=-, two children.
    let out = cmd_family(&root_ctx, &store, Scope::Owner, "");
    let mut lines = out.lines();
    assert_eq!(
        lines.next(),
        Some("OK 4"),
        "counted header (self+parent+2 children): {out}"
    );
    let self_line = lines.next().unwrap();
    assert!(
        self_line.starts_with(&format!("self {} ", root.sid.as_str())),
        "self: {self_line}"
    );
    assert_eq!(
        lines.next(),
        Some("parent - - -"),
        "root has no parent: {out}"
    );
    let kids: Vec<&str> = lines.collect();
    assert_eq!(kids.len(), 2, "two children: {out}");
    assert!(
        kids[0].starts_with(&format!("child {} ", child_a.sid.as_str())),
        "child a: {out}"
    );
    assert!(
        kids[1].starts_with(&format!("child {} ", child_b.sid.as_str())),
        "child b: {out}"
    );

    // Explicit `<sid>` of a child (Owner): self=child, parent=root, no children.
    let cout = cmd_family(&root_ctx, &store, Scope::Owner, child_a.sid.as_str());
    assert!(
        cout.contains(&format!("self {} ", child_a.sid.as_str())),
        "child self: {cout}"
    );
    assert!(
        cout.contains(&format!("parent {} ", root.sid.as_str())),
        "child parent=root: {cout}"
    );
    assert!(!cout.contains("\nchild "), "a leaf has no children: {cout}");

    // An explicit sid is Owner-only, fenced at the dispatch (pinned in
    // `escalated_op_fences_argument_dependent_seams` and
    // `dispatch_authorization_applies_the_escalated_op`), so the handler has no
    // second check.
    let edge = Scope::Edge(EdgeToken::generate());
    // The no-arg form (resolved session) is allowed for an Edge (already gated).
    assert!(
        cmd_family(&root_ctx, &store, edge, "").starts_with("OK "),
        "no-arg edge ok"
    );

    // An unknown sid fails closed.
    assert_eq!(
        cmd_family(&root_ctx, &store, Scope::Owner, "s-nope"),
        "ERR no such session\n"
    );
}

// ---- session connections: the §6 wire verbs ---------------------------------

/// Register two sessions and return `(store, private conn store, src, dst)`; the
/// private record store keeps connection-verb tests off the process-wide singleton.
fn connection_fixture() -> (
    crate::session_store::Store,
    crate::connections::ConnectionStore,
    SessionHandle,
    SessionHandle,
) {
    let store = session_store::new_store();
    let a = registered_session(1, -1, b"");
    let b = registered_session(2, -1, b"");
    store.write().unwrap().register(a.clone());
    store.write().unwrap().register(b.clone());
    (store, crate::connections::new_connection_store(), a, b)
}

/// The token hex bound to `op=` on a `connect` reply line.
fn reply_token(reply: &str, op: &str) -> Option<EdgeToken> {
    reply
        .split_whitespace()
        .find_map(|t| t.strip_prefix(&format!("{op}=")))
        .and_then(EdgeToken::from_hex)
}

/// `connect dst= src=` (default kind=both) mints the three human-fidelity rows into
/// the destination's table, replies every minted `op=<hex>`, and the result shows
/// in the token-free `edges` listing.
#[test]
fn connect_verb_round_trip_is_visible_in_edges() {
    let (store, conn, a, b) = connection_fixture();
    let out = cmd_connect_in(
        &conn,
        &store,
        Scope::Owner,
        &format!("dst={} src={}", b.sid.as_str(), a.sid.as_str()),
    );
    assert!(
        out.starts_with("OK read-screen="),
        "reply leads with the pull op: {out}"
    );
    for op in ["read-screen", "write-input", "signal"] {
        assert!(
            reply_token(&out, op).is_some(),
            "{op}=<hex> must be delivered: {out}"
        );
    }
    // The delivered write token is a REAL edge on the destination.
    let tok = reply_token(&out, "write-input").unwrap();
    assert!(
        decide_edge(
            &b.ctx.edges.lock().unwrap(),
            &tok,
            &b.sid,
            Op::WriteInput,
            &b.ctx.nonce
        )
        .is_permitted(),
        "the delivered token authorizes"
    );
    // Visible in `edges` on the destination — triples only, no hex.
    let edges = cmd_edges(&b.ctx);
    assert!(edges.starts_with("OK 3\n"), "{edges}");
    assert!(
        edges.contains(&format!(
            "{} {} read-screen",
            a.sid.as_str(),
            b.sid.as_str()
        )),
        "{edges}"
    );
    assert!(!edges.contains(&tok.to_hex()), "no token leak: {edges}");
    // A scoped edge cannot connect (Owner-only, also gated by the table).
    assert_eq!(
        cmd_connect_in(&conn, &store, edge(), "dst=s-x src=s-y"),
        "ERR denied\n"
    );
}

/// Declarative set semantics on the wire: a same-kind re-connect replies the same
/// live tokens; a pull→push re-kind leaves exactly the push rows, so no observer
/// reads both kinds at once via `edges`.
#[test]
fn connect_verb_is_declarative_across_kind_transitions() {
    let (store, conn, a, b) = connection_fixture();
    let line = |kind: &str| format!("dst={} src={} kind={kind}", b.sid.as_str(), a.sid.as_str());

    let first = cmd_connect_in(&conn, &store, Scope::Owner, &line("pull"));
    assert!(first.starts_with("OK read-screen="), "{first}");
    assert!(
        !first.contains("write-input="),
        "pull mints no push: {first}"
    );
    // Idempotent re-connect: byte-identical reply — the original token stays.
    let again = cmd_connect_in(&conn, &store, Scope::Owner, &line("pull"));
    assert_eq!(first, again, "same-kind re-connect must not rotate tokens");

    // Re-kind pull→push: ONE atomic transition; `edges` shows exactly push.
    let pull_tok = reply_token(&first, "read-screen").unwrap();
    let pushed = cmd_connect_in(&conn, &store, Scope::Owner, &line("push"));
    assert!(!pushed.contains("read-screen="), "{pushed}");
    assert!(
        pushed.contains("write-input=") && pushed.contains("signal="),
        "{pushed}"
    );
    let edges = cmd_edges(&b.ctx);
    assert!(
        edges.starts_with("OK 2\n"),
        "exactly the push rows: {edges}"
    );
    assert!(
        !edges.contains("read-screen"),
        "pull must not survive the transition: {edges}"
    );
    assert_eq!(
        decide_edge(
            &b.ctx.edges.lock().unwrap(),
            &pull_tok,
            &b.sid,
            Op::ReadScreen,
            &b.ctx.nonce
        ),
        aterm_session::EdgeDecision::Deny,
        "the replaced kind's token dies in the transition"
    );
}

/// The `connect` argument fences: usage, self-loop, unknown/exited dst.
#[test]
fn connect_verb_arguments_fail_closed() {
    let (store, conn, a, b) = connection_fixture();
    for bad in [
        "",
        "dst=s-x",
        "src=s-x",
        "dst=s-x src=s-y kind=bogus",
        "dst= src=s-y",
        "dst=s-x src=s-y extra",
    ] {
        assert!(
            cmd_connect_in(&conn, &store, Scope::Owner, bad).starts_with("ERR usage"),
            "{bad:?} must be a usage error"
        );
    }
    assert_eq!(
        cmd_connect_in(
            &conn,
            &store,
            Scope::Owner,
            &format!("dst={0} src={0}", a.sid.as_str())
        ),
        "ERR self-loop\n"
    );
    assert_eq!(
        cmd_connect_in(&conn, &store, Scope::Owner, "dst=s-nope src=s-any"),
        "ERR no such session\n",
        "dst must be registered"
    );
    // An Exited dst refuses the mint (its nonce already fails closed).
    let mut dead = registered_session(9, -1, b"");
    dead.state = SessionState::Exited;
    let dead_sid = dead.sid.clone();
    store.write().unwrap().register(dead);
    assert_eq!(
        cmd_connect_in(
            &conn,
            &store,
            Scope::Owner,
            &format!("dst={} src={}", dead_sid.as_str(), b.sid.as_str())
        ),
        "ERR exited\n"
    );
    // Nothing above left residue.
    assert!(conn.records().is_empty());
}

/// `disconnect … kind=pull` removes only the read-screen row of a recorded
/// connection; the bare form dissolves the rest; an unknown pair fails closed;
/// unrecorded wire-grant rows dissolve through the op-filtered sweep fallback.
#[test]
fn disconnect_verb_kind_filters_and_sweeps_unrecorded_grants() {
    let (store, conn, a, b) = connection_fixture();
    let pair = format!("dst={} src={}", b.sid.as_str(), a.sid.as_str());
    assert!(
        cmd_connect_in(&conn, &store, Scope::Owner, &pair).starts_with("OK"),
        "fixture connect"
    );

    // kind=pull: exactly the read row goes; push stays live.
    assert_eq!(
        cmd_disconnect_in(&conn, &store, Scope::Owner, &format!("{pair} kind=pull")),
        "OK 1\n"
    );
    let edges = cmd_edges(&b.ctx);
    assert!(
        edges.starts_with("OK 2\n") && !edges.contains("read-screen"),
        "only read-screen removed: {edges}"
    );

    // The bare form dissolves the rest; a repeat fails closed.
    assert_eq!(
        cmd_disconnect_in(&conn, &store, Scope::Owner, &pair),
        "OK 2\n"
    );
    assert_eq!(cmd_edges(&b.ctx), "OK 0\n");
    assert_eq!(
        cmd_disconnect_in(&conn, &store, Scope::Owner, &pair),
        "ERR no such connection\n"
    );

    // Wire-granted rows with NO record: the op-filtered sweep fallback.
    let c = SessionId::new("s-wire-src");
    {
        let mut tbl = b.ctx.edges.lock().unwrap();
        let _ = tbl.grant(c.clone(), b.sid.clone(), Op::ReadScreen, b.ctx.nonce);
        let _ = tbl.grant(c.clone(), b.sid.clone(), Op::WriteInput, b.ctx.nonce);
    }
    let wire_pair = format!("dst={} src={}", b.sid.as_str(), c.as_str());
    assert_eq!(
        cmd_disconnect_in(
            &conn,
            &store,
            Scope::Owner,
            &format!("{wire_pair} kind=push")
        ),
        "OK 1\n",
        "push sweep takes only write-input"
    );
    let edges = cmd_edges(&b.ctx);
    assert!(
        edges.starts_with("OK 1\n") && edges.contains("read-screen"),
        "{edges}"
    );
    assert_eq!(
        cmd_disconnect_in(&conn, &store, Scope::Owner, &wire_pair),
        "OK 1\n"
    );
    // A scoped edge cannot disconnect; unknown dst fails closed.
    assert_eq!(
        cmd_disconnect_in(&conn, &store, edge(), &pair),
        "ERR denied\n"
    );
    assert_eq!(
        cmd_disconnect_in(&conn, &store, Scope::Owner, "dst=s-nope src=s-x"),
        "ERR no such session\n"
    );
}

/// `flows` lists a minted triple across the whole instance (Owner-only), and
/// `flows --json` groups the pair's ops — no token ever appears.
#[test]
fn flows_verb_lists_the_aggregated_graph() {
    let (store, conn, a, b) = connection_fixture();
    assert_eq!(
        cmd_flows(&store, Scope::Owner, ""),
        "OK 0\n",
        "empty fabric"
    );
    let out = cmd_connect_in(
        &conn,
        &store,
        Scope::Owner,
        &format!("dst={} src={} kind=pull", b.sid.as_str(), a.sid.as_str()),
    );
    let tok = reply_token(&out, "read-screen").unwrap();

    assert_eq!(
        cmd_flows(&store, Scope::Owner, ""),
        format!("OK 1\n{} {} read-screen\n", a.sid.as_str(), b.sid.as_str())
    );
    let json = cmd_flows(&store, Scope::Owner, "--json");
    let body = json.strip_prefix("OK 1\n").expect("json_ok framing");
    assert!(body.contains("\"flows\":[{"), "{body}");
    assert!(
        body.contains(&format!("\"src\":\"{}\"", a.sid.as_str()))
            && body.contains(&format!("\"dst\":\"{}\"", b.sid.as_str()))
            && body.contains("\"ops\":[\"read-screen\"]"),
        "{body}"
    );
    assert!(!json.contains(&tok.to_hex()), "no token leak: {json}");

    // Owner-only + strict flags.
    assert_eq!(cmd_flows(&store, edge(), ""), "ERR denied\n");
    assert!(cmd_flows(&store, Scope::Owner, "bogus").starts_with("ERR usage"));
}

/// For Owner only, `family` adds discovery rows: the source reads `pushes`, the
/// destination `pushed-by`, a pull pair `pulls`/`pulled-by`, and an unregistered src
/// `unknown -`. An edge-scoped caller gets none of them (count intact).
#[test]
fn family_connection_rows_are_owner_only() {
    let (store, conn, a, b) = connection_fixture();
    assert!(
        cmd_connect_in(
            &conn,
            &store,
            Scope::Owner,
            &format!("dst={} src={} kind=push", b.sid.as_str(), a.sid.as_str()),
        )
        .starts_with("OK"),
        "fixture connect"
    );
    // A foreign wire-granted src pulling A: rows in A's table, src unknown.
    let x = SessionId::new("s-foreign");
    {
        let mut tbl = a.ctx.edges.lock().unwrap();
        let _ = tbl.grant(x.clone(), a.sid.clone(), Op::ReadScreen, a.ctx.nonce);
    }

    // Owner @ A: self+parent + `pushes B` + `pulled-by X` = 4 rows.
    let fa = cmd_family(&a.ctx, &store, Scope::Owner, "");
    assert!(fa.starts_with("OK 4\n"), "{fa}");
    assert!(
        fa.contains(&format!("pushes {} alive ", b.sid.as_str())),
        "{fa}"
    );
    assert!(
        fa.contains(&format!("pulled-by {} unknown -", x.as_str())),
        "foreign src prints honestly: {fa}"
    );
    // Owner @ B: the inbound face of the same connection.
    let fb = cmd_family(&b.ctx, &store, Scope::Owner, "");
    assert!(
        fb.contains(&format!("pushed-by {} alive ", a.sid.as_str())),
        "{fb}"
    );
    assert!(!fb.contains("\npushes "), "B pushes nothing: {fb}");

    // An edge-scoped caller gets NO connection rows — and the count agrees.
    let fe = cmd_family(&a.ctx, &store, edge(), "");
    assert!(fe.starts_with("OK 2\n"), "self+parent only: {fe}");
    for kind in ["pushes ", "pushed-by ", "pulls ", "pulled-by "] {
        assert!(!fe.contains(kind), "edge scope must not see {kind}: {fe}");
    }
}

/// The pure half of `raise <sid>`: one sid token resolves through the registry to
/// the local id the main-thread hop raises; unknown sids and malformed argument
/// lists fail closed without touching the proxy.
#[test]
fn raise_target_resolves_registered_sids() {
    let store = session_store::new_store();
    let h = registered_session(5, -1, b"");
    store.write().unwrap().register(h.clone());
    assert_eq!(raise_target(&store, h.sid.as_str()), Ok(5));
    assert_eq!(
        raise_target(&store, &format!("  {}  ", h.sid.as_str())),
        Ok(5),
        "whitespace-tolerant"
    );
    for bad in ["s-nope", "", "s-a s-b"] {
        assert!(raise_target(&store, bad).is_err(), "{bad:?}");
    }
}

/// `feed-bin` framing: the request-line parse extracts the optional selector and
/// declared length, rejects malformed or oversize lines, and recognizes the verb with
/// or without an `@<sel>` prefix.
#[test]
fn feed_bin_framing_parse() {
    // Bare form.
    assert!(matches!(
        parse_feed_bin("feed-bin 4", "feed-bin"),
        FeedBinFrame::Ok(None, 4, None)
    ));
    assert!(binary_frame_verb("feed-bin 4").is_some());
    // Cross-session form.
    assert!(matches!(
        parse_feed_bin("@7 feed-bin 10", "feed-bin"),
        FeedBinFrame::Ok(Some(Selector::Local(7)), 10, None)
    ));
    assert!(binary_frame_verb("@7 feed-bin 10").is_some());
    // Not feed-bin.
    assert!(binary_frame_verb("feed 0a").is_none());
    assert!(binary_frame_verb("@7 feed 0a").is_none());
    // Malformed (missing / non-numeric length) -> Malformed: no payload was
    // announced, so the caller keeps the connection.
    assert!(matches!(
        parse_feed_bin("feed-bin", "feed-bin"),
        FeedBinFrame::Malformed
    ));
    assert!(matches!(
        parse_feed_bin("feed-bin xx", "feed-bin"),
        FeedBinFrame::Malformed
    ));
    // A valid but oversize length is TooLarge, not Malformed: the client pipelined
    // N bytes we refuse to read, so the caller must close rather than desync.
    assert!(matches!(
        parse_feed_bin(&format!("feed-bin {}", MAX_FEED_BIN + 1), "feed-bin"),
        FeedBinFrame::TooLarge
    ));
    // Exactly the cap is allowed.
    assert!(
        matches!(parse_feed_bin(&format!("feed-bin {MAX_FEED_BIN}"), "feed-bin"), FeedBinFrame::Ok(None, n, None) if n == MAX_FEED_BIN)
    );
    // Trailing tokens after the length are Malformed: a canonical frame is exactly
    // `[@sel] feed-bin <n>`, and reading `feed-bin 1 junk` as a 1-byte frame would
    // consume a byte of the next pipelined line. Trailing whitespace is fine.
    assert!(matches!(
        parse_feed_bin("feed-bin 1 junk", "feed-bin"),
        FeedBinFrame::Malformed
    ));
    assert!(matches!(
        parse_feed_bin("@7 feed-bin 10 extra", "feed-bin"),
        FeedBinFrame::Malformed
    ));
    assert!(matches!(
        parse_feed_bin("feed-bin 4   ", "feed-bin"),
        FeedBinFrame::Ok(None, 4, None)
    ));
}

/// A bounded binary header is the authoritative input boundary: the licence fence
/// completes before the server can block on a slow payload, and rejected or
/// unauthorized headers never mutate cursor state.
#[test]
#[cfg(unix)]
fn binary_input_header_fences_before_payload_and_only_after_authorization() {
    struct FenceCheckedReader {
        inner: std::io::Cursor<Vec<u8>>,
        fenced: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl std::io::Read for FenceCheckedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            assert!(
                self.fenced.get(),
                "payload read began before the authorized target fence"
            );
            self.inner.read(buf)
        }
    }

    impl std::io::BufRead for FenceCheckedReader {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            self.inner.fill_buf()
        }

        fn consume(&mut self, amount: usize) {
            self.inner.consume(amount);
        }
    }

    let store = session_store::new_store();
    let (front, front_rx) = pipe_session(1);
    store.write().unwrap().register(front.clone());
    let active = active_for_handle(&front);
    let fenced = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut reader = FenceCheckedReader {
        inner: std::io::Cursor::new(b"ABCafter\n".to_vec()),
        fenced: fenced.clone(),
    };
    let mut dispatch = |_event, _session| Ok(InputOutcome::Ok);
    let mut cancel = {
        let fenced = fenced.clone();
        move |session| {
            assert_eq!(session, 1);
            fenced.set(true);
            "OK\n".to_string()
        }
    };
    let mut out = Vec::new();
    assert!(run_feed_bin_routed(
        "@1 feed-bin 3",
        "feed-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut out,
    ));
    assert!(fenced.get());
    assert_eq!(String::from_utf8_lossy(&out), "OK 3 bytes\n");
    assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));
    assert!(drain_pipe(&front_rx).is_empty());

    // A rejected frame still consumes its bounded payload, but an unauthorized
    // header cannot use the licence fence as a UI side channel.
    let cancel_count = std::cell::Cell::new(0usize);
    let mut cancel = |_session| {
        cancel_count.set(cancel_count.get() + 1);
        "OK\n".to_string()
    };
    let mut reader = std::io::Cursor::new(b"XYZafter\n".to_vec());
    let mut denied = Vec::new();
    assert!(run_feed_bin_routed(
        "@1 feed-bin 3",
        "feed-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Edge(EdgeToken::generate()),
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut denied,
    ));
    assert_eq!(cancel_count.get(), 0, "denied header must not touch UI");
    assert_eq!(String::from_utf8_lossy(&denied), "ERR denied\n");
    assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));

    // A targetable authorized attempt supersedes even with a malformed or
    // over-cap length, like ordinary verbs' pre-argument-parse boundary; an
    // unknown target stays side-effect free.
    let mut empty = std::io::Cursor::new(Vec::<u8>::new());
    let mut malformed = Vec::new();
    assert!(run_feed_bin_routed(
        "@1 feed-bin nope",
        "feed-bin",
        &mut empty,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut malformed,
    ));
    assert_eq!(cancel_count.get(), 1);
    let mut oversize = Vec::new();
    assert!(!run_feed_bin_routed(
        &format!("@1 feed-bin {}", MAX_FEED_BIN + 1),
        "feed-bin",
        &mut empty,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut oversize,
    ));
    assert_eq!(cancel_count.get(), 2);
    let mut missing = Vec::new();
    assert!(run_feed_bin_routed(
        "@999999 feed-bin nope",
        "feed-bin",
        &mut empty,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut missing,
    ));
    assert_eq!(
        cancel_count.get(),
        2,
        "unknown target cannot fence a window"
    );
}

/// The header fence is an early liveness boundary, not an authority cache: target
/// and capability scope are sampled again after the payload, so a stalled `@.`
/// frame follows the new active session and a revoked edge can no longer write. A
/// failed early fence closes before touching payload bytes.
#[test]
#[cfg(unix)]
fn binary_input_revalidates_target_and_authority_after_payload() {
    struct OnFirstRead<F> {
        inner: std::io::Cursor<Vec<u8>>,
        action: Option<F>,
    }

    impl<F: FnOnce()> std::io::Read for OnFirstRead<F> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if let Some(action) = self.action.take() {
                action();
            }
            self.inner.read(buf)
        }
    }

    impl<F: FnOnce()> std::io::BufRead for OnFirstRead<F> {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            self.inner.fill_buf()
        }

        fn consume(&mut self, amount: usize) {
            self.inner.consume(amount);
        }
    }

    let store = session_store::new_store();
    let (first, first_rx) = pipe_session(1);
    let (second, second_rx) = pipe_session(2);
    store.write().unwrap().register(first.clone());
    store.write().unwrap().register(second.clone());
    let active = active_for_handle(&first);

    let switched_active = active.clone();
    let switched_to = second.clone();
    let mut reader = OnFirstRead {
        inner: std::io::Cursor::new(b"X".to_vec()),
        action: Some(move || {
            *switched_active.lock().unwrap_or_else(|p| p.into_inner()) = Some(ActiveSession {
                term: switched_to.term.clone(),
                master: switched_to.master,
                id: switched_to.local_id,
                ctx: switched_to.ctx.clone(),
            });
        }),
    };
    let canceled = std::cell::RefCell::new(Vec::new());
    let mut cancel = |session| {
        canceled.borrow_mut().push(session);
        "OK\n".to_string()
    };
    let routed = std::cell::RefCell::new(None);
    let mut dispatch = |event, session| {
        *routed.borrow_mut() = Some((event, session));
        Ok(InputOutcome::Ok)
    };
    let mut out = Vec::new();
    assert!(run_feed_bin_routed(
        "@. feed-bin 1",
        "feed-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut out,
    ));
    assert_eq!(&*canceled.borrow(), &[1, 2]);
    assert!(matches!(
        &*routed.borrow(),
        Some((InputEvent::KeySequence(bytes), Some(2))) if bytes == b"X"
    ));
    assert_eq!(String::from_utf8_lossy(&out), "OK 1 bytes\n");
    assert!(drain_pipe(&first_rx).is_empty());
    assert!(drain_pipe(&second_rx).is_empty());

    let (guarded, guarded_rx) = pipe_session(3);
    store.write().unwrap().register(guarded.clone());
    let scope = edge_granted(Op::WriteInput, &guarded.ctx);
    let Scope::Edge(token) = scope else {
        unreachable!("edge_granted always returns an edge scope");
    };
    let revoke_ctx = guarded.ctx.clone();
    let mut reader = OnFirstRead {
        inner: std::io::Cursor::new(b"Y".to_vec()),
        action: Some(move || {
            assert!(
                revoke_ctx
                    .edges
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .revoke(&token),
                "the header-authorized edge was live before payload receipt",
            );
        }),
    };
    let mut cancel_count = 0usize;
    let mut cancel = |_session| {
        cancel_count += 1;
        "OK\n".to_string()
    };
    let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
        panic!("revoked payload reached the App input seam")
    };
    let mut denied = Vec::new();
    assert!(run_feed_bin_routed(
        "@3 feed-bin 1",
        "feed-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut denied,
    ));
    assert_eq!(
        cancel_count, 1,
        "only the header fence ran before revocation"
    );
    assert_eq!(String::from_utf8_lossy(&denied), "ERR denied\n");
    assert!(drain_pipe(&guarded_rx).is_empty());

    // The inverse: a header not yet authorized performs no early UI mutation, but a
    // grant made while its payload arrives is rechecked and the actual target is
    // fenced before direct egress.
    let (granted_late, granted_late_rx) = pipe_session(4);
    store.write().unwrap().register(granted_late.clone());
    let token = EdgeToken::generate();
    let late_scope = Scope::Edge(token);
    let grant_ctx = granted_late.ctx.clone();
    let mut reader = OnFirstRead {
        inner: std::io::Cursor::new(b"W".to_vec()),
        action: Some(move || {
            let dst = grant_ctx.self_id.clone();
            let nonce = grant_ctx.nonce;
            assert!(
                grant_ctx
                    .edges
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(
                        token,
                        SessionId::new("s-late-controller"),
                        dst,
                        Op::WriteInput,
                        nonce,
                    )
            );
        }),
    };
    let canceled_late = std::cell::RefCell::new(Vec::new());
    let mut cancel = |session| {
        canceled_late.borrow_mut().push(session);
        "OK\n".to_string()
    };
    let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
        panic!("background explicit feed unexpectedly entered App input")
    };
    let mut accepted = Vec::new();
    assert!(run_feed_bin_routed(
        "@4 feed-bin 1",
        "feed-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: late_scope,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut accepted,
    ));
    assert_eq!(&*canceled_late.borrow(), &[4]);
    assert_eq!(String::from_utf8_lossy(&accepted), "OK 1 bytes\n");
    assert_eq!(drain_pipe(&granted_late_rx), b"W");

    let mut reader = OnFirstRead {
        inner: std::io::Cursor::new(b"Z".to_vec()),
        action: Some(|| panic!("payload read despite a failed header fence")),
    };
    let mut cancel = |_session| "ERR licence fence unavailable\n".to_string();
    let mut dispatch = |_event, _session| Ok(InputOutcome::Ok);
    let mut failed = Vec::new();
    assert!(!run_feed_bin_routed(
        "@2 feed-bin 1",
        "feed-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut cancel,
        &mut failed,
    ));
    assert_eq!(
        String::from_utf8_lossy(&failed),
        "ERR licence fence unavailable\n"
    );
}

/// `feed-bin <n>\n<bytes>` / `paste-bin <n>\n<bytes>` end to end, one row per
/// framing or transform case, mirroring `serve`: a `BufReader` over the
/// control input stream, the request line first, then the binary frame run as the
/// Owner. (A denied edge's frame is consumed too:
/// `feed_bin_self_edge_denied_after_active_session_swings`.)
#[test]
#[cfg(unix)]
fn binary_frames_land_exact_bytes_and_keep_the_stream_framed() {
    enum Reply {
        Exact(&'static str),
        Prefix(&'static str),
    }
    enum Frame {
        Any,
        Is(&'static str),
        Unchecked,
    }
    struct Row {
        label: &'static str,
        input: Vec<u8>,
        verb: &'static str,
        line: Option<&'static str>,
        frame: Frame,
        keep: bool,
        reply: Reply,
        /// The bytes that reached the PTY (`None`: not asserted).
        pty: Option<&'static [u8]>,
        /// The next request line (`None`: the connection closes, never read).
        next: Option<&'static str>,
    }
    let rows = [
        // An Owner's length-prefixed payload lands the exact raw bytes (including
        // NUL, which a line-delimited `send` cannot carry) on the target PTY,
        // replies `OK <n> bytes`, and leaves the stream framed for the next request.
        Row {
            label: "feed-bin is raw and framed",
            input: b"feed-bin 3\n\x00\x01\x02next-line\n".to_vec(),
            verb: "feed-bin",
            line: Some("feed-bin 3"),
            frame: Frame::Any,
            keep: true,
            reply: Reply::Exact("OK 3 bytes\n"),
            pty: Some(b"\x00\x01\x02"),
            next: Some("next-line"),
        },
        // `paste-bin` applies paste semantics (the seam's `format_paste`: bare LF
        // becomes CR, no bracketed paste in a fresh engine), not raw bytes.
        Row {
            label: "paste-bin maps LF to CR",
            input: b"paste-bin 5\nab\ncdafter\n".to_vec(),
            verb: "paste-bin",
            line: Some("paste-bin 5"),
            frame: Frame::Is("paste-bin"),
            keep: true,
            reply: Reply::Exact("OK 5 bytes\n"),
            pty: Some(b"ab\rcd"),
            next: Some("after"),
        },
        // A declared length over MAX_FEED_BIN closes the connection: the client
        // has already pipelined N bytes we refuse to read, and keeping the
        // connection would let them (here `subscribe all`) dispatch as verbs.
        Row {
            label: "an over-cap length closes the connection",
            input: format!("feed-bin {}\nsubscribe all\n", MAX_FEED_BIN + 1).into_bytes(),
            verb: "feed-bin",
            line: None,
            frame: Frame::Any,
            keep: false,
            reply: Reply::Prefix("ERR feed-bin too large"),
            pty: Some(b""),
            next: None,
        },
        // A malformed length replies `ERR usage` WITHOUT consuming any payload:
        // the next line is whatever followed the bad request line verbatim.
        Row {
            label: "a bad length consumes no payload",
            input: b"feed-bin notanumber\nfollowing\n".to_vec(),
            verb: "feed-bin",
            line: None,
            frame: Frame::Unchecked,
            keep: true,
            reply: Reply::Prefix("ERR usage"),
            pty: None,
            next: Some("following"),
        },
    ];
    for row in rows {
        let store = session_store::new_store();
        let (h_self, self_rx) = pipe_session(1);
        store.write().unwrap().register(h_self.clone());
        let active = active_for_handle(&h_self);
        let mut reader = BufReader::new(std::io::Cursor::new(row.input));
        let mut out: Vec<u8> = Vec::new();
        let line = read_request_line(&mut reader).expect("the request line");
        if let Some(want) = row.line {
            assert_eq!(line, want, "{}", row.label);
        }
        match row.frame {
            Frame::Any => assert!(binary_frame_verb(&line).is_some(), "{}", row.label),
            Frame::Is(verb) => {
                assert_eq!(binary_frame_verb(&line), Some(verb), "{}", row.label)
            }
            Frame::Unchecked => {}
        }
        let keep = run_feed_bin(
            &line,
            row.verb,
            &mut reader,
            &active,
            &store,
            Scope::Owner,
            &mut out,
        );
        assert_eq!(keep, row.keep, "{}: connection kept?", row.label);
        let reply = String::from_utf8_lossy(&out);
        match row.reply {
            Reply::Exact(want) => assert_eq!(reply, want, "{}", row.label),
            Reply::Prefix(want) => assert!(reply.starts_with(want), "{}: {reply:?}", row.label),
        }
        if let Some(want) = row.pty {
            assert_eq!(drain_pipe(&self_rx), want, "{}: PTY bytes", row.label);
        }
        if let Some(want) = row.next {
            let next = read_request_line(&mut reader).expect("the following line survives");
            assert_eq!(next, want, "{}: the stream stays framed", row.label);
        }
    }
}

#[test]
#[cfg(unix)]
fn explicit_front_paste_bin_routes_to_the_targeted_app_input_seam() {
    let store = session_store::new_store();
    let (front, front_rx) = pipe_session(1);
    store.write().unwrap().register(front.clone());
    let active = active_for_handle(&front);
    let input = b"@1 paste-bin 7\n\xe4\xb8\xad\xf0\x9f\x99\x82after\n";
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    let line = read_request_line(&mut reader).expect("paste-bin request line");
    let mut reply = Vec::new();
    let mut routed = None;
    let mut dispatch = |event, session| {
        routed = Some((event, session));
        Ok(InputOutcome::Ok)
    };
    let mut clear_license = |_session| "OK\n".to_string();
    assert!(run_feed_bin_routed(
        &line,
        "paste-bin",
        &mut reader,
        FeedBinRoute {
            active: &active,
            store: &store,
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut clear_license,
        &mut reply,
    ));
    assert_eq!(String::from_utf8_lossy(&reply), "OK 7 bytes\n");
    let Some((InputEvent::Paste(text, _), target)) = routed else {
        panic!("visible explicit paste-bin bypassed the App input seam");
    };
    assert_eq!(text, "中🙂");
    assert_eq!(target, Some(1), "the exact named session rides the wake");
    assert!(
        drain_pipe(&front_rx).is_empty(),
        "front paste-bin is not double-written through background egress"
    );
    assert_eq!(
        read_request_line(&mut reader).as_deref(),
        Some("after"),
        "the following request remains framed"
    );

    // No terminal at all (a native app in front): a flagless `paste-bin` (the
    // `aterm-ctl paste --stdin` route) still reaches the App input seam, framed.
    let no_terminal: ActiveHandle = Arc::new(Mutex::new(None));
    let mut reader = BufReader::new(std::io::Cursor::new(b"paste-bin 6\nBinaryafter\n"));
    let line = read_request_line(&mut reader).expect("paste-bin request line");
    let mut reply = Vec::new();
    let mut routed = None;
    let mut dispatch = |event, _session| {
        routed = Some(event);
        Ok(InputOutcome::Ok)
    };
    assert!(run_feed_bin_routed(
        &line,
        "paste-bin",
        &mut reader,
        FeedBinRoute {
            active: &no_terminal,
            store: &session_store::new_store(),
            scope: Scope::Owner,
            subscribers: None,
        },
        &mut dispatch,
        &mut clear_license,
        &mut reply,
    ));
    assert_eq!(String::from_utf8_lossy(&reply), "OK 6 bytes\n");
    assert!(
        matches!(&routed, Some(InputEvent::Paste(text, _)) if text == "Binary"),
        "{routed:?}"
    );
    assert_eq!(read_request_line(&mut reader).as_deref(), Some("after"));
}

/// A hidden `paste-bin` / `feed-bin` into a session whose peer is gone answers
/// `ERR write failed`, not `OK <n> bytes`: the hidden arm reports its own write
/// receipt (a headless driver has no front window). The stream stays framed past
/// the consumed payload, nothing enters App input, and the front session is
/// untouched.
#[test]
#[cfg(unix)]
fn background_binary_frame_reports_a_dead_peer_as_write_failed() {
    let store = session_store::new_store();
    let (front, front_rx) = pipe_session(1);
    let (background, background_rx) = pipe_session(2);
    store.write().unwrap().register(front.clone());
    store.write().unwrap().register(background.clone());
    let active = active_for_handle(&front);
    // The background peer must stay gone: `pipe_session`'s pipe is CLOEXEC only
    // after a window, so a child a concurrent test spawns can inherit the read end
    // and keep the peer alive. Kill the peer in the kernel instead
    // (`kill_peer_in_kernel`), with a probe proving it dead before any frame.
    drop(background_rx);
    kill_peer_in_kernel(background.master, "the background session's peer");

    for (verb, frame) in [
        ("paste-bin", &b"@2 paste-bin 3\nabcafter\n"[..]),
        ("feed-bin", &b"@2 feed-bin 3\nxyzafter\n"[..]),
    ] {
        let mut reader = BufReader::new(std::io::Cursor::new(frame));
        let line = read_request_line(&mut reader).expect("request line");
        let mut reply = Vec::new();
        let mut dispatch = |_event, _session| -> Result<InputOutcome, String> {
            panic!("background {verb} unexpectedly entered App input")
        };
        let mut clear_license = |_session| "OK\n".to_string();
        assert!(run_feed_bin_routed(
            &line,
            verb,
            &mut reader,
            FeedBinRoute {
                active: &active,
                store: &store,
                scope: Scope::Owner,
                subscribers: None,
            },
            &mut dispatch,
            &mut clear_license,
            &mut reply,
        ));
        assert_eq!(
            String::from_utf8_lossy(&reply),
            "ERR write failed\n",
            "{verb} into a dead peer must not answer a false OK"
        );
        assert!(
            drain_pipe(&front_rx).is_empty(),
            "{verb} must not touch the front session"
        );
        assert_eq!(
            read_request_line(&mut reader).as_deref(),
            Some("after"),
            "{verb}: the following request remains framed"
        );
    }
}

/// Cross-session `send`/`feed` (the raw helper) and `key`/`ctrl` (the vocabulary
/// helper) into a session whose peer is gone report the failed write, as
/// `cross_mouse_apply` does.
#[test]
#[cfg(unix)]
fn cross_session_input_reports_a_dead_peer_as_write_failed() {
    let (h, rx) = pipe_session(74);
    // `drop` closes only this process's reader; a child that inherited a duplicate
    // through cloexec_pipe's window keeps the peer alive. Kill the peer where no
    // descriptor can revive it (see `kill_peer_in_kernel`).
    drop(rx);
    kill_peer_in_kernel(h.master, "a cross-session send into a dead peer");
    assert_eq!(
        cross_raw_input(&h.term, &h.ctx, send_bytes("ls")),
        Ok(InputOutcome::WriteFailed),
        "raw send into a dead peer is a failed write"
    );
    assert_eq!(
        cross_input(&h.term, &h.ctx, parse_key("enter"), "ERR\n"),
        "ERR write failed\n",
        "a key into a dead peer is a failed write, not OK"
    );
    // A malformed line is still the usage error, decided before any write.
    assert_eq!(
        cross_input(
            &h.term,
            &h.ctx,
            parse_key("notakey"),
            control_input::KEY_USAGE
        ),
        control_input::KEY_USAGE
    );
}

/// What one run of [`paste_while_the_peer_closes_mid_write`] observed.
#[cfg(unix)]
struct MidWriteClose {
    reply: String,
    /// The probe after the reply: one more byte into the pipe earned `EPIPE`,
    /// so no reader existed anywhere — not this test's, not a stranger's.
    peer_observed_dead: bool,
    reached_kernel: bool,
    spill_empty: bool,
}

/// Build a wedged pipe with an older interactive frame in its spill, close the peer
/// from a thread while a paste waits behind it, and report the paste's answer and
/// whether the peer was dead.
///
/// [`cloexec_pipe`] cannot close Darwin's inherit window, so a concurrent test's
/// child may hold a duplicate of the read end. Holding only delays the verdict
/// (until the stranger exits, then `EPIPE`); a stranger that reads lets the spill
/// reach the kernel, making `OK` correct for that run. The one-byte probe after the
/// reply says which case occurred, so verdicts are asserted only over a peer
/// observed dead.
#[cfg(unix)]
fn paste_while_the_peer_closes_mid_write() -> MidWriteClose {
    let (handle, reader) = pipe_session(73);
    aterm_pty::set_nonblocking(handle.master, true).expect("nonblocking test master");
    handle.ctx.sink.note_master_nonblocking(true);

    let mut filled = 0_usize;
    loop {
        match aterm_pty::write_some(handle.master, &[b'.'; 4096]) {
            Ok(n) if n > 0 => filled += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected pipe fill result: {other:?}"),
        }
    }
    assert!(filled > 0, "pipe reached real backpressure");
    assert_eq!(
        handle
            .ctx
            .sink
            .write_frame_nonparking(b"older")
            .expect("older frame enters spill"),
        5,
    );
    assert!(
        !handle.ctx.sink.egress_drained_to_kernel(),
        "negative control: bytes really remain process-local before paste",
    );

    // MID-WRITE IS A FACT, not 25 ms of hoping: the peer closes once the
    // paste is queued behind the older frame — the spill holds more than
    // that frame. A panic here drops `reader` as it unwinds, so the paste
    // below is released either way.
    let older = handle.ctx.sink.spilled_bytes();
    let sink = Arc::clone(&handle.ctx.sink);
    let closer = std::thread::spawn(move || {
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the paste is in flight behind the older frame",
            || (sink.spilled_bytes() > older).then_some(()),
        );
        drop(reader);
    });
    let reply = cross_input(
        &handle.term,
        &handle.ctx,
        Some(InputEvent::Paste("newer".into(), PasteFraming::AtDrain)),
        "ERR malformed\n",
    );
    closer.join().expect("peer closer");
    // SAFETY: a one-byte write from a static buffer to this test's own live
    // (non-blocking) write end.
    let probe = unsafe { libc::write(handle.master, b"x".as_ptr().cast(), 1) };
    let peer_observed_dead =
        (probe, std::io::Error::last_os_error().raw_os_error()) == (-1, Some(libc::EPIPE));
    MidWriteClose {
        reply,
        peer_observed_dead,
        reached_kernel: handle.ctx.sink.wait_egress_drained_to_kernel(),
        spill_empty: handle.ctx.sink.egress_drained_to_kernel(),
    }
}

/// A cross-session paste reply is a kernel-delivery receipt, not just acceptance
/// into the process-local spill: closing the peer while the paste waits behind an
/// older spilled frame, `cross_input` keeps the sink's sticky failure and returns
/// an error once the queued bytes are discarded.
///
/// Asserted only over runs whose peer the probe saw dead; a run a stranger voided
/// (see [`paste_while_the_peer_closes_mid_write`]) is rebuilt, and the test fails
/// only if every attempt is void.
#[test]
#[cfg(unix)]
fn cross_session_paste_reports_a_dead_spill_peer_as_write_failed() {
    const ATTEMPTS: usize = 3;
    for attempt in 1..=ATTEMPTS {
        let run = paste_while_the_peer_closes_mid_write();
        if !run.peer_observed_dead {
            eprintln!(
                "attempt {attempt}: a stranger still held the read end (reply {:?}); \
                 the run is void, not a verdict",
                run.reply
            );
            continue;
        }
        assert_eq!(run.reply, "ERR write failed\n");
        assert!(
            !run.reached_kernel,
            "empty-after-peer-death remains a sticky completion failure",
        );
        assert!(
            run.spill_empty,
            "nothing process-local remains: the handoff's polling fence is settled",
        );
        return;
    }
    panic!("every one of {ATTEMPTS} runs had a stranger on the read end");
}

/// Run `write` on its own thread and return its answer, failing (instead of
/// hanging the suite) if it has not answered within the hang detector: the
/// verdict a control lane needs, since a write that parks parks the lane. A
/// parked writer is left behind on failure; the caller's later peer close
/// releases it.
#[cfg(unix)]
fn answered_without_parking<T: Send + 'static>(
    what: &str,
    write: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(write());
    });
    rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .unwrap_or_else(|_| {
            panic!("{what}: the write parked its caller on a program that reads nothing")
        })
}

/// Control input into a background session whose program reads nothing never
/// parks the lane that serves it (2026-10-14). These arms wrote through the
/// blocking egress, which waits for the program to read with no bound: eight
/// `@<sid> paste`s into one stuck tab took every request lane, and every other
/// client, `@<sid> signal` (the remedy) included, was answered busy for as long
/// as the program did not read.
///
/// With the pipe full: `send` and `key` are queued behind it, accepted for
/// delivery, and answered at once; a paste waits for its bytes to reach the
/// program only up to its bound, then answers that they are queued unread; and
/// at the session's input cap a write is refused, nothing written, with a reply
/// that starts `ERR busy` (nothing to retry twice). Each fails, at the hang
/// detector, against the blocking egress.
#[test]
#[cfg(unix)]
fn background_input_into_a_program_that_reads_nothing_never_parks_the_lane() {
    let (handle, reader) = pipe_session(76);
    aterm_pty::set_nonblocking(handle.master, true).expect("nonblocking test master");
    handle.ctx.sink.note_master_nonblocking(true);
    loop {
        match aterm_pty::write_some(handle.master, &[b'.'; 4096]) {
            Ok(n) if n > 0 => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected pipe fill result: {other:?}"),
        }
    }
    let term = Arc::clone(&handle.term);
    let ctx = Arc::clone(&handle.ctx);

    let (t, c) = (Arc::clone(&term), Arc::clone(&ctx));
    let sent = answered_without_parking("send", move || cross_raw_input(&t, &c, send_bytes("ls")));
    assert_eq!(sent, Ok(InputOutcome::Ok), "accepted for delivery");
    assert!(
        ctx.sink.spilled_bytes() >= 2,
        "the bytes wait in the session's spill, for the program to read"
    );
    let (t, c) = (Arc::clone(&term), Arc::clone(&ctx));
    let key = answered_without_parking("key", move || {
        cross_input(&t, &c, parse_key("enter"), "ERR\n")
    });
    assert_eq!(key, "OK\n");

    let (t, c) = (Arc::clone(&term), Arc::clone(&ctx));
    let paste = answered_without_parking("paste", move || {
        background_input(
            &t,
            &c,
            &InputEvent::Paste("pasted".into(), PasteFraming::AtDrain),
            std::time::Duration::ZERO,
        )
    });
    assert_eq!(
        paste,
        Err(PASTE_UNREAD.to_string()),
        "a paste the program has not read is in doubt, never OK or busy"
    );

    // A paste whose caller hangs up stops waiting then, whatever its bound.
    let (t, c) = (Arc::clone(&term), Arc::clone(&ctx));
    let hung_up = answered_without_parking("paste whose caller hung up", move || {
        let (client, server) = CtlStream::pair().expect("socket pair");
        let _serving = ServingFd::enter(&server);
        drop(client);
        background_input(
            &t,
            &c,
            &InputEvent::Paste("pasted".into(), PasteFraming::AtDrain),
            std::time::Duration::from_secs(1),
        )
    });
    assert_eq!(hung_up, Err(HUNG_UP_REPLY.to_string()));

    // One frame takes the spill past its cap; the next write is refused whole.
    let cap = vec![b'x'; aterm_session::sink::SinkWriter::SPILL_CAP];
    let accepted = ctx
        .sink
        .write_frame_interactive_with_receipt(&cap)
        .expect("the frame that reaches the cap is accepted")
        .accepted();
    assert_eq!(accepted, cap.len());
    let spilled = ctx.sink.spilled_bytes();
    let (t, c) = (Arc::clone(&term), Arc::clone(&ctx));
    let refused = answered_without_parking("key at the cap", move || {
        cross_input(&t, &c, parse_key("enter"), "ERR\n")
    });
    assert_eq!(refused, INPUT_QUEUE_FULL);
    assert!(
        refused.starts_with("ERR busy"),
        "a driver's back-off retries it"
    );
    assert_eq!(
        ctx.sink.spilled_bytes(),
        spilled,
        "a refused write accepts nothing"
    );
    drop(reader);
}

/// A background paste's `OK` is a kernel receipt, so one whose queued bytes a
/// discard drops while it waits (`signal`'s, `reset flush`) is never `OK`: the
/// drain fence reads the emptied spill as drained, and the paste was answered
/// `OK` though the program never got it. Now it is `ERR write failed`, in doubt.
///
/// On a raw pty nobody reads (a discard measures the tty's queue: macOS only),
/// whose two ends the test leaks, as [`pipe_session`] leaks its write end.
#[test]
#[cfg(target_os = "macos")]
fn a_background_paste_a_discard_drops_is_never_ok() {
    let (master, _slave) = crate::input_stall::tests::raw_pty_pair();
    let handle = registered_session(78, master, b"");
    handle.ctx.sink.note_master_nonblocking(true);
    loop {
        match aterm_pty::write_some(handle.master, &[b'.'; 4096]) {
            Ok(n) if n > 0 => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected tty fill result: {other:?}"),
        }
    }
    let (term, ctx) = (Arc::clone(&handle.term), Arc::clone(&handle.ctx));
    let (tx, rx) = std::sync::mpsc::channel();
    let (t, c) = (Arc::clone(&term), Arc::clone(&ctx));
    std::thread::spawn(move || {
        let _ = tx.send(background_input(
            &t,
            &c,
            &InputEvent::Paste("dropped".into(), PasteFraming::AtDrain),
            aterm_spec::wait::MUST_HAPPEN,
        ));
    });
    let queued = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(1),
        || (ctx.sink.spilled_bytes() > 0).then_some(()),
    );
    assert!(queued.is_some(), "the paste was never queued");
    assert!(
        ctx.sink
            .discard_unread_input(aterm_session::sink::Discard::Flush)
            .is_some(),
        "the discard ran"
    );
    assert_eq!(
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN),
        Ok(Ok(InputOutcome::WriteFailed)),
        "a paste the discard dropped was answered as delivered"
    );
}

/// A wheel writes one report per notch. Cut short at the session's input cap
/// after its first notch went, it is a failed write (`ERR write failed`), never
/// `ERR busy … nothing written`: a driver retrying that would scroll the
/// notches that went twice. Once nothing goes, it is refused busy.
#[test]
#[cfg(unix)]
fn a_wheel_cut_short_at_the_input_cap_is_not_answered_nothing_written() {
    let (handle, reader) = pipe_session(79);
    aterm_pty::set_nonblocking(handle.master, true).expect("nonblocking test master");
    handle.ctx.sink.note_master_nonblocking(true);
    loop {
        match aterm_pty::write_some(handle.master, &[b'.'; 4096]) {
            Ok(n) if n > 0 => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            other => panic!("unexpected pipe fill result: {other:?}"),
        }
    }
    let (term, ctx) = (&handle.term, &handle.ctx);
    // SGR mouse tracking: a wheel notch is a report.
    term_lock(term).process(b"\x1b[?1000h\x1b[?1006h");
    let below = vec![b'x'; aterm_session::sink::SinkWriter::SPILL_CAP - 1];
    let accepted = ctx
        .sink
        .write_frame_interactive_with_receipt(&below)
        .expect("a frame below the cap is accepted")
        .accepted();
    assert_eq!(accepted, below.len());
    let before = ctx.sink.spilled_bytes();
    assert_eq!(
        cross_mouse_apply(term, ctx, "wheeldown left 2 4 lines=3"),
        Err("ERR write failed\n".to_string())
    );
    let report = ctx.sink.spilled_bytes() - before;
    assert!(report > 0, "the first notch went");
    assert_eq!(
        cross_mouse_apply(term, ctx, "wheeldown left 2 4 lines=3"),
        Err(INPUT_QUEUE_FULL.to_string()),
        "at the cap nothing goes"
    );
    assert_eq!(ctx.sink.spilled_bytes() - before, report);
    drop(reader);
}

/// An input-write reply is stamped with the `content_seq` baseline (`seq=<n>` before
/// the trailing newline), so a driver can `await seq <n+1>` for the output its input
/// caused. Input verbs only, on `OK` only.
#[test]
fn stamp_input_seq_appends_baseline_to_ok_input_replies() {
    let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
    crate::term_lock(&term).process(b"state"); // advance content_seq off 0
    let seq = crate::term_lock(&term).content_seq();
    // `send`/`feed` OK replies gain ` seq=<n>` before the newline.
    assert_eq!(
        stamp_input_seq("send", "OK\n".to_string(), &term),
        format!("OK seq={seq}\n")
    );
    assert_eq!(
        stamp_input_seq("feed", "OK 3 bytes\n".to_string(), &term),
        format!("OK 3 bytes seq={seq}\n")
    );
    // A non-input verb is NOT stamped.
    assert_eq!(
        stamp_input_seq("text", "OK\n".to_string(), &term),
        "OK\n".to_string()
    );
    // An ERR reply is NOT stamped (no baseline to correlate).
    assert_eq!(
        stamp_input_seq("send", "ERR no window\n".to_string(), &term),
        "ERR no window\n".to_string()
    );
}

/// `feed-bin`'s self path re-verifies the edge against the session active now, not
/// the op alone: the global ActiveHandle retargets `@.`/self to the frontmost tab
/// on every switch (`sync_active_session`), so a `WriteInput` edge granted on
/// session B must not inject raw bytes into session A after A comes to the front.
#[test]
#[cfg(unix)]
fn feed_bin_self_edge_denied_after_active_session_swings() {
    let store = session_store::new_store();
    // Session A is FRONTMOST — what `@.`/self resolves to via the active handle.
    let (h_a, a_rx) = pipe_session(1);
    // Session B is where the edge was legitimately granted (a DIFFERENT session).
    let (h_b, _b_rx) = pipe_session(2);
    store.write().unwrap().register(h_a.clone());
    let active = active_for_handle(&h_a);

    // A WriteInput edge GRANTED on B's table: the op matches feed-bin, but it holds
    // no grant against the now-active A. (Scope is Copy, reused for both asserts.)
    let edge_b = edge_granted(Op::WriteInput, &h_b.ctx);
    // Positive control: the SAME edge legitimately drives its OWN session B.
    assert!(
        cross_session_authorized(edge_b, "feed", &h_b.ctx),
        "B's edge authorizes feed-bin against its own session",
    );

    let input: Vec<u8> = b"feed-bin 3\nXYZafter\n".to_vec();
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    let mut out: Vec<u8> = Vec::new();
    let line = read_request_line(&mut reader).unwrap();
    let keep = run_feed_bin(
        &line,
        "feed-bin",
        &mut reader,
        &active,
        &store,
        edge_b,
        &mut out,
    );

    assert!(keep, "denied frame keeps the connection");
    assert_eq!(
        String::from_utf8_lossy(&out),
        "ERR denied\n",
        "B's edge must NOT feed-bin the swung-to active session A",
    );
    assert!(drain_pipe(&a_rx).is_empty(), "nothing reached A's pty");
    // The denial still CONSUMED the N payload bytes, so the stream stays framed.
    let next = read_request_line(&mut reader).unwrap();
    assert_eq!(
        next, "after",
        "denial still consumes the payload (stream framed)"
    );
}

/// `read_request_line` strips the `\n` (and a CRLF `\r`), yields a final
/// unterminated line at EOF, and drops a runaway line past the cap (None).
#[test]
fn read_request_line_strips_newline_and_cr() {
    let input: Vec<u8> = b"plain\r\ncrlf\r\nlast-no-nl".to_vec();
    let mut reader = BufReader::new(std::io::Cursor::new(input));
    assert_eq!(read_request_line(&mut reader).as_deref(), Some("plain"));
    assert_eq!(read_request_line(&mut reader).as_deref(), Some("crlf"));
    assert_eq!(
        read_request_line(&mut reader).as_deref(),
        Some("last-no-nl"),
        "EOF yields the tail"
    );
    assert_eq!(read_request_line(&mut reader), None, "then EOF");
}

#[test]
fn guarded_line_deadline_rearms_once_per_buffer_refill() {
    let input = b"ACK 0123456789abcdef0123456789abcdef\n".to_vec();
    let mut reader = BufReader::with_capacity(128, std::io::Cursor::new(input));
    let mut rearms = 0usize;
    let line = read_request_line_with_policy(&mut reader, false, || {
        rearms += 1;
        Ok(())
    });
    assert_eq!(
        line.as_deref(),
        Some("ACK 0123456789abcdef0123456789abcdef")
    );
    assert_eq!(
        rearms, 1,
        "a fully buffered acknowledgement must not re-arm per byte"
    );
}

/// `ready` reports the target's readiness: a fresh OSC-133 prompt is `prompt`,
/// an in-flight command times out (not ready), a completed command is `prompt`
/// again, and a session marked `Exited` in the registry fails closed.
#[test]
fn ready_verb_reports_prompt_executing_and_exit() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;

    // A fresh prompt (OSC 133 A) -> PromptOnly -> ready immediately.
    term.lock().unwrap().process(b"\x1b]133;A\x07$ ");
    assert_eq!(
        cmd_ready(term, &store, 0, &h.ctx, "2000", &subscribe::new_registry()),
        "OK ready prompt\n",
        "fresh prompt is ready"
    );

    // An executing command -> NOT ready -> a short timeout returns `OK timeout`.
    term.lock()
        .unwrap()
        .process(b"\x1b]133;B\x07sleep\n\x1b]133;C\x07");
    assert_eq!(
        cmd_ready(term, &store, 0, &h.ctx, "0", &subscribe::new_registry()),
        "OK timeout\n",
        "executing is not ready"
    );

    // The command completes -> Complete -> ready again (prompt-end).
    term.lock().unwrap().process(b"\x1b]133;D;0\x07");
    assert_eq!(
        cmd_ready(term, &store, 0, &h.ctx, "2000", &subscribe::new_registry()),
        "OK ready prompt\n",
        "completed is ready"
    );

    // A session marked Exited never becomes ready -> ERR exited (fail closed).
    store
        .write()
        .unwrap()
        .set_state(0, session_store::SessionState::Exited);
    assert_eq!(
        cmd_ready(term, &store, 0, &h.ctx, "2000", &subscribe::new_registry()),
        "ERR exited\n",
        "exited fails closed"
    );
}

/// The wait family reads `<ms>` one way ([`aterm_control::wire::wait_ms`]): a
/// whole number of milliseconds, or the verb's usage line. `ready`, `await` and
/// `await momentum` read it leniently, so `ready 2s` or `await … timeout=1.5`
/// waited the 30 s default — here, on a surface already settled, each answered
/// as if `<ms>` were fine — where `turn` and `await consent` refused it.
#[test]
fn the_wait_family_refuses_an_ms_that_is_not_one() {
    use std::time::Instant;

    use control_session::cmd_await_momentum;

    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let registry = subscribe::new_registry();
    // A fresh prompt: `ready` and `await gone` both answer at once when asked well.
    crate::term_lock(&h.term).process(b"\x1b]133;A\x07$ ");
    let ready = |tail: &str| cmd_ready(&h.term, &store, 0, &h.ctx, tail, &registry);
    let await_ = |tail: &str| cmd_await(&h.term, &store, 0, &h.ctx, tail, &registry);
    let dark = || Ok((0.0, Instant::now()));
    let momentum = |tail: &str| cmd_await_momentum(&h.term, &store, 0, tail, &registry, &dark);

    for tail in ["2s", "timeout=2s", "1.5", "-1", "5 6", "timeout="] {
        assert!(
            ready(tail).starts_with("ERR usage: ready"),
            "`ready {tail}`"
        );
    }
    for tail in ["", "2000", "timeout=2000"] {
        assert_eq!(ready(tail), "OK ready prompt\n", "`ready {tail}`");
    }

    for tail in [
        "gone xyzzy timeout=2s",
        "gone xyzzy timeout 1.5",
        "gone xyzzy timeout=-1",
    ] {
        assert!(
            await_(tail).starts_with("ERR usage: await "),
            "`await {tail}`"
        );
    }
    for tail in [
        "gone xyzzy",
        "gone xyzzy timeout=2000",
        "gone xyzzy timeout 2000",
    ] {
        assert!(await_(tail).starts_with("OK gone "), "`await {tail}`");
    }

    for tail in ["momentum 0.3 timeout=2s", "momentum 0.3 timeout 1.5"] {
        assert!(
            momentum(tail).starts_with("ERR usage: await momentum"),
            "`await {tail}`"
        );
    }
    assert_eq!(momentum("momentum 0.3 timeout=2000"), "OK momentum 0.00\n");
}

/// `ready` for a plain shell (no OSC-133) settles on a stable `content_seq`: with no
/// in-flight block it returns `OK ready idle` once output stops changing across the
/// settle window.
#[test]
fn ready_verb_settles_on_idle_without_shell_integration() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"some output\r\n");
    store.write().unwrap().register(h.clone());
    // No OSC-133 at all: the settle heuristic fires (content_seq holds steady).
    assert_eq!(
        cmd_ready(
            &h.term,
            &store,
            0,
            &h.ctx,
            "2000",
            &subscribe::new_registry()
        ),
        "OK ready idle\n",
        "idle plain shell"
    );
}

/// `turn` happy path: the text is delivered once with paste semantics (a first
/// word holding `=` ends the option parse and stays text), one Enter verifiably
/// lands (the echo advances content_seq), the reply settles with the settled rows
/// and the turn's lease id, and the lease is released after.
#[test]
fn turn_types_submits_verified_and_returns_settled_screen() {
    use std::cell::{Cell, RefCell};
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;

    let pasted: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let presses = Cell::new(0u32);
    let paste = |text: &str| {
        pasted.borrow_mut().push(text.to_string());
        // The app echoes the typed text (content advances, then goes quiet).
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    let press = |name: &str| {
        assert_eq!(name, "enter", "default submit key is enter");
        presses.set(presses.get() + 1);
        // The app accepts the submit and prints its reply — seq advances.
        term.lock().unwrap().process(b"\r\nresponse-line\r\n");
        true
    };
    let out = cmd_turn(
        term,
        &store,
        0,
        "idle=50 timeout=60000 x=1 hello peer",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    assert!(
        out.starts_with("OK 24 turn submitted=1 status=settled seq="),
        "verified submit + settle, screen framed like text: {}",
        out.lines().next().unwrap_or("")
    );
    // The verdict carries the inline `dur_ms=`/`hash=` the catalog + INTROSPECTION.md
    // promise (the FNV of the settled screen, for replay-diffing without a 2nd call).
    let verdict = out.lines().next().unwrap_or("");
    assert!(
        verdict.contains(" dur_ms=") && verdict.contains(" hash=") && verdict.contains(" id="),
        "turn verdict carries dur_ms=, hash= and its lease id=: {verdict}"
    );
    assert!(out.contains("response-line"), "settled rows in the reply");
    assert_eq!(
        pasted.borrow().as_slice(),
        ["x=1 hello peer"],
        "the message text (options stripped, an unknown k=v kept) is delivered once"
    );
    assert_eq!(presses.get(), 1, "one press sufficed — no blind re-press");
    assert_eq!(
        *h.ctx.turn_lease.lock().unwrap(),
        None,
        "lease released after the turn"
    );
}

/// `turn if-rows=<first>-<last>:<hex16>`, the narrow fence: `if-gen=` refuses on any
/// output since the read, so a self-repainting screen (spinner, timer) would refuse
/// every time. The rows fence types while only rows outside its range moved and
/// refuses a change inside it (a keystroke echoed in the input box) with nothing
/// pasted or pressed. Negative controls: `if-gen=` (the whole-screen fence) over
/// the same outside repaint refuses with nothing pasted; a range past the grid is
/// `changed`; a malformed fence of either kind is the usage line. Parity: rows
/// from `text --json`, hashed by the client's
/// [`RowsFence::hash_rows`], equal the server's [`crate::control::rows_hash`]
/// (owner, 2026-09-29).
#[test]
fn turn_if_rows_lets_the_rows_outside_move_and_refuses_a_change_inside() {
    use aterm_types::control_verbs::RowsFence;
    use std::cell::{Cell, RefCell};
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let pasted: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let presses = Cell::new(0u32);
    let paste = |text: &str| {
        pasted.borrow_mut().push(text.to_string());
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    let press = |_: &str| {
        presses.set(presses.get() + 1);
        term.lock().unwrap().process(b"\r\nresponse-line\r\n");
        true
    };
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    let registry = subscribe::new_registry();
    // Row 0 a spinner, rows 1-3 an input box, the cursor in it.
    term.lock()
        .unwrap()
        .process("tick 1\r\n────────\r\n❯ \r\n────────\x1b[3;3H".as_bytes());
    let hash = crate::control::rows_hash(&term.lock().unwrap(), 1, 3).expect("in the grid");
    let fence = RowsFence {
        first: 1,
        last: 3,
        hash,
    };
    let judged = crate::control::screen_gen(&term.lock().unwrap());
    // PARITY: the client hashes what `text --json` served.
    let args = crate::control::text_args("").expect("args");
    let json = crate::control::cmd_text_json_looked(term, &h.ctx, args);
    let body = json.split_once('\n').map_or(json.as_str(), |(_, b)| b);
    let value: aterm_json::Value = aterm_json::from_str(body.trim()).expect("json");
    let rows: Vec<&str> = value["rows"].as_array().expect("rows")[1..=3]
        .iter()
        .map(|r| r.as_str().expect("a row"))
        .collect();
    assert_eq!(
        RowsFence::hash_rows(rows),
        hash,
        "one definition, both ends"
    );
    // The spinner row repaints: OUTSIDE the range.
    term.lock().unwrap().process(b"\x1b[1;1Htick 2\x1b[3;3H");
    // Negative control: the whole-screen fence refuses that repaint.
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-gen={judged} idle=50 timeout=5000 hello peer"),
        &registry,
        &h.ctx,
        &io,
    );
    assert!(out.contains("skipped reason=changed"), "{out}");
    assert!(pasted.borrow().is_empty());
    // The rows fence types over it.
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-rows={fence} idle=50 timeout=60000 hello peer"),
        &registry,
        &h.ctx,
        &io,
    );
    assert!(
        out.lines()
            .next()
            .is_some_and(|v| v.contains("submitted=1")),
        "{out}"
    );
    assert_eq!(pasted.borrow().as_slice(), ["hello peer"]);
    // A change INSIDE the range (a keystroke in the box): refused.
    let inside = crate::control::rows_hash(&term.lock().unwrap(), 1, 3).expect("in the grid");
    let now = RowsFence {
        first: 1,
        last: 3,
        hash: inside,
    };
    term.lock().unwrap().process(b"\x1b[3;3Hx");
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-rows={now} idle=50 timeout=5000 hello again"),
        &registry,
        &h.ctx,
        &io,
    );
    assert!(
        out.starts_with("OK 0 turn skipped reason=changed submitted=0 seq="),
        "{out}"
    );
    assert_eq!(
        pasted.borrow().len(),
        1,
        "nothing pasted into the changed box"
    );
    // A range past the grid reads as moved.
    let out = cmd_turn(
        term,
        &store,
        0,
        &format!("if-rows=900-901:{:016x} idle=50 hello", now.hash),
        &registry,
        &h.ctx,
        &io,
    );
    assert!(out.contains("skipped reason=changed"), "{out}");
    // A malformed fence, either kind, is the usage line, before anything is typed.
    for bad in ["if-rows=3-1:0123456789abcdef", "if-gen=12"] {
        let out = cmd_turn(
            term,
            &store,
            0,
            &format!("{bad} idle=50 hello"),
            &registry,
            &h.ctx,
            &io,
        );
        assert!(out.starts_with("ERR usage: turn"), "{bad}: {out}");
    }
    assert_eq!(pasted.borrow().len(), 1, "a bad fence types nothing");
}

/// A headless window wearing the rainbow kitty, ticked once so its engine is live,
/// for the even-hand laws. Its session writes into a real pipe (keep the returned
/// read end): a key whose write fails is revoked at the seam (stamp and press
/// credit), so only keys that reached the wire lay ribbon.
//
// Unix only: `SinkWriter::new`'s `i32` is a PTY master FD on unix but an opaque
// ConPTY registry key on Windows (`aterm_pty::windows::write_some` rejects unknown
// keys), and the `try_write_frame_immediate` family is fail-closed there
// (`ImmediateWrite::BusyZero`). No Windows fixture whose write reaches the wire
// exists yet.
#[cfg(unix)]
fn rainbow_kitty_window() -> (crate::App, crate::WindowId, std::fs::File) {
    use std::os::unix::io::FromRawFd;
    let (rd, wr) = cloexec_pipe();
    let rx = unsafe { std::fs::File::from_raw_fd(rd) };
    let mut app =
        crate::App::headless_for_test_with_sink(Arc::new(aterm_session::sink::SinkWriter::new(wr)));
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    let wid = crate::WindowId(0);
    app.windows.get_mut(&wid).expect("fixture window").focused = true;
    app.frontmost_window = Some(wid);
    app.cursor_frame(
        wid,
        crate::app_render::CursorFxInputs::sample_for_test(std::time::Instant::now()),
    )
    .expect("the fixture window ticks");
    (app, wid, rx)
}

/// An agent's typed turn lays ribbon and a pasted turn does not (the even hand,
/// `docs/design/RAINBOW-KITTY-V2.md` §28), through the real seam: every
/// `turn typed=1` press goes through `App::input` like a keyboard's, its echo
/// advances the caret one cell and the engine lays a segment; a plain `turn` is one
/// paste, whose echo carries no keystroke licence and lays nothing.
#[test]
// Unix-pinned with its fixture — see `rainbow_kitty_window` for why a
// Windows twin needs a ConPTY-backed sink and not a portable pipe.
#[cfg(unix)]
fn an_agent_s_typed_turn_lays_ribbon_and_a_pasted_turn_does_not() {
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, Instant};

    use aterm_grapheme::GraphemeClusters;

    use crate::app_render::CursorFxInputs;
    use crate::input::{InputEvent, Source};

    let drive = |line: &str| -> (String, usize) {
        let (app, wid, _rx) = rainbow_kitty_window();
        let app = RefCell::new(app);
        let col = Cell::new(0u16);
        // The seam, then the echo: the caret one cell on per glyph, and the
        // frame that spends the key's licence.
        let echo = |a: &mut crate::App, cells: u16| {
            col.set(col.get() + cells);
            let now = Instant::now() + Duration::from_millis(4);
            a.cursor_frame(
                wid,
                CursorFxInputs {
                    cur: Some((0, col.get())),
                    ..CursorFxInputs::sample_for_test(now)
                },
            )
            .expect("fixture window");
        };
        let key = |ev: InputEvent| {
            let mut a = app.borrow_mut();
            let _ = a.input(wid, ev, Source::Controller);
            echo(&mut a, 1);
            true
        };
        let paste = |text: &str| {
            let mut a = app.borrow_mut();
            let cells = text.graphemes().count() as u16;
            let _ = a.input(
                wid,
                InputEvent::Paste(text.to_string(), crate::input::PasteFraming::AtDrain),
                Source::Controller,
            );
            echo(&mut a, cells);
            true
        };
        let press = |_: &str| true;
        let momentum = || Ok((0.0, Instant::now()));
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let out = cmd_turn(
            &h.term,
            &store,
            0,
            line,
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                key: &key,
                momentum: &momentum,
                driver: None,
                actor: aterm_slog::Actor::Ctl,
                cause: None,
                mark: None,
            },
        );
        let laid = app.borrow().windows[&wid]
            .fx
            .cursor()
            .glow
            .ribbon_segments();
        (out.lines().next().unwrap_or("").to_string(), laid)
    };

    let (verdict, laid) = drive("typed=1 cadence=1 submit=none idle=20 timeout=60000 hello world");
    assert!(
        verdict.starts_with("OK 24 turn submitted=0 status=settled")
            && verdict.ends_with(" typed=11 pasted=0"),
        "a typed turn presses every grapheme and says so: {verdict}"
    );
    assert!(
        laid > 0,
        "eleven keys through the seam lay a ribbon: segments={laid}"
    );

    let (verdict, laid) = drive("submit=none idle=20 timeout=60000 hello world");
    assert!(
        verdict.starts_with("OK 24 turn submitted=0 status=settled") && !verdict.contains("typed="),
        "a plain turn's verdict is byte-identical to before: {verdict}"
    );
    assert_eq!(
        laid, 0,
        "a paste has no keystroke behind any cell: no ribbon"
    );
}

/// The typed turn's only signature is its even cadence (§28): key for key the route
/// sees the tuple a keyboard builds (base key under SHIFT for a capital or shifted
/// symbol, `Space`, `Enter` for a line break, the US base-layout identity in the
/// Kitty alternate field) and nothing names the presser. Presses land on an
/// absolute schedule (`t0 + i·cadence`), never early.
#[test]
fn the_typed_turn_s_only_signature_is_its_even_cadence() {
    use std::cell::RefCell;
    use std::time::Instant;

    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

    use crate::input::InputEvent;

    let seen: RefCell<Vec<(InputEvent, Instant)>> = RefCell::new(Vec::new());
    let pasted = RefCell::new(Vec::<String>::new());
    let key = |ev: InputEvent| {
        seen.borrow_mut().push((ev, Instant::now()));
        true
    };
    let paste = |t: &str| {
        pasted.borrow_mut().push(t.to_string());
        true
    };
    let press = |_: &str| true;
    let momentum = || Ok((0.0, Instant::now()));
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let cadence_ms = 40u64;
    let out = cmd_turn(
        &h.term,
        &store,
        0,
        &format!("typed=1 cadence={cadence_ms} submit=none idle=20 timeout=60000 Hi, a-1!\\n"),
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            momentum: &momentum,
            driver: None,
            actor: aterm_slog::Actor::Ctl,
            cause: None,
            mark: None,
        },
    );
    assert!(out.starts_with("OK "), "{out}");
    assert!(
        pasted.borrow().is_empty(),
        "inside the cap nothing is pasted"
    );

    let seen = seen.borrow();
    let keyboard = |key: Key, mods: Modifiers, base_layout: Option<char>| InputEvent::Key {
        key,
        mods,
        base_layout,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    // The paste verb's `\n` spelling is the text's own here: `\` `n` are
    // two typed glyphs, exactly as on a keyboard.
    let want = [
        keyboard(Key::Character('h'), Modifiers::SHIFT, Some('h')),
        keyboard(Key::Character('i'), Modifiers::empty(), Some('i')),
        keyboard(Key::Character(','), Modifiers::empty(), Some(',')),
        keyboard(Key::Named(NamedKey::Space), Modifiers::empty(), None),
        keyboard(Key::Character('a'), Modifiers::empty(), Some('a')),
        keyboard(Key::Character('-'), Modifiers::empty(), Some('-')),
        keyboard(Key::Character('1'), Modifiers::empty(), Some('1')),
        keyboard(Key::Character('1'), Modifiers::SHIFT, Some('1')),
        keyboard(Key::Character('\\'), Modifiers::empty(), Some('\\')),
        keyboard(Key::Character('n'), Modifiers::empty(), Some('n')),
    ];
    let got: Vec<&InputEvent> = seen.iter().map(|(ev, _)| ev).collect();
    assert_eq!(got.len(), want.len(), "one press per grapheme: {got:?}");
    for (g, w) in got.iter().zip(&want) {
        assert_eq!(*g, w, "the seam sees the keyboard's tuple");
    }
    // THE CADENCE: never early (an absolute schedule).
    let iois: Vec<u128> = seen
        .windows(2)
        .map(|w| (w[1].1 - w[0].1).as_millis())
        .collect();
    // The law is the absolute schedule the verb sleeps to: press `n` lands at or
    // after `start + n·cadence`. A per-interval floor does not hold, since the
    // stamp is taken after the sleep and one late press shortens the next gap.
    let origin = seen[0].1;
    for (n, (_, at)) in seen.iter().enumerate() {
        let due = u128::from(cadence_ms) * n as u128;
        let landed = (*at - origin).as_millis();
        assert!(
            landed + 1 >= due,
            "press {n} landed {landed} ms after the first, before its {due} ms slot: {iois:?}"
        );
    }
}

/// A typed turn's prefix is capped ([`control_session::TYPED_CAP`] presses, the
/// remainder one paste), and `cap=<n>` moves the bound: the same 300
/// graphemes are 300 presses and no paste with `cap=300`, while `cap=100` presses
/// 100 and pastes 200. A `cap` past [`control_session::TYPED_CAP_MAX`] is held to
/// it, and a non-positive or non-numeric `cap` is the usage line, typing nothing.
/// An agent reads a long paste as pasted, and the same text pressed key by key as
/// its person's words.
#[test]
fn cap_gives_a_typed_turn_room_to_press_a_long_text() {
    use std::cell::{Cell, RefCell};
    use std::time::Instant;

    use crate::input::InputEvent;

    let run = |args: &str| -> (String, usize, Vec<String>) {
        let keys = Cell::new(0usize);
        let pasted = RefCell::new(Vec::<String>::new());
        let key = |_: InputEvent| {
            keys.set(keys.get() + 1);
            true
        };
        let paste = |t: &str| {
            pasted.borrow_mut().push(t.to_string());
            true
        };
        let press = |_: &str| true;
        let momentum = || Ok((0.0, Instant::now()));
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let text: String = "abcde ".repeat(50); // 300 graphemes
        let out = cmd_turn(
            &h.term,
            &store,
            0,
            &format!("typed=1 cadence=1 submit=none idle=20 timeout=60000 {args} {text}"),
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                key: &key,
                momentum: &momentum,
                driver: None,
                actor: aterm_slog::Actor::Ctl,
                cause: None,
                mark: None,
            },
        );
        let verdict = out.lines().next().unwrap_or("").to_string();
        (verdict, keys.get(), pasted.into_inner())
    };

    // No cap: the default caps the typed prefix, and the remainder is ONE paste
    // of exactly the text past it (the seam voices it as one up-strum).
    let cap = control_session::TYPED_CAP;
    let (verdict, keys, pasted) = run("");
    assert!(
        verdict.ends_with(&format!(" typed={cap} pasted={}", 300 - cap)),
        "{verdict}"
    );
    assert_eq!(keys, cap);
    assert_eq!(
        format!(
            "{}{}",
            "abcde ".repeat(50).get(..cap).unwrap(),
            pasted.concat()
        ),
        "abcde ".repeat(50),
        "one paste, exactly the text past the cap: {pasted:?}"
    );
    assert_eq!(pasted.len(), 1);

    let (verdict, keys, pasted) = run("cap=300");
    assert!(verdict.ends_with(" typed=300 pasted=0"), "{verdict}");
    assert_eq!(keys, 300);
    assert!(pasted.is_empty(), "nothing is pasted: {pasted:?}");

    let (verdict, keys, pasted) = run("cap=100");
    assert!(verdict.ends_with(" typed=100 pasted=200"), "{verdict}");
    assert_eq!(keys, 100);
    assert_eq!(pasted.len(), 1, "one paste: {pasted:?}");

    // Held to the ceiling: a huge cap types what there is, no more.
    let (verdict, keys, pasted) = run("cap=999999");
    assert!(verdict.ends_with(" typed=300 pasted=0"), "{verdict}");
    assert_eq!((keys, pasted.len()), (300, 0));

    // Not a positive number: the usage line, nothing typed or pasted.
    for bad in ["cap=0", "cap=-3", "cap=lots"] {
        let (verdict, keys, pasted) = run(bad);
        assert!(verdict.starts_with("ERR usage: turn"), "{bad}: {verdict}");
        assert_eq!((keys, pasted.len()), (0, 0), "{bad} typed something");
    }
}

/// One typed `turn` of "hello world" (11 keys) into a fresh session, running
/// `during(<keys delivered so far>, &session)` after each key (a person's key, a
/// screen switch, a slow route) and counting Enters: `(verdict, keys, enters)`.
/// `opts` lead the request; `fence` names `if-gen=` at the starting screen.
fn typed_turn_with(
    opts: &str,
    fence: bool,
    during: &dyn Fn(usize, &SessionHandle),
) -> (String, usize, u32) {
    use std::cell::Cell;
    use std::time::Instant;

    use crate::input::InputEvent;

    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let keys = Cell::new(0usize);
    let enters = Cell::new(0u32);
    let key = |_: InputEvent| {
        keys.set(keys.get() + 1);
        during(keys.get(), &h);
        true
    };
    let paste = |_: &str| true;
    let press = |_: &str| {
        enters.set(enters.get() + 1);
        h.term.lock().unwrap().process(b"\r\nresponse-line\r\n");
        true
    };
    let momentum = || Ok((0.0, Instant::now()));
    let fence = if fence {
        format!(
            "if-gen={} ",
            crate::control::screen_gen(&h.term.lock().unwrap())
        )
    } else {
        String::new()
    };
    let out = cmd_turn(
        &h.term,
        &store,
        0,
        &format!("{fence}typed=1 idle=20 {opts} hello world"),
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            momentum: &momentum,
            driver: None,
            actor: aterm_slog::Actor::Ctl,
            cause: None,
            mark: None,
        },
    );
    let verdict = out.lines().next().unwrap_or("").to_string();
    (verdict, keys.get(), enters.get())
}

/// A fenced typed turn is held between its keys and presses nothing when the hold
/// breaks: `if-gen=`/`if-rows=` are checked before the first key, and a long notice
/// takes seconds to type. A person's key counted after the fence (between keys or
/// in the final echo settle) stops it with no later key and no Enter:
/// `skipped reason=person typed=<k>`; a screen switch gives `reason=changed`.
/// Negative controls: with nobody at the keys it types all 11 and submits, and an
/// unfenced typed turn is untouched (a person's key extends it).
#[test]
fn a_fenced_typed_turn_stops_at_a_persons_key_and_presses_nothing() {
    let person_at = |n: usize| {
        move |k: usize, h: &SessionHandle| {
            if k == n {
                h.ctx
                    .human_input
                    .note(crate::metrics::now_us(), crate::human_input::Gesture::Other);
            }
        }
    };
    let fast = "cadence=1 timeout=60000";
    // A person's key after the fifth: five typed, nothing pressed.
    let (verdict, keys, enters) = typed_turn_with(fast, true, &person_at(5));
    assert!(
        verdict.starts_with("OK 0 turn skipped reason=person typed=5 pasted=0 submitted=0 seq="),
        "{verdict}"
    );
    assert_eq!((keys, enters), (5, 0), "no key after it, and no Enter");
    // In the echo settle after the last key: every key typed, no Enter.
    let (verdict, keys, enters) = typed_turn_with(fast, true, &person_at(11));
    assert!(
        verdict.starts_with("OK 0 turn skipped reason=person typed=11 pasted=0 submitted=0"),
        "{verdict}"
    );
    assert_eq!((keys, enters), (11, 0), "the Enter is not pressed");
    // A screen switch after the third (the alternate screen entered).
    let switch_at_3 = |k: usize, h: &SessionHandle| {
        if k == 3 {
            h.term.lock().unwrap().process(b"\x1b[?1049h");
        }
    };
    let (verdict, keys, enters) = typed_turn_with(fast, true, &switch_at_3);
    assert!(
        verdict.starts_with("OK 0 turn skipped reason=changed typed=3 pasted=0 submitted=0"),
        "{verdict}"
    );
    assert_eq!((keys, enters), (3, 0));
    // Negative controls: nobody at the keys, typed and submitted; and the unfenced
    // turn a person types into goes on.
    let (verdict, keys, enters) = typed_turn_with(fast, true, &|_, _| {});
    assert!(
        verdict.contains(" submitted=1 ") && verdict.ends_with(" typed=11 pasted=0"),
        "{verdict}"
    );
    assert_eq!((keys, enters), (11, 1));
    let (verdict, keys, enters) = typed_turn_with(fast, false, &person_at(5));
    assert!(verdict.ends_with(" typed=11 pasted=0"), "{verdict}");
    assert_eq!((keys, enters), (11, 1));
}

/// The composer's all-`─` rule, the test grid's full width.
fn composer_rule() -> String {
    "─".repeat(80)
}

/// A model of Claude Code's composer for the caret-anchor tests: top rule, caret row
/// `❯ <draft>` wrapping every `wrap` graphemes onto rows indented by two, bottom
/// rule, footer and rows under it, drawn from row `at` as one finished DEC-2026
/// frame with the cursor after the draft, as Claude Code repaints at every key. On a
/// `full` screen a new draft row moves the top rule up instead of the bottom rule
/// down. `lag` delays the next key's echo, `hold` holds it until the following key
/// has gone out (an order, not a clock), and `later` writes bytes after their delay
/// on a thread. `fence`, set before the fence is read, names other rows than the
/// composer's three.
struct Composer {
    top: String,
    wrap: usize,
    at: std::cell::Cell<usize>,
    full: std::cell::Cell<bool>,
    lag: std::cell::Cell<Option<std::time::Duration>>,
    hold: std::cell::Cell<bool>,
    held: std::cell::RefCell<Option<(String, usize)>>,
    fence: std::cell::Cell<Option<(usize, usize)>>,
    later: std::cell::RefCell<Vec<(String, std::time::Duration)>>,
    draft: std::cell::RefCell<Vec<String>>,
    footer: std::cell::RefCell<String>,
    under: std::cell::RefCell<Vec<String>>,
}

impl Composer {
    fn new(top: &str, wrap: usize) -> Self {
        Self {
            top: top.to_string(),
            wrap,
            at: std::cell::Cell::new(1),
            full: std::cell::Cell::new(false),
            lag: std::cell::Cell::new(None),
            hold: std::cell::Cell::new(false),
            held: std::cell::RefCell::default(),
            fence: std::cell::Cell::new(None),
            later: std::cell::RefCell::default(),
            draft: std::cell::RefCell::default(),
            footer: std::cell::RefCell::new(
                "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents".to_string(),
            ),
            under: std::cell::RefCell::default(),
        }
    }

    /// The frame's bytes as the model stands, `extra` drawn first inside
    /// the same bracket.
    fn frame(&self, extra: &str) -> String {
        let draft = self.draft.borrow();
        let mut rows: Vec<String> = draft.chunks(self.wrap).map(<[String]>::concat).collect();
        if rows.is_empty() {
            rows.push(String::new());
        }
        let at = self.at.get();
        let mut f = format!("\x1b[?2026h{extra}\x1b[{};1H\x1b[J{}", at + 1, self.top);
        for (i, row) in rows.iter().enumerate() {
            f.push_str(if i == 0 { "\r\n❯ " } else { "\r\n  " });
            f.push_str(row);
        }
        f.push_str("\r\n");
        f.push_str(&composer_rule());
        f.push_str("\r\n");
        f.push_str(&self.footer.borrow());
        for row in self.under.borrow().iter() {
            f.push_str("\r\n");
            f.push_str(row);
        }
        let col = 2 + rows.last().map_or(0, |r| r.chars().count());
        f.push_str(&format!(
            "\x1b[{};{}H\x1b[?2026l",
            at + rows.len() + 1,
            col + 1
        ));
        f
    }

    /// One grapheme into the draft; a full screen's new row moves the
    /// frame up.
    fn push(&self, g: &str) {
        let rows = |n: usize| n.div_ceil(self.wrap).max(1);
        let before = rows(self.draft.borrow().len());
        self.draft.borrow_mut().push(g.to_string());
        if self.full.get() && rows(self.draft.borrow().len()) > before {
            self.at.set(self.at.get() - 1);
        }
    }

    /// Repaint now, `extra` first inside the same bracket.
    fn draw(&self, h: &SessionHandle, extra: &str) {
        h.term.lock().unwrap().process(self.frame(extra).as_bytes());
    }
}

/// What a typed turn into a [`Composer`] did: its verdict line, graphemes delivered
/// and when, the keys (1-based) sent before the previous key echoed, and the Enters
/// pressed.
struct Typing {
    verdict: String,
    keys: Vec<String>,
    sent_at: Vec<std::time::Instant>,
    ahead: Vec<usize>,
    enters: u32,
}

/// One `typed=1 cadence=1` turn of `text` into a fresh session showing `tick 1` over
/// a [`Composer`] with top rule `top`, fenced `if-rows=` on the composer's three rows
/// as the driver fences them (`composer_rows`, cursor on the caret row). Each key is
/// echoed as a finished frame, then `during(<keys delivered>, &composer, &session)`
/// runs; `during(0, …)` runs before the fence is read.
fn composer_turn_with(
    top: &str,
    wrap: usize,
    opts: &str,
    text: &str,
    during: &dyn Fn(usize, &Composer, &SessionHandle),
) -> Typing {
    composer_turn_on(-1, top, wrap, opts, text, during)
}

/// [`composer_turn_with`] over a session whose sink is `master` — a real
/// pty's, for a test that needs the kernel's count of unread input.
///
/// THE TIMEOUTS THESE TURNS NAME: a turn returns at its verdict, so for a
/// verdict that must come — every key typed, `submitted=1`, a stop at a
/// fact — the `timeout=` is a hang detector (60000 ms), and a few seconds
/// only cut a slow but correct turn short. A short timeout is named only
/// where `reason=deadline` is the subject.
fn composer_turn_on(
    master: i32,
    top: &str,
    wrap: usize,
    opts: &str,
    text: &str,
    during: &dyn Fn(usize, &Composer, &SessionHandle),
) -> Typing {
    use std::cell::{Cell, RefCell};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use aterm_grapheme::GraphemeClusters;

    let store = session_store::new_store();
    let h = registered_session(0, master, b"");
    store.write().unwrap().register(h.clone());
    let c = Composer::new(top, wrap);
    c.draw(&h, "tick 1");
    during(0, &c, &h);
    let fence = {
        let at = c.at.get();
        let (first, last) = c.fence.get().unwrap_or((at, at + 2));
        let hash =
            crate::control::rows_hash(&h.term.lock().unwrap(), first, last).expect("in the grid");
        aterm_types::control_verbs::RowsFence { first, last, hash }
    };
    let graphemes: Vec<String> = text.graphemes().map(str::to_string).collect();
    // The highest key whose echo is on the grid.
    let drawn = AtomicUsize::new(0);
    let keys = RefCell::new(Vec::new());
    let sent_at = RefCell::new(Vec::new());
    let ahead = RefCell::new(Vec::new());
    let enters = Cell::new(0u32);
    let verdict = std::thread::scope(|s| {
        // `(bytes, the key they echo, delay)`, written in order.
        let (tx, rx) = std::sync::mpsc::channel::<(String, usize, Duration)>();
        let (term, echoed) = (&h.term, &drawn);
        s.spawn(move || {
            for (bytes, k, delay) in rx {
                std::thread::sleep(delay);
                let mut t = term.lock().unwrap();
                t.process(bytes.as_bytes());
                echoed.fetch_max(k, Ordering::SeqCst);
            }
        });
        let key = |_: InputEvent| {
            sent_at.borrow_mut().push(Instant::now());
            let k = keys.borrow().len() + 1;
            if drawn.load(Ordering::SeqCst) + 1 < k {
                ahead.borrow_mut().push(k);
            }
            let g = &graphemes[k - 1];
            keys.borrow_mut().push(g.clone());
            c.push(g);
            let frame = c.frame("");
            match (c.lag.take(), c.hold.take()) {
                (Some(lag), _) => tx.send((frame, k, lag)).expect("the echo thread"),
                (None, true) => *c.held.borrow_mut() = Some((frame, k)),
                (None, false) => {
                    let mut t = h.term.lock().unwrap();
                    // A held echo lands once the key after it is out.
                    if let Some((before, j)) = c.held.take() {
                        t.process(before.as_bytes());
                        drawn.fetch_max(j, Ordering::SeqCst);
                    }
                    t.process(frame.as_bytes());
                    drawn.fetch_max(k, Ordering::SeqCst);
                }
            }
            during(k, &c, &h);
            for (bytes, delay) in c.later.take() {
                tx.send((bytes, 0, delay)).expect("the echo thread");
            }
            true
        };
        let paste = |_: &str| true;
        let press = |_: &str| {
            enters.set(enters.get() + 1);
            h.term.lock().unwrap().process(b"\r\nresponse-line\r\n");
            true
        };
        let momentum = || Ok((0.0, Instant::now()));
        let out = cmd_turn(
            &h.term,
            &store,
            0,
            &format!("if-rows={fence} typed=1 cadence=1 idle=20 {opts} {text}"),
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                key: &key,
                momentum: &momentum,
                driver: None,
                actor: aterm_slog::Actor::Ctl,
                cause: None,
                mark: None,
            },
        );
        drop(tx);
        out.lines().next().unwrap_or("").to_string()
    });
    Typing {
        verdict,
        keys: keys.into_inner(),
        sent_at: sent_at.into_inner(),
        ahead: ahead.into_inner(),
        enters: enters.get(),
    }
}

/// A box that takes the composer's place mid-notice stops typing before its digits.
/// A subagent's Bash box can mount between two keys without moving the person
/// count or epoch, and the notice's digits would answer it. A turn fenced
/// `if-rows=` on the composer is held to that frame: a box drawn in its place after
/// the 28th key stops it (`skipped reason=changed typed=28`, delivered exactly
/// `[aterm harness] Claude Code `, no digit of `2.1.291`, no Enter), for a labelled
/// and an all-`─` top rule. Negative control (why the caret matters): an
/// AskUserQuestion box between all-`─` borders like the composer's rules, cursor on
/// its `4. Type something.` row with the lower border under it; both rules match,
/// and the caret-less ` ☐ Colors` row under the top border stops it.
#[test]
fn a_box_that_takes_the_composers_place_stops_the_typing_before_its_digits() {
    let rule = composer_rule();
    let labelled = format!("{} xhigh ─", "─".repeat(72));
    let bash: Vec<String> = [
        rule.as_str(),
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        " Esc to cancel",
    ]
    .map(str::to_string)
    .to_vec();
    let question: Vec<String> = [
        rule.as_str(),
        " ☐ Colors",
        "",
        "Which color scheme should the demo page use?",
        "",
        "❯ 1. Dark (Recommended)",
        "  2. Light",
        "  3. High contrast",
        "  4. Type something.",
        rule.as_str(),
        "  5. Chat about this",
        "",
        "Enter to select · ↑/↓ to navigate · Esc to cancel",
    ]
    .map(str::to_string)
    .to_vec();
    let text = "[aterm harness] Claude Code 2.1.291 is installed";
    for (top, rows, cursor) in [
        (&labelled, &bash, 4),
        (&rule, &bash, 4),
        (&rule, &question, 8),
    ] {
        // The box, one finished frame over the composer after key 28,
        // the cursor on its row `cursor`.
        let mount = |k: usize, c: &Composer, h: &SessionHandle| {
            if k == 28 {
                let at = c.at.get();
                let frame = format!(
                    "\x1b[?2026h\x1b[{};1H\x1b[J{}\x1b[{};3H\x1b[?2026l",
                    at + 1,
                    rows.join("\r\n"),
                    at + cursor + 1
                );
                h.term.lock().unwrap().process(frame.as_bytes());
            }
        };
        let t = composer_turn_with(top, 76, "timeout=60000", text, &mount);
        assert!(
            t.verdict
                .starts_with("OK 0 turn skipped reason=changed typed=28 pasted=0 submitted=0"),
            "{top} / {}: {}",
            rows[1],
            t.verdict
        );
        assert_eq!(
            t.keys.concat(),
            "[aterm harness] Claude Code ",
            "no digit reaches the box"
        );
        assert_eq!(t.enters, 0);
    }
}

/// The caret anchor follows everything a composer's screen does while typed into,
/// in one turn that types every key and submits: the footer losing
/// `· ← for agents` at the first key; `tick 2` above and a workflow timer under the
/// footer (key 3); wrapping that moves the bottom rule down (keys 10 and 19); the
/// session survey drawn over the top rule, the frame two rows down (key 15); a full
/// screen from key 20 whose next wrap (key 28) moves the top rule up.
#[test]
fn the_caret_anchor_rides_the_screen_around_a_growing_composer() {
    let during = |k: usize, c: &Composer, h: &SessionHandle| match k {
        1 => {
            *c.footer.borrow_mut() = "  ⏵⏵ auto mode on (shift+tab to cycle)".to_string();
            c.draw(h, "");
        }
        3 => {
            c.under
                .borrow_mut()
                .push("◯ cache-lever-pick · 20m 59s".to_string());
            c.draw(h, "\x1b[1;1Htick 2");
        }
        15 => {
            let at = c.at.get();
            c.at.set(at + 2);
            c.draw(
                h,
                &format!(
                    "\x1b[{};1H● How is Claude doing this session? (optional)\r\n  1: Bad    \
                     2: Fine   3: Good   0: Dismiss",
                    at + 1
                ),
            );
        }
        20 => c.full.set(true),
        _ => {}
    };
    let labelled = format!("{} xhigh ─", "─".repeat(72));
    let text = "rides the rows around its frame";
    let t = composer_turn_with(&labelled, 9, "timeout=60000", text, &during);
    assert!(
        t.verdict.contains(" submitted=1 ") && t.verdict.ends_with(" typed=31 pasted=0"),
        "{}",
        t.verdict
    );
    assert_eq!((t.keys.concat().as_str(), t.enters), (text, 1));
}

/// **A TORN FRAME IS NEVER JUDGED** (the review of 2026-09-30, item D):
/// while the program's DEC-2026 frame is open with writes in it the
/// cursor is its WRITE position, not its caret (a Claude Code transcript
/// redraw has held one ~280 ms), so the key waits for the frame to
/// finish, and a frame never finished stops the typing at the turn's
/// deadline — `reason=deadline typed=0`, never `changed`, no Enter. The
/// frame is torn BEFORE the first key, so this verdict is the only one a
/// correct turn can give however late the test thread runs: torn after
/// key 3 under a 300 ms deadline, a stall before key 3 cut the typing
/// short at `typed=1`. The waits mid-text are the finished frame's below.
/// NEGATIVE CONTROL: the same write in a CLOSED bracket, the cursor left
/// outside the frame, is judged — `reason=changed typed=3`. A torn frame
/// the program FINISHES 100 ms later, the caret back in the composer, is
/// judged then and the typing goes on — on its cadence: the key that
/// waited anchors the schedule, so the seven keys after it take their
/// 7 × 20 ms, not a burst catching up on the time spent (a ~280 ms
/// redraw would have been 140 keys back to back at `cadence=2`). And
/// the hold is ARMED whatever the cursor did when the fence was read: a
/// cursor off the frame then (a repaint's write position, as a fence
/// read mid-frame finds it) is judged at the first key, `reason=changed
/// typed=0`, not a hold that silently dropped out for the whole text.
#[test]
fn a_torn_frame_is_never_judged() {
    let write_at = |n: usize, close: &'static str| {
        move |k: usize, _: &Composer, h: &SessionHandle| {
            if k == n {
                let write = format!("\x1b[?2026h\x1b[20;1H⎿ transcript{close}");
                h.term.lock().unwrap().process(write.as_bytes());
            }
        }
    };
    let rule = composer_rule();
    let text = "hello world";
    let t = composer_turn_with(&rule, 76, "timeout=300", text, &write_at(0, ""));
    assert!(
        t.verdict
            .starts_with("OK 0 turn skipped reason=deadline typed=0 pasted=0 submitted=0"),
        "{}",
        t.verdict
    );
    assert_eq!((t.keys.len(), t.enters), (0, 0));
    let t = composer_turn_with(
        &rule,
        76,
        "timeout=60000",
        text,
        &write_at(3, "\x1b[?2026l"),
    );
    assert!(
        t.verdict
            .starts_with("OK 0 turn skipped reason=changed typed=3 pasted=0 submitted=0"),
        "{}",
        t.verdict
    );
    assert_eq!((t.keys.len(), t.enters), (3, 0));
    let finished_later = |k: usize, c: &Composer, h: &SessionHandle| {
        if k == 3 {
            let write = "\x1b[?2026h\x1b[20;1H⎿ transcript";
            h.term.lock().unwrap().process(write.as_bytes());
            let done = std::time::Duration::from_millis(100);
            c.later.borrow_mut().push((c.frame(""), done));
        }
    };
    let t = composer_turn_with(&rule, 76, "cadence=20 timeout=60000", text, &finished_later);
    assert!(t.verdict.contains(" submitted=1 "), "{}", t.verdict);
    assert_eq!((t.keys.concat().as_str(), t.enters), (text, 1));
    let ms = |a: usize, b: usize| t.sent_at[b].duration_since(t.sent_at[a]).as_millis();
    assert!(ms(2, 3) >= 100, "key 4 waited for the frame: {}", ms(2, 3));
    assert!(ms(3, 10) >= 135, "no burst after the wait: {}", ms(3, 10));
    let t = composer_turn_with(
        &rule,
        76,
        "timeout=60000",
        text,
        &write_at(0, "\x1b[?2026l"),
    );
    assert!(
        t.verdict
            .starts_with("OK 0 turn skipped reason=changed typed=0 pasted=0 submitted=0"),
        "{}",
        t.verdict
    );
    assert_eq!((t.keys.len(), t.enters), (0, 0));
}

/// A digit waits for the key before it to echo: a box's first frame reaches the grid
/// a render after it mounts, and keys in flight go to it. So before a digit the
/// caret must have moved since the previous key went out; that key's echo, held
/// 40 ms here, lands before `2` is pressed, and digits whose predecessors echo at
/// once are not slowed. Negative control: a letter after a key whose echo is held
/// until the letter has gone out (an order, not a clock) goes out on schedule.
#[test]
fn a_digit_waits_for_the_key_before_it_to_echo() {
    let hold_back = |k: usize, c: &Composer, _: &SessionHandle| {
        // Key 8, the space before `2`: echoed 40 ms late.
        if k == 7 {
            c.lag.set(Some(std::time::Duration::from_millis(40)));
        }
        // Key 16, the space before `is`: echoed once `i` is out.
        if k == 15 {
            c.hold.set(true);
        }
    };
    let text = "version 2.1.291 is in";
    let t = composer_turn_with(&composer_rule(), 76, "timeout=60000", text, &hold_back);
    assert!(
        t.verdict.contains(" submitted=1 ") && t.verdict.ends_with(" typed=21 pasted=0"),
        "{}",
        t.verdict
    );
    assert_eq!(t.keys.concat(), text);
    assert_eq!(
        t.ahead,
        [17],
        "only the letter after a held-back echo went out ahead of it"
    );
}

/// **A DIGIT WAITS FOR THE PROGRAM TO READ EVERY KEY BEFORE IT**
/// (2026-10-01): the caret-moved check alone was satisfied by an OLDER
/// key's echo while later keys sat unread — keys a box mounting then
/// would take, a digit answering it. On a real pty a line written to its
/// master stands unread in the slave's queue until read
/// ([`aterm_session::sink::SinkWriter::input_backlog`]): left there after
/// key 8, the space before `2`, and never read, the text stops at its
/// deadline with the digit unpressed — `version ` typed, nothing more;
/// read by a reader meanwhile, the digit goes out and the text is
/// submitted. NEGATIVE CONTROL, the wait is the digit's: a text of
/// letters types every key past the same unread line.
///
/// The deadline is the subject only of the line never read, and where it
/// falls is the clock's: a test thread stalled past it before key 8 cuts
/// the text shorter, still with no digit and no Enter — which is what is
/// asserted, not `typed=8`. The witness that holds on every run is the
/// line read meanwhile: whenever a digit goes out, the program has read
/// every key before it — the queue is empty at its send.
///
/// macOS only: elsewhere a tty cannot count its unread input
/// (`aterm_pty::input_queue_len` answers `None`), and the digit keeps the
/// caret check alone, as `control_session::keys_unread` says.
#[cfg(target_os = "macos")]
#[test]
fn a_digit_waits_for_the_program_to_read_every_key_before_it() {
    use aterm_session::sink::Discard;
    let (master, slave) = aterm_pty::open_pty_pair().expect("a pty pair");
    let digits_over_unread = std::cell::Cell::new(0usize);
    let unread_after = |at: usize, drain: bool| {
        let digits_over_unread = &digits_over_unread;
        move |k: usize, c: &Composer, h: &SessionHandle| {
            if k == at {
                aterm_pty::write_all(master, b"x\n");
                if drain {
                    let sink = h.ctx.sink.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                        let _ = sink.discard_unread_input(Discard::Flush);
                    });
                }
            }
            let sent_digit = c
                .draft
                .borrow()
                .last()
                .is_some_and(|g| g.chars().all(|ch| ch.is_ascii_digit()));
            if sent_digit && aterm_pty::input_queue_len(master) != Some(0) {
                digits_over_unread.set(digits_over_unread.get() + 1);
            }
        }
    };
    let rule = composer_rule();
    let text = "version 2.1.291 is in";
    let t = composer_turn_on(
        master,
        &rule,
        76,
        "timeout=1500",
        text,
        &unread_after(8, false),
    );
    let typed = t.keys.concat();
    assert!(
        t.verdict.contains("skipped reason=deadline typed=")
            && t.verdict.contains(&format!(" typed={} ", t.keys.len())),
        "{}",
        t.verdict
    );
    assert!(
        "version ".starts_with(&typed),
        "no digit while the line stands unread: {typed:?}"
    );
    assert_eq!(t.enters, 0);
    // NEGATIVE CONTROL: letters do not wait on it.
    let letters = "version is in";
    let t = composer_turn_on(
        master,
        &rule,
        76,
        "timeout=60000",
        letters,
        &unread_after(3, false),
    );
    assert!(t.verdict.contains(" submitted=1 "), "{}", t.verdict);
    assert_eq!(t.keys.concat(), letters);
    // Read meanwhile: the digit goes out, the text is whole — and no
    // digit went out while the line was still unread.
    let t = composer_turn_on(
        master,
        &rule,
        76,
        "timeout=60000",
        text,
        &unread_after(8, true),
    );
    assert!(t.verdict.contains(" submitted=1 "), "{}", t.verdict);
    assert_eq!(t.keys.concat(), text);
    assert_eq!(
        digits_over_unread.get(),
        0,
        "a digit went out while a key before it stood unread"
    );
    aterm_pty::close_fd(slave);
    aterm_pty::close_fd(master);
}

/// **A TYPED TURN FENCED ON ROWS THAT ARE NO INPUT FRAME IS HELD TO THE
/// PERSON AND THE SCREEN ALONE** (the review of the item-D build): the
/// caret anchor is armed only where the `if-rows=` rows are a frame — a
/// rule over a caret row down to a rule. A driver fencing the one row it
/// types into (a shell prompt's, the `if-rows=` help's own example) got
/// a frame whose top and bottom were that row, the row under the cursor
/// never matched it, and every such turn stopped at its first key
/// (`skipped reason=changed typed=0`) — the word the help calls a retry
/// signal, so the driver would retry for ever. (The composer's own three
/// rows arm it: `a_box_that_takes_the_composers_place_stops_the_typing_before_its_digits`.)
#[test]
fn a_typed_turn_fenced_on_no_input_frame_types_every_key() {
    let text = "echo 2.1.291";
    let caret_row = |k: usize, c: &Composer, _: &SessionHandle| {
        if k == 0 {
            let caret = c.at.get() + 1;
            c.fence.set(Some((caret, caret)));
        }
    };
    let t = composer_turn_with(&composer_rule(), 76, "timeout=60000", text, &caret_row);
    assert!(
        t.verdict.contains(" submitted=1 ") && t.verdict.ends_with(" typed=12 pasted=0"),
        "{}",
        t.verdict
    );
    assert_eq!((t.keys.concat().as_str(), t.enters), (text, 1));
}

/// A typed turn is never submitted in part. A head whose schedule ends past the
/// deadline is not begun (`typed=0`, nothing pressed), and one a slow route lets the
/// deadline overtake stops with no Enter: `skipped reason=deadline typed=<k>`, k
/// short of the text. Negative control: the slow route given the time types all 11
/// and submits.
#[test]
fn a_typed_turn_the_deadline_overtakes_is_never_submitted() {
    // 11 keys 100 ms apart need a second; 500 ms cannot hold them.
    let (verdict, keys, enters) = typed_turn_with("cadence=100 timeout=500", false, &|_, _| {});
    assert!(
        verdict.starts_with("OK 0 turn skipped reason=deadline typed=0 pasted=0 submitted=0"),
        "{verdict}"
    );
    assert_eq!((keys, enters), (0, 0), "not begun");
    // The schedule fits (11 ms), the route does not: 60 ms a key. Eleven
    // such keys cannot fit 300 ms, so the cut is certain; WHERE it falls
    // is the clock's — a test thread stalled past the deadline before the
    // first key cuts it at `typed=0` — and the subject is what follows
    // the cut: no Enter, the keys typed said.
    let slow =
        |_: usize, _: &SessionHandle| std::thread::sleep(std::time::Duration::from_millis(60));
    let (verdict, keys, enters) = typed_turn_with("cadence=1 timeout=300", false, &slow);
    assert!(
        verdict.starts_with("OK 0 turn skipped reason=deadline typed="),
        "{verdict}"
    );
    assert!(keys < 11, "cut short of the text: {keys}");
    assert!(
        verdict.contains(&format!(" typed={keys} pasted=0 ")),
        "{verdict}"
    );
    assert_eq!(enters, 0, "the part typed is not submitted");
    // NEGATIVE CONTROL: the same slow route given the time.
    let (verdict, keys, enters) = typed_turn_with("cadence=1 timeout=60000", false, &slow);
    assert!(verdict.contains(" submitted=1 "), "{verdict}");
    assert_eq!((keys, enters), (11, 1));
}

/// `await momentum` returns when the ribbon exhales and never polls: with the metric
/// at 0.40 and a floor of 0.30 the release law puts the crossing at
/// `2·ln(0.40/0.30)` = 575 ms, and the verb takes exactly two readings (the one it
/// solved from and the confirming one). If the human re-lights the ribbon during the
/// park, it solves the next crossing from that reading and takes a third; a ribbon
/// that never exhales answers `OK timeout` with one deadline read. (A dark ribbon
/// and the `<ms>` grammar: `the_wait_family_refuses_an_ms_that_is_not_one`.)
#[test]
fn await_momentum_returns_when_the_ribbon_exhales_and_never_polls() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    use control_session::cmd_await_momentum;

    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());

    // 1 — one crossing, two reads.
    let reads = Cell::new(0u32);
    let read = || {
        reads.set(reads.get() + 1);
        Ok((if reads.get() == 1 { 0.40 } else { 0.10 }, Instant::now()))
    };
    let t0 = Instant::now();
    let out = cmd_await_momentum(
        &h.term,
        &store,
        0,
        "momentum 0.30 timeout 60000",
        &subscribe::new_registry(),
        &read,
    );
    let waited = t0.elapsed();
    assert_eq!(out, "OK momentum 0.10\n");
    assert_eq!(reads.get(), 2, "solve, then confirm — no read in between");
    let crossing = Duration::from_secs_f32(2.0 * (0.40f32 / 0.30).ln());
    // Not before the crossing (load only makes a wake later); that it is
    // no tick is the read count above.
    assert!(
        waited >= crossing,
        "returned at the analytic crossing ({crossing:?}), not before: {waited:?}"
    );

    // 2 — the human re-lights the ribbon during the park: a third read.
    let reads = Cell::new(0u32);
    let read = || {
        reads.set(reads.get() + 1);
        let v = match reads.get() {
            1 => 0.40,
            2 => 0.35,
            _ => 0.05,
        };
        Ok((v, Instant::now()))
    };
    let t0 = Instant::now();
    let out = cmd_await_momentum(
        &h.term,
        &store,
        0,
        "momentum 0.30 timeout=60000",
        &subscribe::new_registry(),
        &read,
    );
    let waited = t0.elapsed();
    assert_eq!(out, "OK momentum 0.05\n");
    assert_eq!(reads.get(), 3);
    let second = Duration::from_secs_f32(2.0 * (0.35f32 / 0.30).ln());
    assert!(waited >= crossing + second, "two crossings: {waited:?}");

    // 3 — a ribbon that never exhales: the deadline, one deadline read.
    let reads = Cell::new(0u32);
    let read = || {
        reads.set(reads.get() + 1);
        Ok((0.90, Instant::now()))
    };
    let out = cmd_await_momentum(
        &h.term,
        &store,
        0,
        "momentum 0.30 timeout 120",
        &subscribe::new_registry(),
        &read,
    );
    assert_eq!(out, "OK timeout\n");
    assert_eq!(reads.get(), 2, "the solving read and the deadline's");
}

/// `turn yield=<floor>` is the same wait before typing: a turn against a lit ribbon
/// types nothing until the crossing and reports how long it yielded; one that
/// outlives its deadline types nothing and says so.
#[test]
fn a_yielding_turn_types_only_after_the_ribbon_exhales() {
    use std::cell::{Cell, RefCell};
    use std::time::{Duration, Instant};

    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let pasted_at: RefCell<Option<Instant>> = RefCell::new(None);
    let paste = |_: &str| {
        *pasted_at.borrow_mut() = Some(Instant::now());
        true
    };
    let press = |_: &str| true;
    let key = |_: crate::input::InputEvent| true;
    let reads = Cell::new(0u32);
    let momentum = || {
        reads.set(reads.get() + 1);
        Ok((if reads.get() == 1 { 0.40 } else { 0.0 }, Instant::now()))
    };
    let t0 = Instant::now();
    let out = cmd_turn(
        &h.term,
        &store,
        0,
        "yield=0.30 submit=none idle=20 timeout=60000 after you",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            key: &key,
            momentum: &momentum,
            driver: None,
            actor: aterm_slog::Actor::Ctl,
            cause: None,
            mark: None,
        },
    );
    let verdict = out.lines().next().unwrap_or("");
    let crossing = Duration::from_secs_f32(2.0 * (0.40f32 / 0.30).ln());
    let typed_after = pasted_at.borrow().expect("the turn typed") - t0;
    assert!(
        typed_after >= crossing,
        "typed only after the crossing: {typed_after:?}"
    );
    assert!(verdict.contains(" yielded_ms="), "{verdict}");

    let never = || Ok((0.9, Instant::now()));
    let untyped = |_: &str| panic!("a timed-out yield must not type");
    let out = cmd_turn(
        &h.term,
        &store,
        0,
        "yield=0.30 submit=none timeout=100 after you",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &untyped,
            press: &press,
            key: &key,
            momentum: &never,
            driver: None,
            actor: aterm_slog::Actor::Ctl,
            cause: None,
            mark: None,
        },
    );
    assert_eq!(out, "ERR yield timeout momentum=0.90\n");
}

/// `await gone <re>` answers `OK gone <seq>` as soon as no visible row matches: at
/// arm when already clear (level-triggered), else on the batch that removes the last
/// match, woken by the producer's notify (no poll). The token names the predicate,
/// distinct from `OK match`; a pattern that stays is `OK timeout`.
#[test]
fn await_gone_verb_latches_at_arm_when_clear_and_when_the_row_leaves() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"Done.\r\n\xe2\x9d\xaf ");
    store.write().unwrap().register(h.clone());
    let registry = subscribe::new_registry();

    // Already clear => latched at arm, answered as `gone` with the latch seq.
    let out = cmd_await(
        &h.term,
        &store,
        0,
        &h.ctx,
        "gone interrupt timeout=2000",
        &registry,
    );
    assert!(
        out.starts_with("OK gone "),
        "an already-clear surface latches at arm: {out:?}"
    );
    assert!(
        out.trim_end()["OK gone ".len()..].parse::<u64>().is_ok(),
        "the latch seq rides the reply: {out:?}"
    );

    // The footer goes up: the same wait holds to its deadline.
    crate::term_lock(&h.term).process(b"\r\nthinking (esc to interrupt)\r\n");
    let out = cmd_await(
        &h.term,
        &store,
        0,
        &h.ctx,
        "gone interrupt timeout=150",
        &registry,
    );
    assert_eq!(
        out, "OK timeout\n",
        "a visible match holds `gone` pending to the deadline"
    );

    // It latches on the clearing frame, woken by the notify the GUI's Wake::Output
    // hook fires. A lost notify would park to the two-minute timeout, where the
    // re-poll still latches, so the bound sits a minute clear of both.
    let (term_t, reg_t) = (h.term.clone(), registry.clone());
    let painter = std::thread::spawn(move || {
        // Painted once the await is PARKED — its subscriber registered — a
        // fact, where 120 ms let a slow await arm on the cleared frame and
        // latch at arm, never exercising the notify.
        aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the `gone` await parks on the session",
            || (reg_t.lock().unwrap().watchers(0) == 1).then_some(()),
        );
        crate::term_lock(&term_t).process(b"\x1b[H\x1b[2Jresponse-line\r\n\xe2\x9d\xaf ");
        reg_t.lock().unwrap().notify(0);
    });
    let t0 = std::time::Instant::now();
    let out = cmd_await(
        &h.term,
        &store,
        0,
        &h.ctx,
        "gone interrupt timeout=120000",
        &registry,
    );
    painter.join().unwrap();
    assert!(
        out.starts_with("OK gone "),
        "the footer leaving latches: {out:?}"
    );
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(60),
        "woken by the clearing frame, not by the timeout"
    );
}

/// `await gone` fails closed on its inputs like `match`: an invalid regex is
/// `ERR badregex`, and a `rows <a> <b>` span meeting no grid row (where `gone` would
/// be vacuously true) is `ERR bad rows` for both row predicates. A span reaching past
/// the grid is intersected with it.
#[test]
fn await_gone_refuses_a_bad_regex_and_a_row_span_off_the_grid() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"esc to interrupt\r\n");
    store.write().unwrap().register(h.clone());
    let registry = subscribe::new_registry();
    assert_eq!(
        cmd_await(
            &h.term,
            &store,
            0,
            &h.ctx,
            "gone (unclosed timeout=100",
            &registry
        ),
        "ERR badregex\n"
    );
    // A 24-row grid: rows 40..=50 meet nothing, rows 5..=2 is inverted.
    for args in [
        "gone interrupt rows 40 50",
        "gone interrupt rows 5 2",
        "match interrupt rows 40 50",
    ] {
        let out = cmd_await(
            &h.term,
            &store,
            0,
            &h.ctx,
            &format!("{args} timeout=100"),
            &registry,
        );
        assert!(
            out.starts_with("ERR bad rows"),
            "{args} meets no visible row: {out:?}"
        );
    }
    // Row 23 is the grid's last row; 23..=99 covers it (and only it), and it
    // is clear of the footer sitting on row 0.
    let out = cmd_await(
        &h.term,
        &store,
        0,
        &h.ctx,
        "gone interrupt rows 23 99 timeout=100",
        &registry,
    );
    assert!(
        out.starts_with("OK gone "),
        "rows 23..=99 is intersected with the grid, and row 23 is clear: {out:?}"
    );
}

/// `turn settle=gone:<re>`, one row per footer timing. The press paints one frame
/// and hands a painter thread a script of (sleep ms, bytes) frames, each followed by
/// a notify; each row uses a fresh session.
#[test]
fn turn_settle_gone_rows() {
    use std::cell::Cell;
    struct Row {
        label: &'static str,
        press_paint: &'static [u8],
        script: &'static [(u64, &'static [u8])],
        options: &'static str,
        must: &'static [&'static str],
        must_not: &'static [&'static str],
    }
    const RESPONSE: &[u8] = b"\x1b[H\x1b[2Jresponse-line\r\n\xe2\x9d\xaf ";
    let rows = [
        // The post-response screen even when the busy footer lands a frame after
        // the submit verified (input line cleared, no footer yet): the footer paints
        // 100 ms later, the response and its departure 300 ms after that. Settle
        // must wait for the footer to appear and then leave.
        Row {
            label: "a late footer appears and then leaves",
            press_paint: b"\r\x1b[2Ksubmitted\r\n",
            script: &[(100, b"thinking (esc to interrupt)\r\n"), (300, RESPONSE)],
            options: "settle=gone:interrupt submit_window=60000 idle=50 timeout=60000 do it",
            must: &["response-line"],
            must_not: &["esc to interrupt"],
        },
        // Common case: the footer is on screen when settle arms (painted in the
        // verifying frame), so phase 3a latches at arm and the turn settles when
        // the footer goes.
        Row {
            label: "a visible footer leaves",
            press_paint: b"\r\nthinking (esc to interrupt)\r\n",
            script: &[(200, RESPONSE)],
            options: "settle=gone:interrupt idle=50 timeout=60000 do it",
            must: &["response-line"],
            must_not: &["esc to interrupt"],
        },
    ];
    for row in rows {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let registry = subscribe::new_registry();
        let term = &h.term;
        let painter: Cell<Option<std::thread::JoinHandle<()>>> = Cell::new(None);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            term.lock().unwrap().process(row.press_paint);
            let (term_t, reg_t, script) = (term.clone(), registry.clone(), row.script);
            painter.set(Some(std::thread::spawn(move || {
                for &(ms, bytes) in script {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    crate::term_lock(&term_t).process(bytes);
                    reg_t.lock().unwrap().notify(0);
                }
            })));
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            row.options,
            &registry,
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        if let Some(j) = painter.take() {
            j.join().unwrap();
        }
        let verdict = out.lines().next().unwrap_or("");
        assert!(
            verdict.contains("submitted=1 status=settled"),
            "{}: {verdict}",
            row.label
        );
        for needle in row.must {
            assert!(
                out.contains(needle),
                "{}: missing {needle:?}: {out}",
                row.label
            );
        }
        for needle in row.must_not {
            assert!(
                !out.contains(needle),
                "{}: still shows {needle:?}: {out}",
                row.label
            );
        }
    }
}

/// `turn`'s submit verification, one row per case. Each row builds a fresh session,
/// optionally seeds OSC 133 state, pastes with echo, and scripts what each Enter
/// paints (by press number; the last entry repeats, and an empty paint is a
/// swallowed press). A press paints synchronously, so a row whose first press
/// does not land waits a short `submit_window`, not the 2 s default, before it
/// presses again.
#[test]
fn turn_submit_verification_rows() {
    use std::cell::Cell;
    struct Row {
        label: &'static str,
        seed: &'static [u8],
        paints: &'static [&'static [u8]],
        options: &'static str,
        verdict: &'static str,
        presses: u32,
        why: &'static str,
    }
    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";
    let rows = [
        // Verified-submit retry: an Enter swallowed mid-paste-ingestion does not
        // advance `content_seq`, so the verb re-presses and the second lands. The
        // press count shows verification, not a timer, drove the retry.
        Row {
            label: "swallowed first Enter",
            seed: b"",
            paints: &[b"", b"\r\nlanded\r\n"],
            options: "idle=50 timeout=60000 submit_window=150 msg",
            verdict: "submitted=1 status=settled",
            presses: 2,
            why: "exactly one re-press after the swallow",
        },
        // At a shell prompt (133;A opens the block, 133;B marks input-ready) `turn`
        // auto-verifies against the OSC-133 command start with no `submit_verify=`,
        // and the command start (133;C -> Executing) attributes the press at once.
        Row {
            label: "auto block-verifies at a shell prompt",
            seed: PROMPT,
            paints: &[b"echo\x1b]133;C\x07\r\nrunning"],
            options: "idle=50 timeout=60000 build",
            verdict: "submitted=1",
            presses: 1,
            why: "one press started the command — no re-press",
        },
        // Auto degrade: the target looks like a shell prompt but the 133 stream is
        // desynced, so no press ever produces a command start (e.g. vte.sh
        // double-sourced clobbering PS0's `133;C`), while the echo advances
        // `content_seq`. Auto must not blindly re-press (each Enter is real input)
        // or report a false `submitted=0 status=timeout`; it degrades to the seq
        // verdict. The small submit_window makes the degrade arrive fast.
        Row {
            label: "auto degrades when the prompt block is stale",
            seed: PROMPT,
            paints: &[b"\r\nran-anyway\r\n$ "],
            options: "idle=50 timeout=60000 submit_window=120 run",
            verdict: "submitted=1 status=settled",
            presses: 1,
            why: "a press whose echo moved the screen was consumed — re-pressing would double-type",
        },
        // The degrade must not fire on a proven-healthy stream: once this session
        // started a command block (133;C seen), no block plus ambient movement looks
        // like a swallowed Enter beside background output. The strict lane
        // re-presses and reports `submitted=0` when nothing starts.
        Row {
            label: "auto stays strict once the stream has started blocks",
            seed: b"\x1b]133;A\x07$ \x1b]133;B\x07echo\x1b]133;C\x07\r\nout\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07",
            paints: &[b"\r\n[bg] tick"],
            options: "idle=50 timeout=60000 submit_window=120 presses=2 run",
            verdict: "submitted=0",
            presses: 2,
            why: "strictness holds — the press budget retries instead of overclaiming",
        },
        // `submit_verify=block` verifies against the OSC-133 command start, not a
        // bare `content_seq` advance, so an ambient repaint (a spinner frame) cannot
        // false-verify; `seq` mode would on the first press.
        Row {
            label: "submit_verify=block ignores an ambient repaint",
            seed: PROMPT,
            paints: &[b"\x1b[2K spinner-tick ", b"echo\x1b]133;C\x07\r\ndone"],
            options: "idle=50 timeout=60000 submit_window=150 submit_verify=block run",
            verdict: "submitted=1",
            presses: 2,
            why: "the ambient repaint did NOT verify; a second press started the block",
        },
    ];
    for row in rows {
        let store = session_store::new_store();
        let h = registered_session(0, -1, b"");
        store.write().unwrap().register(h.clone());
        let term = &h.term;
        if !row.seed.is_empty() {
            term.lock().unwrap().process(row.seed);
        }
        let presses = Cell::new(0u32);
        let paste = |text: &str| {
            term.lock().unwrap().process(text.as_bytes());
            true
        };
        let press = |_: &str| {
            presses.set(presses.get() + 1);
            let at = (presses.get() as usize - 1).min(row.paints.len() - 1);
            let paint = row.paints[at];
            if !paint.is_empty() {
                term.lock().unwrap().process(paint);
            }
            true
        };
        let out = cmd_turn(
            term,
            &store,
            0,
            row.options,
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        );
        assert!(
            out.contains(row.verdict),
            "{}: {}",
            row.label,
            out.lines().next().unwrap_or("")
        );
        assert_eq!(presses.get(), row.presses, "{}: {}", row.label, row.why);
    }
}

/// The turn lease: a second driver colliding with an in-flight turn is refused with
/// the holder's id. (A completed turn's `id=` and its released lease:
/// `turn_types_submits_verified_and_returns_settled_screen`.)
#[test]
fn turn_lease_refuses_concurrent_writers() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    // A held lease (as if another connection's turn were mid-flight) refuses
    // a new turn, naming the holder.
    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
        id: 41,
        driver: None,
        typing: true,
    });
    let paste = |_: &str| true;
    let press = |_: &str| true;
    let out = cmd_turn(
        &h.term,
        &store,
        0,
        "timeout=2000 hi",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    assert_eq!(
        out, "ERR busy turn=41\n",
        "held lease refuses a second turn"
    );
}

/// Cooperative lease: `lease acquire` grants a holder-named, TTL'd hold that
/// `lease status` reports and `lease release` (holder-matched) drops. Another
/// holder's acquire is refused while live; the same holder renews.
#[test]
fn lease_acquire_status_release_and_holder_exclusion() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());

    // Idle: status is `none`.
    assert_eq!(cmd_lease(&h.ctx, "status"), "OK lease none\n");
    // Bare `lease` == `lease status`.
    assert_eq!(cmd_lease(&h.ctx, ""), "OK lease none\n");

    // Acquire with an explicit holder + ttl; status now reports that holder.
    let out = cmd_lease(&h.ctx, "acquire ttl=60000 holder=agent-a");
    assert!(
        out.contains("holder=agent-a") && out.contains("ttl_ms=60000"),
        "{out}"
    );
    assert!(
        cmd_lease(&h.ctx, "status").contains("holder=agent-a"),
        "status shows holder"
    );

    // A DIFFERENT holder is refused while it is live.
    let denied = cmd_lease(&h.ctx, "acquire holder=agent-b");
    assert!(
        denied.starts_with("ERR lease held holder=agent-a"),
        "{denied}"
    );
    // The SAME holder renews (not refused).
    assert!(cmd_lease(&h.ctx, "acquire holder=agent-a").starts_with("OK lease acquired"));

    // Wrong holder cannot release; the right holder can.
    assert!(cmd_lease(&h.ctx, "release holder=agent-b").starts_with("ERR lease held by agent-a"));
    assert_eq!(
        cmd_lease(&h.ctx, "release holder=agent-a"),
        "OK lease released\n"
    );
    assert_eq!(cmd_lease(&h.ctx, "status"), "OK lease none\n");

    // `force` steals any held cooperative lease.
    assert!(cmd_lease(&h.ctx, "acquire holder=agent-c").starts_with("OK lease acquired"));
    assert_eq!(cmd_lease(&h.ctx, "release force"), "OK lease released\n");
}

/// A live cooperative lease blocks a `turn` (`ERR busy lease=<holder>`) but is
/// advisory for raw writes (`send`/`key`/`feed`), which only the `turn` lease
/// governs (the write-seam check ignores `Drive` leases).
#[test]
fn cooperative_lease_blocks_turn_but_not_raw_writes() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;

    assert!(cmd_lease(&h.ctx, "acquire holder=agent-a ttl=60000").starts_with("OK lease"));

    // A `turn` is HARD arbitration — it refuses to stomp a live cooperative hold.
    let paste = |_: &str| true;
    let press = |_: &str| true;
    let out = cmd_turn(
        term,
        &store,
        0,
        "timeout=2000 hi",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    assert_eq!(
        out, "ERR busy lease=agent-a\n",
        "turn refuses a live cooperative lease"
    );

    // The cooperative lease does not block a raw write: the write-arbitration
    // predicate the seam uses yields no block for a `Drive` lease, from any
    // connection.
    let held = h.ctx.turn_lease.lock().unwrap();
    for conn in [None, Some(1), Some(2)] {
        assert_eq!(
            held.as_ref()
                .and_then(|l| l.write_block(crate::metrics::now_us(), conn)),
            None,
            "a cooperative lease never hard-blocks a raw write"
        );
    }
}

/// The hard hold: `lease acquire … hard` refuses every other connection's write
/// verbs (and `turn`) `ERR busy lease=<holder>` on the same write seam a `turn`
/// holds, while its own connection writes on; its own `turn` holds the slot for the
/// turn and hands the lease back as the turn ends (e.g. a relaunch line typed under
/// a restart's hold, with no gap). A renewal moves it to the renewing connection.
/// Negative controls: a lapsed hard lease blocks nothing, reads as `none` and is
/// stealable by a new acquire, and `lease status` says `hard=1` only of a hard one.
#[test]
fn a_hard_lease_holds_every_other_connection_and_its_own_types_through() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let on = |conn: u64| SERVING_CONNECTION.with(|s| s.set(Some(conn)));
    on(7);
    let got = cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard");
    assert!(got.ends_with(" hard=1\n"), "{got}");
    assert!(cmd_lease(&h.ctx, "status").contains(" hard=1"));
    let block = |conn: Option<u64>| {
        h.ctx
            .turn_lease
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|l| l.write_block(crate::metrics::now_us(), conn))
    };
    assert_eq!(block(Some(8)).as_deref(), Some("lease=aterm-harness@1"));
    assert_eq!(block(None).as_deref(), Some("lease=aterm-harness@1"));
    assert_eq!(
        block(Some(7)),
        None,
        "the holder's own connection writes on"
    );
    let paste = |_: &str| true;
    let press = |_: &str| true;
    let io = TurnIo {
        paste: &paste,
        press: &press,
        ..TurnIo::paste_only()
    };
    let turn = || {
        cmd_turn(
            term,
            &store,
            0,
            "timeout=300 idle=1 hi",
            &subscribe::new_registry(),
            &h.ctx,
            &io,
        )
    };
    on(8);
    assert_eq!(turn(), "ERR busy lease=aterm-harness@1\n", "another's turn");
    on(7);
    let own = turn();
    assert!(!own.starts_with("ERR busy"), "the holder's own turn: {own}");
    let after = h.ctx.turn_lease.lock().unwrap().clone();
    assert!(
        matches!(
            &after,
            Some(crate::Lease::Drive { holder, conn: Some(7), hard: true, .. })
                if holder == "aterm-harness@1"
        ),
        "handed back as the turn ends: {after:?}"
    );
    // A renewal from another connection moves the hold there.
    on(9);
    assert!(
        cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000 hard")
            .starts_with("OK lease acquired")
    );
    assert_eq!(block(Some(9)), None);
    assert_eq!(block(Some(7)).as_deref(), Some("lease=aterm-harness@1"));
    // Negative controls: cooperative says no `hard=1`; lapsed blocks nothing.
    assert!(!cmd_lease(&h.ctx, "acquire holder=aterm-harness@1 ttl=60000").contains("hard"));
    assert!(!cmd_lease(&h.ctx, "status").contains("hard"));
    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
        holder: "stale".to_string(),
        expires_us: crate::metrics::now_us().saturating_sub(1),
        conn: Some(7),
        hard: true,
        holder_pid: None,
    });
    assert_eq!(block(Some(8)), None, "a lapsed hard lease holds nothing");
    assert_eq!(
        cmd_lease(&h.ctx, "status"),
        "OK lease none\n",
        "lapsed reads as none"
    );
    assert!(cmd_lease(&h.ctx, "acquire holder=fresh").starts_with("OK lease acquired"));
    SERVING_CONNECTION.with(|s| s.set(None));
}

/// A FORCED preemption of a DRIVEN turn wakes the driving session too
/// (`DriverGeometry`'s `EveryMovedLeaseOwesAWake`): the driver's band reads
/// the peer's lease (`▸ @<sid>`), and the wedged turn's own guard finds the
/// slot taken or empty when it drops and posts nothing — so without the
/// driver's wake its window's row holds on a lease mark that has moved. A
/// refused release posts nothing, and an undriven turn wakes only itself.
#[test]
fn force_preempting_a_driven_turn_wakes_its_driver_too() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let driver = aterm_session::SessionId::generate();
    let own = h.ctx.self_id.clone();
    let _ = crate::presence::take_lease_wakes();

    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
        id: 78,
        driver: Some(driver.clone()),
        typing: true,
    });
    let refused = cmd_lease(&h.ctx, "release");
    assert!(refused.starts_with("ERR busy turn=78"), "{refused}");
    assert_eq!(
        crate::presence::take_lease_wakes(),
        [],
        "a refused release moves no lease and wakes nobody"
    );

    assert_eq!(cmd_lease(&h.ctx, "release force"), "OK lease released\n");
    assert_eq!(
        crate::presence::take_lease_wakes(),
        [(own.clone(), false), (driver, false)],
        "the preempted turn's session AND its driving session are woken"
    );

    // An undriven turn: its own session alone.
    *h.ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
        id: 79,
        driver: None,
        typing: true,
    });
    assert_eq!(cmd_lease(&h.ctx, "release force"), "OK lease released\n");
    assert_eq!(crate::presence::take_lease_wakes(), [(own, false)]);
}

/// `turn` on a dead session fails closed with `ERR exited`, and a bad submit key
/// name is a usage error, both before any settle wait.
#[test]
fn turn_fails_closed_on_exit_and_bad_submit_key() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;

    let paste = |_: &str| true;
    let bad_press = |_: &str| false; // parse_key rejected the name
    let out = cmd_turn(
        term,
        &store,
        0,
        "submit=bogus-key timeout=3000 hi",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &bad_press,
            ..TurnIo::paste_only()
        },
    );
    assert!(
        out.starts_with("ERR usage: turn"),
        "bad key is usage: {out}"
    );

    store
        .write()
        .unwrap()
        .set_state(0, session_store::SessionState::Exited);
    let press = |_: &str| true;
    let out = cmd_turn(
        term,
        &store,
        0,
        "timeout=3000 hi",
        &subscribe::new_registry(),
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            ..TurnIo::paste_only()
        },
    );
    assert_eq!(out, "ERR exited\n", "exited session fails closed");
}

/// The accept loop backs off on exhaustion: `EMFILE` returns at once on every retry,
/// so it waits, doubling to a cap; a per-connection error (peer reset before
/// accept) is retried at once.
#[cfg(unix)]
#[test]
fn the_accept_loop_backs_off_on_descriptor_exhaustion_only() {
    let emfile = std::io::Error::from_raw_os_error(libc::EMFILE);
    let mut wait = None;
    let mut seen = Vec::new();
    for _ in 0..10 {
        wait = accept_error_backoff(&emfile, wait);
        seen.push(wait.expect("exhaustion backs off"));
    }
    assert!(seen[1] > seen[0], "the wait grows: {seen:?}");
    assert_eq!(seen[8], seen[9], "and is capped: {seen:?}");
    for code in [libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
        let e = std::io::Error::from_raw_os_error(code);
        assert!(accept_error_backoff(&e, None).is_some(), "{e}");
    }
    for code in [libc::ECONNABORTED, libc::EINTR] {
        let e = std::io::Error::from_raw_os_error(code);
        assert_eq!(accept_error_backoff(&e, None), None, "{e}");
    }
    assert_eq!(
        accept_error_backoff(&std::io::Error::other("no os code"), None),
        None
    );
}

/// `fabric status` / `fabric attach` arm the supervisor of a running instance
/// without a relaunch. Drives the same decision function both dispatch entries
/// call, against the real launcher with an argv that exits at once (`true`), enough
/// to show the supervisor thread started and the endpoint latch flipped. `post`'s
/// refusal is the observable (`no-bridge=1` before, `queued=1` after), since that
/// token is what an agent acts on.
///
/// And it arms only what the instance's config names: an argv of the caller's
/// own, with none configured or another configured, is refused and arms
/// nothing — the Owner token every in-session client holds does not choose the
/// process that holds `Scope::Bridge`. What aterm.toml names is the test's to
/// say (`fabric_launch::set_command_on_disk_for_tests`), never the developer's
/// own file.
///
/// The armed thread outlives the test (no stop handle); with no `BRIDGE_CONTEXT`
/// published in a test process it serves nothing and only re-spawns `true` under
/// the launcher's back-off.
#[test]
fn fabric_attach_arms_the_supervisor_of_a_running_instance_once_and_for_owner_only() {
    crate::fabric::with_link_reset(fabric_attach_body);
}

#[cfg(unix)]
fn fabric_attach_body() {
    let store = session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    let ctx = handle.ctx.clone();
    store.write().unwrap().register(handle);
    let edge = Scope::Edge(EdgeToken::generate());
    let run = |scope: Scope, line: &str| {
        let (selector, verb, rest) = request_head(line);
        assert_eq!(verb, "fabric");
        dispatch_fabric_verb(rest, selector.as_ref(), scope, &store)
    };
    let post = || crate::fabric::cmd_post(&ctx, "to=@s-peer kind=ask --wait=0 on?", None);
    // What `fabric status` says about a bridge that has not attached.
    let unattached = format!(
        "build={} bridge_lane=- bridge_build=- lane_faults=0",
        aterm_types::version::build_id()
    );

    // A fresh instance: nothing armed or configured, and `post` says nothing will
    // drain the outbox.
    assert_eq!(
        run(Scope::Owner, "fabric status"),
        format!(
            "OK state=absent supervised=0 command=- reason=- rtt_ms=- link_age_ms=- {unattached}\n"
        )
    );
    assert_eq!(post(), "ERR fabric absent id=1 no-bridge=1\n");
    assert_eq!(
        run(Scope::Owner, "fabric attach"),
        "ERR fabric no command\n",
        "no argv and no launch config"
    );
    // An argv of the caller's choosing, with nothing configured: refused, and
    // nothing armed.
    assert_eq!(
        run(Scope::Owner, "fabric attach /usr/bin/true --fleet lab"),
        format!(
            "ERR fabric not configured command={}\n",
            pct_encode("/usr/bin/true --fleet lab")
        )
    );
    assert!(!crate::fabric::bridge_supervised(), "nothing armed");
    assert_eq!(run(Scope::Owner, "fabric"), FABRIC_USAGE);
    assert_eq!(run(Scope::Owner, "fabric status extra"), FABRIC_USAGE);

    // The gate: an edge is denied both forms, as is the bridge connection (the
    // process the supervisor runs, not a party to arming it), and a selector is
    // refused even from Owner, as for every owner-only, session-independent verb.
    for line in ["fabric status", "fabric attach /usr/bin/true"] {
        assert_eq!(run(edge, line), "ERR denied\n", "{line} from an edge");
        assert_eq!(
            run(Scope::Bridge, line),
            "ERR denied\n",
            "{line} from the bridge connection"
        );
        assert_eq!(
            run(Scope::Owner, &format!("@s-other {line}")),
            "ERR denied\n",
            "{line} with a selector"
        );
    }
    assert!(
        !crate::fabric::bridge_supervised(),
        "a refused attach arms nothing"
    );

    // Pre-flight: a program that cannot run is refused before the latch closes
    // (reply names it, nothing armed, `post`'s advice unchanged), so a typo in
    // aterm.toml does not wedge the one attach slot.
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    crate::fabric_launch::set_command_on_disk_for_tests(Some(words(
        "/nonexistent/aterm-link serve",
    )));
    assert_eq!(
        run(Scope::Owner, "fabric attach"),
        format!(
            "ERR fabric not executable program={} reason=not-found\n",
            pct_encode("/nonexistent/aterm-link")
        )
    );
    assert_eq!(
        run(Scope::Owner, "fabric status"),
        format!(
            "OK state=absent supervised=0 command=- reason=- rtt_ms=- link_age_ms=- {unattached}\n"
        ),
        "a refused attach records nothing"
    );
    assert_eq!(post(), "ERR fabric absent id=2 no-bridge=1\n");

    // ATTACH, with a tiny fake bridge: a program that exits at once.
    let program = ["/usr/bin/true", "/bin/true"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("a `true` binary");
    let command = pct_encode(&format!("{program} --fleet lab"));
    crate::fabric_launch::set_command_on_disk_for_tests(Some(words(&format!(
        "{program} --fleet lab"
    ))));
    // Another program than the one configured is still not the caller's to
    // choose...
    assert_eq!(
        run(Scope::Owner, "fabric attach /usr/bin/false --fleet lab"),
        format!(
            "ERR fabric not configured command={}\n",
            pct_encode("/usr/bin/false --fleet lab")
        )
    );
    assert!(!crate::fabric::bridge_supervised(), "nothing armed");
    // ...and the configured one, named as `aterm fabric on` names it, arms.
    assert_eq!(
        run(
            Scope::Owner,
            &format!("fabric attach {program} --fleet lab")
        ),
        format!("OK attached command={command}\n")
    );
    assert_eq!(
        run(Scope::Owner, "fabric status"),
        format!(
            "OK state=absent supervised=1 command={command} reason=- rtt_ms=- link_age_ms=- {unattached}\n"
        ),
        "the bridge connects asynchronously; the SUPERVISOR is what attached"
    );
    // ...and the endpoint's advice flipped at the same moment.
    assert_eq!(post(), "ERR fabric absent id=3 queued=1\n");

    // Once per process: a second attach, bare form included, is refused, names
    // what runs and applies nothing.
    assert_eq!(
        run(Scope::Owner, "fabric attach /usr/bin/false"),
        format!("ERR fabric already supervised command={command}\n")
    );
    assert_eq!(
        run(Scope::Owner, "fabric attach"),
        format!("ERR fabric already supervised command={command}\n")
    );
    assert_eq!(
        run(Scope::Owner, "fabric status"),
        format!(
            "OK state=absent supervised=1 command={command} reason=- rtt_ms=- link_age_ms=- {unattached}\n"
        )
    );
}

#[cfg(not(unix))]
fn fabric_attach_body() {
    let store = session_store::new_store();
    assert_eq!(
        dispatch_fabric_verb("status", None, Scope::Owner, &store),
        format!(
            "OK state=absent supervised=0 command=- reason=- rtt_ms=- link_age_ms=- build={} \
             bridge_lane=- bridge_build=- lane_faults=0\n",
            aterm_types::version::build_id()
        )
    );
    assert_eq!(
        dispatch_fabric_verb("attach x", None, Scope::Owner, &store),
        "ERR fabric unavailable on this platform\n"
    );
}

/// `deliver`, `outbox` and `outbox sent` are refused to every scope but the bridge
/// connection, Owner included: `aterm-ctl @self` is Owner, so Owner access would put
/// forging an attested `from=` within reach of one prompt injection. Drives the
/// dispatch's decision function. `hold` is not a bridge verb; its fleet-hold rule
/// is pinned by
/// `the_local_owner_may_hold_its_session_and_never_a_fleet_hold_inbox_hold`.
#[test]
fn the_bridge_verbs_refuse_every_scope_but_the_bridge_inbox_hold() {
    // `deliver` marks the session bridge-governed in the process-wide link, so
    // this takes the shared reset guard (`fabric::with_link_reset`).
    crate::fabric::with_link_reset(the_bridge_verbs_refuse_every_scope_but_the_bridge_body);
}

fn the_bridge_verbs_refuse_every_scope_but_the_bridge_body() {
    let store = session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    let sid = handle.sid.as_str().to_string();
    store.write().unwrap().register(handle);
    let edge = Scope::Edge(EdgeToken::generate());
    let run = |scope: Scope, line: &str| {
        let (selector, verb, rest) = request_head(line);
        dispatch_bridge_verb(verb, rest, selector.as_ref(), scope, &store)
    };

    for (scope, name) in [(Scope::Owner, "owner"), (edge, "edge")] {
        assert_eq!(
            run(
                scope,
                &format!("deliver {sid} off=1 from=h-a kind=task text=x")
            )
            .as_deref(),
            Some("ERR denied\n"),
            "deliver from {name} scope"
        );
        // `hold` is not on this plane any more: from EVERY scope it falls
        // through to the owner-only dispatch, which is where its gate lives.
        assert!(
            run(scope, &format!("hold {sid} on reason=x")).is_none(),
            "hold from {name} scope is not a bridge-plane verb"
        );
        // The outbound plane is the same authority: an Owner reading `outbox` would
        // read every session's outbound traffic, and forging `outbox sent` would
        // release a `post --wait` for a message that never left.
        assert_eq!(
            run(scope, "outbox").as_deref(),
            Some("ERR denied\n"),
            "outbox from {name} scope"
        );
        assert_eq!(
            run(scope, &format!("outbox sent {sid} 1 off=9")).as_deref(),
            Some("ERR denied\n"),
            "outbox sent from {name} scope"
        );
    }
    // From the inherited bridge connection, both land.
    assert_eq!(
        run(
            Scope::Bridge,
            &format!("deliver {sid} off=1 from=h-a kind=task trust=human text=x")
        )
        .as_deref(),
        Some("OK 1\n")
    );
    assert!(
        run(Scope::Bridge, &format!("hold {sid} on reason=x")).is_none(),
        "hold is owner-class now, from the bridge too"
    );
    assert_eq!(
        run(Scope::Bridge, "outbox").as_deref(),
        Some("OK 0\n"),
        "an empty outbox is a zero-length frame, not a refusal"
    );
    assert_eq!(
        run(Scope::Bridge, &format!("outbox sent {sid} 1 off=9")).as_deref(),
        Some("ERR no such post\n"),
        "reached the handler, which is the point"
    );
    // A SELECTOR is refused even from the bridge: `@<other> deliver <sid> …`
    // would otherwise aim the bridge plane at a session the sid does not name.
    assert_eq!(
        run(
            Scope::Bridge,
            &format!("@1 deliver {sid} off=2 from=h-a kind=task text=x")
        )
        .as_deref(),
        Some("ERR denied\n")
    );
    // Every BridgeOnly row of the table reaches a handler (bare, so each answers
    // its usage or an empty read, never the `no_handler` refusal).
    for spec in aterm_types::control_verbs::VERBS {
        if spec.access == aterm_types::control_verbs::Access::BridgeOnly {
            assert_ne!(
                run(Scope::Bridge, spec.name),
                Some(no_handler(spec.name)),
                "bridge-only {:?} has no dispatch arm",
                spec.name
            );
        }
    }
    // Everything else falls through untouched.
    assert!(run(Scope::Bridge, "text").is_none());
    assert!(run(Scope::Owner, "inbox").is_none());
}

/// THE BRIDGE NAMES ITS LANE AND BUILD, AND THE INSTANCE REPORTS BOTH. A
/// bridge's `hello` is recorded for the incarnation that owns the link and
/// answered with this instance's own lane and build; `fabric status` then
/// prints the two builds side by side, so a bridge of another build than its
/// instance is named rather than inferred. A `hello` from a ghost lane records
/// nothing, malformed input is refused, and every bridge-plane line this
/// endpoint cannot parse — here a `deliver` carrying a field from a later lane
/// — is counted as a lane fault for the incarnation that sent it.
#[test]
fn a_bridges_hello_and_its_unparsed_lines_are_reported_for_its_incarnation() {
    crate::fabric::with_link_reset(a_bridges_hello_and_its_unparsed_lines_body);
}

fn a_bridges_hello_and_its_unparsed_lines_body() {
    let store = session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    let sid = handle.sid.as_str().to_string();
    store.write().unwrap().register(handle);
    let run = |line: &str| {
        let (selector, verb, rest) = request_head(line);
        dispatch_bridge_verb(verb, rest, selector.as_ref(), Scope::Bridge, &store)
            .expect("a bridge-plane verb")
    };
    let own = aterm_types::version::build_id();
    let lane = aterm_types::control_verbs::BRIDGE_LANE;
    let ghost = crate::fabric::next_bridge_generation();
    let generation = crate::fabric::next_bridge_generation();
    crate::fabric::bridge_attached(generation);
    let tail = |bridge_lane: &str, bridge_build: &str, faults: u64| {
        format!(
            "build={own} bridge_lane={bridge_lane} bridge_build={bridge_build} \
             lane_faults={faults}\n"
        )
    };
    assert!(
        fabric_status_line().ends_with(&tail("-", "-", 0)),
        "a fresh attach has said nothing: {}",
        fabric_status_line()
    );

    crate::fabric::set_lane_generation(ghost);
    assert_eq!(
        run("hello lane=1 build=9.9.0+gghost"),
        format!("OK stale=1 lane={lane} build={own}\n")
    );
    crate::fabric::set_lane_generation(generation);
    assert!(fabric_status_line().ends_with(&tail("-", "-", 0)));
    assert_eq!(
        run("hello lane=1 build=9.9.0+g0123456789ab later=1"),
        format!("OK lane={lane} build={own}\n")
    );
    assert!(
        fabric_status_line().ends_with(&tail("1", "9.9.0+g0123456789ab", 0)),
        "the bridge's build beside the instance's: {}",
        fabric_status_line()
    );

    // Input is checked: each is refused, and each refusal is a line this
    // endpoint could not parse.
    for bad in [
        "hello",
        "hello build=x",
        "hello lane=one",
        "hello lane=1 stray",
        "hello lane=1 build=a%20b",
    ] {
        assert_eq!(
            run(bad),
            "ERR usage: hello lane=<n> [build=<token>]\n",
            "{bad}"
        );
    }
    assert!(fabric_status_line().ends_with(&tail("1", "9.9.0+g0123456789ab", 5)));
    // A row carrying a field this endpoint does not parse is a lane fault too;
    // a recipient's verdict (`no such session`) is not one.
    assert!(
        run(&format!(
            "deliver {sid} off=1 from=h-a kind=note trust=human shiny=1 text=x"
        ))
        .starts_with("ERR usage: deliver")
    );
    assert_eq!(
        run("deliver s-nobody off=2 from=h-a kind=note text=x"),
        "ERR no such session\n"
    );
    assert!(fabric_status_line().ends_with(&tail("1", "9.9.0+g0123456789ab", 6)));

    // A new incarnation starts over.
    let next = crate::fabric::next_bridge_generation();
    crate::fabric::bridge_attached(next);
    assert!(fabric_status_line().ends_with(&tail("-", "-", 0)));
}

/// THE BUS MIRROR OFF TAKES NO LOCK OF ITS OWN (session log phase 4, ruling
/// Q5): on a default instance — its bridge attached and its hello recorded
/// without `log=turns`, so nothing is engaged or retained — `fabric status`,
/// the recorded `hello`, a replaced bus log (`link log` `forgot=1`), an
/// update's draw, its Commit's reading and its stand-down each answer while
/// another thread holds the mirror's tails' lock, and say what they said
/// before the mirror was built. Each runs on its own thread, so every path
/// that would wait is named; the wait is one one-minute hang detector.
#[test]
fn the_mirror_off_answers_with_the_mirrors_lock_held() {
    crate::fabric::with_link_reset(the_mirror_off_answers_with_the_mirrors_lock_held_body);
}

fn the_mirror_off_answers_with_the_mirrors_lock_held_body() {
    type Path = Box<dyn FnOnce(&crate::session_store::Store) -> String + Send>;
    let store = session_store::new_store();
    store
        .write()
        .unwrap()
        .register(crate::session_store::test_handle(1));
    let generation = crate::fabric::next_bridge_generation();
    crate::fabric::bridge_attached(generation);
    let paths: [(&'static str, Path); 6] = [
        (
            "hello",
            Box::new(move |store| {
                crate::fabric::set_lane_generation(generation);
                dispatch_bridge_verb("hello", "lane=2 build=x", None, Scope::Bridge, store)
                    .unwrap_or_default()
            }),
        ),
        (
            "fabric status",
            Box::new(|store| dispatch_fabric_verb("status", None, Scope::Owner, store)),
        ),
        (
            "forgot=1",
            Box::new(|store| {
                crate::session_log_mirror::forget_landed(store);
                String::new()
            }),
        ),
        (
            "draw",
            Box::new(|store| {
                crate::session_store::SessionHandoff::from_store(&store.read().unwrap())
                    .mirror_dropped
                    .join(" ")
            }),
        ),
        (
            "commit's reading",
            Box::new(|store| store.read().unwrap().mirror().past_draw().to_string()),
        ),
        (
            "stand-down",
            Box::new(|store| {
                store.read().unwrap().mirror().lift_commit_fence();
                String::new()
            }),
        ),
    ];
    let names: Vec<&'static str> = paths.iter().map(|(name, _)| *name).collect();
    let guard = store.read().unwrap();
    let held = guard.mirror().hold_retired();
    let (tx, rx) = std::sync::mpsc::channel();
    for (name, path) in paths {
        let (store, tx) = (store.clone(), tx.clone());
        std::thread::spawn(move || {
            let answer = path(&store);
            let _ = tx.send((name, answer));
        });
    }
    drop(tx);
    let deadline = std::time::Instant::now() + EVENTUALLY;
    let mut answers = std::collections::BTreeMap::new();
    while answers.len() < names.len() {
        match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok((name, answer)) => {
                answers.insert(name, answer);
            }
            Err(_) => break,
        }
    }
    drop(held);
    drop(guard);
    let waited: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| !answers.contains_key(name))
        .collect();
    assert!(
        waited.is_empty(),
        "a default instance waited on the mirror's lock: {waited:?}"
    );
    let hello = &answers["hello"];
    assert!(
        hello.starts_with("OK lane=") && !hello.contains("log="),
        "recorded, the mirror not asked: {hello}"
    );
    let status = &answers["fabric status"];
    assert!(
        status.starts_with("OK state=") && !status.contains("mirror="),
        "the line as it was: {status}"
    );
    assert_eq!(answers["draw"], "", "no tombstone drawn");
    assert_eq!(
        answers["commit's reading"], "false",
        "a replaced bus log stands no update down"
    );
    assert!(!store.read().unwrap().mirror().served());
}

/// The local Owner may halt its own session and never touch a fleet hold.
///
/// Owner sets a local hold (`origin=local`, the only origin it may name): the
/// PTY-reaching verbs answer `ERR halted … origin=local`, `status`'s leaf read and
/// the `inbox` header show `hold=1`, and Owner's `hold off` lifts it. An edge token
/// and a selector are refused, and an Owner `off` that finds only a standing
/// `origin=fleet` hold is refused, so an injected agent holding Owner cannot lift
/// the halt meant to stop it; an Owner `on` beside it sets the owner's own slot
/// and leaves the fleet's standing. The bridge sets fleet holds by default and its
/// `off` lifts only a fleet one. Drives the decision function both entries call.
#[test]
fn the_local_owner_may_hold_its_session_and_never_a_fleet_hold_inbox_hold() {
    // A bridge-issued `hold` records the session as bridge-governed in the
    // process-wide link, so this takes the reset guard too.
    crate::fabric::with_link_reset(
        the_local_owner_may_hold_its_session_and_never_a_fleet_hold_body,
    );
}

fn the_local_owner_may_hold_its_session_and_never_a_fleet_hold_body() {
    let store = session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    let sid = handle.sid.as_str().to_string();
    let ctx = handle.ctx.clone();
    store.write().unwrap().register(handle);
    let edge = Scope::Edge(EdgeToken::generate());
    let run = |scope: Scope, line: &str| {
        let (selector, verb, rest) = request_head(line);
        assert_eq!(verb, "hold");
        dispatch_hold_verb(rest, selector.as_ref(), scope, &store)
    };

    // OWNER: on, halted, status/inbox say so, off, lifted.
    assert_eq!(
        run(Scope::Owner, &format!("hold {sid} on reason=demo")),
        "OK hold=1\n"
    );
    for verb in ["key", "turn"] {
        assert_eq!(
            crate::fabric::halt_refusal(&ctx, verb).as_deref(),
            Some("ERR halted reason=demo origin=local\n"),
            "{verb} under the owner's halt"
        );
    }
    assert_eq!(
        u8::from(ctx.fabric.hold().is_some()),
        1,
        "the leaf read `status hold=` is built from"
    );
    assert!(
        crate::fabric::cmd_inbox(&ctx, "--peek").starts_with("OK 0 hold=1 holder=- "),
        "inbox header: hold=1, and `holder=` is the keyboard holder — nobody"
    );
    assert!(
        !crate::fabric::touched_contains(&sid),
        "an owner's local halt is not bridge governance"
    );
    assert_eq!(run(Scope::Owner, &format!("hold {sid} off")), "OK hold=0\n");
    for verb in ["key", "turn"] {
        assert!(
            crate::fabric::halt_refusal(&ctx, verb).is_none(),
            "{verb} after the owner's lift"
        );
    }

    // EDGE: refused, on and off. A SELECTOR: refused, from Owner and Bridge.
    assert_eq!(
        run(edge, &format!("hold {sid} on reason=x")),
        "ERR denied\n"
    );
    assert_eq!(run(edge, &format!("hold {sid} off")), "ERR denied\n");
    assert_eq!(
        run(Scope::Owner, &format!("@1 hold {sid} on reason=x")),
        "ERR denied\n"
    );
    assert_eq!(
        run(Scope::Bridge, &format!("@1 hold {sid} on reason=x")),
        "ERR denied\n"
    );
    assert!(crate::fabric::halt_refusal(&ctx, "key").is_none());

    // Owner may not name the fleet origin, standing hold or not.
    assert_eq!(
        run(
            Scope::Owner,
            &format!("hold {sid} on origin=fleet reason=x")
        ),
        crate::fabric::HOLD_DENIED
    );
    assert_eq!(
        run(Scope::Owner, &format!("hold {sid} off origin=fleet")),
        crate::fabric::HOLD_DENIED
    );

    // BRIDGE: unchanged. Fleet by default, and the fleet hold then stands
    // against every Owner act.
    assert_eq!(
        run(
            Scope::Bridge,
            &format!("hold {sid} on reason=main%20broken")
        ),
        "OK hold=1\n"
    );
    assert_eq!(
        crate::fabric::halt_refusal(&ctx, "turn").as_deref(),
        Some("ERR halted reason=main%20broken origin=fleet\n")
    );
    assert_eq!(
        run(Scope::Owner, &format!("hold {sid} off")),
        crate::fabric::HOLD_DENIED
    );
    // The owner's own hold beside it: its slot, not a replacement…
    assert_eq!(
        run(Scope::Owner, &format!("hold {sid} on reason=mine")),
        "OK hold=1\n"
    );
    assert_eq!(ctx.fabric.holds().origins(), "fleet,local");
    // …and the owner's `off` lifts that and nothing else.
    assert_eq!(run(Scope::Owner, &format!("hold {sid} off")), "OK hold=1\n");
    assert_eq!(
        crate::fabric::halt_refusal(&ctx, "turn").as_deref(),
        Some("ERR halted reason=main%20broken origin=fleet\n"),
        "the fleet hold stands, neither lifted nor replaced"
    );
    assert!(
        crate::fabric::touched_contains(&sid),
        "the BRIDGE's hold did make the session bridge-governed"
    );
    // The bridge lifts; the owner can hold again; and the bridge's fleet
    // `off` leaves that local hold standing — the owner's `off` lifts it.
    assert_eq!(
        run(Scope::Bridge, &format!("hold {sid} off")),
        "OK hold=0\n"
    );
    assert_eq!(
        run(Scope::Owner, &format!("hold {sid} on reason=mine")),
        "OK hold=1\n"
    );
    assert_eq!(
        run(Scope::Bridge, &format!("hold {sid} off")),
        "OK hold=1\n"
    );
    assert_eq!(
        crate::fabric::halt_refusal(&ctx, "turn").as_deref(),
        Some("ERR halted reason=mine origin=local\n")
    );
    assert_eq!(run(Scope::Owner, &format!("hold {sid} off")), "OK hold=0\n");
    assert!(crate::fabric::halt_refusal(&ctx, "turn").is_none());
}

/// `post`'s scope gate is a per-handler check: an edge is refused outright, since a
/// write-input edge over S authorizes typing into S, not speaking as S on the bus.
/// The bridge is owner-class, so it may post.
#[test]
fn post_refuses_an_edge_token_connection_inbox_hold() {
    assert!(!post_scope_denied(Scope::Owner));
    assert!(!post_scope_denied(Scope::Bridge));
    assert!(post_scope_denied(Scope::Edge(EdgeToken::generate())));
}

/// The dispatch's gates on a real pty: a raw program that stopped reading holds the
/// human's Enter; once the watch publishes it as stalled, a driver's `key down` is
/// refused `ERR busy input-unread` (before publication it is written with a warning;
/// see `input_stall`'s tests). A held session answers `ERR halted` first, and the
/// refused attempt consumes no `id=` sequence: once the program reads, the same key
/// is a first attempt that writes, and only then a duplicate.
#[test]
#[cfg(target_os = "macos")]
fn the_unread_input_gate_refuses_after_the_halt_and_gives_the_id_back() {
    use crate::input_stall::tests::{raw_pty_pair, slave_read_all};
    let (master, slave) = raw_pty_pair();
    let h = registered_session(9, master, b"");
    h.ctx.sink.note_master_nonblocking(true);
    h.ctx
        .sink
        .write_frame_nonparking(b"\r")
        .expect("the human's Enter");
    crate::input_stall::tests::publish_stall(&h.ctx);

    let refused =
        crate::input_stall::refusal(&h.ctx, "key", "down", false).expect("the gate refuses");
    assert!(refused.starts_with("ERR busy input-unread"), "{refused}");
    assert_eq!(
        crate::input_stall::refusal(&h.ctx, "key", "down", true),
        None
    );
    assert_eq!(
        crate::input_stall::refusal(&h.ctx, "signal", "term", false),
        None
    );

    // A local hold: the halt, asked first, is the answer.
    assert!(crate::fabric::apply_hold_for_test(
        &h.ctx,
        Some(crate::fabric::Hold {
            reason: "demo".to_string(),
            origin: "local".to_string(),
        })
    ));
    assert_eq!(
        crate::fabric::halt_refusal(&h.ctx, "key").as_deref(),
        Some("ERR halted reason=demo origin=local\n"),
    );
    assert!(crate::fabric::apply_hold_for_test(&h.ctx, None));

    // The refused attempt never reached the claim, so its key is unspent.
    let key = format!("{}:4:1", h.ctx.nonce.to_hex());
    let attempt = || match crate::input_stall::refusal(&h.ctx, "key", "down", false) {
        Some(refusal) => refusal,
        None => {
            crate::pty_idem::guarded(&h.ctx, Scope::Owner, "key", Some(key.as_str()), || match h
                .ctx
                .sink
                .write_frame_nonparking(b"\x1b[B")
            {
                Ok(_) => "OK\n".to_string(),
                Err(_) => "ERR write failed\n".to_string(),
            })
        }
    };
    assert!(attempt().starts_with("ERR busy input-unread"));
    // Nothing was written behind the Enter: the program reads it alone.
    assert_eq!(slave_read_all(slave), b"\r");
    assert_eq!(
        attempt(),
        "OK\n",
        "the refused attempt gave its sequence back"
    );
    assert_eq!(attempt(), "OK dup=1\n");
    assert_eq!(slave_read_all(slave), b"\x1b[B");
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);
}

/// `turn` never stacks Enters behind one the program has not read: into a raw
/// program that is not reading, a `presses=3` turn pastes, presses Enter once, sees
/// it unread and stops (`submitted=0`, reply shape unchanged), leaving the text and
/// one `\r` queued. Negative control: into a pipe (nothing to consult) it presses
/// all three.
///
/// NOT A BLOCK (owner direction, 2026-09-29): the guard ENDS the presses,
/// it never waits for the Enter to be read. The Enter here is never read,
/// yet the turn answers within one submit window and its settle, far
/// inside its budget — a program that is merely slow reads its one Enter
/// when it gets to it. The budget is the verb's ten-minute ceiling, so a
/// turn that waited on the unread Enter runs it out and the minute's hang
/// detector on `dur_ms=` tells the two apart however loaded the machine.
#[test]
#[cfg(target_os = "macos")]
fn the_unread_input_gate_lets_a_turn_press_enter_once_into_a_program_that_is_not_reading() {
    use crate::input_stall::tests::{raw_pty_pair, slave_read_all};
    let turn = |h: &SessionHandle| -> (String, u32) {
        let store = session_store::new_store();
        let subscribers = crate::subscribe::new_registry();
        store.write().unwrap().register(h.clone());
        let presses = std::cell::Cell::new(0u32);
        let paste = |text: &str| h.ctx.sink.write_frame_nonparking(text.as_bytes()).is_ok();
        let press = |key: &str| {
            assert_eq!(key, "enter");
            presses.set(presses.get() + 1);
            h.ctx.sink.write_frame_nonparking(b"\r").is_ok()
        };
        let reply = control_session::cmd_turn(
            &h.term,
            &store,
            h.local_id,
            "idle=1 timeout=600000 submit_window=150 presses=3 -- hi",
            &subscribers,
            &h.ctx,
            &control_session::TurnIo {
                paste: &paste,
                press: &press,
                ..control_session::TurnIo::paste_only()
            },
        );
        (reply, presses.get())
    };

    let (master, slave) = raw_pty_pair();
    let h = registered_session(9, master, b"");
    h.ctx.sink.note_master_nonblocking(true);
    let (reply, presses) = turn(&h);
    assert!(reply.starts_with("OK "), "{reply}");
    assert!(reply.contains("submitted=0"), "{reply}");
    let dur_ms: u64 = reply
        .split_whitespace()
        .find_map(|f| f.strip_prefix("dur_ms="))
        .and_then(|n| n.parse().ok())
        .expect("the verdict's dur_ms");
    assert!(
        std::time::Duration::from_millis(dur_ms) < aterm_spec::wait::MUST_HAPPEN,
        "the guard ended the presses; the turn did not wait on the unread Enter: {reply}"
    );
    assert_eq!(presses, 1, "one Enter, never a stack of them");
    assert_eq!(slave_read_all(slave), b"hi\r");
    aterm_pty::close_fd(master);
    aterm_pty::close_fd(slave);

    let (control, _rx) = pipe_session(10);
    let (reply, presses) = turn(&control);
    assert!(reply.contains("submitted=0"), "{reply}");
    assert_eq!(presses, 3, "with no reading, the re-press stands");
}

/// An aimed App verb is gated on what it drives, not on the sid it names.
///
/// `@<sid> tab …` passes the App-lane halt gate via `aimed_app_lane` so the selector
/// reaches the resolver, then drives `hosting_window(sid)`, so `tab close N`
/// retires whatever session tab N hosts. Gated only on the aimed session's hold, a
/// partial halt (`bridge_lost` halts only `LINK.touched` sids) would let
/// `@<unheld> tab close N` retire a held sibling that `@<held> close` and bare
/// `tab close` refuse.
#[test]
fn an_aimed_app_verb_asks_the_app_lane_halt_question_inbox_hold() {
    crate::fabric::with_link_reset(an_aimed_app_verb_asks_the_app_lane_halt_question_body);
}

fn an_aimed_app_verb_asks_the_app_lane_halt_question_body() {
    let store = session_store::new_store();
    let held = crate::session_store::test_handle(1);
    let unheld = crate::session_store::test_handle(2);
    let held_sid = held.sid.as_str().to_string();
    let unheld_ctx = unheld.ctx.clone();
    store.write().unwrap().register(held);
    store.write().unwrap().register(unheld);
    assert_eq!(
        dispatch_hold_verb(
            &format!("{held_sid} on reason=fabric-lost origin=fleet"),
            None,
            Scope::Bridge,
            &store,
        ),
        "OK hold=1\n",
    );

    // The session gate alone says the request may proceed, because the sid it
    // names is not the sid it would retire.
    assert!(
        crate::fabric::halt_refusal(&unheld_ctx, "tab").is_none(),
        "the unheld window-mate is not itself held: this is precisely why the \
         session gate is the wrong question for a verb that drives a window",
    );
    // The APP-lane gate — which the aimed forms now also ask — refuses it.
    assert_eq!(
        crate::fabric::app_halt_refusal(&store, "tab").as_deref(),
        Some("ERR halted reason=fabric-lost origin=fleet\n"),
    );
    // `spawn` rides the same lane and is a named residual of the halt set (it mints
    // a session rather than reaching one; see `is_pty_reaching`), so widening the
    // gate must not sweep it in.
    assert!(is_aimed_app_verb("spawn"));
    assert!(
        crate::fabric::app_halt_refusal(&store, "spawn").is_none(),
        "the gate asks `is_pty_reaching`, not `is this an aimed App verb`",
    );

    // ONE LITERAL, NOT TWO: every verb the lane routes to the resolver is a
    // verb this gate asks about, over the whole shipped table.
    for spec in aterm_types::control_verbs::VERBS {
        let routed = aimed_app_lane(
            spec.name,
            Some(&Selector::Local(1)),
            NativeControlPrincipal::Owner,
        )
        .is_some();
        assert_eq!(
            routed,
            is_aimed_app_verb(spec.name),
            "{}: the aimed lane and the gate that follows it must name the \
             same verbs",
            spec.name,
        );
    }
}

/// The `fabric:` lease namespace belongs to the bridge connection.
///
/// `Scope::Bridge` alone may acquire a `lease … holder=fabric:*`. `lease_acquire`
/// otherwise takes any 1..=64 printable-ASCII holder, so an in-session Owner agent
/// could show `driving=lease:fabric:<human>` in `who`, attribute its drive to that
/// human, keep it off the bus control ledger (the bridge filters `fabric:` holders
/// out of its lease event) and pre-empt the real mirror when the human claimed the
/// keyboard.
#[test]
fn only_the_bridge_may_claim_the_fabric_lease_namespace() {
    // The bridge mirror's exact `acquire` shape, which the rule must keep refusing
    // from any other scope.
    assert!(!lease_forges_fabric_holder(
        Scope::Bridge,
        "acquire holder=fabric:h-andrew ttl=30000"
    ));
    // The same line from any other scope is a forgery.
    for scope in [
        Scope::Owner,
        Scope::Edge(aterm_session::EdgeToken::generate()),
    ] {
        assert!(lease_forges_fabric_holder(
            scope,
            "acquire holder=fabric:h-andrew ttl=30000"
        ));
        assert!(lease_forges_fabric_holder(
            scope,
            "acquire ttl=1 holder=fabric:"
        ));
        // An ordinary holder is untouched: this fences a NAMESPACE, not the verb.
        assert!(!lease_forges_fabric_holder(scope, "acquire holder=drv-7"));
        assert!(!lease_forges_fabric_holder(scope, "acquire"));
        assert!(!lease_forges_fabric_holder(scope, "status"));
        assert!(!lease_forges_fabric_holder(scope, ""));
        // Acquire only: `release … force` stealing a `fabric:` hold is a separate
        // operator override, recorded in the function's doc.
        assert!(!lease_forges_fabric_holder(
            scope,
            "release holder=fabric:h-andrew"
        ));
    }
}

/// The bridge's end is a drop guard, so it fires however the bridge connection
/// ends (return, hangup, unwind). Dropping the guard is the production event: by
/// default the governed session keeps running with its advisory, and under
/// `[fabric] hold_on_bridge_lost = true` it is halted.
#[test]
fn dropping_the_bridge_guard_halts_the_governed_sessions_inbox_hold() {
    // Dropping the guard moves the instance link to `disconnected`, which
    // `status`'s `fabric=` and `post --wait` read; the reset guard serializes
    // tests that read it.
    crate::fabric::with_link_reset(dropping_the_bridge_guard_halts_the_governed_sessions_body);
}

fn dropping_the_bridge_guard_halts_the_governed_sessions_body() {
    let store = session_store::new_store();
    let handle = crate::session_store::test_handle(1);
    let sid = handle.sid.as_str().to_string();
    let ctx = handle.ctx.clone();
    store.write().unwrap().register(handle);
    assert_eq!(
        dispatch_bridge_verb(
            "deliver",
            &format!("{sid} off=1 from=h-a kind=task trust=human text=x"),
            None,
            Scope::Bridge,
            &store,
        )
        .as_deref(),
        Some("OK 1\n")
    );
    assert!(crate::fabric::halt_refusal(&ctx, "turn").is_none());
    // The default: the governed session keeps running, with its advisory.
    let generation = crate::fabric::next_bridge_generation();
    crate::fabric::bridge_attached(generation);
    drop(BridgeLostGuard {
        store: store.clone(),
        generation,
    });
    assert!(
        crate::fabric::halt_refusal(&ctx, "turn").is_none(),
        "a lost bridge halts nothing by default"
    );
    assert_eq!(
        ctx.fabric.hold_advisory().map(|h| h.reason).as_deref(),
        Some("fabric-lost")
    );
    // THE KNOB: the fail-closed sweep.
    crate::fabric::set_hold_on_bridge_lost(true);
    drop(BridgeLostGuard {
        store: store.clone(),
        generation: crate::fabric::next_bridge_generation(),
    });
    assert_eq!(
        crate::fabric::halt_refusal(&ctx, "turn").as_deref(),
        Some("ERR halted reason=fabric-lost origin=fleet\n"),
        "under the knob, killing the bridge HALTS what it governed"
    );
}

/// The length-prefixed `post` frame is detected with the handler's own option
/// vocabulary, so a `len=` inside a message body is not a frame (a separate parser
/// would desync the stream).
#[test]
fn the_post_frame_detector_agrees_with_the_body_rule_inbox_hold() {
    assert_eq!(post_frame_len("post to=h-a kind=note len=12"), Some(Ok(12)));
    assert_eq!(
        post_frame_len("@s-a post to=h-a kind=ask --wait len=3"),
        Some(Ok(3))
    );
    assert_eq!(
        post_frame_len("post to=h-a kind=note hello len=12"),
        None,
        "a `len=` inside the BODY is not a frame"
    );
    assert_eq!(post_frame_len("post to=h-a kind=note hi"), None);
    // After `--` everything is body: a `len=` there announces no frame.
    assert_eq!(post_frame_len("post to=h-a kind=note -- len=12"), None);
    assert_eq!(post_frame_len("send len=4"), None);
    let refused = |line: &str| match post_frame_len(line) {
        Some(Err(refusal)) => refusal,
        other => panic!("{line}: {other:?}"),
    };
    assert!(refused("post to=h-a kind=note len=99999999").starts_with("ERR too large"));
    assert!(
        refused("post to=h-a kind=note len=abc").starts_with("ERR usage"),
        "a typo is not a size problem"
    );
}

// ---- A6: exactly-once at the PTY seam ---------------------------------------

/// Drive one `feed-bin` frame through the production framing path, recording every
/// event that reached the input seam. Returns `(reply, events)`, so "typed once" is
/// counted at the seam.
#[cfg(unix)]
fn feed_bin_once(
    active: &ActiveHandle,
    store: &Store,
    line: &str,
    payload: &[u8],
    seen: &std::cell::RefCell<Vec<Vec<u8>>>,
) -> String {
    let mut body = payload.to_vec();
    body.extend_from_slice(b"after\n");
    let mut reader = std::io::Cursor::new(body);
    let mut dispatch = |event: InputEvent, _session| {
        if let InputEvent::KeySequence(bytes) = &event {
            seen.borrow_mut().push(bytes.clone());
        }
        Ok(InputOutcome::Ok)
    };
    let mut clear_license = |_session| "OK\n".to_string();
    let mut out = Vec::new();
    assert!(
        run_feed_bin_routed(
            line,
            "feed-bin",
            &mut reader,
            FeedBinRoute {
                active,
                store,
                scope: Scope::Owner,
                subscribers: None,
            },
            &mut dispatch,
            &mut clear_license,
            &mut out,
        ),
        "the frame must keep the connection: {line}"
    );
    // The stream stays framed: a duplicate is refused after the announced payload
    // is consumed, so the next request line is the client's, never payload bytes.
    assert_eq!(
        read_request_line(&mut reader).as_deref(),
        Some("after"),
        "the announced payload must be consumed even by a refused frame"
    );
    String::from_utf8_lossy(&out).into_owned()
}

/// `feed-bin id=k` twice types once and the second answers `dup=1`, which resolves
/// the bridge's in-doubt window: a bridge that crashed between writing and
/// recording replays the key on restart and is told `dup=1` if the write landed,
/// or writes for the first time if not.
#[test]
#[cfg(unix)]
fn feed_idempotent_feed_bin_types_once_and_the_replay_says_dup() {
    let store = session_store::new_store();
    let (front, _rx) = pipe_session(1);
    let key = format!("{}:9:1", front.ctx.nonce.to_hex());
    store.write().unwrap().register(front.clone());
    let active = active_for_handle(&front);
    let seen = std::cell::RefCell::new(Vec::new());

    // The bridge writes, then dies before it can record the result.
    let first = feed_bin_once(
        &active,
        &store,
        &format!("@1 feed-bin 3 id={key}"),
        b"ABC",
        &seen,
    );
    assert_eq!(first, "OK 3 bytes\n");
    assert_eq!(seen.borrow().as_slice(), [b"ABC".to_vec()]);

    // It restarts and replays the SAME key. Nothing is typed.
    let replay = feed_bin_once(
        &active,
        &store,
        &format!("@1 feed-bin 3 id={key}"),
        b"ABC",
        &seen,
    );
    assert_eq!(replay, "OK dup=1\n", "the replay is recognized, not typed");
    assert_eq!(
        seen.borrow().len(),
        1,
        "exactly one keystroke reached the seam across the crash window"
    );

    // The OTHER side of the window: a key the endpoint never saw is FRESH, so a
    // bridge that crashed BEFORE the write loses nothing by replaying.
    let next = format!("{}:9:2", front.ctx.nonce.to_hex());
    assert_eq!(
        feed_bin_once(
            &active,
            &store,
            &format!("@1 feed-bin 3 id={next}"),
            b"DEF",
            &seen
        ),
        "OK 3 bytes\n"
    );
    assert_eq!(seen.borrow().len(), 2);

    // And an UNKEYED frame is the pre-A6 wire, byte for byte.
    assert_eq!(
        feed_bin_once(&active, &store, "@1 feed-bin 3", b"GHI", &seen),
        "OK 3 bytes\n"
    );
    assert_eq!(seen.borrow().len(), 3);
}

/// A keyed attempt that moves no bytes must not consume its sequence: a zero-length
/// `feed-bin 0 id=<k>` is refused as a usage error before the claim, leaving the
/// key a first attempt. `send` with an empty tail shares the refusal
/// (`pty_idem::empty_payload_refusal`), and both call sites refuse before the claim.
#[test]
#[cfg(unix)]
fn feed_idempotent_an_empty_keyed_payload_consumes_no_sequence() {
    let store = session_store::new_store();
    let (front, _rx) = pipe_session(1);
    let key = format!("{}:9:1", front.ctx.nonce.to_hex());
    store.write().unwrap().register(front.clone());
    let active = active_for_handle(&front);
    let seen = std::cell::RefCell::new(Vec::new());

    assert!(
        feed_bin_once(
            &active,
            &store,
            &format!("@1 feed-bin 0 id={key}"),
            b"",
            &seen
        )
        .starts_with("ERR usage"),
        "a keyed empty frame is a usage error, not an applied write"
    );
    assert!(seen.borrow().is_empty(), "nothing reached the seam");
    assert_eq!(
        feed_bin_once(
            &active,
            &store,
            &format!("@1 feed-bin 3 id={key}"),
            b"ABC",
            &seen
        ),
        "OK 3 bytes\n",
        "the same key is still a FIRST attempt — the empty frame claimed nothing"
    );
    assert_eq!(seen.borrow().as_slice(), [b"ABC".to_vec()]);
    // An UNKEYED empty frame keeps its old answer.
    assert_eq!(
        feed_bin_once(&active, &store, "@1 feed-bin 0", b"", &seen),
        "OK 0 bytes\n"
    );
}

/// Owner and Bridge are distinct classes at `caller_actor` and `cmd_whoami`: the
/// exits ledger attributes them apart (`by=ctl` / `by=bridge`). The only test that
/// drives either function with `Scope::Bridge`.
#[test]
fn owner_and_bridge_differ_at_whoami_and_at_caller_actor() {
    let h = registered_session(0, -1, b"");
    assert_eq!(
        control_session::caller_actor(Scope::Owner, &h.ctx),
        crate::session_store::ExitActor::Ctl,
        "an owner-token close is attributed to a ctl client"
    );
    assert_eq!(
        control_session::caller_actor(Scope::Bridge, &h.ctx),
        crate::session_store::ExitActor::Bridge,
        "a bridge close is attributed to the bridge"
    );
    assert_ne!(
        control_session::cmd_whoami(&h.ctx, Scope::Owner),
        control_session::cmd_whoami(&h.ctx, Scope::Bridge),
        "cmd_whoami is the site that reports them differently"
    );
}

/// A mark's producer is namespaced by authority, so a local driver cannot burn the
/// bridge's sequence.
///
/// The key's `<producer>` is caller-typed and the `<epoch>` is public (roster
/// `nonce=`, `$ATERM_LAUNCH_NONCE`), so an Owner connection could send
/// `send id=<epoch>:<bridge producer>:18446744073709551615` with an empty tail,
/// settle `Applied` with nothing typed, and pin the bridge's high-water at the top
/// of the u64 range; every later `Bridge::feed` would answer `OK dup=1` and write
/// nothing.
///
/// `Scope::Bridge` is a connection, not a token, so no token can claim in its
/// namespace. Answering is not claiming: a local caller naming a sequence the
/// bridge consumed is still answered `OK dup=1` and writes nothing.
/// Only installing a high-water in the bridge's namespace is fenced.
#[test]
fn feed_idempotent_a_producer_is_namespaced_by_the_callers_authority() {
    let h = registered_session(0, -1, b"");
    let hex = h.ctx.nonce.to_hex();
    let bridge_producer = 7u64;

    // T1's one verb: claim the bridge's producer at the top of the range.
    let attack = format!("{hex}:{bridge_producer}:{}", u64::MAX);
    assert_eq!(
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "send", Some(attack.as_str()), || {
            // `send` with an empty tail: OK, and nothing typed.
            "OK\n".to_string()
        }),
        "OK\n"
    );

    // The human's keystroke, arriving over the bridge under the bridge's own
    // key. It must RUN.
    let typed = std::cell::Cell::new(0u32);
    let bridge_key = format!("{hex}:{bridge_producer}:1");
    let feed = || {
        crate::pty_idem::guarded(
            &h.ctx,
            Scope::Bridge,
            "feed-bin",
            Some(bridge_key.as_str()),
            || {
                typed.set(typed.get() + 1);
                "OK 3 bytes\n".to_string()
            },
        )
    };
    assert_eq!(
        feed(),
        "OK 3 bytes\n",
        "an Owner caller claimed the bridge's producer and swallowed the keystroke"
    );
    assert_eq!(typed.get(), 1, "the bytes actually reached the seam");

    // And the bridge's own replay is still recognized INSIDE its own realm —
    // the exactly-once property this module exists for is unchanged.
    assert_eq!(feed(), "OK dup=1\n");
    assert_eq!(typed.get(), 1);

    // The local realm still remembers its own claim, so nothing was weakened
    // for the caller that made it.
    assert_eq!(
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "send", Some(attack.as_str()), || {
            panic!("a local replay must not run again")
        }),
        "OK dup=1\n"
    );

    // Answering is not claiming: the fence is on the install, so an Owner asking
    // about a sequence the bridge consumed is answered from the bridge's mark
    // and writes nothing (asserted below).
    let other = format!("{hex}:{}:1", bridge_producer + 1);
    assert_eq!(
        crate::pty_idem::guarded(
            &h.ctx,
            Scope::Bridge,
            "feed-bin",
            Some(other.as_str()),
            || "OK 3 bytes\n".to_string()
        ),
        "OK 3 bytes\n"
    );
    assert_eq!(
        crate::pty_idem::guarded(
            &h.ctx,
            Scope::Owner,
            "feed-bin",
            Some(other.as_str()),
            || { panic!("a consumed sequence must be answered without writing") }
        ),
        "OK dup=1\n",
        "the bridge's consumed sequence is answerable from any connection"
    );
}

/// `turn id=k` twice submits once. The guard wraps the same
/// [`control_session::cmd_turn`] the dispatch calls, with a `TurnIo` that counts
/// what reached the app.
#[test]
fn feed_idempotent_turn_submits_once() {
    use std::cell::{Cell, RefCell};
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let term = &h.term;
    let key = format!("{}:4:1", h.ctx.nonce.to_hex());

    let pasted: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let presses = Cell::new(0u32);
    let paste = |text: &str| {
        pasted.borrow_mut().push(text.to_string());
        term.lock().unwrap().process(text.as_bytes());
        true
    };
    let press = |name: &str| {
        presses.set(presses.get() + 1);
        assert_eq!(name, "enter");
        term.lock().unwrap().process(b"\r\nresponse-line\r\n");
        true
    };
    let run = || {
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "turn", Some(key.as_str()), || {
            control_session::cmd_turn(
                term,
                &store,
                0,
                "idle=50 timeout=60000 hello peer",
                &subscribe::new_registry(),
                &h.ctx,
                &TurnIo {
                    paste: &paste,
                    press: &press,
                    ..TurnIo::paste_only()
                },
            )
        })
    };

    let first = run();
    assert!(
        first.starts_with("OK 24 turn submitted=1"),
        "the first turn runs: {}",
        first.lines().next().unwrap_or("")
    );
    let again = run();
    // In `turn`'s own framing (`Framing::Lines`: `OK <n>` then n rows), the
    // duplicate is a zero-row listing with the marker in the tail; a bare
    // `OK dup=1` would read as a malformed header to `aterm-ctl`.
    assert_eq!(again, "OK 0 dup=1\n", "the replay does not re-submit");
    // A Lines client can parse it: `aterm-ctl`'s `stream_count` rule takes the
    // tail's first whitespace token as a row count.
    let tail = again.trim_end().strip_prefix("OK ").expect("an OK header");
    assert_eq!(
        tail.split_whitespace()
            .next()
            .and_then(|t| t.parse::<usize>().ok()),
        Some(0),
        "a Lines client reads the dup header as a zero-row listing: {again:?}"
    );
    assert_eq!(
        pasted.borrow().as_slice(),
        ["hello peer"],
        "typed exactly once"
    );
    assert_eq!(presses.get(), 1, "submitted exactly once");
}

/// The in-doubt verdict: an attempt that did not answer `OK` may still have typed
/// (`cmd_turn` types, then can fail to submit), so the sequence is kept and its
/// outcome recorded as unknown. A replay is told so and runs nothing; `timeline`
/// carries a row for a human.
///
/// `ERR busy` and `ERR denied` are decided before any byte moves, so they give the
/// sequence back and a retry is a first attempt.
#[test]
fn feed_idempotent_an_unknown_outcome_is_reported_and_never_replayed() {
    let h = registered_session(0, -1, b"");
    let key = format!("{}:4:1", h.ctx.nonce.to_hex());
    let runs = std::cell::Cell::new(0u32);

    // A write that failed in a way that says nothing about the PTY.
    let out = crate::pty_idem::guarded(&h.ctx, Scope::Owner, "send", Some(key.as_str()), || {
        runs.set(runs.get() + 1);
        "ERR write failed\n".to_string()
    });
    assert_eq!(out, "ERR write failed\n");
    // The replay is refused rather than re-typed OR reported as a duplicate.
    let replay = crate::pty_idem::guarded(&h.ctx, Scope::Owner, "send", Some(key.as_str()), || {
        runs.set(runs.get() + 1);
        "ERR write failed\n".to_string()
    });
    assert_eq!(replay, "ERR in-doubt seq=1\n");
    assert_eq!(runs.get(), 1, "an in-doubt sequence is never replayed");

    // VISIBLE: the row is on this session's timeline, with no input bytes on it.
    let timeline = h.ctx.timeline.lock().unwrap();
    let doubt: Vec<_> = timeline
        .since(None)
        .filter(|e| e.kind == "in-doubt")
        .collect();
    assert_eq!(doubt.len(), 1, "exactly one in-doubt row");
    assert_eq!(doubt[0].payload, "producer=4 seq=1 reply=ERR-write");
    drop(timeline);

    // The carve-outs: a lease or authority refusal is decided before any byte,
    // so it gives the sequence back.
    for refusal in ["ERR busy turn=7\n", "ERR denied\n"] {
        let h = registered_session(0, -1, b"");
        let key = format!("{}:4:1", h.ctx.nonce.to_hex());
        let runs = std::cell::Cell::new(0u32);
        let attempt = || {
            crate::pty_idem::guarded(&h.ctx, Scope::Owner, "send", Some(key.as_str()), || {
                runs.set(runs.get() + 1);
                if runs.get() == 1 {
                    refusal.to_string()
                } else {
                    "OK 3 bytes\n".to_string()
                }
            })
        };
        assert_eq!(attempt(), refusal);
        assert_eq!(attempt(), "OK 3 bytes\n", "{refusal} released the sequence");
        assert_eq!(runs.get(), 2);
        // And having landed, it is now a duplicate.
        assert_eq!(attempt(), "OK dup=1\n");
        assert_eq!(runs.get(), 2);
        assert!(
            h.ctx
                .timeline
                .lock()
                .unwrap()
                .since(None)
                .all(|e| e.kind != "in-doubt"),
            "a pre-write refusal is not in doubt"
        );
    }
}

/// A sequence below the mark is a duplicate too (everything under the producer's
/// high-water was consumed), and marks are per producer, so two drivers cannot make
/// each other's keys look like replays.
#[test]
fn feed_idempotent_the_mark_is_per_producer_and_covers_everything_under_it() {
    let h = registered_session(0, -1, b"");
    let hex = h.ctx.nonce.to_hex();
    let runs = std::cell::Cell::new(0u32);
    let go = |key: String| {
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "send", Some(key.as_str()), || {
            runs.set(runs.get() + 1);
            "OK\n".to_string()
        })
    };
    assert_eq!(go(format!("{hex}:1:5")), "OK\n");
    assert_eq!(go(format!("{hex}:1:3")), "OK dup=1\n", "under the mark");
    assert_eq!(go(format!("{hex}:1:5")), "OK dup=1\n", "at the mark");
    assert_eq!(go(format!("{hex}:1:6")), "OK\n", "above the mark");
    // A DIFFERENT producer's sequence 5 is its own first attempt.
    assert_eq!(go(format!("{hex}:2:5")), "OK\n");
    assert_eq!(runs.get(), 3);
}

/// THE KEY ON AN UNKEYED VERB IS CONTENT. `turn`'s help says the other input
/// verbs "take NO key — a leading id= on `paste` is delivered as literal
/// text". This is the DELIVERY of that claim: a `paste` stamped with a well-formed key
/// has the dispatch's option scan leave its tail untouched (`paste` is
/// neither keyed nor guarded), the SAME bytes reach the PTY — `id=` and all
/// — and no sequence is consumed, so the key is still a first attempt for a
/// verb that does take one. A driver that believed the old "every input verb
/// shares" wording got exactly this: its key typed into the shell, with no
/// exactly-once protection at all.
#[test]
#[cfg(unix)]
fn unkeyed_verbs_deliver_a_leading_id_as_content() {
    let (h, rx) = pipe_session(1);
    let key = format!("{}:7:1", h.ctx.nonce.to_hex());
    // The literal trailing `\n` is the paste verb's line break, sent as CR.
    let line = format!("id={key} hi\\n");
    assert!(!crate::pty_idem::is_keyed_verb("paste"));

    // The dispatch's scan takes nothing: the tail is the whole line.
    let (leading, rest) = control_input::take_leading_options("paste", &line);
    assert_eq!(leading.idem, None);
    assert!(leading.refusal.is_none());
    assert_eq!(rest, line);

    // The `paste` arm's event, delivered through the real sink: the key
    // lands on the PTY as text, byte for byte, and the `\n` as CR.
    assert_eq!(
        cross_input(
            &h.term,
            &h.ctx,
            Some(InputEvent::Paste(paste_text(&rest), PasteFraming::AtDrain)),
            "ERR\n",
        ),
        "OK\n"
    );
    assert_eq!(
        drain_pipe(&rx),
        format!("id={key} hi\r").as_bytes(),
        "a leading id= on `paste` is delivered as literal text"
    );

    // …and nothing claimed it: the same key on a keyed verb is FRESH, not
    // `dup=1` — the paste bought no exactly-once protection.
    let reply = crate::pty_idem::guarded(&h.ctx, Scope::Owner, "send", Some(&key), || {
        "OK\n".to_string()
    });
    assert_eq!(reply, "OK\n", "a paste consumed no sequence");
}

/// A pipe-backed session whose master accepts the immediate sink write: the write
/// end is made and declared `O_NONBLOCK` as `spawn` does for a real master (the
/// guarded press refuses a blocking description).
#[cfg(unix)]
fn guarded_pipe_session(local_id: u64) -> (crate::session_store::SessionHandle, std::fs::File) {
    let (h, rx) = pipe_session(local_id);
    aterm_pty::set_nonblocking(h.master, true).expect("nonblocking test master");
    h.ctx.sink.note_master_nonblocking(true);
    (h, rx)
}

/// `key if=<re> <name>` is one check-and-press: a matching row delivers the key and
/// replies `OK` (stamped `seq=`); no match writes nothing and replies `OK skipped`
/// (stamped, exit 0). `send if=` is the same guard on a raw write, submit included.
#[test]
#[cfg(unix)]
fn a_guarded_key_presses_when_a_row_matches_and_writes_nothing_when_none_does() {
    let (h, rx) = guarded_pipe_session(1);
    let proceed = control_input::compile_guard(Some("Do.you.want.to.proceed"))
        .unwrap()
        .unwrap();
    let elsewhere = control_input::compile_guard(Some("Nothing.of.the.kind"))
        .unwrap()
        .unwrap();

    // The prompt is NOT up: the guard misses, the digit never lands.
    assert_eq!(
        control_input::cmd_key_guarded(&h.term, &h.ctx, proceed.as_ref(), "1"),
        "OK skipped\n"
    );
    assert!(
        drain_pipe(&rx).is_empty(),
        "a skipped guard must write nothing"
    );

    // The prompt is up: the guard matches, the digit is delivered.
    crate::term_lock(&h.term).process(b"Do you want to proceed?\r\n  1. Yes\r\n");
    assert_eq!(
        control_input::cmd_key_guarded(&h.term, &h.ctx, proceed.as_ref(), "1"),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"1", "the matched press reaches the PTY");

    // A guard that names something else still misses on that same screen.
    assert_eq!(
        control_input::cmd_key_guarded(&h.term, &h.ctx, elsewhere.as_ref(), "1"),
        "OK skipped\n"
    );
    assert!(drain_pipe(&rx).is_empty());

    // The dispatch stamps BOTH answers with the content baseline, so the
    // driver's next `await seq` is anchored whichever way the guard went.
    let seq = crate::term_lock(&h.term).content_seq();
    assert_eq!(
        stamp_input_seq("key", "OK skipped\n".to_string(), &h.term),
        format!("OK skipped seq={seq}\n")
    );
    assert_eq!(
        stamp_input_seq("key", "OK\n".to_string(), &h.term),
        format!("OK seq={seq}\n")
    );

    // A malformed key name is the usage line — before any lock, nothing written.
    assert_eq!(
        control_input::cmd_key_guarded(&h.term, &h.ctx, proceed.as_ref(), "notakey"),
        control_input::KEY_USAGE
    );
    assert!(drain_pipe(&rx).is_empty());

    // `send if=…`: the raw body with the literal `\n` submit form → CR.
    assert_eq!(
        control_input::cmd_send_guarded(&h.term, &h.ctx, proceed.as_ref(), "y\\n"),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"y\r");
    assert_eq!(
        control_input::cmd_send_guarded(&h.term, &h.ctx, elsewhere.as_ref(), "y\\n"),
        "OK skipped\n"
    );
    assert!(drain_pipe(&rx).is_empty());
}

/// A GUARDED `send` LONGER THAN THE PTY'S INPUT QUEUE IS DELIVERED WHOLE, IN
/// ORDER (macOS: a raw slave's queue holds 1022 bytes). The harness types its
/// texts with `send if-gen=… if=… -- <text>`, and its instruction to save the
/// work before a rate-limit switch is 1.1 KB: over a real pty whose program has
/// not read yet, the guard holds, the reply is `OK`, and the program reads all
/// 1118 bytes in order — the kernel took 1022 and the rest followed from the
/// sink's spill as it read. The guarded Enter that submits it is refused with
/// zero bytes (`ERR busy sink`) while any of the text is still queued, and goes
/// after it once the program has read it.
///
/// Until 2026-10-16 the same `send` answered `ERR write failed partial
/// accepted=1022` and the rest was never written (the sink's real-pty test
/// `a_guarded_text_longer_than_the_input_queue_is_delivered_whole_and_in_order`
/// keeps that write as its negative control). NEGATIVE CONTROL here: a guard
/// that misses writes none of it (`OK skipped`, nothing queued, nothing read).
#[test]
#[cfg(target_os = "macos")]
fn a_guarded_send_longer_than_the_ptys_input_queue_is_delivered_whole() {
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
    unsafe { libc::cfmakeraw(&mut t) };
    assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
    aterm_pty::set_nonblocking(slave, true).expect("nonblocking slave");
    aterm_pty::set_nonblocking(master, true).expect("nonblocking master");
    let h = registered_session(1, master, b"");
    h.ctx.sink.note_master_nonblocking(true);
    crate::term_lock(&h.term).process(b"\r\n\xe2\x80\xba Ask Codex to do anything");
    let composer = control_input::compile_guard(Some("^› Ask"))
        .unwrap()
        .unwrap();
    let elsewhere = control_input::compile_guard(Some("Nothing.of.the.kind"))
        .unwrap()
        .unwrap();
    let mut text = "[aterm harness] GPT-6-Astra is close to its usage limit, so aterm switched \
                    this session to gpt-6-luna for one job only: saving your work. "
        .to_string();
    while text.len() < 1100 {
        text.push_str("Commit and push. ");
    }
    text.truncate(1100);
    text.push_str(" ATERM-SAVED-3f9a");
    text.truncate(1118);
    let read = |n: usize| -> Vec<u8> {
        let mut got = Vec::new();
        let done = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(5),
            || {
                let mut buf = [0_u8; 4096];
                let r = unsafe { libc::read(slave, buf.as_mut_ptr().cast(), buf.len()) };
                if let Ok(r) = usize::try_from(r) {
                    got.extend_from_slice(&buf[..r]);
                }
                (got.len() >= n).then_some(())
            },
        );
        assert!(done.is_some(), "only {} of {n} bytes came", got.len());
        got
    };
    let unread = || {
        let mut one = [0_u8; 1];
        let r = unsafe { libc::read(slave, one.as_mut_ptr().cast(), 1) };
        r < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock
    };

    // NEGATIVE CONTROL: the guard misses, nothing of it is written or queued.
    assert_eq!(
        control_input::cmd_send_guarded(&h.term, &h.ctx, elsewhere.as_ref(), &text),
        "OK skipped\n"
    );
    assert!(h.ctx.sink.egress_drained_to_kernel(), "nothing queued");
    assert!(unread(), "nothing written");

    // The guard holds: the whole text, in order.
    assert_eq!(
        control_input::cmd_send_guarded(&h.term, &h.ctx, composer.as_ref(), &text),
        "OK\n"
    );
    assert!(
        !h.ctx.sink.egress_drained_to_kernel(),
        "the rest is queued behind the 1022 bytes the queue took"
    );
    // The submit waits behind the text: zero bytes.
    assert_eq!(
        control_input::cmd_key_guarded(&h.term, &h.ctx, composer.as_ref(), "enter"),
        "ERR busy sink\n"
    );
    assert_eq!(read(text.len()), text.as_bytes(), "every byte, in order");
    // Read whole, the Enter goes (refused only while the drainer still holds
    // the fd lock for its last look at the emptied spill), after the text.
    let pressed = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || match control_input::cmd_key_guarded(&h.term, &h.ctx, composer.as_ref(), "enter")
            .as_str()
        {
            "OK\n" => Some(()),
            "ERR busy sink\n" => None,
            other => panic!("the Enter: {other}"),
        },
    );
    assert!(pressed.is_some(), "the Enter never went");
    assert_eq!(read(1), b"\r");
    assert!(unread());
    drop(h);
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// `id=` and `if=` compose in either order, and a skip gives its sequence back: the
/// same key retried after a skip is re-evaluated against the live screen, while a
/// retry after a press is `dup=1` with nothing written.
#[test]
#[cfg(unix)]
fn a_guarded_key_composes_with_the_idempotency_key_and_a_skip_gives_the_sequence_back() {
    let (h, rx) = guarded_pipe_session(1);
    let key = format!("{}:9:1", h.ctx.nonce.to_hex());

    // Either order is one request.
    let (a, rest_a) =
        control_input::take_leading_options("key", &format!("id={key} if=Do.you.want 1"));
    let (b, rest_b) =
        control_input::take_leading_options("key", &format!("if=Do.you.want id={key} 1"));
    assert_eq!(a, b);
    assert_eq!((rest_a.as_str(), rest_b.as_str()), ("1", "1"));
    assert_eq!(a.idem.as_deref(), Some(key.as_str()));
    assert_eq!(a.guard.as_deref(), Some("Do.you.want"));
    assert!(a.refusal.is_none());
    let guard = control_input::compile_guard(a.guard.as_deref())
        .unwrap()
        .unwrap();

    let attempt = || {
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "key", Some(key.as_str()), || {
            control_input::cmd_key_guarded(&h.term, &h.ctx, guard.as_ref(), "1")
        })
    };

    // No prompt: skipped, nothing written — and the sequence is NOT consumed.
    assert_eq!(attempt(), "OK skipped\n");
    assert!(drain_pipe(&rx).is_empty());

    // The prompt appears; the SAME sequence retried presses (a first attempt).
    crate::term_lock(&h.term).process(b"Do you want to proceed?\r\n");
    assert_eq!(
        attempt(),
        "OK\n",
        "a skipped guard gave its sequence back, so the retry is fresh"
    );
    assert_eq!(drain_pipe(&rx), b"1");

    // Retried again after the press: a duplicate, nothing written.
    assert_eq!(attempt(), "OK dup=1\n");
    assert!(drain_pipe(&rx).is_empty());

    // A fenced skip (`reason=changed`) gives its sequence back too: the retry
    // fenced on the current screen is a first attempt that presses.
    let key = format!("{}:9:2", h.ctx.nonce.to_hex());
    let stale = screen_gen(&crate::term_lock(&h.term));
    crate::term_lock(&h.term).process(b"the screen moved\r\n");
    let fenced = |generation: ScreenGen| {
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "key", Some(key.as_str()), || {
            control_input::cmd_key_fenced(
                &h.term,
                &h.ctx,
                &control_input::InputFence {
                    generation: Some(generation),
                    ..control_input::InputFence::default()
                },
                "1",
            )
        })
    };
    assert_eq!(fenced(stale), "OK skipped reason=changed\n");
    assert!(drain_pipe(&rx).is_empty());
    let current = screen_gen(&crate::term_lock(&h.term));
    assert_eq!(fenced(current), "OK\n", "the changed skip gave it back");
    assert_eq!(drain_pipe(&rx), b"1");
}

/// Box A, then box B drawn over it: two approval boxes the text guard
/// cannot tell apart — the swap a supervisor's `key 1` must not land on.
#[cfg(unix)]
const BOX_A: &[u8] = b"\x1b[2J\x1b[H Bash command\r\n   ls -la /tmp\r\n\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n";
#[cfg(unix)]
const BOX_B: &[u8] = b"\x1b[2J\x1b[H Bash command\r\n   rm -rf /tmp/work\r\n\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n   2. No\r\n";

/// The swap: a supervisor read box A (`gen=`/`hash=`), then box B replaced it.
/// `key if-gen=<A's gen> 1` and `key if-fp=<A's hash> 1` answer
/// `OK skipped reason=changed` and write nothing. Negative controls: the text guard
/// alone still matches B, and a fence built from B's own stamp presses.
#[test]
#[cfg(unix)]
fn a_fenced_key_never_presses_into_the_box_that_replaced_the_one_it_read() {
    use control_input::InputFence;
    let stamp = |h: &crate::session_store::SessionHandle| {
        let t = crate::term_lock(&h.term);
        (screen_gen(&t), screen_stamp(&t).1)
    };
    let (h, rx) = guarded_pipe_session(1);
    crate::term_lock(&h.term).process(BOX_A);
    let (gen_a, fp_a) = stamp(&h);
    crate::term_lock(&h.term).process(BOX_B);
    let (gen_b, fp_b) = stamp(&h);
    assert_ne!(
        (gen_a, fp_a),
        (gen_b, fp_b),
        "PRECONDITION: B is a new screen"
    );

    let proceed = control_input::compile_guard(Some("Do.you.want.to.proceed"))
        .unwrap()
        .unwrap();
    // Negative control 1: the guard alone matches B and would have pressed.
    assert!(
        aterm_core::terminal::first_matching_row(
            &*crate::term_lock(&h.term),
            proceed.as_ref(),
            aterm_core::terminal::RowRange::All
        )
        .is_some()
    );

    let press = |fence: InputFence<'_>| control_input::cmd_key_fenced(&h.term, &h.ctx, &fence, "1");
    for stale in [
        InputFence {
            generation: Some(gen_a),
            ..InputFence::default()
        },
        InputFence {
            fp: Some(fp_a),
            ..InputFence::default()
        },
        // The fences are asked before the guard, which would match here.
        InputFence {
            guard: Some(proceed.as_ref()),
            generation: Some(gen_a),
            fp: None,
            ..InputFence::default()
        },
    ] {
        assert_eq!(press(stale), "OK skipped reason=changed\n");
        assert!(
            drain_pipe(&rx).is_empty(),
            "a moved screen must write nothing"
        );
    }
    // Negative control 2: B's own stamp presses (the pipe echoes nothing, so the
    // screen and stamp stay B's across both presses).
    assert_eq!(
        press(InputFence {
            generation: Some(gen_b),
            ..InputFence::default()
        }),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"1");
    assert_eq!(
        press(InputFence {
            guard: Some(proceed.as_ref()),
            generation: None,
            fp: Some(fp_b),
            ..InputFence::default()
        }),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"1");

    // An identical REPAINT moves the generation but not the hash: `if-gen=`
    // is the strict fence, `if-fp=` lets a redraw of the same box through.
    crate::term_lock(&h.term).process(BOX_B);
    let (gen_c, fp_c) = stamp(&h);
    assert!(
        gen_c != gen_b && fp_c == fp_b,
        "PRECONDITION: a same-text repaint"
    );
    assert_eq!(
        press(InputFence {
            generation: Some(gen_b),
            ..InputFence::default()
        }),
        "OK skipped reason=changed\n"
    );
    assert!(drain_pipe(&rx).is_empty());
    assert_eq!(
        press(InputFence {
            fp: Some(fp_b),
            ..InputFence::default()
        }),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"1");

    // A current fence with a guard that misses is the guard's own answer,
    // byte-identical to a plain `if=` miss.
    let elsewhere = control_input::compile_guard(Some("Nothing.of.the.kind"))
        .unwrap()
        .unwrap();
    assert_eq!(
        press(InputFence {
            guard: Some(elsewhere.as_ref()),
            generation: Some(gen_c),
            fp: None,
            ..InputFence::default()
        }),
        "OK skipped\n"
    );
    // `send if-gen=` is the same fence on a raw write.
    assert_eq!(
        control_input::cmd_send_fenced(
            &h.term,
            &h.ctx,
            &InputFence {
                generation: Some(gen_a),
                ..InputFence::default()
            },
            "y\\n"
        ),
        "OK skipped reason=changed\n"
    );
    assert!(drain_pipe(&rx).is_empty());
}

/// The re-entry swap: box A on a fresh alternate screen, then the TUI leaves and
/// re-enters and draws box B with the same number of writes. The per-grid
/// `content_seq` repeats (the negative control) while the screen generation moves,
/// so `if-gen=<A>` writes nothing, in one batch and across two (a batch that exits
/// and re-enters sees no alt-flag change).
#[test]
#[cfg(unix)]
fn a_re_entered_alternate_screen_never_passes_the_generation_fence() {
    use control_input::InputFence;
    const ALT_A: &[u8] = b"\x1b[?1049h\x1b[H Bash command\r\n   ls -la /tmp/work\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n";
    const ALT_B: &[u8] = b"\x1b[H Bash command\r\n   rm -rf /tmp/work\r\n Do you want to proceed?\r\n \xe2\x9d\xaf 1. Yes\r\n";
    for split in [false, true] {
        let (h, rx) = guarded_pipe_session(1);
        crate::term_lock(&h.term).process(ALT_A);
        let gen_a = screen_gen(&crate::term_lock(&h.term));
        if split {
            crate::term_lock(&h.term).process(b"\x1b[?1049l");
            crate::term_lock(&h.term).process(&[b"\x1b[?1049h".as_slice(), ALT_B].concat());
        } else {
            crate::term_lock(&h.term)
                .process(&[b"\x1b[?1049l\x1b[?1049h".as_slice(), ALT_B].concat());
        }
        let gen_b = screen_gen(&crate::term_lock(&h.term));
        assert!(
            visible_row(&crate::term_lock(&h.term), 1).contains("rm -rf"),
            "PRECONDITION (split={split}): box B is up"
        );
        assert_eq!(
            gen_a.seq, gen_b.seq,
            "NEGATIVE CONTROL (split={split}): the per-grid seq repeats"
        );
        assert_ne!(gen_a, gen_b, "split={split}: the generation moved");
        let press = |generation| {
            control_input::cmd_key_fenced(
                &h.term,
                &h.ctx,
                &InputFence {
                    generation: Some(generation),
                    ..InputFence::default()
                },
                "1",
            )
        };
        assert_eq!(press(gen_a), "OK skipped reason=changed\n");
        assert!(drain_pipe(&rx).is_empty(), "split={split}: nothing written");
        // B's own generation presses: the fence is deciding, not stuck.
        assert_eq!(press(gen_b), "OK\n");
        assert_eq!(drain_pipe(&rx), b"1");
    }
}

/// The person fence (`key|send if-human=<n>`) through the real App input seam: a
/// person's `↓` (`Source::Human`) reaches the PTY and moves the person count by one;
/// a key or send fenced on the count read before it answers
/// `OK skipped reason=person` (zero bytes, content seq unmoved and stamped). The
/// person check runs first, so a stale generation too still says `reason=person`.
/// A fenced write never moves the person count.
///
/// Negative controls: fenced on the current generation alone the key presses, and
/// fenced on the count read after the person's `↓` the key and send both land.
#[test]
#[cfg(unix)]
fn a_person_fenced_write_is_refused_after_a_persons_key_through_the_app_seam() {
    use crate::input::Source;
    use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
    use control_input::InputFence;
    let (rd, wr) = cloexec_pipe();
    let rx = unsafe { std::fs::File::from_raw_fd(rd) };
    aterm_pty::set_nonblocking(wr, true).expect("nonblocking test master");
    let sink = Arc::new(SinkWriter::new(wr));
    sink.note_master_nonblocking(true);
    let mut app = crate::App::headless_for_test_with_sink(sink);
    let (term, ctx) = {
        let s0 = app.pool.get(0).expect("session 0");
        (s0.term.clone(), s0.ctx.clone())
    };
    let before = ctx.human_input.seq();
    let down = InputEvent::Key {
        key: Key::Named(NamedKey::ArrowDown),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type: KeyEventType::Press,
    };
    let _ = app.input(crate::WindowId(0), down, Source::Human);
    assert!(
        !drain_pipe(&rx).is_empty(),
        "the person's arrow reached the PTY"
    );
    assert_eq!(
        ctx.human_input.seq(),
        before + 1,
        "and moved the session's person count by one"
    );
    let counted = ctx.human_input.seq();

    let generation = screen_gen(&crate::term_lock(&term));
    let seq = crate::term_lock(&term).content_seq();
    let read_before = InputFence {
        human: Some(before),
        ..InputFence::default()
    };
    let reply = control_input::cmd_key_fenced(&term, &ctx, &read_before, "down");
    assert_eq!(reply, "OK skipped reason=person\n");
    assert!(drain_pipe(&rx).is_empty(), "nothing written");
    assert_eq!(crate::term_lock(&term).content_seq(), seq);
    assert_eq!(
        control_input::cmd_send_fenced(&term, &ctx, &read_before, "keep going"),
        "OK skipped reason=person\n"
    );
    assert!(drain_pipe(&rx).is_empty(), "a fenced send: nothing written");
    let stale = ScreenGen {
        epoch: generation.epoch,
        seq: generation.seq.wrapping_add(7),
    };
    assert_eq!(
        control_input::cmd_key_fenced(
            &term,
            &ctx,
            &InputFence {
                generation: Some(stale),
                human: Some(before),
                ..InputFence::default()
            },
            "down"
        ),
        "OK skipped reason=person\n",
        "the person is asked before the screen"
    );
    assert!(drain_pipe(&rx).is_empty());
    assert_eq!(ctx.human_input.seq(), counted, "a skip moves no count");

    // Negative control 1: without the option the same key presses.
    let current = InputFence {
        generation: Some(generation),
        ..InputFence::default()
    };
    assert_eq!(
        control_input::cmd_key_fenced(&term, &ctx, &current, "down"),
        "OK\n"
    );
    assert!(
        !drain_pipe(&rx).is_empty(),
        "the key not fenced on the person landed"
    );

    // Negative control 2: fenced on the count read after the person's key, the
    // key and the send both land.
    let read_after = InputFence {
        human: Some(ctx.human_input.seq()),
        ..InputFence::default()
    };
    assert_eq!(
        control_input::cmd_key_fenced(&term, &ctx, &read_after, "down"),
        "OK\n"
    );
    assert!(!drain_pipe(&rx).is_empty(), "the person-fenced key landed");
    assert_eq!(
        control_input::cmd_send_fenced(&term, &ctx, &read_after, "x"),
        "OK\n"
    );
    assert_eq!(drain_pipe(&rx), b"x");
    assert_eq!(
        ctx.human_input.seq(),
        counted,
        "no fenced write — skipped or written — moved the person count"
    );
}

/// `turn … submit=guarded:<re>`: the text is typed, and Enter is pressed
/// only while `<re>` still matches. A box that replaced the composer during
/// the paste settle gets NO Enter, and the reply says so in the verb's
/// Lines framing. NEGATIVE CONTROL: the same turn against a composer that
/// stays up writes exactly one Enter.
#[test]
#[cfg(unix)]
fn a_guarded_turn_submit_never_presses_enter_into_a_screen_its_guard_left() {
    use std::cell::RefCell;
    let store = session_store::new_store();
    let (h, rx) = guarded_pipe_session(0);
    store.write().unwrap().register(h.clone());
    const COMPOSER: &[u8] =
        b"\x1b[2J\x1b[H\xe2\x94\x80\xe2\x94\x80\r\n\xe2\x9d\xaf \r\n\xe2\x94\x80\xe2\x94\x80\r\n";
    crate::term_lock(&h.term).process(COMPOSER);

    let pasted: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let swap_in_box = |text: &str| {
        pasted.borrow_mut().push(text.to_string());
        crate::term_lock(&h.term).process(BOX_B);
        true
    };
    let press = |_: &str| panic!("a guarded submit presses through the fence, never io.press");
    let turn = |paste: &dyn Fn(&str) -> bool, line: &str| {
        cmd_turn(
            &h.term,
            &store,
            0,
            line,
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        )
    };
    let out = turn(
        &swap_in_box,
        "idle=20 timeout=3000 submit_window=100 presses=1 submit=guarded:^\u{276f} hello",
    );
    let seq = crate::term_lock(&h.term).content_seq();
    assert!(
        out.starts_with(&format!(
            "OK 0 turn skipped reason=guard submitted=0 seq={seq} id="
        )),
        "{out}"
    );
    assert_eq!(out.lines().count(), 1, "zero rows follow: {out}");
    assert_eq!(pasted.borrow().as_slice(), ["hello"], "the text WAS typed");
    assert!(drain_pipe(&rx).is_empty(), "no Enter reached the PTY");

    // Negative control: the composer stays up, so the guard matches and exactly
    // one Enter is written.
    crate::term_lock(&h.term).process(COMPOSER);
    let echo = |text: &str| {
        crate::term_lock(&h.term).process(format!("\x1b[2;3H{text}").as_bytes());
        true
    };
    let out = turn(
        &echo,
        "idle=20 timeout=3000 submit_window=100 presses=1 submit=guarded:^\u{276f} hello",
    );
    assert!(out.starts_with("OK "), "{out}");
    assert!(!out.contains("skipped"), "{out}");
    assert!(out.lines().next().unwrap().ends_with(" pressed=1"), "{out}");
    assert_eq!(drain_pipe(&rx), b"\r", "exactly one Enter");

    // A bad or empty pattern fails before anything is typed.
    pasted.borrow_mut().clear();
    assert_eq!(
        turn(&swap_in_box, "submit=guarded:( hello"),
        "ERR badregex\n"
    );
    assert!(turn(&swap_in_box, "submit=guarded: hello").starts_with("ERR usage: turn"));
    assert!(
        pasted.borrow().is_empty(),
        "nothing typed on a refused pattern"
    );
    assert!(drain_pipe(&rx).is_empty());
}

/// Claude Code draws earlier prompts with the composer's `❯` at column 0, so `^❯`
/// still matches a transcript row after a box replaced the composer (the negative
/// control's any-row scan finds it). The guarded submit matches only the cursor's
/// row, so it writes nothing; with the cursor on a live composer below the same
/// transcript row it presses exactly once.
#[test]
#[cfg(unix)]
fn a_guarded_submit_is_bound_to_the_cursor_row_not_a_transcript_caret() {
    let store = session_store::new_store();
    let (h, rx) = guarded_pipe_session(0);
    store.write().unwrap().register(h.clone());
    const RM_BOX: &str = "\x1b[2J\x1b[H\u{276f} Use the Bash tool to run exactly this one line, verbatim, as one call: S=$PWD/tmp; for p in a b; do set -- $p; rm -rf\r\n  $S/$1; done\r\n\r\n  \u{23bf}  $ S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done\r\n\u{2500}\u{2500}\u{2500}\r\n Bash command\r\n\r\n   S=$PWD/tmp; for p in a b; do set -- $p; rm -rf $S/$1; done\r\n   Remove directories tmp/a and tmp/b\r\n\r\n Do you want to proceed?\r\n \u{276f} 1. Yes\r\n   2. No\r\n\r\n Esc to cancel \u{00b7} Tab to amend";
    const COMPOSER: &str = "\x1b[2J\x1b[H\u{276f} earlier prompt the human typed\r\n\r\n\u{2500}\u{2500}\u{2500}\r\n\u{276f} \r\n\u{2500}\u{2500}\u{2500}\r\n  ? for shortcuts\x1b[4;3H";
    crate::term_lock(&h.term).process(COMPOSER.as_bytes());
    let press = |_: &str| panic!("a guarded submit presses through the fence, never io.press");
    let turn = |paste: &dyn Fn(&str) -> bool| {
        cmd_turn(
            &h.term,
            &store,
            0,
            "idle=20 timeout=3000 submit_window=100 presses=1 submit=guarded:^\u{276f} hello",
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste,
                press: &press,
                ..TurnIo::paste_only()
            },
        )
    };

    let box_arrives = |_: &str| {
        crate::term_lock(&h.term).process(RM_BOX.as_bytes());
        true
    };
    let out = turn(&box_arrives);
    let caret = control_input::compile_guard(Some("^\u{276f}"))
        .unwrap()
        .unwrap();
    assert_eq!(
        aterm_core::terminal::first_matching_row(
            &*crate::term_lock(&h.term),
            caret.as_ref(),
            aterm_core::terminal::RowRange::All
        ),
        Some(0),
        "NEGATIVE CONTROL: an any-row guard matches the transcript prompt"
    );
    assert!(
        out.starts_with("OK 0 turn skipped reason=guard submitted=0 "),
        "{out}"
    );
    assert!(drain_pipe(&rx).is_empty(), "no Enter into the rm box");

    // The composer is back, the transcript row still above it, the cursor
    // on the composer's row: one Enter.
    crate::term_lock(&h.term).process(COMPOSER.as_bytes());
    let echo = |text: &str| {
        crate::term_lock(&h.term).process(format!("\x1b[4;3H{text}").as_bytes());
        true
    };
    let out = turn(&echo);
    assert!(!out.contains("skipped"), "{out}");
    assert!(out.lines().next().unwrap().ends_with(" pressed=1"), "{out}");
    assert_eq!(drain_pipe(&rx), b"\r", "exactly one Enter");
}

/// Keyed attention over the wire: two owners set and clear their own entries,
/// `meta` shows the most recent with its owner, the bare form is owner `-`, and
/// malformed owners or a full map are refused with nothing stored.
#[test]
fn keyed_attention_on_the_wire_is_per_owner_and_the_bare_form_still_works() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let meta = |rest: &str| cmd_meta(&h.term, &store, 0, &h.ctx, rest);
    let read = || meta("").0;

    assert_eq!(
        meta("set attention owner=sup claude needs approval: rm -rf tmp"),
        ("OK\n".to_string(), true)
    );
    assert_eq!(
        meta("set attention owner=human check the deploy"),
        ("OK\n".to_string(), true)
    );
    let line = read();
    assert!(line.contains(" attention=check%20the%20deploy "), "{line}");
    assert!(
        line.contains(" attention_owner=human attention_owners=2 "),
        "{line}"
    );
    assert_eq!(
        meta("unset attention owner=human"),
        ("OK\n".to_string(), true)
    );
    let line = read();
    assert!(
        line.contains(" attention=claude%20needs%20approval:%20rm%20-rf%20tmp "),
        "{line}"
    );
    assert!(
        line.contains(" attention_owner=sup attention_owners=1 "),
        "{line}"
    );

    // The bare form: owner `-` (printed `%2D`), cleared by the bare unset
    // only — the keyed entry outlives it.
    assert_eq!(
        meta("set attention legacy badge"),
        ("OK\n".to_string(), true)
    );
    assert!(read().contains(" attention=legacy%20badge "));
    assert!(read().contains(" attention_owner=%2D attention_owners=2 "));
    assert_eq!(meta("unset attention"), ("OK\n".to_string(), true));
    assert!(read().contains(" attention_owner=sup "));
    // A bare text that merely CONTAINS owner= is text.
    assert_eq!(
        meta("set attention see owner=sup"),
        ("OK\n".to_string(), true)
    );
    assert!(read().contains(" attention=see%20owner=sup "), "{}", read());
    meta("unset attention");

    // Refusals store nothing.
    assert!(
        meta("set attention owner= text")
            .0
            .starts_with("ERR usage: owner=")
    );
    assert!(
        meta("set attention owner=has|bar text")
            .0
            .starts_with("ERR usage: owner=")
    );
    assert!(
        meta("set attention owner=sup")
            .0
            .starts_with("ERR usage: meta set attention")
    );
    assert!(
        meta("unset attention owner=sup extra")
            .0
            .starts_with("ERR usage")
    );
    // `sup` plus six more is seven keyed owners; the eighth slot is the
    // bare owner's, so a new keyed owner is refused and the bare one is not.
    for i in 0..6 {
        assert_eq!(
            meta(&format!("set attention owner=o{i} x")).0,
            "OK\n",
            "o{i}"
        );
    }
    assert!(
        meta("set attention owner=o6 x")
            .0
            .starts_with("ERR attention owners full")
    );
    assert!(read().contains(" attention_owners=7 "), "{}", read());
    assert_eq!(meta("set attention bare").0, "OK\n");
    assert!(read().contains(" attention_owners=8 "), "{}", read());
}

/// The supervisor key: Owner-writable only, shown on `meta` and every `sessions`
/// row, exclusive between holders, and a connection-bound claim is released by its
/// own connection's end only.
#[test]
fn the_supervisor_key_is_owner_only_shown_in_the_roster_and_released_with_its_connection() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    // The row up to `supervisor=`; `autopilot=` (`-`: no agent runs here),
    // the owner's columns (`path_evidence= copy= upgrade=`) and the harness's
    // `land_at= task=` follow it.
    let row = || {
        let body = control_session::cmd_sessions_store(&store, None);
        let line = body.lines().nth(1).expect("one row");
        let (head, owner) = line.split_once(" path_evidence=").expect("owner columns");
        assert!(
            owner.ends_with(" copy=- upgrade=- land_at=- task=-"),
            "{line}"
        );
        head.strip_suffix(" autopilot=-")
            .unwrap_or_else(|| panic!("autopilot= beside supervisor=: {line}"))
            .to_string()
    };
    assert!(row().ends_with(" supervisor=-"), "{}", row());

    let sup = |scope: Scope, conn: Option<u64>, rest: &str| {
        control_session::cmd_meta_supervisor(&h.ctx, scope, conn, &|_| None, rest)
    };
    // Not a supervisor write: falls through to `meta`.
    assert_eq!(sup(Scope::Owner, Some(1), "set title x"), None);
    // An edge may not claim to supervise.
    assert_eq!(
        sup(edge(), Some(1), "set supervisor sup-a"),
        Some(("ERR denied\n".to_string(), false))
    );
    assert!(row().ends_with(" supervisor=-"));

    assert_eq!(
        sup(Scope::Owner, Some(41), "set supervisor sup-a"),
        Some(("OK\n".to_string(), true))
    );
    assert!(row().ends_with(" supervisor=sup-a"), "{}", row());
    assert!(
        cmd_meta(&h.term, &store, 0, &h.ctx, "")
            .0
            .contains(" supervisor=sup-a supervisor_proven=0 ")
    );
    assert_eq!(
        sup(Scope::Owner, Some(42), "set supervisor sup-b"),
        Some(("ERR busy supervisor=sup-a\n".to_string(), false))
    );
    for bad in [
        "set supervisor",
        "set supervisor -",
        "set supervisor a b",
        "set supervisor a ttl=0",
        "set supervisor a ttl=600001",
        "unset supervisor now",
        "unset supervisor holder=",
        "unset supervisor holder=a b",
    ] {
        assert!(
            sup(Scope::Owner, Some(41), bad)
                .unwrap()
                .0
                .starts_with("ERR usage"),
            "{bad}"
        );
    }

    // Another connection ending releases nothing; its own does.
    let subscribers = subscribe::new_registry();
    release_connection_claims(&store, &subscribers, None, 40);
    assert!(row().ends_with(" supervisor=sup-a"));
    release_connection_claims(&store, &subscribers, None, 41);
    assert!(row().ends_with(" supervisor=-"), "{}", row());

    // A `ttl=` claim is not bound to the connection that set it.
    assert_eq!(
        sup(Scope::Owner, Some(43), "set supervisor sup-b ttl=60000"),
        Some(("OK\n".to_string(), true))
    );
    release_connection_claims(&store, &subscribers, None, 43);
    assert!(row().ends_with(" supervisor=sup-b"), "{}", row());
    // `holder=` gives back only the named holder's claim: another's
    // stays (a stopped supervisor never clears its successor's)…
    assert_eq!(
        sup(Scope::Owner, None, "unset supervisor holder=sup-a"),
        Some(("OK\n".to_string(), false))
    );
    assert!(row().ends_with(" supervisor=sup-b"), "{}", row());
    assert_eq!(
        sup(Scope::Owner, None, "unset supervisor holder=sup-b"),
        Some(("OK\n".to_string(), true))
    );
    assert!(row().ends_with(" supervisor=-"));
    // …while the bare unset clears whichever holder's it is.
    assert_eq!(
        sup(Scope::Owner, Some(44), "set supervisor sup-c ttl=60000"),
        Some(("OK\n".to_string(), true))
    );
    assert_eq!(
        sup(Scope::Owner, None, "unset supervisor"),
        Some(("OK\n".to_string(), true))
    );
    assert!(row().ends_with(" supervisor=-"));
}

/// Build an [`ActiveHandle`] over a `pipe_session` handle (cross-session feed-bin
/// tests resolve `@.`/self through it as the production `serve` loop does).
#[cfg(unix)]
fn active_for_handle(h: &crate::session_store::SessionHandle) -> ActiveHandle {
    Arc::new(Mutex::new(Some(ActiveSession {
        term: h.term.clone(),
        master: h.master,
        id: h.local_id,
        ctx: h.ctx.clone(),
    })))
}

// ---- the closed-session index (session log phase 2A) -------------------------

/// A headless app with a second session (a stub tab) that wrote a few records,
/// granted `grants` to session 0 as an edge's source. The caller closes it
/// through the shipping retire. Returns the app, the session's ctx (kept
/// alive as a late holder would) and the tokens.
fn closed_session_fixture(
    grants: &[aterm_session::Op],
) -> (
    crate::App,
    std::sync::Arc<crate::SessionCtx>,
    Vec<aterm_session::EdgeToken>,
) {
    let mut app = crate::App::headless_for_test();
    let source = app.pool.get(0).expect("session 0").ctx.self_id.clone();
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
    let local = app.next_session_id - 1;
    let ctx = std::sync::Arc::clone(&app.pool.get(local).expect("the stub").ctx);
    {
        let mut tl = ctx.timeline.lock().unwrap();
        let now = tl.slog().now();
        tl.slog_mut().open_session(None, now);
        for _ in 0..3 {
            tl.slog_mut().human_input(true, now);
        }
    }
    let tokens = grants
        .iter()
        .map(|op| {
            ctx.edges
                .lock()
                .unwrap()
                .grant(source.clone(), ctx.self_id.clone(), *op, ctx.nonce)
        })
        .collect();
    (app, ctx, tokens)
}

/// The session's local id in `app`'s registry, before it closes.
fn local_of(app: &crate::App, ctx: &crate::SessionCtx) -> u64 {
    app.store
        .read()
        .unwrap()
        .by_sid(&ctx.self_id)
        .expect("registered")
        .local_id
}

/// `LOG AND BRIEF OUTLIVE THE SESSION` for the owner: after the close, `log
/// @<sid>` (and `@<local>`) answers what `log` read of it at its close, the
/// close record included, in either `json` spelling, and `brief` its rollup.
/// CONTROL: before the close the index holds nothing for it, and another verb,
/// or a sid nobody closed, still answers `ERR no such session`.
#[test]
fn log_and_brief_answer_a_closed_session_for_the_owner() {
    use super::{Scope, Selector, closed_session_reply};
    let (mut app, ctx, _) = closed_session_fixture(&[]);
    let sid = Selector::Sid(ctx.self_id.clone());
    let local = local_of(&app, &ctx);
    assert_eq!(
        closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Owner),
        "ERR no such session\n",
        "CONTROL: nothing kept while it lives"
    );
    app.retire_session_registration(local);
    let at_close = super::control_session::cmd_log(&ctx, "");
    assert!(at_close.contains(" close - "), "{at_close}");
    for sel in [Selector::Sid(ctx.self_id.clone()), Selector::Local(local)] {
        assert_eq!(
            closed_session_reply(Some(&sel), "log", "", &app.store, Scope::Owner),
            at_close
        );
    }
    let json = super::control_session::cmd_log_json(&ctx, "");
    for spelling in ["--json", "json"] {
        assert_eq!(
            closed_session_reply(Some(&sid), "log", spelling, &app.store, Scope::Owner),
            json
        );
    }
    let since = closed_session_reply(Some(&sid), "log", "since=2 ep=1", &app.store, Scope::Owner);
    assert_eq!(since, super::control_session::cmd_log(&ctx, "since=2 ep=1"));
    let brief = closed_session_reply(Some(&sid), "brief", "", &app.store, Scope::Owner);
    assert!(
        brief.starts_with(&format!(
            "OK {} brief sid={} v=2 ",
            brief.lines().count() - 1,
            ctx.self_id.as_str()
        )),
        "{brief}"
    );
    assert_eq!(
        closed_session_reply(Some(&sid), "text", "", &app.store, Scope::Owner),
        "ERR no such session\n"
    );
    assert_eq!(
        closed_session_reply(
            Some(&Selector::Sid(aterm_session::SessionId::generate())),
            "log",
            "",
            &app.store,
            Scope::Owner
        ),
        "ERR no such session\n"
    );
}

/// AN EDGE LEARNS NOTHING OF A CLOSED SESSION IT COULD NOT READ: one granted
/// `ReadScreen` on the session reads its log and brief after the close (the
/// live rule on the grants it had); one granted only `WriteInput`, or holding
/// no grant at all, gets `ERR no such session` word for word, as for a sid
/// nobody closed; `brief all` stays Owner-only. When the grant's source closes,
/// its grant goes from the index too. NEGATIVE CONTROL: the same read edge reads
/// the session while it lives, and the write edge does not.
#[test]
fn an_edge_learns_nothing_of_a_closed_session() {
    use super::{Scope, Selector, closed_session_reply, cross_session_authorized};
    use aterm_session::Op;
    let (mut app, ctx, tokens) = closed_session_fixture(&[Op::ReadScreen, Op::WriteInput]);
    let (read, write) = (Scope::Edge(tokens[0]), Scope::Edge(tokens[1]));
    assert!(
        cross_session_authorized(read, "log", &ctx),
        "NEGATIVE CONTROL: reads live"
    );
    assert!(
        !cross_session_authorized(write, "log", &ctx),
        "NEGATIVE CONTROL"
    );
    let local = local_of(&app, &ctx);
    app.retire_session_registration(local);
    let sid = Selector::Sid(ctx.self_id.clone());
    let owner = closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Owner);
    assert!(owner.starts_with("OK "), "{owner}");
    assert_eq!(
        closed_session_reply(Some(&sid), "log", "", &app.store, read),
        owner
    );
    assert!(closed_session_reply(Some(&sid), "brief", "", &app.store, read).starts_with("OK "));
    for scope in [write, Scope::Edge(aterm_session::EdgeToken::generate())] {
        for verb in ["log", "brief"] {
            assert_eq!(
                closed_session_reply(Some(&sid), verb, "", &app.store, scope),
                "ERR no such session\n"
            );
        }
    }
    assert_eq!(
        closed_session_reply(Some(&sid), "brief", "all", &app.store, read),
        "ERR denied\n"
    );
    // The grant's source (session 0) closes: its grant leaves the index.
    app.retire_session_registration(0);
    assert_eq!(
        closed_session_reply(Some(&sid), "log", "", &app.store, read),
        "ERR no such session\n"
    );
    assert!(
        closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Owner).starts_with("OK ")
    );
}

#[test]
fn a_closing_sessions_own_read_grant_dies_with_its_source() {
    use super::{Scope, Selector, closed_session_reply, cross_session_authorized};
    use aterm_session::{EdgeToken, Op};

    let (mut app, ctx, other_read) = closed_session_fixture(&[Op::ReadScreen]);
    let grant = super::control_session::cmd_grant(
        &ctx,
        Scope::Owner,
        &format!("{} read-screen", ctx.self_id.as_str()),
    );
    let own_read = EdgeToken::from_hex(grant.strip_prefix("OK ").unwrap().trim()).unwrap();
    assert!(
        cross_session_authorized(Scope::Edge(own_read), "log", &ctx),
        "the owner-granted token reads the live session"
    );

    let local = local_of(&app, &ctx);
    app.retire_session_registration(local);
    let sid = Selector::Sid(ctx.self_id.clone());
    let owner = closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Owner);
    assert!(owner.starts_with("OK "), "the closed log remains: {owner}");
    assert_eq!(
        closed_session_reply(
            Some(&sid),
            "log",
            "",
            &app.store,
            Scope::Edge(other_read[0])
        ),
        owner,
        "a source that remains live keeps its read grant"
    );
    assert_eq!(
        closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Edge(own_read)),
        "ERR no such session\n",
        "the closing source loses its grant even on its own retained log"
    );
}

/// Closed read grants can be revoked by token from another owner's session,
/// or dissolved by source. No unknown token or edge-scoped caller gains
/// authority, and a successful revoke removes the token exactly once.
#[test]
fn a_closed_sessions_read_grants_stay_revocable_and_never_answer_twice() {
    use super::control_session::{cmd_disconnect_in, cmd_revoke_with_closed};
    use super::{Scope, Selector, closed_session_reply};
    use aterm_session::Op;
    let (mut app, ctx, tokens) =
        closed_session_fixture(&[Op::ReadScreen, Op::ReadScreen, Op::ReadScreen]);
    let owner_ctx = std::sync::Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let source = owner_ctx.self_id.clone();
    let local = local_of(&app, &ctx);
    app.retire_session_registration(local);
    let sid = Selector::Sid(ctx.self_id.clone());
    let reads = |scope: Scope| {
        closed_session_reply(Some(&sid), "log", "", &app.store, scope).starts_with("OK ")
    };
    let (edge, hex) = (Scope::Edge(tokens[0]), tokens[0].to_hex());
    assert!(
        reads(edge),
        "NEGATIVE CONTROL: the grant reads the closed session"
    );
    // The verb, from another session's owner connection, reaches the index.
    assert_eq!(
        cmd_revoke_with_closed(&owner_ctx, &app.store, edge, &hex),
        "ERR denied\n"
    );
    assert_eq!(
        cmd_revoke_with_closed(&owner_ctx, &app.store, Scope::Owner, &hex),
        "OK\n"
    );
    assert!(!reads(edge), "revoked: it reads nothing");
    assert_eq!(
        cmd_revoke_with_closed(&owner_ctx, &app.store, Scope::Owner, &hex),
        "ERR no such edge\n",
        "once"
    );
    assert_eq!(
        cmd_revoke_with_closed(
            &owner_ctx,
            &app.store,
            Scope::Owner,
            &aterm_session::EdgeToken::generate().to_hex()
        ),
        "ERR no such edge\n",
        "NEGATIVE CONTROL: a token no table holds"
    );
    // `disconnect` dissolves the source's grants on the closed session.
    let second = Scope::Edge(tokens[1]);
    assert!(reads(second), "NEGATIVE CONTROL");
    let reply = cmd_disconnect_in(
        &crate::connections::ConnectionStore::default(),
        &app.store,
        Scope::Owner,
        &format!("dst={} src={}", ctx.self_id.as_str(), source.as_str()),
    );
    assert_eq!(reply, "OK 2\n", "both grants it still held");
    assert!(!reads(second) && !reads(Scope::Edge(tokens[2])));
}

/// A disconnect that already resolved its live destination still revokes
/// the same grants after retirement, before or after its connection record
/// is consumed. An independently minted read grant must survive that precise
/// disconnect; write grants do not survive retirement.
#[test]
fn a_resolved_disconnect_revokes_the_same_read_grant_across_retirement() {
    use super::{Scope, Selector, closed_session_reply};
    use aterm_session::{ConnectionKind, Op};
    for close_first in [false, true] {
        let (mut app, ctx, raw) = closed_session_fixture(&[Op::ReadScreen]);
        let source = app.pool.get(0).unwrap().ctx.self_id.clone();
        let dst = ctx.self_id.clone();
        let local = local_of(&app, &ctx);
        // A private record store keeps the already-resolved disconnect's
        // record available while retirement completes its other work. This
        // models that request holding its record before the close sweep.
        let conn = crate::connections::ConnectionStore::default();
        assert!(crate::connections::connect_in(
            &conn,
            &source,
            &dst,
            &ctx.edges,
            &ctx.nonce,
            ConnectionKind::Both,
            "wire"
        ));
        let minted = conn.records()[&(source.clone(), dst.clone())]
            .tokens
            .clone();
        let read = minted
            .iter()
            .find(|(op, _)| *op == Op::ReadScreen)
            .unwrap()
            .1;
        let resolved = {
            let store = app.store.read().unwrap();
            Arc::clone(&store.by_sid(&dst).unwrap().ctx.edges)
        };
        if close_first {
            app.retire_session_registration(local);
        }
        assert_eq!(
            resolved.lock().unwrap().authorize(&read, &dst, &ctx.nonce),
            Some(Op::ReadScreen),
            "positive control: the connection read is still granted"
        );
        assert_eq!(
            crate::connections::disconnect_kind_in(&conn, &source, &dst, &resolved, None, "wire"),
            Some(if close_first { 1 } else { 3 }),
            "retirement prunes writes but keeps the read in the same table"
        );
        if !close_first {
            app.retire_session_registration(local);
        }
        let selector = Selector::Sid(dst.clone());
        let can_read = |token| {
            closed_session_reply(Some(&selector), "log", "", &app.store, Scope::Edge(token))
                .starts_with("OK ")
        };
        assert!(
            !can_read(read),
            "the disconnected read never survives in the index"
        );
        assert!(
            can_read(raw[0]),
            "the independent read was not part of the connection"
        );
        for (op, token) in &minted {
            assert_eq!(
                ctx.edges.lock().unwrap().authorize(token, &dst, &ctx.nonce),
                None,
                "{op:?}"
            );
        }
        assert_eq!(
            super::control_session::cmd_revoke(&ctx, Scope::Owner, &raw[0].to_hex()),
            "OK\n",
            "an already-resolved revoke reaches the retained table too"
        );
        assert!(!can_read(raw[0]));
    }
}

/// Both revoke forms call the real audit sink only after releasing their
/// authority table. A logger callback must be able to inspect that table.
#[test]
fn revocation_audits_run_outside_the_authority_table_lock() {
    use super::Scope;
    use super::control_session::cmd_revoke;
    use aterm_session::Op;
    let (app, ctx, tokens) = closed_session_fixture(&[Op::ReadScreen, Op::ReadScreen]);
    let source = app.pool.get(0).unwrap().ctx.self_id.clone();
    let edges = Arc::clone(&ctx.edges);
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = Arc::clone(&seen);
    crate::test_log::observe(
        move |record| {
            if record.target() == "session_edge" {
                assert!(
                    edges.try_lock().is_ok(),
                    "audit nested inside the edge table"
                );
                observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
        || {
            assert_eq!(cmd_revoke(&ctx, Scope::Owner, &tokens[0].to_hex()), "OK\n");
            assert_eq!(
                cmd_revoke(&ctx, Scope::Owner, &format!("src={}", source.as_str())),
                "OK 1\n"
            );
        },
    );
    assert_eq!(
        seen.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "both audits reached the sink"
    );
}

/// An owner grant already admitted while live may finish after retirement.
/// Even an initially empty closed table remains reachable by token revoke
/// and by the grant source's close.
#[test]
fn a_late_owner_grant_on_an_initially_empty_closed_table_remains_revocable() {
    use super::control_session::{cmd_grant, cmd_revoke_with_closed};
    use super::{Scope, Selector, closed_session_reply};
    for source_closes in [false, true] {
        let (mut app, ctx, _) = closed_session_fixture(&[]);
        let owner = Arc::clone(&app.pool.get(0).unwrap().ctx);
        app.retire_session_registration(local_of(&app, &ctx));
        let reply = cmd_grant(
            &ctx,
            Scope::Owner,
            &format!("{} read-screen", owner.self_id.as_str()),
        );
        let token =
            aterm_session::EdgeToken::from_hex(reply.strip_prefix("OK ").unwrap().trim_end())
                .unwrap();
        let selector = Selector::Sid(ctx.self_id.clone());
        let reads = |app: &crate::App| {
            closed_session_reply(Some(&selector), "log", "", &app.store, Scope::Edge(token))
                .starts_with("OK ")
        };
        assert!(reads(&app), "the admitted owner grant completed");
        if source_closes {
            app.retire_session_registration(0);
        } else {
            assert_eq!(
                cmd_revoke_with_closed(&owner, &app.store, Scope::Owner, &token.to_hex()),
                "OK\n"
            );
        }
        assert!(
            !reads(&app),
            "a zero close-time row count never hides a later grant"
        );
    }
}

/// AN AMBIGUOUS CLOSED SID IS REFUSED as a live one is: a live foreign instance
/// publishing the id the closed session had gets the ambiguity refusal, never
/// this process's answer for it. CONTROL: without that entry it reads.
#[test]
fn an_ambiguous_closed_sid_is_refused() {
    use super::{Scope, Selector, closed_session_reply};
    let _guard = crate::identity_claim::rendezvous_test_guard();
    let dir = aterm_tempfile::tempdir().expect("scratch control dir");
    crate::identity_claim::set_rendezvous_override(Some(dir.path().to_path_buf()));
    let (mut app, ctx, _) = closed_session_fixture(&[]);
    let local = local_of(&app, &ctx);
    app.retire_session_registration(local);
    crate::identity_claim::note_adopted(&ctx.self_id);
    let sid = Selector::Sid(ctx.self_id.clone());
    assert!(
        closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Owner).starts_with("OK "),
        "CONTROL: nobody else serves it"
    );
    std::fs::create_dir_all(dir.path().join("graph")).expect("graph dir");
    std::fs::write(
        dir.path().join("graph").join(ctx.self_id.as_str()),
        live_foreign_entry(),
    )
    .expect("plant a foreign entry");
    let refusal = closed_session_reply(Some(&sid), "log", "", &app.store, Scope::Owner);
    assert!(
        refusal.starts_with("ERR ambiguous session id ")
            && refusal.contains(&format!("pid {LIVE_FOREIGN_PID}")),
        "{refusal}"
    );
    crate::identity_claim::set_rendezvous_override(None);
}

/// A journal under `<scratch>/logs` holding one dead chain for `sid`, as an
/// earlier process that died wrote it: the session's open and a run under it
/// (seqs 1 and 2), in a segment of instance `p1-aa`, whose lock nobody holds,
/// so a read derives its crash closes. Returns the logs folder.
#[cfg(unix)]
fn journal_with_a_dead_chain(
    scratch: &std::path::Path,
    sid: &aterm_session::SessionId,
) -> std::path::PathBuf {
    use aterm_slog::journal::{FORMAT, Frame, Inst, encode_frame};
    let _ = std::fs::remove_dir_all(scratch);
    let logs = scratch.join("logs");
    std::fs::create_dir_all(&logs).expect("logs");
    let dir = crate::session_log_journal::fs::PinnedJournalFs::open(&logs, true)
        .expect("opens")
        .expect("made");
    let short = aterm_slog::ShortId::new(sid.as_str()).expect("a sid is an id");
    let mut log = aterm_slog::Log::new(1);
    let at = aterm_slog::Stamp::new(1_760_600_000_000, 0);
    log.open_session(
        aterm_slog::Kind::Spawn,
        aterm_slog::Src::Aterm,
        aterm_slog::Conf::Exact,
        aterm_slog::Actor::Nobody,
        aterm_slog::Fields::new(),
        at,
    )
    .expect("opens");
    let _ = log
        .open(
            aterm_slog::Open::new(
                aterm_slog::Kind::Cmd,
                aterm_slog::Src::Shell,
                aterm_slog::Conf::Strong,
            ),
            at,
        )
        .expect("a run");
    let inst = Inst::new("p1-aa").expect("an instance");
    let mut seg = {
        use crate::session_log_journal::fs::JournalFs as _;
        dir.create_segment("p1-aa.1.slog").expect("a segment")
    };
    let mut bytes = String::new();
    let take = aterm_slog::journal::take(&log, 0, 512, true);
    for f in std::iter::once(Frame::Seg {
        v: FORMAT,
        inst,
        n: 1,
        t: at.wall_ms,
        build: "test".to_string(),
    })
    .chain(aterm_slog::journal::take_frames(
        short, &take, true, at.wall_ms,
    )) {
        bytes.push_str(&encode_frame(&f).expect("encodes"));
        bytes.push('\n');
    }
    let _ = seg.write(bytes.as_bytes()).expect("writes");
    logs
}

/// `log @<sid>` and `brief @<sid>` OF A SESSION FROM AN EARLIER PROCESS
/// (phase 2B): the owner reads it from the journal, its crash closes derived
/// at read time (the earlier process's lock is gone); an edge, a local id, a
/// journal that is off and a sid it does not hold each get the caller's
/// `ERR no such session`, the bytes an unknown sid gets.
#[cfg(unix)]
#[test]
fn a_session_from_an_earlier_process_answers_from_the_journal_for_the_owner_only() {
    use super::{Scope, Selector, closed_session_reply, journal_session_reply_with};
    let base = std::fs::canonicalize(std::env::temp_dir()).expect("tmp");
    let scratch = base.join(format!("aterm-ctl-journal-{}", std::process::id()));
    let sid = aterm_session::SessionId::generate();
    let logs = journal_with_a_dead_chain(&scratch, &sid);
    let reader = crate::session_log_journal::read::Reader::at(logs);
    let store = crate::session_store::new_store();
    let sel = Selector::Sid(sid.clone());
    let owner =
        journal_session_reply_with(Some(&reader), Some(&sel), "log", "", &store, Scope::Owner)
            .expect("the owner reads the journal");
    assert!(owner.contains("gap - gap why=crash after=2"), "{owner}");
    assert!(owner.contains(" close r1 cmd out=gone"), "{owner}");
    assert!(owner.contains(" close - spawn out=gone"), "{owner}");
    let brief =
        journal_session_reply_with(Some(&reader), Some(&sel), "brief", "", &store, Scope::Owner)
            .expect("the owner's brief");
    assert!(brief.starts_with("OK "), "{brief}");
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    assert_eq!(
        journal_session_reply_with(Some(&reader), Some(&sel), "log", "", &store, edge),
        None,
        "an edge is answered no such session"
    );
    assert_eq!(
        closed_session_reply(Some(&sel), "log", "", &store, edge),
        "ERR no such session\n",
        "byte for byte"
    );
    assert_eq!(
        journal_session_reply_with(
            Some(&reader),
            Some(&Selector::Local(7)),
            "log",
            "",
            &store,
            Scope::Owner
        ),
        None
    );
    assert_eq!(
        journal_session_reply_with(None, Some(&sel), "log", "", &store, Scope::Owner),
        None,
        "the journal off reads nothing"
    );
    let unknown = Selector::Sid(aterm_session::SessionId::generate());
    assert_eq!(
        journal_session_reply_with(
            Some(&reader),
            Some(&unknown),
            "log",
            "",
            &store,
            Scope::Owner
        ),
        None
    );
    // A cursor of the dead process past what its journal holds is reset.
    let past = journal_session_reply_with(
        Some(&reader),
        Some(&sel),
        "log",
        "since=3 ep=1",
        &store,
        Scope::Owner,
    )
    .expect("a reply");
    assert!(past.contains("why=reset"), "{past}");
    // So is `brief`'s, by the same rule.
    let brief_past = journal_session_reply_with(
        Some(&reader),
        Some(&sel),
        "brief",
        "since=3 ep=1",
        &store,
        Scope::Owner,
    )
    .expect("a reply");
    assert!(brief_past.contains("why=reset"), "{brief_past}");
    // A cursor of an epoch the journal never numbered (a carried
    // successor's, the next epoch, at a seq the journal holds) is reset and
    // the reset said, by `log` and `brief` alike: never read on inside the
    // journal's numbering.
    for verb in ["log", "brief"] {
        let next = journal_session_reply_with(
            Some(&reader),
            Some(&sel),
            verb,
            "since=1 ep=2",
            &store,
            Scope::Owner,
        )
        .expect("a reply");
        assert!(next.contains("why=reset"), "{verb}: {next}");
    }
    // The reply hands out the journal's own cursor (`as_of=` its head, `ep=`
    // its epoch), never one into the records it derived past the head:
    // following it shows those again, never a journaled one, never a reset.
    let header = owner.lines().next().expect("a header");
    let field = |text: &str, k: &str| {
        text.lines()
            .next()
            .and_then(|h| h.split_whitespace().find_map(|t| t.strip_prefix(k)))
            .expect("the header names it")
            .to_string()
    };
    let (as_of, ep) = (field(&owner, "as_of="), field(&owner, "ep="));
    assert_eq!((as_of.as_str(), ep.as_str()), ("2", "1"), "{header}");
    let own = journal_session_reply_with(
        Some(&reader),
        Some(&sel),
        "log",
        &format!("since={as_of} ep={ep}"),
        &store,
        Scope::Owner,
    )
    .expect("a reply");
    assert!(
        own.starts_with("OK 3 ") && !own.contains("why=reset"),
        "{own}"
    );
    assert!(own.contains("gap - gap why=crash after=2"), "{own}");
    assert!(
        own.lines()
            .skip(1)
            .all(|l| l.split(' ').next().and_then(|n| n.parse::<u64>().ok()) > Some(2)),
        "only what the reply derives: {own}"
    );
    // `brief` hands out the same cursor, so its own reads on as `log`'s.
    assert_eq!(
        (field(&brief, "as_of="), field(&brief, "ep=")),
        (as_of.clone(), ep.clone()),
        "{brief}"
    );
    let own_brief = journal_session_reply_with(
        Some(&reader),
        Some(&sel),
        "brief",
        &format!("since={as_of} ep={ep}"),
        &store,
        Scope::Owner,
    )
    .expect("a reply");
    assert!(!own_brief.contains("why=reset"), "{own_brief}");
    let _ = std::fs::remove_dir_all(&scratch);
}

/// THE JOURNAL NEVER OUTLIVES A REVOKE (phase 2B over main's shared closed
/// grants, d1260a8554): a session closed in this process whose sid the disk
/// journal ALSO holds. While the index keeps it, the owner reads the index
/// (its real close, no crash the journal's dead chain would say) and the
/// edge's grant reads it; once the owner revokes the grant, the index refuses
/// the edge and the read falls to the journal, which answers Owner-class
/// reads only: the edge gets `ERR no such session` for `log` and `brief`,
/// the bytes an unknown sid gets. CONTROL: the journal does hold the sid
/// (the owner reads it there), so a fallback that answered an edge would
/// hand the revoked edge the session's story.
#[cfg(unix)]
#[test]
fn a_revoked_edge_cannot_read_a_closed_session_through_the_journal() {
    use super::control_session::cmd_revoke_with_closed;
    use super::{Scope, Selector, closed_session_reply_with, journal_session_reply_with};
    const NO_SUCH: &str = "ERR no such session\n";
    let (mut app, ctx, tokens) = closed_session_fixture(&[aterm_session::Op::ReadScreen]);
    let owner_ctx = std::sync::Arc::clone(&app.pool.get(0).expect("session 0").ctx);
    let local = local_of(&app, &ctx);
    app.retire_session_registration(local);
    let base = std::fs::canonicalize(std::env::temp_dir()).expect("tmp");
    let scratch = base.join(format!("aterm-ctl-journal-revoked-{}", std::process::id()));
    let logs = journal_with_a_dead_chain(&scratch, &ctx.self_id);
    let reader = crate::session_log_journal::read::Reader::at(logs);
    let sel = Selector::Sid(ctx.self_id.clone());
    let edge = Scope::Edge(tokens[0]);
    let read = |verb: &str, scope: Scope| {
        closed_session_reply_with(Some(&reader), Some(&sel), verb, "", &app.store, scope)
    };
    let journal = journal_session_reply_with(
        Some(&reader),
        Some(&sel),
        "log",
        "",
        &app.store,
        Scope::Owner,
    )
    .expect("CONTROL: the journal holds the sid");
    assert!(journal.contains("why=crash"), "{journal}");
    let owner = read("log", Scope::Owner);
    assert!(owner.contains(" close - "), "the index's close: {owner}");
    assert!(
        !owner.contains("why=crash"),
        "the index, not the journal: {owner}"
    );
    assert!(
        read("log", edge).starts_with("OK "),
        "NEGATIVE CONTROL: the grant reads the closed session"
    );
    assert_eq!(
        cmd_revoke_with_closed(&owner_ctx, &app.store, Scope::Owner, &tokens[0].to_hex()),
        "OK\n"
    );
    assert_eq!(
        read("log", edge),
        NO_SUCH,
        "revoked: not the journal's story"
    );
    assert_eq!(read("brief", edge), NO_SUCH, "nor its brief");
    assert!(
        read("log", Scope::Owner).contains(" close - "),
        "the owner still reads the index"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// THE HARNESS TOKEN (design §2.5, phase 3A): the in-memory token
/// authenticates as Owner labelled `harness` — the same power, another name —
/// and the instance token as Owner labelled `ctl`; both fold a `TOKEN` line's
/// verb. The harness token is never the instance token, and it is in no
/// variable of this process's environment (it is never written anywhere: a
/// `OnceLock` in memory is all that holds it).
#[test]
fn the_harness_token_reads_as_harness_and_holds_owner_power() {
    let instance = "0f".repeat(32);
    let harness = control_auth::harness_token().expect("entropy is available");
    assert_ne!(harness, instance);
    assert_eq!(harness.len(), 64);
    let (scope, principal, verb) =
        first_line_scope(&format!("AUTH {harness}"), &instance, || None).expect("authenticates");
    assert!(matches!(scope, Scope::Owner), "{scope:?}");
    assert_eq!((principal, verb), (Principal::Harness, None));
    let (_, principal, verb) =
        first_line_scope(&format!("TOKEN {harness} status"), &instance, || None).expect("folds");
    assert_eq!(
        (principal, verb.as_deref()),
        (Principal::Harness, Some("status"))
    );
    let (scope, principal, _) =
        first_line_scope(&format!("AUTH {instance}"), &instance, || None).expect("instance");
    assert!(matches!(scope, Scope::Owner));
    assert_eq!(principal, Principal::Instance);
    assert!(
        std::env::vars().all(|(k, v)| !k.contains(harness) && !v.contains(harness)),
        "the harness token is in no environment variable"
    );
}

/// A wrong token is still refused (`ERR auth` at the lane): the harness
/// token's check is one more constant-time compare, not a softer gate.
#[test]
fn a_wrong_token_is_still_err_auth() {
    let instance = "0f".repeat(32);
    let wrong = "e".repeat(64);
    let h = crate::session_store::test_handle(1);
    let ctx = h.ctx.clone();
    assert_eq!(
        first_line_scope(&format!("AUTH {wrong}"), &instance, || Some(ctx.clone())).err(),
        Some(control_denials::WRONG_TOKEN)
    );
    // The harness token with its last digit changed is no token.
    let harness = control_auth::harness_token().expect("entropy");
    let flip = if harness.ends_with('0') { "1" } else { "0" };
    let near = format!("{}{flip}", &harness[..harness.len() - 1]);
    assert!(first_line_scope(&format!("AUTH {near}"), &instance, || Some(ctx.clone())).is_err());
}

/// `cause=` (phase 3A, ruling R6): taken among `send`'s and `turn`'s leading
/// options in any order, once; a malformed or repeated one is a carried usage
/// refusal; no other verb takes it.
#[test]
fn cause_is_taken_among_the_leading_options_once() {
    use crate::session_log::CauseRef;
    let (o, tail) = control_input::take_leading_options("send", "cause=@5 id=a:b:c hi");
    assert_eq!(
        (o.cause, o.idem.as_deref(), tail.as_str()),
        (Some(CauseRef::Mail(5)), Some("a:b:c"), "hi")
    );
    let (o, tail) = control_input::take_leading_options("send", "id=a:b:c cause=12 if=x hi");
    assert_eq!((o.cause, tail.as_str()), (Some(CauseRef::Rec(12)), "hi"));
    let (o, tail) = control_input::take_leading_options("turn", "cause=7 idle=5 hi");
    assert_eq!(
        (o.cause, tail.as_str()),
        (Some(CauseRef::Rec(7)), "idle=5 hi")
    );
    for bad in [
        "cause=@",
        "cause=0",
        "cause=x",
        "cause=-1",
        "cause=@5 cause=6",
        "cause=@1x",
    ] {
        let (o, _) = control_input::take_leading_options("send", &format!("{bad} hi"));
        assert_eq!(
            o.refusal.as_deref(),
            Some(crate::session_log::CAUSE_USAGE),
            "{bad}"
        );
    }
    let (o, tail) = control_input::take_leading_options("key", "cause=7 enter");
    assert_eq!(
        (o.cause, tail.as_str()),
        (None, "cause=7 enter"),
        "key takes none"
    );
}

/// R7: `cause=` names only what THIS session holds — mail delivered to it
/// (never another session's offset, though it lies below this one's newest),
/// a record seq inside its current seq space and outside every hole.
#[test]
fn cause_names_only_what_this_session_holds() {
    use crate::session_log::{CauseRef, validate_cause};
    let store = session_store::new_store();
    let a = registered_session(1, -1, b"");
    let b = registered_session(2, -1, b"");
    store.write().unwrap().register(a.clone());
    store.write().unwrap().register(b.clone());
    let deliver = |sid: &str, off: u64| {
        crate::fabric::cmd_deliver(
            &store,
            &format!("{sid} off={off} from=h-a kind=task trust=agent text=x"),
        )
    };
    assert!(deliver(a.sid.as_str(), 100).starts_with("OK"));
    assert!(deliver(b.sid.as_str(), 200).starts_with("OK"));
    assert!(deliver(a.sid.as_str(), 300).starts_with("OK"));
    assert_eq!(
        validate_cause(&a.ctx, CauseRef::Mail(100)).map(|c| c.to_string()),
        Ok("@100".to_string())
    );
    let other = validate_cause(&a.ctx, CauseRef::Mail(200));
    assert_eq!(
        other,
        Err("ERR cause: @200 names no mail this session holds\n".to_string()),
        "another session's offset, below this one's newest"
    );
    assert!(validate_cause(&a.ctx, CauseRef::Mail(150)).is_err());
    // Seqs: the head is one; a hole (an update's overlap) and past the head
    // are not.
    let head = {
        let mut tl = a.ctx.timeline.lock().unwrap();
        let log = tl.slog_mut().log_mut_for_journal();
        let at = aterm_slog::Stamp::new(1, 1);
        let before = log.last_seq();
        log.skip_to(before + 5);
        log.mark(
            aterm_slog::Mark::new(
                aterm_slog::Kind::Bell,
                aterm_slog::Src::Engine,
                aterm_slog::Conf::Exact,
            ),
            at,
        )
        .expect("a mark");
        assert!(before >= 1);
        log.last_seq()
    };
    assert_eq!(
        validate_cause(&a.ctx, CauseRef::Rec(1)).map(|c| c.to_string()),
        Ok("1".to_string())
    );
    assert_eq!(
        validate_cause(&a.ctx, CauseRef::Rec(head)).map(|c| c.to_string()),
        Ok(head.to_string())
    );
    assert_eq!(
        validate_cause(&a.ctx, CauseRef::Rec(head - 2)),
        Err(format!(
            "ERR cause: {} names no record of this session\n",
            head - 2
        )),
        "a seq inside a hole"
    );
    assert!(
        validate_cause(&a.ctx, CauseRef::Rec(head + 1)).is_err(),
        "past the head"
    );
}

/// R6: `turn` takes `cause=` anywhere among its options; a valid one types
/// exactly the text and names the turn's cause in the log, an invalid one
/// types nothing and answers `ERR`, and a second `cause=` (one taken leading
/// by the dispatch, one among the options) is a usage refusal.
#[test]
fn a_turn_takes_cause_among_its_options_and_types_only_the_text() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let typed = std::cell::RefCell::new(String::new());
    let paste = |text: &str| {
        typed.borrow_mut().push_str(text);
        true
    };
    let press = |_: &str| true;
    let run = |line: &str, cause: Option<aterm_slog::ShortId>| {
        typed.borrow_mut().clear();
        let reply = cmd_turn(
            &h.term,
            &store,
            0,
            line,
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                actor: aterm_slog::Actor::Ctl,
                cause,
                ..TurnIo::paste_only()
            },
        );
        (reply, typed.borrow().clone())
    };
    let (reply, text) = run("submit=none idle=5 timeout=60000 cause=1 hi", None);
    assert!(reply.starts_with("OK "), "{reply}");
    assert_eq!(text, "hi", "the cause is never typed");
    let (reply, text) = run("submit=none idle=5 timeout=60000 cause=999999 hi", None);
    assert_eq!(
        (reply.as_str(), text.as_str()),
        ("ERR cause: 999999 names no record of this session\n", ""),
        "an invalid cause types nothing"
    );
    // The dispatch checked the one among the options before the claim and
    // hands it in (`io.cause`): the turn takes it, typing only the text.
    let (reply, text) = run(
        "submit=none idle=5 timeout=60000 cause=1 hi",
        aterm_slog::ShortId::new("1"),
    );
    assert!(reply.starts_with("OK "), "{reply}");
    assert_eq!(text, "hi");
    // A second among the options is the usage, before anything is typed; one
    // leading and one among the options the dispatch refuses the same way
    // (`a_refused_turn_option_leaves_its_id_for_the_retry`).
    let (reply, text) = run("submit=none idle=5 cause=1 cause=2 hi", None);
    assert_eq!(
        (reply.as_str(), text.as_str()),
        (crate::session_log::CAUSE_USAGE, ""),
        "a second cause= is refused"
    );
}

/// A REFUSED TURN NEVER BURNS ITS `id=`: every refusal a `turn` makes of its
/// own words — an option's bad value, a bad pattern, a `cause=` among its
/// options that names nothing this session holds — is decided by
/// `turn_preflight` before the exactly-once claim, as the dispatch does, so
/// the corrected retry under the same key types. NEGATIVE
/// CONTROL: the same refusal decided inside the claim (the turn's own parse,
/// as before) settles the key in doubt, and the corrected retry types nothing.
#[test]
fn a_refused_turn_option_leaves_its_id_for_the_retry() {
    let store = session_store::new_store();
    let h = registered_session(0, -1, b"");
    store.write().unwrap().register(h.clone());
    let typed = std::cell::RefCell::new(String::new());
    let paste = |text: &str| {
        typed.borrow_mut().push_str(text);
        true
    };
    let press = |_: &str| true;
    let turn = |line: &str, cause: Option<aterm_slog::ShortId>| {
        cmd_turn(
            &h.term,
            &store,
            0,
            line,
            &subscribe::new_registry(),
            &h.ctx,
            &TurnIo {
                paste: &paste,
                press: &press,
                actor: aterm_slog::Actor::Ctl,
                cause,
                ..TurnIo::paste_only()
            },
        )
    };
    let key = format!("{}:4:1", h.ctx.nonce.to_hex());
    // The dispatch's order: the turn's own words, then the claim around it.
    let dispatch = |line: &str| -> String {
        let cause = match control_session::turn_preflight(line) {
            Err(refusal) => return refusal,
            Ok(None) => None,
            Ok(Some(raw)) => match crate::session_log::validate_cause(&h.ctx, raw) {
                Ok(id) => Some(id),
                Err(refusal) => return refusal,
            },
        };
        crate::pty_idem::guarded(&h.ctx, Scope::Owner, "turn", Some(key.as_str()), || {
            turn(line, cause)
        })
    };
    assert_eq!(
        dispatch("idle=5 cause=999999 hi"),
        "ERR cause: 999999 names no record of this session\n"
    );
    assert_eq!(dispatch("idle=abc hi"), control_session::TURN_USAGE);
    assert_eq!(dispatch("settle=match:( hi"), "ERR badregex\n");
    assert_eq!(
        dispatch("cause=1 cause=2 hi"),
        crate::session_log::CAUSE_USAGE
    );
    let reply = dispatch("submit=none idle=5 timeout=60000 cause=1 hi");
    assert!(
        reply.starts_with("OK "),
        "the corrected retry types: {reply}"
    );
    assert_eq!(typed.borrow().as_str(), "hi");

    // NEGATIVE CONTROL: decided inside the claim, the refusal burns the key.
    typed.borrow_mut().clear();
    let key = format!("{}:4:2", h.ctx.nonce.to_hex());
    let burnt = crate::pty_idem::guarded(&h.ctx, Scope::Owner, "turn", Some(key.as_str()), || {
        turn("idle=5 cause=999999 hi", None)
    });
    assert!(burnt.starts_with("ERR cause: "), "{burnt}");
    let retry = crate::pty_idem::guarded(&h.ctx, Scope::Owner, "turn", Some(key.as_str()), || {
        turn("submit=none idle=5 timeout=60000 hi", None)
    });
    assert_eq!(retry, "ERR in-doubt seq=2\n");
    assert_eq!(typed.borrow().as_str(), "", "nothing typed");
}

/// R14: `feed-bin` and `paste-bin` both note their input in the session log
/// (`mark - input verb=feed` / `verb=paste`), and the bytes never reach it.
#[test]
#[cfg(unix)]
fn feed_bin_and_paste_bin_note_their_input() {
    for (verb, word) in [("feed-bin", "feed"), ("paste-bin", "paste")] {
        let store = session_store::new_store();
        let (h, rx) = pipe_session(1);
        store.write().unwrap().register(h.clone());
        let active = active_for_handle(&h);
        let input = format!("@1 {verb} 10\nsecret-XYZ");
        let mut reader = BufReader::new(std::io::Cursor::new(input.into_bytes()));
        let line = read_request_line(&mut reader).expect("the request line");
        let mut out = Vec::new();
        assert!(run_feed_bin(
            &line,
            verb,
            &mut reader,
            &active,
            &store,
            Scope::Owner,
            &mut out
        ));
        let reply = String::from_utf8_lossy(&out);
        assert!(reply.starts_with("OK 10 bytes"), "{verb}: {reply}");
        let _ = drain_pipe(&rx);
        let tl = h.ctx.timeline.lock().unwrap();
        let lines: Vec<String> = tl
            .slog()
            .log()
            .window()
            .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Content).expect("fits"))
            .collect();
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!(" mark - input verb={word} by=ctl src=ctl "))),
            "{verb}: {lines:?}"
        );
        assert!(
            lines.iter().all(|l| !l.contains("secret")),
            "{verb}: the bytes never reach the log: {lines:?}"
        );
        let brief = tl.slog().log().brief("s-1", 600, u64::MAX / 2).text;
        assert!(!brief.contains("secret"), "{brief}");
    }
}

/// A `turn`'s first byte notes its input in the session log (`mark - input
/// verb=turn`) and wakes the session's watchers once the timeline lock is
/// released (a one-minute hang detector), leaving no wake owed.
#[cfg(unix)]
#[test]
fn a_turn_wakes_the_logs_watchers() {
    let store = session_store::new_store();
    let (h, _reader) = crate::control_input_ack::testing::owned_handle(0);
    store.write().unwrap().register(h.clone());
    let subscribers = subscribe::new_registry();
    let _ = h.ctx.timeline.lock().unwrap().slog_take_wake();
    let sub = crate::subscribe::SubscriberSet::register(&subscribers, &[0]);
    let mark = crate::control_input_ack::InputMark::at_epoch(
        &h.ctx,
        Some(&subscribers),
        0,
        aterm_slog::Actor::Harness,
        crate::session_log::InputVerb::Turn,
        None,
        true,
    );
    let paste = |text: &str| crate::control_input_ack::testing::paste(&h, &mark, text);
    let press = |_: &str| true;
    let reply = cmd_turn(
        &h.term,
        &store,
        0,
        "submit=none idle=5 timeout=60000 hi",
        &subscribers,
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            actor: aterm_slog::Actor::Harness,
            mark: Some(&mark),
            ..TurnIo::paste_only()
        },
    );
    assert!(reply.starts_with("OK "), "{reply}");
    assert!(
        sub.wait(aterm_spec::wait::MUST_HAPPEN),
        "the turn's input never woke the watchers"
    );
    let mut tl = h.ctx.timeline.lock().unwrap();
    assert!(!tl.slog_take_wake(), "the turn left its wake owed");
    assert!(
        tl.slog()
            .log()
            .window()
            .any(|r| r.k == aterm_slog::Kind::Input && r.by == aterm_slog::Actor::Harness),
        "the turn's input mark, by the connection's actor"
    );
}

// ---- `mark` (phase 3A, design §7.4) -----------------------------------------

/// A session whose log holds an agent run (`claude`), a turn and an open
/// approval step on a Bash box: the step's node.
fn with_open_approval(h: &SessionHandle) -> aterm_slog::Node {
    let mut g = h.ctx.timeline.lock().unwrap();
    g.note_root_group(100);
    assert!(g.note_foreground_group(300));
    assert!(g.set_program(300, Some("claude".into())));
    let pgid = g.agent().program_pgid;
    let _ = g.sweep_slog(&crate::session_log::BlockRead::default(), false, 100, pgid);
    let stamp = |seq| crate::session_timeline::AgentStamp {
        generation: crate::control::ScreenGen { epoch: 0, seq },
        fp: 0x2a,
    };
    let claude = Some(aterm_phase::Program::Claude);
    g.publish_agent("busy", None, None, claude, stamp(1));
    g.publish_agent(
        "prompt",
        Some("bash:not-read-only".into()),
        None,
        claude,
        stamp(2),
    );
    let st = g.slog().log().state();
    assert_eq!(
        st.top_need().map(|n| n.word),
        Some("approval:bash"),
        "PRECONDITION: the approval's need"
    );
    st.step().expect("the approval step is open").node
}

/// An edge from a fresh minted session to `h` holding `op`.
fn edge_to(h: &SessionHandle, op: aterm_session::Op) -> (Scope, SessionId) {
    let src = SessionId::generate();
    let tok = h
        .ctx
        .edges
        .lock()
        .unwrap()
        .grant(src.clone(), h.sid.clone(), op, h.ctx.nonce);
    (Scope::Edge(tok), src)
}

/// THE FUNCTIONAL NEGATIVE CONTROL (design §7.4; ruling R5's proof): `mark p<n>
/// act word=approve` on an open approval, from the owner, the harness and a
/// `WriteInput` edge, through the `mark` arm's own function, answers `OK` and
/// puts NO byte on the PTY: the positive control's `Z`, written after them, is
/// the first and only byte the PTY reads (it is FIFO, so a byte a mark made
/// would come first; no timeout window is trusted). The approval stays open
/// and its need stands.
#[test]
#[cfg(unix)]
fn a_mark_act_on_an_open_approval_writes_no_byte() {
    let store = session_store::new_store();
    let (h, rx) = pipe_session(1);
    store.write().unwrap().register(h.clone());
    let subscribers = subscribe::new_registry();
    let step = with_open_approval(&h);
    let (edge, src) = edge_to(&h, aterm_session::Op::WriteInput);
    let act = format!("{step} act word=approve rule=r1 re=1");
    for (scope, harness, by) in [
        (Scope::Owner, false, "ctl".to_string()),
        (Scope::Owner, true, "harness".to_string()),
    ] {
        let _principal = harness.then(|| ServingPrincipal::enter(Principal::Harness));
        let reply = dispatch_mark(&h.ctx, scope, 1, &act, &subscribers);
        assert!(
            reply.starts_with("OK seq=") && reply.ends_with(&format!(" node={step} by={by}\n")),
            "{reply}"
        );
    }
    // The edge, which may not read the session, marks the deepest open node
    // (the approval's step) and is told nothing of it.
    let reply = dispatch_mark(&h.ctx, edge, 1, "- act word=approve rule=r1", &subscribers);
    assert_eq!(reply, format!("OK by={}\n", src.as_str()));
    // The positive control, through the write a `send` makes to a background
    // session.
    assert!(
        cross_input(
            &h.term,
            &h.ctx,
            Some(InputEvent::KeySequence(b"Z".to_vec())),
            "ERR\n"
        )
        .starts_with("OK"),
        "the sentinel was written"
    );
    let (tx, got) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut rx = rx;
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while rx.read_exact(&mut byte).is_ok() {
            seen.push(byte[0]);
            if byte[0] == b'Z' {
                break;
            }
        }
        let _ = tx.send(seen);
    });
    let seen = got
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the sentinel reached the PTY");
    reader.join().unwrap();
    assert_eq!(seen, b"Z", "a mark put a byte on the PTY: {seen:?}");
    let g = h.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    assert!(log.is_open(step), "the approval stays open");
    assert_eq!(
        log.state().top_need().map(|n| n.word),
        Some("approval:bash")
    );
    assert_eq!(
        log.window()
            .filter(|r| r.k == aterm_slog::Kind::Act)
            .count(),
        3,
        "three claims, each a record and nothing else"
    );
}

/// `by=` is the connection's: `ctl`, `harness`, an edge's minted source; `as=`
/// is printed as a self-declared name and never as `by=`. R1: an edge granted
/// under a reserved word around `grant`'s refusal (the mapping alone) is
/// nobody — never `by=harness` in the reply, the log, `brief`, a carry or a
/// fold snapshot.
#[test]
fn a_marks_author_is_the_connection_and_never_a_chosen_word() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subscribers = subscribe::new_registry();
    let reply = dispatch_mark(
        &h.ctx,
        Scope::Owner,
        1,
        "- verdict word=done as=reviewer",
        &subscribers,
    );
    assert!(reply.ends_with(" node=- by=ctl\n"), "{reply}");
    let forged = h.ctx.edges.lock().unwrap().grant(
        SessionId::new("harness"),
        h.sid.clone(),
        aterm_session::Op::WriteInput,
        h.ctx.nonce,
    );
    // A chosen name is never an actor word either.
    assert_eq!(
        dispatch_mark(
            &h.ctx,
            Scope::Edge(forged),
            1,
            "- label text=trust%20me as=harness",
            &subscribers,
        ),
        "ERR mark: as=harness is a session log actor\n"
    );
    let reply = dispatch_mark(
        &h.ctx,
        Scope::Edge(forged),
        1,
        "- label text=trust%20me as=bot",
        &subscribers,
    );
    assert!(reply.ends_with(" by=-\n"), "{reply}");
    let g = h.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    let lines: Vec<String> = log
        .window()
        .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).expect("fits"))
        .collect();
    assert!(
        lines.iter().any(
            |l| l.contains(" mark - verdict word=done as=reviewer by=ctl src=mark conf=claim ")
        ),
        "{lines:?}"
    );
    assert!(
        lines.iter().all(|l| !l.contains("by=harness")),
        "never by=harness in the log: {lines:?}"
    );
    let brief = log.brief(h.sid.as_str(), 600, u64::MAX / 2).text;
    assert!(!brief.contains("by=harness"), "{brief}");
    assert!(
        brief.contains("claim verdict done by=ctl as=reviewer "),
        "{brief}"
    );
    let snap = log.fold().snapshot();
    let refs: Vec<&str> = snap.iter().map(String::as_str).collect();
    let back = aterm_slog::Fold::from_snapshot(&refs).expect("reads");
    assert_eq!(
        back.state().label.as_ref().map(|c| c.by),
        Some(aterm_slog::Actor::Nobody)
    );
    assert!(
        back.state().label_trusted.is_none(),
        "never a trusted label"
    );
    let carried = log
        .carry(aterm_slog::CarryBudget::SESSION, &[])
        .expect("carries");
    assert!(
        carried.lines.iter().all(|l| !l.contains("by=harness")),
        "never by=harness in a carry"
    );
}

/// A WRITER THAT MAY NOT READ LEARNS NOTHING OF THE SESSION from `mark`: an
/// edge holding `WriteInput` alone names neither a node nor a `re=` (`ERR
/// denied`, the same for an open node, a closed one and one never issued, a
/// record that exists and one that does not), and its `OK` names neither the
/// seq its claim took nor the node it landed on (an edge token holds one op,
/// so every edge `mark` is such a writer's). NEGATIVE CONTROL: the owner, who
/// may read, gets the full answer and names a node and a `re=`.
#[test]
fn a_mark_tells_a_writer_that_may_not_read_nothing() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    let step = with_open_approval(&h);
    let source = SessionId::generate();
    let writer = h.ctx.edges.lock().unwrap().grant(
        source.clone(),
        h.sid.clone(),
        aterm_session::Op::WriteInput,
        h.ctx.nonce,
    );
    for probe in [
        format!("{step} note text=x"),
        "r999 note text=x".to_string(),
        "- note text=x re=1".to_string(),
        "- note text=x re=999999".to_string(),
        "- note text=x re=@5".to_string(),
    ] {
        assert_eq!(
            dispatch_mark(&h.ctx, Scope::Edge(writer), 1, &probe, &subs),
            "ERR denied\n",
            "{probe}"
        );
    }
    let reply = dispatch_mark(&h.ctx, Scope::Edge(writer), 1, "- note text=x", &subs);
    assert_eq!(reply, format!("OK by={}\n", source.as_str()));
    // NEGATIVE CONTROL: the owner may read, and is told.
    let reply = dispatch_mark(
        &h.ctx,
        Scope::Owner,
        1,
        &format!("{step} note text=y re=1"),
        &subs,
    );
    assert!(
        reply.starts_with("OK seq=") && reply.contains(&format!(" node={step} by=ctl")),
        "{reply}"
    );
    let reply = dispatch_mark(&h.ctx, Scope::Owner, 1, "r999 note text=z", &subs);
    assert_eq!(reply, "ERR mark: no node r999\n");
}

/// The grammar per kind (§7.4's table) and every refusal of shape: text is
/// refused, never stripped or clamped.
#[test]
fn mark_grammar_refuses_what_a_claim_may_not_say() {
    use crate::session_log::{MARK_USAGE, MarkReq};
    for ok in [
        "- note text=hello%20there",
        "- verdict word=on-track re=3 as=lead",
        "t2 label text=fixing ttl=60",
        "- need word=help",
        "- clear",
        "- clear word=review",
        "p4 act word=approve rule=r1 re=@88231",
    ] {
        assert!(MarkReq::parse(ok).is_ok(), "{ok}: {:?}", MarkReq::parse(ok));
    }
    let refused = |rest: &str| MarkReq::parse(rest).expect_err(rest);
    assert_eq!(
        refused("- note text=a%E2%80%AEb"),
        "ERR mark: text has forbidden formatting\n"
    );
    assert_eq!(refused("- note text=%20%20"), "ERR mark: text is empty\n");
    assert_eq!(
        refused(&format!("- note text={}", "x".repeat(161))),
        "ERR mark: text over 160 bytes encoded\n"
    );
    assert_eq!(
        refused(&format!("- label text={}", "x".repeat(41))),
        "ERR mark: label over 32 graphemes or 40 bytes\n"
    );
    assert_eq!(
        refused(&format!("- label text={}", "%C3%A9".repeat(21))),
        "ERR mark: label over 32 graphemes or 40 bytes\n",
        "21 two-byte graphemes fit 32 graphemes but not 40 bytes"
    );
    assert!(MarkReq::parse(&format!("- label text={}", "%C3%A9".repeat(20))).is_ok());
    assert_eq!(
        refused(&format!("- label text={}", "ab".repeat(16) + "c")),
        "ERR mark: label over 32 graphemes or 40 bytes\n"
    );
    assert_eq!(
        refused("- label text=x ttl=0"),
        "ERR mark: ttl is 1..=3600\n"
    );
    assert_eq!(
        refused("- label text=x ttl=3601"),
        "ERR mark: ttl is 1..=3600\n"
    );
    assert_eq!(
        refused("- verdict word=great"),
        "ERR mark: word=great is not a verdict word (on-track off-track blocked done failed needs-human unsure)\n"
    );
    assert_eq!(
        refused("- need word=failure"),
        "ERR mark: word=failure is not a need word (review decision help)\n"
    );
    for usage in [
        "",
        "-",
        "- shout",
        "- note",
        "- verdict",
        "- note text=a word=done",
        "- note text=a text=b",
        "- verdict word=done ttl=5",
        "- act word=approve rule=@5",
        "n note text=a",
        "x7 note text=a",
        "- note text=a stray",
    ] {
        assert_eq!(refused(usage), MARK_USAGE, "{usage:?}");
    }
    assert_eq!(
        refused("- note text=a as=has%20space"),
        "ERR mark: as= is [A-Za-z0-9._-]{1,32}\n"
    );
}

/// A named node must be open: a closed one is refused (a mark never moves to
/// the deepest node in silence), and one never issued is named so; `-`
/// lands on the deepest open node and the reply says which.
#[test]
fn a_mark_lands_only_on_an_open_node() {
    let h = registered_session(1, -1, b"");
    let subscribers = subscribe::new_registry();
    let step = with_open_approval(&h);
    let reply = dispatch_mark(&h.ctx, Scope::Owner, 1, "- note text=x", &subscribers);
    assert!(
        reply.ends_with(&format!(" node={step} by=ctl\n")),
        "{reply}"
    );
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let claude = Some(aterm_phase::Program::Claude);
        let stamp = crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 0, seq: 3 },
            fp: 0x2a,
        };
        g.publish_agent("busy", None, None, claude, stamp);
    }
    assert_eq!(
        dispatch_mark(
            &h.ctx,
            Scope::Owner,
            1,
            &format!("{step} act word=approve"),
            &subscribers
        ),
        format!("ERR mark: {step} is not open\n")
    );
    assert_eq!(
        dispatch_mark(&h.ctx, Scope::Owner, 1, "p999 note text=x", &subscribers),
        "ERR mark: no node p999\n"
    );
}

/// One claim a second with bursts of twenty per author: the twenty-first is
/// `ERR busy retry_ms=`, a second later one more lands, and the harness's
/// bucket is untouched by `ctl`'s spending.
#[test]
fn the_twenty_first_mark_is_busy() {
    let h = registered_session(1, -1, b"");
    let subscribers = subscribe::new_registry();
    let ms = Arc::new(std::sync::atomic::AtomicU64::new(1_000));
    let read = Arc::clone(&ms);
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .set_clock(crate::session_log::Clock::reading(move || {
            let t = read.load(std::sync::atomic::Ordering::SeqCst);
            aterm_slog::Stamp::new(1_760_000_000_000 + t, t)
        }));
    for i in 0..20 {
        let reply = dispatch_mark(
            &h.ctx,
            Scope::Owner,
            1,
            &format!("- note text=n{i}"),
            &subscribers,
        );
        assert!(reply.starts_with("OK "), "{i}: {reply}");
    }
    let busy = dispatch_mark(&h.ctx, Scope::Owner, 1, "- note text=more", &subscribers);
    assert!(busy.starts_with("ERR busy retry_ms="), "{busy}");
    {
        let _principal = ServingPrincipal::enter(Principal::Harness);
        let reply = dispatch_mark(
            &h.ctx,
            Scope::Owner,
            1,
            "- verdict word=on-track",
            &subscribers,
        );
        assert!(
            reply.ends_with(" by=harness\n"),
            "the harness's own bucket: {reply}"
        );
    }
    ms.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst);
    let again = dispatch_mark(&h.ctx, Scope::Owner, 1, "- note text=later", &subscribers);
    assert!(again.starts_with("OK "), "{again}");
}

/// A claim's need ranks last, and its `clear` lifts only claims: a failed
/// command's `need=failure` survives `mark - clear` from an Owner client
/// (design §3.7; the 2A rule let any in-session agent lift it).
#[test]
fn a_marks_need_ranks_last_and_its_clear_lifts_only_claims() {
    let h = registered_session(1, -1, b"");
    let subscribers = subscribe::new_registry();
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        let r = log
            .open(
                aterm_slog::Open::new(
                    aterm_slog::Kind::Cmd,
                    aterm_slog::Src::Shell,
                    aterm_slog::Conf::Exact,
                ),
                now,
            )
            .expect("cmd");
        log.close(
            r,
            aterm_slog::Close::new(
                aterm_slog::Out::Run(aterm_slog::RunOut::Fail),
                aterm_slog::Src::Shell,
                aterm_slog::Conf::Exact,
            ),
            now,
        )
        .expect("fail");
    }
    assert!(
        dispatch_mark(&h.ctx, Scope::Owner, 1, "- need word=help", &subscribers).starts_with("OK ")
    );
    let needs = |h: &SessionHandle| -> Vec<&'static str> {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .slog()
            .log()
            .state()
            .needs
            .iter()
            .flatten()
            .map(|n| n.word)
            .collect()
    };
    assert_eq!(needs(&h), vec!["failure", "help"], "the claim ranks last");
    assert!(dispatch_mark(&h.ctx, Scope::Owner, 1, "- clear", &subscribers).starts_with("OK "));
    assert_eq!(
        needs(&h),
        vec!["failure"],
        "the claim lifted, the failure stands"
    );
}

/// A mark is not halted (`hold`) and not leased: it puts no byte on a PTY.
#[test]
fn mark_is_not_halted_or_leased() {
    let h = registered_session(1, -1, b"");
    assert!(crate::fabric::apply_hold_for_test(
        &h.ctx,
        Some(crate::fabric::Hold {
            reason: "incident".to_string(),
            origin: "fleet".to_string(),
        })
    ));
    assert!(
        crate::fabric::halt_refusal(&h.ctx, "send").is_some(),
        "CONTROL: send is halted"
    );
    assert!(
        crate::fabric::halt_refusal(&h.ctx, "mark").is_none(),
        "mark is not"
    );
    assert!(!crate::fabric::is_pty_reaching("mark"));
    let subscribers = subscribe::new_registry();
    assert!(
        dispatch_mark(&h.ctx, Scope::Owner, 1, "- note text=held", &subscribers).starts_with("OK ")
    );
}

/// VERSION SKEW (phase 3A): `mark`'s reply is one `OK`/`ERR` status line, the
/// framing an older client defaults to for a verb it does not know.
#[test]
fn mark_is_status_framed() {
    assert_eq!(
        aterm_types::control_verbs::framing_of("mark", "mark - note text=x"),
        aterm_types::control_verbs::Framing::Status
    );
}

// ---- `await log` (phase 3A; ruling R2) ---------------------------------------

/// `await log <args>` on `h`, in process (no hangup slicing: a missing wake
/// parks it to its deadline).
fn await_log(h: &SessionHandle, store: &Store, subs: &Subscribers, args: &str) -> String {
    control_session::cmd_await(
        &h.term,
        store,
        h.local_id,
        &h.ctx,
        &format!("log {args}"),
        subs,
    )
}

/// A session continued from its own log's carry, as a seamless update's
/// successor continues it: the same open path (the approval stays open), an
/// overlap hole the reader crosses `why=handoff`.
fn carried_across_an_update(h: &SessionHandle) {
    let mut g = h.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    let out = log
        .carry(aterm_slog::CarryBudget::SESSION, &[])
        .expect("carries");
    let adopt = aterm_slog::CarryAdopt {
        ep: log.ep() + 1,
        next_seq: log.last_seq() + 1000,
        ids: log.id_counters(),
        limits: aterm_slog::Limits::SESSION,
        at: g.slog().now(),
    };
    let (continued, _, _) = aterm_slog::Log::from_carry(&out.lines, adopt).expect("reads");
    g.slog_mut().replace_log(continued);
}

/// R2: an approval need, carried across an update, latches: two since-less
/// `await log need` calls each answer at once from the fold's open needs
/// (one-minute hang detector: a wait that replayed the ring would meet the
/// carried hole and, re-armed, meet it again). A claim's need does not latch.
#[test]
fn a_latched_need_across_a_handoff_answers_without_looping() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    let step = with_open_approval(&h);
    carried_across_an_update(&h);
    for call in 0..2 {
        let (tx, rx) = std::sync::mpsc::channel();
        let (h2, store2, subs2) = (h.clone(), store.clone(), subs.clone());
        std::thread::spawn(move || {
            let _ = tx.send(await_log(&h2, &store2, &subs2, "need timeout=600000"));
        });
        let reply = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("a latched need answers at once");
        assert!(
            reply.contains(" latched=1 ") && reply.contains(&format!(" open {step} approval ")),
            "call {call}: {reply}"
        );
    }
    // A claim's need alone does not latch: the wait is for what comes next.
    let h3 = registered_session(2, -1, b"");
    store.write().unwrap().register(h3.clone());
    assert!(dispatch_mark(&h3.ctx, Scope::Owner, 2, "- need word=help", &subs).starts_with("OK "));
    assert_eq!(
        await_log(&h3, &store, &subs, "need timeout=0"),
        "OK timeout\n"
    );
}

/// R2: a wait that crosses a loss with no match answers the gap, its
/// `resync_to` the read's own `next`; re-armed there it does not meet the same
/// gap. With a match past a loss, it answers the match and the gap before it.
#[test]
fn a_rearm_at_resync_to_never_returns_the_same_gap() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    let (ep, last) = {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        for _ in 0..300 {
            log.mark(
                aterm_slog::Mark::new(
                    aterm_slog::Kind::Bell,
                    aterm_slog::Src::Engine,
                    aterm_slog::Conf::Exact,
                ),
                now,
            )
            .expect("bell");
        }
        g.slog_mut().set_log_byte_budget(4096);
        let log = g.slog().log();
        assert!(log.lost() > 0, "PRECONDITION: the ring evicted");
        (log.ep(), log.last_seq())
    };
    let reply = await_log(
        &h,
        &store,
        &subs,
        &format!("kind=note since=1 ep={ep} timeout=0"),
    );
    let resync = reply
        .strip_prefix("OK gap why=evicted ")
        .and_then(|r| {
            r.split_whitespace()
                .find_map(|t| t.strip_prefix("resync_to="))
        })
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("{reply}"));
    assert_eq!(resync, last, "resync_to is the read's next: {reply}");
    assert_eq!(
        await_log(
            &h,
            &store,
            &subs,
            &format!("kind=note since={resync} ep={ep} timeout=0")
        ),
        "OK timeout\n",
        "re-armed at resync_to: no gap again"
    );
    // A match past the loss carries the gap before it.
    assert!(dispatch_mark(&h.ctx, Scope::Owner, 1, "- note text=here", &subs).starts_with("OK "));
    let reply = await_log(
        &h,
        &store,
        &subs,
        &format!("kind=note since=1 ep={ep} timeout=0"),
    );
    assert!(
        reply.starts_with("OK seq=")
            && reply.contains(" gap=evicted:")
            && reply.contains(" mark - note "),
        "{reply}"
    );
}

/// The predicates: `need` fires on a failed command's close, an approval's
/// open and a `need` mark (its claim shown); NEGATIVE: an agent mark, a status
/// mark and a verdict claim never do. `since=` reads what is already there
/// (latched by its cursor); `open=`/`close=`/`kind=` match their records.
#[test]
fn await_log_predicates_match_exactly_their_records() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    let head = |h: &SessionHandle| {
        let g = h.ctx.timeline.lock().unwrap();
        (g.slog().log().ep(), g.slog().log().last_seq())
    };
    let (ep, s0) = head(&h);
    assert!(
        dispatch_mark(&h.ctx, Scope::Owner, 1, "- verdict word=blocked", &subs).starts_with("OK ")
    );
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let now = g.slog().now();
        let log = g.slog_mut().log_mut_for_journal();
        log.mark(
            aterm_slog::Mark::new(
                aterm_slog::Kind::Status,
                aterm_slog::Src::Status,
                aterm_slog::Conf::Strong,
            )
            .fields(
                aterm_slog::Fields::new()
                    .word(aterm_slog::Key::Phase, "idle")
                    .expect("phase"),
            ),
            now,
        )
        .expect("status");
    }
    assert_eq!(
        await_log(
            &h,
            &store,
            &subs,
            &format!("need since={s0} ep={ep} timeout=0")
        ),
        "OK timeout\n",
        "a verdict claim and a status mark raise no need"
    );
    let step = with_open_approval(&h);
    let reply = await_log(
        &h,
        &store,
        &subs,
        &format!("need since={s0} ep={ep} timeout=0"),
    );
    assert!(
        reply.contains(&format!(" open {step} approval ")),
        "{reply}"
    );
    let (_, s1) = head(&h);
    assert!(dispatch_mark(&h.ctx, Scope::Owner, 1, "- need word=help", &subs).starts_with("OK "));
    let reply = await_log(
        &h,
        &store,
        &subs,
        &format!("need since={s1} ep={ep} timeout=0"),
    );
    assert!(
        reply.contains(" need word=help by=ctl src=mark conf=claim "),
        "{reply}"
    );
    let reply = await_log(
        &h,
        &store,
        &subs,
        &format!("open=step since={s0} ep={ep} timeout=0"),
    );
    assert!(reply.contains(&format!(" open {step} ")), "{reply}");
    // From the start of this epoch: no reset gap that never happened.
    let reply = await_log(
        &h,
        &store,
        &subs,
        &format!("kind=verdict since=0 ep={ep} timeout=0"),
    );
    assert!(
        reply.starts_with("OK seq=") && reply.contains(" mark - verdict word=blocked "),
        "{reply}"
    );
    assert!(!reply.contains("gap="), "{reply}");
}

/// A wrong epoch reads `why=reset`; usage errors answer at once and park
/// nothing; `timeout=0` is the dirty check.
#[test]
fn await_log_refuses_its_usage_at_once_and_says_a_reset() {
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    for bad in [
        "since=3 timeout=0 need",
        "open=page",
        "kind=nonsense",
        "need open=run",
        "",
        "need since=x ep=1",
        // `since=` names a cursor only with the epoch it came with.
        "kind=verdict since=0",
        "need since=4",
    ] {
        assert_eq!(
            await_log(&h, &store, &subs, bad),
            crate::session_log::AWAIT_LOG_USAGE,
            "{bad:?}"
        );
    }
    let reply = await_log(&h, &store, &subs, "close=run since=1 ep=999 timeout=0");
    assert!(reply.starts_with("OK gap why=reset "), "{reply}");
    assert_eq!(
        await_log(&h, &store, &subs, "close=run timeout=0"),
        "OK timeout\n"
    );
}

/// LIVE: a parked `await log` returns when another thread writes the record it
/// waits for — a mark through the `mark` arm, a `turn`'s first byte — and a
/// session's close delivers its close record before `ERR exited` (one-minute
/// hang detectors: a missing wake parks the waiter to its deadline). Each wait
/// reads from the head taken before it is spawned, so whether the record lands
/// before or after it parks, it is the record that answers it (no sleep orders
/// the two).
#[cfg(unix)]
#[test]
fn a_parked_await_log_wakes_on_the_record() {
    let store = session_store::new_store();
    let (h, _reader) = crate::control_input_ack::testing::owned_handle(1);
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    let park = |what: &str| {
        let (ep, head) = {
            let g = h.ctx.timeline.lock().unwrap();
            (g.slog().log().ep(), g.slog().log().last_seq())
        };
        let args = format!("{what} since={head} ep={ep} timeout=600000");
        let (tx, rx) = std::sync::mpsc::channel();
        let (h2, store2, subs2) = (h.clone(), store.clone(), subs.clone());
        std::thread::spawn(move || {
            let _ = tx.send(await_log(&h2, &store2, &subs2, &args));
        });
        rx
    };
    // A mark.
    let rx = park("kind=note");
    assert!(dispatch_mark(&h.ctx, Scope::Owner, 1, "- note text=wake", &subs).starts_with("OK "));
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("woken by the mark");
    assert!(reply.contains(" mark - note text=wake "), "{reply}");
    // A turn's first byte.
    let rx = park("kind=input");
    let mark = crate::control_input_ack::InputMark::at_epoch(
        &h.ctx,
        Some(&subs),
        1,
        aterm_slog::Actor::Bridge,
        crate::session_log::InputVerb::Turn,
        None,
        true,
    );
    let paste = |text: &str| crate::control_input_ack::testing::paste(&h, &mark, text);
    let press = |_: &str| true;
    let _ = cmd_turn(
        &h.term,
        &store,
        1,
        "submit=none idle=5 timeout=60000 hi",
        &subs,
        &h.ctx,
        &TurnIo {
            paste: &paste,
            press: &press,
            actor: aterm_slog::Actor::Bridge,
            mark: Some(&mark),
            ..TurnIo::paste_only()
        },
    );
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("woken by the turn");
    assert!(
        reply.contains(" mark - input verb=turn by=bridge "),
        "{reply}"
    );
    // The session's close.
    let rx = park("close=session");
    store.write().unwrap().deregister_local_as(
        1,
        crate::session_store::ExitReason::CtlClose,
        crate::session_store::ExitActor::Ctl,
    );
    subs.lock().unwrap().notify(1);
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("woken by the close");
    assert!(reply.contains(" close - spawn out=ctl-close "), "{reply}");
}

/// A SHELL THAT EXITED ON ITS OWN: its session is `Exited` at once
/// (`exit_session_logical`, which says first that the close is due) and its
/// log's close is written by the retire's deregister afterwards, so `await
/// log` parks on for that close instead of answering `ERR exited` over the
/// record it asked for. `timeout=0` on such a session is the deterministic
/// probe (it times out: the close is coming); a parked waiter is answered the
/// close once the deregister writes it and wakes it (a one-minute hang
/// detector), and the close clears the flag. NEGATIVE CONTROLS: an `Exited`
/// session whose close nobody said is due (a held pane restored across an
/// update registers that way) answers `ERR exited` at once for every form, and
/// so does a waiter parked while the close was due once the exit handling ends
/// without a retire (`--hold`) and wakes it.
#[test]
fn await_log_waits_for_the_close_of_a_shell_that_exited() {
    use crate::session_store::{ExitActor, ExitReason, SessionState};
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    h.ctx.timeline.lock().unwrap().slog_mut().note_close_due();
    store.write().unwrap().set_state(1, SessionState::Exited);
    for what in ["close=session", "need", "close=run", "kind=verdict"] {
        assert_eq!(
            await_log(&h, &store, &subs, &format!("{what} timeout=0")),
            "OK timeout\n",
            "{what}: the close is still to come"
        );
    }
    let (ep, head) = {
        let g = h.ctx.timeline.lock().unwrap();
        (g.slog().log().ep(), g.slog().log().last_seq())
    };
    let (tx, rx) = std::sync::mpsc::channel();
    for what in ["close=session", "need"] {
        let args = format!("{what} since={head} ep={ep} timeout=600000");
        let (h2, store2, subs2, tx) = (h.clone(), store.clone(), subs.clone(), tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(await_log(&h2, &store2, &subs2, &args));
        });
    }
    store
        .write()
        .unwrap()
        .deregister_local_as(1, ExitReason::ShellExit, ExitActor::Unknown);
    subs.lock().unwrap().notify(1);
    for _ in 0..2 {
        let reply = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("woken by the retire");
        assert!(reply.contains(" close - spawn out=shell-exit "), "{reply}");
    }
    assert!(
        !h.ctx.timeline.lock().unwrap().slog().close_due(),
        "the close cleared what was due"
    );
    // NEGATIVE CONTROL: ended with no close due (a restored held pane).
    let held = registered_session(2, -1, b"");
    store.write().unwrap().register(held.clone());
    store.write().unwrap().set_state(2, SessionState::Exited);
    for what in ["close=session", "need", "close=run", "kind=verdict"] {
        assert_eq!(
            await_log(&held, &store, &subs, &format!("{what} timeout=0")),
            "ERR exited\n",
            "{what}: no close is coming"
        );
    }
    // NEGATIVE CONTROL: the exit handling ends with no retire (`--hold`,
    // `App::settle_log_close`) right after the waiter's look found the close
    // due, and its wake ends the wait (a one-minute hang detector).
    let kept = registered_session(3, -1, b"");
    store.write().unwrap().register(kept.clone());
    kept.ctx
        .timeline
        .lock()
        .unwrap()
        .slog_mut()
        .note_close_due();
    store.write().unwrap().set_state(3, SessionState::Exited);
    let mut looks = 0;
    let reply = control_session::await_log_seamed(
        &store,
        3,
        &kept.ctx,
        crate::session_log::LogAwait::parse(&["close=session"]).expect("parses"),
        u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis()).unwrap_or(u64::MAX),
        &subs,
        || {
            looks += 1;
            if looks == 1 {
                let mut tl = kept.ctx.timeline.lock().unwrap();
                assert!(tl.slog_mut().take_close_due(), "the close was due");
                drop(tl);
                subs.lock().unwrap().notify(3);
            }
        },
    );
    assert_eq!(reply, "ERR exited\n");
    assert_eq!(looks, 2, "parked once, then answered");
}

/// A RETIRE THAT LANDS RIGHT AFTER THE WAITER'S LOOK (phase 3A review): the
/// waiter saw the session ended and read its log (no close yet); the retire
/// then wrote the close and deregistered it before the waiter decided. The
/// waiter answers that close, never `ERR exited`: the look is its only
/// observation (the log and whether the close is due, under one lock), and
/// the retire's wake is latched. Driven on this one thread through the
/// waiter's seam (`await_log_seamed`), so the interleaving is exact and a
/// waiter that missed the wake would park to the one-minute hang detector.
///
/// RED before the fix: the waiter read the log, then the registration, found
/// the session gone and answered `ERR exited` over the close it waited for.
#[test]
fn await_log_answers_a_close_written_right_after_its_look() {
    use crate::session_store::{ExitActor, ExitReason, SessionState};
    let store = session_store::new_store();
    let h = registered_session(1, -1, b"");
    store.write().unwrap().register(h.clone());
    let subs = subscribe::new_registry();
    h.ctx.timeline.lock().unwrap().slog_mut().note_close_due();
    store.write().unwrap().set_state(1, SessionState::Exited);
    let mut retired = 0;
    let reply = control_session::await_log_seamed(
        &store,
        1,
        &h.ctx,
        crate::session_log::LogAwait::parse(&["close=session"]).expect("parses"),
        u64::try_from(aterm_spec::wait::MUST_HAPPEN.as_millis()).unwrap_or(u64::MAX),
        &subs,
        || {
            retired += 1;
            if retired == 1 {
                store.write().unwrap().deregister_local_as(
                    1,
                    ExitReason::ShellExit,
                    ExitActor::Unknown,
                );
                subs.lock().unwrap().notify(1);
            }
        },
    );
    assert!(reply.contains(" close - spawn out=shell-exit "), "{reply}");
    assert_eq!(retired, 1, "the retire landed after the waiter's one look");
}

/// `subscribe … log` (phase 3A): `since-log=` and `log-ep=` go together, name
/// one target, and each refuses a value that does not parse; every refusal
/// registers nothing. An edge holding only `WriteInput` is refused the stream
/// (it is a read).
#[test]
fn since_log_needs_log_ep_and_a_single_target() {
    let store = session_store::new_store();
    let a = registered_session(0, -1, b"");
    let b = registered_session(1, -1, b"");
    store.write().unwrap().register(a.clone());
    store.write().unwrap().register(b.clone());
    let active = active_for(&a);
    let registry = subscribe::new_registry();
    for (line, want) in [
        (
            "subscribe @. log since-log=4",
            "ERR usage: since-log= and log-ep= go together\n",
        ),
        (
            "subscribe @. log log-ep=1",
            "ERR usage: since-log= and log-ep= go together\n",
        ),
        (
            "subscribe @. log since-log=x log-ep=1",
            "ERR bad since-log\n",
        ),
        ("subscribe @. log since-log=4 log-ep=-1", "ERR bad log-ep\n"),
        (
            "subscribe @0,@1 log since-log=4 log-ep=1",
            "ERR resume anchors (since=/since-turn=/since-block=/since-log=) require a single target\n",
        ),
        (
            "subscribe @* log since-log=4 log-ep=1",
            "ERR resume anchors (since=/since-turn=/since-block=/since-log=) require a single target\n",
        ),
    ] {
        let mut out: Vec<u8> = Vec::new();
        run_subscribe(line, &active, &store, &registry, Scope::Owner, &mut out);
        assert_eq!(String::from_utf8_lossy(&out), want, "{line}");
        assert_eq!(registry.lock().unwrap().watched_sessions(), 0, "{line}");
    }
    let tok = a.ctx.edges.lock().unwrap().grant(
        SessionId::generate(),
        a.sid.clone(),
        Op::WriteInput,
        a.nonce,
    );
    let mut out: Vec<u8> = Vec::new();
    run_subscribe(
        "subscribe @. log",
        &active,
        &store,
        &registry,
        Scope::Edge(tok),
        &mut out,
    );
    assert_eq!(String::from_utf8_lossy(&out), "ERR denied\n");
}

/// A FULL POOL GROWS (AD-2 of the codex-alab end-to-end run, 2026-10-16:
/// the fifth codex-alab of an instance was refused its `subscribe @self`
/// for good, and with it its claim): past the four started lanes, each
/// further long-lived subscriber starts one more lane and is admitted, until
/// the ceiling, where the next is refused at once as before. Negative
/// control: a grower whose thread does not start admits nothing past the
/// started lanes and publishes no lane.
#[test]
fn a_full_push_pool_grows_a_lane_per_subscriber_up_to_its_ceiling() {
    use std::io::BufRead;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let ceiling = CONTROL_SUBSCRIPTION_WORKERS + 2;
    let subscriptions = SubscriptionDispatch::new(ceiling);
    subscriptions
        .jobs
        .set_capacity(CONTROL_SUBSCRIPTION_WORKERS);
    let grown = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&grown);
    assert!(
        subscriptions
            .grow
            .set(Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                true
            }))
            .is_ok()
    );
    let mut clients = Vec::new();
    for index in 0..ceiling {
        let (client, server) = CtlStream::pair().expect("subscription socket pair");
        clients.push(client);
        assert!(
            subscriptions
                .try_submit(format!("subscribe @{index} screen"), Scope::Owner, server)
                .is_ok(),
            "subscriber {index} of {ceiling} is admitted"
        );
    }
    assert_eq!(
        grown.load(Ordering::SeqCst),
        2,
        "one lane per subscriber past four"
    );
    assert_eq!(subscriptions.jobs.capacity(), ceiling);
    let (overflow, overflow_server) = CtlStream::pair().expect("overflow pair");
    overflow
        .set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
        .expect("bound rejection read");
    handoff_subscription(
        &subscriptions,
        "subscribe @. screen".to_string(),
        Scope::Owner,
        overflow_server,
    );
    let mut rejection = String::new();
    BufReader::new(overflow)
        .read_line(&mut rejection)
        .expect("the subscriber past the ceiling gets a prompt response");
    assert_eq!(rejection, "ERR subscription capacity busy; retry\n");
    assert_eq!(
        grown.load(Ordering::SeqCst),
        2,
        "nothing grows past the ceiling"
    );

    // The negative control: no lane starts, nothing is admitted past four.
    let stuck = SubscriptionDispatch::new(ceiling);
    stuck.jobs.set_capacity(CONTROL_SUBSCRIPTION_WORKERS);
    assert!(stuck.grow.set(Box::new(|| false)).is_ok());
    let mut kept = Vec::new();
    for index in 0..CONTROL_SUBSCRIPTION_WORKERS {
        let (client, server) = CtlStream::pair().expect("pair");
        kept.push(client);
        assert!(
            stuck
                .try_submit(format!("subscribe @{index} screen"), Scope::Owner, server)
                .is_ok()
        );
    }
    let (_client, server) = CtlStream::pair().expect("pair");
    assert!(
        stuck
            .try_submit("subscribe @. screen".to_string(), Scope::Owner, server)
            .is_err(),
        "CONTROL: a lane that did not start admits nobody"
    );
    assert_eq!(stuck.jobs.capacity(), CONTROL_SUBSCRIPTION_WORKERS);
}

/// The push ceiling answers to the descriptor limit: an eighth of it, never
/// fewer than the started lanes nor more than the hard ceiling.
#[test]
fn the_push_ceiling_is_an_eighth_of_the_descriptor_limit_within_its_bounds() {
    assert_eq!(subscriptions_cap_for(Some(256)), 32, "launchd's soft limit");
    assert_eq!(
        subscriptions_cap_for(Some(16)),
        CONTROL_SUBSCRIPTION_WORKERS
    );
    assert_eq!(
        subscriptions_cap_for(Some(1 << 20)),
        CONTROL_SUBSCRIPTION_MAX
    );
    assert_eq!(subscriptions_cap_for(None), CONTROL_SUBSCRIPTION_WORKERS);
}
